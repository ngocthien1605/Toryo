//! `ClaudeOneShotAdapter` / `CodexExecAdapter` (plan §6.2, §6.2a, §6.3): one process per
//! turn, built on the M0 kit's launch lines (`claude`, `codex`), process supervision and
//! capture (`exec`). Each stdout line is turned into live events as it arrives.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use async_trait::async_trait;
use tokio::sync::mpsc;

use super::adapter::{AgentAdapter, AgentSession, TurnId};
use super::claude::{ClaudeLaunch, LaunchMode};
use super::codex::CodexLaunch;
use super::events::{line_events, terminal_events};
use super::exec::{run_turn_with, TurnOpts};
use crate::config::{expand_vars, Effective, Profile, ProfileKind};
use crate::domain::{
    AgentEvent, AgentHealth, ErrorClass, OpenSession, Role, Tri, TurnInput, UsageCapabilities,
};

/// A real CLI adapter for one profile.
pub struct CliAdapter {
    cfg: Arc<Effective>,
    profile_name: String,
    profile: Profile,
    claude_mode: LaunchMode,
}

impl CliAdapter {
    pub fn new(cfg: Arc<Effective>, profile: &str, claude_mode: LaunchMode) -> Result<Self> {
        let p = cfg.profile(profile)?.clone();
        Ok(Self {
            cfg,
            profile_name: profile.to_string(),
            profile: p,
            claude_mode,
        })
    }

    pub fn kind(&self) -> ProfileKind {
        self.profile.kind
    }
}

pub fn capabilities(kind: ProfileKind) -> UsageCapabilities {
    match kind {
        // Claude: stream events carry usage; per-run on --resume (M0-VERIFY 4, R48).
        ProfileKind::Claude => UsageCapabilities {
            live: true,
            terminal: true,
            cumulative_on_resume: Tri::No,
        },
        // Codex exec: usage only at turn.completed; cumulative per thread (R21).
        ProfileKind::Codex => UsageCapabilities {
            live: false,
            terminal: true,
            cumulative_on_resume: Tri::Yes,
        },
    }
}

#[async_trait]
impl AgentAdapter for CliAdapter {
    async fn open(&self, req: OpenSession) -> Result<Box<dyn AgentSession>> {
        if self.profile.kind == ProfileKind::Claude
            && req.role == Role::Implementer
            && self.claude_mode == LaunchMode::CwdRunStore
        {
            bail!("the Claude implementer needs launch mode restricted or setting-sources (cwd must be the worktree)");
        }
        let _ = &self.cfg; // the effective config stays alive for the session's lifetime
        Ok(Box::new(CliSession {
            profile_name: self.profile_name.clone(),
            profile: self.profile.clone(),
            claude_mode: self.claude_mode,
            external: req.resume.clone(),
            req,
            rx: None,
            cancel: Arc::new(AtomicBool::new(false)),
            task: None,
            turns: 0,
            done: false,
        }))
    }

    async fn health(&self) -> Result<AgentHealth> {
        let exe = expand_vars(&self.profile.exe)?;
        let exe2 = exe.clone();
        let out = tokio::task::spawn_blocking(move || {
            std::process::Command::new(&exe2).arg("--version").output()
        })
        .await??;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(AgentHealth {
            exe,
            version: crate::evidence::parse_version(&text),
        })
    }

    fn usage_capabilities(&self) -> UsageCapabilities {
        capabilities(self.profile.kind)
    }
}

struct CliSession {
    profile_name: String,
    profile: Profile,
    claude_mode: LaunchMode,
    req: OpenSession,
    external: Option<String>,
    rx: Option<mpsc::UnboundedReceiver<AgentEvent>>,
    cancel: Arc<AtomicBool>,
    task: Option<tokio::task::JoinHandle<()>>,
    turns: u32,
    done: bool,
}

