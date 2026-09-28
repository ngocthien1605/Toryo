//! Turning vendor stream lines into `AgentEvent`s (the only place, besides the parsers,
//! that knows Claude/Codex JSON). Used live by the CLI adapters and by the replay mode of
//! `FakeAgentAdapter`, so recorded real streams go through exactly the same code.

use serde_json::Value;

use super::claude::{classify_api_error, ClaudeParser};
use super::codex::CodexParser;
use super::TurnOutcome;
use crate::config::ProfileKind;
use crate::domain::{AgentEvent, ErrorClass, TokenUsage};
use crate::usage::{Accounting, CodexSubsets};

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// Live events for one stream line (non-terminal events only; the terminal ones come from
/// the parser's final outcome, see `terminal_events`).
pub fn line_events(kind: ProfileKind, line: &[u8]) -> Vec<AgentEvent> {
    let Ok(v) = serde_json::from_slice::<Value>(line) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match kind {
        ProfileKind::Claude => match v["type"].as_str() {
            Some("system") => match v["subtype"].as_str() {
                Some("init") => {
                    if let Some(id) = v["session_id"].as_str() {
                        out.push(AgentEvent::SessionIdentified(id.to_string()));
                    }
                }
                Some("api_retry") => out.push(AgentEvent::Retrying {
                    attempt: v["attempt"].as_u64().unwrap_or(0) as u32,
                    delay_ms: v["retry_delay_ms"].as_u64().unwrap_or(0),
                    class: classify_api_error(v["error"].as_str().unwrap_or("")),
                }),
                _ => {}
            },
            Some("stream_event") => {
                if let Some(t) = v.pointer("/event/delta/text").and_then(Value::as_str) {
                    out.push(AgentEvent::TextDelta(t.to_string()));
                }
            }
            Some("assistant") => {
                for block in v
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if block["type"] == "tool_use" {
                        out.push(AgentEvent::ToolActivity {
                            name: block["name"].as_str().unwrap_or("?").to_string(),
                            summary: short(&block["input"].to_string(), 200),
                        });
                    }
                }
            }
            _ => {}
        },
        ProfileKind::Codex => match v["type"].as_str() {
            Some("thread.started") => {
                if let Some(id) = v["thread_id"].as_str() {
                    out.push(AgentEvent::SessionIdentified(id.to_string()));
                }
            }
            Some("item.completed") => {
                let item = &v["item"];
                match item["type"].as_str() {
                    Some("command_execution") => out.push(AgentEvent::ToolActivity {
                        name: "command".into(),
                        summary: short(
                            &format!(
                                "{} → exit {}",
                                item["command"].as_str().unwrap_or(""),
                                item["exit_code"]
                            ),
                            200,
                        ),
                    }),
                    Some("file_change") => out.push(AgentEvent::ToolActivity {
                        name: "file_change".into(),
                        summary: short(
                            &item["changes"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|c| c["path"].as_str())
                                .collect::<Vec<_>>()
                                .join(", "),
                            200,
                        ),
                    }),
                    Some(other) if other != "agent_message" && other != "reasoning" => {
                        out.push(AgentEvent::ToolActivity {
                            name: other.to_string(),
                            summary: String::new(),
                        })
                    }
                    _ => {}
                }
            }
            _ => {}
        },
    }
    out
}

/// Terminal events derived from the parser's outcome: permission denials, usage, then
/// exactly one `TurnCompleted` / `TurnFailed`.
pub fn terminal_events(o: &TurnOutcome) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    for d in &o.permission_denials {
        out.push(AgentEvent::PermissionDenied {
            tool: d["tool_name"].as_str().unwrap_or("?").to_string(),
            detail: short(&d["tool_input"].to_string(), 300),
        });
    }
    out.push(AgentEvent::Usage(TokenUsage {
        buckets: o.usage,
        accounting: if o.usage.is_none() {
            Accounting::Unknown
        } else {
            o.accounting
        },
    }));
    if o.success {
        out.push(AgentEvent::TurnCompleted {
            final_text: o.final_text.clone(),
            structured: o.structured.clone(),
        });
    } else {
        out.push(AgentEvent::TurnFailed {
            class: o.error_class.unwrap_or(ErrorClass::Unknown),
            detail: o.error_detail.clone().unwrap_or_default(),
            structured: o.structured.clone(),
        });
    }
    out
}

/// Replay a recorded stream through the real parser: the events a live turn would produce.
pub fn replay(kind: ProfileKind, stream: &str, exit_code: Option<i32>) -> Vec<AgentEvent> {
    let mut claude = ClaudeParser::new();
    let mut codex = CodexParser::new();
    let mut out = Vec::new();
    for line in stream.lines().filter(|l| !l.trim().is_empty()) {
        out.extend(line_events(kind, line.as_bytes()));
        match kind {
            ProfileKind::Claude => claude.feed_line(line.as_bytes()),
            ProfileKind::Codex => codex.feed_line(line.as_bytes()),
        }
    }
    let outcome = match kind {
        ProfileKind::Claude => claude.finish(exit_code),
        ProfileKind::Codex => codex.finish(exit_code, CodexSubsets::default()),
    };
    out.extend(terminal_events(&outcome));
    out
}
