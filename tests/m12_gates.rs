//! M12 gate tests: check gates that cannot start (M12-05), and the other gate-related
//! behaviour of the milestone as it comes.
//!
//! - M12-05 a check gate whose program is not there is a **machine** stop (`gate_unavailable`)
//!   before any gate runs: no implementer turn, no fix cycle, `task config --reload` allowed,
//!   a plain `resume` goes on; `doctor` finds it first; the spawn-error path gives the same
//!   class.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use toryo::engine::store::{AttemptRow, AttemptStatus};
use toryo::engine::PipelineState as St;

/// The fixture's verify command replaced by `line` (a toryo.yaml-only repository).
fn with_verify_command(env: &Env, line: &str) {
    let y = std::fs::read_to_string(&env.config).unwrap();
    let old = r#"    - { name: divide-present, program: git, args: ["grep", "-q", "Divide-Numbers", "--", "src"] }"#;
    assert!(y.contains(old), "{y}");
    std::fs::write(&env.config, y.replace(old, line)).unwrap();
}

fn put_back_the_working_gate(env: &Env, line: &str) {
    let y = std::fs::read_to_string(&env.config).unwrap();
    let old = r#"    - { name: divide-present, program: git, args: ["grep", "-q", "Divide-Numbers", "--", "src"] }"#;
    assert!(y.contains(line), "{y}");
    std::fs::write(&env.config, y.replace(line, old)).unwrap();
}

fn events(env: &Env, id: &str) -> Vec<Value> {
    std::fs::read_to_string(task(env, id).run_store.join("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn stage(env: &Env, id: &str, stage: &str) -> Vec<AttemptRow> {
    attempts(env, id)
        .into_iter()
        .filter(|a| a.stage == stage)
        .collect()
}

fn run_to_the_gate(env: &Env, id: &str) -> String {
    let s = script(
        env,
        &format!("{id}.json"),
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(env, &["run", id, "--yes", "--fake-agents", &s], &[]);
    err
}

/// AC-1: `program: no-such-program`. `task create` accepts it without a word (no refusal, no
/// note); `doctor` FAILs; Verify escalates `gate_unavailable` before any gate ran and before
/// another implementer turn; `task config --reload` is allowed there; the plain `resume`
/// after the fix costs no fix cycle.
#[test]
fn m12_05_a_check_gate_that_cannot_start_is_a_machine_stop() {
    let env = setup("codex-openai");
    let line = r#"    - { name: build, program: no-such-program, args: [] }"#;
    with_verify_command(&env, line);
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "G-1"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert!(
        !err.contains("note:") && !err.contains("no-such-program"),
        "{err}"
    );
    // doctor finds it first
    let o = command(&env, &["doctor"], &[]).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    let fail = out
        .lines()
        .find(|l| l.contains("check gate `build`"))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(
        fail.contains("FAIL") && fail.contains("no-such-program"),
        "{fail}"
    );
    // the run stops at Verify, before any gate and before another implementer turn
    let err = run_to_the_gate(&env, "G-1");
    let t = task(&env, "G-1");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(t.machine.reason_class(), Some("gate_unavailable"), "{err}");
    let why = t.machine.reason().unwrap().to_string();
    assert!(
        why.starts_with("gate_unavailable: check gate `build`")
            && why.contains("no-such-program")
            && why.contains("`toryo resume G-1`")
            && why.contains("`toryo task config G-1 --reload`"),
        "{why}"
    );
    let ev = events(&env, "G-1");
    assert!(
        !ev.iter().any(|e| e["type"] == "verify_command"),
        "no gate ran"
    );
    assert!(
        !t.artifacts().join("VERIFY_RESULT.json").exists(),
        "no result"
    );
    assert_eq!(stage(&env, "G-1", "implement").len(), 1);
    assert_eq!(t.fix_cycle, 0);
    let v = stage(&env, "G-1", "verify");
    assert_eq!(
        (v.len(), v[0].status, v[0].error_class.as_deref()),
        (1, AttemptStatus::Failed, Some("gate_unavailable"))
    );
    // the way out: fix the config, `task config --reload` is allowed, a plain resume goes on
    put_back_the_working_gate(&env, line);
    let (code, err) = toryo(&env, &["task", "config", "G-1", "--reload"], &[]);
    assert_eq!(code, 0, "{err}");
    let s2 = script(
        &env,
        "g1b.json",
        json!({ "reviewer": [turn(vec![approved()])] }),
    );
    let (code, err) = toryo(&env, &["resume", "G-1", "--yes", "--fake-agents", &s2], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "G-1");
    assert_eq!(t.state, St::Completed, "{err}");
    assert_eq!(t.fix_cycle, 0, "a machine stop costs no fix cycle");
    assert_eq!(stage(&env, "G-1", "implement").len(), 1, "no further turn");
}

/// AC-2: the program passes the preflight (it exists) but cannot be started — a text file
/// where a program should be. The spawn error is the same class, not a plain error (was:
/// `error: starting `build` (…): spawning …`, exit 2, the task left `Verifying`).
#[test]
fn m12_05_a_program_that_exists_but_cannot_start_is_the_same_stop() {
    let env = setup("codex-openai");
    let bad = env.root.join(if cfg!(windows) {
        "not-a-program.exe"
    } else {
        "not-a-program"
    });
    std::fs::write(&bad, "this is text, not a program\n").unwrap();
    let program = bad.display().to_string().replace('\\', "/");
    with_verify_command(
        &env,
        &format!("    - {{ name: build, program: '{program}', args: [] }}"),
    );
    create(&env, "G-2");
    let err = run_to_the_gate(&env, "G-2");
    let t = task(&env, "G-2");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(t.machine.reason_class(), Some("gate_unavailable"), "{err}");
    let why = t.machine.reason().unwrap();
    assert!(
        why.contains("could not be started") && why.contains("not-a-program"),
        "{why}"
    );
    assert_eq!(t.fix_cycle, 0);
    assert_eq!(stage(&env, "G-2", "implement").len(), 1);
}