impl CliSession {
    /// argv (without the program) and the prompt actually sent.
    fn build(&self, prompt: &str) -> Result<(Vec<String>, String)> {
        let exe = PathBuf::from(expand_vars(&self.profile.exe)?);
        let model = self.req.model.clone().or_else(|| self.profile.model.clone());
        match self.profile.kind {
            ProfileKind::Claude => {
                let mut l = match (self.req.role, &self.req.run_store) {
                    // R49: file tools confined to the worktree, never the run store
                    (Role::Implementer, _) => ClaudeLaunch::implementer(exe, self.req.cwd.clone()),
                    (_, Some(rs)) => ClaudeLaunch::read_only(exe, self.req.cwd.clone(), rs.clone()),
                    (_, None) => {
                        let mut l = ClaudeLaunch::read_only(exe, self.req.cwd.clone(), self.req.cwd.clone());
                        l.add_run_store = false;
                        l
                    }
                };
                l.mode = self.claude_mode;
                l.json_schema = self.req.schema.clone();
                l.context_file = self.req.context_file.clone();
                l.model = model;
                l.resume = self.external.clone();
                let (_cwd, args) = l.argv();
                Ok((args, prompt.to_string()))
            }
            ProfileKind::Codex => {
                let schema_path = match &self.req.schema {
                    Some(text) => {
                        std::fs::create_dir_all(&self.req.capture_dir)?;
                        let p = self.req.capture_dir.join(format!("{}.schema.json", self.req.role.as_str()));
                        std::fs::write(&p, text)?;
                        Some(p)
                    }
                    None => None,
                };
                let l = CodexLaunch {
                    exe,
                    cwd: self.req.cwd.clone(),
                    model,
                    settings: self.profile.codex.clone().unwrap_or_default(),
                    output_schema: schema_path,
                    resume_thread: self.external.clone(),
                    extra_args: vec![],
                };
                for w in l.preflight() {
                    eprintln!("WARNING: {w}");
                }
                // Codex has no system-prompt file flag: the context becomes the prompt preamble (§7).
                let prompt = match &self.req.context_file {
                    Some(f) => format!("{}\n\n{prompt}", std::fs::read_to_string(f)?),
                    None => prompt.to_string(),
                };
                Ok((l.argv(), prompt))
            }
        }
    }

    fn cwd(&self) -> PathBuf {
        match (self.profile.kind, self.claude_mode) {
            (ProfileKind::Claude, LaunchMode::CwdRunStore) if self.req.role != Role::Implementer => {
                self.req.run_store.clone().unwrap_or_else(|| self.req.cwd.clone())
            }
            _ => self.req.cwd.clone(),
        }
    }
}

#[async_trait]
impl AgentSession for CliSession {
    fn external_id(&self) -> Option<&str> {
        self.external.as_deref()
    }

    async fn start_turn(&mut self, input: TurnInput) -> Result<TurnId> {
        if let Some(t) = self.task.take() {
            let _ = t.await;
        }
        let (args, prompt) = self.build(&input.prompt)?;
        let (tx, rx) = mpsc::unbounded_channel();
        self.rx = Some(rx);
        self.done = false;
        self.cancel.store(false, Ordering::SeqCst);
        let kind = self.profile.kind;
        let profile_name = self.profile_name.clone();
        let profile = self.profile.clone();
        let cwd = self.cwd();
        let capture = self.req.capture_dir.clone();
        let opts = TurnOpts {
            // limits are enforced by run_to_completion; these are generous backstops
            max: Duration::from_secs(4 * 3600),
            stall: Duration::from_secs(4 * 3600),
            kill_after: None,
            echo: input.echo,
            env: input.env.clone(),
            cancel: Some(self.cancel.clone()),
        };
        let label = input.label.clone();
        self.task = Some(tokio::task::spawn_blocking(move || {
            let tx_lines = tx.clone();
            let mut on_line = move |line: &[u8]| {
                for e in line_events(kind, line) {
                    let _ = tx_lines.send(e);
                }
            };
            let res = run_turn_with(
                &profile_name, &profile, args, &cwd, &prompt, &capture, &label, opts, &mut on_line,
            );
            match res {
                Ok(run) => {
                    for e in terminal_events(&run.outcome) {
                        let _ = tx.send(e);
                    }
                }
                Err(e) => {
                    let _ = tx.send(AgentEvent::TurnFailed {
                        class: ErrorClass::ProcessCrashed,
                        detail: format!("{e:#}"),
                        structured: None,
                    });
                }
            }
        }));
        self.turns += 1;
        Ok(self.turns)
    }

    async fn next_event(&mut self) -> Option<AgentEvent> {
        if self.done {
            return None;
        }
        let e = self.rx.as_mut()?.recv().await?;
        match &e {
            AgentEvent::SessionIdentified(id) => self.external = Some(id.clone()),
            e if e.is_terminal() => self.done = true,
            _ => {}
        }
        Some(e)
    }

    async fn interrupt(&mut self) -> Result<()> {
        self.cancel.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn close(mut self: Box<Self>) -> Result<()> {
        if let Some(t) = self.task.take() {
            self.cancel.store(true, Ordering::SeqCst);
            let _ = t.await;
        }
        Ok(())
    }
}
