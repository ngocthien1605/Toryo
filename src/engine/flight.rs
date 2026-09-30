//! The flight recorder (M10, ARCH-02): the engine's typed events — what happened in a run,
//! as data. The engine emits an [`EngineEvent`] at every state change, stage, turn, checkpoint, verify command,
//! gate, budget warning, escalation and pause; a [`Recorder`] wraps each one in an
//! [`Envelope`] (version, per-task sequence number, time, task, lock generation) and hands it
//! to its sinks:
//! - the **flight recorder** ([`FileSink`]): one JSON object per line in
//!   `<data_dir>\runs\<id>\events.jsonl`, next to `runstore\` and `captures\`;
//! - the stderr renderer and the notifier (added by later M10 tasks).
//!
//! Free text (reasons) goes through the same redaction as captures (R26/R42) before any sink
//! sees it. A sink error never fails the run: it is reported once on stderr.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::state::{GateKind, Machine, PipelineState as St};
use super::store::{AttemptStatus, TaskRow};
use crate::config::Effective;
use crate::process::redact::{Redactions, Secret};
use crate::usage::{Accounting, Buckets};

/// The flight recorder's file name in a task's run store.
pub const EVENTS_FILE: &str = "events.jsonl";

/// The envelope version (`v`).
pub const VERSION: u32 = 1;

/// A turn's usage as the engine recorded it: the four buckets and the accounting (R41).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnUsage {
    #[serde(flatten)]
    pub buckets: Buckets,
    pub accounting: Accounting,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    /// A drive command (`run`, `resume`, `approve --continue`) took the task; the envelope
    /// carries the lock generation.
    RunStarted {
        owner: String,
    },
    /// `event` is the pipeline event's name (`plan_ready`, `escalate`, …; `recovery` for what
    /// the §10 recovery changed).
    StateChanged {
        from: St,
        to: St,
        event: String,
    },
    /// `detail`: what the stage header shows after the profile (the verify boundary and
    /// checkout, a resumed review session).
    StageStarted {
        stage: String,
        attempt: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    StageFinished {
        stage: String,
        attempt: u32,
        status: AttemptStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_class: Option<String>,
        duration_ms: u64,
    },
    TurnFinished {
        stage: String,
        attempt: u32,
        profile: String,
        success: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<TurnUsage>,
        /// R2: the reservation of a turn with no telemetry (then `usage` is absent).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reserved_tokens: Option<i64>,
        duration_ms: u64,
    },
    /// `found`: the checkpoint was already committed (trailer match, R40).
    CheckpointCreated {
        head: String,
        kind: String,
        #[serde(default)]
        found: bool,
    },
    VerifyCommand {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit: Option<i32>,
        timed_out: bool,
        ok: bool,
        duration_ms: u64,
    },
    GateOpened {
        gate: String,
        occurrence: u32,
    },
    BudgetWarning {
        used: i64,
        cap: i64,
    },
    Escalated {
        reason_class: String,
        reason: String,
    },
    Paused {
        reason: String,
    },
    /// Emitted once on every exit of a drive command (R1). `from`: the state when the drive
    /// started; `error`: the command stopped with this error (redacted, shortened).
    RunEnded {
        state: St,
        next_action: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<St>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// An error in `run_ended` is shortened to this many characters.
pub const ERROR_MAX: usize = 400;

/// At most `max` characters; an ellipsis marks a cut.
pub fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

impl EngineEvent {
    /// The `type` tag.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::RunStarted { .. } => "run_started",
            Self::StateChanged { .. } => "state_changed",
            Self::StageStarted { .. } => "stage_started",
            Self::StageFinished { .. } => "stage_finished",
            Self::TurnFinished { .. } => "turn_finished",
            Self::CheckpointCreated { .. } => "checkpoint_created",
            Self::VerifyCommand { .. } => "verify_command",
            Self::GateOpened { .. } => "gate_opened",
            Self::BudgetWarning { .. } => "budget_warning",
            Self::Escalated { .. } => "escalated",
            Self::Paused { .. } => "paused",
            Self::RunEnded { .. } => "run_ended",
        }
    }
}

/// One line of `events.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u32,
    /// Per task, strictly increasing across runs (gaps allowed).
    pub seq: u64,
    /// RFC 3339, UTC.
    pub ts: String,
    pub task: String,
    /// The task lock generation of the process that emitted it (R38).
    pub generation: i64,
    #[serde(flatten)]
    pub event: EngineEvent,
    /// Not written: the CLI has never printed a line for this event at this point (e.g. a
    /// transition inside `finish_and`), so the live stderr renderer skips it and the text of
    /// a run stays what it was before M10. Other sinks (the file, `watch`) see it.
    #[serde(skip)]
    pub quiet: bool,
}

/// A consumer of engine events. An error is reported once by the [`Recorder`]; the run
/// goes on.
pub trait EventSink: Send + Sync {
    /// Shown in the one error report.
    fn name(&self) -> &str;
    fn emit(&self, e: &Envelope) -> Result<()>;
}

