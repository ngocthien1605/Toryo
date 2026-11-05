//! Machine-readable views of a task (M10, DES-10): the JSON documents of `toryo status --json`
//! and `toryo task show --json`. Field names are snake_case; an absent value is `null`, so every
//! key is always there. The schema is in the README ("JSON output").

use std::path::Path;

use serde_json::{json, Value};

use super::store::{ApprovalRow, AttemptRow, TaskRow};

/// What `status` shows of a task besides its row: computed by the caller (lock probe, frozen
/// configuration, cleanup).
pub struct TaskFacts {
    /// The lock owner while another process holds the task lock.
    pub active_owner: Option<String>,
    /// R59: the verify of this task proves compile level only (X++).
    pub compile_only: Option<bool>,
    pub next_action: String,
    /// M17-A7 (AC-11): `{ proven, total, exempt, pass }` of the verify result in force, when it has
    /// a red-green gate; `None` (`null`) otherwise.
    pub red_green: Option<Value>,
    /// M17-B13 (AC-27): the best-of-N fan-out ([`fanout_of`]); `None` (`candidates: null`) when the
    /// task has none.
    pub fanout: Option<Value>,
}

/// The fix-cycle counters as `status` and SUMMARY.md show them: `1/2` (with `sep` between)
/// for the single bound; `1 (gate retries 1/3, review rounds 0/3)` with the org chart's split
/// limits (M11-C4).
pub fn fix_cycles(t: &TaskRow, sep: &str) -> String {
    match t.machine.split_limits() {
        Some(l) => format!(
            "{} (gate retries {}/{}, review rounds {}/{})",
            t.fix_cycle, l.gate_retries, l.max_gate_retries, l.review_rounds, l.max_review_rounds
        ),
        None => format!("{}{sep}{}", t.fix_cycle, t.machine.max_fix_cycles()),
    }
}

/// M11-E3: what an environment holds, in words: `approved` once the holder's final gate
/// approved **this commit** (`approved_head`: the head it approved, M11-R8), else `not
/// approved yet`; `deploying now` while the gate runs (`active`: the holder's run is alive),
/// `interrupted …` when it was killed, stopped or timed out, `the deploy failed`.
pub fn deployment_status(
    d: &super::store::Deployment,
    holder: Option<super::state::PipelineState>,
    approved_head: Option<&str>,
    active: bool,
) -> &'static str {
    use super::state::PipelineState as St;
    match (d.finished_at.as_ref(), d.ok, holder) {
        (None, _, _) if active => "deploying now",
        (None, _, _) | (Some(_), None, _) => {
            "interrupted — the environment may hold a partial deploy"
        }
        (Some(_), Some(false), _) => "the deploy failed",
        (_, _, Some(St::Completed)) if approved_head == Some(d.sha.as_str()) => "approved",
        (_, _, Some(St::Cancelled)) => "not approved — the task was cancelled",
        _ => "not approved yet",
    }
}

/// M11-E3: `d365-env holds BR-001 @ 1a2b3c4 (not approved yet)`.
pub fn deployment_line(d: &super::store::Deployment, status: &str) -> String {
    format!(
        "{} holds {} @ {} ({status})",
        d.lock,
        d.task,
        crate::git::short(&d.sha)
    )
}

/// M11-E3: one entry of `environments[]` in `status --json` / `task show --json`.
pub fn deployment_json(d: &super::store::Deployment, status: &str, approved: bool) -> Value {
    json!({
        "lock": d.lock,
        "task": d.task,
        "sha": d.sha,
        "gate": d.gate,
        "machine": d.machine,
        "started_at": d.started_at,
        "finished_at": d.finished_at,
        "ok": d.ok,
        "approved": approved,
        "status": status,
    })
}

