//! Agent launch lines and stream parsers used by the M0 spike (plan §6).
//!
//! These are the pieces M1's `ClaudeOneShotAdapter` / `CodexExecAdapter` will
//! wrap; in M0 they are driven by `toryo spike …` so the real CLIs can be
//! exercised and their output captured as fixtures.

pub mod adapter;
pub mod claude;
pub mod cli;
pub mod codex;
pub mod events;
pub mod exec;
pub mod fake;
pub mod hub;

use serde::Serialize;
use serde_json::Value;

use crate::usage::{Accounting, Buckets};

pub use crate::domain::ErrorClass;

#[derive(Debug, Clone, Serialize, Default)]
pub struct CommandExec {
    pub command: String,
    pub status: Option<String>,
    pub exit_code: Option<i64>,
}

/// Everything the spike needs to judge one turn.
#[derive(Debug, Clone, Serialize)]
pub struct TurnOutcome {
    pub success: bool,
    pub session_id: Option<String>,
    pub structured: Option<Value>,
    pub final_text: String,
    pub error_class: Option<ErrorClass>,
    pub error_detail: Option<String>,
    pub retryable: bool,
    pub permission_denials: Vec<Value>,
    pub usage: Option<Buckets>,
    pub raw_usage: Option<Value>,
    pub accounting: Accounting,
    pub vendor_cost_usd: Option<f64>,
    pub protocol_errors: Vec<String>,
    /// Error notices the agent printed (Codex `error` lines, e.g. "Reconnecting... 1/5"),
    /// diagnostics only: the turn's own end decides the outcome (EC-02).
    pub warnings: Vec<String>,
    pub malformed_lines: u32,
    pub oversized_lines: u32,
    pub tool_activity: Vec<String>,
    pub commands: Vec<CommandExec>,
    /// Claude `system/init` event (tools, MCP servers, memory files … — evidence for M0-VERIFY 2).
    pub init: Option<Value>,
    pub retries: Vec<Value>,
}

impl TurnOutcome {
    pub fn empty() -> Self {
        Self {
            success: false,
            session_id: None,
            structured: None,
            final_text: String::new(),
            error_class: None,
            error_detail: None,
            retryable: false,
            permission_denials: Vec::new(),
            usage: None,
            raw_usage: None,
            accounting: Accounting::Unknown,
            vendor_cost_usd: None,
            protocol_errors: Vec::new(),
            warnings: Vec::new(),
            malformed_lines: 0,
            oversized_lines: 0,
            tool_activity: Vec::new(),
            commands: Vec::new(),
            init: None,
            retries: Vec::new(),
        }
    }
}

/// JSONL framing over redacted chunks, with the plan's 32 MB per-line sanity cap.
pub struct LineSplitter {
    buf: Vec<u8>,
    cap: usize,
    dropping: bool,
    pub oversized: u32,
}

impl LineSplitter {
    pub const DEFAULT_CAP: usize = 32 * 1024 * 1024;

    pub fn new(cap: usize) -> Self {
        Self {
            buf: Vec::new(),
            cap,
            dropping: false,
            oversized: 0,
        }
    }

    /// Feed bytes; returns complete lines (without the trailing `\n` / `\r\n`).
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for &b in chunk {
            if b == b'\n' {
                if self.dropping {
                    self.dropping = false;
                } else {
                    let mut line = std::mem::take(&mut self.buf);
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    out.push(line);
                }
                self.buf.clear();
                continue;
            }
            if self.dropping {
                continue;
            }
            self.buf.push(b);
            if self.buf.len() > self.cap {
                self.buf.clear();
                self.dropping = true;
                self.oversized += 1;
            }
        }
        out
    }

    pub fn finish(&mut self) -> Option<Vec<u8>> {
        if self.dropping || self.buf.is_empty() {
            return None;
        }
        Some(std::mem::take(&mut self.buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_crlf_and_caps() {
        let mut s = LineSplitter::new(8);
        let lines = s.push(b"one\r\ntwo\nthree-is-too-long\nfo");
        assert_eq!(lines, vec![b"one".to_vec(), b"two".to_vec()]);
        assert_eq!(s.oversized, 1);
        assert_eq!(s.push(b"ur\n"), vec![b"four".to_vec()]);
        assert_eq!(s.push(b"tail"), Vec::<Vec<u8>>::new());
        assert_eq!(s.finish(), Some(b"tail".to_vec()));
    }
}
