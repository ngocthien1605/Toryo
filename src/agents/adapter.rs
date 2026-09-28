//! The adapter contract (plan §6.1): one streaming session per stage, events only.

use std::time::Instant;

use anyhow::Result;
use async_trait::async_trait;

use crate::domain::{
    AgentEvent, AgentHealth, ErrorClass, OpenSession, StageOutcome, TurnInput, TurnLimits,
    UsageCapabilities,
};

pub type TurnId = u32;

#[async_trait]
pub trait AgentAdapter: Send + Sync {
    /// Open a new session, or resume `req.resume`.
    async fn open(&self, req: OpenSession) -> Result<Box<dyn AgentSession>>;
    /// Executable, version (and, for real CLIs, that it starts at all).
    async fn health(&self) -> Result<AgentHealth>;
    /// R41/R48: what this adapter's usage telemetry supports.
    fn usage_capabilities(&self) -> UsageCapabilities;
}

#[async_trait]
pub trait AgentSession: Send {
    /// The external session id (Claude `session_id` / Codex `thread_id`), once known.
    fn external_id(&self) -> Option<&str>;
    /// Start a turn. A later turn on the same session resumes it (correction turns).
    async fn start_turn(&mut self, input: TurnInput) -> Result<TurnId>;
    /// The next event; `None` after the terminal event has been delivered.
    async fn next_event(&mut self) -> Option<AgentEvent>;
    /// No-op for the v0.1 one-shot adapters (they never ask for approvals).
    async fn respond(&mut self, _request: u32, _allow: bool) -> Result<()> {
        Ok(())
    }
    /// Stop the running turn (Job termination for real CLIs); the stream then ends with
    /// `TurnFailed`.
    async fn interrupt(&mut self) -> Result<()>;
    async fn close(self: Box<Self>) -> Result<()>;
}

/// Run one turn to its terminal event, enforcing the stall and max limits.
/// A limit hit interrupts the turn and reports `Timeout` (`timed_out = true`).
pub async fn run_to_completion(
    session: &mut dyn AgentSession,
    input: TurnInput,
    limits: TurnLimits,
) -> Result<StageOutcome> {
    let started = Instant::now();
    let label = input.label.clone();
    session.start_turn(input).await?;
    let mut out = StageOutcome {
        label,
        ..Default::default()
    };
    let deadline = tokio::time::Instant::now() + limits.max;
    let mut interrupted = false;
    loop {
        let now = tokio::time::Instant::now();
        let wait = if interrupted {
            // after an interrupt, give the session a moment to deliver its terminal event
            std::time::Duration::from_secs(10)
        } else {
            limits.stall.min(deadline.saturating_duration_since(now))
        };
        match tokio::time::timeout(wait, session.next_event()).await {
            Ok(Some(e)) => {
                let terminal = e.is_terminal();
                out.apply(&e);
                if terminal {
                    break;
                }
            }
            Ok(None) => {
                if out.error_class.is_none() && !out.success {
                    out.error_class = Some(ErrorClass::ProcessCrashed);
                    out.error_detail = Some("event stream ended without a terminal event".into());
                }
                break;
            }
            Err(_) if !interrupted => {
                out.timed_out = true;
                interrupted = true;
                let _ = session.interrupt().await;
            }
            Err(_) => break,
        }
    }
    if out.timed_out {
        out.success = false;
        out.error_class = Some(ErrorClass::Timeout);
        out.error_detail = Some(format!(
            "no terminal event within the limits (stall {}s, max {}s)",
            limits.stall.as_secs(),
            limits.max.as_secs()
        ));
    }
    out.duration_ms = started.elapsed().as_millis();
    if let Some(id) = session.external_id() {
        out.session_id.get_or_insert_with(|| id.to_string());
    }
    Ok(out)
}