/// One task of `status --json`. With split limits (M11-C4) it also has `gate_retries`,
/// `max_gate_retries`, `review_rounds`, `max_review_rounds` — only then, so the document of a
/// single-bound task is unchanged.
pub fn task_json(t: &TaskRow, f: &TaskFacts) -> Value {
    let mut v = task_json_m10(t, f);
    if let Some(l) = t.machine.split_limits() {
        v["gate_retries"] = json!(l.gate_retries);
        v["max_gate_retries"] = json!(l.max_gate_retries);
        v["review_rounds"] = json!(l.review_rounds);
        v["max_review_rounds"] = json!(l.max_review_rounds);
    }
    v
}

fn task_json_m10(t: &TaskRow, f: &TaskFacts) -> Value {
    json!({
        "id": t.id,
        "title": t.title,
        "state": t.state,
        "fix_cycle": t.fix_cycle,
        "max_fix_cycles": t.machine.max_fix_cycles(),
        "spent_tokens": t.spent_tokens,
        "reserved_tokens": t.reserved_tokens,
        "budget_tokens": t.budget_tokens,
        "active_owner": f.active_owner,
        "compile_only": f.compile_only,
        "next_action": f.next_action,
        // M17-A7 (AC-11, AC-13): always there, `null` without a red-green gate
        "red_green": f.red_green,
        // M17-B13 (AC-27, AC-13): always there, `null` without a fan-out
        "candidates": f.fanout.as_ref().map(|v| v["candidates"].clone()),
    })
}

/// M17-B13 (AC-27): the fan-out of a task, in Toryo's words only (X35) — read-only, like
/// [`red_green_of`]: `{ phase, n, winner, by, reason, note, candidates: [ { id, profile, outcome,
/// head, verify, tokens, winner, by, branch } ] }`. `reason` is the score's rung (or why the score
/// decided); the compare turn's own reason is the agent's text and is never shown here. `None`:
/// the task has no fan-out, or the store cannot be read.
pub fn fanout_of(st: &super::store::Store, t: &TaskRow) -> Option<Value> {
    let i = t.id.clone();
    let (fan, rows, atts, tokens) = st
        .read(move |tx| {
            let Some(f) = super::store::fanout(tx, &i)? else {
                return Ok(None);
            };
            let rows = super::store::candidates(tx, &i)?;
            let atts = super::store::attempts(tx, &i)?;
            let mut tokens = Vec::new();
            for c in &rows {
                tokens.push(super::store::candidate_tokens(tx, &i, c.k)?);
            }
            Ok(Some((f, rows, atts, tokens)))
        })
        .ok()??;
    let by = fan.by.clone();
    let list: Vec<Value> = rows
        .iter()
        .zip(tokens)
        .map(|(c, tokens)| {
            let stage = super::candidates::stage(c.k);
            let last = atts.iter().rev().find(|a| a.stage == stage);
            let outcome = match c.outcome.as_deref() {
                Some(o) => match o.strip_prefix("disqualified:") {
                    Some(class) => format!("disqualified({class})"),
                    None => o.to_string(),
                },
                None => match last.map(|a| a.status) {
                    Some(super::store::AttemptStatus::Interrupted) => "interrupted".into(),
                    Some(super::store::AttemptStatus::Running) => "running".into(),
                    _ if c.state == "turn_done" => "running".into(),
                    _ => "pending".into(),
                },
            };
            let winner = fan.winner == Some(c.k);
            json!({
                "id": super::candidates::id(c.k),
                "profile": c.profile,
                "outcome": outcome,
                "head": c.head,
                "verify": c.verify_pass,
                "tokens": tokens,
                "winner": winner,
                "by": if winner { by.clone() } else { None },
                "branch": c.branch,
            })
        })
        .collect();
    Some(json!({
        "phase": fan.phase,
        "n": fan.n,
        "winner": fan.winner.map(super::candidates::id),
        "by": fan.by,
        "reason": if fan.by.as_deref() == Some("compare") { None } else { fan.reason.clone() },
        "note": fan.note,
        "candidates": list,
    }))
}

