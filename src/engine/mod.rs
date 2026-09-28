//! The engine (plan §4): the pipeline state machine (M2). SQLite state, recovery and the
//! task lock follow in M3; the stage runners in M4–M6.

pub mod state;

pub use state::{
    Gate, GateKind, Machine, PipelineError, PipelineEvent, PipelineState, MAX_PLAN_AUTO_REJECTIONS,
};
