//! `toryo spike …` — the M0 spike commands (plan §17). Each one answers one or more
//! `M0-VERIFY` items against the real CLIs / OS on this machine and records the
//! evidence in `<data_dir>/m0/results.jsonl`.

pub mod agents;
pub mod fixture;
pub mod init_config;
pub mod job;
pub mod lowil;
pub mod pipeline;
pub mod smoke;
// Moved to the core in M1 (R52); kept as aliases for the spike code.
pub use crate::agents::exec as turn;
pub use crate::engine::render;
pub use crate::evidence as results;
