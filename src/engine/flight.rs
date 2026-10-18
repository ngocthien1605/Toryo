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
use std::sync::Arc;

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
        /// M16-AT (X39): the process id of the drive command (for a checker: is it still alive).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
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
        /// M11-E3: `environment` for an environment gate; absent otherwise (the M10 shape).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
    },
    /// M11-E3: an environment gate waits for its machine-wide lock (once per wait);
    /// `holder`: the task whose deployment holds it, when known.
    LockWaiting {
        lock: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        holder: Option<String>,
    },
    /// M11-E3: an environment gate ended; `sha` is what the environment of `lock` received.
    EnvironmentDeployed {
        lock: String,
        sha: String,
        ok: bool,
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
        /// M16-AT (X39): how the drive ended — `done` (a terminal state), `waiting` (a gate,
        /// an escalation, a pause), `stopped` (a stop request, Ctrl+C included), `error`,
        /// `panicked`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<String>,
        /// M16-AT (X39): the exit code of the drive command.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    // ── M16-AT (X39): diagnostic events — written to the file, never printed (always quiet),
    // left out of `watch` and of the dashboard's `last:`; for checkers and post-mortems ──
    /// A machine-wide lock was taken: the verify lock (`machine:verify`) or an environment
    /// gate's lock. Written right after the OS lock is held.
    LockAcquired {
        lock: String,
    },
    /// The lock is given back. Written right **before** the OS lock is released, so the next
    /// holder's `lock_acquired` always comes after it.
    LockReleased {
        lock: String,
    },
    /// The engine takes up a stop request: `source` is `ctrl_c` (the console's Ctrl+C) or
    /// `stop` (`toryo stop <id>`).
    CancelRequested {
        source: String,
    },
    /// A turn's agent started; `pid`: its process (none for a scripted fake agent, which runs
    /// in this process).
    AgentSpawned {
        stage: String,
        attempt: u32,
        profile: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
    },
    /// Toryo ended a turn's agent before it finished: `reason` is `stop` (a stop request) or
    /// `limit` (the stall or max limit). A real agent's process tree is ended with its Job.
    AgentKilled {
        stage: String,
        attempt: u32,
        profile: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
        reason: String,
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
            Self::LockWaiting { .. } => "lock_waiting",
            Self::EnvironmentDeployed { .. } => "environment_deployed",
            Self::GateOpened { .. } => "gate_opened",
            Self::BudgetWarning { .. } => "budget_warning",
            Self::Escalated { .. } => "escalated",
            Self::Paused { .. } => "paused",
            Self::RunEnded { .. } => "run_ended",
            Self::LockAcquired { .. } => "lock_acquired",
            Self::LockReleased { .. } => "lock_released",
            Self::CancelRequested { .. } => "cancel_requested",
            Self::AgentSpawned { .. } => "agent_spawned",
            Self::AgentKilled { .. } => "agent_killed",
        }
    }

    /// M16-AT (X39): a diagnostic event — in the file, never on the console, in `watch` or in
    /// the dashboard's `last:` (their text is what it was before these events existed).
    pub fn is_diagnostic(&self) -> bool {
        matches!(
            self,
            Self::LockAcquired { .. }
                | Self::LockReleased { .. }
                | Self::CancelRequested { .. }
                | Self::AgentSpawned { .. }
                | Self::AgentKilled { .. }
        )
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
///
/// M15 (review M9): the text is inert on a terminal — a reason, an error or a `next:` line that
/// an agent or a vendor wrote carries no escape sequence, carriage return or direction
/// override to the console ([`crate::term::text`]). The recorded event keeps what was sent.
pub fn render(env: &Envelope) -> String {
    crate::term::text(&render_raw(env))
}

fn render_raw(env: &Envelope) -> String {
    use EngineEvent as E;
    let secs = |ms: u64| ms as f64 / 1000.0;
    match &env.event {
        E::RunStarted { owner, .. } => format!(
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
            kind,
        } => {
            let exit = match exit {
                Some(code) => format!("exit {code}"),
                None if *timed_out => "timed out".to_string(),
                None => "no exit code".to_string(),
            };
            format!(
                "  {} {name} ({exit}, {:.1}s){}",
                if *ok { "✔" } else { "✘" },
                secs(*duration_ms),
                kind.as_deref()
                    .map(|k| format!(" — {k} gate"))
                    .unwrap_or_default()
            )
        }
        E::LockWaiting { lock, holder } => format!(
            "   waiting for lock {lock}{}",
            holder
                .as_deref()
                .map(|h| format!(" (held by {h})"))
                .unwrap_or_default()
        ),
        E::EnvironmentDeployed { lock, sha, ok } => format!(
            "   {lock}: {} {} ({})",
            if *ok { "deployed" } else { "deploy failed for" },
            crate::git::short(sha),
            if *ok { "gate passed" } else { "gate failed" }
        ),
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
        // diagnostic events: never printed (quiet) and never shown by `watch` — a line only
        // for a reader of the raw file
        E::LockAcquired { lock } => format!("   lock {lock} acquired"),
        E::LockReleased { lock } => format!("   lock {lock} released"),
        E::CancelRequested { source } => format!("   stop request taken up ({source})"),
        E::AgentSpawned {
            stage,
            attempt,
            profile,
            pid,
        } => format!(
            "   agent {stage}/{attempt} ({profile}) started{}",
            pid.map(|p| format!(", pid {p}")).unwrap_or_default()
        ),
        E::AgentKilled {
            stage,
            attempt,
            profile,
            pid,
            reason,
        } => format!(
            "   agent {stage}/{attempt} ({profile}) ended by toryo ({reason}){}",
            pid.map(|p| format!(", pid {p}")).unwrap_or_default()
        ),
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
            // R9: a closed stderr is an error of this sink (reported once), never a panic
            crate::term::try_line(crate::term::Stream::Err, &render(e))?;
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

/// The history `watch` prints first: everything, or the last [`WATCH_HISTORY`] lines. Rendered
/// (`json` false) the window counts no diagnostic event (X39), so the rendered history is the
/// one it was before they were written; `--json` is the file's last lines, as before.
pub fn history(lines: &[String], from_start: bool, json: bool) -> &[String] {
    if from_start {
        return lines;
    }
    if json {
        return &lines[lines.len().saturating_sub(WATCH_HISTORY)..];
    }
    let mut counted = 0;
    let mut from = lines.len();
    while from > 0 && counted < WATCH_HISTORY {
        from -= 1;
        if !diagnostic_line(&lines[from]) {
            counted += 1;
        }
    }
    &lines[from..]
}

/// The `type` of a line names a diagnostic event (X39)?
fn diagnostic_line(line: &str) -> bool {
    serde_json::from_str::<Envelope>(line).is_ok_and(|e| e.event.is_diagnostic())
}

/// What `watch` prints for one line of the file: the line itself (`--json`) or its rendered
/// text; nothing for a line that is not a JSON object (a damaged line) or, rendered, not an
/// event this toryo knows — or a diagnostic event (X39: the file's, not the console's).
pub fn watch_line(line: &str, json: bool) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok().filter(Value::is_object)?;
    if json {
        return Some(line.to_string());
    }
    serde_json::from_value::<Envelope>(v)
        .ok()
        .filter(|e| !e.event.is_diagnostic())
        .map(|e| render(&e))
}

/// How long `watch` waits without a new event before it stops following a task that is not
/// moving on its own.
pub const WATCH_IDLE: std::time::Duration = std::time::Duration::from_secs(2);

/// Does `watch` stop? After [`WATCH_IDLE`] without a new event, when no live process drives the
/// task (`active`: the recorded lock owner is alive) — whatever its state (R7). A drive holds the
/// lock until after its `run_ended`, so a task that waits for a human, is finished, crashed, or
/// was never run ends the follow once its driver is gone, and a waiting state reached under
/// `--yes` while `approve_now` still works does not. `state` is kept for the caller's messages.
pub fn watch_done(_state: St, active: bool, idle: std::time::Duration) -> bool {
    idle >= WATCH_IDLE && !active
}

/// The engine commits a transition before it writes its events: `watch` stops only when
/// [`watch_done`] held on two polls in a row with no new event in between (R1, R7).
#[derive(Debug, Default)]
pub struct DonePolls {
    in_a_row: u32,
}

impl DonePolls {
    /// Record one poll; `true`: stop now (then read the file one last time).
    pub fn observe(&mut self, done: bool, new_events: bool) -> bool {
        self.in_a_row = if done && !new_events {
            self.in_a_row + 1
        } else {
            0
        };
        self.in_a_row >= 2
    }
}

/// The highest `seq` in an events file (0: none).
pub fn last_seq(path: &Path) -> u64 {
    scan(path).0
}

/// The highest `seq` and the number of event lines of an events file ([`seqs`]).
pub fn scan(path: &Path) -> (u64, usize) {
    let s = seqs(path);
    (s.iter().copied().max().unwrap_or(0), s.len())
}

/// The `seq` of every event line of an events file, in order. A complete JSON object on an
/// unterminated last line counts too (R6): the next event closes that line, and its `seq` must
/// not be reused.
pub fn seqs(path: &Path) -> Vec<u64> {
    let text = std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let mut seqs: Vec<u64> = read_lines(&text)
        .iter()
        .filter_map(|(_, v)| v["seq"].as_u64())
        .collect();
    let tail = text.rsplit('\n').next().unwrap_or("").trim();
    if let Ok(v) = serde_json::from_str::<Value>(tail) {
        seqs.extend(v["seq"].as_u64());
    }
    seqs
}

/// Above this a `seq` is implausible: JSON consumers (JavaScript) lose integers beyond 2^53.
pub const SEQ_MAX: u64 = (1 << 53) - 1;

/// The first `seq` of a new recorder, from the `seq`s already in the file: the highest + 1. When
/// the highest is implausible (a damaged or hand-edited file, e.g. `u64::MAX`), numbering restarts
/// above both the highest plausible `seq` and the number of lines (R7: never below an earlier
/// valid value), with a warning — never a panic or a wrap.
pub fn next_seq(seqs: &[u64]) -> (u64, Option<String>) {
    let max = seqs.iter().copied().max().unwrap_or(0);
    if max < SEQ_MAX {
        return (max + 1, None);
    }
    let plausible = seqs
        .iter()
        .copied()
        .filter(|s| *s < SEQ_MAX)
        .max()
        .unwrap_or(0);
    let n = plausible.max(seqs.len() as u64) + 1;
    (
        n,
        Some(format!(
            "the last seq ({max}) is implausible — numbering restarts at {n}"
        )),
    )
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
        snake_case(head)
    } else {
        "other".into()
    }
}

/// `InvalidOutput` → `invalid_output`, `implementer failed` → `implementer_failed`,
/// `protected_config_changed` unchanged.
pub fn snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    let mut prev: Option<char> = None;
    for c in s.trim().chars() {
        let c = if matches!(c, ' ' | '-') { '_' } else { c };
        if c.is_uppercase() && prev.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit()) {
            out.push('_');
        }
        if !(c == '_' && out.ends_with('_')) {
            out.extend(c.to_lowercase());
        }
        prev = Some(c);
    }
    out
}

