//! The pipeline state machine (plan §5.2; R4, R5, R14, R29, R32, R38, R40, R44a).
//!
//! Pure: no processes, files or database. The engine (and, since M2, the `spike pipeline`
//! prototype) asks [`Machine::apply`] before every step; M3 persists each transition in the
//! same SQLite transaction as the stage result.
//!
//! ```text
//! New → Planning → WaitingPlanApproval ─reject→ Planning (attempt n+1)       | Cancelled
//!                        │approve
//!                        ▼
//!                  Implementing → Checkpointing → Verifying ─fail→ Fixing → Checkpointing
//!                                                    │pass
//!                                                    ▼
//!                                                Reviewing ─changes_requested→ Fixing
//!                                                    │approved          │max cycles / blocked
//!                                                    ▼                  ▼
//!                                         WaitingFinalApproval      Escalated ─resume --note→ Fixing
//!                                                    │approve / reject          │cancel → Cancelled
//!                                                    ▼                          │approve --gate final --force
//!                                              Completed / Fixing               ▼ → WaitingFinalApproval
//! Any running state ─budget exceeded→ Paused ─budget set + resume→ previous state | Cancelled
//! Any state ─unrecoverable error→ Failed
//! ```
//!
//! Owner decisions (2026-09-28, M2):
//! - human plan rejections are unlimited (the human can cancel at any gate); an automatic
//!   plan rejection (plan touches protected paths) re-plans at most
//!   [`MAX_PLAN_AUTO_REJECTIONS`] times, then `Escalated`;
//! - an Implementer path-policy violation is a fix cycle (`Fixing`, bounded by
//!   `max_fix_cycles`) like a failed verify or requested changes.
//!
//! `stop` (R38) is deliberately **not** an event: it interrupts the running stage and leaves
//! the state unchanged; `resume` runs attempt n+1 of the same stage.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Automatic plan rejections (protected paths in `files_likely_involved`) before `Escalated`.
pub const MAX_PLAN_AUTO_REJECTIONS: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PipelineState {
    New,
    Planning,
    WaitingPlanApproval,
    Implementing,
    Checkpointing,
    Verifying,
    Fixing,
    Reviewing,
    WaitingFinalApproval,
    Escalated,
    Paused,
    Completed,
    Cancelled,
    Failed,
}

