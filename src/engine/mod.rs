//! The engine (plan §4): the pipeline state machine (M2), SQLite state, recovery and the
//! task lock (M3), the ProjectContextLoader (M4) and the stage runners.

pub mod artifacts;
pub mod checks;
pub mod context;
pub mod contracts;
pub mod decisions;
pub mod flight;
pub mod lock;
pub mod protect;
pub mod recovery;
pub mod render;
pub mod runner;
pub mod snapshot;
pub mod state;
pub mod store;
pub mod task;
pub mod untrusted;
pub mod verify;
pub mod worktree;
pub mod xpp;

pub use state::{
    Gate, GateKind, Machine, PipelineError, PipelineEvent, PipelineState, MAX_PLAN_AUTO_REJECTIONS,
};
