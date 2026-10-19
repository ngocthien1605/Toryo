//! M16-AT (X39) acceptance: the diagnostic events of the flight recorder, through the `toryo`
//! binary with fake agents — `run_started.pid`, `run_ended.outcome` / `exit_code`, the verify
//! lock's `lock_acquired` / `lock_released`, `agent_spawned` / `agent_killed`, and
//! `cancel_requested` for a `toryo stop`. None of them is printed.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use toryo::engine::store::AttemptStatus;
use toryo::engine::PipelineState as St;

fn events(env: &Env, id: &str) -> Vec<Value> {
    std::fs::read_to_string(env.data.join("runs").join(id).join("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn at(ev: &[Value], f: impl Fn(&Value) -> bool) -> usize {
    ev.iter()
        .position(f)
        .unwrap_or_else(|| panic!("not found in {ev:#?}"))
}

#[test]
fn a_run_records_its_pid_its_end_its_locks_and_its_agents() {
    let env = setup("codex-openai");
    create(&env, "AE-1");
    let s = script(
        &env,
        "happy.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "AE-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "AE-1").state, St::Completed);
    let ev = events(&env, "AE-1");
    let started = &ev[at(&ev, |e| e["type"] == "run_started")];
    assert!(started["pid"].as_u64().is_some_and(|p| p > 0), "{started}");
    let ended = ev.last().unwrap();
    assert_eq!(ended["type"], "run_ended");
    assert_eq!(
        (ended["outcome"].as_str(), ended["exit_code"].as_i64()),
        (Some("done"), Some(0)),
        "{ended}"
    );
    // the verify lock: taken and given back inside the verify attempt
    let v_start = at(&ev, |e| {
        e["type"] == "stage_started" && e["stage"] == "verify"
    });
    let v_end = at(&ev, |e| {
        e["type"] == "stage_finished" && e["stage"] == "verify"
    });
    let acq = at(&ev, |e| {
        e["type"] == "lock_acquired" && e["lock"] == "machine:verify"
    });
    let rel = at(&ev, |e| {
        e["type"] == "lock_released" && e["lock"] == "machine:verify"
    });
    assert!(
        acq < v_start && v_start < rel && rel < v_end,
        "{acq} {v_start} {rel} {v_end}"
    );
    // one agent per turn, scripted (no pid), before its stage ends
    for stage in ["plan", "implement", "review"] {
        let sp = at(&ev, |e| e["type"] == "agent_spawned" && e["stage"] == stage);
        assert!(
            ev[sp]["pid"].is_null(),
            "a fake agent has no process: {}",
            ev[sp]
        );
        let fin = at(&ev, |e| {
            e["type"] == "stage_finished" && e["stage"] == stage
        });
        assert!(sp < fin, "{stage}");
    }
    assert!(!ev
        .iter()
        .any(|e| e["type"] == "agent_killed" || e["type"] == "cancel_requested"));
    // timestamps of one process only go forward
    let ts: Vec<&str> = ev.iter().map(|e| e["ts"].as_str().unwrap()).collect();
    assert!(ts.windows(2).all(|w| w[0] < w[1]), "{ts:?}");
    // none of them is printed
    for word in [
        "lock machine:verify acquired",
        "agent plan/1",
        "stop request taken up",
    ] {
        assert!(!err.contains(word), "{word}: {err}");
    }
}

#[test]
fn a_stop_records_the_request_the_killed_agent_and_a_stopped_end() {
    let env = setup("codex-openai");
    create(&env, "AE-2");
    let s = script(
        &env,
        "hang.json",
        json!({ "planner": [turn(vec![plan()])], "implementer": [turn(vec![hang()])] }),
    );
    let mut child = spawn(
        &env,
        &["run", "AE-2", "--yes", "--fake-agents", &s],
        "ae2.log",
    );
    wait_until("AE-2 to implement", 60, || {
        attempts(&env, "AE-2")
            .iter()
            .any(|a| a.stage == "implement" && a.status == AttemptStatus::Running)
    });
    let (code, err) = toryo(&env, &["stop", "AE-2"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(child.wait().unwrap().success());
    let ev = events(&env, "AE-2");
    let cancel = at(&ev, |e| e["type"] == "cancel_requested");
    assert_eq!(ev[cancel]["source"], "stop", "{}", ev[cancel]);
    let killed = at(&ev, |e| e["type"] == "agent_killed");
    assert_eq!(
        (ev[killed]["stage"].as_str(), ev[killed]["reason"].as_str()),
        (Some("implement"), Some("stop")),
        "{}",
        ev[killed]
    );
    let spawned = at(&ev, |e| {
        e["type"] == "agent_spawned" && e["stage"] == "implement"
    });
    assert!(
        spawned < killed && killed < cancel,
        "{spawned} {killed} {cancel}"
    );
    let ended = ev.last().unwrap();
    assert_eq!(
        (ended["outcome"].as_str(), ended["exit_code"].as_i64()),
        (Some("stopped"), Some(0)),
        "{ended}"
    );
}