/// The class of an escalation the state machine decides itself, from the pipeline event that
/// led to it (R6): the fix-cycle bound, a blocked review, the plan-policy bound.
pub fn machine_class(event: &str) -> Option<&'static str> {
    match event {
        "verify_failed" | "changes_requested" | "final_rejected" | "path_policy_violation" => {
            Some("max_fix_cycles")
        }
        "review_blocked" => Some("review_blocked"),
        "plan_policy_violation" => Some("plan_policy"),
        _ => None,
    }
}

/// The typed class of an escalation (R6), snake_case: the state machine's own class, else the
/// class the engine gives (the attempt's error class, the escalation kind), else — only as a
/// fallback — the prefix of the reason.
pub fn escalation_class(event: &str, class: Option<&str>, reason: &str) -> String {
    match (machine_class(event), class.filter(|c| !c.trim().is_empty())) {
        (Some(c), _) => c.to_string(),
        (None, Some(c)) => snake_case(c),
        (None, None) => reason_class(reason),
    }
}

/// The events a transition from `from` (by the pipeline event `event`) to the machine `m`
/// stands for, in the order the CLI prints them: the escalation or pause it led to (its
/// reason is printed before the `[state]` line), the state change, the gate it opened.
/// `class`: the escalation's class, when the caller knows it ([`escalation_class`]).
pub fn transition_events(
    from: St,
    event: &str,
    m: &Machine,
    class: Option<&str>,
) -> Vec<EngineEvent> {
    let to = m.state();
    let reason = m.reason().unwrap_or("").to_string();
    let mut out = Vec::new();
    if to == St::Escalated && from != St::Escalated {
        // M11-C4: with split limits the bound that was hit is the class
        let reason_class = match (m.split_limits(), event) {
            (Some(_), "verify_failed" | "path_policy_violation") => "max_gate_retries".into(),
            (Some(_), "changes_requested" | "final_rejected") => "max_review_rounds".into(),
            _ => escalation_class(event, class, &reason),
        };
        out.push(EngineEvent::Escalated {
            reason_class,
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
    Redactions::new(&task_secrets(cfg, t))
}

/// The secrets of [`task_redactions`] (M11-E3: an environment gate adds its own).
pub fn task_secrets(cfg: &Effective, t: &TaskRow) -> Vec<Secret> {
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
    secrets
}

/// Every string field of the event, redacted in place (R6: fails closed). The enum fields
/// (states, statuses, the accounting) are not text and stay typed, so a secret equal to, say, a
/// state name can never make the redacted event unreadable — and there is no fallback to the
/// unredacted event.
fn redact_event(r: &Redactions, mut e: EngineEvent) -> EngineEvent {
    if r.is_empty() {
        return e;
    }
    let s = |x: &mut String| *x = r.redact_str(x);
    let o = |x: &mut Option<String>| {
        if let Some(v) = x {
            *v = r.redact_str(v);
        }
    };
    use EngineEvent as E;
    match &mut e {
        E::RunStarted { owner, .. } => s(owner),
        E::StateChanged { event, .. } => s(event),
        E::LockAcquired { lock } | E::LockReleased { lock } => s(lock),
        E::CancelRequested { source } => s(source),
        E::AgentSpawned { stage, profile, .. } => {
            s(stage);
            s(profile);
        }
        E::AgentKilled {
            stage,
            profile,
            reason,
            ..
        } => {
            s(stage);
            s(profile);
            s(reason);
        }
        E::StageStarted {
            stage,
            profile,
            detail,
            ..
        } => {
            s(stage);
            o(profile);
            o(detail);
        }
        E::StageFinished {
            stage, error_class, ..
        } => {
            s(stage);
            o(error_class);
        }
        E::TurnFinished { stage, profile, .. } => {
            s(stage);
            s(profile);
        }
        E::CheckpointCreated { head, kind, .. } => {
            s(head);
            s(kind);
        }
        E::VerifyCommand { name, kind, .. } => {
            s(name);
            o(kind);
        }
        E::LockWaiting { lock, holder } => {
            s(lock);
            o(holder);
        }
        E::EnvironmentDeployed { lock, sha, .. } => {
            s(lock);
            s(sha);
        }
        E::GateOpened { gate, .. } => s(gate),
        E::BudgetWarning { .. } => {}
        E::Escalated {
            reason_class,
            reason,
        } => {
            s(reason_class);
            s(reason);
        }
        E::Paused { reason } => s(reason),
        E::RunEnded {
            next_action,
            error,
            outcome,
            ..
        } => {
            s(next_action);
            o(error);
            o(outcome);
        }
    }
    e
}

/// Length limits of free text, applied after redaction (R7): `run_ended.error` to
/// [`ERROR_MAX`] characters.
fn limit(mut e: EngineEvent) -> EngineEvent {
    if let EngineEvent::RunEnded {
        error: Some(err), ..
    } = &mut e
    {
        *err = shorten(err, ERROR_MAX);
    }
    e
}

/// M16-AT (X39): the time of an event — RFC 3339 in UTC like [`super::store::now`], and
/// strictly later than the previous event's of this process (a clock that stands still or
/// steps back gives the last time plus a microsecond), so a checker can order the events of
/// one process by `ts` alone.
fn event_time() -> String {
    use time::format_description::well_known::Rfc3339;
    static LAST: std::sync::Mutex<Option<time::OffsetDateTime>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let mut now = time::OffsetDateTime::now_utc();
    if let Some(prev) = *last {
        if now <= prev {
            now = prev + time::Duration::microseconds(1);
        }
    }
    *last = Some(now);
    now.format(&Rfc3339).unwrap_or_else(|_| "unknown".into())
}

/// Wraps events in envelopes and hands them to the sinks. A clone shares everything — the
/// sequence numbers, the sinks, the dispatch lock — so a turn's hook can record events while
/// the engine records its own (M16-AT): one event at a time, in `seq` order in the file.
#[derive(Clone)]
pub struct Recorder {
    task: String,
    generation: i64,
    next: Arc<AtomicU64>,
    redact: Arc<Redactions>,
    sinks: Arc<Vec<Box<dyn EventSink>>>,
    reported: Arc<Vec<AtomicBool>>,
    dispatching: Arc<std::sync::Mutex<()>>,
}

impl Recorder {
    /// A recorder without sinks; `last_seq` is the highest sequence number already written.
    pub fn new(task: &str, generation: i64, last_seq: u64, redact: Redactions) -> Self {
        Self {
            task: task.to_string(),
            generation,
            next: Arc::new(AtomicU64::new(last_seq.saturating_add(1))),
            redact: Arc::new(redact),
            sinks: Arc::new(Vec::new()),
            reported: Arc::new(Vec::new()),
            dispatching: Arc::new(std::sync::Mutex::new(())),
        }
    }

    /// The recorder of a command that holds the task lock at `generation`: the flight
    /// recorder file and the stderr lines, with the task's secrets redacted.
    pub fn for_task(t: &TaskRow, generation: i64) -> Self {
        let path = events_path(t);
        let redact = super::task::frozen_config(t)
            .map(|c| task_redactions(&c, t))
            .unwrap_or_else(|_| Redactions::empty());
        let (first, warning) = next_seq(&seqs(&path));
        if let Some(w) = warning {
            crate::errln!("!! flight recorder {}: {w}", path.display());
        }
        Self::new(&t.id, generation, first - 1, redact)
            .with(Box::new(FileSink::new(path)))
            .with(Box::new(StderrSink))
    }

    /// Add a sink — while the recorder is built, before it is cloned.
    pub fn with(mut self, sink: Box<dyn EventSink>) -> Self {
        Arc::get_mut(&mut self.sinks)
            .expect("Recorder::with before the recorder is cloned")
            .push(sink);
        Arc::get_mut(&mut self.reported)
            .expect("Recorder::with before the recorder is cloned")
            .push(AtomicBool::new(false));
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
        self.transition_classed(from, event, m, state_line, reason_line, None);
    }

    /// [`Self::transition`] with the escalation's class, when the caller knows it (R6).
    pub fn transition_classed(
        &self,
        from: St,
        event: &str,
        m: &Machine,
        state_line: bool,
        reason_line: bool,
        class: Option<&str>,
    ) {
        for e in transition_events(from, event, m, class) {
            let shown = match e {
                EngineEvent::StateChanged { .. } => state_line,
                EngineEvent::Escalated { .. } | EngineEvent::Paused { .. } => reason_line,
                _ => false,
            };
            self.dispatch(e, !shown);
        }
    }

    fn dispatch(&self, e: EngineEvent, quiet: bool) {
        // one event at a time across clones: `seq`, `ts` and the line's place in the file agree
        let _one = self.dispatching.lock().unwrap_or_else(|e| e.into_inner());
        let env = Envelope {
            v: VERSION,
            // never wraps: at the (implausible) end of the range the number stays there. A plain
            // compare-exchange loop: `fetch_update` is deprecated from Rust 1.99 and its
            // replacement `try_update` is newer than our rust-version (M11-ci)
            seq: {
                let mut n = self.next.load(Ordering::SeqCst);
                loop {
                    match self.next.compare_exchange_weak(
                        n,
                        n.saturating_add(1),
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    ) {
                        Ok(prev) => break prev,
                        Err(now) => n = now,
                    }
                }
            },
            ts: event_time(),
            task: self.task.clone(),
            generation: self.generation,
            // R7: redact first, then shorten — a cut never leaves a secret's prefix behind
            event: limit(redact_event(&self.redact, e)),
            quiet,
        };
        for (s, reported) in self.sinks.iter().zip(self.reported.iter()) {
            if let Err(err) = s.emit(&env) {
                if !reported.swap(true, Ordering::SeqCst) {
                    // R9: never `eprintln!` here: it panics when stderr is gone, and a panic
                    // while `RunEndGuard` unwinds would abort the process
                    let _ = writeln!(
                        std::io::stderr(),
                        "{}",
                        crate::term::text(&format!(
                            "!! {}: {err:#} — the run continues without it",
                            s.name()
                        ))
                    );
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

    /// M16-AT (X39): the diagnostic events are in the file (`watch --json` shows the file),
    /// never in the rendered `watch`; and they never take a place in its history of 20.
    #[test]
    fn diagnostic_events_stay_out_of_the_rendered_watch_and_its_history() {
        let c = Collect::default();
        let r = Recorder::new("T", 1, 0, Redactions::empty()).with(Box::new(c.clone()));
        for i in 0..25 {
            r.emit_quiet(EngineEvent::GateOpened {
                gate: format!("g{i}"),
                occurrence: 1,
            });
            r.emit_quiet(EngineEvent::LockAcquired { lock: "l".into() });
        }
        let lines: Vec<String> =
            c.0.lock()
                .unwrap()
                .iter()
                .map(|e| serde_json::to_string(e).unwrap())
                .collect();
        let h = history(&lines, false, false);
        let gates = h.iter().filter(|l| l.contains("gate_opened")).count();
        assert_eq!(
            gates, WATCH_HISTORY,
            "the history is 20 events, not 20 lines"
        );
        assert!(h[0].contains("\"g5\""), "{}", h[0]);
        for l in h {
            let shown = watch_line(l, false);
            assert_eq!(shown.is_some(), l.contains("gate_opened"), "{l}");
            assert!(watch_line(l, true).is_some(), "--json is the file: {l}");
        }
        assert!(EngineEvent::CancelRequested {
            source: "stop".into()
        }
        .is_diagnostic());
        assert!(!EngineEvent::Paused { reason: "r".into() }.is_diagnostic());
    }

    /// M16-AT (X39): within a process the times of the events only go forward, and a clone of
    /// the recorder numbers its events with the original's.
    #[test]
    fn event_times_increase_and_clones_share_the_numbering() {
        let c = Collect::default();
        let r = Recorder::new("T", 1, 6, Redactions::empty()).with(Box::new(c.clone()));
        let r2 = r.clone();
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let r = r2.clone();
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        r.emit_quiet(EngineEvent::LockReleased { lock: "l".into() });
                    }
                })
            })
            .collect();
        for _ in 0..50 {
            r.emit_quiet(EngineEvent::LockAcquired { lock: "l".into() });
        }
        for t in threads {
            t.join().unwrap();
        }
        let got = c.0.lock().unwrap().clone();
        assert_eq!(got.len(), 250);
        let seqs: Vec<u64> = got.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, (7..257).collect::<Vec<u64>>(), "in seq order, no gap");
        let ts: Vec<time::OffsetDateTime> = got
            .iter()
            .map(|e| {
                time::OffsetDateTime::parse(&e.ts, &time::format_description::well_known::Rfc3339)
                    .unwrap()
            })
            .collect();
        assert!(ts.windows(2).all(|w| w[0] < w[1]), "strictly increasing");
    }

    #[test]
    fn every_variant_round_trips_through_its_line() {
        let all = vec![
            EngineEvent::RunStarted {
                pid: None,
                owner: "me".into(),
            },
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
                kind: None,
            },
            EngineEvent::VerifyCommand {
                name: "dbsync".into(),
                exit: Some(0),
                timed_out: false,
                ok: true,
                duration_ms: 100,
                kind: Some("environment".into()),
            },
            EngineEvent::LockWaiting {
                lock: "d365-env".into(),
                holder: Some("T-12".into()),
            },
            EngineEvent::EnvironmentDeployed {
                lock: "d365-env".into(),
                sha: "1a2b3c4d5e6f".into(),
                ok: true,
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
                outcome: None,
                exit_code: None,
                state: St::Completed,
                next_action: "done".into(),
                from: None,
                error: None,
            },
            EngineEvent::RunEnded {
                outcome: None,
                exit_code: None,
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
                    duration_ms: 1234,
                    kind: None,
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
                outcome: None,
                exit_code: None,
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

    /// M15 (review M9): what `run` and `watch` print of an event cannot drive the terminal —
    /// an OSC 52 clipboard write, "erase the line above" plus a forged end banner, a carriage
    /// return over the `!!`, a bidi override; the words stay.
    #[test]
    fn rendered_text_is_inert_on_a_terminal() {
        let r = |e: EngineEvent| render(&env(e));
        assert_eq!(
            r(EngineEvent::Escalated {
                reason_class: "review_blocked".into(),
                reason: "blocked\u{1b}]52;c;QUJD\u{7}\n\u{1b}[1A\u{1b}[2K══ T-1: Completed ══\rok"
                    .into(),
            }),
            "!! blocked\n══ T-1: Completed ══ok"
        );
        assert_eq!(
            r(EngineEvent::Paused {
                reason: "quota \u{202e}dlo\u{9b}31m".into()
            }),
            "!! quota \u{FFFD}dlo\u{FFFD}31m"
        );
        let end = r(EngineEvent::RunEnded {
            outcome: None,
            exit_code: None,
            state: St::Escalated,
            next_action: "a\u{1b}[2Jb".into(),
            from: None,
            error: Some("x\u{7}y\r\n".into()),
        });
        assert_eq!(
            end,
            "══════════ T-1: Escalated ══════════\nnext: ab\n!! stopped with an error: x\u{FFFD}y\n"
        );
        // the recorded event is data: it keeps what was sent, and JSON escapes it
        let line = serde_json::to_string(&env(EngineEvent::Paused {
            reason: "quota \u{1b}]0;owned\u{7}".into(),
        }))
        .unwrap();
        assert!(
            line.contains(r"\u001b]0;owned\u0007") && !line.contains('\u{1b}'),
            "{line}"
        );
    }

    /// The events with no line before M10 get one for `watch`; they are always emitted quiet.
    #[test]
    fn events_without_a_cli_line_render_for_watch() {
        let r = |e: EngineEvent| render(&env(e));
        assert_eq!(
            r(EngineEvent::RunStarted {
                pid: None,
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
        let ev = transition_events(St::New, "start", &m, None);
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
        let ev = transition_events(St::Planning, "plan_ready", &m, None);
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
        let ev = transition_events(St::Planning, "escalate", &m, None);
        assert_eq!(
            ev[0],
            EngineEvent::Escalated {
                reason_class: "protected_config_changed".into(),
                reason: "protected_config_changed: .toryo/x".into()
            }
        );
        assert!(matches!(ev[1], EngineEvent::StateChanged { .. }));
        let m = machine(&[Ev::Start, Ev::BudgetExceeded]);
        let ev = transition_events(St::Planning, "budget_exceeded", &m, None);
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
        // R6: always snake_case
        assert_eq!(
            reason_class("InvalidOutput: rejected 3 times"),
            "invalid_output"
        );
        assert_eq!(reason_class("no colon here"), "other");
        assert_eq!(reason_class("a (weird) prefix: x"), "other");
    }

    /// R6: the class comes from the source — the state machine's event, else the engine's
    /// class — and the reason text is only the fallback.
    #[test]
    fn escalation_classes_are_typed_and_snake_case() {
        for (s, want) in [
            ("InvalidOutput", "invalid_output"),
            ("RateLimit", "rate_limit"),
            ("protected_config_changed", "protected_config_changed"),
            ("implementer failed", "implementer_failed"),
            ("PermissionDenied", "permission_denied"),
        ] {
            assert_eq!(snake_case(s), want, "{s}");
        }
        let bound = "not accepted after 1 fix cycle(s) (last: verify failed)";
        assert_eq!(
            escalation_class("verify_failed", None, bound),
            "max_fix_cycles"
        );
        assert_eq!(
            escalation_class("changes_requested", Some("x"), bound),
            "max_fix_cycles"
        );
        assert_eq!(
            escalation_class("review_blocked", None, "the reviewer blocked the task"),
            "review_blocked"
        );
        assert_eq!(
            escalation_class("plan_policy_violation", None, "the plan still lists …"),
            "plan_policy"
        );
        assert_eq!(
            escalation_class("escalate", Some("xpp_toolchain_unavailable"), "no NuGet: x"),
            "xpp_toolchain_unavailable"
        );
        assert_eq!(
            escalation_class("escalate", Some("InvalidOutput"), "whatever: x"),
            "invalid_output"
        );
        assert_eq!(
            escalation_class("escalate", None, "approval_mismatch: the branch …"),
            "approval_mismatch",
            "fallback"
        );
        let m = machine(&[
            Ev::Start,
            Ev::Escalate {
                reason: "no prefix here".into(),
            },
        ]);
        let ev = transition_events(St::Planning, "escalate", &m, Some("policy_violation"));
        assert_eq!(
            ev[0],
            EngineEvent::Escalated {
                reason_class: "policy_violation".into(),
                reason: "no prefix here".into()
            }
        );
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

    /// R6: redaction fails closed — a secret equal to a state name used to make the redacted
    /// event unreadable, and the recorder fell back to the unredacted one.
    #[test]
    fn redaction_never_falls_back_to_the_original_event() {
        let c = Collect::default();
        let r = Recorder::new(
            "T-1",
            1,
            0,
            Redactions::new(&[Secret {
                name: "K".into(),
                value: "Completed".into(),
            }]),
        )
        .with(Box::new(c.clone()));
        r.emit(EngineEvent::RunEnded {
            outcome: None,
            exit_code: None,
            state: St::Completed,
            next_action: "done: Completed — merge it".into(),
            from: Some(St::Reviewing),
            error: Some("Completed twice".into()),
        });
        let got = c.0.lock().unwrap().clone();
        match &got[0].event {
            EngineEvent::RunEnded {
                state,
                next_action,
                error,
                ..
            } => {
                assert_eq!(*state, St::Completed, "the enum stays typed");
                assert_eq!(next_action, "done: [REDACTED:K] — merge it");
                assert_eq!(error.as_deref(), Some("[REDACTED:K] twice"));
            }
            e => panic!("{e:?}"),
        }
    }

    /// R9: an event emitted while a panic unwinds (what `RunEndGuard` does), through a sink that
    /// fails, is reported without a second panic — which would abort the process. (Before R9 the
    /// report used `eprintln!`, which panics only when stderr is gone: not reproducible here, so
    /// this test guards the path rather than failing first.)
    #[test]
    fn a_failing_sink_while_unwinding_does_not_panic_again() {
        struct EmitOnDrop(Recorder);
        impl Drop for EmitOnDrop {
            fn drop(&mut self) {
                self.0.emit(EngineEvent::BudgetWarning { used: 1, cap: 2 });
            }
        }
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = EmitOnDrop(
                Recorder::new("T", 1, 0, Redactions::empty())
                    .with(Box::new(Broken))
                    .with(Box::new(StderrSink)),
            );
            panic!("the first panic");
        }));
        assert!(res.is_err(), "the first panic is the only one");
    }

    /// R7: the recorder redacts before it shortens. A secret that starts just before the
    /// 400-character cut used to lose its tail to the cut and its prefix stayed in the file.
    #[test]
    fn a_long_error_is_redacted_before_it_is_shortened() {
        let secret = "zq7-SECRET-token-value-0123456789";
        let c = Collect::default();
        let r = Recorder::new(
            "T",
            1,
            0,
            Redactions::new(&[Secret {
                name: "K".into(),
                value: secret.into(),
            }]),
        )
        .with(Box::new(c.clone()));
        let error = format!("{}{secret} and more", "x".repeat(ERROR_MAX - 5));
        r.emit(EngineEvent::RunEnded {
            outcome: None,
            exit_code: None,
            state: St::New,
            next_action: "n".into(),
            from: None,
            error: Some(error),
        });
        let got = c.0.lock().unwrap().clone();
        let line = serde_json::to_string(&got[0]).unwrap();
        assert!(
            !line.contains("zq7"),
            "a prefix of the secret survived: {line}"
        );
        match &got[0].event {
            EngineEvent::RunEnded { error: Some(e), .. } => {
                assert_eq!(e.chars().count(), ERROR_MAX);
                assert!(e.ends_with('…'));
            }
            e => panic!("{e:?}"),
        }
    }

    /// R7: a secret in **every** string field of **every** event variant never reaches a sink.
    /// Each string of each variant's JSON is replaced by the secret in turn; a replacement that
    /// no longer parses is an enum (a state, a status), not text. A field added later is covered
    /// as soon as its variant is in `all`.
    #[test]
    fn every_string_field_of_every_variant_is_redacted() {
        const SECRET: &str = "SEKRIT-4a7b9c";
        fn string_paths(v: &Value, at: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
            match v {
                Value::String(_) => out.push(at.clone()),
                Value::Object(o) => {
                    for (k, x) in o {
                        at.push(k.clone());
                        string_paths(x, at, out);
                        at.pop();
                    }
                }
                // R9: arrays too (`#<index>` in the path)
                Value::Array(a) => {
                    for (i, x) in a.iter().enumerate() {
                        at.push(format!("#{i}"));
                        string_paths(x, at, out);
                        at.pop();
                    }
                }
                _ => {}
            }
        }
        fn set(v: &mut Value, path: &[String]) {
            match path.split_first() {
                None => *v = Value::String(format!("before {SECRET} after")),
                Some((k, rest)) => {
                    match k.strip_prefix('#').and_then(|i| i.parse::<usize>().ok()) {
                        Some(i) => set(&mut v[i], rest),
                        None => set(&mut v[k.as_str()], rest),
                    }
                }
            }
        }
        /// R9: an exhaustive match that names every field (no `..`, no wildcard): a new variant
        /// or a new field does not compile until it is added here — and to `all` below, with
        /// every `Option<String>` as `Some`.
        fn fields_named(e: &EngineEvent) -> &'static str {
            use EngineEvent as E;
            match e {
                E::RunStarted { owner: _, pid: _ } => "run_started",
                E::StateChanged {
                    from: _,
                    to: _,
                    event: _,
                } => "state_changed",
                E::StageStarted {
                    stage: _,
                    attempt: _,
                    profile: _,
                    detail: _,
                } => "stage_started",
                E::StageFinished {
                    stage: _,
                    attempt: _,
                    status: _,
                    error_class: _,
                    duration_ms: _,
                } => "stage_finished",
                E::TurnFinished {
                    stage: _,
                    attempt: _,
                    profile: _,
                    success: _,
                    usage: _,
                    reserved_tokens: _,
                    duration_ms: _,
                } => "turn_finished",
                E::CheckpointCreated {
                    head: _,
                    kind: _,
                    found: _,
                } => "checkpoint_created",
                E::VerifyCommand {
                    name: _,
                    exit: _,
                    timed_out: _,
                    ok: _,
                    duration_ms: _,
                    kind: _,
                } => "verify_command",
                E::LockWaiting { lock: _, holder: _ } => "lock_waiting",
                E::EnvironmentDeployed {
                    lock: _,
                    sha: _,
                    ok: _,
                } => "environment_deployed",
                E::GateOpened {
                    gate: _,
                    occurrence: _,
                } => "gate_opened",
                E::BudgetWarning { used: _, cap: _ } => "budget_warning",
                E::Escalated {
                    reason_class: _,
                    reason: _,
                } => "escalated",
                E::Paused { reason: _ } => "paused",
                E::RunEnded {
                    state: _,
                    next_action: _,
                    from: _,
                    error: _,
                    outcome: _,
                    exit_code: _,
                } => "run_ended",
                E::LockAcquired { lock: _ } => "lock_acquired",
                E::LockReleased { lock: _ } => "lock_released",
                E::CancelRequested { source: _ } => "cancel_requested",
                E::AgentSpawned {
                    stage: _,
                    attempt: _,
                    profile: _,
                    pid: _,
                } => "agent_spawned",
                E::AgentKilled {
                    stage: _,
                    attempt: _,
                    profile: _,
                    pid: _,
                    reason: _,
                } => "agent_killed",
            }
        }
        /// The number of arms of `fields_named`.
        const VARIANTS: usize = 19;
        let all = vec![
            EngineEvent::RunStarted {
                owner: "o".into(),
                pid: Some(1),
            },
            EngineEvent::StateChanged {
                from: St::New,
                to: St::Planning,
                event: "start".into(),
            },
            EngineEvent::StageStarted {
                stage: "s".into(),
                attempt: 1,
                profile: Some("p".into()),
                detail: Some("d".into()),
            },
            EngineEvent::StageFinished {
                stage: "s".into(),
                attempt: 1,
                status: AttemptStatus::Failed,
                error_class: Some("c".into()),
                duration_ms: 1,
            },
            EngineEvent::TurnFinished {
                stage: "s".into(),
                attempt: 1,
                profile: "p".into(),
                success: true,
                usage: Some(TurnUsage {
                    buckets: Buckets::default(),
                    accounting: Accounting::Exact,
                }),
                reserved_tokens: Some(1),
                duration_ms: 1,
            },
            EngineEvent::CheckpointCreated {
                head: "h".into(),
                kind: "k".into(),
                found: true,
            },
            EngineEvent::VerifyCommand {
                name: "n".into(),
                exit: Some(0),
                timed_out: false,
                ok: true,
                duration_ms: 1,
                kind: Some("k".into()),
            },
            EngineEvent::LockWaiting {
                lock: "l".into(),
                holder: Some("h".into()),
            },
            EngineEvent::EnvironmentDeployed {
                lock: "l".into(),
                sha: "s".into(),
                ok: false,
            },
            EngineEvent::GateOpened {
                gate: "g".into(),
                occurrence: 1,
            },
            EngineEvent::BudgetWarning { used: 1, cap: 2 },
            EngineEvent::Escalated {
                reason_class: "c".into(),
                reason: "r".into(),
            },
            EngineEvent::Paused { reason: "r".into() },
            EngineEvent::RunEnded {
                state: St::Completed,
                next_action: "n".into(),
                from: Some(St::New),
                error: Some("e".into()),
                outcome: Some("done".into()),
                exit_code: Some(0),
            },
            EngineEvent::LockAcquired { lock: "l".into() },
            EngineEvent::LockReleased { lock: "l".into() },
            EngineEvent::CancelRequested {
                source: "ctrl_c".into(),
            },
            EngineEvent::AgentSpawned {
                stage: "s".into(),
                attempt: 1,
                profile: "p".into(),
                pid: Some(1),
            },
            EngineEvent::AgentKilled {
                stage: "s".into(),
                attempt: 1,
                profile: "p".into(),
                pid: Some(1),
                reason: "stop".into(),
            },
        ];
        let kinds: std::collections::BTreeSet<&str> = all.iter().map(fields_named).collect();
        assert_eq!(
            kinds.len(),
            VARIANTS,
            "every variant of `fields_named` has a sample"
        );
        for e in &all {
            assert_eq!(fields_named(e), e.kind());
        }
        let c = Collect::default();
        let r = Recorder::new(
            "T",
            1,
            0,
            Redactions::new(&[Secret {
                name: "K".into(),
                value: SECRET.into(),
            }]),
        )
        .with(Box::new(c.clone()));
        let mut text_fields = 0;
        for e in all {
            let base = serde_json::to_value(&e).unwrap();
            let mut paths = Vec::new();
            string_paths(&base, &mut Vec::new(), &mut paths);
            for p in paths.into_iter().filter(|p| p[0] != "type") {
                let mut v = base.clone();
                set(&mut v, &p);
                let Ok(with_secret) = serde_json::from_value::<EngineEvent>(v) else {
                    continue; // an enum field: not text
                };
                text_fields += 1;
                r.emit(with_secret);
                let env = c.0.lock().unwrap().pop().unwrap();
                let line = serde_json::to_string(&env).unwrap();
                assert!(!line.contains(SECRET), "{p:?} of {}: {line}", e.kind());
                assert!(line.contains("[REDACTED:K]"), "{p:?}: {line}");
            }
        }
        // every text field of the twelve variants today; a new one raises the count
        assert!(text_fields >= 18, "{text_fields}");
    }

    /// R6: `seq` never panics or wraps; an implausible last `seq` restarts above the line count;
    /// a complete object on an unterminated last line keeps its `seq`.
    #[test]
    fn seq_is_checked_and_an_unterminated_object_counts() {
        assert_eq!(next_seq(&[1, 41, 3]), (42, None));
        assert_eq!(next_seq(&[]), (1, None));
        let (n, w) = next_seq(&[3, u64::MAX, 5, 6, 7, 8, 9]);
        assert_eq!(
            n, 10,
            "above the line count (7) and the highest plausible (9)"
        );
        assert!(w.unwrap().contains("implausible"));
        assert!(next_seq(&[SEQ_MAX]).1.is_some());
        assert!(next_seq(&[SEQ_MAX - 1]).1.is_none());
        // R7: never below an earlier valid value (the line count alone would give 4)
        assert_eq!(next_seq(&[500, 501, u64::MAX]).0, 502);
        // a recorder that starts at the end of the range saturates instead of wrapping
        let c = Collect::default();
        let r = Recorder::new("T", 1, u64::MAX, Redactions::empty()).with(Box::new(c.clone()));
        r.emit(EngineEvent::BudgetWarning { used: 1, cap: 2 });
        r.emit(EngineEvent::BudgetWarning { used: 1, cap: 2 });
        assert!(c.0.lock().unwrap().iter().all(|e| e.seq == u64::MAX));

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(EVENTS_FILE);
        std::fs::write(&p, "{\"seq\":1}\n{\"seq\":5}").unwrap();
        assert_eq!(scan(&p), (5, 2));
        assert_eq!(last_seq(&p), 5, "the next process does not reuse 5");
        std::fs::write(&p, "{\"seq\":1}\n{\"seq\":5,\"ty").unwrap();
        assert_eq!(scan(&p), (1, 1), "a partial object does not count");
    }

    #[test]
    fn the_file_sink_appends_lines_and_closes_a_partial_one() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(EVENTS_FILE);
        std::fs::write(&p, "{\"seq\":1}\n{\"seq\":2,\"trunc").unwrap();
        assert_eq!(last_seq(&p), 1, "a partial last line is skipped");
        let r = Recorder::new("T", 1, last_seq(&p), Redactions::empty())
            .with(Box::new(FileSink::new(p.clone())));
        r.emit(EngineEvent::RunStarted {
            pid: None,
            owner: "o".into(),
        });
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
        assert_eq!(history(&lines, true, false).len(), 25);
        assert_eq!(history(&lines, false, false), &lines[5..]);
        assert_eq!(history(&lines, false, true), &lines[5..]);
        assert_eq!(history(&lines[..3], false, false).len(), 3);
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
        // R7: a waiting state still driven (under `--yes`, `approve_now` at work) goes on
        assert!(!watch_done(St::WaitingPlanApproval, true, idle));
        assert!(!watch_done(St::WaitingFinalApproval, true, idle));
        assert!(watch_done(St::WaitingPlanApproval, false, idle));
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

    /// R7: `watch` stops only after two "done" polls in a row with no new event in between.
    #[test]
    fn done_must_hold_on_two_polls_in_a_row() {
        let mut d = DonePolls::default();
        assert!(!d.observe(true, false), "one poll is not enough");
        assert!(d.observe(true, false), "the second in a row stops");
        let mut d = DonePolls::default();
        assert!(!d.observe(true, false));
        assert!(!d.observe(false, false), "not done: starts over");
        assert!(!d.observe(true, false));
        assert!(!d.observe(true, true), "a new event starts over");
        assert!(!d.observe(true, false));
        assert!(d.observe(true, false));
    }

    #[test]
    fn an_unwritable_file_is_reported_not_raised() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(EVENTS_FILE);
        std::fs::create_dir_all(&p).unwrap(); // a directory in its place
        let r = Recorder::new("T", 1, 0, Redactions::empty()).with(Box::new(FileSink::new(p)));
        r.emit(EngineEvent::RunStarted {
            pid: None,
            owner: "o".into(),
        });
        r.emit(EngineEvent::RunStarted {
            pid: None,
            owner: "o".into(),
        });
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
