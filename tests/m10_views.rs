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

// ── B2: toryo watch ──────────────────────────────────────────────────────────────────

fn events_lines(env: &Env, id: &str) -> Vec<String> {
    std::fs::read_to_string(task(env, id).run_store.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn watch_prints_the_history_of_a_finished_run() {
    let env = setup("codex-openai");
    create(&env, "W-1");
    let s = script(
        &env,
        "w1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, run_err) = toryo(&env, &["run", "W-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{run_err}");
    let file = events_lines(&env, "W-1");
    assert!(file.len() > 20, "{}", file.len());

    // AC-1: the whole history, rendered, and exit 0
    let (code, text, err) = out(&env, &["watch", "W-1", "--from-start", "--no-follow"]);
    assert_eq!(code, 0, "{err}");
    for want in [
        "▶ W-1 — run started (generation",
        "   [state] New --start--> Planning",
        "── PLAN (attempt 1, claude-max) ──",
        "   gate open: plan (occurrence 1)",
        "── IMPLEMENT (attempt 1, codex-openai) ──",
        "   turn implement/1 (codex-openai): completed",
        "✔ checkpoint ",
        "  ✔ divide-present (exit 0, ",
        "── REVIEW (attempt 1, claude-max) ──",
        "   [state] WaitingFinalApproval --final_approved--> Completed",
        "══════════ W-1: Completed ══════════",
    ] {
        assert!(text.contains(want), "{want:?} missing:\n{text}");
    }
    // every engine line the run printed is in the history, with the same text; `watch` also
    // shows the transitions the run never printed (quiet)
    for line in run_err
        .lines()
        .filter(|l| l.starts_with("   [state]") || l.starts_with("── "))
    {
        assert!(text.contains(line), "{line:?} missing:\n{text}");
    }
    let quiet = "   [state] Verifying --verify_passed--> Reviewing";
    assert!(text.contains(quiet) && !run_err.contains(quiet));

    // AC-3: --json prints the file's lines
    let (code, raw, _) = out(
        &env,
        &["watch", "W-1", "--json", "--from-start", "--no-follow"],
    );
    assert_eq!(code, 0);
    let lines: Vec<String> = raw.lines().map(str::to_string).collect();
    assert_eq!(lines, file);
    for l in &lines {
        serde_json::from_str::<Value>(l).unwrap();
    }
    // without --from-start: the last 20
    let (_, raw, _) = out(&env, &["watch", "W-1", "--json", "--no-follow"]);
    let last: Vec<String> = raw.lines().map(str::to_string).collect();
    assert_eq!(last, file[file.len() - 20..]);

    // following a finished task stops by itself (2 s without an event)
    let t = std::time::Instant::now();
    let (code, _, err) = out(&env, &["watch", "W-1", "--json"]);
    assert_eq!(code, 0, "{err}");
    assert!(t.elapsed() < std::time::Duration::from_secs(20));

    // a partial last line (a crash mid-write) is skipped
    let p = task(&env, "W-1").run_store.join("events.jsonl");
    let mut text = std::fs::read_to_string(&p).unwrap();
    text.push_str("{\"v\":1,\"seq\":99999,\"ty");
    std::fs::write(&p, text).unwrap();
    let (code, raw, _) = out(
        &env,
        &["watch", "W-1", "--json", "--from-start", "--no-follow"],
    );
    assert_eq!(code, 0);
    assert_eq!(raw.lines().count(), file.len());
}

#[test]
fn watch_errors_on_a_missing_task_or_file() {
    let env = setup("codex-openai");
    let (code, _, err) = out(&env, &["watch", "NOPE-1", "--no-follow"]);
    assert_eq!(code, 2);
    assert!(err.contains("NOPE-1"), "{err}");
    create(&env, "W-2"); // never ran: no flight recorder
    for args in [vec!["watch", "W-2", "--no-follow"], vec!["watch", "W-2"]] {
        let (code, _, err) = out(&env, &args);
        assert_eq!(code, 2, "{args:?}");
        assert!(err.contains("no flight recorder"), "{err}");
    }
}

/// AC-2: `watch` follows a run in another process — no lock — and exits on its own at the
/// plan gate.
#[test]
fn watch_follows_a_running_task_and_stops_at_the_gate() {
    let env = setup("codex-openai");
    create(&env, "W-3");
    let delay = json!({"kind": "delay", "ms": 2500});
    let s = script(
        &env,
        "w3.json",
        json!({
            "planner": [turn(vec![delay.clone(), plan()])],
            "implementer": [turn(vec![implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let mut run = spawn(&env, &["run", "W-3", "--fake-agents", &s], "w3-run.log");
    let p = task(&env, "W-3").run_store.join("events.jsonl");
    wait_until("the run to start", 60, || p.is_file());
    let mut watch = command(&env, &["watch", "W-3", "--json", "--from-start"], &[])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let t = std::time::Instant::now();
    let status = loop {
        if let Some(s) = watch.try_wait().unwrap() {
            break s;
        }
        if t.elapsed() > std::time::Duration::from_secs(90) {
            let _ = watch.kill();
            panic!("watch did not stop on its own");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert!(run.wait().unwrap().success());
    assert!(status.success());
    let o = watch.wait_with_output().unwrap();
    let raw = String::from_utf8_lossy(&o.stdout);
    let ev: Vec<Value> = raw
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(
        ev.iter()
            .any(|e| e["type"] == "gate_opened" && e["gate"] == "plan"),
        "{raw}"
    );
    assert_eq!(ev.last().unwrap()["type"], "run_ended", "{raw}");
    assert_eq!(task(&env, "W-3").state, St::WaitingPlanApproval);
    // it saw the run while it happened: the plan stage started before watch was done
    assert_eq!(raw.lines().collect::<Vec<_>>(), events_lines(&env, "W-3"));
}

/// Ctrl+C ends `watch` with exit 0 while the task is still running. Windows: CTRL_BREAK to
/// `watch` in its own process group (Ctrl+C cannot be sent to another group; `watch` treats both
/// alike); skipped without a console. Unix: SIGINT.
#[test]
fn ctrl_c_ends_watch_with_exit_0() {
    let env = setup("codex-openai");
    create(&env, "W-4");
    let s = script(
        &env,
        "w4.json",
        json!({"planner": [turn(vec![hang()])], "implementer": [], "reviewer": []}),
    );
    // the hanging run is killed however the test ends
    struct Killed(std::process::Child);
    impl Drop for Killed {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _run = Killed(spawn(
        &env,
        &["run", "W-4", "--fake-agents", &s],
        "w4-run.log",
    ));
    let p = task(&env, "W-4").run_store.join("events.jsonl");
    wait_until("the run to start", 60, || p.is_file());
    let mut c = command(&env, &["watch", "W-4"], &[]);
    c.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0000_0200); // CREATE_NEW_PROCESS_GROUP
    }
    let mut watch = c.spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2500));
    assert!(watch.try_wait().unwrap().is_none(), "still following");
    #[cfg(windows)]
    let sent = unsafe {
        windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent(1, watch.id()) != 0
    };
    #[cfg(unix)]
    let sent = unsafe { libc::kill(watch.id() as i32, libc::SIGINT) == 0 };
    #[cfg(not(any(windows, unix)))]
    let sent = false;
    if sent {
        let t = std::time::Instant::now();
        let status = loop {
            if let Some(s) = watch.try_wait().unwrap() {
                break s;
            }
            assert!(
                t.elapsed() < std::time::Duration::from_secs(20),
                "watch ignored the interrupt"
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        assert_eq!(status.code(), Some(0));
    } else {
        eprintln!("no console to send the interrupt to: skipped");
        let _ = watch.kill();
    }
}
