//! Claude Code headless launch (plan §6.2, R16, R17, R30, R44b) and stream parser.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use super::{ErrorClass, TurnOutcome};
use crate::usage::{claude_buckets, Accounting, Buckets};

/// Launch mode under test in M0 (R30 preferred + the two §6.2 fallbacks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
pub enum LaunchMode {
    /// `--restricted --safe-mode` (preferred, R30).
    Restricted,
    /// `--safe-mode --setting-sources ""` (fallback 1).
    SettingSources,
    /// `--safe-mode`, cwd = run store, worktree via `--add-dir` (fallback 2).
    CwdRunStore,
}

#[derive(Debug, Clone)]
pub struct ClaudeLaunch {
    pub exe: PathBuf,
    pub mode: LaunchMode,
    pub worktree: PathBuf,
    pub run_store: PathBuf,
    /// Inline JSON schema text (`--json-schema` takes the schema itself).
    pub json_schema: Option<String>,
    pub context_file: Option<PathBuf>,
    pub model: Option<String>,
    pub resume: Option<String>,
    /// Role tools (R16). Planner/Reviewer: Read, Grep, Glob.
    pub tools: Vec<String>,
    /// M0 diagnostics only: flags to drop / extra args to append, recorded in the capture.
    pub drop_flags: Vec<String>,
    pub extra_args: Vec<String>,
}

impl ClaudeLaunch {
    pub fn read_only(exe: PathBuf, worktree: PathBuf, run_store: PathBuf) -> Self {
        Self {
            exe,
            mode: LaunchMode::Restricted,
            worktree,
            run_store,
            json_schema: None,
            context_file: None,
            model: None,
            resume: None,
            tools: vec!["Read".into(), "Grep".into(), "Glob".into()],
            drop_flags: Vec::new(),
            extra_args: Vec::new(),
        }
    }

    /// (cwd, argv without the program). Prompt goes on stdin.
    pub fn argv(&self) -> (PathBuf, Vec<String>) {
        let mut a: Vec<String> = vec!["-p".into()];
        let (cwd, add_dir): (&Path, &Path) = match self.mode {
            LaunchMode::Restricted => {
                a.extend(["--restricted".into(), "--safe-mode".into()]);
                (&self.worktree, &self.run_store)
            }
            LaunchMode::SettingSources => {
                a.extend([
                    "--safe-mode".into(),
                    "--setting-sources".into(),
                    String::new(),
                ]);
                (&self.worktree, &self.run_store)
            }
            LaunchMode::CwdRunStore => {
                a.push("--safe-mode".into());
                (&self.run_store, &self.worktree)
            }
        };
        a.extend(["--settings".into(), r#"{"disableAllHooks":true}"#.into()]);
        a.extend(["--add-dir".into(), add_dir.display().to_string()]);
        a.extend([
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            "--include-partial-messages".into(),
        ]);
        if let Some(s) = &self.json_schema {
            a.extend(["--json-schema".into(), s.clone()]);
        }
        if let Some(c) = &self.context_file {
            a.extend([
                "--append-system-prompt-file".into(),
                c.display().to_string(),
            ]);
        }
        a.extend([
            "--permission-mode".into(),
            "dontAsk".into(),
            "--permission-prompts".into(),
            "none".into(),
        ]);
        let tools = self.tools.join(",");
        a.extend([
            "--tools".into(),
            tools.clone(),
            "--allowedTools".into(),
            tools,
        ]);
        a.extend([
            "--disallowedTools".into(),
            "mcp__*".into(),
            "--strict-mcp-config".into(),
        ]);
        if let Some(m) = &self.model {
            a.extend(["--model".into(), m.clone()]);
        }
        if let Some(r) = &self.resume {
            a.extend(["--resume".into(), r.clone()]);
        }
        let a = drop_flags(a, &self.drop_flags);
        let mut a = a;
        a.extend(self.extra_args.iter().cloned());
        (cwd.to_path_buf(), a)
    }
}

/// Remove `--flag` and, for flags that take a value, its value.
fn drop_flags(args: Vec<String>, drop: &[String]) -> Vec<String> {
    const TAKES_VALUE: &[&str] = &[
        "--setting-sources",
        "--settings",
        "--add-dir",
        "--output-format",
        "--json-schema",
        "--append-system-prompt-file",
        "--permission-mode",
        "--permission-prompts",
        "--tools",
        "--allowedTools",
        "--disallowedTools",
        "--model",
        "--resume",
    ];
    let mut out = Vec::new();
    let mut skip_next = false;
    for x in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if drop.iter().any(|d| d == &x) {
            skip_next = TAKES_VALUE.contains(&x.as_str());
            continue;
        }
        out.push(x);
    }
    out
}

/// Stream parser following the zeroshot transcript rules adopted in R17.
#[derive(Default)]
pub struct ClaudeParser {
    session_id: Option<String>,
    terminal: Option<Value>,
    after_terminal: u32,
    malformed: u32,
    ignored_types: BTreeMap<String, u32>,
    retries: Vec<Value>,
    protocol_errors: Vec<String>,
    init: Option<Value>,
    text: String,
    tools: Vec<String>,
    /// provisional usage per message id
    provisional: BTreeMap<String, Value>,
    current_msg: Option<String>,
}

impl ClaudeParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed_line(&mut self, line: &[u8]) {
        if line.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        if self.terminal.is_some() {
            self.after_terminal += 1; // first result is terminal; discard the rest
            return;
        }
        let v: Value = match serde_json::from_slice(line) {
            Ok(v) => v,
            Err(_) => {
                self.malformed += 1;
                return;
            }
        };
        let ty = v
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if let Some(sid) = v.get("session_id").and_then(Value::as_str) {
            match &self.session_id {
                None => self.session_id = Some(sid.to_string()),
                Some(s) if s != sid => self.protocol_errors.push(format!(
                    "session_id changed from {s} to {sid} within one turn"
                )),
                _ => {}
            }
        }
        match ty.as_str() {
            "system" => match v.get("subtype").and_then(Value::as_str) {
                Some("init") => self.init = Some(v.clone()),
                Some("api_retry") => self.retries.push(v.clone()),
                _ => {}
            },
            "stream_event" => self.stream_event(v.get("event").unwrap_or(&Value::Null)),
            "assistant" => self.assistant(&v),
            "result" => self.terminal = Some(v),
            other => *self.ignored_types.entry(other.to_string()).or_default() += 1,
        }
    }

