//! Ctrl+C as a stop request (M13-12, X29; R23-28: moved out of `lock.rs` as it was): the
//! console handler, its test hook, and the stop row a press becomes.

use super::*;

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
pub(super) fn interrupt_waits(task: &str, generation: i64) -> bool {
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
pub(super) fn interrupt_to_stop(store: &Store, task: &str, generation: i64) {
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
