//! The store actor (plan §4, §5.3; R24): one thread owns the SQLite connection and runs
//! every command in **one transaction** — the event row and the state/projection update
//! commit together or not at all. Other threads (and the stop poller) talk to it through
//! [`Store::call`]. Other processes open their own store; SQLite WAL + `busy_timeout`
//! serialise writers across processes.

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::state::{GateKind, Machine, PipelineEvent, PipelineState};
use crate::config::data_dir;
use crate::domain::TokenUsage;
use crate::usage::{Accounting, Buckets, Weights};

pub const SCHEMA_VERSION: i64 = 1;

/// Plan §5.3, plus the columns the engine needs to rebuild a task after a crash
/// (`machine`, `repo`, `implementers`, `feedback`, `handoff`, …).
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS tasks (
  id TEXT PRIMARY KEY, title TEXT NOT NULL, state TEXT NOT NULL,
  task_sha256 TEXT NOT NULL,
  acceptance_ids TEXT NOT NULL,
  run_store TEXT NOT NULL,
  verify_checkout TEXT,
  config_sha256 TEXT NOT NULL,
  lock_generation INTEGER NOT NULL DEFAULT 0,
  paused_from_state TEXT,
  fix_cycle INTEGER NOT NULL DEFAULT 0,
  implementer_index INTEGER NOT NULL DEFAULT 0,
  worktree TEXT, branch TEXT, base_commit TEXT,
  last_reviewed_head TEXT,
  budget_tokens INTEGER, spent_tokens INTEGER NOT NULL DEFAULT 0,
  reserved_tokens INTEGER NOT NULL DEFAULT 0,
  budget_usd_est REAL, spent_usd_est REAL NOT NULL DEFAULT 0,
  run_lock_owner TEXT, run_lock_at TEXT,
  run_lock_pid INTEGER, run_lock_created TEXT,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  -- engine columns (M3)
  repo TEXT NOT NULL,
  task_file TEXT NOT NULL,
  machine TEXT NOT NULL,
  implementers TEXT NOT NULL,
  planner TEXT NOT NULL,
  reviewer TEXT NOT NULL,
  feedback TEXT,
  handoff TEXT
);
CREATE TABLE IF NOT EXISTS stage_attempts (
  id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id),
  stage TEXT NOT NULL, attempt INTEGER NOT NULL,
  kind TEXT NOT NULL,
  fallback_from TEXT,
  profile TEXT, external_session_id TEXT, pid INTEGER, pid_created_at TEXT,
  status TEXT NOT NULL,
  error_class TEXT, error_detail TEXT,
  git_head_before TEXT, git_head_after TEXT,
  subject_commit TEXT,
  partial_commit TEXT,
  checkpoint_kind TEXT,
  protected_manifest_id TEXT,
  partial_snapshot_id TEXT,
  context_sha256 TEXT,
  argv_sha256 TEXT,
  correction_turns INTEGER NOT NULL DEFAULT 0,
  started_at TEXT NOT NULL, ended_at TEXT,
  UNIQUE (task_id, stage, attempt)
);
CREATE TABLE IF NOT EXISTS approvals (
  id TEXT PRIMARY KEY, task_id TEXT NOT NULL, gate TEXT NOT NULL,
  occurrence INTEGER NOT NULL,
  status TEXT NOT NULL,
  reason TEXT, requested_at TEXT NOT NULL, resolved_at TEXT,
  subject_kind TEXT NOT NULL,
  subject_sha256 TEXT,
  subject_commit TEXT,
  verify_head TEXT,
  subject_digest TEXT NOT NULL,
  expected_prefix TEXT,
  approved_by TEXT,
  UNIQUE (task_id, gate, occurrence)
);
CREATE TABLE IF NOT EXISTS artifacts (
  id TEXT PRIMARY KEY, task_id TEXT NOT NULL, stage_attempt_id TEXT,
  logical_name TEXT NOT NULL,
  version INTEGER NOT NULL,
  kind TEXT NOT NULL, path TEXT NOT NULL,
  sha256 TEXT NOT NULL, size_bytes INTEGER NOT NULL, created_at TEXT NOT NULL,
  superseded_by TEXT,
  UNIQUE (task_id, logical_name, version), UNIQUE (task_id, path)
);
CREATE TABLE IF NOT EXISTS artifact_heads (
  task_id TEXT NOT NULL, logical_name TEXT NOT NULL, artifact_id TEXT NOT NULL REFERENCES artifacts(id),
  PRIMARY KEY (task_id, logical_name)
);
CREATE TABLE IF NOT EXISTS usage (
  id INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL, stage_attempt_id TEXT NOT NULL,
  profile TEXT NOT NULL, model TEXT,
  billing_kind TEXT NOT NULL,
  uncached_input_tokens INTEGER, cache_read_tokens INTEGER, cache_write_tokens INTEGER,
  output_tokens INTEGER,
  reasoning_tokens INTEGER,
  budget_tokens INTEGER,
  accounting TEXT NOT NULL,
  reservation_tokens INTEGER,
  reconciled_by INTEGER,
  includes_unreported INTEGER NOT NULL DEFAULT 0,
  est_cost_usd REAL, price_table_version TEXT,
  vendor_reported_cost_usd REAL,
  at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS usage_baselines (
  profile TEXT NOT NULL, external_session_id TEXT NOT NULL,
  counters TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (profile, external_session_id)
);
CREATE TABLE IF NOT EXISTS control_requests (
  id INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  target_generation INTEGER NOT NULL,
  requested_by TEXT NOT NULL, requested_at TEXT NOT NULL,
  handled_at TEXT, outcome TEXT
);
CREATE TABLE IF NOT EXISTS events (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT, stage_attempt_id TEXT,
  kind TEXT NOT NULL, payload TEXT NOT NULL, at TEXT NOT NULL
);
"#;

/// Reservation held for a turn with no telemetry (R41), until M7 derives it per stage.
pub const DEFAULT_UNKNOWN_RESERVATION: i64 = 200_000;

pub fn now() -> String {
    crate::evidence::now_rfc3339()
}

pub fn default_db_path() -> PathBuf {
    data_dir().join("toryo.db")
}

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// The text of a panic payload (`panic!` with a literal or a formatted message).
fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(no message)".into())
}

