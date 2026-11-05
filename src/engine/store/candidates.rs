//! M17-B2 (schema version 4): the best-of-N fan-out of a task's first Implementing stage (one
//! `fanouts` row) and its candidates (one `candidates` row each). The engine moves a fan-out
//! through its phases — `creating`, `turns`, `verify`, `select`, `selected`, then `promoted` —
//! or ends it as `single` (the single-implementer path, once). A candidate is recorded before its
//! worktree and branch are made (`created`), and keeps what its turn, its checkpoint and its
//! check gates gave: `state` (`pending` → `turn_done` → `finished`), `outcome`, `head`,
//! `verify_pass`, `proven`, `changed_lines`, the verify feedback, `winner`.

use super::*;

/// The fan-out of a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FanoutRow {
    pub task_id: String,
    /// The candidates admitted (2..=4).
    pub n: u32,
    pub phase: String,
    pub winner: Option<u32>,
    /// `only`, `compare` or `score`.
    pub by: Option<String>,
    /// The compare turn's reason, or the rung of the score that decided.
    pub reason: Option<String>,
    /// What the owner is told once (a budget that admitted fewer, the single path).
    pub note: Option<String>,
    pub started_at: String,
    pub updated_at: String,
}

/// One candidate of a fan-out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateRow {
    pub task_id: String,
    pub k: u32,
    pub profile: String,
    pub worktree: PathBuf,
    pub branch: String,
    /// Toryo made the worktree and the branch (recorded before, set after).
    pub created: bool,
    /// The worktree folder was removed (the branch is kept).
    pub removed: bool,
    /// `pending` (its turn has not finished), `turn_done` (the turn passed its checks; the
    /// checkpoint follows), `finished` (`outcome` is set).
    pub state: String,
    /// `checkpointed`, `no_change`, `failed_turn`, `disqualified:<class>`.
    pub outcome: Option<String>,
    pub head: Option<String>,
    /// The check gates passed (`None`: not verified).
    pub verify_pass: Option<bool>,
    /// Acceptance criteria proven red-green.
    pub proven: Option<u32>,
    /// Added + deleted lines, base..head (`git diff --numstat`).
    pub changed_lines: Option<i64>,
    /// The verify feedback of a failing candidate (a winner by score enters Fixing with it).
    pub feedback: Option<String>,
    pub winner: bool,
    pub detail: Option<String>,
}

const FANOUT_COLS: &str = "task_id, n, phase, winner, by, reason, note, started_at, updated_at";
const CANDIDATE_COLS: &str = "task_id, k, profile, worktree, branch, created, removed, state, outcome, head, verify_pass, proven, changed_lines, feedback, winner, detail";

