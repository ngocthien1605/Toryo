//! Process supervision (plan §13, R23 per-stage Job Object, R36 verify boundary, R42 redaction).
//!
//! One [`Running`] = one stage process tree. On Windows every child is created
//! suspended, assigned to its own `KILL_ON_JOB_CLOSE` Job Object and only then
//! resumed, so nothing ever runs outside a Job. On Unix (used for the tests and
//! for running the spike on Linux) a process group stands in for the Job: stop
//! works the same way, crash-cleanup does not (documented, Windows-only).

pub mod env;
pub mod redact;

#[cfg(unix)]
pub mod unix;
#[cfg(windows)]
pub mod windows;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use redact::{Redactions, StreamRedactor};

/// Execution boundary for the spawned tree (R36).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// Same token as toryo (agents: their own sandbox is the boundary).
    None,
    /// Windows Low Integrity Level copy of the user token.
    LowIntegrity,
}

impl Boundary {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "none" => Boundary::None,
            "low_integrity" => Boundary::LowIntegrity,
            "separate_user" => bail!("boundary `separate_user` is not part of the M0 spike"),
            other => bail!("unknown boundary `{other}`"),
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Boundary::None => "none",
            Boundary::LowIntegrity => "low_integrity",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SpawnSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Complete child environment (already built by [`env::build_env`]).
    pub env: BTreeMap<String, String>,
    /// Written to stdin, then stdin is closed. `None` = empty stdin.
    pub stdin: Option<Vec<u8>>,
    pub boundary: Boundary,
}

/// (pid, creation time) — identifies a process across PID reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProcId {
    pub pid: u32,
    pub created: u64,
}

pub enum Chunk {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub timed_out: bool,
    pub stalled: bool,
    pub cancelled: bool,
    pub duration_ms: u128,
    /// ActiveProcesses reached 0 after termination (or the tree exited on its own).
    pub tree_gone: bool,
    /// The root exited but descendants were still in the Job (holding the pipes or not):
    /// they were terminated with the Job at the end of the stage.
    pub lingering_killed: bool,
}

#[cfg(unix)]
pub use unix::Job;
/// Platform job handle.
#[cfg(windows)]
pub use windows::Job;

pub struct Running {
    pub job: Job,
    pub root: ProcId,
    child: platform::Child,
    rx: mpsc::Receiver<Option<(bool, Vec<u8>)>>,
    open_streams: usize,
    started: Instant,
}

#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

/// Spawn `spec` inside a fresh Job. Output of both pipes is redacted before it
/// leaves the reader thread (R42).
pub fn spawn(spec: &SpawnSpec, redactions: &Redactions) -> Result<Running> {
    let started = Instant::now();
    let spawned = platform::spawn_in_job(spec)?;
    let (tx, rx) = mpsc::channel();
    let mut open = 0;
    for (is_err, reader) in [(false, spawned.stdout), (true, spawned.stderr)] {
        if let Some(mut r) = reader {
            open += 1;
            let tx = tx.clone();
            let red = redactions.clone();
            std::thread::spawn(move || {
                let mut s = StreamRedactor::new(red);
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    match r.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let out = s.push(&buf[..n]);
                            if !out.is_empty() && tx.send(Some((is_err, out))).is_err() {
                                return;
                            }
                        }
                    }
                }
                let tail = s.finish();
                if !tail.is_empty() {
                    let _ = tx.send(Some((is_err, tail)));
                }
                let _ = tx.send(None);
            });
        }
    }
    if let Some(mut w) = spawned.stdin {
        let data = spec.stdin.clone().unwrap_or_default();
        std::thread::spawn(move || {
            let _ = w.write_all(&data);
            let _ = w.flush();
            drop(w);
        });
    }
    Ok(Running {
        job: spawned.job,
        root: spawned.root,
        child: spawned.child,
        rx,
        open_streams: open,
        started,
    })
}

#[derive(Default)]
pub struct WaitOptions {
    /// Kill the tree when the whole run exceeds this.
    pub max: Option<Duration>,
    /// Kill the tree when no output byte arrives for this long (agents only; never verify commands).
    pub stall: Option<Duration>,
    /// Polled about once per second; returning true terminates the tree (spike stand-in for R38 control requests).
    pub cancel: Option<Box<dyn Fn() -> bool + Send>>,
}

