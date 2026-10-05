//! `CommandRunner` (plan §6.1, R10): how verify commands run. v0.1 ships `LocalRunner`
//! (this machine, optional Low-IL boundary, R36); a `RemoteRunner` for the D365FO dev VM
//! can implement the same trait later.

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc::UnboundedSender;

use crate::process::redact::Redactions;
use crate::process::{spawn, strip_ansi, Boundary, Chunk, SpawnSpec, WaitOptions};

#[derive(Debug, Clone)]
pub struct RunSpec {
    pub name: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub boundary: Boundary,
    pub timeout: Duration,
    /// M11-E2 (review M2): the task's secrets, redacted from the output as it is read.
    pub redact: Redactions,
    /// Set to `true` to terminate the command's Job.
    pub cancel: Option<Arc<AtomicBool>>,
}

/// M12-05: the command could not be started at all (no such program, not an executable) — a
/// problem of the machine, never a failing gate. Carried typed, so the engine never reads it
/// out of the message.
#[derive(Debug)]
pub struct StartFailed {
    pub name: String,
    pub program: String,
    /// The OS error, with its context.
    pub error: String,
}

impl std::fmt::Display for StartFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "starting `{}` ({}): {}",
            self.name, self.program, self.error
        )
    }
}

impl std::error::Error for StartFailed {}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CommandResult {
    pub name: String,
    /// `None` when the command was stopped (timeout, cancel).
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub duration_ms: u128,
    /// stdout + stderr, ANSI-stripped (R50).
    pub output: String,
    /// M11-R17: the signal that ended the command (Unix; `exit` is then -1). Left out when
    /// there is none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    /// M12-16: the file holding `output` (redacted) when the caller asked for one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<PathBuf>,
}

/// M12-16: the first part of a gate's output that is kept ...
pub const OUTPUT_HEAD_KEEP: usize = 1024 * 1024;
/// ... and the last part; what lies between is dropped and marked.
pub const OUTPUT_TAIL_KEEP: usize = 8 * 1024 * 1024;

/// M12-16: a gate's raw output, bounded — the first [`OUTPUT_HEAD_KEEP`] bytes and the last
/// [`OUTPUT_TAIL_KEEP`] bytes; the rest is counted and replaced by one marker line.
#[derive(Default)]
pub struct BoundedOutput {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    omitted: u64,
    total: u64,
}

impl BoundedOutput {
    pub fn push(&mut self, bytes: &[u8]) {
        self.total += bytes.len() as u64;
        let room = OUTPUT_HEAD_KEEP - self.head.len();
        let (to_head, rest) = bytes.split_at(room.min(bytes.len()));
        self.head.extend_from_slice(to_head);
        self.tail.extend(rest.iter().copied());
        if self.tail.len() > OUTPUT_TAIL_KEEP {
            let over = self.tail.len() - OUTPUT_TAIL_KEEP;
            self.tail.drain(..over);
            self.omitted += over as u64;
        }
    }

    /// Bytes seen so far, kept or not.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// The kept bytes, with the marker line where bytes were dropped.
    pub fn into_bytes(self) -> Vec<u8> {
        let mut out = self.head;
        if self.omitted > 0 {
            out.extend(
                format!(
                    "\n[... {} bytes of output omitted: Toryo keeps the first {} MB and the last {} MB of a gate's output ...]\n",
                    self.omitted,
                    OUTPUT_HEAD_KEEP / (1024 * 1024),
                    OUTPUT_TAIL_KEEP / (1024 * 1024)
                )
                .bytes(),
            );
        }
        out.extend(self.tail);
        out
    }
}

/// M12-16: what the engine asks of a gate run besides [`RunSpec`]. Nothing by default.
#[derive(Default, Clone)]
pub struct GateIo {
    /// Write the (redacted, bounded) output here when the run is over.
    pub log: Option<PathBuf>,
    /// A line on the console every this long while the gate runs: its name, the time and the
    /// bytes of output so far.
    pub heartbeat: Option<Duration>,
}

