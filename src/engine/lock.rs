//! The per-task lock (R24, R38) and the `toryo stop` protocol.
//!
//! The lock is an exclusive OS file lock on `<data_dir>/locks/<task>.lock`. The OS releases
//! it when the owner exits or crashes, so there is no stale-lock logic: holding it proves
//! no other `toryo` process runs that task. Every mutating command takes it **first**,
//! before reading task state for a decision (lock-first rule).

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
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
            st.read(move |tx| store::lock_owner(tx, &t)).ok().flatten()
        };
        let mut tries = 0;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {
                    let o = owner_now(store);
                    let dead = matches!(&o, Some((_, _, Some(pid), Some(c))) if !store::identity_alive(*pid, c));
                    // R23-09: no owner recorded is the same window — an owner that ended cleanly
                    // cleared its columns first, and the OS lets its lock go a moment later
                    if (dead || o.is_none()) && tries < 15 {
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

    /// Is a stop addressed to this owner waiting? (Checked at every stage boundary.) A Ctrl+C
    /// in this process is such a stop (M13-12, X29).
    pub fn stop_requested(&self) -> Result<bool> {
        interrupt_to_stop(&self.store, &self.task, self.generation);
        let (t, g) = (self.task.clone(), self.generation);
        Ok(self
            .store
            .read(move |tx| store::pending_stop(tx, &t, g))?
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
                // M13-12 (X29): a Ctrl+C becomes the row `toryo stop` writes, found just below
                interrupt_to_stop(&st, &t, g);
                let (t2, g2) = (t.clone(), g);
                if let Ok(Some(_)) = st.read(move |tx| store::pending_stop(tx, &t2, g2)) {
                    f2.store(true, Ordering::SeqCst);
                    return;
                }
                for _ in 0..10 {
                    if d2.load(Ordering::SeqCst) {
                        return;
                    }
                    if interrupt_waits(&t, g) {
                        break; // not the rest of the second
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
        // the owner columns first, then the OS lock — explicitly, not only by closing the handle
        // (review L9, R23-09: Windows may release a closed handle's lock asynchronously)
        self.clear_owner();
        let _ = self._file.unlock();
    }
}

/// M13-12 (X29): how often Ctrl+C was pressed since [`catch_interrupt`].
static INTERRUPTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The owner (task, lock generation) the Ctrl+C was already turned into a stop request for.
static INTERRUPT_SENT: std::sync::Mutex<Option<(String, i64)>> = std::sync::Mutex::new(None);

/// What the first Ctrl+C prints.
const INTERRUPT_NOTE: &str = "\nstop requested — the running turn or gate is ended at the next boundary; press Ctrl+C again to end now\n";

/// M13-12 (X29): from now on the first Ctrl+C in this process is a stop request — the same as
/// `toryo stop <id>` for the task this process drives: the running turn or gate is ended, the
/// attempt is recorded, `run_ended` is written. The second Ctrl+C ends the process at once, as
/// Ctrl+C did before (recovery takes over at the next `resume`). No new crate: the console
/// handler on Windows, `SIGINT` elsewhere. The agents and gates do not see the key: they are
/// started without a console window, in their Job.
///
/// `TORYO_TEST_INTERRUPT_AFTER_MS` (tests only: `3000`, or `3000,3000` for two presses) makes
/// the process send itself the console event, each press that many milliseconds **after the
/// start of the process** — not after the previous press: `6000,6000` is two presses at once,
/// `6000,12000` is two presses six seconds apart — only for a process that has a console of
/// its own.
pub fn catch_interrupt() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(install_interrupt_handler);
}

/// The handler of [`catch_interrupt`] and its test hook.
fn install_interrupt_handler() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
        unsafe extern "system" fn handler(kind: u32) -> windows_sys::Win32::Foundation::BOOL {
            // CTRL_C_EVENT, CTRL_BREAK_EVENT; anything else (close, logoff) as usual
            if kind > 1 {
                return 0;
            }
            if INTERRUPTS.fetch_add(1, Ordering::SeqCst) == 0 {
                // (the system calls the handler on a thread of its own: printing is safe).
                // R23-15: never `eprint!` — it panics when stderr is closed (a `run A B` child
                // whose parent is gone), and a panic cannot leave an `extern "system"` frame:
                // the process would abort instead of stopping its task
                use std::io::Write;
                let _ = std::io::stderr().write_all(INTERRUPT_NOTE.as_bytes());
                return 1;
            }
            // the second time: not handled here — the default handler ends the process
            0
        }
        // SAFETY: registers a handler that counts in an atomic and prints one line
        unsafe {
            // M16-R3 (N-16): a process started with Ctrl+C ignored (`start /b`, a runner or a shell
            // that ignores it, CREATE_NEW_PROCESS_GROUP) inherits the ignoring and would never see
            // the key, however its handler is set: take Ctrl+C back for this process first
            SetConsoleCtrlHandler(None, 0);
            SetConsoleCtrlHandler(Some(handler), 1);
        }
    }
    #[cfg(unix)]
    {
        extern "C" fn handler(_: libc::c_int) {
            INTERRUPTS.fetch_add(1, Ordering::SeqCst);
            // SAFETY: write(2) and signal(2) are async-signal-safe; the next SIGINT gets the
            // default action (the process ends)
            unsafe {
                let _ = libc::write(
                    2,
                    INTERRUPT_NOTE.as_ptr() as *const libc::c_void,
                    INTERRUPT_NOTE.len(),
                );
                libc::signal(libc::SIGINT, libc::SIG_DFL);
            }
        }
        // SAFETY: the handler only does what is async-signal-safe (see there)
        unsafe {
            libc::signal(libc::SIGINT, handler as *const () as libc::sighandler_t);
        }
    }
    if let Ok(after) = std::env::var("TORYO_TEST_INTERRUPT_AFTER_MS") {
        let times: Vec<u64> = after
            .split(',')
            .filter_map(|t| t.trim().parse().ok())
            .collect();
        std::thread::spawn(move || {
            let started = Instant::now();
            for at in times {
                std::thread::sleep(Duration::from_millis(at).saturating_sub(started.elapsed()));
                press_ctrl_c();
            }
        });
    }
}

/// Send this process the event a Ctrl+C sends (the test hook of [`catch_interrupt`]).
fn press_ctrl_c() {
    #[cfg(windows)]
    // SAFETY: no pointers; group 0 is every process on this console
    unsafe {
        use windows_sys::Win32::System::Console::{
            GenerateConsoleCtrlEvent, SetConsoleCtrlHandler,
        };
        // a process started by one that ignores Ctrl+C inherits that (a test runner may):
        // here the key is pressed as on the owner's console
        SetConsoleCtrlHandler(None, 0);
        if GenerateConsoleCtrlEvent(0, 0) == 0 {
            crate::errln!(
                "TORYO_TEST_INTERRUPT_AFTER_MS: no Ctrl+C could be sent ({})",
                std::io::Error::last_os_error()
            );
        }
    }
    #[cfg(unix)]
    // SAFETY: no pointers; `raise` runs the handler on this thread before it returns
    unsafe {
        libc::raise(libc::SIGINT);
    }
}

/// Was Ctrl+C pressed (M13-12)?
pub fn interrupted() -> bool {
    INTERRUPTS.load(Ordering::SeqCst) > 0
}

/// A Ctrl+C that was not yet turned into a stop request for this owner.
fn interrupt_waits(task: &str, generation: i64) -> bool {
    interrupted()
        && INTERRUPT_SENT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_none_or(|(t, g)| t != task || *g != generation)
}

/// M13-12 (X29): turn a Ctrl+C into the stop request `toryo stop` writes for this owner —
/// once per owner; the engine then answers it like any stop. A `resume` without an id that
/// drives several tasks stops each of them at its first boundary.
fn interrupt_to_stop(store: &Store, task: &str, generation: i64) {
    if !interrupt_waits(task, generation) {
        return;
    }
    let (t, by) = (task.to_string(), format!("{} (Ctrl+C)", owner_label()));
    if store
        .call(move |tx| store::insert_stop(tx, &t, generation, &by))
        .is_ok()
    {
        *INTERRUPT_SENT.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((task.to_string(), generation));
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
    if is_device_name(lock) {
        return Some("is a Windows device name (con, prn, aux, nul, com0–com9, lpt0–lpt9) — choose another (e.g. d365-env)");
    }
    None
}

/// Whether `name` is a Windows device name — `con`, `prn`, `aux`, `nul`, `com0`–`com9`,
/// `lpt0`–`lpt9`, in any case, also before a dot (`nul.lock` is the device): no file or
/// folder can have it (M11-R8 for a lock name, M13-14 for a task id).
pub fn is_device_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let numbered = |p: &str| {
        stem.strip_prefix(p)
            .is_some_and(|n| n.len() == 1 && ('1'..='9').contains(&n.chars().next().unwrap()))
    };
    ["con", "prn", "aux", "nul", "com0", "lpt0"].contains(&stem.as_str())
        || numbered("com")
        || numbered("lpt")
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
        Ok(
            try_lock_at(&env_lock_path(lock), holder)?.map(|file| EnvLock {
                file,
                name: lock.to_string(),
            }),
        )
    }
}