fn fanout_from(r: &rusqlite::Row) -> rusqlite::Result<FanoutRow> {
    Ok(FanoutRow {
        task_id: r.get(0)?,
        n: r.get::<_, i64>(1)? as u32,
        phase: r.get(2)?,
        winner: r.get::<_, Option<i64>>(3)?.map(|k| k as u32),
        by: r.get(4)?,
        reason: r.get(5)?,
        note: r.get(6)?,
        started_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

fn candidate_from(r: &rusqlite::Row) -> rusqlite::Result<CandidateRow> {
    Ok(CandidateRow {
        task_id: r.get(0)?,
        k: r.get::<_, i64>(1)? as u32,
        profile: r.get(2)?,
        worktree: PathBuf::from(r.get::<_, String>(3)?),
        branch: r.get(4)?,
        created: r.get::<_, i64>(5)? != 0,
        removed: r.get::<_, i64>(6)? != 0,
        state: r.get(7)?,
        outcome: r.get(8)?,
        head: r.get(9)?,
        verify_pass: r.get::<_, Option<i64>>(10)?.map(|v| v != 0),
        proven: r.get::<_, Option<i64>>(11)?.map(|v| v as u32),
        changed_lines: r.get(12)?,
        feedback: r.get(13)?,
        winner: r.get::<_, i64>(14)? != 0,
        detail: r.get(15)?,
    })
}

/// The fan-out of `task`, if one was started.
pub fn fanout(tx: &Transaction, task: &str) -> Result<Option<FanoutRow>> {
    Ok(tx
        .query_row(
            &format!("SELECT {FANOUT_COLS} FROM fanouts WHERE task_id = ?1"),
            [task],
            fanout_from,
        )
        .optional()?)
}

pub fn insert_fanout(
    tx: &Transaction,
    task: &str,
    n: u32,
    phase: &str,
    note: Option<&str>,
) -> Result<()> {
    let at = now();
    tx.execute(
        "INSERT INTO fanouts (task_id, n, phase, note, started_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        params![task, n as i64, phase, note, at],
    )?;
    event(
        tx,
        Some(task),
        None,
        "fanout",
        &serde_json::json!({"phase": phase, "n": n, "note": note}),
    )
}

pub fn set_fanout_phase(tx: &Transaction, task: &str, phase: &str) -> Result<()> {
    tx.execute(
        "UPDATE fanouts SET phase = ?2, updated_at = ?3 WHERE task_id = ?1",
        params![task, phase, now()],
    )?;
    event(
        tx,
        Some(task),
        None,
        "fanout",
        &serde_json::json!({"phase": phase}),
    )
}

/// Take a fan-out back that never got past the creation of its worktrees (X28 style).
pub fn delete_fanout(tx: &Transaction, task: &str) -> Result<()> {
    tx.execute("DELETE FROM candidates WHERE task_id = ?1", [task])?;
    tx.execute("DELETE FROM fanouts WHERE task_id = ?1", [task])?;
    event(
        tx,
        Some(task),
        None,
        "fanout",
        &serde_json::json!({"phase": "taken back"}),
    )
}

/// Record the winner (`by`: `only`, `compare`, `score`) and move the fan-out to `selected`.
pub fn set_fanout_winner(
    tx: &Transaction,
    task: &str,
    k: u32,
    by: &str,
    reason: Option<&str>,
) -> Result<()> {
    tx.execute(
        "UPDATE fanouts SET phase = 'selected', winner = ?2, by = ?3, reason = ?4, updated_at = ?5 WHERE task_id = ?1",
        params![task, k as i64, by, reason, now()],
    )?;
    tx.execute(
        "UPDATE candidates SET winner = (k = ?2) WHERE task_id = ?1",
        params![task, k as i64],
    )?;
    event(
        tx,
        Some(task),
        None,
        "fanout",
        &serde_json::json!({"phase": "selected", "winner": k, "by": by, "reason": reason}),
    )
}

pub fn set_fanout_note(tx: &Transaction, task: &str, note: &str) -> Result<()> {
    tx.execute(
        "UPDATE fanouts SET note = ?2, updated_at = ?3 WHERE task_id = ?1",
        params![task, note, now()],
    )?;
    Ok(())
}

/// The candidates of `task`, by `k`.
pub fn candidates(tx: &Transaction, task: &str) -> Result<Vec<CandidateRow>> {
    let mut st = tx.prepare(&format!(
        "SELECT {CANDIDATE_COLS} FROM candidates WHERE task_id = ?1 ORDER BY k"
    ))?;
    let rows = st.query_map([task], candidate_from)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Record candidate `k` — before its worktree and branch exist (AC-14).
pub fn insert_candidate(
    tx: &Transaction,
    task: &str,
    k: u32,
    profile: &str,
    worktree: &Path,
    branch: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO candidates (task_id, k, profile, worktree, branch) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![task, k as i64, profile, worktree.display().to_string(), branch],
    )?;
    Ok(())
}

pub fn set_candidate_created(tx: &Transaction, task: &str, k: u32, created: bool) -> Result<()> {
    tx.execute(
        "UPDATE candidates SET created = ?3 WHERE task_id = ?1 AND k = ?2",
        params![task, k as i64, created],
    )?;
    Ok(())
}

pub fn set_candidate_removed(tx: &Transaction, task: &str, k: u32) -> Result<()> {
    tx.execute(
        "UPDATE candidates SET removed = 1 WHERE task_id = ?1 AND k = ?2",
        params![task, k as i64],
    )?;
    Ok(())
}

pub fn set_candidate_state(tx: &Transaction, task: &str, k: u32, state: &str) -> Result<()> {
    tx.execute(
        "UPDATE candidates SET state = ?3 WHERE task_id = ?1 AND k = ?2",
        params![task, k as i64, state],
    )?;
    Ok(())
}

/// The candidate's turn is over: `outcome`, its checkpoint `head` (when it made one) and a word
/// on why.
pub fn set_candidate_outcome(
    tx: &Transaction,
    task: &str,
    k: u32,
    outcome: &str,
    head: Option<&str>,
    detail: Option<&str>,
) -> Result<()> {
    tx.execute(
        "UPDATE candidates SET state = 'finished', outcome = ?3, head = COALESCE(?4, head), detail = ?5 WHERE task_id = ?1 AND k = ?2",
        params![task, k as i64, outcome, head, detail],
    )?;
    event(
        tx,
        Some(task),
        None,
        "candidate",
        &serde_json::json!({"k": k, "outcome": outcome, "head": head, "detail": detail}),
    )
}

/// What the candidate's check gates gave.
pub fn set_candidate_verify(
    tx: &Transaction,
    task: &str,
    k: u32,
    pass: bool,
    proven: Option<u32>,
    changed_lines: i64,
    feedback: Option<&str>,
) -> Result<()> {
    tx.execute(
        "UPDATE candidates SET verify_pass = ?3, proven = ?4, changed_lines = ?5, feedback = ?6 WHERE task_id = ?1 AND k = ?2",
        params![task, k as i64, pass, proven.map(i64::from), changed_lines, feedback],
    )?;
    Ok(())
}

/// M17-B9: the implementer chain after a fan-out — the winner first (fix cycles), then the
/// configured chain.
pub fn set_implementers(tx: &Transaction, task: &str, chain: &[String], idx: usize) -> Result<()> {
    tx.execute(
        "UPDATE tasks SET implementers = ?2, implementer_index = ?3, updated_at = ?4 WHERE id = ?1",
        params![task, serde_json::to_string(chain)?, idx as i64, now()],
    )?;
    Ok(())
}

/// M17-B11: the tokens of candidate `k` of `task` — every usage row of its attempts (stage
/// `implement#c<k>`): the counted tokens, else the reservation of a turn with no telemetry.
pub fn candidate_tokens(tx: &Transaction, task: &str, k: u32) -> Result<i64> {
    Ok(tx.query_row(
        "SELECT COALESCE(SUM(COALESCE(u.budget_tokens, u.reservation_tokens, 0)), 0) FROM usage u JOIN stage_attempts a ON a.id = u.stage_attempt_id WHERE u.task_id = ?1 AND a.stage = ?2 AND u.reconciled_by IS NULL",
        params![task, format!("implement#c{k}")],
        |r| r.get(0),
    )?)
}
