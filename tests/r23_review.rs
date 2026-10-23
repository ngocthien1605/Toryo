//! The fixes of the review of 2026-10-23 (`docs/REVIEW-2026-10-23.md`): one test or more per
//! item of the fix prompt (R23-nn), each failing before its fix.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::json;
use toryo::engine::store::{self, DeployAsk};
use toryo::engine::PipelineState as St;

/// The tests that take a task lock in this process set `TORYO_DATA_DIR` for it: one at a time.
static IN_PROCESS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn events(env: &Env, id: &str) -> Vec<serde_json::Value> {
    let p = task(env, id).run_store.join("events.jsonl");
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l}: {e}")))
        .collect()
}

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

/// A task at its final gate: planned, approved, implemented, verified, reviewed.
fn at_final_gate(env: &Env, id: &str) {
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
    let (_, err) = toryo(env, &["run", id, "--fake-agents", &s], &[]);
    assert_eq!(task(env, id).state, St::WaitingPlanApproval, "{err}");
    let (_, err) = toryo(
        env,
        &[
            "approve",
            id,
            "--gate",
            "plan",
            "--continue",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    assert_eq!(task(env, id).state, St::WaitingFinalApproval, "{err}");
}

/// R23-03 (C2): the branch moved while the final gate was open. Was: the escalation kept no
/// class, `status` offered `approve --gate final --force`, which opened the same gate again,
/// and the next approval was refused again — a loop, a DECISIONS.md line per lap. Now the class
/// `approval_mismatch` is stored, `next:` says to put the branch back, and `--force` refuses
/// while the branch is not at the checkpoint.
#[test]
fn r23_03_a_moved_branch_at_the_final_gate_is_no_loop() {
    let env = setup("codex-openai");
    at_final_gate(&env, "M-1");
    let t = task(&env, "M-1");
    let checkpoint = t.machine.checkpoint().unwrap().to_string();
    git(
        &t.worktree,
        &["commit", "-q", "--allow-empty", "-m", "after the gate"],
    );
    let (code, err) = toryo(&env, &["approve", "M-1", "--gate", "final"], &[]);
    assert_ne!(code, 0, "{err}");
    assert!(err.contains("approval_mismatch"), "{err}");
    let t = task(&env, "M-1");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(t.machine.reason_class(), Some("approval_mismatch"), "{err}");
    let next = toryo::engine::runner::next_action(&t);
    assert!(
        next.contains("reset --hard") && next.contains(toryo::git::short(&checkpoint)),
        "{next}"
    );
    // `--force` does not open a gate whose approval would be refused
    let (code, err) = toryo(&env, &["approve", "M-1", "--gate", "final", "--force"], &[]);
    assert_ne!(code, 0, "{err}");
    assert!(
        err.contains("approval_mismatch") && err.contains("the final gate was not opened"),
        "{err}"
    );
    assert_eq!(task(&env, "M-1").state, St::Escalated, "{err}");
    // the way out that `next:` names
    git(&t.worktree, &["reset", "-q", "--hard", &checkpoint]);
    let (code, err) = toryo(&env, &["approve", "M-1", "--gate", "final", "--force"], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = toryo(&env, &["approve", "M-1", "--gate", "final"], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "M-1").state, St::Completed, "{err}");
}

/// R23-07 (C7): the stop poll of a running task is a read. Was: `stop_requested` (and the
/// `StopWatch` thread, once a second, and `toryo stop`) asked through `Store::call`, a
/// `BEGIN IMMEDIATE` transaction: while another process held the write lock, the poll waited
/// in SQLite's busy handler (15 s) and then failed the drive with "database is locked". A
/// read in WAL mode never waits for a writer.
#[test]
fn r23_07_the_stop_poll_never_waits_for_a_writer() {
    let env = setup("codex-openai");
    create(&env, "S-1");
    // (this process takes the task lock itself: its data folder is the fixture's)
    // (the data folder of this process is one variable: one such test at a time)
    let _one = IN_PROCESS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("TORYO_DATA_DIR", &env.data);
    let st = db(&env);
    let lock = toryo::engine::lock::TaskLock::acquire(&st, "S-1").unwrap();
    // another process (here: another connection) holds the write lock
    let other = rusqlite::Connection::open(env.data.join("toryo.db")).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = std::time::Instant::now();
    let asked = lock.stop_requested();
    let took = started.elapsed();
    other.execute_batch("ROLLBACK").unwrap();
    assert!(matches!(asked, Ok(false)), "{asked:?}");
    assert!(
        took < std::time::Duration::from_secs(3),
        "the poll waited {took:?}"
    );
    let watch = lock.watch_stop();
    assert!(!watch.stopped());
    drop(watch);
    lock.release();
}

/// R23-08 (C8, C9): an escalation keeps its class on the machine, the same class its
/// `escalated` event names. Was: only the escalations made through `escalate` and
/// `finish_classed` stored one; every escalation that ended an attempt through `end_and` (an
/// invalid plan, a protected change, a failed turn, a blocked review) had none, and `status`
/// read the reason's text instead.
#[test]
fn r23_08_an_escalation_keeps_its_class_on_the_machine() {
    let env = setup("codex-openai");
    create(&env, "V-1");
    let s = script(
        &env,
        "v1.json",
        json!({ "planner": [turn(vec![json!({"kind": "complete", "structured": {"not": "a plan"}})])] }),
    );
    let (_, err) = toryo(&env, &["run", "V-1", "--fake-agents", &s], &[]);
    let t = task(&env, "V-1");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(t.machine.reason_class(), Some("invalid_output"), "{err}");
    let ev = events(&env, "V-1");
    let escalated = ev
        .iter()
        .rfind(|e| e["type"] == "escalated")
        .expect("an escalated event");
    assert_eq!(escalated["reason_class"], "invalid_output");
    // the classes a plain `resume` takes back into the stage are snake_case, as stored
    for c in toryo::engine::state::MACHINE_CAUSED {
        assert_eq!(toryo::engine::flight::snake_case(c), c);
    }
}

/// R23-09 (C10): the task lock is still held by the OS for a moment after its owner let it go
/// (Windows releases a lock lazily) and no owner is recorded any more. Was: the retry for a
/// dead owner did not run — no owner is not a dead one — and `toryo resume` right after a
/// `toryo run` that ended normally was refused at once: `task_active (owner ?, since ?)`.
#[test]
fn r23_09_a_lock_let_go_a_moment_ago_is_taken() {
    let env = setup("codex-openai");
    create(&env, "L-9");
    // (the data folder of this process is one variable: one such test at a time)
    let _one = IN_PROCESS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("TORYO_DATA_DIR", &env.data);
    let st = db(&env);
    // the OS lock is held, no owner is in the row — and it goes a second later
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(toryo::engine::lock::lock_path("L-9"))
        .or_else(|_| {
            std::fs::create_dir_all(env.data.join("locks"))?;
            std::fs::File::create(toryo::engine::lock::lock_path("L-9"))
        })
        .unwrap();
    f.lock().unwrap();
    let held = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1000));
        f.unlock().unwrap();
    });
    let got = toryo::engine::lock::TaskLock::acquire(&st, "L-9");
    held.join().unwrap();
    assert!(got.is_ok(), "{:?}", got.err());
    // a lock that stays held is still refused, once the retry is over
    let again = toryo::engine::lock::TaskLock::acquire(&st, "L-9");
    let e = format!("{:#}", again.err().expect("the first one still holds it"));
    assert!(e.contains("is active"), "{e}");
}