impl CommandResult {
    pub fn ok(&self) -> bool {
        self.exit == Some(0)
    }

    /// M11-R17: how the command ended, in words — `exit 1`, `exit -1 (killed by signal 9,
    /// SIGKILL)`, `no exit code (timed out)`. On Unix an exit code of -1 always means a signal
    /// ended the process: a program that cannot be started is an error, never an exit code.
    pub fn ended(&self) -> String {
        let name = |n: i32| match n {
            1 => ", SIGHUP",
            2 => ", SIGINT",
            3 => ", SIGQUIT",
            4 => ", SIGILL",
            6 => ", SIGABRT",
            7 => ", SIGBUS",
            8 => ", SIGFPE",
            9 => ", SIGKILL",
            11 => ", SIGSEGV",
            13 => ", SIGPIPE",
            15 => ", SIGTERM",
            _ => "",
        };
        match (self.exit, self.signal) {
            (Some(c), Some(s)) => format!("exit {c} (killed by signal {s}{})", name(s)),
            (Some(c), None) => format!("exit {c}"),
            (None, _) if self.timed_out => "no exit code (timed out)".into(),
            (None, _) => "no exit code (stopped)".into(),
        }
    }

    /// The last `n` lines of output.
    pub fn tail(&self, n: usize) -> String {
        let lines: Vec<&str> = self.output.lines().collect();
        lines[lines.len().saturating_sub(n)..].join("\n")
    }
}

#[async_trait]
pub trait CommandRunner: Send + Sync {
    /// Run to completion; `sink` receives output chunks (ANSI-stripped) as they arrive.
    async fn run(
        &self,
        spec: RunSpec,
        sink: Option<UnboundedSender<String>>,
    ) -> Result<CommandResult>;
}

/// Runs on this machine inside a per-command Job Object.
pub struct LocalRunner;

#[async_trait]
impl CommandRunner for LocalRunner {
    async fn run(
        &self,
        spec: RunSpec,
        sink: Option<UnboundedSender<String>>,
    ) -> Result<CommandResult> {
        self.run_with(spec, sink, GateIo::default()).await
    }
}

