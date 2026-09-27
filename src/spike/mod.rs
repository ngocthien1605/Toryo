//! `thien spike …` — the M0 spike commands (plan §17). Each one answers one or more
//! `M0-VERIFY` items against the real CLIs / OS on this machine and records the
//! evidence in `<data_dir>/m0/results.jsonl`.

pub mod agents;
pub mod fixture;
pub mod job;
pub mod lowil;
pub mod render;
pub mod results;
pub mod smoke;
pub mod turn;
