//! The per-task lock (R24, R38) and the `toryo stop` protocol.
//!
//! The lock is an exclusive OS file lock on `<data_dir>/locks/<task>.lock`. The OS releases
//! it when the owner exits or crashes, so there is no stale-lock logic: holding it proves
//! no other `toryo` process runs that task. Every mutating command takes it **first**,
//! before reading task state for a decision (lock-first rule).

use std::fs::{File, OpenOptions, TryLockError};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::store::{self, Store};
use crate::config::data_dir;

/// Exit code of a command that found the task lock held (plan §11 `task_active`).
pub const EXIT_TASK_ACTIVE: i32 = 3;

/// The lock is held by another process.
#[derive(Debug, thiserror::Error)]
#[error("task {task} is active (owner {owner}, since {since}) — nothing was changed")]
pub struct TaskActive {
    pub task: String,
    pub owner: String,
    pub since: String,
}

pub fn lock_path(task: &str) -> PathBuf {
    data_dir().join("locks").join(format!("{task}.lock"))
}

fn open_lock_file(task: &str) -> Result<File> {
    let p = lock_path(task);
    std::fs::create_dir_all(p.parent().expect("parent"))?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&p)
        .with_context(|| format!("opening {}", p.display()))
}

/// This process as a lock owner (`tasks.run_lock_owner`).
pub fn owner_label() -> String {
    format!("{} pid {}", store::whoami(), std::process::id())
}

/// A held task lock. Dropping it clears the owner columns (like [`TaskLock::release`], CODE-09:
/// an early `?` return never leaves them set) and releases the OS lock.
pub struct TaskLock {
    _file: File,
    pub task: String,
    pub generation: i64,
    store: Store,
    released: bool,
}

