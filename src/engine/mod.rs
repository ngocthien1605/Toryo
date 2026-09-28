//! The engine (plan §4): the pipeline state machine (M2), SQLite state, recovery and the
//! task lock (M3), the ProjectContextLoader (M4) and the stage runners.

pub mod checks;
pub mod context;
pub mod contracts;
pub mod state;
pub mod lock;
pub mod recovery;
pub mod snapshot;
pub mod render;
pub mod runner;
pub mod task;
pub mod worktree;
pub mod store;

pub use state::{
    Gate, GateKind, Machine, PipelineError, PipelineEvent, PipelineState, MAX_PLAN_AUTO_REJECTIONS,
};
