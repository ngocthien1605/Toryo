//! M9 group A — trust boundaries (plan v0.8 R60, R61), end to end through the `toryo`
//! binary with fake agents. The unit-level checks live next to the code (`config.rs`,
//! `engine/context.rs`, `engine/runner.rs`).
#![cfg(feature = "m0-kit")]

mod common;

use common::*;
use toryo::engine::PipelineState as St;

/// R60 AC-1/AC-3 (`profiles_machine_only`, plan §16): a repository `toryo.yaml` that
/// defines profiles does not load — `task create` and `doctor` say so and print the block to
/// move — while a task frozen before the change keeps running; its `--reload` is refused with
/// the same message until the file is fixed.
#[test]
fn profiles_machine_only() {
    let env = setup("codex-openai");
    create(&env, "R-1");
    let s = script(
        &env,
        "r1.json",
        serde_json::json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "R-1", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "R-1").state, St::WaitingPlanApproval, "{err}");

    // someone commits the profiles into the repository file (the pre-R60 layout)
    let good = std::fs::read_to_string(&env.config).unwrap();
    std::fs::write(&env.config, format!("{good}{MACHINE_PROFILES}")).unwrap();

    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "R-2"],
        &[],
    );
    assert_ne!(code, 0);
    assert!(
        err.contains("`profiles:` may not be defined in the repository config (R60"),
        "{err}"
    );
    assert!(
        err.contains(&env.machine.display().to_string()),
        "names the machine config: {err}"
    );
    assert!(
        err.contains("\nprofiles:\n  claude-max:\n"),
        "carries the block: {err}"
    );

    let (code, err) = toryo(&env, &["task", "config", "R-1", "--reload"], &[]);
    assert_ne!(code, 0, "{err}");
    assert!(
        err.contains("may not be defined in the repository config (R60"),
        "{err}"
    );

    let o = command(&env, &["doctor"], &[]).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        text.contains("may not be defined in the repository config"),
        "{text}"
    );

    // the frozen task is unaffected: it runs to its final gate on its frozen profiles
    let (code, err) = toryo(
        &env,
        &[
            "approve",
            "R-1",
            "--gate",
            "plan",
            "--continue",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "R-1").state, St::WaitingFinalApproval, "{err}");

    // the file fixed (profiles back in the machine config only): the reload works again
    std::fs::write(&env.config, good).unwrap();
    let (code, err) = toryo(&env, &["task", "config", "R-1", "--reload"], &[]);
    assert_eq!(code, 0, "{err}");
}
