//! M16-02 acceptance: `toryo run A B …` runs several tasks at once, each in its own `toryo run`
//! child process (X37), and the verify lock (X36, O-23) lets one task at a time run its check
//! gates — through the `toryo` binary with fake agents.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use toryo::engine::PipelineState as St;

/// The events of a task's flight recorder.
fn events(env: &Env, id: &str) -> Vec<Value> {
    let p = env.data.join("runs").join(id).join("events.jsonl");
    std::fs::read_to_string(&p)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// `ts` of the first event of `kind` (RFC 3339 in UTC compares as text).
fn first_ts(evs: &[Value], kind: &str) -> String {
    evs.iter()
        .find(|e| e["type"] == kind)
        .and_then(|e| e["ts"].as_str())
        .unwrap_or_else(|| panic!("no {kind} in {evs:?}"))
        .to_string()
}

fn last_ts(evs: &[Value], kind: &str) -> String {
    evs.iter()
        .rev()
        .find(|e| e["type"] == kind)
        .and_then(|e| e["ts"].as_str())
        .unwrap_or_else(|| panic!("no {kind} in {evs:?}"))
        .to_string()
}

/// A planner that takes its time, so two runs overlap.
fn slow_plan(env: &Env) -> String {
    script(
        env,
        "slow-plan.json",
        json!({ "planner": [turn(vec![json!({"kind":"delay","ms":1500}), plan()])] }),
    )
}

/// The whole pipeline, approved by `--yes`.
fn happy(env: &Env) -> String {
    script(
        env,
        "happy.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    )
}

/// Adds a check gate that writes `<task> start|end <ticks>` lines to `gates.log` around a sleep
/// of `ms`, after the fixture's own gate.
fn with_logging_gate(env: &Env, ms: u64) -> std::path::PathBuf {
    let log = env.root.join("gates.log");
    let fwd = log.display().to_string().replace('\\', "/");
    let cfg = std::fs::read_to_string(&env.config).unwrap();
    let gate = if cfg!(windows) {
        format!(
            r#"    - {{ name: slow, program: pwsh, args: ["-NoProfile", "-NonInteractive", "-Command", "Add-Content -Path '{fwd}' -Value ($env:TORYO_TASK + ' start ' + [DateTime]::UtcNow.Ticks); Start-Sleep -Milliseconds {ms}; Add-Content -Path '{fwd}' -Value ($env:TORYO_TASK + ' end ' + [DateTime]::UtcNow.Ticks)"] }}"#
        )
    } else {
        format!(
            r#"    - {{ name: slow, program: sh, args: ["-c", "echo \"$TORYO_TASK start $(date +%s%N)\" >> '{fwd}'; sleep {}; echo \"$TORYO_TASK end $(date +%s%N)\" >> '{fwd}'"] }}"#,
            ms as f64 / 1000.0
        )
    };
    let anchor = "args: [\"grep\", \"-q\", \"Divide-Numbers\", \"--\", \"src\"] }\n";
    assert!(cfg.contains(anchor), "{cfg}");
    std::fs::write(
        &env.config,
        cfg.replacen(anchor, &format!("{anchor}{gate}\n"), 1),
    )
    .unwrap();
    log
}

/// The `(task, start, end)` intervals of `gates.log`.
fn intervals(log: &std::path::Path) -> Vec<(String, u128, u128)> {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let mut open: Vec<(String, u128)> = Vec::new();
    let mut out = Vec::new();
    for l in text.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        let (task, what, at) = (w[0].to_string(), w[1], w[2].parse::<u128>().unwrap());
        if what == "start" {
            open.push((task, at));
        } else {
            let i = open.iter().position(|(t, _)| *t == task).expect("a start");
            let (t, s) = open.remove(i);
            out.push((t, s, at));
        }
    }
    assert!(open.is_empty(), "{text}");
    out
}

#[test]
fn several_tasks_run_at_once_each_with_its_lines_and_one_end_block() {
    let env = setup("codex-openai");
    create(&env, "M-1");
    create(&env, "M-2");
    let s = slow_plan(&env);
    let (code, err) = toryo(&env, &["run", "M-1", "M-2", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    for id in ["M-1", "M-2"] {
        assert_eq!(task(&env, id).state, St::WaitingPlanApproval, "{err}");
    }
    assert!(err.contains("▶ run M-1 M-2 — 2 at a time"), "{err}");
    // every line of a child carries its task's id
    assert!(err.contains("M-1 │ ") && err.contains("M-2 │ "), "{err}");
    assert!(err.contains("══════════ 2 tasks ══════════"), "{err}");
    assert!(
        err.contains("M-1  WaitingPlanApproval    (exit 0) — next: "),
        "{err}"
    );
    // the two runs overlapped: each started before the other ended
    let (a, b) = (events(&env, "M-1"), events(&env, "M-2"));
    assert!(
        first_ts(&a, "run_started") < last_ts(&b, "run_ended"),
        "{a:?}\n{b:?}"
    );
    assert!(
        first_ts(&b, "run_started") < last_ts(&a, "run_ended"),
        "{a:?}\n{b:?}"
    );
}

#[test]
fn with_one_job_the_tasks_run_one_after_the_other() {
    let env = setup("codex-openai");
    create(&env, "Q-1");
    create(&env, "Q-2");
    let s = slow_plan(&env);
    let (code, err) = toryo(
        &env,
        &["run", "Q-1", "Q-2", "--jobs", "1", "--fake-agents", &s],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("1 at a time"), "{err}");
    let (a, b) = (events(&env, "Q-1"), events(&env, "Q-2"));
    assert!(
        last_ts(&a, "run_ended") <= first_ts(&b, "run_started"),
        "{a:?}\n{b:?}"
    );
}

#[test]
fn ids_are_checked_before_anything_starts() {
    let env = setup("codex-openai");
    create(&env, "C-1");
    let s = slow_plan(&env);
    for (args, want) in [
        (vec!["run", "C-1", "C-1"], "`C-1` is given twice"),
        (
            vec!["run", "C-1", "NOPE-1"],
            "no task NOPE-1 — nothing was started",
        ),
        (vec!["run", "C-1", "a/b"], "is not a task id"),
    ] {
        let mut a = args.clone();
        a.extend(["--fake-agents", &s]);
        let (code, err) = toryo(&env, &a, &[]);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(err.contains(want), "{args:?}: {err}");
    }
    assert_eq!(task(&env, "C-1").state, St::New, "nothing ran");
    assert!(events(&env, "C-1").is_empty());
    // --jobs is 1 to 8
    for bad in ["0", "9"] {
        let (code, err) = toryo(&env, &["run", "C-1", "--jobs", bad], &[]);
        assert_eq!(code, 2, "{bad}: {err}");
    }
}

#[test]
fn a_task_another_process_drives_is_refused_and_the_others_run() {
    let env = setup("codex-openai");
    create(&env, "H-1");
    create(&env, "H-2");
    let hang_s = script(
        &env,
        "hang.json",
        json!({ "planner": [turn(vec![hang()])] }),
    );
    let mut child = spawn(&env, &["run", "H-1", "--fake-agents", &hang_s], "h1.log");
    wait_until("H-1 to plan", 60, || {
        attempts(&env, "H-1")
            .iter()
            .any(|a| a.stage == "plan" && a.status == toryo::engine::store::AttemptStatus::Running)
    });
    let s = slow_plan(&env);
    let (code, err) = toryo(&env, &["run", "H-1", "H-2", "--fake-agents", &s], &[]);
    // the highest exit code: H-1's task_active
    assert_eq!(code, 3, "{err}");
    assert!(
        err.contains("H-1 │ task_active: task H-1 is active"),
        "{err}"
    );
    assert!(err.contains("(exit 3)"), "{err}");
    assert_eq!(task(&env, "H-2").state, St::WaitingPlanApproval, "{err}");
    assert!(child.try_wait().unwrap().is_none(), "H-1's own run goes on");
    let (code, err) = toryo(&env, &["stop", "H-1"], &[]);
    assert_eq!(code, 0, "{err}");
    child.wait().unwrap();
}

#[test]
fn a_task_waits_for_the_verify_lock_before_its_check_gates_run() {
    let env = setup("codex-openai");
    let log = with_logging_gate(&env, 200);
    create(&env, "V-1");
    // another holder of the machine's verify lock (X36): the test itself
    let lock_path = env.data.join("locks").join("machine").join("verify.lock");
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    held.try_lock().unwrap();
    let s = happy(&env);
    let mut child = spawn(
        &env,
        &["run", "V-1", "--yes", "--fake-agents", &s],
        "v1.log",
    );
    wait_until("V-1 to wait for the verify lock", 90, || {
        events(&env, "V-1")
            .iter()
            .any(|e| e["type"] == "lock_waiting" && e["lock"] == "machine:verify")
    });
    assert_eq!(task(&env, "V-1").state, St::Verifying);
    assert!(status_out(&env, "V-1").contains("waiting for lock machine:verify"));
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(
        intervals(&log).is_empty(),
        "no check gate ran while the lock was held"
    );
    held.unlock().unwrap();
    drop(held);
    let status = child.wait().unwrap();
    assert!(
        status.success(),
        "{}",
        std::fs::read_to_string(env.root.join("v1.log")).unwrap()
    );
    assert_eq!(task(&env, "V-1").state, St::Completed);
    assert_eq!(intervals(&log).len(), 1);
}

#[test]
fn a_stop_ends_the_wait_for_the_verify_lock() {
    let env = setup("codex-openai");
    create(&env, "W-1");
    let lock_path = env.data.join("locks").join("machine").join("verify.lock");
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    held.try_lock().unwrap();
    let s = happy(&env);
    let mut child = spawn(
        &env,
        &["run", "W-1", "--yes", "--fake-agents", &s],
        "w1.log",
    );
    wait_until("W-1 to wait for the verify lock", 90, || {
        events(&env, "W-1")
            .iter()
            .any(|e| e["type"] == "lock_waiting" && e["lock"] == "machine:verify")
    });
    let (code, err) = toryo(&env, &["stop", "W-1"], &[]);
    assert_eq!(code, 0, "{err}");
    child.wait().unwrap();
    let t = task(&env, "W-1");
    assert_eq!(
        t.state,
        St::Verifying,
        "the stage is interrupted, not failed"
    );
    assert!(attempts(&env, "W-1").iter().any(
        |a| a.stage == "verify" && a.status == toryo::engine::store::AttemptStatus::Interrupted
    ));
    assert!(!status_out(&env, "W-1").contains("waiting for lock"));
    // once the lock is free, a resume verifies
    drop(held);
    let (code, err) = toryo(&env, &["resume", "W-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "W-1").state, St::Completed, "{err}");
}

#[test]
fn the_check_gates_of_two_tasks_run_at_once_never_overlap() {
    let env = setup("codex-openai");
    let log = with_logging_gate(&env, 2500);
    create(&env, "G-1");
    create(&env, "G-2");
    let s = happy(&env);
    let (code, err) = toryo(
        &env,
        &["run", "G-1", "G-2", "--yes", "--fake-agents", &s],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    for id in ["G-1", "G-2"] {
        assert_eq!(task(&env, id).state, St::Completed, "{err}");
    }
    let mut iv = intervals(&log);
    assert_eq!(iv.len(), 2, "{iv:?}");
    iv.sort_by_key(|i| i.1);
    assert_ne!(iv[0].0, iv[1].0);
    assert!(iv[0].2 <= iv[1].1, "the gates overlapped: {iv:?}");
}

/// M16-R2: a run killed while it waited for the verify lock leaves its wait row; the next run
/// of the task clears it even when it gets the lock at once, so `status` never shows the task
/// waiting while it verifies.
#[test]
fn a_wait_row_a_killed_run_left_is_gone_with_the_next_run() {
    let env = setup("codex-openai");
    create(&env, "K-1");
    let lock_path = env.data.join("locks").join("machine").join("verify.lock");
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    held.try_lock().unwrap();
    let s = happy(&env);
    let mut child = spawn(
        &env,
        &["run", "K-1", "--yes", "--fake-agents", &s],
        "k1.log",
    );
    wait_until("K-1 to wait for the verify lock", 90, || {
        events(&env, "K-1")
            .iter()
            .any(|e| e["type"] == "lock_waiting" && e["lock"] == "machine:verify")
    });
    child.kill().unwrap();
    child.wait().unwrap();
    let wait = |env: &Env| {
        db(env)
            .call(|tx| toryo::engine::store::env_wait(tx, "K-1"))
            .unwrap()
    };
    assert_eq!(
        wait(&env).as_deref(),
        Some("machine:verify"),
        "the killed run left its row"
    );
    drop(held);
    let (code, err) = toryo(&env, &["resume", "K-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "K-1").state, St::Completed, "{err}");
    assert_eq!(wait(&env), None);
}
