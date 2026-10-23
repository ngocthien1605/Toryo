//! The waits for a machine-wide lock (R23-27): an environment gate's lock and the verify lock
//! (X36), and the guard that records their `lock_acquired` / `lock_released` (X39).

use super::*;

impl<'a> Engine<'a> {
    /// Take the machine-wide lock `lock` for task `t`, waiting while another task holds it:
    /// one `lock_waiting` event, `status` shows the wait, a `toryo stop` ends it (`None`).
    pub(super) fn env_lock(
        &self,
        t: &TaskRow,
        lock: &str,
        stop: &StopWatch,
    ) -> Result<Option<EnvLock>> {
        let label = format!("{} ({})", t.id, crate::engine::lock::owner_label());
        let me = t.id.clone();
        self.wait_for_lock(
            t,
            lock,
            || EnvLock::try_acquire(lock, &label),
            || {
                // R23-07: a lookup, never the write lock
                let (i, l) = (me.clone(), lock.to_string());
                self.store.read(move |tx| {
                    Ok(store::deployments(tx)?
                        .into_iter()
                        .find(|d| d.lock == l && d.finished_at.is_none() && d.task != i)
                        .map(|d| d.task))
                })
            },
            stop,
        )
    }

    /// M16-02 (X35, O-23): take the machine's verify lock for task `t`, waiting while another
    /// task verifies — like [`Self::env_lock`]: one `lock_waiting` event, `status` shows the
    /// wait, a `toryo stop` ends it (`None`).
    pub(super) fn verify_lock(&self, t: &TaskRow, stop: &StopWatch) -> Result<Option<VerifyLock>> {
        let label = format!("{} ({})", t.id, crate::engine::lock::owner_label());
        self.wait_for_lock(
            t,
            crate::engine::lock::VERIFY_LOCK,
            || VerifyLock::try_acquire(&label),
            || {
                // M16-R4: the holder names itself right after its `lock_acquired` is recorded;
                // a waiter that comes in between asks again for a moment (at most a second)
                for _ in 0..20 {
                    let i = t.id.clone();
                    if let Some(h) = self.store.read(move |tx| verifying_now(tx, &i))? {
                        return Ok(Some(h));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Ok(None)
            },
            stop,
        )
    }

    /// R23-27 (review 2026-10-23 A3): the one wait of [`Self::env_lock`] and
    /// [`Self::verify_lock`] — try to take the lock (`try_acquire`), and while another holds it:
    /// a wait row for `status`, one `lock_waiting` event naming `holder()` when known, a poll
    /// about every 2 s, and a stop request that ends the wait at once (`None`). The wait row of
    /// the task goes on every way out: when the lock is taken, on a stop, on an error (M16-R1),
    /// and one a run that died while it waited left behind is gone before this one waits
    /// (M16-R2: this process holds the task lock).
    fn wait_for_lock<L>(
        &self,
        t: &TaskRow,
        name: &str,
        try_acquire: impl Fn() -> Result<Option<L>>,
        holder: impl Fn() -> Result<Option<String>>,
        stop: &StopWatch,
    ) -> Result<Option<L>> {
        let done_waiting = || -> Result<()> {
            let i = t.id.clone();
            self.store.call(move |tx| store::set_env_wait(tx, &i, None))
        };
        done_waiting()?;
        let mut waiting = false;
        let res = (|| -> Result<Option<L>> {
            loop {
                if let Some(l) = try_acquire()? {
                    return Ok(Some(l));
                }
                if !waiting {
                    waiting = true;
                    let (i, l) = (t.id.clone(), name.to_string());
                    self.store
                        .call(move |tx| store::set_env_wait(tx, &i, Some(&l)))?;
                    self.rec.emit(EngineEvent::LockWaiting {
                        lock: name.to_string(),
                        holder: holder()?,
                    });
                }
                // poll about every 2 s; a stop request ends the wait at once
                for _ in 0..(env_lock_poll_ms() / 100).max(1) {
                    if stop.stopped() {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        })();
        if waiting {
            match &res {
                Ok(_) => done_waiting()?,
                Err(_) => {
                    let _ = done_waiting();
                }
            }
        }
        res
    }
}

/// M16-AT (X39): a held machine-wide lock (the verify lock, an environment lock) that records
/// `lock_acquired` when it is wrapped — right after the OS lock is held — and `lock_released`
/// when it is dropped, **before** the OS lock is let go: the next holder's `lock_acquired`
/// always comes after this holder's `lock_released`. Every way out of the stage drops it.
pub(super) struct Held<'r, L> {
    pub(super) rec: &'r Recorder,
    pub(super) name: String,
    pub(super) lock: Option<L>,
}

impl<'r, L> Held<'r, L> {
    pub(super) fn new(rec: &'r Recorder, name: &str, lock: L) -> Self {
        rec.emit_quiet(EngineEvent::LockAcquired { lock: name.into() });
        Self {
            rec,
            name: name.into(),
            lock: Some(lock),
        }
    }
}

impl<L> Drop for Held<'_, L> {
    fn drop(&mut self) {
        self.rec.emit_quiet(EngineEvent::LockReleased {
            lock: self.name.clone(),
        });
        drop(self.lock.take());
    }
}

/// M16-02: the task that holds the verify lock — the one the holder file names, when it is
/// not `me`, its recorded lock owner is alive and it is `Verifying`. For the `lock_waiting`
/// event and `status` only: the OS lock decides, this only names.
pub fn verifying_now(tx: &rusqlite::Transaction, me: &str) -> Result<Option<String>> {
    // M16-R2: the holder file names the task that took the lock (a task that released it for
    // its environment gates is no longer named); a file a crash left behind names a task that
    // is no longer driven or no longer verifying
    let Some(id) = crate::engine::lock::verify_holder().filter(|id| id != me) else {
        return Ok(None);
    };
    let Ok(t) = store::get_task(tx, &id) else {
        return Ok(None);
    };
    let alive = matches!(
        store::lock_owner(tx, &id)?,
        Some((_, _, Some(pid), Some(created))) if store::identity_alive(pid, &created)
    );
    Ok((alive && t.state == St::Verifying).then_some(id))
}

/// How often a waiting environment gate tries its lock again: about every 2 s
/// (`TORYO_TEST_ENV_LOCK_POLL_MS` for tests).
pub(super) fn env_lock_poll_ms() -> u64 {
    std::env::var("TORYO_TEST_ENV_LOCK_POLL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000)
}
