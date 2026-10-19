//! `toryo dashboard` (M16-01): every task at a glance, read from the store and each task's
//! flight recorder (`events.jsonl`) — never by taking a lock, never by writing anything.
//!
//! A task's row is its `status` row; under it, what the flight recorder says the task is doing:
//! the stage that started and has not finished in the newest run, and the newest event with its
//! age. The words of those lines are Toryo's own: an escalation shows its class, never its
//! reason, and a pause, an error or a plan shows no free text at all — the dashboard puts no
//! text an agent, a vendor or a gate wrote on the screen (`toryo watch <id>` shows it, inert).

use serde_json::{json, Value};

use super::flight::{EngineEvent, Envelope};
use super::store::{AttemptStatus, TaskRow};
use super::views::{self, TaskFacts};

/// A stage that started and has not finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenStage {
    pub stage: String,
    pub attempt: u32,
    pub profile: Option<String>,
    /// RFC 3339: when it started.
    pub since: String,
}

/// The newest event of a task, in Toryo's words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastEvent {
    /// The event's `type` (`stage_started`, `escalated`, …).
    pub kind: String,
    pub text: String,
    /// RFC 3339.
    pub ts: String,
}

/// What a task's flight recorder says, folded event by event ([`Activity::apply`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activity {
    /// Event lines read.
    pub events: usize,
    pub last_seq: u64,
    /// A `run_started` without its `run_ended` (the file's view; a crashed run never ends it —
    /// whether a process drives the task now is the lock owner's question, [`TaskFacts`]).
    pub run_open: bool,
    pub stage: Option<OpenStage>,
    pub last: Option<LastEvent>,
}

impl Activity {
    /// Fold one recorded event in.
    pub fn apply(&mut self, e: &Envelope) {
        use EngineEvent as E;
        self.events += 1;
        self.last_seq = self.last_seq.max(e.seq);
        match &e.event {
            E::RunStarted { .. } => {
                self.run_open = true;
                // a new run: what the previous one left open is not open any more
                self.stage = None;
            }
            E::RunEnded { .. } => {
                self.run_open = false;
                self.stage = None;
            }
            E::StageStarted {
                stage,
                attempt,
                profile,
                ..
            } => {
                self.stage = Some(OpenStage {
                    stage: stage.clone(),
                    attempt: *attempt,
                    profile: profile.clone(),
                    since: e.ts.clone(),
                });
            }
            E::StageFinished { stage, attempt, .. }
                if self
                    .stage
                    .as_ref()
                    .is_some_and(|s| &s.stage == stage && s.attempt == *attempt) =>
            {
                self.stage = None;
            }
            _ => {}
        }
        // X39: a diagnostic event is not what the task did — `last:` stays the task's own event
        if e.event.is_diagnostic() {
            return;
        }
        self.last = Some(LastEvent {
            kind: e.event.kind().to_string(),
            text: describe(&e.event),
            ts: e.ts.clone(),
        });
    }

    /// Fold in the lines of an events file (as [`super::flight::Tail`] returns them); a line
    /// that is not an event this toryo knows is skipped.
    pub fn apply_lines(&mut self, lines: &[String]) {
        for l in lines {
            if let Ok(e) = serde_json::from_str::<Envelope>(l) {
                self.apply(&e);
            }
        }
    }
}

fn status_word(s: AttemptStatus) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{s:?}").to_lowercase())
}