/// Handle to the store actor. Cloning shares the same actor thread.
#[derive(Clone)]
pub struct Store {
    tx: mpsc::Sender<Job>,
    path: PathBuf,
}

fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(15))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

impl Store {
    pub fn open_default() -> Result<Store> {
        Store::open(&default_db_path())
    }

    /// Open (and migrate) the database and start the actor thread.
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut conn =
            Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        configure(&conn)?;
        {
            let tx = conn.transaction()?;
            tx.execute_batch(SCHEMA)?;
            let v: Option<String> = tx
                .query_row(
                    "SELECT value FROM meta WHERE key='schema_version'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            match v {
                None => {
                    tx.execute(
                        "INSERT INTO meta(key, value) VALUES('schema_version', ?1)",
                        [SCHEMA_VERSION.to_string()],
                    )?;
                }
                Some(v) if v == SCHEMA_VERSION.to_string() => {}
                Some(v) => bail!(
                    "database {} has schema version {v}; this toryo expects {SCHEMA_VERSION}",
                    path.display()
                ),
            }
            // additive migrations within schema version 1
            let cols: Vec<String> = {
                let mut st = tx.prepare("PRAGMA table_info(tasks)")?;
                let rows = st.query_map([], |r| r.get::<_, String>(1))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            for (c, ty) in [("run_lock_pid", "INTEGER"), ("run_lock_created", "TEXT")] {
                if !cols.iter().any(|x| x == c) {
                    tx.execute_batch(&format!("ALTER TABLE tasks ADD COLUMN {c} {ty}"))?;
                }
            }
            tx.commit()?;
        }
        let (tx, rx) = mpsc::channel::<Job>();
        thread::Builder::new()
            .name("toryo-store".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    // CODE-09: a panicking job must not end the actor (`call` reports it to
                    // its caller); a transaction it left open is rolled back
                    let _ = panic::catch_unwind(AssertUnwindSafe(|| job(&mut conn)));
                    if !conn.is_autocommit() {
                        let _ = conn.execute_batch("ROLLBACK");
                    }
                }
            })?;
        Ok(Store {
            tx,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Run `f` in one transaction on the actor thread; commit if it returns `Ok`. A panic in
    /// `f` rolls the transaction back and is returned as an error (the actor keeps serving).
    pub fn call<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction) -> Result<T> + Send + 'static,
    {
        let (rtx, rrx) = mpsc::channel();
        let job: Job = Box::new(move |conn: &mut Connection| {
            let res = panic::catch_unwind(AssertUnwindSafe(|| {
                let tx =
                    conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let v = f(&tx)?;
                tx.commit()?;
                Ok(v)
            }))
            .unwrap_or_else(|p| Err(anyhow!("store job panicked: {}", panic_message(&*p))));
            let _ = rtx.send(res);
        });
        self.tx
            .send(job)
            .map_err(|_| anyhow!("store actor stopped"))?;
        rrx.recv().map_err(|_| anyhow!("store actor stopped"))?
    }
}