/// R23-10 (C11): a crash between the review and its recorded decision. Was: the follow-ups
/// of an approval ("approved with these open issues") were appended to DECISIONS.md — which is
/// append-only — before the decision was recorded; after the crash the task was still
/// `Reviewing`, the resumed reviewer could answer differently, and the record stood. Now the
/// follow-ups are written after the decision.
#[test]
fn r23_10_no_follow_up_is_recorded_before_the_review_decision() {
    let env = setup("codex-openai");
    create(&env, "F-1");
    let mut review = approved();
    review["structured"]["issues"] = json!([
        {"id": "I-1", "severity": "low", "blocking": false, "description": "rename later"}
    ]);
    let s = script(
        &env,
        "f1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![review.clone()]), turn(vec![review])],
        }),
    );
    let (code, err) = toryo(
        &env,
        &["run", "F-1", "--yes", "--fake-agents", &s],
        &[("TORYO_TEST_CRASH_AT", "review:before_decision")],
    );
    assert_eq!(code, 86, "crashed at the point: {err}");
    let t = task(&env, "F-1");
    assert_eq!(t.state, St::Reviewing, "{err}");
    let dec = std::fs::read_to_string(t.artifacts().join("DECISIONS.md")).unwrap_or_default();
    assert!(!dec.contains("follow-ups"), "{dec}");
    // the resumed review records them, once
    let (code, err) = toryo(&env, &["resume", "F-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "F-1");
    assert_eq!(t.state, St::Completed, "{err}");
    let dec = std::fs::read_to_string(t.artifacts().join("DECISIONS.md")).unwrap();
    assert_eq!(dec.matches("follow-ups").count(), 1, "{dec}");
}