/// The exclusive OS lock of the file `p` (created when missing) if it is free, with `holder`
/// written inside; `None` while another process holds it.
fn try_lock_at(p: &Path, holder: &str) -> Result<Option<File>> {
    std::fs::create_dir_all(p.parent().expect("parent"))?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(p)
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
    Ok(Some(file))
}

/// M16-02 (X36, O-23): the name the verify lock is shown under (`lock_waiting`, `status`, the
/// wait row). M16-R2: with a `:`, which no environment lock name may hold
/// ([`lock_name_problem`]) — an environment gate's `lock: verify` is another lock.
pub const VERIFY_LOCK: &str = "machine:verify";

/// M16-02: the file of the verify lock, `<data_dir>/locks/machine/verify.lock` — a folder of its
/// own, so neither a task id (`locks/<task>.lock`) nor an environment lock (`locks/env/…`) can
/// name the same file.
pub fn verify_lock_path() -> PathBuf {
    data_dir().join("locks").join("machine").join("verify.lock")
}

/// M16-02 (X35, O-23): one task at a time runs its check gates on this machine. Every task's
/// verify folders (the checkout, `TORYO_VERIFY_BUILD`, the caches, temp and home) are labelled
/// Low, so the code under test of one task — running at Low integrity — can write the folders
/// of another; while two tasks verify at the same time it could change what the other's gate
/// builds or reads, or what its after-run checks compare. Held from before the checkout is
/// reset to the checkpoint until the checks of the checkout after the check gates (and the
/// hashes of what they ran) are done; released before the environment gates, which read a
/// fresh export no Low code can write (X6) and queue on their own lock (M11-E3) — so a task
/// never holds both, and no two tasks can wait for each other. An OS file lock like [`EnvLock`].
pub struct VerifyLock {
    file: File,
}