// ── rows ─────────────────────────────────────────────────────────────────────────────

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

const TASK_COLS: &str = "id, title, state, task_sha256, acceptance_ids, run_store, config_sha256, lock_generation, fix_cycle, implementer_index, worktree, branch, base_commit, last_reviewed_head, spent_tokens, reserved_tokens, run_lock_owner, run_lock_at, created_at, updated_at, repo, task_file, machine, implementers, planner, reviewer, feedback, handoff, budget_tokens";

fn state_from(s: &str) -> rusqlite::Result<PipelineState> {
    serde_json::from_value(Value::String(s.to_string())).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn json_from<T: for<'de> Deserialize<'de>>(s: &str) -> rusqlite::Result<T> {
    serde_json::from_str(s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn task_from_row(r: &rusqlite::Row) -> rusqlite::Result<TaskRow> {
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

fn state_str(s: PipelineState) -> String {
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

fn gate_name(k: GateKind) -> &'static str {
    match k {
        GateKind::Plan => "plan",
        GateKind::Final => "final",
    }
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
                params![task_id, gate_name(old.kind), old.occurrence as i64, status, reason, at, whoami()],
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
                    format!("{task_id}/{}/{}", gate_name(g.kind), g.occurrence),
                    task_id,
                    gate_name(g.kind),
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

// ── stage attempts ───────────────────────────────────────────────────────────────────

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
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Cancelled => "cancelled",
        }
    }
    fn parse(s: &str) -> Self {
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
}

const ATTEMPT_COLS: &str = "id, task_id, stage, attempt, kind, fallback_from, profile, external_session_id, pid, pid_created_at, status, error_class, error_detail, git_head_before, git_head_after, subject_commit, partial_commit, checkpoint_kind, partial_snapshot_id, correction_turns, started_at, ended_at, context_sha256";

fn attempt_from_row(r: &rusqlite::Row) -> rusqlite::Result<AttemptRow> {
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
}

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
           ended_at = CASE WHEN ?2 = 'running' THEN NULL ELSE ?11 END
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
            now()
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

// ── usage (R41, minimal until M7) ────────────────────────────────────────────────────

pub fn has_usage(tx: &Transaction, attempt: &str) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT COUNT(*) FROM usage WHERE stage_attempt_id = ?1",
        [attempt],
        |r| r.get::<_, i64>(0),
    )? > 0)
}

