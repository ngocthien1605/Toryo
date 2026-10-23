//! The SQL schema of the store (R23-32, moved out of `store.rs` as it was); its version is
//! `SCHEMA_VERSION`, raised as M13-05 says.

/// Plan §5.3, plus the columns the engine needs to rebuild a task after a crash
/// (`machine`, `repo`, `implementers`, `feedback`, `handoff`, …).
pub(super) const SCHEMA: &str = r#"
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
  undo_pending INTEGER NOT NULL DEFAULT 0,
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
CREATE TABLE IF NOT EXISTS environment_deployments (
  lock TEXT PRIMARY KEY, task TEXT NOT NULL, sha TEXT NOT NULL, gate TEXT NOT NULL,
  machine TEXT NOT NULL, started_at TEXT NOT NULL, finished_at TEXT, ok INTEGER
);
CREATE TABLE IF NOT EXISTS environment_waits (
  task TEXT PRIMARY KEY, lock TEXT NOT NULL, since TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS deploy_confirmations (
  task TEXT NOT NULL, gate TEXT NOT NULL, commit_sha TEXT NOT NULL, config_sha256 TEXT NOT NULL,
  mode TEXT NOT NULL, asked_at TEXT NOT NULL, confirmed_at TEXT,
  PRIMARY KEY (task, gate, commit_sha, config_sha256)
);
"#;
