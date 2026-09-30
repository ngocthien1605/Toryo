//! Toryo (棟梁) — orchestrates Claude Code and Codex (plan v0.7).
//!
//! Core (M1): vendor-neutral domain types (`domain`), the agent adapter layer (`agents`:
//! `AgentAdapter`/`AgentSession`, the Claude/Codex CLI adapters, `FakeAgentAdapter`,
//! `AgentHub`), `CommandRunner` (`runner`), process containment and redaction
//! (`process`), `GitCmd` + canaries (`git`), usage buckets, config, version-bound
//! evidence (`evidence`, R51) and `doctor`.
//!
//! `spike` (feature `m0-kit`, default on) is the M0 kit, including the single-process
//! pipeline prototype — the behavioural reference for M2–M8 (R52).

pub mod agents;
pub mod config;
pub mod doctor;
pub mod domain;
pub mod engine;
pub mod evidence;
pub mod git;
pub mod notify;
pub mod process;
pub mod runner;
#[cfg(feature = "m0-kit")]
pub mod spike;
pub mod usage;
