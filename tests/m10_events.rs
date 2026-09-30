//! M10 acceptance (ARCH-02): the engine's events and the per-task flight recorder
//! (`<data_dir>\runs\<id>\events.jsonl`), on `FakeAgentAdapter`.
//!
//! - A1: the lines of a run to the final gate, in order; a later run appends with a higher
//!   lock generation; an escalation carries its typed reason class; a profile secret never
//!   reaches the file; an unwritable file does not fail the run.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use toryo::engine::PipelineState as St;

/// The parsed lines of the task's `events.jsonl`.
fn events(env: &Env, id: &str) -> Vec<Value> {
    let p = task(env, id).run_store.join("events.jsonl");
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l}: {e}")))
        .collect()
}

fn types(ev: &[Value]) -> Vec<String> {
    ev.iter()
        .map(|e| e["type"].as_str().unwrap().to_string())
        .collect()
}

/// The position of the first event matching `f` at or after `from`.
fn find(ev: &[Value], from: usize, f: impl Fn(&Value) -> bool) -> usize {
    ev.iter()
        .enumerate()
        .skip(from)
        .find(|(_, e)| f(e))
        .map(|(i, _)| i)
        .unwrap_or_else(|| panic!("not found after {from}: {:#?}", types(ev)))
}

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

#[test]
fn a_run_to_the_final_gate_is_recorded_in_order_across_runs() {
    let env = setup("codex-openai");
    create(&env, "EV-1");
    let s = happy(&env);
    let (code, err) = toryo(&env, &["run", "EV-1", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "EV-1").state, St::WaitingPlanApproval);
    let first = events(&env, "EV-1");
    let g1 = first[0]["generation"].as_i64().unwrap();
    let (code, err) = toryo(
        &env,
        &[
            "approve",
            "EV-1",
            "--gate",
            "plan",
            "--continue",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "EV-1").state, St::WaitingFinalApproval);
    let ev = events(&env, "EV-1");

    // AC-1: every line is an envelope; seq strictly increasing
    for e in &ev {
        assert_eq!(e["v"], 1, "{e}");
        assert_eq!(e["task"], "EV-1", "{e}");
        assert!(e["ts"].as_str().is_some_and(|t| t.contains('T')), "{e}");
    }
    let seqs: Vec<u64> = ev.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");

    // AC-1: in order
    let mut at = find(&ev, 0, |e| e["type"] == "run_started");
    let states: Vec<(String, String)> = ev
        .iter()
        .filter(|e| e["type"] == "state_changed")
        .map(|e| {
            (
                e["from"].as_str().unwrap().to_string(),
                e["to"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let want = [
        ("New", "Planning"),
        ("Planning", "WaitingPlanApproval"),
        ("WaitingPlanApproval", "Implementing"),
        ("Implementing", "Checkpointing"),
        ("Checkpointing", "Verifying"),
        ("Verifying", "Reviewing"),
        ("Reviewing", "WaitingFinalApproval"),
    ];
    let want: Vec<(String, String)> = want
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    assert_eq!(states, want, "{:#?}", types(&ev));
    for stage in ["plan", "implement", "verify", "review"] {
        at = find(&ev, at, |e| {
            e["type"] == "stage_started" && e["stage"] == stage
        });
        at = find(&ev, at, |e| {
            e["type"] == "stage_finished" && e["stage"] == stage && e["status"] == "succeeded"
        });
    }
    let plan_gate = find(&ev, 0, |e| {
        e["type"] == "gate_opened" && e["gate"] == "plan"
    });
    let final_gate = find(&ev, 0, |e| {
        e["type"] == "gate_opened" && e["gate"] == "final"
    });
    assert!(plan_gate < final_gate);
    assert!(ev[final_gate]["occurrence"].as_u64().is_some());
    let last = ev.last().unwrap();
    assert_eq!(last["type"], "run_ended", "{last}");
    assert_eq!(last["state"], "WaitingFinalApproval");
    assert!(last["next_action"]
        .as_str()
        .unwrap()
        .contains("--gate final"));
    // the turn, checkpoint and verify command events of the run
    let turn = find(&ev, 0, |e| {
        e["type"] == "turn_finished" && e["stage"] == "implement"
    });
    assert_eq!(ev[turn]["profile"], "codex-openai");
    assert_eq!(ev[turn]["success"], true);
    let cp = find(&ev, 0, |e| e["type"] == "checkpoint_created");
    assert_eq!(ev[cp]["kind"], "normal");
    let vc = find(&ev, 0, |e| e["type"] == "verify_command");
    assert_eq!(
        (
            ev[vc]["name"].clone(),
            ev[vc]["exit"].clone(),
            ev[vc]["ok"].clone()
        ),
        (json!("divide-present"), json!(0), json!(true))
    );

    // AC-2: the second command appended with a higher generation, seq continuing
    assert!(ev.len() > first.len());
    assert_eq!(
        ev[..first.len()],
        first[..],
        "earlier lines are kept as they were"
    );
    let second = &ev[first.len()..];
    assert!(second
        .iter()
        .all(|e| e["generation"].as_i64().unwrap() > g1));
    assert_eq!(
        second[0]["seq"].as_u64().unwrap(),
        first.last().unwrap()["seq"].as_u64().unwrap() + 1
    );
    // the approval itself is recorded by the approving command
    assert_eq!(second[0]["type"], "state_changed", "{:#?}", types(second));
    assert_eq!(second[0]["event"], "plan_approved");
}

#[test]
fn an_escalation_carries_its_typed_reason_class() {
    let env = setup("codex-openai");
    commit_repo(&env, &[(".gitignore", ".claude/\n")], "ignore claude");
    create(&env, "EV-3");
    let s = script(
        &env,
        "p.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![
                write("src/Calculator.psm1", DIVIDE),
                write(".claude/settings.local.json", "{}\n"),
                implementation(),
            ])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "EV-3", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(task(&env, "EV-3").state, St::Escalated, "{err}");
    let ev = events(&env, "EV-3");
    let esc = find(&ev, 0, |e| e["type"] == "escalated");
    assert_eq!(
        ev[esc]["reason_class"], "protected_config_changed",
        "{}",
        ev[esc]
    );
    assert!(ev[esc]["reason"]
        .as_str()
        .unwrap()
        .contains(".claude/settings.local.json"));
    // the escalation comes before its state change, the attempt's end before both
    let sc = find(&ev, esc, |e| {
        e["type"] == "state_changed" && e["to"] == "Escalated"
    });
    assert_eq!(sc, esc + 1);
    let fin = find(&ev, 0, |e| {
        e["type"] == "stage_finished" && e["stage"] == "implement"
    });
    assert!(fin < esc);
    assert_eq!(ev[fin]["status"], "failed");
    assert_eq!(ev[fin]["error_class"], "protected_config_changed");
    assert_eq!(ev.last().unwrap()["type"], "run_ended");
    // `task cleanup` removes it with the run store
    let (code, err) = toryo(&env, &["task", "cleanup", "EV-3"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(!task(&env, "EV-3").run_store.join("events.jsonl").exists());
}

#[test]
fn a_profile_secret_never_reaches_the_flight_recorder() {
    const SECRET: &str = "m10-s3cr3t-\"q\"-value";
    let env = setup("codex-openai");
    std::fs::write(
        &env.machine,
        MACHINE_PROFILES.replace(
            "exe: codex-not-installed }",
            "exe: codex-not-installed, secret_env: { TORYO_M10_TOKEN: \"env:TORYO_M10_SECRET\" } }",
        ),
    )
    .unwrap();
    create(&env, "EV-4");
    let s = script(
        &env,
        "s.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![fail("Unknown", &format!("the tool printed {SECRET} and died"))])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(
        &env,
        &["run", "EV-4", "--yes", "--fake-agents", &s],
        &[("TORYO_M10_SECRET", SECRET)],
    );
    assert_eq!(task(&env, "EV-4").state, St::Escalated, "{err}");
    let text = std::fs::read_to_string(task(&env, "EV-4").run_store.join("events.jsonl")).unwrap();
    let escaped = serde_json::to_string(SECRET).unwrap();
    assert!(!text.contains(SECRET), "{text}");
    assert!(!text.contains(&escaped[1..escaped.len() - 1]), "{text}");
    let ev = events(&env, "EV-4");
    let esc = find(&ev, 0, |e| e["type"] == "escalated");
    let reason = ev[esc]["reason"].as_str().unwrap();
    assert!(
        reason.contains("[REDACTED:TORYO_M10_TOKEN]") && reason.contains("and died"),
        "{reason}"
    );
    assert_eq!(ev[esc]["reason_class"], "implementer_failed");
}

#[test]
fn an_unwritable_flight_recorder_does_not_fail_the_run() {
    let env = setup("codex-openai");
    create(&env, "EV-5");
    let p = task(&env, "EV-5").run_store.join("events.jsonl");
    std::fs::create_dir_all(&p).unwrap(); // a directory in its place
    let s = happy(&env);
    let (code, err) = toryo(&env, &["run", "EV-5", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "EV-5").state, St::Completed, "{err}");
    assert_eq!(
        err.matches("!! flight recorder:").count(),
        1,
        "reported once: {err}"
    );
}

/// Review fix R2: `turn_finished.usage` is what the store recorded. A Codex reviewer (cumulative
/// counters per session) with a correction turn reports 3300 then 4400 in total; the store keeps
/// 3300 and the delta 1100 — and so must the events. A turn with no usage shows its reservation.
#[test]
fn turn_usage_in_events_is_what_the_store_recorded() {
    let env = setup("codex-openai");
    let cfg = std::fs::read_to_string(&env.config).unwrap();
    std::fs::write(
        &env.config,
        cfg.replace(
            "planner: { profile: claude-max }",
            "planner: { profile: claude-max }\n  reviewer: { profile: codex-openai }",
        ),
    )
    .unwrap();
    create(&env, "EV-6");
    let partial = json!({"kind":"complete","structured":{"decision":"approved","summary":"s","acceptance_criteria":[
        {"id":"AC-1","met":true,"evidence":"e1"}],"issues":[]}});
    let usage = |input: u64, output: u64| json!({"kind":"usage","input":input,"cache_read":0,"output":output});
    let s = script(
        &env,
        "r2.json",
        json!({
            "planner": [turn(vec![plan()])], // no usage: a reservation
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), usage(100, 10), implementation()])],
            "reviewer": [turn(vec![usage(3000, 300), partial]), turn(vec![usage(4000, 400), approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "EV-6", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let ev = events(&env, "EV-6");
    let turns: Vec<&Value> = ev.iter().filter(|e| e["type"] == "turn_finished").collect();
    let total = |e: &Value| {
        ["uncached_input", "cache_read", "cache_write", "output"]
            .iter()
            .map(|k| e["usage"][k].as_u64().unwrap())
            .sum::<u64>()
    };
    let review: Vec<u64> = turns
        .iter()
        .filter(|e| e["stage"] == "review")
        .map(|e| total(e))
        .collect();
    assert_eq!(review, vec![3300, 1100], "{turns:#?}");
    let i = "EV-6".to_string();
    let rows: Vec<i64> = db(&env)
        .call(move |tx| {
            let mut q = tx.prepare(
                "SELECT u.budget_tokens FROM usage u JOIN stage_attempts a ON a.id = u.stage_attempt_id WHERE u.task_id = ?1 AND a.stage = 'review' ORDER BY u.id",
            )?;
            let v = q
                .query_map([i], |r| r.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(v)
        })
        .unwrap();
    assert_eq!(rows, vec![3300, 1100], "the store");
    let plan = turns.iter().find(|e| e["stage"] == "plan").unwrap();
    assert!(plan.get("usage").is_none(), "{plan}");
    assert_eq!(plan["reserved_tokens"], 200_000, "{plan}");
}

// ── review fix R5: every escalation and pause prints its reason once ─────────────────

/// `!! <reason>` appears exactly once on stderr, on the line right before the `[state]` line
/// into `Escalated`/`Paused`.
fn reason_once_before(err: &str, reason: &str, state_line: Option<&str>) {
    let lines: Vec<&str> = err.lines().collect();
    let want = format!("!! {reason}");
    let at: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| **l == want)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(at.len(), 1, "{want:?} not exactly once:\n{err}");
    assert_eq!(
        lines
            .get(at[0] + 1)
            .copied()
            .filter(|l| l.starts_with("   [state]")),
        state_line,
        "the [state] line follows:\n{err}"
    );
}

fn blocked() -> Value {
    json!({"kind":"complete","structured":{"decision":"blocked","summary":"contradiction","acceptance_criteria":[
        {"id":"AC-1","met":false,"evidence":"e1"},{"id":"AC-2","met":false,"evidence":"e2"},{"id":"AC-3","met":false,"evidence":"e3"}],
        "issues":[{"id":"I-1","severity":"high","blocking":true,"description":"TASK.md contradicts AGENTS.md"}]}})
}

#[test]
fn a_blocked_review_prints_its_reason_once() {
    let env = setup("codex-openai");
    create(&env, "R5-1");
    let s = script(
        &env,
        "b.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![blocked()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "R5-1", "--yes", "--fake-agents", &s], &[]);
    reason_once_before(
        &err,
        "the reviewer blocked the task",
        Some("   [state] Reviewing --review_blocked--> Escalated"),
    );
    // R6: a typed class, not `other`
    let ev = events(&env, "R5-1");
    let esc = find(&ev, 0, |e| e["type"] == "escalated");
    assert_eq!(ev[esc]["reason_class"], "review_blocked");
}

#[test]
fn the_fix_cycle_bound_prints_its_reason_once() {
    let env = setup("codex-openai");
    let (code, err) = toryo(
        &env,
        &[
            "task",
            "create",
            "--file",
            "TASK.md",
            "--id",
            "R5-2",
            "--max-fix-cycles",
            "1",
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    // the verify command greps for Divide-Numbers: never written, verify fails twice
    let s = script(
        &env,
        "f.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("tests/x.ps1", "# x\n"), implementation()]), turn(vec![write("tests/y.ps1", "# y\n"), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "R5-2", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(task(&env, "R5-2").state, St::Escalated, "{err}");
    reason_once_before(
        &err,
        "not accepted after 1 fix cycle(s) (last: verify failed)",
        // the verify stage ends in `finish_and`, whose `[state]` line stays quiet (owner
        // decision Q1); the transition is in events.jsonl and `watch`
        None,
    );
    assert!(!err.contains("--verify_failed--> Escalated"), "{err}");
    let ev = events(&env, "R5-2");
    let esc = find(&ev, 0, |e| e["type"] == "escalated");
    assert_eq!(ev[esc]["reason_class"], "max_fix_cycles");
}

#[test]
fn the_plan_policy_bound_prints_its_reason_once() {
    let env = setup("codex-openai");
    create(&env, "R5-3");
    let touching = json!({"kind":"complete","structured":{"understanding":"u","files_likely_involved":["TASK.md"],"steps":["x"],"validation":["tests"],"plan_markdown":"md"}});
    let s = script(
        &env,
        "p.json",
        json!({"planner": (0..4).map(|_| turn(vec![touching.clone()])).collect::<Vec<_>>()}),
    );
    let (_, err) = toryo(&env, &["run", "R5-3", "--fake-agents", &s], &[]);
    let reason = task(&env, "R5-3").machine.reason().unwrap().to_string();
    assert!(reason.contains("automatic re-plans"), "{reason}");
    reason_once_before(
        &err,
        &reason,
        Some("   [state] Planning --plan_policy_violation--> Escalated"),
    );
}

/// The two escalation paths that printed no reason before M10-R5: an implementer failure without
/// changes, and a review rejected three times.
#[test]
fn a_failed_implementer_and_a_rejected_review_print_their_reason_once() {
    let env = setup("codex-openai");
    create(&env, "R5-4");
    let s = script(
        &env,
        "i.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![fail("Unknown", "boom")])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "R5-4", "--yes", "--fake-agents", &s], &[]);
    reason_once_before(
        &err,
        "implementer failed: Unknown — boom",
        Some("   [state] Implementing --escalate--> Escalated"),
    );

    create(&env, "R5-5");
    let partial = json!({"kind":"complete","structured":{"decision":"approved","summary":"s","acceptance_criteria":[
        {"id":"AC-1","met":true,"evidence":"e1"}],"issues":[]}});
    let s = script(
        &env,
        "r.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![partial.clone()]), turn(vec![partial.clone()]), turn(vec![partial])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "R5-5", "--yes", "--fake-agents", &s], &[]);
    let reason = task(&env, "R5-5").machine.reason().unwrap().to_string();
    assert!(reason.starts_with("InvalidOutput"), "{reason}");
    reason_once_before(
        &err,
        &reason,
        Some("   [state] Reviewing --escalate--> Escalated"),
    );
}

/// Review fix R6: an attempt §10 recovery ends (a crash right after the implement attempt
/// began) gets its `stage_finished`, and the killed turn's reservation its `turn_finished`.
#[test]
fn attempts_ended_by_recovery_are_recorded() {
    let env = setup("codex-openai");
    create(&env, "EV-7");
    let s = script(
        &env,
        "c.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]), turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, _) = toryo(
        &env,
        &["run", "EV-7", "--yes", "--fake-agents", &s],
        &[("TORYO_TEST_CRASH_AT", "implement:after_begin")],
    );
    assert_eq!(code, 86, "crashed");
    let (code, err) = toryo(&env, &["resume", "EV-7", "--yes", "--fake-agents", &s], &[]);
    assert!(code == 0 || code == 1, "{err}");
    let ev = events(&env, "EV-7");
    let resumed = find(&ev, 1, |e| e["type"] == "run_started");
    let fin = find(&ev, resumed, |e| {
        e["type"] == "stage_finished" && e["stage"] == "implement" && e["attempt"] == 1
    });
    assert_eq!(ev[fin]["status"], "interrupted", "{}", ev[fin]);
    let turn = find(&ev, resumed, |e| {
        e["type"] == "turn_finished" && e["stage"] == "implement" && e["attempt"] == 1
    });
    assert!(turn < fin);
    assert!(ev[turn].get("usage").is_none());
    assert!(
        ev[turn]["reserved_tokens"].as_i64().unwrap() > 0,
        "{}",
        ev[turn]
    );
}

// ── review fixes round 2: R7 ─────────────────────────────────────────────────────────

/// R7.1: the crashed process already recorded (and emitted) the failed turn's reservation;
/// recovery must not emit a second `turn_finished` for it.
#[test]
fn recovery_does_not_repeat_a_turn_the_crashed_process_recorded() {
    let env = setup("codex-openai");
    create(&env, "R7-1");
    let s = script(
        &env,
        "c.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), fail("Unknown", "boom")]),
                turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]),
            ],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(
        &env,
        &["run", "R7-1", "--yes", "--fake-agents", &s],
        &[("TORYO_TEST_CRASH_AT", "implement:before_partial_commit")],
    );
    assert_eq!(code, 86, "{err}");
    let (_, err) = toryo(&env, &["resume", "R7-1", "--yes", "--fake-agents", &s], &[]);
    let ev = events(&env, "R7-1");
    let turns: Vec<&Value> = ev
        .iter()
        .filter(|e| e["type"] == "turn_finished" && e["stage"] == "implement" && e["attempt"] == 1)
        .collect();
    assert_eq!(turns.len(), 1, "{turns:#?}\n{err}");
    assert!(turns[0]["reserved_tokens"].as_i64().unwrap() > 0);
}

/// R7.3: a final-gate rejection at the fix-cycle bound escalates and prints its reason once,
/// right before its `[state]` line.
#[test]
fn a_rejection_at_the_bound_prints_its_reason_once() {
    let env = setup("codex-openai");
    let (code, err) = toryo(
        &env,
        &[
            "task",
            "create",
            "--file",
            "TASK.md",
            "--id",
            "R7-3",
            "--max-fix-cycles",
            "0",
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let s = script(
        &env,
        "g.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "R7-3", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = toryo(
        &env,
        &[
            "approve",
            "R7-3",
            "--gate",
            "plan",
            "--continue",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let (code, err) = toryo(
        &env,
        &["reject", "R7-3", "--gate", "final", "--reason", "more"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "R7-3").state, St::Escalated);
    reason_once_before(
        &err,
        "not accepted after 0 fix cycle(s) (last: final gate rejected: more)",
        Some("   [state] WaitingFinalApproval --rejected--> Escalated"),
    );
}

/// R7.5: a panic inside a drive still ends the record with `run_ended { error: "panicked" }`;
/// the exit code stays 101.
#[test]
fn a_panic_still_ends_the_record() {
    let env = setup("codex-openai");
    create(&env, "R7-5");
    let s = script(&env, "p.json", json!({"planner": [turn(vec![plan()])]}));
    let (code, err) = toryo(
        &env,
        &["run", "R7-5", "--fake-agents", &s],
        &[("TORYO_TEST_PANIC_AT", "plan:after_begin")],
    );
    assert_eq!(code, 101, "{err}");
    let ev = events(&env, "R7-5");
    let last = ev.last().unwrap();
    assert_eq!(last["type"], "run_ended", "{ev:#?}");
    assert_eq!(last["error"], "panicked");
    assert_eq!(last["state"], "Planning");
}