/// What one turn's usage row is recorded from (R41).
pub struct UsageIn<'a> {
    pub task: &'a str,
    pub attempt: &'a str,
    pub stage: &'a str,
    pub profile: &'a str,
    /// The model the turn ran with (C1): the role's, else the profile's; `None` when neither
    /// names one (the CLI's default).
    pub model: Option<&'a str>,
    pub billing: &'a str,
    /// The external session (Codex thread / Claude session) the counters belong to.
    pub session: Option<&'a str>,
    /// The adapter reports cumulative totals for a resumed session (Codex).
    pub cumulative: bool,
    /// Proven: a killed turn's tokens are inside the next cumulative total of its session.
    pub killed_included: bool,
    pub usage: Option<TokenUsage>,
    pub weights: Weights,
    /// `pipeline.unknown_turn_reservation_tokens`, if set.
    pub reservation: Option<u64>,
}

fn counters(b: &Buckets) -> Value {
    serde_json::json!({"uncached_input": b.uncached_input, "cache_read": b.cache_read, "cache_write": b.cache_write, "output": b.output, "reasoning": b.reasoning})
}

fn from_counters(v: &Value) -> Buckets {
    let g = |k: &str| v[k].as_u64().unwrap_or(0);
    Buckets {
        uncached_input: g("uncached_input"),
        cache_read: g("cache_read"),
        cache_write: g("cache_write"),
        output: g("output"),
        reasoning: v["reasoning"].as_u64(),
    }
}