impl LocalRunner {
    /// [`CommandRunner::run`] with the log and the heartbeat of M12-16.
    pub async fn run_with(
        &self,
        spec: RunSpec,
        sink: Option<UnboundedSender<String>>,
        io: GateIo,
    ) -> Result<CommandResult> {
        tokio::task::spawn_blocking(move || {
            let s = SpawnSpec {
                program: spec.program.clone(),
                args: spec.args.clone(),
                cwd: spec.cwd.clone(),
                env: spec.env.clone(),
                stdin: None,
                boundary: spec.boundary,
            };
            // the raw streams, interleaved as they arrive; the whole output is redacted at the
            // end (M11-E2): a per-stream redactor holds back bytes and would reorder stdout and
            // stderr when any secret is configured (group E review)
            let running = spawn(&s, &Redactions::empty()).map_err(|e| {
                anyhow::Error::new(StartFailed {
                    name: spec.name.clone(),
                    program: spec.program.display().to_string(),
                    error: format!("{e:#}"),
                })
            })?;
            let cancel_flag = spec.cancel.clone();
            // M12-16: bounded — the first 1 MB and the last 8 MB; the count is shared with the
            // heartbeat, which runs on this thread's `wait` loop
            let mut out = BoundedOutput::default();
            let seen = Arc::new(AtomicU64::new(0));
            let heartbeat = io.heartbeat.map(|every| {
                let (name, seen) = (spec.name.clone(), seen.clone());
                let beat: crate::process::Heartbeat = Box::new(move |elapsed| {
                    eprintln!(
                        "  ... {name} still running: {}:{:02} elapsed, {} bytes of output so far",
                        elapsed.as_secs() / 60,
                        elapsed.as_secs() % 60,
                        seen.load(Ordering::Relaxed)
                    );
                });
                (every, beat)
            });
            let info = running.wait(
                WaitOptions {
                    max: Some(spec.timeout),
                    stall: None,
                    cancel: cancel_flag.map(|c| {
                        Box::new(move || c.load(Ordering::SeqCst)) as Box<dyn Fn() -> bool + Send>
                    }),
                    heartbeat,
                },
                |c| {
                    let (Chunk::Stdout(b) | Chunk::Stderr(b)) = c;
                    if let Some(tx) = &sink {
                        // live chunks (no engine caller uses them) are redacted chunk by chunk
                        // M11-R8: colour codes go first, so a secret split by one is found
                        let _ = tx.send(
                            spec.redact
                                .redact_str(&strip_ansi(&crate::process::decode_output(&b))),
                        );
                    }
                    out.push(&b);
                    seen.store(out.total(), Ordering::Relaxed);
                },
            )?;
            // M11-R8: strip the colour codes, then redact: a secret split by a colour code is found
            // M12-15 (X23): decode (a cp932 line is not `�`), then colour codes, then redact
            let output = spec
                .redact
                .redact_str(&strip_ansi(&crate::process::decode_output(
                    &out.into_bytes(),
                )));
            // M12-16: the log holds exactly the text the result holds; a log that cannot be
            // written is not a failing gate
            let log = io.log.as_ref().and_then(|p| {
                if let Some(dir) = p.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                match std::fs::write(p, &output) {
                    Ok(()) => Some(p.clone()),
                    Err(e) => {
                        eprintln!("WARNING: could not write the gate log {}: {e}", p.display());
                        None
                    }
                }
            });
            Ok(CommandResult {
                name: spec.name,
                exit: if info.timed_out || info.cancelled {
                    None
                } else {
                    info.code
                },
                timed_out: info.timed_out,
                cancelled: info.cancelled,
                duration_ms: info.duration_ms,
                output,
                signal: info.signal,
                log,
            })
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M12-16: below the bound nothing is dropped; above it the first 1 MB and the last 8 MB
    /// stay with one marker between.
    #[test]
    fn bounded_output_keeps_the_head_and_the_tail() {
        let mut small = BoundedOutput::default();
        small.push(b"abc");
        small.push(b"def");
        assert_eq!(small.total(), 6);
        assert_eq!(small.into_bytes(), b"abcdef");
        // exactly head + tail: nothing omitted
        let mut edge = BoundedOutput::default();
        edge.push(&vec![b'a'; OUTPUT_HEAD_KEEP]);
        edge.push(&vec![b'b'; OUTPUT_TAIL_KEEP]);
        let bytes = edge.into_bytes();
        assert_eq!(bytes.len(), OUTPUT_HEAD_KEEP + OUTPUT_TAIL_KEEP);
        // one byte more: one byte omitted, marked
        let mut over = BoundedOutput::default();
        over.push(&vec![b'a'; OUTPUT_HEAD_KEEP]);
        over.push(&vec![b'b'; OUTPUT_TAIL_KEEP + 1]);
        let bytes = over.into_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("\n[... 1 bytes of output omitted"),
            "{}",
            &text[OUTPUT_HEAD_KEEP..OUTPUT_HEAD_KEEP + 120]
        );
        assert!(bytes.starts_with(b"aaaa") && bytes.ends_with(b"bbbb"));
        assert!(bytes.len() < OUTPUT_HEAD_KEEP + OUTPUT_TAIL_KEEP + 200);
        // many small chunks
        let mut many = BoundedOutput::default();
        for i in 0..1_000_000u32 {
            many.push(format!("line {i:06}\n").as_bytes());
        }
        let n = many.total();
        let bytes = many.into_bytes();
        assert!(n > (OUTPUT_HEAD_KEEP + OUTPUT_TAIL_KEEP) as u64);
        assert!(bytes.starts_with(b"line 000000\n") && bytes.ends_with(b"line 999999\n"));
    }
}