/// M17-B13: the fan-out phase as the dashboard's `now:` says it — `candidates 1/2 verifying`
/// (how many of the candidates are past this phase, of how many) — or `None` once it ended.
pub fn fanout_progress(v: &Value) -> Option<String> {
    let list = v["candidates"].as_array()?;
    let n = list.len();
    let count = |f: &dyn Fn(&Value) -> bool| list.iter().filter(|c| f(c)).count();
    let turned = |c: &Value| {
        !matches!(
            c["outcome"].as_str(),
            Some("running" | "pending" | "interrupted")
        )
    };
    let (done, verb) = match v["phase"].as_str()? {
        "creating" => (0, "creating worktrees"),
        "turns" => (count(&turned), "implementing"),
        "verify" => (
            count(&|c: &Value| c["outcome"] != "checkpointed" || !c["verify"].is_null()),
            "verifying",
        ),
        "select" => (n, "selecting"),
        "selected" => (n, "promoting"),
        _ => return None,
    };
    Some(format!("candidates {done}/{n} {verb}"))
}

/// M17-B13 (AC-27): the lines `status`, `task show` and the final gate print for a fan-out, in
/// Toryo's words: each candidate's outcome, then the winner, how it was chosen and the other
/// candidates' kept branches (or the note of the single path).
pub fn fanout_lines(v: &Value) -> Vec<String> {
    let list = v["candidates"].as_array().cloned().unwrap_or_default();
    let words: Vec<String> = list
        .iter()
        .map(|c| {
            let gates = match c["verify"].as_bool() {
                Some(true) => ", check gates passed",
                Some(false) => ", check gates failed",
                None => "",
            };
            format!(
                "{} {} {}{gates}",
                c["id"].as_str().unwrap_or("?"),
                c["profile"].as_str().unwrap_or("?"),
                c["outcome"].as_str().unwrap_or("?")
            )
        })
        .collect();
    let phase = v["phase"].as_str().unwrap_or("?");
    let mut out = vec![if matches!(phase, "promoted" | "single") {
        format!("best-of-N: {}", words.join(" · "))
    } else {
        format!("best-of-N ({phase}): {}", words.join(" · "))
    }];
    if let Some(w) = list.iter().find(|c| c["winner"] == true) {
        let losers: Vec<&str> = list
            .iter()
            .filter(|c| c["winner"] != true)
            .filter_map(|c| c["branch"].as_str())
            .collect();
        out.push(format!(
            "winner {} ({}) by {}{}{}",
            w["id"].as_str().unwrap_or("?"),
            w["profile"].as_str().unwrap_or("?"),
            v["by"].as_str().unwrap_or("?"),
            v["reason"]
                .as_str()
                .map(|r| format!(" — {r}"))
                .unwrap_or_default(),
            if losers.is_empty() {
                String::new()
            } else {
                format!(
                    " · the other candidates' branches are kept: {}",
                    losers.join(", ")
                )
            }
        ));
    }
    if let Some(n) = v["note"].as_str() {
        out.push(n.to_string());
    }
    out
}

/// M17-A7: the red-green summary of the task's verify result in force (read-only: `status` and the
/// dashboard never take the write lock, X35), or `None` — no verify result, none with a
/// red-green gate, or one that cannot be read.
pub fn red_green_of(st: &super::store::Store, t: &TaskRow) -> Option<Value> {
    let v = super::artifacts::head_read(st, t, "verify-result").ok()??;
    let bytes = super::artifacts::read_version(&v).ok()?;
    let vr: Value = serde_json::from_slice(&bytes).ok()?;
    super::redgreen::summary_of(&vr)
}

/// A usage row as `task show` prints it (R41).
pub struct UsageLine {
    pub id: i64,
    pub attempt: String,
    pub profile: String,
    pub accounting: String,
    pub budget_tokens: Option<i64>,
    pub reservation_tokens: Option<i64>,
    pub reconciled_by: Option<i64>,
    pub includes_unreported: bool,
}