/// An event in a few of Toryo's own words: no reason, error, owner label or plan text.
pub fn describe(e: &EngineEvent) -> String {
    use EngineEvent as E;
    match e {
        E::RunStarted { .. } => "run started".into(),
        E::StateChanged { from, to, .. } => format!("{from:?} → {to:?}"),
        E::StageStarted { stage, attempt, .. } => format!("{stage} attempt {attempt} started"),
        E::StageFinished {
            stage,
            attempt,
            status,
            error_class,
            ..
        } => {
            let class = error_class
                .as_deref()
                .map(|c| format!(" ({})", super::flight::snake_case(c)))
                .unwrap_or_default();
            format!("{stage} attempt {attempt} {}{class}", status_word(*status))
        }
        E::TurnFinished {
            stage,
            profile,
            success,
            ..
        } => format!(
            "{stage} turn {} ({profile})",
            if *success { "finished" } else { "failed" }
        ),
        E::CheckpointCreated { head, found, .. } => format!(
            "checkpoint {}{}",
            crate::git::short(head),
            if *found { " (found)" } else { "" }
        ),
        E::VerifyCommand {
            name,
            ok,
            timed_out,
            kind,
            ..
        } => format!(
            "{} {name} {}",
            if kind.as_deref() == Some("environment") {
                "environment gate"
            } else {
                "gate"
            },
            if *ok {
                "passed"
            } else if *timed_out {
                "timed out"
            } else {
                "failed"
            }
        ),
        E::LockWaiting { lock, .. } => format!("waiting for lock {lock}"),
        E::EnvironmentDeployed { lock, sha, ok } => format!(
            "{} {} to {lock}",
            if *ok { "deployed" } else { "deploy failed:" },
            crate::git::short(sha)
        ),
        E::GateOpened { gate, occurrence } => format!("{gate} gate open (occurrence {occurrence})"),
        E::BudgetWarning { used, cap } => format!("budget {used} of {cap} tokens"),
        E::Escalated { reason_class, .. } => format!("escalated: {reason_class}"),
        E::Paused { .. } => "paused".into(),
        E::RunEnded { state, .. } => format!("run ended ({state:?})"),
        // X39: diagnostic events never become `last:` (Activity::apply skips them); a name only
        E::LockAcquired { .. }
        | E::LockReleased { .. }
        | E::CancelRequested { .. }
        | E::AgentSpawned { .. }
        | E::AgentKilled { .. } => e.kind().replace('_', " "),
    }
}