impl TaskLock {
    /// Take the lock (non-blocking), then — in one transaction — increment the lock
    /// generation, record the owner and expire requests addressed to older owners.
    pub fn acquire(store: &Store, task: &str) -> Result<TaskLock> {
        let file = open_lock_file(task)?;
        // Windows may release the lock of a killed owner a moment after the process is gone
        // ("depends upon available system resources"): while the recorded owner is dead,
        // retry briefly; a live owner is refused at once.
        let t = task.to_string();
        let owner_now = move |st: &Store| {
            let t = t.clone();
            st.call(move |tx| store::lock_owner(tx, &t)).ok().flatten()
        };
        let mut tries = 0;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {
                    let o = owner_now(store);
                    let dead = matches!(&o, Some((_, _, Some(pid), Some(c))) if !store::identity_alive(*pid, c));
                    if dead && tries < 15 {
                        tries += 1;
                        std::thread::sleep(Duration::from_millis(200));
                        continue;
                    }
                    let (owner, since) = o
                        .map(|(l, s, _, _)| (l, s))
                        .unwrap_or_else(|| ("?".into(), "?".into()));
                    return Err(TaskActive {
                        task: task.to_string(),
                        owner,
                        since,
                    }
                    .into());
                }
                Err(TryLockError::Error(e)) => return Err(e).context("locking the task"),
            }
        }
        let owner = owner_label();
        let t = task.to_string();
        let generation = store.call(move |tx| store::claim_lock(tx, &t, &owner))?;
        Ok(TaskLock {
            _file: file,
            task: task.to_string(),
            generation,
            store: store.clone(),
            released: false,
        })
    }

    /// Is a stop addressed to this owner waiting? (Checked at every stage boundary.)
    pub fn stop_requested(&self) -> Result<bool> {
        let (t, g) = (self.task.clone(), self.generation);
        Ok(self
            .store
            .call(move |tx| store::pending_stop(tx, &t, g))?
            .is_some())
    }

    /// Mark the pending stop(s) handled.
    pub fn handle_stop(&self, outcome: &str) -> Result<()> {
        let (t, g, o) = (self.task.clone(), self.generation, outcome.to_string());
        self.store
            .call(move |tx| store::handle_stops(tx, &t, g, &o).map(|_| ()))
    }

    /// Poll for a stop every second while a stage runs; sets the returned flag (which the
    /// stage passes to its turn / command as the cancel flag).
    pub fn watch_stop(&self) -> StopWatch {
        let flag = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let (f2, d2, st, t, g) = (
            flag.clone(),
            done.clone(),
            self.store.clone(),
            self.task.clone(),
            self.generation,
        );
        let handle = std::thread::spawn(move || {
            while !d2.load(Ordering::SeqCst) {
                let (t2, g2) = (t.clone(), g);
                if let Ok(Some(_)) = st.call(move |tx| store::pending_stop(tx, &t2, g2)) {
                    f2.store(true, Ordering::SeqCst);
                    return;
                }
                for _ in 0..10 {
                    if d2.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
        StopWatch {
            flag,
            done,
            handle: Some(handle),
        }
    }

    /// Clear the owner columns; the OS lock goes when `self` drops.
    pub fn release(mut self) {
        self.clear_owner();
    }

    /// Clear the owner columns of this generation, once.
    fn clear_owner(&mut self) {
        if std::mem::replace(&mut self.released, true) {
            return;
        }
        let (t, g) = (self.task.clone(), self.generation);
        let _ = self.store.call(move |tx| store::release_lock(tx, &t, g));
    }
}

impl Drop for TaskLock {
    fn drop(&mut self) {
        // the owner columns first: the OS lock (`_file`) is released after this, with the fields
        self.clear_owner();
    }
}

/// Background stop poller for one stage.
pub struct StopWatch {
    pub flag: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StopWatch {
    pub fn stopped(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

impl Drop for StopWatch {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Is the task lock free right now? A probe: taken and released at once, never a mutation.
pub fn probe_free(task: &str) -> Result<bool> {
    let f = open_lock_file(task)?;
    match f.try_lock() {
        Ok(()) => {
            let _ = f.unlock();
            Ok(true)
        }
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopOutcome {
    /// The owner stopped the running stage (or stopped at a stage boundary).
    Stopped(String),
    /// Nothing was running.
    NoOwner,
    /// The owner did not answer within the wait.
    Pending,
}

/// `toryo stop` (R38): persist a stop request for the current owner and wait for it.
pub fn stop(store: &Store, task: &str, wait: Duration) -> Result<StopOutcome> {
    let by = owner_label();
    let t = task.to_string();
    let mut generation = store.call(move |tx| store::lock_generation(tx, &t))?;
    if probe_free(task)? {
        let (t, b) = (task.to_string(), by.clone());
        let id = store.call(move |tx| store::insert_stop(tx, &t, generation, &b))?;
        store.call(move |tx| store::set_control_outcome(tx, id, "no_owner"))?;
        return Ok(StopOutcome::NoOwner);
    }
    let (t, b) = (task.to_string(), by.clone());
    let mut id = store.call(move |tx| store::insert_stop(tx, &t, generation, &b))?;
    let started = Instant::now();
    while started.elapsed() < wait {
        std::thread::sleep(Duration::from_millis(250));
        let row = store.call(move |tx| store::control_request(tx, id))?;
        if let Some(o) = row.outcome {
            if o != "expired" {
                return Ok(StopOutcome::Stopped(o));
            }
        }
        let t = task.to_string();
        let g = store.call(move |tx| store::lock_generation(tx, &t))?;
        if g != generation {
            // a new owner took the task: re-address the stop once per generation
            generation = g;
            let (t, b) = (task.to_string(), by.clone());
            id = store.call(move |tx| store::insert_stop(tx, &t, g, &b))?;
            continue;
        }
        if probe_free(task)? {
            // the owner exited without handling it (finished or crashed)
            let row = store.call(move |tx| store::control_request(tx, id))?;
            if let Some(o) = row.outcome {
                if o != "expired" {
                    return Ok(StopOutcome::Stopped(o));
                }
            }
            store.call(move |tx| store::set_control_outcome(tx, id, "no_owner"))?;
            return Ok(StopOutcome::NoOwner);
        }
    }
    Ok(StopOutcome::Pending)
}