/// `task show --json`: the task object of [`task_json`] plus the worktree, branch, run store,
/// stage attempts, usage rows, gate occurrences and the flight recorder's file.
pub fn show_json(
    task: Value,
    t: &TaskRow,
    attempts: &[AttemptRow],
    usage: &[UsageLine],
    gates: &[ApprovalRow],
    events_file: &Path,
) -> Value {
    let mut v = task;
    v["worktree"] = json!(t.worktree);
    v["branch"] = json!(t.branch);
    v["run_store"] = json!(t.run_store);
    v["attempts"] = attempts
        .iter()
        .map(|a| {
            json!({
                "id": a.id,
                "stage": a.stage,
                "attempt": a.attempt,
                "status": a.status,
                "profile": a.profile,
                "error_class": a.error_class,
                "head_after": a.git_head_after,
                "partial_commit": a.partial_commit,
            })
        })
        .collect();
    v["usage"] = usage
        .iter()
        .map(|u| {
            json!({
                "id": u.id,
                "attempt": u.attempt,
                "profile": u.profile,
                "accounting": u.accounting,
                "counted_tokens": u.budget_tokens,
                "reservation_tokens": u.reservation_tokens,
                "reconciled_by": u.reconciled_by,
                "includes_unreported": u.includes_unreported,
            })
        })
        .collect();
    v["gates"] = gates
        .iter()
        .map(|g| {
            json!({
                "gate": g.gate,
                "occurrence": g.occurrence,
                "status": g.status,
                "subject_digest": g.subject_digest,
                "reason": g.reason,
                "requested_at": g.requested_at,
                "resolved_at": g.resolved_at,
            })
        })
        .collect();
    v["events_file"] = json!(events_file);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::state::Machine;
    use crate::engine::store::AttemptStatus;

    fn row() -> TaskRow {
        serde_json::from_value(json!({
            "id": "T-1", "title": "Divide", "state": "Verifying", "task_sha256": "s",
            "acceptance_ids": ["AC-1"], "run_store": "C:/d/runs/T-1", "config_sha256": "c",
            "lock_generation": 3, "fix_cycle": 1, "implementer_index": 0, "worktree": "C:/w/T-1",
            "branch": "toryo/T-1", "base_commit": "b", "last_reviewed_head": null,
            "spent_tokens": 120, "reserved_tokens": 0, "run_lock_owner": null, "run_lock_at": null,
            "created_at": "t", "updated_at": "t", "repo": "C:/r", "task_file": "C:/r/TASK.md",
            "machine": Machine::new(4), "implementers": ["codex-openai"], "planner": "claude-max",
            "reviewer": "claude-max", "feedback": null, "handoff": null, "budget_tokens": null
        }))
        .unwrap()
    }

    #[test]
    fn a_task_object_has_every_key_even_when_empty() {
        let v = task_json(
            &row(),
            &TaskFacts {
                active_owner: None,
                compile_only: Some(false),
                next_action: "`toryo run T-1`".into(),
                red_green: None,
                fanout: None,
            },
        );
        assert_eq!(v["id"], "T-1");
        assert_eq!(v["state"], "Verifying");
        assert_eq!(
            (v["fix_cycle"].clone(), v["max_fix_cycles"].clone()),
            (json!(1), json!(4))
        );
        assert_eq!(v["spent_tokens"], 120);
        // (M17: `red_green` is `null` without a red-green gate, `candidates` without a fan-out)
        for k in ["budget_tokens", "active_owner", "red_green", "candidates"] {
            assert!(v.get(k).is_some_and(Value::is_null), "{k}: {v}");
        }
        assert_eq!(v["compile_only"], false);
        assert_eq!(v["next_action"], "`toryo run T-1`");
    }

    #[test]
    fn show_adds_the_attempts_usage_gates_and_the_events_file() {
        let t = row();
        let a = AttemptRow {
            id: "T-1/implement/1".into(),
            task_id: "T-1".into(),
            stage: "implement".into(),
            attempt: 1,
            kind: "agent".into(),
            fallback_from: None,
            profile: Some("codex-openai".into()),
            external_session_id: None,
            pid: None,
            pid_created_at: None,
            status: AttemptStatus::Succeeded,
            error_class: None,
            error_detail: None,
            git_head_before: None,
            git_head_after: Some("abc".into()),
            subject_commit: None,
            partial_commit: None,
            checkpoint_kind: None,
            partial_snapshot_id: None,
            correction_turns: 0,
            started_at: "t".into(),
            ended_at: None,
            context_sha256: None,
            undo_pending: false,
        };
        let u = UsageLine {
            id: 7,
            attempt: a.id.clone(),
            profile: "codex-openai".into(),
            accounting: "unknown".into(),
            budget_tokens: None,
            reservation_tokens: Some(200_000),
            reconciled_by: None,
            includes_unreported: false,
        };
        let g = ApprovalRow {
            gate: "plan".into(),
            occurrence: 1,
            status: "approved".into(),
            reason: None,
            subject_digest: "d".into(),
            requested_at: "t".into(),
            resolved_at: Some("t2".into()),
        };
        let v = show_json(
            json!({"id": "T-1"}),
            &t,
            &[a],
            &[u],
            &[g],
            Path::new("C:/d/runs/T-1/events.jsonl"),
        );
        assert_eq!(v["id"], "T-1");
        assert_eq!(v["branch"], "toryo/T-1");
        assert_eq!(v["attempts"][0]["status"], "succeeded");
        assert_eq!(v["attempts"][0]["head_after"], "abc");
        assert!(v["attempts"][0]["partial_commit"].is_null());
        assert_eq!(v["usage"][0]["reservation_tokens"], 200_000);
        assert_eq!(v["gates"][0]["status"], "approved");
        assert_eq!(v["events_file"], "C:/d/runs/T-1/events.jsonl");
    }

    /// M11-E3: what an environment holds, in words, for each state of its record and of the
    /// holder task; and the line `status` prints.
    #[test]
    fn deployment_status_in_words() {
        use crate::engine::state::PipelineState as St;
        use crate::engine::store::Deployment;
        let d = |finished: bool, ok: Option<bool>| Deployment {
            lock: "d365-env".into(),
            task: "BR-001".into(),
            sha: "1a2b3c4d5e6f7a8b".into(),
            gate: "dbsync".into(),
            machine: "UDE-1".into(),
            started_at: "t0".into(),
            finished_at: finished.then(|| "t1".into()),
            ok,
        };
        let cases = [
            (d(false, None), Some(St::Verifying), true, "deploying now"),
            (
                d(false, None),
                Some(St::Verifying),
                false,
                "interrupted — the environment may hold a partial deploy",
            ),
            (
                d(true, None),
                Some(St::Verifying),
                false,
                "interrupted — the environment may hold a partial deploy",
            ),
            (
                d(true, Some(false)),
                Some(St::Fixing),
                false,
                "the deploy failed",
            ),
            (d(true, Some(true)), Some(St::Completed), false, "approved"),
            (
                d(true, Some(true)),
                Some(St::Cancelled),
                false,
                "not approved — the task was cancelled",
            ),
            (
                d(true, Some(true)),
                Some(St::WaitingFinalApproval),
                false,
                "not approved yet",
            ),
            (d(true, Some(true)), None, false, "not approved yet"),
        ];
        // (the holder's final gate approved this row's commit)
        for (dep, state, active, want) in cases {
            let head = Some(dep.sha.as_str());
            assert_eq!(
                deployment_status(&dep, state, head, active),
                want,
                "{dep:?}"
            );
        }
        // M11-R8: a completed task whose final gate approved another commit, or none known
        let done = d(true, Some(true));
        for head in [Some("ffffffffffffffff"), None] {
            assert_eq!(
                deployment_status(&done, Some(St::Completed), head, false),
                "not approved yet",
                "{head:?}"
            );
        }
        assert_eq!(
            deployment_line(&d(true, Some(true)), "not approved yet"),
            "d365-env holds BR-001 @ 1a2b3c4d5e6f (not approved yet)"
        );
        assert!(crate::engine::lock::valid_lock_name("d365-env"));
        assert!(crate::engine::lock::valid_lock_name("env_1.a"));
        for bad in ["", "..", "a/b", r"a\b", "c:x", "x y"] {
            assert!(!crate::engine::lock::valid_lock_name(bad), "{bad}");
        }
    }
}
