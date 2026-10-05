//! `CommandRunner` (plan §6.1, R10): how verify commands run. v0.1 ships `LocalRunner`
//! (this machine, optional Low-IL boundary, R36); a `RemoteRunner` for the D365FO dev VM
//! can implement the same trait later.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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
            let mut out = Vec::new();
            let info = running.wait(
                WaitOptions {
                    max: Some(spec.timeout),
                    stall: None,
                    cancel: cancel_flag.map(|c| {
                        Box::new(move || c.load(Ordering::SeqCst)) as Box<dyn Fn() -> bool + Send>
                    }),
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
                    out.extend(b);
                },
            )?;
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
                // M11-R8: strip the colour codes, then redact: a secret split by a colour code is found
                // M12-15 (X23): decode (a cp932 line is not `�`), then colour codes, then redact
                output: spec
                    .redact
                    .redact_str(&strip_ansi(&crate::process::decode_output(&out))),
                signal: info.signal,
            })
        })
        .await?
    }
}
