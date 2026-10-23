//! Claude Code headless launch (plan §6.2, R16, R17, R30, R44b) and stream parser.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;
use serde_json::Value;

use super::{ErrorClass, TurnOutcome};
use crate::domain::permission_denials_detail;
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
    /// Pre-approved tools (`--allowedTools`); `None` = same as `tools`.
    pub allowed_tools: Option<Vec<String>>,
    /// `dontAsk` for the read-only roles; `acceptEdits` for the fallback Implementer.
    pub permission_mode: String,
    /// Pass the run store as `--add-dir`. Off for the Implementer: every working directory is
    /// writable under `acceptEdits`, and the run store (TASK.md, PLAN.md) must stay read-only.
    pub add_run_store: bool,
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
            allowed_tools: None,
            permission_mode: "dontAsk".into(),
            add_run_store: true,
            drop_flags: Vec::new(),
            extra_args: Vec::new(),
        }
    }

    /// Claude as the fallback Implementer: file tools only, no shell (Bash/PowerShell are not
    /// in `--tools`, and `--restricted` removes code-running tools anyway). `acceptEdits`
    /// auto-approves edits inside the working directory (the worktree) only; with
    /// `--restricted` the file tools are confined to it, and `--permission-prompts none`
    /// denies anything that would ask (writes elsewhere, settings / git / tool config).
    pub fn implementer(exe: PathBuf, worktree: PathBuf) -> Self {
        let mut l = Self::read_only(exe, worktree.clone(), worktree);
        l.tools = ["Read", "Grep", "Glob", "Edit", "Write"]
            .map(String::from)
            .to_vec();
        l.allowed_tools = Some(["Read", "Grep", "Glob"].map(String::from).to_vec());
        l.permission_mode = "acceptEdits".into();
        l.add_run_store = false;
        l
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
        if self.add_run_store || self.mode == LaunchMode::CwdRunStore {
            a.extend(["--add-dir".into(), add_dir.display().to_string()]);
        }
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
            self.permission_mode.clone(),
            "--permission-prompts".into(),
            "none".into(),
        ]);
        let tools = self.tools.join(",");
        let allowed = self
            .allowed_tools
            .as_ref()
            .map(|t| t.join(","))
            .unwrap_or_else(|| tools.clone());
        a.extend(["--tools".into(), tools, "--allowedTools".into(), allowed]);
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

/// Provisional usage of one turn (R17, R41). `message_start` / `message_delta` stream events
/// and `assistant` messages carry usage that is cumulative per message, so the turn's usage
/// so far is the maximum of each counter per message id, summed over the messages. The
/// parser keeps it for a turn killed before its `result`; the live `Usage` events of a
/// running turn come from it too (EC-05).
#[derive(Debug, Clone, Default)]
pub struct ProvisionalUsage {
    per_message: BTreeMap<String, Value>,
    /// `message_delta` carries no id: it belongs to the last `message_start`.
    current: Option<String>,
}

