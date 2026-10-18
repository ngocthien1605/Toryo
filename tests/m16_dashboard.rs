//! M16-01 acceptance: `toryo dashboard` — every task at a glance from the store and each task's
//! `events.jsonl`, through the `toryo` binary with fake agents. Read-only: it takes no lock (a
//! task driven at that moment is shown, not refused) and writes nothing.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use toryo::engine::store::AttemptStatus;
use toryo::engine::PipelineState as St;

fn out(env: &Env, args: &[&str]) -> (i32, String, String) {
    let o = command(env, args, &[]).output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

fn dashboard_json(env: &Env) -> Value {
    let (code, stdout, stderr) = out(env, &["dashboard", "--json"]);
    assert_eq!(code, 0, "{stderr}");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("not one JSON document ({e}): {stdout}"))
}

/// Every file under `dir` with its size: what "writes nothing" is checked against.
fn files(dir: &std::path::Path) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push((
                    p.display().to_string(),
                    e.metadata().map(|m| m.len()).unwrap_or(0),
                ));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn an_empty_store_is_one_frame_and_one_document() {
    let env = setup("codex-openai");
    let (code, stdout, stderr) = out(&env, &["dashboard"]);
    assert_eq!(code, 0, "{stderr}");
    // stdout is a pipe here: one frame, then exit — no screen control
    assert!(
        stdout.starts_with("toryo dashboard — 0 tasks · 0 active · 0 waiting for you · "),
        "{stdout}"
    );
    assert!(
        stdout.contains("no tasks (`toryo task create --file TASK.md`)"),
        "{stdout}"
    );
    assert!(!stdout.contains('\u{1b}'), "{stdout:?}");
    let d = dashboard_json(&env);
    assert_eq!(d["tasks"], json!([]));
    assert!(
        d["generated_at"].as_str().is_some_and(|s| s.ends_with('Z')),
        "{d}"
    );
}

#[test]
fn the_dashboard_shows_every_task_with_its_last_event_and_next_action() {
    let env = setup("codex-openai");
    create(&env, "D-1");
    create(&env, "D-2");
    let s = script(
        &env,
        "plan.json",
        json!({ "planner": [turn(vec![plan()])] }),
    );
    let (code, err) = toryo(&env, &["run", "D-1", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "D-1").state, St::WaitingPlanApproval);

    // the run stores (the SQLite files are left out: closing the last connection checkpoints
    // the WAL whoever reads)
    let before = files(&env.data.join("runs"));
    let (code, stdout, stderr) = out(&env, &["dashboard", "--once"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.starts_with("toryo dashboard — 2 tasks · 0 active · 1 waiting for you"),
        "{stdout}"
    );
    // D-1: its status row, the newest event of its flight recorder, the next action
    let d1 = stdout.find("D-1 ").expect("a D-1 row");
    let block = &stdout[d1..];
    assert!(
        block.starts_with("D-1            WaitingPlanApproval    fix 0/"),
        "{stdout}"
    );
    assert!(
        block.contains("last: run ended (WaitingPlanApproval), "),
        "{stdout}"
    );
    assert!(
        block.contains("next: review ") && block.contains("`toryo approve D-1 --gate plan"),
        "{stdout}"
    );
    // D-2 never ran: no flight recorder, no activity line
    let d2 = &stdout[stdout.find("D-2 ").expect("a D-2 row")..];
    let mut lines = d2.lines();
    assert!(
        lines.next().unwrap().starts_with("D-2            New"),
        "{stdout}"
    );
    assert_eq!(
        lines.next().unwrap().trim(),
        "next: `toryo run D-2`",
        "{stdout}"
    );
    // read-only: not one file was added or changed
    assert!(!before.is_empty());
    assert_eq!(files(&env.data.join("runs")), before);

    let d = dashboard_json(&env);
    let tasks = d["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    let t1 = tasks.iter().find(|t| t["id"] == "D-1").unwrap();
    // the `status --json` task object, plus `activity`
    assert_eq!(t1["state"], "WaitingPlanApproval");
    assert!(t1["next_action"].as_str().unwrap().contains("--gate plan"));
    let a = &t1["activity"];
    assert!(a["events"].as_u64().unwrap() >= 4, "{a}");
    assert_eq!(a["run_open"], false);
    assert!(a["stage"].is_null(), "{a}");
    assert_eq!(a["last"]["type"], "run_ended");
    let t2 = tasks.iter().find(|t| t["id"] == "D-2").unwrap();
    assert_eq!(t2["activity"]["events"], 0);
    assert!(t2["activity"]["last"].is_null());

    // --hide-done leaves out finished tasks only
    let (code, err) = toryo(&env, &["task", "cleanup", "D-2", "--force"], &[]);
    assert_eq!(code, 0, "{err}");
    let (_, stdout, _) = out(&env, &["dashboard", "--hide-done"]);
    assert!(
        stdout.contains("D-1 ") && !stdout.contains("D-2 "),
        "{stdout}"
    );
    let (_, stdout, _) = out(&env, &["dashboard"]);
    assert!(stdout.contains("D-2            Cancelled"), "{stdout}");
}

#[test]
fn a_task_driven_now_is_shown_active_with_its_open_stage_and_never_refused() {
    let env = setup("codex-openai");
    create(&env, "D-3");
    let hang_s = script(
        &env,
        "hang.json",
        json!({ "planner": [turn(vec![plan()])], "implementer": [turn(vec![hang()])] }),
    );
    let mut child = spawn(
        &env,
        &["run", "D-3", "--yes", "--fake-agents", &hang_s],
        "d3.log",
    );
    wait_until("D-3 to be in its implement stage", 60, || {
        attempts(&env, "D-3")
            .iter()
            .any(|x| x.stage == "implement" && x.status == AttemptStatus::Running)
    });
    wait_until("the implement stage in the flight recorder", 30, || {
        dashboard_json(&env)["tasks"][0]["activity"]["stage"]["stage"] == "implement"
    });
    let (code, stdout, stderr) = out(&env, &["dashboard"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("1 active"), "{stdout}");
    assert!(stdout.contains("D-3            Implementing"), "{stdout}");
    assert!(stdout.contains(" · ACTIVE ("), "{stdout}");
    assert!(
        stdout.contains("now: implement attempt 1 (codex-openai) for "),
        "{stdout}"
    );
    let d = dashboard_json(&env);
    let t = &d["tasks"][0];
    assert!(t["active_owner"].is_string(), "{t}");
    assert_eq!(t["activity"]["run_open"], true);
    assert_eq!(t["activity"]["stage"]["attempt"], 1);
    // the dashboard took no lock: the run is still going
    assert!(
        child.try_wait().unwrap().is_none(),
        "the run is still going"
    );
    let (code, err) = toryo(&env, &["stop", "D-3"], &[]);
    assert_eq!(code, 0, "{err}");
    child.wait().unwrap();
    // after the run: no "now:" — the open stage ended with the run
    let (_, stdout, _) = out(&env, &["dashboard"]);
    assert!(!stdout.contains("now: "), "{stdout}");
    assert!(stdout.contains("last: run ended ("), "{stdout}");
}
