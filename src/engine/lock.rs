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
    // M12-12 (c): `toryo run ../../repo/.git/index` made `index.lock` there
    super::task::valid_task_id(task)?;
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

/// M11-E3: the file of an environment gate's machine-wide lock,
/// `<data_dir>/locks/env/<lock>.lock` — a folder of its own, so no task id (`locks/<task>.lock`)
/// can name the same file (group E review). Lower case: Windows file names ignore case.
pub fn env_lock_path(lock: &str) -> PathBuf {
    data_dir()
        .join("locks")
        .join("env")
        .join(format!("{}.lock", lock.to_ascii_lowercase()))
}

/// An environment-gate lock name is part of a file name: letters, digits, `-`, `_`, `.`
/// (not only dots).
pub fn valid_lock_name(lock: &str) -> bool {
    lock_name_problem(lock).is_none()
}

/// Why `lock` cannot be an environment-gate lock name, or None. M11-R8: a Windows device name
/// (`con`, `nul`, `com1`, … — also before a dot) is not a file; names are case-insensitive
/// (`D365-Env` is `d365-env`: one lock file and one deployment row).
pub fn lock_name_problem(lock: &str) -> Option<&'static str> {
    let chars = !lock.is_empty()
        && lock.chars().any(|c| c != '.')
        && lock
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !chars {
        return Some("use letters, digits, `-`, `_` and `.` only (e.g. d365-env)");
    }
    let stem = lock
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let numbered = |p: &str| {
        stem.strip_prefix(p)
            .is_some_and(|n| n.len() == 1 && ('1'..='9').contains(&n.chars().next().unwrap()))
    };
    if ["con", "prn", "aux", "nul"].contains(&stem.as_str()) || numbered("com") || numbered("lpt") {
        return Some("is a Windows device name (con, prn, aux, nul, com1–com9, lpt1–lpt9) — choose another (e.g. d365-env)");
    }
    None
}

/// The stored form of a lock name (M11-R8): lower case, so `D365-Env` and `d365-env` are one
/// lock and one deployment row.
pub fn lock_key(lock: &str) -> String {
    lock.to_ascii_lowercase()
}

/// M11-E3: an environment gate's machine-wide lock — one gate per lock name runs at a time,
/// across all tasks (they share one environment). An exclusive OS file lock like
/// [`TaskLock`]: the OS releases it when the holder exits or crashes; the holder's label is
/// written inside (for a human; Windows does not let other processes read a locked file, so
/// `status` names the holder from `environment_deployments`). Held only while the gate runs.
pub struct EnvLock {
    file: File,
    pub name: String,
}

impl EnvLock {
    /// Take the lock if it is free (non-blocking); `None` while another process holds it.
    pub fn try_acquire(lock: &str, holder: &str) -> Result<Option<EnvLock>> {
        if !valid_lock_name(lock) {
            anyhow::bail!(
                "environment gate lock `{lock}`: use letters, digits, `-`, `_` and `.` only"
            );
        }
        let p = env_lock_path(lock);
        std::fs::create_dir_all(p.parent().expect("parent"))?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&p)
            .with_context(|| format!("opening {}", p.display()))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("locking {}", p.display()))
            }
        }
        use std::io::{Seek, Write};
        let _ = file.set_len(0);
        let _ = file.seek(std::io::SeekFrom::Start(0));
        let _ = writeln!(file, "{holder}");
        Ok(Some(EnvLock {
            file,
            name: lock.to_string(),
        }))
    }
}

impl Drop for EnvLock {
    fn drop(&mut self) {
        // explicitly, not only by closing the handle (review L9: Windows may release a
        // closed handle's lock asynchronously)
        let _ = self.file.unlock();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// M11-R8: Windows device names are refused (also before a dot, any case); names are one
    /// lock whatever their case.
    #[test]
    fn lock_names_refuse_device_names_and_ignore_case() {
        for bad in [
            "con",
            "NUL",
            "Aux",
            "prn.x",
            "com1",
            "COM9",
            "lpt3",
            "lpt1.lock",
        ] {
            let why = lock_name_problem(bad).unwrap_or_else(|| panic!("{bad}"));
            assert!(why.contains("Windows device name"), "{bad}: {why}");
        }
        for ok in [
            "d365-env", "console", "com10", "com0", "lpt", "nul-env", "x.con",
        ] {
            assert_eq!(lock_name_problem(ok), None, "{ok}");
        }
        assert_eq!(lock_key("D365-Env"), "d365-env");
        assert_eq!(env_lock_path("D365-Env"), env_lock_path("d365-env"));
    }

    /// Group E review: an environment lock never shares a file with a task lock (task ids
    /// allow `-`, so `env-<lock>` could be one), and its name is folded to lower case.
    #[test]
    fn environment_locks_have_their_own_folder() {
        assert_ne!(env_lock_path("d365-env"), lock_path("env-d365-env"));
        assert_ne!(
            env_lock_path("d365-env").parent(),
            lock_path("env-d365-env").parent()
        );
        assert_eq!(env_lock_path("D365-Env"), env_lock_path("d365-env"));
        assert!(env_lock_path("d365-env").ends_with("env/d365-env.lock"));
    }
}