/// Record one turn's usage (R41) and update the task's spent / reserved tokens. Returns the
/// row id.
/// - nothing known → `unknown` with a reservation (config, else the largest exact turn of the
///   same stage in this task, else 200 000) — never zero;
/// - `provisional` (a killed Claude turn's streamed counts) → charged as they are;
/// - `exact` on a cumulative adapter → the delta against the session's durable baseline
///   (a counter going down is a reset); killed turns of the same session not yet absorbed make
///   it `includes_unreported`, and when the adapter is proven to include them they are
///   `reconciled_by` this row and stop counting.
pub fn record_usage_in(tx: &Transaction, u: &UsageIn) -> Result<i64> {
    let acc = u.usage.and_then(|x| x.buckets.map(|b| (b, x.accounting)));
    let at = now();
    match acc {
        None | Some((_, Accounting::Unknown)) => {
            let res = match u.reservation {
                Some(r) => r as i64,
                None => tx
                    .query_row(
                        "SELECT MAX(u.budget_tokens) FROM usage u JOIN stage_attempts a ON a.id = u.stage_attempt_id WHERE u.task_id = ?1 AND a.stage = ?2 AND u.accounting = 'exact'",
                        params![u.task, u.stage],
                        |r| r.get::<_, Option<i64>>(0),
                    )?
                    .filter(|m| *m > 0)
                    .unwrap_or(DEFAULT_UNKNOWN_RESERVATION),
            };
            tx.execute(
                "INSERT INTO usage (task_id, stage_attempt_id, profile, model, billing_kind, accounting, reservation_tokens, at) VALUES (?1, ?2, ?3, ?4, ?5, 'unknown', ?6, ?7)",
                params![u.task, u.attempt, u.profile, u.model, u.billing, res, at],
            )?;
            tx.execute(
                "UPDATE tasks SET reserved_tokens = reserved_tokens + ?2 WHERE id = ?1",
                params![u.task, res],
            )?;
            Ok(tx.last_insert_rowid())
        }
        Some((b, acc)) => {
            let exact = acc == Accounting::Exact;
            let mut delta = b;
            let mut includes_unreported = false;
            let mut absorb: Vec<(i64, String, i64, i64)> = Vec::new();
            if let (true, true, Some(sess)) = (exact, u.cumulative, u.session) {
                let base: Option<String> = tx
                    .query_row("SELECT counters FROM usage_baselines WHERE profile = ?1 AND external_session_id = ?2", params![u.profile, sess], |r| r.get(0))
                    .optional()?;
                if let Some(base) = base {
                    let bb = from_counters(&serde_json::from_str(&base)?);
                    let reset = b.uncached_input < bb.uncached_input
                        || b.cache_read < bb.cache_read
                        || b.cache_write < bb.cache_write
                        || b.output < bb.output;
                    if !reset {
                        delta = Buckets {
                            uncached_input: b.uncached_input - bb.uncached_input,
                            cache_read: b.cache_read - bb.cache_read,
                            cache_write: b.cache_write - bb.cache_write,
                            output: b.output - bb.output,
                            reasoning: match (b.reasoning, bb.reasoning) {
                                (Some(x), Some(y)) if x >= y => Some(x - y),
                                (x, _) => x,
                            },
                        };
                    }
                }
                tx.execute(
                    "INSERT INTO usage_baselines (profile, external_session_id, counters, updated_at) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(profile, external_session_id) DO UPDATE SET counters = excluded.counters, updated_at = excluded.updated_at",
                    params![u.profile, sess, counters(&b).to_string(), at],
                )?;
                // killed turns of this session since its last exact total (none has absorbed them)
                let mut q = tx.prepare(
                    "SELECT u.id, u.accounting, COALESCE(u.reservation_tokens, 0), COALESCE(u.budget_tokens, 0) FROM usage u JOIN stage_attempts a ON a.id = u.stage_attempt_id
                     WHERE u.task_id = ?1 AND u.profile = ?2 AND a.external_session_id = ?3 AND u.accounting IN ('unknown', 'provisional') AND u.reconciled_by IS NULL AND u.stage_attempt_id != ?4
                       AND u.id > COALESCE((SELECT MAX(u2.id) FROM usage u2 JOIN stage_attempts a2 ON a2.id = u2.stage_attempt_id
                                            WHERE u2.task_id = ?1 AND u2.profile = ?2 AND a2.external_session_id = ?3 AND u2.accounting = 'exact'), 0)",
                )?;
                absorb = q
                    .query_map(params![u.task, u.profile, sess, u.attempt], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                includes_unreported = !absorb.is_empty();
            }
            let budget = delta.budget_tokens(&u.weights) as i64;
            tx.execute(
                "INSERT INTO usage (task_id, stage_attempt_id, profile, model, billing_kind, uncached_input_tokens, cache_read_tokens, cache_write_tokens, output_tokens, reasoning_tokens, budget_tokens, accounting, includes_unreported, at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    u.task,
                    u.attempt,
                    u.profile,
                    u.model,
                    u.billing,
                    delta.uncached_input as i64,
                    delta.cache_read as i64,
                    delta.cache_write as i64,
                    delta.output as i64,
                    delta.reasoning.map(|r| r as i64),
                    budget,
                    if exact { "exact" } else { "provisional" },
                    includes_unreported as i64,
                    at
                ],
            )?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "UPDATE tasks SET spent_tokens = spent_tokens + ?2 WHERE id = ?1",
                params![u.task, budget],
            )?;
            if u.killed_included {
                for (rid, acc, res, spent) in absorb {
                    tx.execute(
                        "UPDATE usage SET reconciled_by = ?2 WHERE id = ?1",
                        params![rid, id],
                    )?;
                    let (ds, dr) = if acc == "unknown" {
                        (0, res)
                    } else {
                        (spent, 0)
                    };
                    tx.execute("UPDATE tasks SET spent_tokens = spent_tokens - ?2, reserved_tokens = reserved_tokens - ?3 WHERE id = ?1", params![u.task, ds, dr])?;
                }
            }
            Ok(id)
        }
    }
}

/// Record a turn's usage with default settings (no session, weights 1.0).
pub fn record_usage(
    tx: &Transaction,
    task: &str,
    attempt: &str,
    profile: &str,
    billing: &str,
    u: Option<TokenUsage>,
) -> Result<()> {
    let stage: String = tx
        .query_row(
            "SELECT stage FROM stage_attempts WHERE id = ?1",
            [attempt],
            |r| r.get(0),
        )
        .unwrap_or_default();
    record_usage_in(
        tx,
        &UsageIn {
            task,
            attempt,
            stage: &stage,
            profile,
            model: None,
            billing,
            session: None,
            cumulative: false,
            killed_included: false,
            usage: u,
            weights: Weights::default(),
            reservation: None,
        },
    )?;
    Ok(())
}