/// The flight recorder: appends one JSON object per line and flushes it. Opened per write, so
/// a crash loses at most the line being written; a partial last line left by a crash is closed
/// with a newline before the next one, so it never glues onto a good line.
pub struct FileSink {
    path: PathBuf,
}

impl FileSink {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl EventSink for FileSink {
    fn name(&self) -> &str {
        "flight recorder"
    }

    fn emit(&self, e: &Envelope) -> Result<()> {
        let mut line = serde_json::to_string(e)?;
        line.push('\n');
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&self.path)
            .with_context(|| format!("cannot write {}", self.path.display()))?;
        if ends_open(&mut f)? {
            line.insert(0, '\n');
        }
        f.write_all(line.as_bytes())
            .and_then(|_| f.flush())
            .with_context(|| format!("cannot write {}", self.path.display()))
    }
}

/// The text of an event: for the events the CLI printed before M10, exactly that text (the
/// stage headers start with an empty line); for the others (`quiet` live), a line for
/// `toryo watch`.
pub fn render(env: &Envelope) -> String {
    use EngineEvent as E;
    let secs = |ms: u64| ms as f64 / 1000.0;
    match &env.event {
        E::RunStarted { owner } => format!(
            "▶ {} — run started (generation {}, {owner})",
            env.task, env.generation
        ),
        E::StateChanged { from, to, event } => {
            // the gate rejections have always been printed as `--rejected-->`
            let name = match event.as_str() {
                "plan_rejected" | "final_rejected" => "rejected",
                n => n,
            };
            format!("   [state] {from:?} --{name}--> {to:?}")
        }
        E::StageStarted {
            stage,
            attempt,
            profile,
            detail,
        } => {
            let parts: Vec<&str> = [profile.as_deref(), detail.as_deref()]
                .into_iter()
                .flatten()
                .collect();
            format!(
                "\n── {} (attempt {attempt}, {}) ──",
                stage.to_uppercase(),
                parts.join(", ")
            )
        }
        E::StageFinished {
            stage,
            attempt,
            status,
            error_class,
            duration_ms,
        } => format!(
            "   {stage} attempt {attempt}: {}{} ({:.1}s)",
            serde_json::to_value(status)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default(),
            error_class
                .as_deref()
                .map(|c| format!(", {c}"))
                .unwrap_or_default(),
            secs(*duration_ms)
        ),
        E::TurnFinished {
            stage,
            attempt,
            profile,
            success,
            usage,
            reserved_tokens,
            duration_ms,
        } => format!(
            "   turn {stage}/{attempt} ({profile}): {} · {} ({:.1}s)",
            if *success { "completed" } else { "failed" },
            match usage {
                Some(u) => format!(
                    "{} tokens, {}",
                    u.buckets.total(),
                    serde_json::to_value(u.accounting)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default()
                ),
                None => match reserved_tokens {
                    Some(n) => format!("no usage reported, ~{n} reserved"),
                    None => "no usage reported".into(),
                },
            },
            secs(*duration_ms)
        ),
        E::CheckpointCreated { head, kind, found } => {
            let h = crate::git::short(head);
            let line = format!("✔ checkpoint {h} ({kind})");
            if *found {
                format!("✔ checkpoint {h} already committed (trailer match)\n{line}")
            } else {
                line
            }
        }
        E::VerifyCommand {
            name,
            exit,
            timed_out,
            ok,
            duration_ms,
        } => {
            let exit = match exit {
                Some(code) => format!("exit {code}"),
                None if *timed_out => "timed out".to_string(),
                None => "no exit code".to_string(),
            };
            format!(
                "  {} {name} ({exit}, {:.1}s)",
                if *ok { "✔" } else { "✘" },
                secs(*duration_ms)
            )
        }
        E::GateOpened { gate, occurrence } => {
            format!("   gate open: {gate} (occurrence {occurrence})")
        }
        E::BudgetWarning { used, cap } => format!(
            "!! budget: {used} of {cap} tokens used (≥ 80 %; admission cap, may overshoot by one turn)"
        ),
        E::Escalated { reason, .. } | E::Paused { reason } => format!("!! {reason}"),
        E::RunEnded {
            state,
            next_action,
            error,
            ..
        } => {
            let mut s = format!(
                "══════════ {}: {state:?} ══════════\nnext: {next_action}",
                env.task
            );
            if let Some(e) = error {
                s.push_str(&format!("\n!! stopped with an error: {e}"));
            }
            s
        }
    }
}

/// The engine's lines on stderr (A2): [`render`] of every event that is not `quiet`.
pub struct StderrSink;

impl EventSink for StderrSink {
    fn name(&self) -> &str {
        "stderr"
    }

    fn emit(&self, e: &Envelope) -> Result<()> {
        if !e.quiet {
            eprintln!("{}", render(e));
        }
        Ok(())
    }
}