impl PipelineState {
    pub const ALL: [PipelineState; 14] = [
        PipelineState::New,
        PipelineState::Planning,
        PipelineState::WaitingPlanApproval,
        PipelineState::Implementing,
        PipelineState::Checkpointing,
        PipelineState::Verifying,
        PipelineState::Fixing,
        PipelineState::Reviewing,
        PipelineState::WaitingFinalApproval,
        PipelineState::Escalated,
        PipelineState::Paused,
        PipelineState::Completed,
        PipelineState::Cancelled,
        PipelineState::Failed,
    ];

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }

    /// Waiting for a human (R5): gates, `Escalated`, `Paused`.
    pub fn is_waiting(self) -> bool {
        matches!(
            self,
            Self::WaitingPlanApproval | Self::WaitingFinalApproval | Self::Escalated | Self::Paused
        )
    }

    /// A stage runs in this state (an agent turn, a checkpoint or a verify run).
    pub fn is_running(self) -> bool {
        matches!(
            self,
            Self::Planning
                | Self::Implementing
                | Self::Checkpointing
                | Self::Verifying
                | Self::Fixing
                | Self::Reviewing
        )
    }

    /// The Implementer writes to the worktree in this state.
    pub fn is_writing(self) -> bool {
        matches!(self, Self::Implementing | Self::Fixing)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateKind {
    Plan,
    Final,
}

/// One gate occurrence (R44a): opened on every entry into a waiting-for-approval state,
/// closed by the decision. `subject` is what is being approved: the plan digest (R32) or
/// the checkpoint SHA.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate {
    pub kind: GateKind,
    pub occurrence: u32,
    pub subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum PipelineEvent {
    /// `New → Planning`.
    Start,
    /// The plan lists protected paths: re-plan (bounded), no gate is opened.
    PlanPolicyViolation {
        paths: Vec<String>,
        /// M11-D2: what is wrong when it is not protected paths — skills off the implementer's
        /// menu, or over its context budget. Absent (and not serialised) for paths.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// A valid plan exists: opens a plan gate occurrence.
    PlanReady {
        subject: String,
    },
    PlanApproved {
        occurrence: u32,
        subject: String,
    },
    PlanRejected {
        occurrence: u32,
        reason: String,
    },
    /// The Implementer turn ended with a tree that passes the path policy.
    ImplementationDone,
    /// The Implementer changed protected paths: restored, nothing committed → fix cycle.
    PathPolicyViolation {
        paths: Vec<String>,
    },
    /// Checkpoint commit made (possibly an empty one, R40).
    Checkpointed {
        head: String,
    },
    VerifyPassed {
        head: String,
    },
    VerifyFailed {
        head: String,
    },
    ReviewApproved {
        head: String,
    },
    ChangesRequested {
        head: String,
    },
    ReviewBlocked {
        head: String,
    },
    FinalApproved {
        occurrence: u32,
        subject: String,
    },
    FinalRejected {
        occurrence: u32,
        reason: String,
    },
    /// `toryo resume --note` from `Escalated`.
    ResumeWithNote {
        note: String,
    },
    /// `toryo approve --gate final --force` from `Escalated`.
    ForceFinal,
    /// Back from `Escalated` into the stage that escalated, without a new fix cycle — after
    /// the human resolved a recovery stop (`resume --discard-partial | --keep-partial`, §10).
    ResumeStage,
    BudgetExceeded,
    /// A running stage cannot go on without a human, but nothing is wrong with the work:
    /// the implementer chain or the Planner/Reviewer is out of quota, `--no-fallback` (R49).
    Pause {
        reason: String,
    },
    /// `toryo resume` from `Paused` (after `budget set`, or once the quota is back).
    Unpause,
    /// The verify result of the current checkpoint is no longer current (`task config
    /// --reload` changed the verify configuration, R35): the checkpoint is verified again —
    /// from the final gate, `Escalated`, or `Reviewing` before the review starts (EC-10).
    Reverify,
    /// A stop that needs a human (max cycles, InvalidOutput, quota, tampering, …).
    Escalate {
        reason: String,
    },
    Cancel,
    /// Unrecoverable error.
    Fail {
        error: String,
    },
}

impl PipelineEvent {
    pub fn name(&self) -> &'static str {
        use PipelineEvent as E;
        match self {
            E::Start => "start",
            E::PlanPolicyViolation { .. } => "plan_policy_violation",
            E::PlanReady { .. } => "plan_ready",
            E::PlanApproved { .. } => "plan_approved",
            E::PlanRejected { .. } => "plan_rejected",
            E::ImplementationDone => "implementation_done",
            E::PathPolicyViolation { .. } => "path_policy_violation",
            E::Checkpointed { .. } => "checkpointed",
            E::VerifyPassed { .. } => "verify_passed",
            E::VerifyFailed { .. } => "verify_failed",
            E::ReviewApproved { .. } => "review_approved",
            E::ChangesRequested { .. } => "changes_requested",
            E::ReviewBlocked { .. } => "review_blocked",
            E::FinalApproved { .. } => "final_approved",
            E::FinalRejected { .. } => "final_rejected",
            E::ResumeWithNote { .. } => "resume_with_note",
            E::ForceFinal => "force_final",
            E::ResumeStage => "resume_stage",
            E::BudgetExceeded => "budget_exceeded",
            E::Pause { .. } => "pause",
            E::Unpause => "unpause",
            E::Reverify => "reverify",
            E::Escalate { .. } => "escalate",
            E::Cancel => "cancel",
            E::Fail { .. } => "fail",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PipelineError {
    #[error("invalid transition: `{event}` in state {state:?}")]
    InvalidTransition {
        state: PipelineState,
        event: &'static str,
    },
    #[error("the task is {0:?}: no further transitions")]
    Terminal(PipelineState),
    #[error("gate occurrence {got} is not the open {kind:?} gate (open: {open:?}); a resolved occurrence never passes (R44a)")]
    StaleOccurrence {
        kind: GateKind,
        got: u32,
        open: Option<u32>,
    },
    #[error("approval is for `{got}` but the gate is for `{expected}` (R32)")]
    SubjectMismatch { expected: String, got: String },
    #[error("{what} is for {got}, but the current checkpoint is {checkpoint:?} (R29)")]
    StaleHead {
        what: &'static str,
        got: String,
        checkpoint: Option<String>,
    },
    #[error("the checkpoint {checkpoint:?} has no passing verify (verified: {verified:?}) (R29)")]
    NotVerified {
        checkpoint: Option<String>,
        verified: Option<String>,
    },
    #[error("leaving Escalated needs a non-empty note (R5)")]
    EmptyNote,
}

/// The state plus what the transition guards need. Serializable: M3 stores it with the task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    state: PipelineState,
    max_fix_cycles: u32,
    /// Increments on **every** entry into `Fixing`; `max_fix_cycles` is the single bound.
    fix_cycle: u32,
    /// Planner attempts (1-based once started).
    plan_attempt: u32,
    plan_auto_rejections: u32,
    plan_approved: bool,
    /// Gate occurrences opened so far (plan and final share the counter).
    occurrences: u32,
    gate: Option<Gate>,
    /// Current checkpoint SHA and the head of the latest passing verify.
    checkpoint: Option<String>,
    verified_head: Option<String>,
    paused_from: Option<PipelineState>,
    escalated_from: Option<PipelineState>,
    /// Why the task is `Escalated` / `Failed`, or what the last fix cycle is about.
    reason: Option<String>,
    /// M11-C4: the split bounds of an org chart's `limits:` — then a failing gate counts
    /// against `max_gate_retries` and a rejected review against `max_review_rounds`, and
    /// `max_fix_cycles` bounds nothing. Absent (and not serialised) for the single bound, so a
    /// machine stored by M10 reads back and behaves exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    split: Option<SplitLimits>,
}

/// The split retry bounds and their counters (M11-C4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitLimits {
    pub max_gate_retries: u32,
    pub max_review_rounds: u32,
    /// Entries into `Fixing` after a failing gate (or a path-policy violation).
    #[serde(default)]
    pub gate_retries: u32,
    /// Entries into `Fixing` after a rejected review (or a rejected final gate).
    #[serde(default)]
    pub review_rounds: u32,
}

/// What sent the work back to the implementer (M11-C4: which split counter it uses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bounce {
    /// A failing gate, or the implementer's own path-policy violation (the build stage).
    Gate,
    /// The reviewer's changes requested, or the human's final rejection (the review stage).
    Review,
}

impl Machine {
    /// M11-C4: split bounds — `max_gate_retries` and `max_review_rounds`. `max_fix_cycles()`
    /// then reports their sum (the most fix cycles the bounds allow), for display only.
    pub fn with_limits(max_gate_retries: u32, max_review_rounds: u32) -> Self {
        Self {
            split: Some(SplitLimits {
                max_gate_retries,
                max_review_rounds,
                gate_retries: 0,
                review_rounds: 0,
            }),
            ..Self::new(max_gate_retries.saturating_add(max_review_rounds))
        }
    }

    /// The split bounds and counters, when the task uses them (M11-C4).
    pub fn split_limits(&self) -> Option<&SplitLimits> {
        self.split.as_ref()
    }

    pub fn new(max_fix_cycles: u32) -> Self {
        Self {
            state: PipelineState::New,
            max_fix_cycles,
            fix_cycle: 0,
            plan_attempt: 0,
            plan_auto_rejections: 0,
            plan_approved: false,
            occurrences: 0,
            gate: None,
            checkpoint: None,
            verified_head: None,
            paused_from: None,
            escalated_from: None,
            reason: None,
            split: None,
        }
    }

    pub fn state(&self) -> PipelineState {
        self.state
    }
    pub fn fix_cycle(&self) -> u32 {
        self.fix_cycle
    }
    pub fn max_fix_cycles(&self) -> u32 {
        self.max_fix_cycles
    }
    pub fn plan_attempt(&self) -> u32 {
        self.plan_attempt
    }
    pub fn plan_approved(&self) -> bool {
        self.plan_approved
    }
    pub fn gate(&self) -> Option<&Gate> {
        self.gate.as_ref()
    }
    pub fn checkpoint(&self) -> Option<&str> {
        self.checkpoint.as_deref()
    }
    pub fn verified_head(&self) -> Option<&str> {
        self.verified_head.as_deref()
    }
    pub fn paused_from(&self) -> Option<PipelineState> {
        self.paused_from
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
    pub fn escalated_from(&self) -> Option<PipelineState> {
        self.escalated_from
    }

    /// Guard for a stage runner: the task must be in `s` (e.g. no Implementer turn before
    /// the plan is approved).
    pub fn expect(&self, s: PipelineState) -> Result<(), PipelineError> {
        if self.state == s {
            Ok(())
        } else {
            Err(PipelineError::InvalidTransition {
                state: self.state,
                event: "run_stage",
            })
        }
    }

    /// Apply `e`; on error nothing changes.
    pub fn apply(&mut self, e: PipelineEvent) -> Result<PipelineState, PipelineError> {
        *self = self.transition(&e)?;
        Ok(self.state)
    }

    /// The single transition function (plan §5.2): everything else calls it.
    pub fn transition(&self, e: &PipelineEvent) -> Result<Machine, PipelineError> {
        use PipelineEvent as E;
        use PipelineState as S;
        if self.state.is_terminal() {
            return Err(PipelineError::Terminal(self.state));
        }
        let invalid = PipelineError::InvalidTransition {
            state: self.state,
            event: e.name(),
        };
        let mut m = self.clone();
        match (self.state, e) {
            // ── any non-terminal state ───────────────────────────────────────────────
            (_, E::Fail { error }) => {
                m.gate = None;
                m.reason = Some(error.clone());
                m.state = S::Failed;
            }
            (_, E::Cancel) => {
                m.gate = None;
                m.state = S::Cancelled;
            }
            (s, E::Escalate { reason }) if s != S::Escalated => m.escalate(s, reason.clone()),
            (s, E::BudgetExceeded) if s.is_running() => {
                m.paused_from = Some(s);
                m.reason = Some("budget".into());
                m.state = S::Paused;
            }
            (s, E::Pause { reason }) if s.is_running() => {
                m.paused_from = Some(s);
                m.reason = Some(reason.clone());
                m.state = S::Paused;
            }
            (S::Paused, E::Unpause) => {
                m.state = m.paused_from.take().ok_or(invalid)?;
            }

            // ── plan + plan gate ─────────────────────────────────────────────────────
            (S::New, E::Start) => {
                m.plan_attempt = 1;
                m.state = S::Planning;
            }
            (S::Planning, E::PlanPolicyViolation { paths, reason }) => {
                if m.plan_auto_rejections >= MAX_PLAN_AUTO_REJECTIONS {
                    let why = match reason {
                        Some(r) => format!(
                            "{r} — still, after {MAX_PLAN_AUTO_REJECTIONS} automatic re-plans"
                        ),
                        None => format!(
                            "the plan still lists protected paths {paths:?} after {MAX_PLAN_AUTO_REJECTIONS} automatic re-plans"
                        ),
                    };
                    m.escalate(S::Planning, why);
                } else {
                    m.plan_auto_rejections += 1;
                    m.plan_attempt += 1;
                }
            }
            (S::Planning, E::PlanReady { subject }) => {
                m.open_gate(GateKind::Plan, subject.clone());
                m.state = S::WaitingPlanApproval;
            }
            (
                S::WaitingPlanApproval,
                E::PlanApproved {
                    occurrence,
                    subject,
                },
            ) => {
                m.close_gate(GateKind::Plan, *occurrence, Some(subject))?;
                m.plan_approved = true;
                m.state = S::Implementing;
            }
            (S::WaitingPlanApproval, E::PlanRejected { occurrence, reason }) => {
                m.close_gate(GateKind::Plan, *occurrence, None)?;
                m.plan_attempt += 1;
                m.plan_auto_rejections = 0;
                m.reason = Some(reason.clone());
                m.state = S::Planning;
            }

            // ── implement / fix → checkpoint → verify ────────────────────────────────
            (S::Implementing | S::Fixing, E::ImplementationDone) => m.state = S::Checkpointing,
            (S::Implementing | S::Fixing, E::PathPolicyViolation { paths }) => {
                m.enter_fixing(format!("protected paths changed: {paths:?}"), Bounce::Gate)
            }
            (S::Checkpointing, E::Checkpointed { head }) => {
                // R40: a no-change turn still produces a checkpoint, so this is the only way on
                m.checkpoint = Some(head.clone());
                m.state = S::Verifying;
            }
            (S::Verifying, E::VerifyPassed { head }) => {
                m.require_checkpoint("verify result", head)?;
                m.verified_head = Some(head.clone());
                m.state = S::Reviewing;
            }
            (S::Verifying, E::VerifyFailed { head }) => {
                m.require_checkpoint("verify result", head)?;
                // never to Review (§5.2)
                m.enter_fixing("verify failed".into(), Bounce::Gate);
            }

            // ── review + final gate ──────────────────────────────────────────────────
            (S::Reviewing, E::ReviewApproved { head }) => {
                m.require_checkpoint("review", head)?;
                m.require_verified()?;
                m.open_gate(GateKind::Final, head.clone());
                m.state = S::WaitingFinalApproval;
            }
            (S::Reviewing, E::ChangesRequested { head }) => {
                m.require_checkpoint("review", head)?;
                m.enter_fixing("changes requested".into(), Bounce::Review);
            }
            (S::Reviewing, E::ReviewBlocked { head }) => {
                m.require_checkpoint("review", head)?;
                m.escalate(S::Reviewing, "the reviewer blocked the task".into());
            }
            (
                S::WaitingFinalApproval,
                E::FinalApproved {
                    occurrence,
                    subject,
                },
            ) => {
                m.close_gate(GateKind::Final, *occurrence, Some(subject))?;
                m.require_checkpoint("final approval", subject)?;
                m.require_verified()?;
                m.state = S::Completed;
            }
            (S::WaitingFinalApproval, E::FinalRejected { occurrence, reason }) => {
                m.close_gate(GateKind::Final, *occurrence, None)?;
                m.enter_fixing(format!("final gate rejected: {reason}"), Bounce::Review);
            }

            // ── leaving Escalated (R5) ───────────────────────────────────────────────
            (S::Escalated, E::ResumeWithNote { note }) => {
                if note.trim().is_empty() {
                    return Err(PipelineError::EmptyNote);
                }
                m.reason = Some(note.clone());
                m.escalated_from = None;
                if m.plan_approved {
                    // the human grants one more cycle: no bound check here
                    m.fix_cycle += 1;
                    m.state = S::Fixing;
                } else {
                    // escalated before the plan was approved: never implement without a plan
                    m.plan_attempt += 1;
                    m.plan_auto_rejections = 0;
                    m.state = S::Planning;
                }
            }
            (S::Escalated, E::ResumeStage) => {
                let back = m
                    .escalated_from
                    .filter(|s| s.is_running())
                    .ok_or(invalid.clone())?;
                if back != S::Planning && !m.plan_approved {
                    return Err(invalid);
                }
                m.escalated_from = None;
                m.state = back;
            }
            (S::WaitingFinalApproval | S::Escalated | S::Reviewing, E::Reverify) => {
                if !m.plan_approved || m.checkpoint.is_none() {
                    return Err(invalid);
                }
                m.gate = None;
                m.verified_head = None;
                m.escalated_from = None;
                m.state = S::Verifying;
            }
            (S::Escalated, E::ForceFinal) => {
                if !m.plan_approved {
                    return Err(invalid);
                }
                m.require_verified()?;
                let head = m.checkpoint.clone().unwrap_or_default();
                m.escalated_from = None;
                m.open_gate(GateKind::Final, head);
                m.state = S::WaitingFinalApproval;
            }
            _ => return Err(invalid),
        }
        Ok(m)
    }

    // ── helpers (operate on the copy inside `transition`) ─────────────────────────────

    fn escalate(&mut self, from: PipelineState, reason: String) {
        self.gate = None;
        self.paused_from = None;
        self.escalated_from = Some(from);
        self.reason = Some(reason);
        self.state = PipelineState::Escalated;
    }

    fn enter_fixing(&mut self, cause: String, bounce: Bounce) {
        // M11-C4: the split bound of this kind of bounce, named in the escalation
        if let Some(l) = &mut self.split {
            let (used, max, what, key) = match bounce {
                Bounce::Gate => (
                    &mut l.gate_retries,
                    l.max_gate_retries,
                    "gate retries",
                    "limits.max_gate_retries",
                ),
                Bounce::Review => (
                    &mut l.review_rounds,
                    l.max_review_rounds,
                    "review rounds",
                    "limits.max_review_rounds",
                ),
            };
            if *used >= max {
                let reason = format!("{used} {what} used ({key}) — not accepted (last: {cause})");
                let from = self.state;
                self.escalate(from, reason);
            } else {
                *used += 1;
                self.fix_cycle += 1;
                self.reason = Some(cause);
                self.state = PipelineState::Fixing;
            }
            return;
        }
        if self.fix_cycle >= self.max_fix_cycles {
            let from = self.state;
            self.escalate(
                from,
                format!(
                    "not accepted after {} fix cycle(s) (last: {cause})",
                    self.max_fix_cycles
                ),
            );
        } else {
            self.fix_cycle += 1;
            self.reason = Some(cause);
            self.state = PipelineState::Fixing;
        }
    }

    fn open_gate(&mut self, kind: GateKind, subject: String) {
        self.occurrences += 1;
        self.gate = Some(Gate {
            kind,
            occurrence: self.occurrences,
            subject,
        });
    }

    /// R44a: only the open occurrence of the right gate can be decided; R32: an approval
    /// names the subject it approves.
    fn close_gate(
        &mut self,
        kind: GateKind,
        occurrence: u32,
        subject: Option<&String>,
    ) -> Result<(), PipelineError> {
        let open = self.gate.as_ref().filter(|g| g.kind == kind);
        match open {
            Some(g) if g.occurrence == occurrence => {
                if let Some(s) = subject {
                    if *s != g.subject {
                        return Err(PipelineError::SubjectMismatch {
                            expected: g.subject.clone(),
                            got: s.clone(),
                        });
                    }
                }
                self.gate = None;
                Ok(())
            }
            _ => Err(PipelineError::StaleOccurrence {
                kind,
                got: occurrence,
                open: open.map(|g| g.occurrence),
            }),
        }
    }

    fn require_checkpoint(&self, what: &'static str, head: &str) -> Result<(), PipelineError> {
        if self.checkpoint.as_deref() == Some(head) {
            Ok(())
        } else {
            Err(PipelineError::StaleHead {
                what,
                got: head.to_string(),
                checkpoint: self.checkpoint.clone(),
            })
        }
    }

    /// R29: review and the final gate need a passing verify of the current checkpoint.
    fn require_verified(&self) -> Result<(), PipelineError> {
        if self.checkpoint.is_some() && self.verified_head == self.checkpoint {
            Ok(())
        } else {
            Err(PipelineError::NotVerified {
                checkpoint: self.checkpoint.clone(),
                verified: self.verified_head.clone(),
            })
        }
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
