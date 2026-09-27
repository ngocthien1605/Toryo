//! Unix stand-in for the Windows Job Object: one process group per stage.
//! Used by the tests and when the spike runs on Linux/macOS. Differences to Windows
//! (documented in the runbook): no kill-on-parent-death, no Low integrity boundary.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use super::{Boundary, ProcId, SpawnSpec};

pub struct Job {
    pgid: i32,
}

impl Job {
    pub fn terminate(&self, _code: u32) -> Result<()> {
        // SAFETY: plain syscall; negative pid addresses the process group.
        unsafe {
            libc::kill(-self.pgid, libc::SIGKILL);
        }
        Ok(())
    }

    pub fn active_processes(&self) -> Result<u32> {
        Ok(self.process_ids()?.len() as u32)
    }

    /// Members of the group (Linux: /proc scan). Zombies are not counted.
    pub fn process_ids(&self) -> Result<Vec<ProcId>> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir("/proc") else {
            // Non-Linux: best effort via kill(0).
            // SAFETY: signal 0 only checks existence.
            let alive = unsafe { libc::kill(-self.pgid, 0) } == 0;
            if alive {
                out.push(ProcId {
                    pid: self.pgid as u32,
                    created: 0,
                });
            }
            return Ok(out);
        };
        for e in rd.flatten() {
            let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            if let Some((state, pgrp, start)) = stat(pid) {
                if pgrp == self.pgid && state != 'Z' && state != 'X' {
                    out.push(ProcId {
                        pid,
                        created: start,
                    });
                }
            }
        }
        Ok(out)
    }

    pub fn wait_empty(&self, max: Duration) -> Result<bool> {
        let deadline = Instant::now() + max;
        loop {
            if self.active_processes()? == 0 {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn clone_handle(&self) -> Job {
        Job { pgid: self.pgid }
    }
}

/// (state, pgrp, starttime) from /proc/<pid>/stat.
fn stat(pid: u32) -> Option<(char, i32, u64)> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &s[s.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    // after ")": state(0) ppid(1) pgrp(2) ... starttime is field 22 overall → index 19 here
    Some((
        f.first()?.chars().next()?,
        f.get(2)?.parse().ok()?,
        f.get(19)?.parse().ok()?,
    ))
}

pub struct Child(std::process::Child);

impl Child {
    pub fn try_exit_code(&mut self) -> Result<Option<i32>> {
        Ok(self.0.try_wait()?.map(|s| s.code().unwrap_or(-1)))
    }
}

pub struct Spawned {
    pub job: Job,
    pub root: ProcId,
    pub child: Child,
    pub stdin: Option<Box<dyn Write + Send>>,
    pub stdout: Option<Box<dyn Read + Send>>,
    pub stderr: Option<Box<dyn Read + Send>>,
}

pub fn spawn_in_job(spec: &SpawnSpec) -> Result<Spawned> {
    if spec.boundary == Boundary::LowIntegrity {
        bail!("verify boundary `low_integrity` is Windows-only (verify_boundary_unavailable)");
    }
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(&spec.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if std::env::var_os("THIEN_FAULT_ASSIGN").is_some() {
        bail!("fault injection: job assignment failed before the child ran (THIEN_FAULT_ASSIGN)");
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", spec.program.display()))?;
    let pid = child.id();
    let created = stat(pid).map(|s| s.2).unwrap_or(0);
    let stdin = child
        .stdin
        .take()
        .map(|s| Box::new(s) as Box<dyn Write + Send>);
    let stdout = child
        .stdout
        .take()
        .map(|s| Box::new(s) as Box<dyn Read + Send>);
    let stderr = child
        .stderr
        .take()
        .map(|s| Box::new(s) as Box<dyn Read + Send>);
    Ok(Spawned {
        job: Job { pgid: pid as i32 },
        root: ProcId { pid, created },
        child: Child(child),
        stdin,
        stdout,
        stderr,
    })
}

pub fn is_alive(p: ProcId) -> bool {
    match stat(p.pid) {
        Some((state, _, start)) => {
            state != 'Z' && state != 'X' && (p.created == 0 || start == p.created)
        }
        None => false,
    }
}

pub fn kill_pid(pid: u32) -> Result<()> {
    // SAFETY: plain syscall.
    let r = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    if r != 0 {
        bail!("kill({pid}) failed");
    }
    Ok(())
}

pub fn current_proc_id() -> ProcId {
    let pid = std::process::id();
    ProcId {
        pid,
        created: stat(pid).map(|s| s.2).unwrap_or(0),
    }
}