    fn stream_event(&mut self, e: &Value) {
        match e.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                let m = e.get("message").unwrap_or(&Value::Null);
                if let Some(id) = m.get("id").and_then(Value::as_str) {
                    self.current_msg = Some(id.to_string());
                    if let Some(u) = m.get("usage") {
                        self.merge_usage(id.to_string(), u);
                    }
                }
            }
            Some("message_delta") => {
                if let (Some(id), Some(u)) = (self.current_msg.clone(), e.get("usage")) {
                    self.merge_usage(id, u);
                }
            }
            Some("content_block_delta") => {
                if let Some(t) = e.pointer("/delta/text").and_then(Value::as_str) {
                    self.text.push_str(t);
                }
            }
            Some("content_block_start") => {
                if let Some(cb) = e.get("content_block") {
                    if cb.get("type").and_then(Value::as_str) == Some("tool_use") {
                        self.tools.push(
                            cb.get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("?")
                                .to_string(),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    fn assistant(&mut self, v: &Value) {
        let m = v.get("message").unwrap_or(&Value::Null);
        if let (Some(id), Some(u)) = (m.get("id").and_then(Value::as_str), m.get("usage")) {
            self.merge_usage(id.to_string(), u);
        }
    }

    /// Keep the maximum seen value per counter for one message (deltas are cumulative per message).
    fn merge_usage(&mut self, id: String, u: &Value) {
        let entry = self
            .provisional
            .entry(id)
            .or_insert_with(|| Value::Object(Default::default()));
        if let (Some(dst), Some(src)) = (entry.as_object_mut(), u.as_object()) {
            for (k, v) in src {
                if let Some(n) = v.as_u64() {
                    let cur = dst.get(k).and_then(Value::as_u64).unwrap_or(0);
                    dst.insert(k.clone(), Value::from(cur.max(n)));
                }
            }
        }
    }

    fn provisional_buckets(&self) -> Option<Buckets> {
        if self.provisional.is_empty() {
            return None;
        }
        let mut total = Buckets::default();
        for u in self.provisional.values() {
            total.add(&claude_buckets(u));
        }
        Some(total)
    }

    pub fn finish(self, process_exit: Option<i32>) -> TurnOutcome {
        let mut o = TurnOutcome::empty();
        o.session_id = self.session_id.clone();
        o.malformed_lines = self.malformed;
        o.protocol_errors = self.protocol_errors.clone();
        o.init = self.init.clone();
        o.tool_activity = self.tools.clone();
        o.retries = self.retries.clone();
        let Some(r) = &self.terminal else {
            // No terminal event → ProcessCrashed; retryable if the CLI was retrying.
            o.error_class = Some(ErrorClass::ProcessCrashed);
            o.error_detail = Some(format!(
                "no result event (exit {:?}, {} malformed lines)",
                process_exit, self.malformed
            ));
            o.retryable = !self.retries.is_empty();
            o.final_text = self.text.clone();
            if let Some(b) = self.provisional_buckets() {
                o.usage = Some(b);
                o.accounting = Accounting::Provisional;
            }
            if let Some(c) = self.last_retry_class() {
                o.error_class = Some(c);
            }
            return o;
        };
        let subtype = r.get("subtype").and_then(Value::as_str).unwrap_or("");
        let is_error = r.get("is_error").and_then(Value::as_bool).unwrap_or(false);
        o.success = subtype == "success" && !is_error;
        o.structured = r.get("structured_output").filter(|v| !v.is_null()).cloned();
        o.final_text = match r.get("result") {
            Some(Value::String(s)) => s.clone(),
            _ => self.text.clone(),
        };
        if o.structured.is_none() && o.success {
            // Payload = result when no structured output (R17)
            o.structured = serde_json::from_str::<Value>(&o.final_text).ok();
        }
        o.permission_denials = r
            .get("permission_denials")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        o.vendor_cost_usd = r.get("total_cost_usd").and_then(Value::as_f64);
        if let Some(u) = r.get("usage") {
            o.usage = Some(claude_buckets(u));
            o.raw_usage = Some(u.clone());
            o.accounting = Accounting::Exact;
        } else if let Some(b) = self.provisional_buckets() {
            o.usage = Some(b);
            o.accounting = Accounting::Provisional;
        }
        if !o.success {
            o.error_detail = Some(match r.get("result") {
                Some(Value::String(s)) if !s.is_empty() => s.clone(),
                _ => match r.get("errors") {
                    Some(e) if !e.is_null() => e.to_string(),
                    _ => format!("Claude result {subtype}"),
                },
            });
            o.error_class = Some(self.last_retry_class().unwrap_or(match subtype {
                "error_max_turns" => ErrorClass::InvalidOutput,
                _ => classify_text(o.error_detail.as_deref().unwrap_or("")),
            }));
        } else if !o.permission_denials.is_empty() {
            // Plan §6.2: any denial fails the stage.
            o.success = false;
            o.error_class = Some(ErrorClass::PermissionDenied);
            o.error_detail = Some(format!(
                "{} permission denial(s)",
                o.permission_denials.len()
            ));
        }
        if self.after_terminal > 0 {
            o.protocol_errors.push(format!(
                "{} line(s) after the terminal result were discarded",
                self.after_terminal
            ));
        }
        o
    }

    fn last_retry_class(&self) -> Option<ErrorClass> {
        let r = self.retries.last()?;
        Some(classify_api_error(
            r.get("error").and_then(Value::as_str).unwrap_or(""),
        ))
    }
}

/// `system/api_retry.error` categories (plan §6.2).
pub fn classify_api_error(category: &str) -> ErrorClass {
    match category {
        "rate_limit" => ErrorClass::RateLimited,
        "overloaded" | "server_error" => ErrorClass::Overloaded,
        "billing_error" | "account_on_hold" => ErrorClass::QuotaExceeded,
        "authentication_failed" | "oauth_org_not_allowed" | "cloud_credential_error" => {
            ErrorClass::Auth
        }
        "invalid_request" | "model_not_found" => ErrorClass::BadRequest,
        "max_output_tokens" => ErrorClass::InvalidOutput,
        _ => ErrorClass::Unknown,
    }
}

fn classify_text(s: &str) -> ErrorClass {
    let l = s.to_ascii_lowercase();
    if l.contains("rate limit") || l.contains("429") {
        ErrorClass::RateLimited
    } else if l.contains("auth") || l.contains("401") || l.contains("403") || l.contains("log in") {
        ErrorClass::Auth
    } else if l.contains("context") && (l.contains("window") || l.contains("too long")) {
        ErrorClass::ContextWindowExceeded
    } else {
        ErrorClass::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(p: &mut ClaudeParser, lines: &[&str]) {
        for l in lines {
            p.feed_line(l.as_bytes());
        }
    }

    #[test]
    fn argv_restricted_matches_plan() {
        let mut l = ClaudeLaunch::read_only("claude.exe".into(), "W".into(), "R".into());
        l.json_schema = Some("{}".into());
        l.context_file = Some("ctx.md".into());
        l.model = Some("opus".into());
        let (cwd, a) = l.argv();
        assert_eq!(cwd, PathBuf::from("W"));
        let s = a.join(" ");
        assert!(s.starts_with(
            "-p --restricted --safe-mode --settings {\"disableAllHooks\":true} --add-dir R"
        ));
        assert!(s.contains("--output-format stream-json --verbose --include-partial-messages"));
        assert!(s.contains("--permission-mode dontAsk --permission-prompts none"));
        assert!(s.contains("--tools Read,Grep,Glob --allowedTools Read,Grep,Glob --disallowedTools mcp__* --strict-mcp-config"));
        assert!(!s.contains("--bare"));
    }

    #[test]
    fn argv_fallbacks() {
        let mut l = ClaudeLaunch::read_only("c".into(), "W".into(), "R".into());
        l.mode = LaunchMode::SettingSources;
        let (_, a) = l.argv();
        let i = a.iter().position(|x| x == "--setting-sources").unwrap();
        assert_eq!(a[i + 1], "", "empty value passed as its own argv element");
        assert!(!a.contains(&"--restricted".to_string()));
        l.mode = LaunchMode::CwdRunStore;
        let (cwd, a) = l.argv();
        assert_eq!(cwd, PathBuf::from("R"));
        let i = a.iter().position(|x| x == "--add-dir").unwrap();
        assert_eq!(a[i + 1], "W");
    }

    #[test]
    fn drop_flag_removes_value_too() {
        let mut l = ClaudeLaunch::read_only("c".into(), "W".into(), "R".into());
        l.drop_flags = vec!["--permission-prompts".into(), "--restricted".into()];
        let (_, a) = l.argv();
        assert!(!a
            .iter()
            .any(|x| x == "--permission-prompts" || x == "--restricted"));
        assert!(a.contains(&"--permission-mode".to_string()));
        assert!(a.contains(&"dontAsk".to_string()));
    }

    // Synthetic lines shaped like the documented stream-json protocol. M0 replaces
    // them with captured fixtures (fixtures/captured/…).
    const INIT: &str = r#"{"type":"system","subtype":"init","session_id":"s1","tools":["Read","Grep","Glob"],"mcp_servers":[]}"#;
    const START: &str = r#"{"type":"stream_event","session_id":"s1","event":{"type":"message_start","message":{"id":"m1","usage":{"input_tokens":12,"cache_read_input_tokens":3000,"cache_creation_input_tokens":0,"output_tokens":1}}}}"#;
    const DELTA_TXT: &str = r#"{"type":"stream_event","session_id":"s1","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hel"}}}"#;
    const DELTA_USE: &str = r#"{"type":"stream_event","session_id":"s1","event":{"type":"message_delta","usage":{"output_tokens":40}}}"#;

    #[test]
    fn success_with_structured_output() {
        let mut p = ClaudeParser::new();
        feed(
            &mut p,
            &[
                INIT,
                START,
                DELTA_TXT,
                DELTA_USE,
                r#"{"type":"result","subtype":"success","is_error":false,"session_id":"s1","result":"done","structured_output":{"decision":"approved"},"usage":{"input_tokens":12,"cache_read_input_tokens":3000,"cache_creation_input_tokens":0,"output_tokens":40},"total_cost_usd":0.01,"permission_denials":[]}"#,
                r#"{"type":"result","subtype":"success","session_id":"s1"}"#,
            ],
        );
        let o = p.finish(Some(0));
        assert!(o.success);
        assert_eq!(o.structured.unwrap()["decision"], "approved");
        assert_eq!(o.accounting, Accounting::Exact);
        assert_eq!(o.usage.unwrap().total(), 3052);
        assert_eq!(
            o.protocol_errors.len(),
            1,
            "second result discarded and reported"
        );
        assert!(o.init.is_some());
    }

    #[test]
    fn killed_turn_counts_provisional_usage() {
        let mut p = ClaudeParser::new();
        feed(&mut p, &[INIT, START, DELTA_USE, "{not json"]);
        let o = p.finish(None);
        assert!(!o.success);
        assert_eq!(o.error_class, Some(ErrorClass::ProcessCrashed));
        assert_eq!(o.accounting, Accounting::Provisional);
        let u = o.usage.unwrap();
        assert_eq!(u.output, 40);
        assert_eq!(u.cache_read, 3000);
        assert_eq!(o.malformed_lines, 1);
    }

    #[test]
    fn permission_denial_fails_the_stage() {
        let mut p = ClaudeParser::new();
        feed(
            &mut p,
            &[
                INIT,
                r#"{"type":"result","subtype":"success","is_error":false,"session_id":"s1","result":"x","permission_denials":[{"tool_name":"Read","tool_input":{"file_path":"C:\\outside.txt"}}]}"#,
            ],
        );
        let o = p.finish(Some(0));
        assert!(!o.success);
        assert_eq!(o.error_class, Some(ErrorClass::PermissionDenied));
        assert_eq!(o.permission_denials.len(), 1);
    }

    #[test]
    fn retry_category_classifies_failure_and_session_change_is_protocol_error() {
        let mut p = ClaudeParser::new();
        feed(
            &mut p,
            &[
                INIT,
                r#"{"type":"system","subtype":"api_retry","session_id":"s1","error":"rate_limit","error_status":429}"#,
                r#"{"type":"system","subtype":"api_retry","session_id":"s2","error":"rate_limit","error_status":429}"#,
            ],
        );
        let o = p.finish(Some(1));
        assert_eq!(o.error_class, Some(ErrorClass::RateLimited));
        assert!(o.retryable);
        assert_eq!(o.protocol_errors.len(), 1);
    }
}
