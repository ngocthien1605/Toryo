//! The task lock's bookkeeping and the control requests (R38; R23-32, moved as they were): owner,
//! generation, stop requests.

use super::*;

/// Called by a new lock owner: generation + 1, owner shown by `status`, and every unhandled
/// request addressed to an older owner expires. Returns the new generation.
pub fn claim_lock(tx: &Transaction, task: &str, owner: &str) -> Result<i64> {
    let at = now();
    let (pid, created) = process_identity();
    let n = tx.execute(
        "UPDATE tasks SET lock_generation = lock_generation + 1, run_lock_owner = ?2, run_lock_at = ?3, run_lock_pid = ?4, run_lock_created = ?5 WHERE id = ?1",
        params![task, owner, at, pid, created],
    )?;
    if n == 0 {
        bail!("no task `{task}`");
    }
    let g: i64 = tx.query_row(
        "SELECT lock_generation FROM tasks WHERE id = ?1",
        [task],
        |r| r.get(0),
    )?;
    let expired = tx.execute(
        "UPDATE control_requests SET handled_at = ?3, outcome = 'expired' WHERE task_id = ?1 AND handled_at IS NULL AND target_generation < ?2",
        params![task, g, at],
    )?;
    event(
        tx,
        Some(task),
        None,
        "lock_acquired",
        &serde_json::json!({"owner": owner, "generation": g, "expired_requests": expired}),
    )?;
    Ok(g)
}

pub fn release_lock(tx: &Transaction, task: &str, generation: i64) -> Result<()> {
    tx.execute(
        "UPDATE tasks SET run_lock_owner = NULL, run_lock_at = NULL, run_lock_pid = NULL, run_lock_created = NULL WHERE id = ?1 AND lock_generation = ?2",
        params![task, generation],
    )?;
    Ok(())
}

/// The recorded lock owner: (label, since, pid, created).
pub type LockOwner = (String, String, Option<i64>, Option<String>);

pub fn lock_owner(tx: &Transaction, task: &str) -> Result<Option<LockOwner>> {
    Ok(tx
        .query_row(
            "SELECT run_lock_owner, run_lock_at, run_lock_pid, run_lock_created FROM tasks WHERE id = ?1 AND run_lock_owner IS NOT NULL",
            [task],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?)
}

pub fn insert_stop(tx: &Transaction, task: &str, generation: i64, by: &str) -> Result<i64> {
    tx.execute(
        "INSERT INTO control_requests (task_id, kind, target_generation, requested_by, requested_at) VALUES (?1, 'stop', ?2, ?3, ?4)",
        params![task, generation, by, now()],
    )?;
    Ok(tx.last_insert_rowid())
}

/// An unhandled stop addressed to this owner.
pub fn pending_stop(tx: &Transaction, task: &str, generation: i64) -> Result<Option<i64>> {
    Ok(tx
        .query_row(
            "SELECT id FROM control_requests WHERE task_id = ?1 AND kind = 'stop' AND handled_at IS NULL AND target_generation = ?2 ORDER BY id LIMIT 1",
            params![task, generation],
            |r| r.get(0),
        )
        .optional()?)
}

/// M16-AT (X39): who asked for the oldest unhandled stop of this owner (`requested_by`: an
/// owner label, `… (Ctrl+C)` for a Ctrl+C in the driving process).
pub fn pending_stop_by(tx: &Transaction, task: &str, generation: i64) -> Result<Option<String>> {
    Ok(tx
        .query_row(
            "SELECT requested_by FROM control_requests WHERE task_id = ?1 AND kind = 'stop' AND handled_at IS NULL AND target_generation = ?2 ORDER BY id LIMIT 1",
            params![task, generation],
            |r| r.get(0),
        )
        .optional()?)
}

/// Mark every unhandled stop for this owner handled with `outcome`.
pub fn handle_stops(tx: &Transaction, task: &str, generation: i64, outcome: &str) -> Result<usize> {
    Ok(tx.execute(
        "UPDATE control_requests SET handled_at = ?3, outcome = ?4 WHERE task_id = ?1 AND kind = 'stop' AND handled_at IS NULL AND target_generation = ?2",
        params![task, generation, now(), outcome],
    )?)
}

#[derive(Debug, Clone, Serialize)]
pub struct ControlRow {
    pub id: i64,
    pub target_generation: i64,
    pub handled_at: Option<String>,
    pub outcome: Option<String>,
}

pub fn control_request(tx: &Transaction, id: i64) -> Result<ControlRow> {
    Ok(tx.query_row(
        "SELECT id, target_generation, handled_at, outcome FROM control_requests WHERE id = ?1",
        [id],
        |r| {
            Ok(ControlRow {
                id: r.get(0)?,
                target_generation: r.get(1)?,
                handled_at: r.get(2)?,
                outcome: r.get(3)?,
            })
        },
    )?)
}

pub fn set_control_outcome(tx: &Transaction, id: i64, outcome: &str) -> Result<()> {
    tx.execute(
        "UPDATE control_requests SET handled_at = ?2, outcome = ?3 WHERE id = ?1 AND handled_at IS NULL",
        params![id, now(), outcome],
    )?;
    Ok(())
}

pub fn lock_generation(tx: &Transaction, task: &str) -> Result<i64> {
    Ok(tx.query_row(
        "SELECT lock_generation FROM tasks WHERE id = ?1",
        [task],
        |r| r.get(0),
    )?)
}
