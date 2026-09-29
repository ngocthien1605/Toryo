//! Vendor-neutral domain types (plan §5.1, §6.1).
//!
//! Orchestration code works only with these: it never sees Claude/Codex JSON, launch
//! flags or parser state (M1 acceptance: "no vendor types outside adapters", checked by
//! `tests/architecture.rs`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::usage::{Accounting, Buckets};

/// Plan §6.1 error classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorClass {
    RateLimited,
    Overloaded,
    QuotaExceeded,
    ContextWindowExceeded,
    Auth,
    BadRequest,
    Network,
    ProcessCrashed,
    Timeout,
    PolicyRefusal,
    PermissionDenied,
    InvalidOutput,
    Unknown,
}

/// The three agent roles of the v0.1 pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Planner,
    Implementer,
    Reviewer,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Planner => "planner",
            Role::Implementer => "implementer",
            Role::Reviewer => "reviewer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tri {
    Yes,
    No,
    Unknown,
}

/// R41/R48: what an adapter's usage telemetry can actually support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageCapabilities {
    /// Usage arrives while the turn runs (Claude stream events: yes; Codex exec: no).
    pub live: bool,
    /// Usage arrives with the terminal event.
    pub terminal: bool,
    /// Usage on a resumed session is cumulative for the session (Codex: yes; Claude: no, M0-VERIFY 4).
    pub cumulative_on_resume: Tri,
    /// A killed turn's tokens are proven to be inside the next cumulative total of its
    /// session (R41). Codex: cannot be shown (no telemetry for the killed turn); Claude: no.
    #[serde(default = "tri_unknown")]
    pub killed_turn_in_resume: Tri,
}

fn tri_unknown() -> Tri {
    Tri::Unknown
}

/// Token usage of one turn. `buckets = None` means nothing is known (never stored as 0, R41).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TokenUsage {
    pub buckets: Option<Buckets>,
    pub accounting: Accounting,
}

impl TokenUsage {
    pub fn total(&self) -> u64 {
        self.buckets.map(|b| b.total()).unwrap_or(0)
    }
}

/// Streamed by an `AgentSession`; a turn always ends with `TurnCompleted` or `TurnFailed`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum AgentEvent {
    SessionIdentified(String),
    TextDelta(String),
    ToolActivity {
        name: String,
        summary: String,
    },
    PermissionDenied {
        tool: String,
        detail: String,
    },
    /// The turn's usage so far. Live adapters send provisional, cumulative-for-the-turn
    /// values while the turn runs (R41); the terminal one is the turn's final usage.
    Usage(TokenUsage),
    /// The agent printed a line that maps to no other event (thinking, tool input, a
    /// started item …). Carries nothing: it only proves the agent is alive, so the stall
    /// limit counts output rather than mapped events (EC-06). Consumers ignore it.
    Heartbeat,
    /// The agent started running a command (`true`, the first one if several run) or the
    /// last running one ended (`false`). A command prints nothing to the stream until it
    /// ends (Codex exec reports it only when it starts and when it ends), however long
    /// it takes (a model build): while one runs, silence is not a stall, so the stall
    /// limit is suspended; the max limit and stop requests still apply (EC-06). Sent by
    /// the real CLI adapter only. Consumers ignore it.
    CommandRunning(bool),
    Retrying {
        attempt: u32,
        delay_ms: u64,
        class: ErrorClass,
    },
    TurnCompleted {
        final_text: String,
        structured: Option<Value>,
    },
    /// `structured` carries a schema payload that arrived before the failure (e.g. a Claude
    /// result whose turn is failed because of a permission denial), for diagnostics only.
    TurnFailed {
        class: ErrorClass,
        detail: String,
        structured: Option<Value>,
    },
}

impl AgentEvent {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            AgentEvent::TurnCompleted { .. } | AgentEvent::TurnFailed { .. }
        )
    }
}

/// Plan §6.2: any permission denial fails the turn (`PermissionDenied`). The failure
/// detail every adapter reports for it, real or fake.
pub fn permission_denials_detail(count: usize) -> String {
    format!("{count} permission denial(s)")
}

/// How to open (or resume) an agent session for one stage.
#[derive(Debug, Clone)]
pub struct OpenSession {
    pub profile: String,
    pub role: Role,
    /// Working directory: the task worktree.
    pub cwd: PathBuf,
    /// Read-only artifact store for Planner/Reviewer (never passed to an Implementer, R49).
    pub run_store: Option<PathBuf>,
    /// Where the turn's prompt, argv, stream and outcome are captured.
    pub capture_dir: PathBuf,
    /// JSON schema text of the role's structured output.
    pub schema: Option<String>,
    /// Role context file (ProjectContextLoader output).
    pub context_file: Option<PathBuf>,
    pub model: Option<String>,
    /// External session id to resume (attempt n+1 of the same stage, correction turns).
    pub resume: Option<String>,
}

