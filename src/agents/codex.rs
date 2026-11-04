//! Codex `exec --json` launch (plan §6.3, R18, R19, R21, R22) and JSONL parser.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::LazyLock;

use regex::Regex;
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

    /// The launch preflight (R18, ARCH-05): what makes the profile unusable, and what only
    /// deserves a warning.
    pub fn check(&self) -> Preflight {
        let mut p = Preflight::default();
        if cfg!(windows) && self.settings.windows_sandbox.is_none() {
            p.refused.push("codex.windows_sandbox is not set: on native Windows the sandbox type is None and workspace-write enforces nothing (R18)".into());
        }
        let mode = &self.settings.sandbox_mode;
        if mode != "workspace-write" {
            let why = format!("sandbox_mode is `{mode}`, plan requires workspace-write");
            // stricter than the plan (nothing is writable): it runs, as it always did
            if mode == "read-only" {
                p.warnings.push(why);
            } else {
                p.refused.push(why);
            }
        }
        // a frozen configuration is not validated again, so check the overrides here too
        let provider = self.settings.provider.as_ref().map(|v| v.id.as_str());
        for k in self.settings.overrides.keys() {
            if let Some(why) = crate::config::denied_override(k, provider) {
                p.refused.push(format!("codex.overrides.{k} {why}"));
            }
        }
        p
    }

    /// Every preflight reason, refusals first. The M0 spike prints them all as warnings
    /// and runs anyway; the CLI adapter uses [`CodexLaunch::check`].
    pub fn preflight(&self) -> Vec<String> {
        let p = self.check();
        p.refused.into_iter().chain(p.warnings).collect()
    }
}

/// The result of [`CodexLaunch::check`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preflight {
    /// The profile is unusable: the CLI adapter refuses the turn (fail closed, ARCH-05).
    pub refused: Vec<String>,
    /// Printed as `WARNING: …`; the turn runs (a `read-only` sandbox).
    pub warnings: Vec<String>,
}