/// M16-R2: `<data_dir>/locks/machine/verify.holder` — the id of the task that holds the verify
/// lock, for `status` and `lock_waiting` (Windows does not let another process read a locked
/// file). Written after the lock is taken, emptied before it is released; a crash leaves it
/// behind, so a reader also asks whether that task is driven and verifying.
pub fn verify_holder_path() -> PathBuf {
    data_dir()
        .join("locks")
        .join("machine")
        .join("verify.holder")
}

/// The task the holder file names, if any (see [`verify_holder_path`]).
pub fn verify_holder() -> Option<String> {
    std::fs::read_to_string(verify_holder_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

impl VerifyLock {
    /// Take the lock if it is free (non-blocking); `None` while another process holds it.
    /// `label` is written in the lock file. The holder file is not: see [`Self::announce`].
    pub fn try_acquire(label: &str) -> Result<Option<VerifyLock>> {
        Ok(try_lock_at(&verify_lock_path(), label)?.map(|file| VerifyLock { file }))
    }

    /// M16-R4: name `task` in the holder file — called once the holder's `lock_acquired` is in
    /// its flight recorder, so no waiter's `lock_waiting` can name a holder before that holder's
    /// own log says it took the lock (a waiter that finds no name yet asks again).
    pub fn announce(&self, task: &str) {
        let _ = std::fs::write(verify_holder_path(), task);
    }
}

impl Drop for VerifyLock {
    fn drop(&mut self) {
        // emptied while the lock is still held: the next holder writes its own id after it
        let _ = std::fs::write(verify_holder_path(), "");
        let _ = self.file.unlock();
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
    let mut generation = store.read(move |tx| store::lock_generation(tx, &t))?;
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
        let row = store.read(move |tx| store::control_request(tx, id))?;
        if let Some(o) = row.outcome {
            if o != "expired" {
                return Ok(StopOutcome::Stopped(o));
            }
        }
        let t = task.to_string();
        let g = store.read(move |tx| store::lock_generation(tx, &t))?;
        if g != generation {
            // a new owner took the task: re-address the stop once per generation
            generation = g;
            let (t, b) = (task.to_string(), by.clone());
            id = store.call(move |tx| store::insert_stop(tx, &t, g, &b))?;
            continue;
        }
        if probe_free(task)? {
            // the owner exited without handling it (finished or crashed)
            let row = store.read(move |tx| store::control_request(tx, id))?;
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
            // M13-R8: COM0 and LPT0 are reserved too
            "com0",
            "LPT0.x",
        ] {
            let why = lock_name_problem(bad).unwrap_or_else(|| panic!("{bad}"));
            assert!(why.contains("Windows device name"), "{bad}: {why}");
        }
        for ok in ["d365-env", "console", "com10", "lpt", "nul-env", "x.con"] {
            assert_eq!(lock_name_problem(ok), None, "{ok}");
        }
        assert_eq!(lock_key("D365-Env"), "d365-env");
        assert_eq!(env_lock_path("D365-Env"), env_lock_path("d365-env"));
    }

    /// M16-R2: the verify lock's name can never be an environment lock's — in the wait row, the
    /// `lock_waiting` event and `status` the two are always told apart; and its files are in a
    /// folder of their own.
    #[test]
    fn the_verify_lock_is_no_environment_lock() {
        assert!(lock_name_problem(VERIFY_LOCK).is_some(), "{VERIFY_LOCK}");
        assert_eq!(
            lock_name_problem("verify"),
            None,
            "an environment lock may be `verify`"
        );
        assert_ne!(verify_lock_path(), env_lock_path("verify"));
        assert_ne!(verify_lock_path(), lock_path("verify"));
        assert_eq!(verify_lock_path().parent(), verify_holder_path().parent());
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
