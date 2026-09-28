//! M1 acceptance (plan §4, R52): "no vendor types outside adapters". The engine-side code
//! — domain types, the adapter contract, the hub, the command runner and the pipeline —
//! must not see Claude/Codex stream JSON, launch lines or parser state.

use std::path::Path;

/// Vendor internals that live only in `agents::{claude, codex, exec, events, cli}`.
const VENDOR_INTERNALS: &[&str] = &[
    "TurnOutcome",
    "TurnRun",
    "ClaudeParser",
    "CodexParser",
    "ClaudeLaunch",
    "CodexLaunch",
    "claude_turn",
    "codex_turn",
    "claude_implement_turn",
    "run_turn_with",
    "structured_output",
    "thread.started",
    "turn.completed",
];

fn check(file: &str, extra: &[&str]) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{file}: {e}"));
    for (n, line) in text.lines().enumerate() {
        let code = line.split("//").next().unwrap_or("");
        for bad in VENDOR_INTERNALS.iter().chain(extra) {
            assert!(
                !code.contains(bad),
                "{file}:{}: vendor type `{bad}` outside the adapters: {line}",
                n + 1
            );
        }
    }
}

#[test]
fn no_vendor_types_outside_adapters() {
    // not even the vendor modules themselves
    let modules = &["claude::", "codex::", "exec::", "events::"];
    check("src/domain.rs", modules);
    check("src/agents/adapter.rs", modules);
    check("src/runner.rs", modules);
    check("src/spike/pipeline.rs", modules);
    // the hub picks an adapter (and passes the Claude launch mode through), nothing more
    check("src/agents/hub.rs", &["exec::", "events::"]);
    check("src/agents/fake.rs", &["claude::", "codex::", "exec::"]);
}
