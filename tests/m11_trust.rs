//! M11 group A (review C1, H2) end to end: the machine floor for the verify boundary and the
//! machine's verify root, through `toryo task create`, a fake run and `toryo doctor`.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use toryo::engine::PipelineState as St;

fn full_run(env: &Env, name: &str) -> String {
    script(
        env,
        name,
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    )
}

fn verify_result(env: &Env, id: &str) -> Value {
    let p = task(env, id).artifacts().join("VERIFY_RESULT.json");
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

/// M11-A2 AC-1: the fixture repository asks for `verify.boundary: none`. Without a machine
/// floor that allows it, `task create` is refused and names both files; with
/// `min_boundary: none` the task is created and every verify result says trusted-code mode.
#[test]
fn a_boundary_below_the_machine_floor_is_refused_at_task_create() {
    let env = setup("codex-openai");
    std::fs::write(&env.machine, MACHINE_PROFILES).unwrap();
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "H2-1"],
        &[],
    );
    assert_ne!(code, 0, "{err}");
    for want in [
        "verify_boundary_below_floor",
        &env.config.display().to_string(),
        &env.machine.display().to_string(),
        "min_boundary: none",
        "keep `verify.boundary: low_integrity`",
    ] {
        assert!(err.contains(want), "{want}: {err}");
    }
    assert!(
        !env.data.join("runs").join("H2-1").exists(),
        "nothing created"
    );
    std::fs::write(&env.machine, format!("{MACHINE_PROFILES}{MACHINE_FLOOR}")).unwrap();
    create(&env, "H2-1");
    let s = full_run(&env, "h2.json");
    let (code, err) = toryo(&env, &["run", "H2-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "H2-1").state, St::Completed, "{err}");
    let vr = verify_result(&env, "H2-1");
    assert!(
        vr["boundary"]
            .as_str()
            .unwrap()
            .starts_with("none (trusted-code mode"),
        "{vr}"
    );
}

/// M11-A2 AC-2: a machine `verify_root` wins over the repository's; `doctor` says where the
/// verify settings came from.
#[test]
fn the_machine_verify_root_wins_and_doctor_shows_its_origin() {
    let env = setup("codex-openai");
    let mvr = env.root.join("machine-verify");
    let fwd = |p: &std::path::Path| p.display().to_string().replace('\\', "/");
    std::fs::write(
        &env.machine,
        format!(
            "{MACHINE_PROFILES}verify: {{ min_boundary: none, verify_root: \"{}\" }}\n",
            fwd(&mvr)
        ),
    )
    .unwrap();
    create(&env, "H2-2");
    let s = full_run(&env, "h2b.json");
    let (code, err) = toryo(&env, &["run", "H2-2", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let vr = verify_result(&env, "H2-2");
    let co = vr["verify_checkout"].as_str().unwrap().replace('\\', "/");
    assert!(co.starts_with(&fwd(&mvr)), "{co}");
    assert!(!env.root.join("verify").join("H2-2").exists());
    let o = command(&env, &["doctor"], &[]).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    let line = |area: &str| {
        out.lines()
            .find(|l| l.contains(area))
            .unwrap_or_else(|| panic!("{area}: {out}"))
            .to_string()
    };
    assert!(
        line("verify verify_root").contains("overridden by the machine config"),
        "{out}"
    );
    assert!(
        line("verify min_boundary").contains("none — source: the machine config"),
        "{out}"
    );
    assert!(
        line("verify boundary").contains("none — source: the repository config"),
        "{out}"
    );
    let o = command(&env, &["doctor", "--json"], &[]).output().unwrap();
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    let root = v
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["area"] == "verify verify_root")
        .unwrap();
    assert_eq!(root["source"], "repo_overridden_by_machine");
}
