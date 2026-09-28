//! Toryo — M0 spike kit (plan v0.6).
//!
//! Only the pieces M0 needs to answer its twelve `M0-VERIFY` questions live here:
//! process containment and the verify boundary, streaming redaction, the exact
//! Claude/Codex launch lines and stream parsers, the `GitCmd` builder with its
//! canaries, and `toryo doctor`. The pipeline itself (M1–M8) starts after M0.

pub mod agents;
pub mod config;
pub mod doctor;
pub mod git;
pub mod process;
pub mod spike;
pub mod usage;