impl Running {
    /// Drain output (calling `on_chunk` for every redacted chunk) until the root
    /// process exits and both pipes are closed, or a limit fires.
    pub fn wait(mut self, opts: WaitOptions, mut on_chunk: impl FnMut(Chunk)) -> Result<ExitInfo> {
        let mut last_output = Instant::now();
        let (mut timed_out, mut stalled, mut cancelled) = (false, false, false);
        let mut last_cancel_poll = Instant::now();
        let mut exited_at: Option<Instant> = None;
        let mut lingering_killed = false;
        loop {
            if self.open_streams == 0 {
                // EC-32: both pipes are closed but the root still runs. Nothing can arrive
                // any more (the channel would report `Disconnected` at once, a busy loop):
                // poll the exit and the limits at a steady pace instead.
                std::thread::sleep(Duration::from_millis(50));
            } else {
                match self.rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(Some((is_err, bytes))) => {
                        last_output = Instant::now();
                        on_chunk(if is_err {
                            Chunk::Stderr(bytes)
                        } else {
                            Chunk::Stdout(bytes)
                        });
                    }
                    Ok(None) => {
                        self.open_streams = self.open_streams.saturating_sub(1);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => self.open_streams = 0,
                }
            }
            let exited = self.child.try_exit_code()?.is_some();
            if exited && self.open_streams == 0 {
                break;
            }
            if exited && exited_at.is_none() {
                exited_at = Some(Instant::now());
            }
            // Root is gone but a descendant still holds a pipe: end the stage after a grace period.
            let lingering = exited_at.is_some_and(|t| t.elapsed() > Duration::from_secs(5));
            if let Some(max) = opts.max {
                if self.started.elapsed() > max {
                    timed_out = true;
                }
            }
            if let Some(stall) = opts.stall {
                if last_output.elapsed() > stall {
                    stalled = true;
                }
            }
            if let Some(c) = &opts.cancel {
                if last_cancel_poll.elapsed() >= Duration::from_secs(1) {
                    last_cancel_poll = Instant::now();
                    if c() {
                        cancelled = true;
                    }
                }
            }
            if timed_out || stalled || cancelled || lingering {
                lingering_killed |= lingering && !(timed_out || stalled || cancelled);
                self.job.terminate(1)?;
                // Drain whatever the readers flushed on EOF (tails are redacted too).
                let deadline = Instant::now() + Duration::from_secs(5);
                while self.open_streams > 0 && Instant::now() < deadline {
                    match self.rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(Some((is_err, b))) => on_chunk(if is_err {
                            Chunk::Stderr(b)
                        } else {
                            Chunk::Stdout(b)
                        }),
                        Ok(None) => self.open_streams -= 1,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => break,
                    }
                }
                break;
            }
        }
        // Descendants that outlive the root (detached, not holding pipes) end with the stage.
        if self.job.active_processes()? > 0
            && self.child.try_exit_code()?.is_some()
            && !(timed_out || stalled || cancelled)
        {
            std::thread::sleep(Duration::from_millis(200));
            if self.job.active_processes()? > 0 {
                lingering_killed = true;
                self.job.terminate(1)?;
            }
        }
        let tree_gone = self.job.wait_empty(Duration::from_secs(5))?;
        let code = self.child.try_exit_code()?;
        Ok(ExitInfo {
            code,
            timed_out,
            stalled,
            cancelled,
            duration_ms: self.started.elapsed().as_millis(),
            tree_gone,
            lingering_killed,
        })
    }
}

/// Is this exact process (pid + creation time) still alive?
pub fn is_alive(p: ProcId) -> bool {
    platform::is_alive(p)
}

/// Terminate one process by id (used to simulate `Stop-Process -Force` on toryo.exe).
pub fn kill_pid(pid: u32) -> Result<()> {
    platform::kill_pid(pid)
}

/// Creation time of the current process, for (pid, created) identities.
pub fn current_proc_id() -> ProcId {
    platform::current_proc_id()
}

/// Give directories a Low mandatory label with inheritance (Windows, R36). No-op elsewhere.
pub fn label_low(dirs: &[PathBuf]) -> Result<()> {
    #[cfg(windows)]
    {
        for d in dirs {
            windows::set_low_label(d)?;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = dirs;
        bail!("Low integrity labels exist only on Windows")
    }
}

/// Remove ANSI escape sequences (CSI / OSC) from captured tool output. pwsh colours
/// warnings even with NO_COLOR set (M0 run); stored artifacts must be plain text.
pub fn strip_ansi(s: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)|[@-Z\\-_])")
            .expect("regex")
    });
    re.replace_all(s, "").into_owned()
}