/// The R21 message table, most specific first, compiled once (CODE-05).
static CODEX_MESSAGES: LazyLock<Vec<(Regex, ErrorClass)>> = LazyLock::new(|| {
    [
        (
            r"(?i)context window|context_length|maximum context",
            ErrorClass::ContextWindowExceeded,
        ),
        // Exhaustion in words a vendor sends even with a 429 status (OpenAI
        // `insufficient_quota`: "You exceeded your current quota …", Codex "You've hit
        // your usage limit").
        (
            r"(?i)usage limit|insufficient_?quota|exceeded your current quota",
            ErrorClass::QuotaExceeded,
        ),
        // EC-03: the status and "rate limit" before the generic quota words. An Azure
        // throttle ("429 … exceeded token rate limit … https://aka.ms/oai/quotaincrease")
        // is transient; read as quota it would switch the implementer for good (R49).
        (
            r"(?i)\b429\b|rate.?limit|too many requests",
            ErrorClass::RateLimited,
        ),
        (
            r"(?i)\bquota\b|quota_?exceeded|\bbilling\b",
            ErrorClass::QuotaExceeded,
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
    ]
    .into_iter()
    .map(|(re, class)| (Regex::new(re).expect("regex"), class))
    .collect()
});

/// R21: exec errors carry only `message`; classify by text.
pub fn classify_message(msg: &str) -> ErrorClass {
    CODEX_MESSAGES
        .iter()
        .find(|(re, _)| re.is_match(msg))
        .map_or(ErrorClass::Unknown, |(_, class)| *class)
}

#[derive(Default)]
pub struct CodexParser {
    thread_id: Option<String>,
    protocol_errors: Vec<String>,
    malformed: u32,
    last_agent_message: Option<String>,
    completed_usage: Option<Value>,
    turn_completed: bool,
    /// `turn.failed`: the only failure a turn reports about itself.
    failed: Option<String>,
    /// Top-level `error` lines. Most are notices the CLI recovers from ("Reconnecting...
    /// 1/5 (stream disconnected before completion)"); a fatal one is followed by
    /// `turn.failed` (EC-02).
    errors: Vec<String>,
    items: Vec<String>,
    commands: Vec<CommandExec>,
    unknown_types: BTreeMap<String, u32>,
}

impl CodexParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed_line(&mut self, line: &[u8]) {
        let v = super::events::parse_line(line);
        self.feed_parsed(line, v.as_ref());
    }

    /// [`Self::feed_line`] for a line already parsed (`v`: `None` when it is not JSON).
    pub fn feed_parsed(&mut self, line: &[u8], v: Option<&Value>) {
        if line.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        let Some(v) = v else {
            self.malformed += 1;
            return;
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
            "error" => self.errors.push(
                v.get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("error")
                    .to_string(),
            ),
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
        }
        // R23-21: counters that are not whole numbers are no exact usage: unknown (reserved)
        if let Some(u) = self
            .completed_usage
            .as_ref()
            .filter(|u| crate::usage::usage_is_exact(u, crate::usage::CODEX_USAGE_KEYS))
        {
            o.usage = Some(codex_buckets(&raw_counters(u), subsets));
            // NB: on a resumed thread these counters are cumulative (R21) — the caller stores the delta.
            o.accounting = Accounting::Exact;
        } else {
            // No live usage in exec mode (R41): a turn without turn.completed is UNKNOWN, never 0.
            o.accounting = Accounting::Unknown;
        }
        // EC-02: the outcome is decided after the stream: only `turn.failed`, or no
        // `turn.completed`, fails the turn. `error` lines are kept as warnings.
        o.warnings = self.errors.clone();
        if let Some(msg) = &self.failed {
            o.error_class = Some(classify_message(msg));
            o.error_detail = Some(msg.clone());
            return o;
        }
        if !self.turn_completed {
            // the stream ended early: the last `error` line, if any, is the best cause
            match self.errors.last() {
                Some(msg) => {
                    o.error_class = Some(classify_message(msg));
                    o.error_detail = Some(msg.clone());
                }
                None => {
                    o.error_class = Some(ErrorClass::ProcessCrashed);
                    o.error_detail = Some(format!(
                        "no turn.completed / turn.failed (exit {process_exit:?})"
                    ));
                }
            }
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

    /// A frozen configuration is not validated again at load, so the launch preflight
    /// refuses the overrides that config validation refuses (ARCH-05) — and only those.
    #[test]
    fn preflight_refuses_overrides_that_reset_the_sandbox() {
        let mut l = launch();
        assert_eq!(l.check(), Preflight::default());
        // the review's configurations that ran before the ARCH-05 checks keep running
        for (k, v) in [
            (
                "model_providers.foundry.query_params.api-version",
                "\"2025-04-01-preview\"",
            ),
            ("model_providers.foundry.request_max_retries", "4"),
            ("sandbox_workspace_write.network_access", "true"),
            ("sandbox_workspace_write.writable_roots", "[\"D:/cache\"]"),
        ] {
            l.settings.overrides.insert(k.into(), v.into());
        }
        assert_eq!(l.check(), Preflight::default());
        for (k, v) in [
            ("sandbox_mode", "\"danger-full-access\""),
            ("model_providers.foundry.base_url", "\"https://elsewhere\""),
        ] {
            let mut bad = l.clone();
            bad.settings.overrides.insert(k.into(), v.into());
            let p = bad.check();
            assert!(p.warnings.is_empty(), "{p:?}");
            assert!(
                p.refused.len() == 1
                    && p.refused[0].starts_with(&format!("codex.overrides.{k} re-sets")),
                "{p:?}"
            );
            assert_eq!(bad.preflight(), p.refused);
        }
    }

    /// `read-only` is stricter than the plan's workspace-write: a warning, and the turn runs
    /// (as before the ARCH-05 checks); a weaker or unknown mode is refused.
    #[test]
    fn preflight_warns_on_read_only_and_refuses_weaker_modes() {
        let mut l = launch();
        l.settings.sandbox_mode = "read-only".into();
        let want = "sandbox_mode is `read-only`, plan requires workspace-write".to_string();
        assert_eq!(
            l.check(),
            Preflight {
                refused: vec![],
                warnings: vec![want.clone()],
            }
        );
        assert_eq!(l.preflight(), vec![want]);
        for mode in ["danger-full-access", "workspace_write", ""] {
            l.settings.sandbox_mode = mode.into();
            let p = l.check();
            assert!(p.warnings.is_empty(), "{mode}: {p:?}");
            assert_eq!(
                p.refused,
                vec![format!(
                    "sandbox_mode is `{mode}`, plan requires workspace-write"
                )]
            );
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

    /// R23-21 (review 2026-10-23): counters that are not whole numbers (a float, a negative
    /// number, a string — a field a CLI update retyped) are no exact usage. Was: read as 0 and
    /// recorded as `Exact`, so the turn's tokens never reached the budget. Now `Unknown`, which
    /// holds a reservation (R41).
    #[test]
    fn usage_counters_that_are_not_whole_numbers_are_unknown() {
        for usage in [
            r#"{"input_tokens":1000.0,"output_tokens":120}"#,
            r#"{"input_tokens":1000,"output_tokens":-5}"#,
            r#"{"input_tokens":"1000","output_tokens":120}"#,
            r#"{"total":1120}"#,
        ] {
            let mut p = CodexParser::new();
            p.feed_line(br#"{"type":"thread.started","thread_id":"th_1"}"#);
            p.feed_line(format!(r#"{{"type":"turn.completed","usage":{usage}}}"#).as_bytes());
            let o = p.finish(Some(0), CodexSubsets::default());
            assert_eq!(o.accounting, Accounting::Unknown, "{usage}");
            assert!(o.usage.is_none(), "{usage}");
            assert!(o.raw_usage.is_some(), "the raw object is kept: {usage}");
        }
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