/// Does the file end in a partial line (no final newline)?
fn ends_open(f: &mut std::fs::File) -> Result<bool> {
    let len = f.metadata()?.len();
    if len == 0 {
        return Ok(false);
    }
    f.seek(SeekFrom::Start(len - 1))?;
    let mut b = [0u8; 1];
    f.read_exact(&mut b)?;
    Ok(b[0] != b'\n')
}

/// The complete lines of an events file that parse as JSON objects, in order; a partial or
/// damaged line is skipped (a crash mid-write, B2).
pub fn read_lines(text: &str) -> Vec<(String, Value)> {
    let complete = match text.rfind('\n') {
        Some(i) => &text[..i],
        None => "",
    };
    complete
        .lines()
        .filter_map(|l| {
            let v: Value = serde_json::from_str(l.trim_end_matches('\r')).ok()?;
            v.is_object().then(|| (l.to_string(), v))
        })
        .collect()
}

/// Follows an events file like `tail -f` (B2): each poll returns the complete lines appended
/// since the last poll; a partial last line waits for its newline. A file that shrank (replaced)
/// is read again from the start; a missing file has no lines yet.
#[derive(Debug, Default)]
pub struct Tail {
    offset: u64,
    partial: Vec<u8>,
}

impl Tail {
    pub fn poll(&mut self, path: &Path) -> std::io::Result<Vec<String>> {
        let mut f = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        if f.metadata()?.len() < self.offset {
            self.offset = 0;
            self.partial.clear();
        }
        f.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        self.offset += buf.len() as u64;
        self.partial.extend(buf);
        Ok(self.complete_lines())
    }

    fn complete_lines(&mut self) -> Vec<String> {
        let Some(last) = self.partial.iter().rposition(|b| *b == b'\n') else {
            return Vec::new();
        };
        let rest = self.partial.split_off(last + 1);
        let done = std::mem::replace(&mut self.partial, rest);
        String::from_utf8_lossy(&done)
            .lines()
            .map(|l| l.trim_end_matches('\r').to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }
}

/// How many events `watch` shows of the history without `--from-start`.
pub const WATCH_HISTORY: usize = 20;

/// The history `watch` prints first: everything, or the last [`WATCH_HISTORY`] lines.
pub fn history(lines: &[String], from_start: bool) -> &[String] {
    if from_start {
        lines
    } else {
        &lines[lines.len().saturating_sub(WATCH_HISTORY)..]
    }
}

/// What `watch` prints for one line of the file: the line itself (`--json`) or its rendered
/// text; nothing for a line that is not a JSON object (a damaged line) or, rendered, not an
/// event this toryo knows.
pub fn watch_line(line: &str, json: bool) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok().filter(Value::is_object)?;
    if json {
        return Some(line.to_string());
    }
    serde_json::from_value::<Envelope>(v)
        .ok()
        .map(|e| render(&e))
}

/// How long `watch` waits without a new event before it stops following a task that is not
/// moving on its own.
pub const WATCH_IDLE: std::time::Duration = std::time::Duration::from_secs(2);

/// Does `watch` stop? After [`WATCH_IDLE`] without a new event, when the task waits for a human
/// (a gate, `Escalated`, `Paused`), is finished, or — whatever its state — no live process drives
/// it (`active`: the recorded lock owner is alive). So a crashed run, a drive that stopped with an
/// error before its first stage, and a task that was never run all end the follow (R1).
pub fn watch_done(state: St, active: bool, idle: std::time::Duration) -> bool {
    idle >= WATCH_IDLE && (!active || state.is_waiting() || state.is_terminal())
}

/// The highest `seq` in an events file (0: none).
pub fn last_seq(path: &Path) -> u64 {
    let text = std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    read_lines(&text)
        .iter()
        .filter_map(|(_, v)| v["seq"].as_u64())
        .max()
        .unwrap_or(0)
}

/// The events file of a task.
pub fn events_path(t: &TaskRow) -> PathBuf {
    t.run_store.join(EVENTS_FILE)
}

/// The error class of an escalation reason: its prefix up to the first colon
/// (`protected_config_changed: …` → `protected_config_changed`, `implementer failed: …` →
/// `implementer_failed`); `other` when there is no short word-like prefix.
pub fn reason_class(reason: &str) -> String {
    let head = reason.split(':').next().unwrap_or("").trim();
    let wordy = !head.is_empty()
        && head.len() <= 40
        && head
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ' '));
    if wordy && reason.contains(':') {
        head.replace([' ', '-'], "_")
    } else {
        "other".into()
    }
}

