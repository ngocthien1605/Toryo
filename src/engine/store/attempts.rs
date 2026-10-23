//! The `stage_attempts` table (R23-32, moved as it was): an attempt begins, ends, and carries its
//! context, session and protected manifest.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    Running,
    Succeeded,
    Failed,
    Interrupted,
    Cancelled,
}

impl AttemptStatus {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Cancelled => "cancelled",
        }
    }
    pub(super) fn parse(s: &str) -> Self {
        match s {
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "interrupted" => Self::Interrupted,
            _ => Self::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AttemptRow {
    pub id: String,
    pub task_id: String,
    pub stage: String,
    pub attempt: u32,
    pub kind: String,
    pub fallback_from: Option<String>,
    pub profile: Option<String>,
    pub external_session_id: Option<String>,
    pub pid: Option<i64>,
    pub pid_created_at: Option<String>,
    pub status: AttemptStatus,
    pub error_class: Option<String>,
    pub error_detail: Option<String>,
    pub git_head_before: Option<String>,
    pub git_head_after: Option<String>,
    pub subject_commit: Option<String>,
    pub partial_commit: Option<String>,
    pub checkpoint_kind: Option<String>,
    pub partial_snapshot_id: Option<String>,
    pub correction_turns: u32,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub context_sha256: Option<String>,
    /// R23-36: a read-only role's write that could not be undone yet (X32) — the next writing
    /// attempt and recovery undo it first.
    pub undo_pending: bool,
}

pub(super) const ATTEMPT_COLS: &str = "id, task_id, stage, attempt, kind, fallback_from, profile, external_session_id, pid, pid_created_at, status, error_class, error_detail, git_head_before, git_head_after, subject_commit, partial_commit, checkpoint_kind, partial_snapshot_id, correction_turns, started_at, ended_at, context_sha256, undo_pending";

pub(super) fn attempt_from_row(r: &rusqlite::Row) -> rusqlite::Result<AttemptRow> {
    Ok(AttemptRow {
        id: r.get(0)?,
        task_id: r.get(1)?,
        stage: r.get(2)?,
        attempt: r.get::<_, i64>(3)? as u32,
        kind: r.get(4)?,
        fallback_from: r.get(5)?,
        profile: r.get(6)?,
        external_session_id: r.get(7)?,
        pid: r.get(8)?,
        pid_created_at: r.get(9)?,
        status: AttemptStatus::parse(&r.get::<_, String>(10)?),
        error_class: r.get(11)?,
        error_detail: r.get(12)?,
        git_head_before: r.get(13)?,
        git_head_after: r.get(14)?,
        subject_commit: r.get(15)?,
        partial_commit: r.get(16)?,
        checkpoint_kind: r.get(17)?,
        partial_snapshot_id: r.get(18)?,
        correction_turns: r.get::<_, i64>(19)? as u32,
        started_at: r.get(20)?,
        ended_at: r.get(21)?,
        context_sha256: r.get(22)?,
        undo_pending: r.get::<_, i64>(23)? != 0,
    })
}

pub struct NewAttempt<'a> {
    pub stage: &'a str,
    /// agent | command | checkpoint
    pub kind: &'a str,
    pub profile: Option<&'a str>,
    pub fallback_from: Option<&'a str>,
    pub head_before: Option<&'a str>,
    pub subject_commit: Option<&'a str>,
}

/// This process's identity for `stage_attempts.pid` / `pid_created_at` (§10 step 2):
/// PID + OS creation time, so a reused PID is never mistaken for it.
pub fn process_identity() -> (i64, String) {
    let p = crate::process::current_proc_id();
    (p.pid as i64, p.created.to_string())
}

/// Is the process recorded as (pid, created) still running?
pub fn identity_alive(pid: i64, created: &str) -> bool {
    match created.parse::<u64>() {
        Ok(c) => crate::process::is_alive(crate::process::ProcId {
            pid: pid as u32,
            created: c,
        }),
        Err(_) => false,
    }
}

pub fn begin_attempt(tx: &Transaction, task: &str, a: &NewAttempt) -> Result<AttemptRow> {
    let n: i64 = tx.query_row(
        "SELECT COALESCE(MAX(attempt), 0) + 1 FROM stage_attempts WHERE task_id = ?1 AND stage = ?2",
        params![task, a.stage],
        |r| r.get(0),
    )?;
    let id = format!("{task}/{}/{n}", a.stage);
    let (pid, created) = process_identity();
    tx.execute(
        "INSERT INTO stage_attempts (id, task_id, stage, attempt, kind, fallback_from, profile, pid, pid_created_at, status, git_head_before, subject_commit, started_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'running', ?10, ?11, ?12)",
        params![id, task, a.stage, n, a.kind, a.fallback_from, a.profile, pid, created, a.head_before, a.subject_commit, now()],
    )?;
    event(
        tx,
        Some(task),
        Some(&id),
        "attempt_started",
        &serde_json::json!({"stage": a.stage, "attempt": n, "profile": a.profile}),
    )?;
    get_attempt(tx, &id)
}

pub fn get_attempt(tx: &Transaction, id: &str) -> Result<AttemptRow> {
    tx.query_row(
        &format!("SELECT {ATTEMPT_COLS} FROM stage_attempts WHERE id = ?1"),
        [id],
        attempt_from_row,
    )
    .optional()?
    .ok_or_else(|| anyhow!("no stage attempt `{id}`"))
}

pub fn attempts(tx: &Transaction, task: &str) -> Result<Vec<AttemptRow>> {
    let mut st = tx.prepare(&format!(
        "SELECT {ATTEMPT_COLS} FROM stage_attempts WHERE task_id = ?1 ORDER BY started_at, rowid"
    ))?;
    let rows = st.query_map([task], attempt_from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The latest attempt of `stage`.
pub fn last_attempt(tx: &Transaction, task: &str, stage: &str) -> Result<Option<AttemptRow>> {
    Ok(tx
        .query_row(
            &format!("SELECT {ATTEMPT_COLS} FROM stage_attempts WHERE task_id = ?1 AND stage = ?2 ORDER BY attempt DESC LIMIT 1"),
            params![task, stage],
            attempt_from_row,
        )
        .optional()?)
}

#[derive(Debug, Clone, Default)]
pub struct AttemptEnd {
    pub error_class: Option<String>,
    pub error_detail: Option<String>,
    pub external_session_id: Option<String>,
    pub head_after: Option<String>,
    pub checkpoint_kind: Option<String>,
    pub partial_commit: Option<String>,
    pub partial_snapshot_id: Option<String>,
    pub correction_turns: Option<u32>,
    /// R23-36: set the attempt's `undo_pending` (once set it stays: a later end of the same
    /// attempt — the checkpoint's commit on the writing attempt — does not clear it).
    pub undo_pending: bool,
}

/// R23-36: the words of a reason whose undo is still pending (X32). Up to schema version 2 they
/// were the only record of it; the migration to version 3 sets `undo_pending` by them.
pub const UNDO_PENDING_WORDS: &str =
    "the next `toryo resume` undoes what the role committed and puts back what it wrote first";

pub fn finish_attempt(
    tx: &Transaction,
    id: &str,
    status: AttemptStatus,
    end: &AttemptEnd,
) -> Result<()> {
    tx.execute(
        "UPDATE stage_attempts SET status = ?2, error_class = COALESCE(?3, error_class), error_detail = COALESCE(?4, error_detail),
           external_session_id = COALESCE(?5, external_session_id), git_head_after = COALESCE(?6, git_head_after),
           checkpoint_kind = COALESCE(?7, checkpoint_kind), partial_commit = COALESCE(?8, partial_commit),
           partial_snapshot_id = COALESCE(?9, partial_snapshot_id), correction_turns = COALESCE(?10, correction_turns),
           ended_at = CASE WHEN ?2 = 'running' THEN NULL ELSE ?11 END,
           undo_pending = CASE WHEN ?12 THEN 1 ELSE undo_pending END
         WHERE id = ?1",
        params![
            id,
            status.as_str(),
            end.error_class,
            end.error_detail,
            end.external_session_id,
            end.head_after,
            end.checkpoint_kind,
            end.partial_commit,
            end.partial_snapshot_id,
            end.correction_turns.map(|c| c as i64),
            now(),
            end.undo_pending
        ],
    )?;
    let a = get_attempt(tx, id)?;
    event(
        tx,
        Some(&a.task_id),
        Some(id),
        "attempt_finished",
        &serde_json::json!({"status": status, "error": end.error_class, "checkpoint_kind": end.checkpoint_kind, "partial_commit": end.partial_commit}),
    )?;
    Ok(())
}

/// The SHA-256 of the context an attempt received (§7).
pub fn set_attempt_context(tx: &Transaction, id: &str, sha256: &str) -> Result<()> {
    tx.execute(
        "UPDATE stage_attempts SET context_sha256 = ?2 WHERE id = ?1",
        params![id, sha256],
    )?;
    Ok(())
}

/// Update the external session id as soon as it is known (so a crash can still resume it).
pub fn set_attempt_session(tx: &Transaction, id: &str, session: &str) -> Result<()> {
    tx.execute(
        "UPDATE stage_attempts SET external_session_id = ?2 WHERE id = ?1",
        params![id, session],
    )?;
    Ok(())
}

/// R35: the protected baseline in force when attempt `id` of `task` started. (R23-32: was raw
/// SQL in the runner.)
pub fn set_attempt_manifest(tx: &Transaction, id: &str, task: &str) -> Result<()> {
    tx.execute(
        "UPDATE stage_attempts SET protected_manifest_id = (SELECT artifact_id FROM artifact_heads WHERE task_id = ?2 AND logical_name = 'protected') WHERE id = ?1",
        params![id, task],
    )?;
    Ok(())
}