/// Quote one argument for a Windows command line (CommandLineToArgvW rules).
pub fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty()
        && !arg
            .chars()
            .any(|c| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '"'))
    {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                backslashes = 0;
                out.push(c);
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_colour_codes() {
        assert_eq!(
            strip_ansi("\u{1b}[33;1mWARNING: x\u{1b}[0m\r\nok"),
            "WARNING: x\r\nok"
        );
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}plain"), "plain");
        assert_eq!(strip_ansi("no codes"), "no codes");
    }

    #[test]
    fn windows_quoting() {
        assert_eq!(quote_windows_arg("abc"), "abc");
        assert_eq!(quote_windows_arg("a b"), "\"a b\"");
        assert_eq!(quote_windows_arg(""), "\"\"");
        assert_eq!(quote_windows_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(
            quote_windows_arg(r"C:\path with space\"),
            r#""C:\path with space\\""#
        );
        assert_eq!(
            quote_windows_arg(r#"{"disableAllHooks":true}"#),
            r#""{\"disableAllHooks\":true}""#
        );
    }

    #[cfg(unix)]
    #[test]
    fn spawn_captures_and_redacts_output() {
        let red = Redactions::new(&[redact::Secret {
            name: "K".into(),
            value: "topsecret-123".into(),
        }]);
        let spec = SpawnSpec {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "printf 'a topsec'; printf 'ret-123 b\\n'; cat; echo err >&2".into(),
            ],
            cwd: std::env::temp_dir(),
            env: env::build_env(std::env::vars(), &BTreeMap::new(), &BTreeMap::new()),
            stdin: Some(b"from-stdin\n".to_vec()),
            boundary: Boundary::None,
        };
        let r = spawn(&spec, &red).unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let info = r
            .wait(WaitOptions::default(), |c| match c {
                Chunk::Stdout(b) => out.extend(b),
                Chunk::Stderr(b) => err.extend(b),
            })
            .unwrap();
        assert_eq!(info.code, Some(0));
        let s = String::from_utf8(out).unwrap();
        assert_eq!(s, "a [REDACTED:K] b\nfrom-stdin\n");
        assert_eq!(err, b"err\n");
        assert!(info.tree_gone);
    }

    #[cfg(unix)]
    #[test]
    fn stall_timeout_kills_the_whole_tree() {
        let spec = SpawnSpec {
            program: "/bin/sh".into(),
            // grandchild keeps running; the group kill must take it down too
            args: vec!["-c".into(), "sleep 300 & echo started; wait".into()],
            cwd: std::env::temp_dir(),
            env: env::build_env(std::env::vars(), &BTreeMap::new(), &BTreeMap::new()),
            stdin: None,
            boundary: Boundary::None,
        };
        let r = spawn(&spec, &Redactions::empty()).unwrap();
        let pids_probe = r.job.clone_handle();
        let info = r
            .wait(
                WaitOptions {
                    stall: Some(Duration::from_millis(800)),
                    ..Default::default()
                },
                |_| {},
            )
            .unwrap();
        assert!(info.stalled);
        assert!(info.tree_gone, "all processes of the group must be gone");
        assert_eq!(pids_probe.active_processes().unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn descendant_holding_the_pipe_does_not_hang_the_stage() {
        let spec = SpawnSpec {
            program: "/bin/sh".into(),
            // root exits at once; the background sleep inherits stdout and keeps it open
            args: vec!["-c".into(), "sleep 300 & echo done".into()],
            cwd: std::env::temp_dir(),
            env: env::build_env(std::env::vars(), &BTreeMap::new(), &BTreeMap::new()),
            stdin: None,
            boundary: Boundary::None,
        };
        let r = spawn(&spec, &Redactions::empty()).unwrap();
        let info = r.wait(WaitOptions::default(), |_| {}).unwrap();
        assert_eq!(info.code, Some(0));
        assert!(info.lingering_killed && info.tree_gone);
        assert!(info.duration_ms < 9000, "{}", info.duration_ms);
    }

    #[cfg(unix)]
    #[test]
    fn cancel_callback_stops_the_tree() {
        let spec = SpawnSpec {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "while true; do echo tick; sleep 0.2; done".into(),
            ],
            cwd: std::env::temp_dir(),
            env: env::build_env(std::env::vars(), &BTreeMap::new(), &BTreeMap::new()),
            stdin: None,
            boundary: Boundary::None,
        };
        let t0 = Instant::now();
        let r = spawn(&spec, &Redactions::empty()).unwrap();
        let info = r
            .wait(
                WaitOptions {
                    cancel: Some(Box::new(move || t0.elapsed() > Duration::from_secs(1))),
                    ..Default::default()
                },
                |_| {},
            )
            .unwrap();
        assert!(info.cancelled && info.tree_gone);
        assert!(info.duration_ms < 6000);
    }
}
