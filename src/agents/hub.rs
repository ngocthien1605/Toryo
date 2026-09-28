//! `AgentHub`: a small synchronous facade over the async adapter layer, for the
//! (still synchronous) engine code. It owns a tokio runtime and picks the adapter for a
//! profile — the real CLI adapter, or the `FakeAgentAdapter` when a fake script is set.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use super::adapter::{run_to_completion_with, AgentAdapter, TurnHooks};
use super::claude::LaunchMode;
use super::cli::CliAdapter;
use super::fake::{FakeAgentAdapter, FakeCall, FakeScript};
use crate::config::Effective;
use crate::domain::{AgentHealth, OpenSession, StageOutcome, TurnInput, TurnLimits, UsageCapabilities};

pub struct AgentHub {
    rt: tokio::runtime::Runtime,
    cfg: Arc<Effective>,
    claude_mode: LaunchMode,
    fake: Option<FakeAgentAdapter>,
}

impl AgentHub {
    pub fn new(cfg: &Effective, claude_mode: LaunchMode, fake_script: Option<&Path>) -> Result<Self> {
        let fake = match fake_script {
            Some(p) => Some(FakeAgentAdapter::new(FakeScript::load(p)?)),
            None => None,
        };
        Ok(Self {
            rt: tokio::runtime::Builder::new_multi_thread().enable_all().build()?,
            cfg: Arc::new(cfg.clone()),
            claude_mode,
            fake,
        })
    }

    pub fn is_fake(&self) -> bool {
        self.fake.is_some()
    }

    /// Calls recorded by the fake adapter (empty for real agents).
    pub fn fake_calls(&self) -> Vec<FakeCall> {
        self.fake.as_ref().map(|f| f.calls()).unwrap_or_default()
    }

    pub fn adapter(&self, profile: &str) -> Result<Box<dyn AgentAdapter>> {
        let kind = self.cfg.profile(profile)?.kind;
        Ok(match &self.fake {
            Some(f) => Box::new(f.for_profile(profile, kind)),
            None => Box::new(CliAdapter::new(self.cfg.clone(), profile, self.claude_mode)?),
        })
    }

    pub fn capabilities(&self, profile: &str) -> Result<UsageCapabilities> {
        Ok(self.adapter(profile)?.usage_capabilities())
    }

    pub fn health(&self, profile: &str) -> Result<AgentHealth> {
        let a = self.adapter(profile)?;
        self.rt.block_on(a.health())
    }

    /// Open a session, run one turn to completion, close.
    pub fn turn(&self, open: OpenSession, input: TurnInput, limits: TurnLimits) -> Result<StageOutcome> {
        self.turn_with(open, input, limits, TurnHooks::default())
    }

    /// [`AgentHub::turn`] with hooks (stop request, session id as soon as known).
    pub fn turn_with(&self, open: OpenSession, input: TurnInput, limits: TurnLimits, hooks: TurnHooks) -> Result<StageOutcome> {
        let adapter = self.adapter(&open.profile)?;
        let capture = open.capture_dir.clone();
        self.rt.block_on(async move {
            let mut session = adapter.open(open).await?;
            let mut out = run_to_completion_with(session.as_mut(), input, limits, hooks).await?;
            session.close().await?;
            out.capture_dir = Some(capture);
            Ok(out)
        })
    }

    /// Run any future on the hub's runtime (e.g. a `CommandRunner`).
    pub fn block_on<F: std::future::Future>(&self, f: F) -> F::Output {
        self.rt.block_on(f)
    }
}
