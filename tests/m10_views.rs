//! M10 acceptance (DES-10): machine-readable and live views of a task, through the `toryo`
//! binary with fake agents.
//!
//! - B1: `status --json` and `task show --json` — one JSON document on stdout; a cleaned-up
//!   task keeps its state and next action.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use toryo::engine::PipelineState as St;

/// Runs `toryo`; returns (exit code, stdout, stderr).
fn out(env: &Env, args: &[&str]) -> (i32, String, String) {
    let o = command(env, args, &[]).output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

/// stdout of a `--json` command: exactly one JSON document.
fn json_of(env: &Env, args: &[&str]) -> Value {
    let (code, stdout, stderr) = out(env, args);
    assert_eq!(code, 0, "{stderr}");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("not one JSON document ({e}): {stdout}"))
}

fn to_final_gate(env: &Env, id: &str) {
    create(env, id);
    let s = script(
        env,
        "happy.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), json!({"kind":"usage","input":100,"cache_read":20,"output":5}), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(env, &["run", id, "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = toryo(
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
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(env, id).state, St::WaitingFinalApproval, "{err}");
}

#[test]
fn status_and_show_print_one_json_document() {
    let env = setup("codex-openai");
    // no tasks: still one document
    assert_eq!(json_of(&env, &["status", "--json"]), json!({"tasks": []}));
    to_final_gate(&env, "J-1");

    let s = json_of(&env, &["status", "--json"]);
    let tasks = s["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    let t = &tasks[0];
    assert_eq!(t["id"], "J-1");
    assert_eq!(t["state"], "WaitingFinalApproval");
    assert_eq!(t["fix_cycle"], 0);
    assert!(t["max_fix_cycles"].as_u64().unwrap() >= 1);
    assert!(t["spent_tokens"].as_i64().unwrap() >= 125, "{t}");
    assert!(t["reserved_tokens"].as_i64().is_some());
    assert!(t["budget_tokens"].is_null());
    assert!(t["active_owner"].is_null(), "nobody holds the lock");
    assert_eq!(t["compile_only"], false);
    assert!(t["next_action"].as_str().unwrap().contains("--gate final"));
    // `status J-1 --json` and `task list --json` are the same document shape
    assert_eq!(json_of(&env, &["status", "J-1", "--json"]), s);
    assert_eq!(json_of(&env, &["task", "list", "--json"]), s);

    let d = json_of(&env, &["task", "show", "J-1", "--json"]);
    for k in [
        "id",
        "state",
        "fix_cycle",
        "max_fix_cycles",
        "spent_tokens",
        "reserved_tokens",
        "budget_tokens",
        "active_owner",
        "compile_only",
        "next_action",
    ] {
        assert_eq!(d[k], t[k], "{k}");
    }
    assert_eq!(d["branch"], "toryo/J-1");
    assert!(d["worktree"].as_str().unwrap().ends_with("J-1"));
    assert!(d["run_store"].as_str().unwrap().contains("J-1"));
    let atts = d["attempts"].as_array().unwrap();
    let stages: Vec<&str> = atts.iter().map(|a| a["stage"].as_str().unwrap()).collect();
    assert_eq!(stages, ["plan", "implement", "verify", "review"], "{d}");
    let imp = &atts[1];
    assert_eq!(imp["status"], "succeeded");
    assert_eq!(imp["profile"], "codex-openai");
    assert!(imp["error_class"].is_null());
    assert!(
        imp["head_after"].as_str().is_some_and(|h| h.len() == 40),
        "{imp}"
    );
    assert!(imp["partial_commit"].is_null());
    let usage = d["usage"].as_array().unwrap();
    assert!(!usage.is_empty());
    let exact = usage.iter().find(|u| u["accounting"] == "exact").unwrap();
    assert_eq!(exact["budget_tokens"], 125);
    assert_eq!(exact["includes_unreported"], false);
    let gates = d["gates"].as_array().unwrap();
    assert_eq!(gates[0]["gate"], "plan");
    assert_eq!(gates[0]["status"], "approved");
    assert_eq!(gates[1]["gate"], "final");
    assert_eq!(gates[1]["status"], "open");
    let ev = d["events_file"].as_str().unwrap();
    assert!(ev.ends_with("events.jsonl") && std::path::Path::new(ev).is_file());
    // the text output is unchanged
    let (_, text, _) = out(&env, &["task", "show", "J-1"]);
    assert!(
        text.contains("stage attempts:") && text.contains("gates:"),
        "{text}"
    );
    assert!(serde_json::from_str::<Value>(&text).is_err());
}

#[test]
fn a_cleaned_up_task_is_still_in_status_json() {
    let env = setup("codex-openai");
    to_final_gate(&env, "J-2");
    let (code, err) = toryo(&env, &["approve", "J-2", "--gate", "final"], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = toryo(&env, &["task", "cleanup", "J-2"], &[]);
    assert_eq!(code, 0, "{err}");
    let s = json_of(&env, &["status", "--json"]);
    let t = &s["tasks"][0];
    assert_eq!(t["id"], "J-2");
    assert_eq!(t["state"], "Completed");
    let next = t["next_action"].as_str().unwrap();
    assert!(
        next.contains("cleaned up") && next.contains("toryo/J-2"),
        "{next}"
    );
}
