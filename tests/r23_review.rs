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

/// R23-11: `budget set --tokens` is bounded as `pipeline.budget_tokens` is (the admission check
/// multiplies the cap by 8 in an i64). Was: any u64, cast to i64.
#[test]
fn r23_11_a_token_cap_too_large_for_the_arithmetic_is_refused() {
    let env = setup("codex-openai");
    create(&env, "B-11");
    let max = toryo::config::MAX_BUDGET_TOKENS;
    let (code, err) = toryo(
        &env,
        &["budget", "set", "B-11", "--tokens", &(max + 1).to_string()],
        &[],
    );
    assert_ne!(code, 0, "{err}");
    assert!(err.contains("--tokens"), "{err}");
    let (code, err) = toryo(
        &env,
        &["budget", "set", "B-11", "--tokens", &max.to_string()],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "B-11").budget_tokens, Some(max as i64));
}

/// R23-14: the R41 live interrupt ends a turn through the stop flag. Was: its `agent_killed`
/// said `reason: stop` — as if the owner had asked — although the engine knew it was the
/// budget. Now `budget`.
#[test]
fn r23_14_a_budget_interrupt_is_named_in_agent_killed() {
    let env = setup_with("codex-openai", "pipeline:\n  budget_tokens: 1000\n");
    create(&env, "K-15");
    let s = script(
        &env,
        "k15.json",
        json!({
            "planner": [turn(vec![json!({"kind": "usage", "input": 5000, "output": 10}), json!({"kind": "hang"})])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "K-15", "--fake-agents", &s], &[]);
    assert_eq!(task(&env, "K-15").state, St::Paused, "{err}");
    let ev = events(&env, "K-15");
    let killed = ev
        .iter()
        .find(|e| e["type"] == "agent_killed")
        .unwrap_or_else(|| panic!("an agent_killed event: {ev:?}"));
    assert_eq!(killed["reason"], "budget", "{killed}");
}

/// R23-16: `toryo run A B` whose own stderr is closed (`| head`, a reader that went away).
/// Was: the parent panicked at its next line ("failed printing to stderr"); its pipes from the
/// children closed with it, and each child died at its next line — mid-task. Now the parent
/// stops printing, keeps reading and waits; the tasks run to their end.
#[test]
fn r23_16_a_closed_stream_ends_no_task_of_a_multi_run() {
    let env = setup("codex-openai");
    create(&env, "P-1");
    create(&env, "P-2");
    let s = script(
        &env,
        "p.json",
        json!({
            "planner": [turn(vec![json!({"kind": "delay", "ms": 3000}), plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let mut c = command(
        &env,
        &["run", "P-1", "P-2", "--yes", "--fake-agents", &s],
        &[],
    );
    c.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child = c.spawn().unwrap();
    // read the first line, then close the pipe
    let mut err = child.stderr.take().unwrap();
    let mut first = [0u8; 64];
    let _ = std::io::Read::read(&mut err, &mut first).unwrap();
    drop(err);
    let status = child.wait().unwrap();
    for id in ["P-1", "P-2"] {
        assert_eq!(
            task(&env, id).state,
            St::Completed,
            "{id} (the parent ended with {status:?})"
        );
    }
}

/// R23-17: task ids that differ only in case. Was: `a-1` next to `A-1` was refused only by
/// the file system on Windows, with "run store … already exists — remove it" (the live task's
/// run store), and accepted where the folders were gone — two rows, one lock file. Now refused
/// by the id, before anything is made.
#[test]
fn r23_17_a_task_id_that_differs_only_in_case_is_refused() {
    let env = setup("codex-openai");
    create(&env, "Case-1");
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "CASE-1"],
        &[],
    );
    assert_ne!(code, 0, "{err}");
    assert!(
        err.contains("task Case-1 already exists") && err.contains("only in case"),
        "{err}"
    );
    assert!(!err.contains("remove it"), "{err}");
}

/// R23-18: eight processes open a fresh store at once (`toryo run A B …` after an upgrade).
/// Was: the schema transaction was deferred — read, then write — and a store whose snapshot
/// went stale before its write got SQLITE_BUSY, which the busy handler does not retry
/// ("database is locked"). Now it takes the write lock first.
#[test]
fn r23_18_many_stores_open_a_fresh_database_at_once() {
    for round in 0..10 {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("toryo.db");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let opened: Vec<_> = (0..8)
            .map(|_| {
                let (p, b) = (p.clone(), barrier.clone());
                std::thread::spawn(move || {
                    b.wait();
                    toryo::engine::store::Store::open(&p).map(|_| ())
                })
            })
            .collect();
        for h in opened {
            let r = h.join().unwrap();
            assert!(r.is_ok(), "round {round}: {:#}", r.unwrap_err());
        }
    }
}

/// R23-19: the machine config gets the rules of a repository file, and a machine config named
/// on the command line must exist. Was: a duplicated key in the machine config kept the last
/// value silently and a YAML tag in it passed; a mistyped `--machine-config` was "no machine
/// config", and its profiles, gates and skills quietly went away.
#[test]
fn r23_19_the_machine_config_is_read_as_strictly_as_the_repository_file() {
    let env = setup("codex-openai");
    let m = std::fs::read_to_string(&env.machine).unwrap();
    // a duplicated profile (the fixture's machine config is `--machine-config` of every call)
    std::fs::write(
        &env.machine,
        format!("{m}profiles:\n  other: {{ kind: codex, billing: subscription, exe: x }}\n"),
    )
    .unwrap();
    let (code, err) = toryo(&env, &["config", "show"], &[]);
    assert_ne!(code, 0, "{err}");
    assert!(
        err.contains("duplicate entry with key \"profiles\""),
        "{err}"
    );
    // a tag, also below the top level
    std::fs::write(
        &env.machine,
        m.replace("exe: codex-not-installed", "exe: !x codex-not-installed"),
    )
    .unwrap();
    let (code, err) = toryo(&env, &["config", "show"], &[]);
    assert_ne!(code, 0, "{err}");
    assert!(
        err.contains("YAML tags are not allowed in the machine config")
            && err.contains("profiles.codex-openai.exe"),
        "{err}"
    );
    // a machine config named on the command line that is not there
    std::fs::remove_file(&env.machine).unwrap();
    let (code, err) = toryo(&env, &["config", "show"], &[]);
    assert_ne!(code, 0, "{err}");
    assert!(err.contains("no such file"), "{err}");
}

/// R23-19: a skill of the same name in the repository and in the machine's `skills_dir` — the
/// repository's is delivered (D3); `doctor` now says so.
#[test]
fn r23_19_doctor_names_a_repository_skill_that_hides_a_machine_one() {
    let env = setup_orgchart(
        "codex-openai",
        "",
        &[(
            ".toryo/skills/xpp-batch/SKILL.md",
            "---\ndescription: d\n---\nREPO\n",
        )],
    );
    let shared = env.root.join("machine-skills");
    std::fs::create_dir_all(shared.join("xpp-batch")).unwrap();
    std::fs::write(shared.join("xpp-batch/SKILL.md"), "MACHINE\n").unwrap();
    std::fs::create_dir_all(shared.join("only-here")).unwrap();
    std::fs::write(shared.join("only-here/SKILL.md"), "MACHINE\n").unwrap();
    let m = std::fs::read_to_string(&env.machine).unwrap();
    let fwd = shared.display().to_string().replace('\\', "/");
    std::fs::write(&env.machine, format!("{m}skills_dir: '{fwd}'\n")).unwrap();
    let o = command(&env, &["doctor"], &[]).output().unwrap();
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        out.contains("skill `xpp-batch` is in the repository")
            && !out.contains("skill `only-here`"),
        "{out}"
    );
}

/// R23-22: a review still invalid after its correction turns. Was: the escalation said only
/// "the review was rejected 3 times" — not which criterion or which rule — and the owner's
/// ways on (`--note`, `--force`) were chosen blind. Now the reason says what was still wrong,
/// and the attempt keeps all of it.
#[test]
fn r23_22_a_rejected_review_says_what_was_wrong() {
    let env = setup("codex-openai");
    create(&env, "RV-1");
    let mut bad = approved();
    bad["structured"]["acceptance_criteria"] = json!([
        {"id": "AC-1", "met": true, "evidence": "e1"},
        {"id": "AC-2", "met": true, "evidence": "e2"}
    ]);
    let s = script(
        &env,
        "rv1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![bad.clone()]), turn(vec![bad.clone()]), turn(vec![bad])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "RV-1", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "RV-1");
    assert_eq!(t.state, St::Escalated, "{err}");
    let why = t.machine.reason().unwrap_or_default().to_string();
    assert!(
        why.starts_with("InvalidOutput: the review was rejected 3 times")
            && why.contains("still wrong:")
            && why.contains("AC-3"),
        "{why}"
    );
    let a = attempts(&env, "RV-1")
        .into_iter()
        .rfind(|a| a.stage == "review")
        .unwrap();
    assert!(
        a.error_detail
            .as_deref()
            .is_some_and(|d| d.contains("AC-3")),
        "{:?}",
        a.error_detail
    );
}
