//! The `tasks` table (R23-32, moved as it was): the task row, its transitions with the approvals
//! they open and close (R44a), the events, the gate subject digests.

use super::*;

/// M13-11 (X28): take back a task that `task create` could not finish — its row and what the
/// creation had written for it (the first artifacts, their events). Only for a task that
/// never ran: nothing else refers to it yet.
pub fn remove_created_task(tx: &Transaction, id: &str) -> Result<()> {
    for sql in [
        "DELETE FROM artifact_heads WHERE task_id = ?1",
        "DELETE FROM artifacts WHERE task_id = ?1",
        "DELETE FROM events WHERE task_id = ?1",
        "DELETE FROM tasks WHERE id = ?1",
    ] {
        tx.execute(sql, [id])?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRow {
    pub id: String,
    pub title: String,
    pub state: PipelineState,
    pub task_sha256: String,
    pub acceptance_ids: Vec<String>,
    pub run_store: PathBuf,
    pub config_sha256: String,
    pub lock_generation: i64,
    pub fix_cycle: u32,
    pub implementer_index: usize,
    pub worktree: PathBuf,
    pub branch: String,
    pub base_commit: String,
    pub last_reviewed_head: Option<String>,
    pub spent_tokens: i64,
    pub reserved_tokens: i64,
    pub run_lock_owner: Option<String>,
    pub run_lock_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub repo: PathBuf,
    pub task_file: PathBuf,
    pub machine: Machine,
    pub implementers: Vec<String>,
    pub planner: String,
    pub reviewer: String,
    /// What the next writing (or planning) turn must address.
    pub feedback: Option<String>,
    /// Hand-over note for the next writing turn (fallback switch, crash recovery).
    pub handoff: Option<String>,
    /// R41 admission cap (`pipeline.budget_tokens`, `toryo budget set`); None = no cap.
    pub budget_tokens: Option<i64>,
}

impl TaskRow {
    /// The run store's artifact directory (`runstore/`).
    pub fn artifacts(&self) -> PathBuf {
        self.run_store.join("runstore")
    }
    /// Where agent streams and prompts are captured.
    pub fn captures(&self) -> PathBuf {
        self.run_store.join("captures")
    }
    pub fn current_implementer(&self) -> Option<&str> {
        self.implementers
            .get(self.implementer_index)
            .map(String::as_str)
    }
}

pub struct NewTask {
    pub id: String,
    pub title: String,
    pub task_sha256: String,
    pub acceptance_ids: Vec<String>,
    pub run_store: PathBuf,
    pub config_sha256: String,
    pub worktree: PathBuf,
    pub branch: String,
    pub base_commit: String,
    pub repo: PathBuf,
    pub task_file: PathBuf,
    pub machine: Machine,
    pub implementers: Vec<String>,
    pub planner: String,
    pub reviewer: String,
    pub budget_tokens: Option<i64>,
}

pub(super) const TASK_COLS: &str = "id, title, state, task_sha256, acceptance_ids, run_store, config_sha256, lock_generation, fix_cycle, implementer_index, worktree, branch, base_commit, last_reviewed_head, spent_tokens, reserved_tokens, run_lock_owner, run_lock_at, created_at, updated_at, repo, task_file, machine, implementers, planner, reviewer, feedback, handoff, budget_tokens";

pub(super) fn state_from(s: &str) -> rusqlite::Result<PipelineState> {
    serde_json::from_value(Value::String(s.to_string())).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

pub(super) fn json_from<T: for<'de> Deserialize<'de>>(s: &str) -> rusqlite::Result<T> {
    serde_json::from_str(s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

pub(super) fn task_from_row(r: &rusqlite::Row) -> rusqlite::Result<TaskRow> {
    Ok(TaskRow {
        id: r.get(0)?,
        title: r.get(1)?,
        state: state_from(&r.get::<_, String>(2)?)?,
        task_sha256: r.get(3)?,
        acceptance_ids: json_from(&r.get::<_, String>(4)?)?,
        run_store: PathBuf::from(r.get::<_, String>(5)?),
        config_sha256: r.get(6)?,
        lock_generation: r.get(7)?,
        fix_cycle: r.get::<_, i64>(8)? as u32,
        implementer_index: r.get::<_, i64>(9)? as usize,
        worktree: PathBuf::from(r.get::<_, String>(10)?),
        branch: r.get(11)?,
        base_commit: r.get(12)?,
        last_reviewed_head: r.get(13)?,
        spent_tokens: r.get(14)?,
        reserved_tokens: r.get(15)?,
        run_lock_owner: r.get(16)?,
        run_lock_at: r.get(17)?,
        created_at: r.get(18)?,
        updated_at: r.get(19)?,
        repo: PathBuf::from(r.get::<_, String>(20)?),
        task_file: PathBuf::from(r.get::<_, String>(21)?),
        machine: json_from(&r.get::<_, String>(22)?)?,
        implementers: json_from(&r.get::<_, String>(23)?)?,
        planner: r.get(24)?,
        reviewer: r.get(25)?,
        feedback: r.get(26)?,
        handoff: r.get(27)?,
        budget_tokens: r.get(28)?,
    })
}

pub(super) fn state_str(s: PipelineState) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

pub fn insert_task(tx: &Transaction, t: &NewTask) -> Result<()> {
    let at = now();
    let n = tx.execute(
        "INSERT OR IGNORE INTO tasks (id, title, state, task_sha256, acceptance_ids, run_store, config_sha256, fix_cycle, worktree, branch, base_commit, created_at, updated_at, repo, task_file, machine, implementers, planner, reviewer, budget_tokens)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9, ?10, ?11, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        params![
            t.id,
            t.title,
            state_str(t.machine.state()),
            t.task_sha256,
            serde_json::to_string(&t.acceptance_ids)?,
            t.run_store.display().to_string(),
            t.config_sha256,
            t.worktree.display().to_string(),
            t.branch,
            t.base_commit,
            at,
            t.repo.display().to_string(),
            t.task_file.display().to_string(),
            serde_json::to_string(&t.machine)?,
            serde_json::to_string(&t.implementers)?,
            t.planner,
            t.reviewer,
            t.budget_tokens,
        ],
    )?;
    if n == 0 {
        bail!("task {} already exists", t.id);
    }
    event(
        tx,
        Some(&t.id),
        None,
        "task_created",
        &serde_json::json!({"branch": t.branch, "base": t.base_commit}),
    )?;
    Ok(())
}

pub fn get_task(tx: &Transaction, id: &str) -> Result<TaskRow> {
    tx.query_row(
        &format!("SELECT {TASK_COLS} FROM tasks WHERE id = ?1"),
        [id],
        task_from_row,
    )
    .optional()?
    .ok_or_else(|| anyhow!("no task `{id}`"))
}

/// R23-17: the id of a task that differs from `id` only in (ASCII) case, if one exists.
pub fn task_id_in_other_case(tx: &Transaction, id: &str) -> Result<Option<String>> {
    Ok(tx
        .query_row(
            "SELECT id FROM tasks WHERE lower(id) = lower(?1) AND id <> ?1 LIMIT 1",
            [id],
            |r| r.get(0),
        )
        .optional()?)
}

pub fn list_tasks(tx: &Transaction) -> Result<Vec<TaskRow>> {
    let mut st = tx.prepare(&format!(
        "SELECT {TASK_COLS} FROM tasks ORDER BY created_at, id"
    ))?;
    let rows = st.query_map([], task_from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn set_feedback(tx: &Transaction, id: &str, feedback: Option<&str>) -> Result<()> {
    tx.execute(
        "UPDATE tasks SET feedback = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, feedback, now()],
    )?;
    Ok(())
}

pub fn set_handoff(tx: &Transaction, id: &str, handoff: Option<&str>) -> Result<()> {
    tx.execute(
        "UPDATE tasks SET handoff = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, handoff, now()],
    )?;
    Ok(())
}

/// `toryo task config --reload` (R35): the new frozen configuration and the roles it resolves.
pub fn set_config(
    tx: &Transaction,
    id: &str,
    config_sha256: &str,
    planner: &str,
    implementers: &[String],
    reviewer: &str,
    idx: usize,
) -> Result<()> {
    tx.execute(
        "UPDATE tasks SET config_sha256 = ?2, planner = ?3, implementers = ?4, reviewer = ?5, implementer_index = ?6, updated_at = ?7 WHERE id = ?1",
        params![id, config_sha256, planner, serde_json::to_string(implementers)?, reviewer, idx as i64, now()],
    )?;
    Ok(())
}

pub fn set_implementer_index(tx: &Transaction, id: &str, idx: usize) -> Result<()> {
    tx.execute(
        "UPDATE tasks SET implementer_index = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, idx as i64, now()],
    )?;
    Ok(())
}

pub fn set_last_reviewed(tx: &Transaction, id: &str, head: &str) -> Result<()> {
    tx.execute(
        "UPDATE tasks SET last_reviewed_head = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, head, now()],
    )?;
    Ok(())
}

pub fn event(
    tx: &Transaction,
    task: Option<&str>,
    attempt: Option<&str>,
    kind: &str,
    payload: &Value,
) -> Result<()> {
    tx.execute(
        "INSERT INTO events (task_id, stage_attempt_id, kind, payload, at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![task, attempt, kind, payload.to_string(), now()],
    )?;
    Ok(())
}

pub fn events(tx: &Transaction, task: &str) -> Result<Vec<(i64, String, Value, String)>> {
    let mut st =
        tx.prepare("SELECT seq, kind, payload, at FROM events WHERE task_id = ?1 ORDER BY seq")?;
    let rows = st.query_map([task], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?, r.get(3)?))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (seq, kind, payload, at) = r?;
        out.push((
            seq,
            kind,
            serde_json::from_str(&payload).unwrap_or(Value::Null),
            at,
        ));
    }
    Ok(out)
}

/// M11-R11: record the escalation class on the task's machine (the engine, right after it
/// escalated with that class). No-op unless the task is `Escalated`.
pub fn set_reason_class(tx: &Transaction, task_id: &str, class: &str) -> Result<()> {
    let mut m = get_task(tx, task_id)?.machine;
    m.set_reason_class(class);
    tx.execute(
        "UPDATE tasks SET machine = ?2, updated_at = ?3 WHERE id = ?1",
        params![task_id, serde_json::to_string(&m)?, now()],
    )?;
    Ok(())
}

/// M11-R16: [`transition`] and — for an escalation — its typed class on the machine, in the
/// caller's transaction: there is no moment at which a crash leaves an escalation without its
/// class (a later plain `toryo resume` reads it). For any other event `class` is ignored.
/// R23-35: the class rides in the event ([`with_class`]); the transition sets it.
pub fn transition_classed(
    tx: &Transaction,
    task_id: &str,
    e: PipelineEvent,
    class: Option<&str>,
) -> Result<Machine> {
    transition(tx, task_id, with_class(e, class))
}

/// R23-35: `e` with the escalation class `class` when it is an `Escalate` that names none yet;
/// any other event as it is.
pub fn with_class(e: PipelineEvent, class: Option<&str>) -> PipelineEvent {
    match (e, class) {
        (
            PipelineEvent::Escalate {
                reason,
                class: None,
            },
            Some(c),
        ) => PipelineEvent::Escalate {
            reason,
            class: Some(c.to_string()),
        },
        (e, _) => e,
    }
}

/// R23-08: [`transition`] (with the state before it, R23-25), and when it escalates the task, the class its `escalated` event
/// carries ([`crate::engine::flight::escalation_class`] of `class`, else the event and the reason) on
/// the machine — the machine and the flight recorder name one class. A machine-caused class
/// ([`crate::engine::state::MACHINE_CAUSED`], which lets a plain `resume` go back into the stage) is
/// stored only when the caller gave it as `class`, never when it is only read out of the
/// reason's text or an error class.
pub fn transition_recorded(
    tx: &Transaction,
    task_id: &str,
    e: PipelineEvent,
    class: Option<&str>,
    error_class: Option<&str>,
) -> Result<(PipelineState, Machine)> {
    let name = e.name();
    let before = get_task(tx, task_id)?.state;
    let m = transition(tx, task_id, with_class(e, class))?;
    if m.state() != PipelineState::Escalated || before == PipelineState::Escalated {
        return Ok((before, m));
    }
    let named = class.or(error_class);
    let c = crate::engine::flight::escalation_class(name, named, m.reason().unwrap_or(""));
    if crate::engine::state::is_machine_caused(&c) && class.is_none() {
        return Ok((before, m));
    }
    set_reason_class(tx, task_id, &c)?;
    Ok((before, get_task(tx, task_id)?.machine))
}

/// Apply `e` to the task's machine and persist it — state, projections, the event and the
/// approvals rows (R44a) — in the caller's transaction. On a refused transition nothing is
/// written.
pub fn transition(tx: &Transaction, task_id: &str, e: PipelineEvent) -> Result<Machine> {
    let t = get_task(tx, task_id)?;
    let before = t.machine.clone();
    let after = before.transition(&e).map_err(|err| {
        anyhow!(
            "{task_id}: state machine refused `{}` in {:?}: {err}",
            e.name(),
            before.state()
        )
    })?;
    let at = now();
    tx.execute(
        "UPDATE tasks SET state = ?2, fix_cycle = ?3, paused_from_state = ?4, machine = ?5, updated_at = ?6 WHERE id = ?1",
        params![
            task_id,
            state_str(after.state()),
            after.fix_cycle() as i64,
            after.paused_from().map(state_str),
            serde_json::to_string(&after)?,
            at
        ],
    )?;
    // approvals (R44a): a new gate occurrence opens a row; the decision closes it
    if let Some(old) = before.gate() {
        if after.gate() != Some(old) {
            let status = match e.name() {
                "plan_approved" | "final_approved" => "approved",
                "plan_rejected" | "final_rejected" => "rejected",
                _ => "superseded",
            };
            let reason = match &e {
                PipelineEvent::PlanRejected { reason, .. }
                | PipelineEvent::FinalRejected { reason, .. } => Some(reason.clone()),
                _ => None,
            };
            tx.execute(
                "UPDATE approvals SET status = ?4, reason = ?5, resolved_at = ?6, approved_by = CASE WHEN ?4 = 'approved' THEN ?7 ELSE NULL END
                 WHERE task_id = ?1 AND gate = ?2 AND occurrence = ?3 AND status = 'open'",
                params![task_id, old.kind.name(), old.occurrence as i64, status, reason, at, whoami()],
            )?;
        }
    }
    if let Some(g) = after.gate() {
        if before.gate() != Some(g) {
            let (kind, sha, commit) = match g.kind {
                GateKind::Plan => ("plan", Some(g.subject.clone()), None),
                GateKind::Final => ("commit", None, Some(g.subject.clone())),
            };
            tx.execute(
                "INSERT INTO approvals (id, task_id, gate, occurrence, status, requested_at, subject_kind, subject_sha256, subject_commit, verify_head, subject_digest)
                 VALUES (?1, ?2, ?3, ?4, 'open', ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    format!("{task_id}/{}/{}", g.kind.name(), g.occurrence),
                    task_id,
                    g.kind.name(),
                    g.occurrence as i64,
                    at,
                    kind,
                    sha,
                    commit,
                    after.verified_head(),
                    subject_digest(kind, sha.as_deref(), commit.as_deref(), after.verified_head()),
                ],
            )?;
        }
    }
    event(
        tx,
        Some(task_id),
        None,
        "transition",
        &serde_json::json!({"from": before.state(), "to": after.state(), "event": e, "fix_cycle": after.fix_cycle()}),
    )?;
    Ok(after)
}

/// R32: `sha256(kind|subject_sha256|subject_commit|verify_head)`.
pub fn subject_digest(
    kind: &str,
    sha: Option<&str>,
    commit: Option<&str>,
    verify: Option<&str>,
) -> String {
    use sha2::{Digest, Sha256};
    let s = format!(
        "{kind}|{}|{}|{}",
        sha.unwrap_or(""),
        commit.unwrap_or(""),
        verify.unwrap_or("")
    );
    hex::encode(Sha256::digest(s.as_bytes()))
}

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalRow {
    pub gate: String,
    pub occurrence: u32,
    pub status: String,
    pub reason: Option<String>,
    pub subject_digest: String,
    pub requested_at: String,
    pub resolved_at: Option<String>,
}

pub fn approvals(tx: &Transaction, task: &str) -> Result<Vec<ApprovalRow>> {
    let mut st = tx.prepare("SELECT gate, occurrence, status, reason, subject_digest, requested_at, resolved_at FROM approvals WHERE task_id = ?1 ORDER BY requested_at, gate, occurrence")?;
    let rows = st.query_map([task], |r| {
        Ok(ApprovalRow {
            gate: r.get(0)?,
            occurrence: r.get::<_, i64>(1)? as u32,
            status: r.get(2)?,
            reason: r.get(3)?,
            subject_digest: r.get(4)?,
            requested_at: r.get(5)?,
            resolved_at: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn whoami() -> String {
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "?".into());
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "localhost".into());
    format!("{user}@{host}")
}
