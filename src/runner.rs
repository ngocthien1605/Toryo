//! `CommandRunner` (plan §6.1, R10): how verify commands run. v0.1 ships `LocalRunner`
//! (this machine, optional Low-IL boundary, R36); a `RemoteRunner` for the D365FO dev VM
//! can implement the same trait later.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
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
}

impl CommandResult {
    pub fn ok(&self) -> bool {
        self.exit == Some(0)
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
            let running = spawn(&s, &spec.redact).with_context(|| {
                format!("starting `{}` ({})", spec.name, spec.program.display())
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
                        let _ = tx.send(strip_ansi(&String::from_utf8_lossy(&b)));
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
                output: strip_ansi(&String::from_utf8_lossy(&out)),
            })
        })
        .await?
    }
}