/// One turn's input.
#[derive(Debug, Clone, Default)]
pub struct TurnInput {
    pub prompt: String,
    /// Capture file prefix, e.g. `plan-1`, `implement`, `review-0-0`.
    pub label: String,
    /// Extra environment for this turn (R46: the Implementer's TEMP/TMP).
    pub env: BTreeMap<String, String>,
    /// Echo the stream to the console.
    pub echo: bool,
}

/// Bounds for `run_to_completion`.
#[derive(Debug, Clone, Copy)]
pub struct TurnLimits {
    /// Whole-turn maximum.
    pub max: Duration,
    /// No event for this long → interrupt (`Timeout`, stalled). Every stdout line of a
    /// real agent is at least a `Heartbeat`, so this measures silence, not mapped events;
    /// it does not run while the agent runs a command (`AgentEvent::CommandRunning`).
    pub stall: Duration,
}

impl Default for TurnLimits {
    fn default() -> Self {
        Self {
            max: Duration::from_secs(30 * 60),
            stall: Duration::from_secs(10 * 60),
        }
    }
}

/// What the engine needs to know about a finished turn — built only from `AgentEvent`s.
#[derive(Debug, Clone, Default, Serialize)]
pub struct StageOutcome {
    pub success: bool,
    pub session_id: Option<String>,
    pub final_text: String,
    pub structured: Option<Value>,
    pub error_class: Option<ErrorClass>,
    pub error_detail: Option<String>,
    pub usage: Option<TokenUsage>,
    pub permission_denials: Vec<(String, String)>,
    pub tool_activity: u32,
    pub retries: u32,
    pub duration_ms: u128,
    /// The turn was interrupted because of the stall or max limit.
    pub timed_out: bool,
    /// The turn was interrupted by a stop request (R38).
    pub cancelled: bool,
    pub label: String,
    pub capture_dir: Option<PathBuf>,
}

impl StageOutcome {
    /// Fold one event into the outcome.
    pub fn apply(&mut self, e: &AgentEvent) {
        match e {
            AgentEvent::SessionIdentified(id) => self.session_id = Some(id.clone()),
            AgentEvent::TextDelta(_) => {}
            AgentEvent::ToolActivity { .. } => self.tool_activity += 1,
            AgentEvent::PermissionDenied { tool, detail } => {
                self.permission_denials.push((tool.clone(), detail.clone()))
            }
            // the latest value replaces the earlier ones: live values are cumulative for
            // the turn and the terminal one is final, so nothing is counted twice
            AgentEvent::Usage(u) => self.usage = Some(*u),
            // liveness only: `run_to_completion` reads them for the stall limit
            AgentEvent::Heartbeat | AgentEvent::CommandRunning(_) => {}
            AgentEvent::Retrying { .. } => self.retries += 1,
            AgentEvent::TurnCompleted {
                final_text,
                structured,
            } => {
                self.success = true;
                self.final_text = final_text.clone();
                self.structured = structured.clone();
            }
            AgentEvent::TurnFailed {
                class,
                detail,
                structured,
            } => {
                self.success = false;
                self.error_class = Some(*class);
                self.error_detail = Some(detail.clone());
                if self.structured.is_none() {
                    self.structured = structured.clone();
                }
            }
        }
    }

    pub fn print_summary(&self) {
        eprintln!("── turn summary ({}) ──", self.label);
        eprintln!("success          : {}", self.success);
        eprintln!(
            "session id       : {}",
            self.session_id.as_deref().unwrap_or("-")
        );
        if let Some(c) = self.error_class {
            eprintln!(
                "error            : {c:?} — {}",
                self.error_detail.as_deref().unwrap_or("")
            );
        }
        eprintln!("permission denials: {}", self.permission_denials.len());
        match self.usage {
            Some(u) => eprintln!("usage ({:?})     : {:?}", u.accounting, u.buckets),
            None => eprintln!("usage            : none reported"),
        }
        eprintln!(
            "duration         : {:.1}s{}",
            self.duration_ms as f64 / 1000.0,
            if self.timed_out { " (timed out)" } else { "" }
        );
        if let Some(d) = &self.capture_dir {
            eprintln!("captured in      : {}", d.display());
        }
    }
}

/// Result of `AgentAdapter::health`.
#[derive(Debug, Clone, Serialize)]
pub struct AgentHealth {
    pub exe: String,
    pub version: Option<String>,
}