/// The events a transition from `from` (by the pipeline event `event`) to the machine `m`
/// stands for, in the order the CLI prints them: the escalation or pause it led to (its
/// reason is printed before the `[state]` line), the state change, the gate it opened.
pub fn transition_events(from: St, event: &str, m: &Machine) -> Vec<EngineEvent> {
    let to = m.state();
    let reason = m.reason().unwrap_or("").to_string();
    let mut out = Vec::new();
    if to == St::Escalated && from != St::Escalated {
        out.push(EngineEvent::Escalated {
            reason_class: reason_class(&reason),
            reason,
        });
    } else if to == St::Paused && from != St::Paused {
        out.push(EngineEvent::Paused { reason });
    }
    out.push(EngineEvent::StateChanged {
        from,
        to,
        event: event.to_string(),
    });
    if from != to && matches!(to, St::WaitingPlanApproval | St::WaitingFinalApproval) {
        if let Some(g) = m.gate() {
            out.push(EngineEvent::GateOpened {
                gate: match g.kind {
                    GateKind::Plan => "plan",
                    GateKind::Final => "final",
                }
                .into(),
                occurrence: g.occurrence,
            });
        }
    }
    out
}

/// Milliseconds from an RFC 3339 time (a stage attempt's `started_at`) to now; 0 when it
/// cannot be read.
pub fn ms_since(rfc3339: &str) -> u64 {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::parse(rfc3339, &Rfc3339)
        .map(|t| {
            let d = time::OffsetDateTime::now_utc() - t;
            d.whole_milliseconds().max(0) as u64
        })
        .unwrap_or(0)
}

/// The secrets of the task's profiles (R42), so no reason carrying one reaches a sink.
/// A profile whose secret cannot be resolved here contributes nothing (the run would fail
/// on it anyway).
pub fn task_redactions(cfg: &Effective, t: &TaskRow) -> Redactions {
    let mut names: Vec<&str> = vec![&t.planner, &t.reviewer];
    names.extend(t.implementers.iter().map(String::as_str));
    names.sort_unstable();
    names.dedup();
    let mut secrets = Vec::new();
    for p in names {
        let Ok(profile) = cfg.profile(p) else {
            continue;
        };
        if let Ok(s) = crate::config::resolve_profile_secrets(profile) {
            secrets.extend(s.into_iter().map(|(name, value)| Secret { name, value }));
        }
    }
    if let Ok(v) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
        secrets.push(Secret {
            name: "CLAUDE_CODE_OAUTH_TOKEN".into(),
            value: v,
        });
    }
    Redactions::new(&secrets)
}

/// Every string of the event, redacted.
fn redact_event(r: &Redactions, e: EngineEvent) -> EngineEvent {
    if r.is_empty() {
        return e;
    }
    fn walk(r: &Redactions, v: &mut Value) {
        match v {
            Value::String(s) => *s = r.redact_str(s),
            Value::Array(a) => a.iter_mut().for_each(|x| walk(r, x)),
            Value::Object(o) => o.values_mut().for_each(|x| walk(r, x)),
            _ => {}
        }
    }
    match serde_json::to_value(&e) {
        Ok(mut v) => {
            walk(r, &mut v);
            serde_json::from_value(v).unwrap_or(e)
        }
        Err(_) => e,
    }
}

/// Wraps events in envelopes and hands them to the sinks.
pub struct Recorder {
    task: String,
    generation: i64,
    next: AtomicU64,
    redact: Redactions,
    sinks: Vec<Box<dyn EventSink>>,
    reported: Vec<AtomicBool>,
}

impl Recorder {
    /// A recorder without sinks; `last_seq` is the highest sequence number already written.
    pub fn new(task: &str, generation: i64, last_seq: u64, redact: Redactions) -> Self {
        Self {
            task: task.to_string(),
            generation,
            next: AtomicU64::new(last_seq + 1),
            redact,
            sinks: Vec::new(),
            reported: Vec::new(),
        }
    }

    /// The recorder of a command that holds the task lock at `generation`: the flight
    /// recorder file and the stderr lines, with the task's secrets redacted.
    pub fn for_task(t: &TaskRow, generation: i64) -> Self {
        let path = events_path(t);
        let redact = super::task::frozen_config(t)
            .map(|c| task_redactions(&c, t))
            .unwrap_or_else(|_| Redactions::empty());
        Self::new(&t.id, generation, last_seq(&path), redact)
            .with(Box::new(FileSink::new(path)))
            .with(Box::new(StderrSink))
    }

    pub fn with(mut self, sink: Box<dyn EventSink>) -> Self {
        self.sinks.push(sink);
        self.reported.push(AtomicBool::new(false));
        self
    }

    pub fn emit(&self, e: EngineEvent) {
        self.dispatch(e, false);
    }

    /// An event the CLI printed no line for (see [`Envelope::quiet`]).
    pub fn emit_quiet(&self, e: EngineEvent) {
        self.dispatch(e, true);
    }

    /// The events of a transition ([`transition_events`]). `state_line`: the CLI prints its
    /// `[state]` line; `reason_line`: it prints the escalation or pause reason (`!! …`).
    pub fn transition(
        &self,
        from: St,
        event: &str,
        m: &Machine,
        state_line: bool,
        reason_line: bool,
    ) {
        for e in transition_events(from, event, m) {
            let shown = match e {
                EngineEvent::StateChanged { .. } => state_line,
                EngineEvent::Escalated { .. } | EngineEvent::Paused { .. } => reason_line,
                _ => false,
            };
            self.dispatch(e, !shown);
        }
    }