impl ProvisionalUsage {
    /// Fold one parsed stream line in; `true` when the line carried usage.
    pub fn observe(&mut self, v: &Value) -> bool {
        match v.get("type").and_then(Value::as_str) {
            Some("stream_event") => {
                let e = v.get("event").unwrap_or(&Value::Null);
                match e.get("type").and_then(Value::as_str) {
                    Some("message_start") => {
                        let m = e.get("message").unwrap_or(&Value::Null);
                        let Some(id) = m.get("id").and_then(Value::as_str) else {
                            return false;
                        };
                        self.current = Some(id.to_string());
                        m.get("usage")
                            .is_some_and(|u| self.merge(id.to_string(), u))
                    }
                    Some("message_delta") => match (self.current.clone(), e.get("usage")) {
                        (Some(id), Some(u)) => self.merge(id, u),
                        _ => false,
                    },
                    _ => false,
                }
            }
            Some("assistant") => {
                let m = v.get("message").unwrap_or(&Value::Null);
                match (m.get("id").and_then(Value::as_str), m.get("usage")) {
                    (Some(id), Some(u)) => self.merge(id.to_string(), u),
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// Keep the maximum seen value per counter for one message (deltas are cumulative per message).
    fn merge(&mut self, id: String, u: &Value) -> bool {
        let entry = self
            .per_message
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
        true
    }

    /// The turn's usage so far; `None` before the first usage-carrying line.
    pub fn buckets(&self) -> Option<Buckets> {
        if self.per_message.is_empty() {
            return None;
        }
        let mut total = Buckets::default();
        for u in self.per_message.values() {
            total.add(&claude_buckets(u));
        }
        Some(total)
    }
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
    /// The CLI was retrying when the stream last spoke: an `api_retry` with no API answer
    /// (`message_start` / `assistant`) after it (EC-04).
    retry_pending: bool,
    /// The failed API call's category on the latest `assistant` message (SDK `error`).
    assistant_error: Option<String>,
    protocol_errors: Vec<String>,
    init: Option<Value>,
    text: String,
    tools: Vec<String>,
    usage: ProvisionalUsage,
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
                Some("api_retry") => {
                    self.retries.push(v.clone());
                    self.retry_pending = true;
                }
                _ => {}
            },
            "stream_event" => self.stream_event(&v),
            "assistant" => self.assistant(&v),
            "result" => self.terminal = Some(v),
            other => *self.ignored_types.entry(other.to_string()).or_default() += 1,
        }
    }

    fn stream_event(&mut self, v: &Value) {
        self.usage.observe(v);
        let e = v.get("event").unwrap_or(&Value::Null);
        match e.get("type").and_then(Value::as_str) {
            // the API answered: an earlier api_retry has recovered
            Some("message_start") => self.retry_pending = false,
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
        self.usage.observe(v);
        self.retry_pending = false;
        // the latest message decides: an error on an earlier message is not the turn's
        self.assistant_error = v.get("error").and_then(Value::as_str).map(str::to_string);
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
            if let Some(b) = self.usage.buckets() {
                o.usage = Some(b);
                o.accounting = Accounting::Provisional;
            }
            // EC-04: the retry category names the cause only if the CLI was still retrying
            // when the stream ended; a retry the API recovered from is not why it died
            if self.retry_pending {
                if let Some(c) = self.last_retry_class() {
                    o.error_class = Some(c);
                }
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
        // R23-21: a usage object whose counters are not whole numbers is no exact usage — the
        // live provisional values (if any) stand, else the turn's usage is unknown (reserved)
        let exact = r
            .get("usage")
            .filter(|u| crate::usage::usage_is_exact(u, crate::usage::CLAUDE_USAGE_KEYS));
        if let Some(u) = r.get("usage") {
            o.raw_usage = Some(u.clone());
        }
        if let Some(u) = exact {
            o.usage = Some(claude_buckets(u));
            o.accounting = Accounting::Exact;
        } else if let Some(b) = self.usage.buckets() {
            o.usage = Some(b);
            o.accounting = Accounting::Provisional;
        }
        if !o.success {
            let detail = match r.get("result") {
                Some(Value::String(s)) if !s.is_empty() => s.clone(),
                _ => match r.get("errors") {
                    Some(e) if !e.is_null() => e.to_string(),
                    _ => format!("Claude result {subtype}"),
                },
            };
            o.error_class = Some(self.failure_class(r, &detail));
            o.error_detail = Some(detail);
        } else if !o.permission_denials.is_empty() {
            // Plan §6.2: any denial fails the stage.
            o.success = false;
            o.error_class = Some(ErrorClass::PermissionDenied);
            o.error_detail = Some(permission_denials_detail(o.permission_denials.len()));
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

    /// EC-04: the class of a failed `result`, from structured fields first — the result
    /// `subtype`, then the failed API call's category (the latest `assistant` message's
    /// `error`) or HTTP status (`api_error_status`) — and from the text only without them.
    /// A category or a status cannot tell a context overflow or an exhausted quota from
    /// any other bad request or throttle (a subscription usage limit comes as a 429), so
    /// the text refines those two. An `api_retry` never decides once a result arrived.
    fn failure_class(&self, r: &Value, detail: &str) -> ErrorClass {
        if r.get("subtype").and_then(Value::as_str) == Some("error_max_turns") {
            return ErrorClass::InvalidOutput;
        }
        let text = classify_text(detail);
        let structured = self
            .assistant_error
            .as_deref()
            .map(classify_api_error)
            .filter(|c| *c != ErrorClass::Unknown)
            .or_else(|| {
                r.get("api_error_status")
                    .and_then(Value::as_u64)
                    .and_then(status_class)
            });
        match structured {
            Some(ErrorClass::BadRequest | ErrorClass::RateLimited)
                if matches!(
                    text,
                    ErrorClass::ContextWindowExceeded | ErrorClass::QuotaExceeded
                ) =>
            {
                text
            }
            Some(c) => c,
            None => text,
        }
    }
}

/// HTTP status of a failed API call (`api_error_status` of the result).
fn status_class(status: u64) -> Option<ErrorClass> {
    Some(match status {
        429 => ErrorClass::RateLimited,
        500 | 502 | 503 | 504 | 529 => ErrorClass::Overloaded,
        401 | 403 => ErrorClass::Auth,
        400 | 404 | 413 | 422 => ErrorClass::BadRequest,
        _ => return None,
    })
}

/// `system/api_retry.error` categories (plan §6.2); the `assistant` message's `error` uses
/// the same names.
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

/// Failure texts of a Claude `result`, most specific first (EC-04). Status codes need word
/// boundaries: "403" must not match inside "214030 tokens".
static CLAUDE_TEXT: LazyLock<Vec<(Regex, ErrorClass)>> = LazyLock::new(|| {
    [
        // the `-p` result text "Prompt is too long", "…: context window exceeded", "Context
        // limit reached · /compact or /clear to continue" (not the quota "limit reached")
        (
            r"(?i)prompt is too long|context[ _-]?(?:window|length|limit)|maximum context",
            ErrorClass::ContextWindowExceeded,
        ),
        // Exhaustion (the implementer fallback keys on it, R49), in words the API sends
        // even with a 429 status: "Claude AI usage limit reached|<reset>", "Credit
        // balance is too low".
        (
            r"(?i)usage limit|hit your limit|credit balance|billing[ _]error|account[ _]on[ _]hold|extra usage|insufficient_?quota|exceeded your current quota",
            ErrorClass::QuotaExceeded,
        ),
        // throttling, before the generic words below: "429 rate limit reached" is transient
        (
            r"(?i)\b429\b|rate[ _-]?limit|too many requests",
            ErrorClass::RateLimited,
        ),
        // "5-hour limit reached", "Opus weekly limit reached"
        (
            r"(?i)limit reached|\bquota\b|quota_?exceeded|\bbilling\b",
            ErrorClass::QuotaExceeded,
        ),
        // "Invalid API key · Please run /login", "OAuth token has expired", 401 / 403
        (
            r"(?i)\b40[13]\b|invalid[ _]api[ _]key|/login\b|\blog ?in\b|auth|forbidden",
            ErrorClass::Auth,
        ),
        // "API Error: 529 Overloaded", "500 Internal server error"
        (
            r"(?i)\b529\b|overloaded|\b50[0234]\b|server[ _]error",
            ErrorClass::Overloaded,
        ),
        // the earlier broad rule, last: "context" with "window" or "too long" anywhere
        (
            r"(?is)context.*(?:window|too long)|(?:window|too long).*context",
            ErrorClass::ContextWindowExceeded,
        ),
    ]
    .into_iter()
    .map(|(re, class)| (Regex::new(re).expect("regex"), class))
    .collect()
});

fn classify_text(s: &str) -> ErrorClass {
    CLAUDE_TEXT
        .iter()
        .find(|(re, _)| re.is_match(s))
        .map_or(ErrorClass::Unknown, |(_, class)| *class)
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
    fn quota_texts_are_quota_exceeded() {
        assert_eq!(
            classify_text("Claude AI usage limit reached|1790000000"),
            ErrorClass::QuotaExceeded
        );
        assert_eq!(
            classify_text("Credit balance is too low"),
            ErrorClass::QuotaExceeded
        );
        assert_eq!(
            classify_text("API Error: 429 rate limit"),
            ErrorClass::RateLimited
        );
    }

    #[test]
    fn argv_implementer_has_file_tools_no_shell_no_run_store() {
        let l = ClaudeLaunch::implementer("claude.exe".into(), "W".into());
        let (cwd, a) = l.argv();
        assert_eq!(cwd, PathBuf::from("W"));
        let s = a.join(" ");
        assert!(s.starts_with("-p --restricted --safe-mode --settings"));
        assert!(!s.contains("--add-dir"), "{s}");
        assert!(s.contains("--permission-mode acceptEdits --permission-prompts none"));
        assert!(s.contains("--tools Read,Grep,Glob,Edit,Write --allowedTools Read,Grep,Glob "));
        assert!(!s.contains("Bash") && !s.contains("PowerShell"));
        assert!(s.contains("--disallowedTools mcp__* --strict-mcp-config"));
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

    /// R23-21 (review 2026-10-23): a result whose usage counters are not whole numbers is no
    /// exact usage — the live provisional values stand (here: none, so the usage is unknown).
    /// Was: read as 0 and recorded as `Exact`.
    #[test]
    fn usage_counters_that_are_not_whole_numbers_are_not_exact() {
        let mut p = ClaudeParser::new();
        feed(
            &mut p,
            &[
                INIT,
                r#"{"type":"result","subtype":"success","is_error":false,"session_id":"s1","result":"x","usage":{"input_tokens":12.0,"output_tokens":"100"}}"#,
            ],
        );
        let o = p.finish(Some(0));
        assert_ne!(o.accounting, Accounting::Exact);
        assert!(o.usage.is_none());
        let mut p = ClaudeParser::new();
        feed(
            &mut p,
            &[
                INIT,
                r#"{"type":"result","subtype":"success","is_error":false,"session_id":"s1","result":"x","usage":{"input_tokens":12,"output_tokens":100}}"#,
            ],
        );
        let o = p.finish(Some(0));
        assert_eq!(o.accounting, Accounting::Exact);
        assert_eq!(o.usage.unwrap().output, 100);
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
