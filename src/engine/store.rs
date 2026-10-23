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

/// 2 (M13-05): `deploy_confirmations`. A database of version 1 is opened and raised (the table is
/// created, nothing else changes); one of a higher version is refused.
pub const SCHEMA_VERSION: i64 = 2;

mod schema;
use schema::SCHEMA;

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
    // R23-18: the change to WAL on a fresh file needs the database to itself, and SQLite does
    // not call the busy handler for it: several processes that open a new store at once (`toryo
    // run A B …` after an upgrade) are retried here, for as long as the busy timeout
    let started = std::time::Instant::now();
    loop {
        match conn.pragma_update(None, "journal_mode", "WAL") {
            Ok(()) => break,
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy
                    && started.elapsed() < Duration::from_secs(15) =>
            {
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e).context("setting journal_mode=WAL"),
        }
    }
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
            // R23-18: the write lock first — a deferred read-then-write transaction whose snapshot
            // goes stale gets SQLITE_BUSY, which the busy handler does not retry
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
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
                // M13-05: an older database is raised — every table of this version was just
                // created by `SCHEMA` (`IF NOT EXISTS`), nothing is altered or dropped
                Some(v) if v.parse::<i64>().is_ok_and(|n| (1..SCHEMA_VERSION).contains(&n)) => {
                    tx.execute(
                        "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
                        [SCHEMA_VERSION.to_string()],
                    )?;
                }
                Some(v) => bail!(
                    "database {} has schema version {v}; this toryo expects {SCHEMA_VERSION} (a newer toryo wrote it: use that one)",
                    path.display()
                ),
            }
            // additive migrations of the `tasks` table (columns added within schema version 1)
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

    /// Run `f` in a deferred, read-only transaction (R6): it never takes the write lock, so a
    /// poller (`watch`, `status`, `task show`, `usage`) never waits for — or delays — a writer.
    /// Nothing `f` writes is kept (the transaction is rolled back).
    pub fn read<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction) -> Result<T> + Send + 'static,
    {
        let (rtx, rrx) = mpsc::channel();
        let job: Job = Box::new(move |conn: &mut Connection| {
            let res = panic::catch_unwind(AssertUnwindSafe(|| {
                let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
                let v = f(&tx)?;
                tx.rollback()?;
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

mod deploy;
pub use deploy::*;
mod tasks;
pub use tasks::*;
mod attempts;
pub use attempts::*;
mod usage;
pub use usage::*;
mod control;
pub use control::*;
mod artifacts;
pub use artifacts::*;

// ── stage attempts ───────────────────────────────────────────────────────────────────

// ── usage (R41) ─────────────────────────────────────────────────────────────────────

// ── task lock bookkeeping + control requests (R38) ───────────────────────────────────

// ── artifacts (registration only; immutable versions are M6, R34) ────────────────────

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

    /// M13-05 AC-4: the schema version is raised (2: `deploy_confirmations`), a database of
    /// version 1 opens and is raised without losing a row, and one a newer toryo wrote is refused.
    #[test]
    fn an_older_database_opens_and_is_raised() {
        assert_eq!(SCHEMA_VERSION, 2);
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("toryo.db");
        // a version-1 database with a task in it: no `deploy_confirmations` table
        {
            let st = Store::open(&p).unwrap();
            st.call(|tx| insert_task(tx, &new_task("OLD-1"))).unwrap();
        }
        let version = |p: &Path| -> String {
            Connection::open(p)
                .unwrap()
                .query_row(
                    "SELECT value FROM meta WHERE key='schema_version'",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch(
                "DROP TABLE deploy_confirmations; UPDATE meta SET value='1' WHERE key='schema_version';",
            )
            .unwrap();
        }
        assert_eq!(version(&p), "1");
        let st = Store::open(&p).unwrap();
        assert_eq!(version(&p), "2");
        assert_eq!(
            st.call(|tx| get_task(tx, "OLD-1")).unwrap().id,
            "OLD-1",
            "nothing was lost"
        );
        // the new table works: asked, not confirmed; the yes; bound to the digest and the commit
        let asks = vec![DeployAsk {
            gate: "dbsync".into(),
            commit: "c1".into(),
            mode: "first".into(),
        }];
        st.call(move |tx| ask_deploys(tx, "OLD-1", "cfg", &asks))
            .unwrap();
        let confirmed = |digest: &'static str, commit: Option<&'static str>| {
            st.call(move |tx| deploy_confirmed(tx, "OLD-1", "dbsync", digest, commit))
                .unwrap()
        };
        assert!(!confirmed("cfg", None), "asked is not confirmed");
        let said = st.call(|tx| confirm_deploys(tx, "OLD-1")).unwrap();
        assert_eq!(said, [("dbsync".to_string(), "c1".to_string())]);
        assert!(confirmed("cfg", None) && confirmed("cfg", Some("c1")));
        assert!(!confirmed("cfg", Some("c2")), "`always`: another commit");
        assert!(!confirmed("other-digest", None), "another configuration");
        assert!(st
            .call(|tx| deploy_confirmed_under_another_config(tx, "OLD-1", "dbsync", "other-digest"))
            .unwrap());
        assert!(st
            .call(|tx| confirm_deploys(tx, "OLD-1"))
            .unwrap()
            .is_empty());
        drop(st);
        // a database a newer toryo wrote
        Connection::open(&p)
            .unwrap()
            .execute_batch("UPDATE meta SET value='3' WHERE key='schema_version';")
            .unwrap();
        let e = Store::open(&p).err().expect("refused").to_string();
        assert!(
            e.contains("schema version 3") && e.contains("expects 2"),
            "{e}"
        );
    }

    /// Review of M13-05: "asked again: the configuration changed since your last yes" is said
    /// only while the configuration in force has no yes of its own. Was: one yes under an
    /// earlier digest made every later stop of an `always` gate say it, for good.
    #[test]
    fn the_changed_configuration_note_ends_with_a_yes_under_the_new_one() {
        let d = tempfile::tempdir().unwrap();
        let st = Store::open(&d.path().join("toryo.db")).unwrap();
        st.call(|tx| insert_task(tx, &new_task("T-1"))).unwrap();
        let yes = |digest: &'static str, commit: &'static str| {
            st.call(move |tx| {
                let asks = [DeployAsk {
                    gate: "dbsync".into(),
                    commit: commit.into(),
                    mode: "always".into(),
                }];
                ask_deploys(tx, "T-1", digest, &asks)?;
                confirm_deploys(tx, "T-1").map(|_| ())
            })
            .unwrap()
        };
        let changed = |digest: &'static str| {
            st.call(move |tx| deploy_confirmed_under_another_config(tx, "T-1", "dbsync", digest))
                .unwrap()
        };
        yes("old", "c1");
        assert!(!changed("old"));
        assert!(changed("new"), "a yes under the old configuration only");
        yes("new", "c2");
        assert!(
            !changed("new"),
            "the last yes was under this configuration: the next commit is asked for, but not because the configuration changed"
        );
    }

    /// M13-11 (X28): a task `task create` could not finish is taken back — its row and its
    /// events — and the id is free again.
    #[test]
    fn a_task_that_was_never_finished_is_taken_back() {
        let d = tempfile::tempdir().unwrap();
        let st = Store::open(&d.path().join("toryo.db")).unwrap();
        st.call(|tx| {
            insert_task(tx, &new_task("N-1"))?;
            event(tx, Some("N-1"), None, "note", &serde_json::json!({})).map(|_| ())
        })
        .unwrap();
        st.call(|tx| insert_task(tx, &new_task("N-2"))).unwrap();
        st.call(|tx| remove_created_task(tx, "N-1")).unwrap();
        let left = st
            .call(|tx| {
                let events: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM events WHERE task_id = 'N-1'",
                    [],
                    |r| r.get(0),
                )?;
                Ok((
                    get_task(tx, "N-1").is_ok(),
                    get_task(tx, "N-2").is_ok(),
                    events,
                ))
            })
            .unwrap();
        assert_eq!(left, (false, true, 0), "only N-1 is gone");
        st.call(|tx| insert_task(tx, &new_task("N-1"))).unwrap();
    }

    /// M11-R16 AC-3: an escalation and its typed class are written in one transaction — both
    /// or neither; for another event the class is ignored.
    #[test]
    fn an_escalation_and_its_class_are_one_transaction() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("t.db")).unwrap();
        s.call(|tx| insert_task(tx, &new_task("T-1"))).unwrap();
        let m = s
            .call(|tx| transition_classed(tx, "T-1", E::Start, Some("skill_unavailable")))
            .unwrap();
        assert_eq!(m.reason_class(), None, "not an escalation");
        // the transaction fails after the escalation: neither the state nor the class is kept
        let r: Result<()> = s.call(|tx| {
            transition_classed(
                tx,
                "T-1",
                E::Escalate { reason: "x".into() },
                Some("environment_gate_unavailable"),
            )?;
            anyhow::bail!("the process died here")
        });
        assert!(r.is_err());
        let t = s.call(|tx| get_task(tx, "T-1")).unwrap();
        assert_eq!(
            (t.state, t.machine.reason_class()),
            (PipelineState::Planning, None)
        );
        let m = s
            .call(|tx| {
                transition_classed(
                    tx,
                    "T-1",
                    E::Escalate { reason: "x".into() },
                    Some("environment_gate_unavailable"),
                )
            })
            .unwrap();
        assert_eq!(m.reason_class(), Some("environment_gate_unavailable"));
        let t = s.call(|tx| get_task(tx, "T-1")).unwrap();
        assert_eq!(
            (t.state, t.machine.reason_class()),
            (
                PipelineState::Escalated,
                Some("environment_gate_unavailable")
            )
        );
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

    /// M10-R6: a read-only call does not wait for the write lock another connection holds (a
    /// `call`, `BEGIN IMMEDIATE`, would wait up to the 15 s busy timeout), and keeps nothing.
    #[test]
    fn a_read_does_not_wait_for_a_writer() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("t.db");
        let writer = Store::open(&p).unwrap();
        let reader = Store::open(&p).unwrap();
        writer.call(|tx| insert_task(tx, &new_task("T-R"))).unwrap();
        let (held_tx, held_rx) = mpsc::channel();
        let w = writer.clone();
        let h = std::thread::spawn(move || {
            w.call(move |tx| {
                tx.execute("UPDATE tasks SET title = 'x' WHERE id = 'T-R'", [])?;
                held_tx.send(()).unwrap();
                std::thread::sleep(Duration::from_secs(3));
                Ok(())
            })
            .unwrap()
        });
        held_rx.recv().unwrap();
        let t = std::time::Instant::now();
        let n: i64 = reader
            .read(|tx| Ok(tx.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(n, 1);
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        // a write inside a read is not kept
        reader
            .read(|tx| {
                let _ = tx.execute("DELETE FROM tasks", []);
                Ok(())
            })
            .ok();
        h.join().unwrap();
        let n: i64 = reader
            .read(|tx| Ok(tx.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(n, 1);
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