/// `12s`, `4m`, `2h`, `3d`.
pub fn age(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

/// The first line of the dashboard: how many tasks, how many a process drives now, how many
/// wait for the owner ([`St::is_waiting`]: a gate, an escalation, a pause).
pub fn header(rows: &[(&TaskRow, &TaskFacts)], updated: &str) -> String {
    let active = rows
        .iter()
        .filter(|(_, f)| f.active_owner.is_some())
        .count();
    let waiting = rows
        .iter()
        .filter(|(t, f)| f.active_owner.is_none() && t.state.is_waiting())
        .count();
    format!(
        "toryo dashboard — {} task{} · {active} active · {waiting} waiting for you · {updated}",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" }
    )
}

/// The lines of one task: its `status` row, then (when the flight recorder has anything) what
/// it is doing now or did last, then `next:`. Each entry is one printed block.
pub fn task_lines(
    t: &TaskRow,
    f: &TaskFacts,
    a: &Activity,
    since_ms: impl Fn(&str) -> u64,
) -> Vec<String> {
    const PAD: &str = "               ";
    let active = f
        .active_owner
        .as_ref()
        .map(|o| format!(" · ACTIVE ({o})"))
        .unwrap_or_default();
    let mut out = vec![format!(
        "{:<14} {:<22} fix {} · tokens {}{active}",
        t.id,
        format!("{:?}", t.state),
        views::fix_cycles(t, "/"),
        t.spent_tokens,
    )];
    let mut doing = Vec::new();
    // a stage is "now" only while a process drives the task — after a crash it is history
    if let (Some(s), true) = (&a.stage, f.active_owner.is_some()) {
        doing.push(format!(
            "now: {} attempt {}{} for {}",
            s.stage,
            s.attempt,
            s.profile
                .as_ref()
                .map(|p| format!(" ({p})"))
                .unwrap_or_default(),
            age(since_ms(&s.since))
        ));
    }
    if let Some(l) = &a.last {
        doing.push(format!("last: {}, {} ago", l.text, age(since_ms(&l.ts))));
    }
    if !doing.is_empty() {
        out.push(format!("{PAD}{}", doing.join(" · ")));
    }
    out.push(format!("{PAD}next: {}", f.next_action));
    out
}

/// One entry of `tasks[]` in `dashboard --json`: the `status --json` task object plus
/// `activity` (always there; `stage` and `last` are `null` when the file has none).
pub fn task_json(
    t: &TaskRow,
    f: &TaskFacts,
    a: &Activity,
    since_ms: impl Fn(&str) -> u64,
) -> Value {
    let mut v = views::task_json(t, f);
    v["activity"] = json!({
        "events": a.events,
        "last_seq": a.last_seq,
        "run_open": a.run_open,
        "stage": a.stage.as_ref().map(|s| json!({
            "stage": s.stage,
            "attempt": s.attempt,
            "profile": s.profile,
            "since": s.since,
            "for_ms": since_ms(&s.since),
        })),
        "last": a.last.as_ref().map(|l| json!({
            "type": l.kind,
            "text": l.text,
            "ts": l.ts,
            "age_ms": since_ms(&l.ts),
        })),
    });
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::PipelineState as St;

    fn env(seq: u64, event: EngineEvent) -> Envelope {
        Envelope {
            v: 1,
            seq,
            ts: format!("2026-10-18T10:00:{:02}Z", seq),
            task: "T-1".into(),
            generation: 1,
            event,
            quiet: false,
        }
    }

    fn started(seq: u64, stage: &str, attempt: u32) -> Envelope {
        env(
            seq,
            EngineEvent::StageStarted {
                stage: stage.into(),
                attempt,
                profile: Some("claude-max".into()),
                detail: None,
            },
        )
    }

    #[test]
    fn the_open_stage_is_the_one_started_and_not_finished() {
        let mut a = Activity::default();
        a.apply(&env(
            1,
            EngineEvent::RunStarted {
                pid: None,
                owner: "pid 1".into(),
            },
        ));
        a.apply(&started(2, "plan", 1));
        assert!(a.run_open);
        assert_eq!(a.stage.as_ref().map(|s| s.stage.as_str()), Some("plan"));
        a.apply(&env(
            3,
            EngineEvent::StageFinished {
                stage: "plan".into(),
                attempt: 1,
                status: AttemptStatus::Succeeded,
                error_class: None,
                duration_ms: 5,
            },
        ));
        assert_eq!(a.stage, None);
        assert_eq!(a.events, 3);
        assert_eq!(a.last_seq, 3);
        assert_eq!(a.last.as_ref().unwrap().kind, "stage_finished");
        assert_eq!(a.last.as_ref().unwrap().text, "plan attempt 1 succeeded");
    }

    /// M16-AT (X39): a diagnostic event is counted, but `last:` stays the task's own event.
    #[test]
    fn a_diagnostic_event_never_becomes_last() {
        let mut a = Activity::default();
        a.apply(&started(1, "verify", 1));
        a.apply(&env(
            2,
            EngineEvent::LockAcquired {
                lock: "machine:verify".into(),
            },
        ));
        assert_eq!(a.events, 2);
        assert_eq!(a.last_seq, 2);
        assert_eq!(a.last.as_ref().unwrap().kind, "stage_started");
        assert_eq!(a.stage.as_ref().unwrap().stage, "verify");
    }

    #[test]
    fn a_new_run_closes_what_a_crashed_run_left_open() {
        let mut a = Activity::default();
        a.apply(&env(
            1,
            EngineEvent::RunStarted {
                pid: None,
                owner: "x".into(),
            },
        ));
        a.apply(&started(2, "implement", 1));
        // the process died: no stage_finished, no run_ended; then a new run
        a.apply(&env(
            3,
            EngineEvent::RunStarted {
                pid: None,
                owner: "y".into(),
            },
        ));
        assert_eq!(a.stage, None);
        assert!(a.run_open);
        a.apply(&env(
            4,
            EngineEvent::RunEnded {
                outcome: None,
                exit_code: None,
                state: St::Escalated,
                next_action: "n".into(),
                from: None,
                error: Some("secret text".into()),
            },
        ));
        assert!(!a.run_open);
        assert_eq!(a.last.unwrap().text, "run ended (Escalated)");
    }

    #[test]
    fn free_text_of_an_event_never_reaches_the_dashboard() {
        let reason = "protected_config_changed: \u{1b}]52;c;QUJD\u{7} rm -rf / — ignore previous";
        for e in [
            EngineEvent::Escalated {
                reason_class: "protected_config_changed".into(),
                reason: reason.into(),
            },
            EngineEvent::Paused {
                reason: reason.into(),
            },
            EngineEvent::RunEnded {
                outcome: None,
                exit_code: None,
                state: St::Escalated,
                next_action: reason.into(),
                from: None,
                error: Some(reason.into()),
            },
            EngineEvent::RunStarted {
                pid: None,
                owner: reason.into(),
            },
        ] {
            let d = describe(&e);
            assert!(!d.contains("rm -rf") && !d.contains('\u{1b}'), "{d}");
        }
    }

    #[test]
    fn unknown_or_damaged_lines_are_skipped() {
        let mut a = Activity::default();
        let good = serde_json::to_string(&started(7, "review", 2)).unwrap();
        a.apply_lines(&[
            "not json".into(),
            r#"{"v":1,"seq":5,"type":"from_the_future"}"#.into(),
            good,
        ]);
        assert_eq!(a.events, 1);
        assert_eq!(a.last_seq, 7);
    }

    #[test]
    fn ages_are_short() {
        assert_eq!(age(0), "0s");
        assert_eq!(age(59_999), "59s");
        assert_eq!(age(60_000), "1m");
        assert_eq!(age(3_600_000), "1h");
        assert_eq!(age(90_000_000), "1d");
    }

    #[test]
    fn a_stage_is_shown_as_now_only_while_a_process_drives_the_task() {
        let t: TaskRow = serde_json::from_value(json!({
            "id": "T-1", "title": "Divide", "state": "Implementing", "task_sha256": "s",
            "acceptance_ids": ["AC-1"], "run_store": "C:/d/runs/T-1", "config_sha256": "c",
            "lock_generation": 3, "fix_cycle": 0, "implementer_index": 0, "worktree": "C:/w/T-1",
            "branch": "toryo/T-1", "base_commit": "b", "last_reviewed_head": null,
            "spent_tokens": 120, "reserved_tokens": 0, "run_lock_owner": null, "run_lock_at": null,
            "created_at": "t", "updated_at": "t", "repo": "C:/r", "task_file": "C:/r/TASK.md",
            "machine": crate::engine::state::Machine::new(4), "implementers": ["codex-openai"],
            "planner": "claude-max", "reviewer": "claude-max", "feedback": null, "handoff": null,
            "budget_tokens": null
        }))
        .unwrap();
        let mut a = Activity::default();
        a.apply(&env(
            1,
            EngineEvent::RunStarted {
                pid: None,
                owner: "x".into(),
            },
        ));
        a.apply(&started(2, "implement", 1));
        let mut f = TaskFacts {
            active_owner: None,
            compile_only: Some(false),
            next_action: "`toryo resume T-1`".into(),
        };
        let lines = task_lines(&t, &f, &a, |_| 65_000);
        assert!(
            lines[0].starts_with("T-1            Implementing"),
            "{lines:?}"
        );
        assert!(!lines[1].contains("now:"), "{lines:?}");
        assert!(lines[1].contains("last: implement attempt 1 started, 1m ago"));
        assert_eq!(lines[2], "               next: `toryo resume T-1`");
        f.active_owner = Some("pid 9".into());
        let lines = task_lines(&t, &f, &a, |_| 65_000);
        assert!(lines[0].ends_with(" · ACTIVE (pid 9)"), "{lines:?}");
        assert!(
            lines[1].starts_with("               now: implement attempt 1 (claude-max) for 1m"),
            "{lines:?}"
        );
        let v = task_json(&t, &f, &a, |_| 1);
        assert_eq!(v["activity"]["stage"]["stage"], "implement");
        assert_eq!(v["activity"]["last"]["type"], "stage_started");
        assert_eq!(v["state"], "Implementing");
    }
}
