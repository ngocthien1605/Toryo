//! The machine-wide locks (R23-28: moved out of `lock.rs` as they were): an environment gate's
//! lock (M11-E3) and the verify lock (M16-02, X36), their files and names.

use super::*;

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