/// `toryo budget set` (R41).
pub fn set_budget(tx: &Transaction, id: &str, tokens: Option<i64>) -> Result<()> {
    tx.execute(
        "UPDATE tasks SET budget_tokens = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, tokens, now()],
    )?;
    Ok(())
}

/// Usage rows of a task: (id, attempt, profile, accounting, budget_tokens, reservation, reconciled_by, includes_unreported).
#[allow(clippy::type_complexity)]
pub fn usage_rows(
    tx: &Transaction,
    task: &str,
) -> Result<
    Vec<(
        i64,
        String,
        String,
        String,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        bool,
    )>,
> {
    let mut q = tx.prepare("SELECT id, stage_attempt_id, profile, accounting, budget_tokens, reservation_tokens, reconciled_by, includes_unreported FROM usage WHERE task_id = ?1 ORDER BY id")?;
    let v = q
        .query_map([task], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get::<_, i64>(7)? != 0,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

/// The usage rows of a task for `toryo usage` (C1): with the attempt's stage, the recorded model
/// (NULL before M10: `None`), the four buckets and the vendor-reported cost.
pub fn usage_detail(tx: &Transaction, task: &str) -> Result<Vec<super::usage_report::UsageRow>> {
    let mut q = tx.prepare(
        "SELECT u.id, a.stage, u.profile, u.model, u.billing_kind, COALESCE(u.uncached_input_tokens, 0), COALESCE(u.cache_read_tokens, 0), COALESCE(u.cache_write_tokens, 0), COALESCE(u.output_tokens, 0), u.accounting, u.reservation_tokens, u.reconciled_by, u.vendor_reported_cost_usd
         FROM usage u JOIN stage_attempts a ON a.id = u.stage_attempt_id WHERE u.task_id = ?1 ORDER BY u.id",
    )?;
    let v = q
        .query_map([task], |r| {
            let n = |i: usize| r.get::<_, i64>(i).map(|x| x.max(0) as u64);
            Ok(super::usage_report::UsageRow {
                id: r.get(0)?,
                stage: r.get(1)?,
                profile: r.get(2)?,
                model: r.get(3)?,
                billing_kind: r.get(4)?,
                buckets: Buckets {
                    uncached_input: n(5)?,
                    cache_read: n(6)?,
                    cache_write: n(7)?,
                    output: n(8)?,
                    reasoning: None,
                },
                accounting: r.get(9)?,
                reservation_tokens: r.get(10)?,
                reconciled_by: r.get(11)?,
                vendor_reported_cost_usd: r.get(12)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

// ── task lock bookkeeping + control requests (R38) ───────────────────────────────────

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

// ── artifacts (registration only; immutable versions are M6, R34) ────────────────────

#[allow(clippy::too_many_arguments)]
pub fn register_artifact(
    tx: &Transaction,
    task: &str,
    attempt: Option<&str>,
    logical: &str,
    kind: &str,
    path: &str,
    sha256: &str,
    size: u64,
) -> Result<String> {
    let v: i64 = tx.query_row(
        "SELECT COALESCE(MAX(version), 0) + 1 FROM artifacts WHERE task_id = ?1 AND logical_name = ?2",
        params![task, logical],
        |r| r.get(0),
    )?;
    let id = format!("{task}/{logical}/{v}");
    tx.execute(
        "INSERT INTO artifacts (id, task_id, stage_attempt_id, logical_name, version, kind, path, sha256, size_bytes, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![id, task, attempt, logical, v, kind, path, sha256, size as i64, now()],
    )?;
    let prev: Option<String> = tx
        .query_row(
            "SELECT artifact_id FROM artifact_heads WHERE task_id = ?1 AND logical_name = ?2",
            params![task, logical],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(p) = prev {
        tx.execute(
            "UPDATE artifacts SET superseded_by = ?2 WHERE id = ?1",
            params![p, id],
        )?;
    }
    tx.execute(
        "INSERT INTO artifact_heads (task_id, logical_name, artifact_id) VALUES (?1, ?2, ?3)
         ON CONFLICT(task_id, logical_name) DO UPDATE SET artifact_id = excluded.artifact_id",
        params![task, logical, id],
    )?;
    Ok(id)
}

/// (id, path, sha256) of an artifact version.
pub fn artifact(tx: &Transaction, id: &str) -> Result<(String, String, String)> {
    Ok(tx.query_row(
        "SELECT id, path, sha256 FROM artifacts WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::state::PipelineEvent as E;

    fn new_task(id: &str) -> NewTask {
        NewTask {
            id: id.into(),
            title: "t".into(),
            task_sha256: "abc".into(),
            acceptance_ids: vec!["AC-1".into()],
            run_store: "/tmp/rs".into(),
            config_sha256: "cfg".into(),
            worktree: "/tmp/wt".into(),
            branch: "toryo/X".into(),
            base_commit: "b".into(),
            repo: "/tmp/repo".into(),
            task_file: "TASK.md".into(),
            machine: Machine::new(2),
            implementers: vec!["codex".into(), "claude".into()],
            planner: "claude".into(),
            reviewer: "claude".into(),
            budget_tokens: None,
        }
    }

    #[test]
    fn transitions_persist_state_events_and_gate_occurrences() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("t.db")).unwrap();
        s.call(|tx| insert_task(tx, &new_task("T-1"))).unwrap();
        assert!(
            s.call(|tx| insert_task(tx, &new_task("T-1"))).is_err(),
            "duplicate id"
        );
        s.call(|tx| transition(tx, "T-1", E::Start)).unwrap();
        s.call(|tx| {
            transition(
                tx,
                "T-1",
                E::PlanReady {
                    subject: "p1".into(),
                },
            )
        })
        .unwrap();
        s.call(|tx| {
            transition(
                tx,
                "T-1",
                E::PlanRejected {
                    occurrence: 1,
                    reason: "no".into(),
                },
            )
        })
        .unwrap();
        s.call(|tx| {
            transition(
                tx,
                "T-1",
                E::PlanReady {
                    subject: "p2".into(),
                },
            )
        })
        .unwrap();
        // a refused transition writes nothing (the whole transaction rolls back)
        assert!(s
            .call(|tx| transition(
                tx,
                "T-1",
                E::PlanApproved {
                    occurrence: 1,
                    subject: "p2".into()
                }
            ))
            .is_err());
        let t = s.call(|tx| get_task(tx, "T-1")).unwrap();
        assert_eq!(t.state, PipelineState::WaitingPlanApproval);
        let ap = s.call(|tx| approvals(tx, "T-1")).unwrap();
        assert_eq!(ap.len(), 2);
        assert_eq!(
            (
                ap[0].occurrence,
                ap[0].status.as_str(),
                ap[0].reason.as_deref()
            ),
            (1, "rejected", Some("no"))
        );
        assert_eq!((ap[1].occurrence, ap[1].status.as_str()), (2, "open"));
        s.call(|tx| {
            transition(
                tx,
                "T-1",
                E::PlanApproved {
                    occurrence: 2,
                    subject: "p2".into(),
                },
            )
        })
        .unwrap();
        let t = s.call(|tx| get_task(tx, "T-1")).unwrap();
        assert_eq!(t.state, PipelineState::Implementing);
        assert_eq!(t.machine.state(), PipelineState::Implementing);
        let ev = s.call(|tx| events(tx, "T-1")).unwrap();
        assert_eq!(ev.iter().filter(|e| e.1 == "transition").count(), 5);
        // a second store (another process) sees the same data
        let s2 = Store::open(&d.path().join("t.db")).unwrap();
        assert_eq!(
            s2.call(|tx| get_task(tx, "T-1")).unwrap().state,
            PipelineState::Implementing
        );
    }

    #[test]
    fn attempts_number_per_stage_and_lock_generation_expires_requests() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("t.db")).unwrap();
        s.call(|tx| insert_task(tx, &new_task("T-2"))).unwrap();
        let a1 = s
            .call(|tx| {
                begin_attempt(
                    tx,
                    "T-2",
                    &NewAttempt {
                        stage: "implement",
                        kind: "agent",
                        profile: Some("codex"),
                        fallback_from: None,
                        head_before: Some("h0"),
                        subject_commit: None,
                    },
                )
            })
            .unwrap();
        let a2 = s
            .call(|tx| {
                begin_attempt(
                    tx,
                    "T-2",
                    &NewAttempt {
                        stage: "implement",
                        kind: "agent",
                        profile: Some("claude"),
                        fallback_from: Some("codex"),
                        head_before: Some("h0"),
                        subject_commit: None,
                    },
                )
            })
            .unwrap();
        assert_eq!((a1.attempt, a2.attempt), (1, 2));
        assert_eq!(a1.id, "T-2/implement/1");
        let id = a1.id.clone();
        s.call(move |tx| {
            finish_attempt(
                tx,
                &id,
                AttemptStatus::Interrupted,
                &AttemptEnd {
                    partial_commit: Some("pc".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
        let back = s.call(|tx| get_attempt(tx, "T-2/implement/1")).unwrap();
        assert_eq!(
            (back.status, back.partial_commit.as_deref()),
            (AttemptStatus::Interrupted, Some("pc"))
        );
        assert_eq!(
            s.call(|tx| last_attempt(tx, "T-2", "implement"))
                .unwrap()
                .unwrap()
                .attempt,
            2
        );

        let g1 = s.call(|tx| claim_lock(tx, "T-2", "a")).unwrap();
        let r = s.call(move |tx| insert_stop(tx, "T-2", g1, "me")).unwrap();
        assert_eq!(
            s.call(move |tx| pending_stop(tx, "T-2", g1)).unwrap(),
            Some(r)
        );
        let g2 = s.call(|tx| claim_lock(tx, "T-2", "b")).unwrap();
        assert_eq!(g2, g1 + 1);
        let row = s.call(move |tx| control_request(tx, r)).unwrap();
        assert_eq!(
            row.outcome.as_deref(),
            Some("expired"),
            "a request never reaches a newer owner"
        );
        assert_eq!(s.call(move |tx| pending_stop(tx, "T-2", g2)).unwrap(), None);
    }

    /// CODE-09: a panic inside a job is the caller's error; its writes roll back and the actor
    /// keeps serving (before: every later call failed with "store actor stopped").
    #[test]
    fn a_panicking_job_is_an_error_and_the_actor_survives() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("t.db")).unwrap();
        let e = s
            .call(|tx| -> Result<()> {
                insert_task(tx, &new_task("T-P"))?;
                panic!("boom in a store job")
            })
            .unwrap_err();
        let msg = format!("{e:#}");
        assert!(
            msg.contains("store job panicked") && msg.contains("boom in a store job"),
            "{msg}"
        );
        assert!(
            s.call(|tx| get_task(tx, "T-P")).is_err(),
            "the panicking job's insert was rolled back"
        );
        s.call(|tx| insert_task(tx, &new_task("T-P"))).unwrap();
        assert_eq!(
            s.call(|tx| get_task(tx, "T-P")).unwrap().state,
            PipelineState::New
        );
    }

    #[test]
    fn usage_rows_charge_spent_or_reserve() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("t.db")).unwrap();
        s.call(|tx| insert_task(tx, &new_task("T-3"))).unwrap();
        let u = TokenUsage {
            buckets: Some(crate::usage::Buckets {
                uncached_input: 10,
                cache_read: 5,
                cache_write: 0,
                output: 2,
                reasoning: None,
            }),
            accounting: Accounting::Exact,
        };
        s.call(move |tx| record_usage(tx, "T-3", "a", "codex", "subscription", Some(u)))
            .unwrap();
        s.call(|tx| record_usage(tx, "T-3", "b", "codex", "subscription", None))
            .unwrap();
        let t = s.call(|tx| get_task(tx, "T-3")).unwrap();
        assert_eq!(
            (t.spent_tokens, t.reserved_tokens),
            (17, DEFAULT_UNKNOWN_RESERVATION)
        );
    }
}
