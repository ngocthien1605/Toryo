//! The fixes of the review of 2026-10-23 (`docs/REVIEW-2026-10-23.md`): one test or more per
//! item of the fix prompt (R23-nn), each failing before its fix.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::json;
use toryo::engine::store::{self, DeployAsk};
use toryo::engine::PipelineState as St;

/// An escalated task (`gate_unavailable`: the build gate's program is not there) — a
/// machine-caused stop from `Verifying`, which a plain `resume` takes back into the stage.
fn escalated(env: &Env, id: &str) {
    let y = std::fs::read_to_string(&env.config).unwrap();
    let old = r#"    - { name: divide-present, program: git, args: ["grep", "-q", "Divide-Numbers", "--", "src"] }"#;
    if y.contains(old) {
        std::fs::write(
            &env.config,
            y.replace(
                old,
                "    - { name: build, program: no-such-program, args: [] }",
            ),
        )
        .unwrap();
    }
    create(env, id);
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
    let t = task(env, id);
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(t.machine.reason_class(), Some("gate_unavailable"), "{err}");
}

/// R23-01 (C0, X17): a `toryo resume` of every task must never be the owner's "yes" to a
/// deploy. Was: the loop judged "waits for a deploy yes" on the list it read before it took
/// each task's lock; a task that stopped to ask while the loop drove an earlier task was then
/// resumed as if the owner had said yes, and its deploy was confirmed. Now the question is
/// asked again on the row read under the lock.
#[test]
fn r23_01_a_resume_of_every_task_never_confirms_a_deploy() {
    let env = setup("codex-openai");
    // A: paused on its budget before the plan turn — a blanket resume drives it first
    create(&env, "A-1");
    let (code, err) = toryo(&env, &["budget", "set", "A-1", "--tokens", "0"], &[]);
    assert_eq!(code, 0, "{err}");
    let none = script(&env, "none.json", json!({}));
    let (_, err) = toryo(&env, &["run", "A-1", "--fake-agents", &none], &[]);
    assert_eq!(task(&env, "A-1").state, St::Paused, "{err}");
    let (code, err) = toryo(&env, &["budget", "set", "A-1", "--none"], &[]);
    assert_eq!(code, 0, "{err}");
    // B: a machine-caused stop, which the loop's list sees as such
    escalated(&env, "B-1");
    // A's plan turn takes a while: B changes while the loop drives A
    let s = script(
        &env,
        "slow.json",
        json!({ "planner": [turn(vec![json!({"kind": "delay", "ms": 6000}), plan()])] }),
    );
    let mut child = spawn(&env, &["resume", "--fake-agents", &s], "blanket.log");
    wait_until("A-1's plan turn", 60, || {
        attempts(&env, "A-1").iter().any(|a| a.stage == "plan")
    });
    // meanwhile B stopped to ask before its deploy (as another `toryo resume B-1` would)
    let b = task(&env, "B-1");
    let mut m = b.machine.clone();
    m.set_reason_class(toryo::engine::gates::ENV_CONFIRM);
    let commit = b.machine.checkpoint().expect("a checkpoint").to_string();
    let (digest, machine) = (b.config_sha256.clone(), serde_json::to_string(&m).unwrap());
    db(&env)
        .call(move |tx| {
            tx.execute("UPDATE tasks SET machine = ?1 WHERE id = 'B-1'", [&machine])?;
            store::ask_deploys(
                tx,
                "B-1",
                &digest,
                &[DeployAsk {
                    gate: "dbsync".into(),
                    commit,
                    mode: "first".into(),
                }],
            )
        })
        .unwrap();
    let status = child.wait().unwrap();
    let err = std::fs::read_to_string(env.root.join("blanket.log")).unwrap();
    assert_ne!(status.code(), Some(0), "B-1 waits for the owner: {err}");
    let confirmed: i64 = db(&env)
        .call(|tx| {
            Ok(tx.query_row(
                "SELECT COUNT(*) FROM deploy_confirmations WHERE task = 'B-1' AND confirmed_at IS NOT NULL",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(confirmed, 0, "no deploy was confirmed: {err}");
    assert!(
        err.contains("B-1: waits for your yes before it deploys"),
        "{err}"
    );
    let b = task(&env, "B-1");
    assert_eq!(b.state, St::Escalated, "{err}");
    assert_eq!(
        b.machine.reason_class(),
        Some(toryo::engine::gates::ENV_CONFIRM),
        "{err}"
    );
}