    fn dispatch(&self, e: EngineEvent, quiet: bool) {
        let env = Envelope {
            v: VERSION,
            seq: self.next.fetch_add(1, Ordering::SeqCst),
            ts: super::store::now(),
            task: self.task.clone(),
            generation: self.generation,
            event: redact_event(&self.redact, e),
            quiet,
        };
        for (s, reported) in self.sinks.iter().zip(&self.reported) {
            if let Err(err) = s.emit(&env) {
                if !reported.swap(true, Ordering::SeqCst) {
                    eprintln!("!! {}: {err:#} — the run continues without it", s.name());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::state::PipelineEvent as Ev;
    use std::sync::{Arc, Mutex};

    /// Records what it receives.
    #[derive(Clone, Default)]
    pub struct Collect(pub Arc<Mutex<Vec<Envelope>>>);

    impl EventSink for Collect {
        fn name(&self) -> &str {
            "collect"
        }
        fn emit(&self, e: &Envelope) -> Result<()> {
            self.0.lock().unwrap().push(e.clone());
            Ok(())
        }
    }

    struct Broken;
    impl EventSink for Broken {
        fn name(&self) -> &str {
            "broken"
        }
        fn emit(&self, _: &Envelope) -> Result<()> {
            anyhow::bail!("no")
        }
    }

    fn machine(events: &[Ev]) -> Machine {
        let mut m = Machine::new(3);
        for e in events {
            m = m.transition(e).unwrap();
        }
        m
    }

    #[test]
    fn every_variant_round_trips_through_its_line() {
        let all = vec![
            EngineEvent::RunStarted { owner: "me".into() },
            EngineEvent::StateChanged {
                from: St::New,
                to: St::Planning,
                event: "start".into(),
            },
            EngineEvent::StageStarted {
                stage: "verify".into(),
                attempt: 1,
                profile: None,
                detail: Some("none in C:\\v".into()),
            },
            EngineEvent::StageFinished {
                stage: "plan".into(),
                attempt: 2,
                status: AttemptStatus::Failed,
                error_class: Some("InvalidOutput".into()),
                duration_ms: 5,
            },
            EngineEvent::TurnFinished {
                stage: "implement".into(),
                attempt: 1,
                profile: "codex-openai".into(),
                success: true,
                usage: Some(TurnUsage {
                    buckets: Buckets {
                        uncached_input: 1,
                        cache_read: 2,
                        cache_write: 3,
                        output: 4,
                        reasoning: None,
                    },
                    accounting: Accounting::Exact,
                }),
                reserved_tokens: None,
                duration_ms: 9,
            },
            EngineEvent::CheckpointCreated {
                head: "abc".into(),
                kind: "normal".into(),
                found: false,
            },
            EngineEvent::VerifyCommand {
                name: "t".into(),
                exit: Some(0),
                timed_out: false,
                ok: true,
                duration_ms: 100,
            },
            EngineEvent::GateOpened {
                gate: "plan".into(),
                occurrence: 1,
            },
            EngineEvent::BudgetWarning { used: 8, cap: 10 },
            EngineEvent::Escalated {
                reason_class: "x".into(),
                reason: "x: y".into(),
            },
            EngineEvent::Paused {
                reason: "budget".into(),
            },
            EngineEvent::RunEnded {
                state: St::Completed,
                next_action: "done".into(),
                from: None,
                error: None,
            },
            EngineEvent::RunEnded {
                state: St::New,
                next_action: "`toryo run T-1`".into(),
                from: Some(St::New),
                error: Some("boom".into()),
            },
        ];
        for (i, e) in all.into_iter().enumerate() {
            let env = Envelope {
                v: 1,
                seq: i as u64 + 1,
                ts: "t".into(),
                task: "T-1".into(),
                generation: 3,
                event: e.clone(),
                quiet: true,
            };
            let line = serde_json::to_string(&env).unwrap();
            let v: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(v["type"], e.kind(), "{line}");
            assert!(v.get("quiet").is_none(), "{line}");
            let back: Envelope = serde_json::from_str(&line).unwrap();
            assert_eq!(back.event, e, "{line}");
            assert!(!back.quiet);
        }
    }

    fn env(e: EngineEvent) -> Envelope {
        Envelope {
            v: 1,
            seq: 1,
            ts: "t".into(),
            task: "T-1".into(),
            generation: 2,
            event: e,
            quiet: false,
        }
    }

    /// A2 AC-2: every event the CLI printed before M10 renders to exactly the text the
    /// engine's `eprintln!` produced (the old format expressions, copied here).
    #[test]
    fn events_render_the_text_the_engine_printed() {
        let r = |e: EngineEvent| render(&env(e));
        let (from, to, name) = (St::Reviewing, St::WaitingFinalApproval, "review_approved");
        assert_eq!(
            r(EngineEvent::StateChanged {
                from,
                to,
                event: name.into()
            }),
            format!("   [state] {from:?} --{name}--> {to:?}")
        );
        // approve_now: `--{gate}_approved-->`, reject_now: `--rejected-->`
        assert_eq!(
            r(EngineEvent::StateChanged {
                from: St::WaitingPlanApproval,
                to: St::Implementing,
                event: "plan_approved".into()
            }),
            format!(
                "   [state] {:?} --{}_approved--> {:?}",
                St::WaitingPlanApproval,
                "plan",
                St::Implementing
            )
        );
        for rejected in ["plan_rejected", "final_rejected"] {
            assert_eq!(
                r(EngineEvent::StateChanged {
                    from: St::WaitingFinalApproval,
                    to: St::Fixing,
                    event: rejected.into()
                }),
                format!(
                    "   [state] {:?} --rejected--> {:?}",
                    St::WaitingFinalApproval,
                    St::Fixing
                )
            );
        }
        // the stage headers: plan, implement / fix-N, verify, review (fresh and resumed)
        let started = |stage: &str, profile: Option<&str>, detail: Option<&str>| {
            r(EngineEvent::StageStarted {
                stage: stage.into(),
                attempt: 2,
                profile: profile.map(str::to_string),
                detail: detail.map(str::to_string),
            })
        };
        assert_eq!(
            started("plan", Some("claude-max"), None),
            format!("\n── PLAN (attempt {}, {}) ──", 2, "claude-max")
        );
        assert_eq!(
            started("fix-1", Some("codex-openai"), None),
            format!(
                "\n── {} (attempt {}, {}) ──",
                "fix-1".to_uppercase(),
                2,
                "codex-openai"
            )
        );
        assert_eq!(
            started("verify", None, Some("none in C:\\v\\T-1")),
            format!(
                "\n── VERIFY (attempt {}, {} in {}) ──",
                2, "none", "C:\\v\\T-1"
            )
        );
        for resumed in [false, true] {
            assert_eq!(
                started(
                    "review",
                    Some("claude-max"),
                    resumed.then_some("resuming the interrupted session")
                ),
                format!(
                    "\n── REVIEW (attempt {}, {}{}) ──",
                    2,
                    "claude-max",
                    if resumed {
                        ", resuming the interrupted session"
                    } else {
                        ""
                    }
                )
            );
        }
        let head = "0123456789abcdef0123";
        for (found, kind) in [(false, "normal"), (true, "no_change")] {
            let mut old = String::new();
            if found {
                old.push_str(&format!(
                    "✔ checkpoint {} already committed (trailer match)\n",
                    crate::git::short(head)
                ));
            }
            old.push_str(&format!(
                "✔ checkpoint {} ({kind})",
                crate::git::short(head)
            ));
            assert_eq!(
                r(EngineEvent::CheckpointCreated {
                    head: head.into(),
                    kind: kind.into(),
                    found
                }),
                old
            );
        }
        for (exit, timed_out, ok, text) in [
            (Some(0), false, true, "exit 0"),
            (Some(3), false, false, "exit 3"),
            (None, true, false, "timed out"),
            (None, false, false, "no exit code"),
        ] {
            let name = "unit-tests";
            assert_eq!(
                r(EngineEvent::VerifyCommand {
                    name: name.into(),
                    exit,
                    timed_out,
                    ok,
                    duration_ms: 1234
                }),
                format!(
                    "  {} {name} ({text}, {:.1}s)",
                    if ok { "✔" } else { "✘" },
                    1234_f64 / 1000.0
                )
            );
        }
        let (used, b) = (85i64, 100i64);
        assert_eq!(
            r(EngineEvent::BudgetWarning { used, cap: b }),
            format!("!! budget: {used} of {b} tokens used (≥ 80 %; admission cap, may overshoot by one turn)")
        );
        let reason = "protected_config_changed: .claude/settings.local.json";
        assert_eq!(
            r(EngineEvent::Escalated {
                reason_class: "protected_config_changed".into(),
                reason: reason.into()
            }),
            format!("!! {reason}")
        );
        let reason = "reviewer_quota: QuotaExceeded x — `toryo resume T-1` when the quota is back";
        assert_eq!(
            r(EngineEvent::Paused {
                reason: reason.into()
            }),
            format!("!! {reason}")
        );
        // drive's closing lines (the final gate notes and the summary path go between)
        let (state, next) = (St::Completed, "done — merge it");
        assert_eq!(
            r(EngineEvent::RunEnded {
                state,
                next_action: next.into(),
                from: Some(St::Reviewing),
                error: None,
            }),
            format!(
                "══════════ {}: {:?} ══════════\nnext: {}",
                "T-1", state, next
            )
        );
    }

    /// The events with no line before M10 get one for `watch`; they are always emitted quiet.
    #[test]
    fn events_without_a_cli_line_render_for_watch() {
        let r = |e: EngineEvent| render(&env(e));
        assert_eq!(
            r(EngineEvent::RunStarted {
                owner: "me pid 7".into()
            }),
            "▶ T-1 — run started (generation 2, me pid 7)"
        );
        assert_eq!(
            r(EngineEvent::StageFinished {
                stage: "plan".into(),
                attempt: 1,
                status: AttemptStatus::Failed,
                error_class: Some("InvalidOutput".into()),
                duration_ms: 1500
            }),
            "   plan attempt 1: failed, InvalidOutput (1.5s)"
        );
        assert_eq!(
            r(EngineEvent::TurnFinished {
                stage: "review".into(),
                attempt: 1,
                profile: "claude-max".into(),
                success: true,
                usage: Some(TurnUsage {
                    buckets: Buckets {
                        uncached_input: 10,
                        cache_read: 20,
                        cache_write: 0,
                        output: 5,
                        reasoning: None
                    },
                    accounting: Accounting::Exact
                }),
                reserved_tokens: None,
                duration_ms: 2000
            }),
            "   turn review/1 (claude-max): completed · 35 tokens, exact (2.0s)"
        );
        assert_eq!(
            r(EngineEvent::TurnFinished {
                stage: "plan".into(),
                attempt: 2,
                profile: "claude-max".into(),
                success: false,
                usage: None,
                reserved_tokens: Some(200_000),
                duration_ms: 100
            }),
            "   turn plan/2 (claude-max): failed · no usage reported, ~200000 reserved (0.1s)"
        );
        assert_eq!(
            r(EngineEvent::GateOpened {
                gate: "final".into(),
                occurrence: 2
            }),
            "   gate open: final (occurrence 2)"
        );
    }

    #[test]
    fn a_transition_stands_for_its_escalation_pause_and_gate() {
        let m = machine(&[Ev::Start]);
        let ev = transition_events(St::New, "start", &m);
        assert_eq!(
            ev,
            vec![EngineEvent::StateChanged {
                from: St::New,
                to: St::Planning,
                event: "start".into()
            }]
        );
        let m = machine(&[
            Ev::Start,
            Ev::PlanReady {
                subject: "s".into(),
            },
        ]);
        let ev = transition_events(St::Planning, "plan_ready", &m);
        assert_eq!(ev.len(), 2);
        assert_eq!(
            ev[1],
            EngineEvent::GateOpened {
                gate: "plan".into(),
                occurrence: 1
            }
        );
        let m = machine(&[
            Ev::Start,
            Ev::Escalate {
                reason: "protected_config_changed: .toryo/x".into(),
            },
        ]);
        let ev = transition_events(St::Planning, "escalate", &m);
        assert_eq!(
            ev[0],
            EngineEvent::Escalated {
                reason_class: "protected_config_changed".into(),
                reason: "protected_config_changed: .toryo/x".into()
            }
        );
        assert!(matches!(ev[1], EngineEvent::StateChanged { .. }));
        let m = machine(&[Ev::Start, Ev::BudgetExceeded]);
        let ev = transition_events(St::Planning, "budget_exceeded", &m);
        assert_eq!(
            ev[0],
            EngineEvent::Paused {
                reason: "budget".into()
            }
        );
    }

    #[test]
    fn reason_classes_are_the_word_before_the_colon() {
        assert_eq!(
            reason_class("xpp_toolchain_unavailable: no NuGet folder"),
            "xpp_toolchain_unavailable"
        );
        assert_eq!(
            reason_class("implementer failed: Unknown — boom"),
            "implementer_failed"
        );
        assert_eq!(
            reason_class("InvalidOutput: rejected 3 times"),
            "InvalidOutput"
        );
        assert_eq!(reason_class("no colon here"), "other");
        assert_eq!(reason_class("a (weird) prefix: x"), "other");
    }

    #[test]
    fn the_recorder_numbers_redacts_and_survives_a_broken_sink() {
        let c = Collect::default();
        let r = Recorder::new(
            "T-1",
            4,
            41,
            Redactions::new(&[Secret {
                name: "K".into(),
                value: "s3cr3t-value".into(),
            }]),
        )
        .with(Box::new(Broken))
        .with(Box::new(c.clone()));
        r.emit(EngineEvent::Paused {
            reason: "saw s3cr3t-value in the output".into(),
        });
        r.emit_quiet(EngineEvent::BudgetWarning { used: 1, cap: 2 });
        let got = c.0.lock().unwrap().clone();
        assert_eq!(got.len(), 2, "the broken sink does not stop the others");
        assert_eq!((got[0].seq, got[1].seq), (42, 43));
        assert_eq!((got[0].task.as_str(), got[0].generation), ("T-1", 4));
        assert!((!got[0].quiet, got[1].quiet) == (true, true));
        let line = serde_json::to_string(&got[0]).unwrap();
        assert!(!line.contains("s3cr3t-value"), "{line}");
        assert!(line.contains("[REDACTED:K]"), "{line}");
    }

    #[test]
    fn the_file_sink_appends_lines_and_closes_a_partial_one() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(EVENTS_FILE);
        std::fs::write(&p, "{\"seq\":1}\n{\"seq\":2,\"trunc").unwrap();
        assert_eq!(last_seq(&p), 1, "a partial last line is skipped");
        let r = Recorder::new("T", 1, last_seq(&p), Redactions::empty())
            .with(Box::new(FileSink::new(p.clone())));
        r.emit(EngineEvent::RunStarted { owner: "o".into() });
        r.emit(EngineEvent::BudgetWarning { used: 1, cap: 2 });
        let text = std::fs::read_to_string(&p).unwrap();
        let lines = read_lines(&text);
        let seqs: Vec<u64> = lines
            .iter()
            .filter_map(|(_, v)| v["seq"].as_u64())
            .collect();
        assert_eq!(seqs, vec![1, 2, 3], "{text}");
        assert!(text.ends_with('\n'));
        assert_eq!(last_seq(&p), 3);
    }

    #[test]
    fn the_tail_returns_complete_lines_and_waits_for_a_partial_one() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(EVENTS_FILE);
        let mut tail = Tail::default();
        assert!(tail.poll(&p).unwrap().is_empty(), "no file yet");
        std::fs::write(&p, "{\"seq\":1}\n{\"seq\":2").unwrap();
        assert_eq!(tail.poll(&p).unwrap(), vec!["{\"seq\":1}"]);
        assert!(tail.poll(&p).unwrap().is_empty(), "the partial line waits");
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"}\r\n{\"seq\":3}\n").unwrap();
        assert_eq!(tail.poll(&p).unwrap(), vec!["{\"seq\":2}", "{\"seq\":3}"]);
        // replaced by a shorter file: read again from the start
        std::fs::write(&p, "{\"seq\":9}\n").unwrap();
        assert_eq!(tail.poll(&p).unwrap(), vec!["{\"seq\":9}"]);
    }

    #[test]
    fn watch_shows_the_last_events_and_renders_or_passes_lines() {
        let lines: Vec<String> = (1..=25).map(|i| i.to_string()).collect();
        assert_eq!(history(&lines, true).len(), 25);
        assert_eq!(history(&lines, false), &lines[5..]);
        assert_eq!(history(&lines[..3], false).len(), 3);
        let line = serde_json::to_string(&env(EngineEvent::GateOpened {
            gate: "plan".into(),
            occurrence: 1,
        }))
        .unwrap();
        assert_eq!(watch_line(&line, true).as_deref(), Some(line.as_str()));
        assert_eq!(
            watch_line(&line, false).as_deref(),
            Some("   gate open: plan (occurrence 1)")
        );
        for bad in ["{\"seq\":2,\"tr", "[1]", "not json"] {
            assert_eq!(watch_line(bad, true), None, "{bad}");
        }
        // a newer event type: kept with --json, not rendered
        let future =
            r#"{"v":2,"seq":1,"ts":"t","task":"T","generation":1,"type":"from_the_future"}"#;
        assert!(watch_line(future, true).is_some());
        assert_eq!(watch_line(future, false), None);
    }

    #[test]
    fn watch_stops_on_a_task_that_does_not_move_on_its_own() {
        let idle = WATCH_IDLE;
        let short = std::time::Duration::from_millis(500);
        assert!(watch_done(St::WaitingPlanApproval, true, idle));
        assert!(watch_done(St::Completed, false, idle));
        assert!(watch_done(St::Paused, false, idle));
        assert!(
            !watch_done(St::WaitingPlanApproval, false, short),
            "not idle yet"
        );
        assert!(!watch_done(St::Implementing, true, idle), "a run drives it");
        assert!(
            watch_done(St::Implementing, false, idle),
            "nobody drives it"
        );
        assert!(watch_done(St::New, false, idle), "nobody drives it (R1)");
        assert!(!watch_done(St::New, true, idle), "a drive is starting");
    }

    #[test]
    fn an_unwritable_file_is_reported_not_raised() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(EVENTS_FILE);
        std::fs::create_dir_all(&p).unwrap(); // a directory in its place
        let r = Recorder::new("T", 1, 0, Redactions::empty()).with(Box::new(FileSink::new(p)));
        r.emit(EngineEvent::RunStarted { owner: "o".into() });
        r.emit(EngineEvent::RunStarted { owner: "o".into() });
    }

    #[test]
    fn elapsed_time_since_an_attempt_start() {
        assert_eq!(ms_since("not a time"), 0);
        let past = time::OffsetDateTime::now_utc() - time::Duration::seconds(2);
        let s = past
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        assert!((1500..60_000).contains(&ms_since(&s)));
    }
}
