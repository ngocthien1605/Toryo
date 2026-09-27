//! Codex `exec --json` launch (plan §6.3, R18, R19, R21, R22) and JSONL parser.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

use super::{CommandExec, ErrorClass, TurnOutcome};
use crate::config::CodexSettings;
use crate::usage::{codex_buckets, raw_counters, Accounting, CodexSubsets};

#[derive(Debug, Clone)]
pub struct CodexLaunch {
    pub exe: PathBuf,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub settings: CodexSettings,
    pub output_schema: Option<PathBuf>,
    pub resume_thread: Option<String>,
    pub extra_args: Vec<String>,
}

/// TOML basic string (JSON escaping is a valid subset for the values we pass).
fn toml_str(s: &str) -> String {
    serde_json::to_string(s).expect("string")
}

impl CodexLaunch {
    /// The `-c` override list, identical for `exec` and `exec resume` (R19).
    pub fn overrides(&self) -> Vec<String> {
        let s = &self.settings;
        let mut c = vec![format!("sandbox_mode={}", toml_str(&s.sandbox_mode))];
        if let Some(ws) = &s.windows_sandbox {
            c.push(format!("windows.sandbox={}", toml_str(ws)));
        }
        if let Some(p) = &s.provider {
            c.push(format!("model_provider={}", toml_str(&p.id)));
            c.push(format!(
                "model_providers.{}.name={}",
                p.id,
                toml_str(&p.name)
            ));
            c.push(format!(
                "model_providers.{}.base_url={}",
                p.id,
                toml_str(&p.base_url)
            ));
            c.push(format!(
                "model_providers.{}.env_key={}",
                p.id,
                toml_str(&p.env_key)
            ));
            c.push(format!(
                "model_providers.{}.wire_api={}",
                p.id,
                toml_str(&p.wire_api)
            ));
        }
        if let Some(e) = &s.model_reasoning_effort {
            c.push(format!("model_reasoning_effort={}", toml_str(e)));
        }
        for (k, v) in &s.overrides {
            c.push(format!("{k}={v}"));
        }
        if let Some(n) = s.project_doc_max_bytes {
            c.push(format!("project_doc_max_bytes={n}"));
        }
        c
    }

    /// argv without the program; prompt goes on stdin (`-`).
    ///
    /// All options go BEFORE the optional `resume <thread>` subcommand: exec-level options are
    /// always accepted there, whether or not clap marks them `global` (only `global` ones are
    /// accepted after `resume`). The `-c` list is identical for exec and resume (R19).
    pub fn argv(&self) -> Vec<String> {
        let mut a: Vec<String> = vec!["exec".into()];
        a.extend([
            "--json".into(),
            "--ignore-user-config".into(),
            "--ignore-rules".into(),
            "--skip-git-repo-check".into(),
        ]);
        if let Some(m) = &self.model {
            a.extend(["-m".into(), m.clone()]);
        }
        for o in self.overrides() {
            a.extend(["-c".into(), o]);
        }
        if let Some(s) = &self.output_schema {
            a.extend(["--output-schema".into(), s.display().to_string()]);
        }
        a.extend(self.extra_args.iter().cloned());
        if let Some(t) = &self.resume_thread {
            a.push("resume".into());
            a.push(t.clone());
        }
        a.push("-".into());
        a
    }

    /// Warnings that make a Codex profile unusable (R18).
    pub fn preflight(&self) -> Vec<String> {
        let mut w = Vec::new();
        if cfg!(windows) && self.settings.windows_sandbox.is_none() {
            w.push("codex.windows_sandbox is not set: on native Windows the sandbox type is None and workspace-write enforces nothing (R18)".into());
        }
        if self.settings.sandbox_mode != "workspace-write" {
            w.push(format!(
                "sandbox_mode is `{}`, plan requires workspace-write",
                self.settings.sandbox_mode
            ));
        }
        w
    }
}

/// R21: exec errors carry only `message`; classify by text.
pub fn classify_message(msg: &str) -> ErrorClass {
    let table: &[(&str, ErrorClass)] = &[
        (
            r"(?i)context window|context_length|maximum context",
            ErrorClass::ContextWindowExceeded,
        ),
        (
            r"(?i)usage limit|quota|insufficient_quota|billing",
            ErrorClass::QuotaExceeded,
        ),
        (
            r"(?i)\b429\b|rate.?limit|too many requests",
            ErrorClass::RateLimited,
        ),
        (
            r"(?i)\b(401|403)\b|unauthori[sz]ed|forbidden|invalid api key|authentication",
            ErrorClass::Auth,
        ),
        (
            r"(?i)\b(500|502|503|504)\b|overloaded|server error",
            ErrorClass::Overloaded,
        ),
        (
            r"(?i)stream disconnected|connection (reset|refused)|timed out|network|dns",
            ErrorClass::Network,
        ),
        (
            r"(?i)\b400\b|invalid_request|bad request|model .*not found",
            ErrorClass::BadRequest,
        ),
    ];
    for (re, class) in table {
        if regex::Regex::new(re).expect("regex").is_match(msg) {
            return *class;
        }
    }
    ErrorClass::Unknown
}

