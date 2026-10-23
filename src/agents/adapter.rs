//! The adapter contract (plan §6.1): one streaming session per stage, events only.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
/// Every event is activity for the stall limit; the real adapters send at least a
/// `Heartbeat` for each line the agent prints, so only silence stalls a turn (EC-06).
/// While the agent runs a command (`CommandRunning(true)` until `CommandRunning(false)`)
/// silence is expected and the stall limit does not run; the max limit and a stop request
/// still end the turn.
pub async fn run_to_completion(
    session: &mut dyn AgentSession,
    input: TurnInput,
    limits: TurnLimits,
) -> Result<StageOutcome> {
    run_to_completion_with(session, input, limits, TurnHooks::default()).await
}

/// How often a stop request and the max limit are checked while a turn runs.
const LIMITS_POLL: Duration = Duration::from_millis(200);

/// R23-21: how long a session has to deliver its terminal event after `interrupt()`. The CLI
/// adapter needs up to about 11 s for it on a loaded machine (the stop poll, 1 s; the drain of
/// its output, 5 s; the wait for its pipes, 5 s) — with 10 s the `TurnFailed` detail and the
/// permission denials of a stopped turn could be lost.
const INTERRUPT_GRACE: Duration = Duration::from_secs(15);

/// Called with the external session id as soon as it is known.
pub type SessionHook = Arc<dyn Fn(&str) + Send + Sync>;

/// Called with the turn's usage so far on every usage event (live adapters, R41).
pub type UsageHook = Arc<dyn Fn(&crate::domain::TokenUsage) + Send + Sync>;

/// M16-AT (X39): called when the turn's agent started, with its process id (none for a
/// scripted fake agent).
pub type SpawnHook = Arc<dyn Fn(Option<u32>) + Send + Sync>;

/// Optional hooks for one turn.
#[derive(Clone, Default)]
pub struct TurnHooks {
    /// A stop request (R38): interrupts the turn, reported as `cancelled`.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Called as soon as the external session id is known, so a crash mid-turn can still
    /// resume the session (R48).
    pub on_session: Option<SessionHook>,
    /// R41: live usage (Claude), e.g. to interrupt a turn when the budget is crossed.
    pub on_usage: Option<UsageHook>,
    /// M16-AT (X39): the agent started (`AgentEvent::Spawned`).
    pub on_spawn: Option<SpawnHook>,
}

/// Like [`run_to_completion`], with [`TurnHooks`].
pub async fn run_to_completion_with(
    session: &mut dyn AgentSession,
    input: TurnInput,
    limits: TurnLimits,
    hooks: TurnHooks,
) -> Result<StageOutcome> {
    let cancel = hooks.cancel.clone();
    let started = Instant::now();
    let label = input.label.clone();
    session.start_turn(input).await?;
    let mut out = StageOutcome {
        label,
        ..Default::default()
    };
    let deadline = tokio::time::Instant::now() + limits.max;
    let mut last_event = tokio::time::Instant::now();
    // EC-06: a running command is silent until it ends, however long it takes
    let mut command_running = false;
    // When a limit is hit unless an event comes first: the stall limit counts from the last
    // event, but not while a command runs; the max limit always counts.
    let limit_at = |last: tokio::time::Instant, running: bool| {
        if running {
            deadline
        } else {
            (last + limits.stall).min(deadline)
        }
    };
    let mut checked = tokio::time::Instant::now();
    let mut interrupted_at: Option<tokio::time::Instant> = None;
    loop {
        let now = tokio::time::Instant::now();
        let mut wait = match interrupted_at {
            // after an interrupt, give the session a moment to deliver its terminal event
            Some(t) => (t + INTERRUPT_GRACE).saturating_duration_since(now),
            None => limit_at(last_event, command_running).saturating_duration_since(now),
        };
        if cancel.is_some() && interrupted_at.is_none() {
            wait = wait.min(LIMITS_POLL);
        }
        let paused = match tokio::time::timeout(wait, session.next_event()).await {
            Ok(Some(e)) => {
                last_event = tokio::time::Instant::now();
                if let AgentEvent::CommandRunning(running) = &e {
                    command_running = *running;
                }
                if let (AgentEvent::SessionIdentified(id), Some(f)) = (&e, &hooks.on_session) {
                    f(id);
                }
                if let (AgentEvent::Usage(u), Some(f)) = (&e, &hooks.on_usage) {
                    f(u);
                }
                if let (AgentEvent::Spawned(pid), Some(f)) = (&e, &hooks.on_spawn) {
                    f(*pid);
                }
                let terminal = e.is_terminal();
                out.apply(&e);
                if terminal {
                    break;
                }
                false
            }
            Ok(None) => {
                if out.error_class.is_none() && !out.success {
                    out.error_class = Some(ErrorClass::ProcessCrashed);
                    out.error_detail = Some("event stream ended without a terminal event".into());
                }
                break;
            }
            Err(_) if interrupted_at.is_some() => break,
            Err(_) => true,
        };
        // The limits are checked whenever the stream pauses and, while events keep coming,
        // every LIMITS_POLL: a stream that never pauses (every line an agent prints is at
        // least a heartbeat) must not hold off a stop request or the max limit.
        let now = tokio::time::Instant::now();
        let due = paused || now >= checked + LIMITS_POLL;
        if interrupted_at.is_some() || !due {
            continue;
        }
        checked = now;
        if cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst)) {
            out.cancelled = true;
        } else if now >= limit_at(last_event, command_running) {
            out.timed_out = true;
        } else {
            continue;
        }
        interrupted_at = Some(now);
        let _ = session.interrupt().await;
    }
    if out.timed_out || out.cancelled {
        out.success = false;
        if out.cancelled {
            out.error_class = Some(ErrorClass::ProcessCrashed);
            out.error_detail = Some("interrupted by `toryo stop`".into());
        } else {
            out.error_class = Some(ErrorClass::Timeout);
            out.error_detail = Some(format!(
                "no terminal event within the limits (stall {}s, max {}s)",
                limits.stall.as_secs(),
                limits.max.as_secs()
            ));
        }
    }
    out.duration_ms = started.elapsed().as_millis();
    if let Some(id) = session.external_id() {
        out.session_id.get_or_insert_with(|| id.to_string());
    }
    Ok(out)
}
