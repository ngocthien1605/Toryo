//! The engine (plan §4): the pipeline state machine (M2). SQLite state, recovery and the
//! task lock follow in M3; the stage runners in M4–M6.

pub mod checks;
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