#[derive(Default)]
pub struct CodexParser {
    thread_id: Option<String>,
    protocol_errors: Vec<String>,
    malformed: u32,
    last_agent_message: Option<String>,
    completed_usage: Option<Value>,
    turn_completed: bool,
    failed: Option<String>,
    items: Vec<String>,
    commands: Vec<CommandExec>,
    unknown_types: BTreeMap<String, u32>,
}

impl CodexParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed_line(&mut self, line: &[u8]) {
        if line.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        let v: Value = match serde_json::from_slice(line) {
            Ok(v) => v,
            Err(_) => {
                self.malformed += 1;
                return;
            }
        };
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "thread.started" => {
                let id = v
                    .get("thread_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                match &self.thread_id {
                    None => self.thread_id = Some(id),
                    Some(t) if *t != id => self
                        .protocol_errors
                        .push(format!("second thread id {id} (first {t})")),
                    _ => {}
                }
            }
            "turn.started" => {}
            "item.started" | "item.updated" | "item.completed" => {
                let item = v.get("item").unwrap_or(&Value::Null);
                let it = item
                    .get("type")
                    .or_else(|| item.get("item_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_string();
                let completed = v.get("type").and_then(Value::as_str) == Some("item.completed");
                if completed {
                    self.items.push(it.clone());
                    match it.as_str() {
                        "agent_message" => {
                            self.last_agent_message =
                                item.get("text").and_then(Value::as_str).map(str::to_string)
                        }
                        "file_change" => {
                            let paths: Vec<&str> = item
                                .get("changes")
                                .and_then(Value::as_array)
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|c| c.get("path").and_then(Value::as_str))
                                        .collect()
                                })
                                .unwrap_or_default();
                            self.commands.push(CommandExec {
                                command: format!("file_change: {}", paths.join(", ")),
                                status: item
                                    .get("status")
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                                exit_code: None,
                            })
                        }
                        "command_execution" => self.commands.push(CommandExec {
                            command: item
                                .get("command")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            status: item
                                .get("status")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            exit_code: item.get("exit_code").and_then(Value::as_i64),
                        }),
                        _ => {}
                    }
                }
            }
            "turn.completed" => {
                self.turn_completed = true;
                self.completed_usage = v.get("usage").cloned();
            }
            "turn.failed" => {
                self.failed = Some(
                    v.pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("turn.failed")
                        .to_string(),
                )
            }
            "error" => {
                self.failed = Some(
                    v.get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("error")
                        .to_string(),
                )
            }
            other => *self.unknown_types.entry(other.to_string()).or_default() += 1,
        }
    }

    pub fn finish(self, process_exit: Option<i32>, subsets: CodexSubsets) -> TurnOutcome {
        let mut o = TurnOutcome::empty();
        o.session_id = self.thread_id.clone();
        o.protocol_errors = self.protocol_errors.clone();
        o.malformed_lines = self.malformed;
        o.tool_activity = self.items.clone();
        o.commands = self.commands.clone();
        o.final_text = self.last_agent_message.clone().unwrap_or_default();
        if let Some(u) = &self.completed_usage {
            o.raw_usage = Some(u.clone());
            o.usage = Some(codex_buckets(&raw_counters(u), subsets));
            // NB: on a resumed thread these counters are cumulative (R21) — the caller stores the delta.
            o.accounting = Accounting::Exact;
        } else {
            // No live usage in exec mode (R41): a turn without turn.completed is UNKNOWN, never 0.
            o.accounting = Accounting::Unknown;
        }
        if let Some(msg) = &self.failed {
            o.error_class = Some(classify_message(msg));
            o.error_detail = Some(msg.clone());
            return o;
        }
        if !self.turn_completed {
            o.error_class = Some(ErrorClass::ProcessCrashed);
            o.error_detail = Some(format!(
                "no turn.completed / turn.failed (exit {process_exit:?})"
            ));
            return o;
        }
        match &self.last_agent_message {
            None => {
                o.error_class = Some(ErrorClass::InvalidOutput);
                o.error_detail = Some("turn.completed without a final agent_message".into());
            }
            Some(t) => match serde_json::from_str::<Value>(t) {
                Ok(v) => {
                    o.structured = Some(v);
                    o.success = true;
                }
                Err(e) => {
                    o.error_class = Some(ErrorClass::InvalidOutput);
                    o.error_detail = Some(format!("final message is not JSON: {e}"));
                }
            },
        }
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CodexProvider;

    fn launch() -> CodexLaunch {
        CodexLaunch {
            exe: "codex.exe".into(),
            cwd: "W".into(),
            model: Some("gpt-5-codex".into()),
            settings: CodexSettings {
                sandbox_mode: "workspace-write".into(),
                windows_sandbox: Some("unelevated".into()),
                provider: Some(CodexProvider {
                    id: "foundry".into(),
                    name: "Azure Foundry".into(),
                    base_url: "https://r.openai.azure.com/openai/v1".into(),
                    env_key: "AZURE_OPENAI_API_KEY".into(),
                    wire_api: "responses".into(),
                }),
                project_doc_max_bytes: None,
                overrides: Default::default(),
                model_reasoning_effort: Some("medium".into()),
            },
            output_schema: Some("implementation.schema.json".into()),
            resume_thread: None,
            extra_args: vec![],
        }
    }

    #[test]
    fn argv_uses_c_overrides_and_stdin() {
        let a = launch().argv();
        let s = a.join(" ");
        assert!(s.starts_with(
            "exec --json --ignore-user-config --ignore-rules --skip-git-repo-check -m gpt-5-codex"
        ));
        assert!(s.contains("-c sandbox_mode=\"workspace-write\""));
        assert!(s.contains("-c windows.sandbox=\"unelevated\""));
        assert!(s.contains(
            "-c model_providers.foundry.base_url=\"https://r.openai.azure.com/openai/v1\""
        ));
        assert!(
            !s.contains("--sandbox "),
            "R19: never the non-global --sandbox flag"
        );
        assert_eq!(a.last().unwrap(), "-");
    }

    #[test]
    fn resume_repeats_identical_overrides() {
        let fresh = launch();
        let mut res = launch();
        res.resume_thread = Some("th_1".into());
        let a = res.argv();
        assert_eq!(a[0], "exec");
        assert_eq!(fresh.overrides(), res.overrides());
        assert_eq!(
            &a[a.len() - 3..],
            &["resume".to_string(), "th_1".to_string(), "-".to_string()]
        );
        // identical option list before the subcommand
        let f = fresh.argv();
        assert_eq!(&f[..f.len() - 1], &a[..a.len() - 3]);
    }

    #[test]
    fn classifier_table() {
        assert_eq!(
            classify_message("stream error: 429 Too Many Requests"),
            ErrorClass::RateLimited
        );
        assert_eq!(
            classify_message("You've hit your usage limit"),
            ErrorClass::QuotaExceeded
        );
        assert_eq!(
            classify_message("unexpected status 401 Unauthorized"),
            ErrorClass::Auth
        );
        assert_eq!(
            classify_message("input exceeds the context window"),
            ErrorClass::ContextWindowExceeded
        );
        assert_eq!(classify_message("something odd"), ErrorClass::Unknown);
    }

    #[test]
    fn parses_success_commands_and_usage() {
        let mut p = CodexParser::new();
        for l in [
            r#"{"type":"thread.started","thread_id":"th_1"}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"item.completed","item":{"id":"i0","type":"command_execution","command":"git commit --allow-empty -m x","aggregated_output":"denied","exit_code":1,"status":"failed"}}"#,
            r#"{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"{\"summary\":\"ok\",\"deviations\":[],\"validation_performed\":[],\"open_issues\":[],\"resolved\":[]}"}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":1000,"cached_input_tokens":600,"output_tokens":120}}"#,
        ] {
            p.feed_line(l.as_bytes());
        }
        let o = p.finish(Some(0), CodexSubsets::default());
        assert!(o.success);
        assert_eq!(o.session_id.as_deref(), Some("th_1"));
        assert_eq!(o.commands[0].status.as_deref(), Some("failed"));
        let u = o.usage.unwrap();
        assert_eq!((u.uncached_input, u.cache_read, u.output), (400, 600, 120));
    }

    #[test]
    fn killed_turn_is_unknown_not_zero() {
        let mut p = CodexParser::new();
        p.feed_line(br#"{"type":"thread.started","thread_id":"th_1"}"#);
        p.feed_line(br#"{"type":"turn.started"}"#);
        let o = p.finish(None, CodexSubsets::default());
        assert_eq!(o.accounting, Accounting::Unknown);
        assert!(o.usage.is_none());
        assert_eq!(o.error_class, Some(ErrorClass::ProcessCrashed));
    }

    #[test]
    fn failed_turn_is_classified() {
        let mut p = CodexParser::new();
        p.feed_line(br#"{"type":"turn.failed","error":{"message":"exceeded retry limit, last status: 429"}}"#);
        let o = p.finish(Some(1), CodexSubsets::default());
        assert_eq!(o.error_class, Some(ErrorClass::RateLimited));
    }
}
