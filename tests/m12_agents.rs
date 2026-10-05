//! M12 agent-CLI tests (M12-14): a CLI that dies without a result shows its stderr.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use toryo::engine::PipelineState as St;

/// AC-1: a CLI that rejects a flag prints to stderr and exits non-zero — here `toryo` itself
/// stands in for the CLI (it is an executable on every platform, and it rejects the `claude`
/// flags with `error: unexpected argument …`). The CLI's stderr text is in the escalation
/// reason and on the console. (Was: `ProcessCrashed — no result event (exit Some(2), 0
/// malformed lines)` and nothing about why.)
#[test]
fn m12_14_a_cli_that_dies_without_a_result_shows_its_error_text() {
    let env = setup("codex-openai");
    let exe = env!("CARGO_BIN_EXE_toryo").replace(std::path::MAIN_SEPARATOR, "/");
    let m = std::fs::read_to_string(&env.machine).unwrap();
    std::fs::write(
        &env.machine,
        m.replace("exe: claude-not-installed", &format!("exe: '{exe}'")),
    )
    .unwrap();
    create(&env, "S-1");
    let (_, err) = toryo(&env, &["run", "S-1", "--yes", "--accept-unverified"], &[]);
    let t = task(&env, "S-1");
    assert_eq!(t.state, St::Escalated, "{err}");
    let why = t.machine.reason().unwrap_or_default().to_string();
    assert!(
        why.contains("no result event")
            && why.contains("the CLI's stderr (last lines)")
            && why.contains("error: unexpected argument"),
        "{why}"
    );
    assert!(!why.contains('\u{1b}'), "colour codes stripped: {why:?}");
    assert!(err.contains("error: unexpected argument"), "console: {err}");
}
