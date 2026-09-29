//! Regression tests for the Planner/Reviewer, verify and gate fixes of the 2026-09-29 review
//! (WP-C2b): the probes that pinned each defect (`probes/probe_engine_flows.rs`,
//! `probes/probe_engine_core.rs`), inverted to pin the fix, plus the cases around them.
//!
//! - EC-01 a failed read-only turn (a permission denial) never yields a review or a plan
//! - EC-10 a reload while Paused sends the checkpoint back to Verify before the review; the
//!   final approval refuses a verify result that is not current and withdraws the gate
//! - EC-11 the final approval refuses a moved branch (`approval_mismatch`); uncommitted changes
//!   in the worktree (an IDE's `.vs/`) are a one-line note, never a refusal (F6)
//! - EC-13 a review correction turn refused by the admission budget pauses the task once
//! - EC-14 a failed Planner/Reviewer turn runs again as plan §11 says for its class — resumed
//!   or fresh, how often (F7) — then escalates with its real class; denials and policy
//!   refusals escalate at once; only invalid output corrects
//! - EC-25 plans and reviews are validated against their schemas in Rust
//! - EC-30 the planner never resumes another profile's session
//! - EC-31 a write by a read-only role is a policy violation, never implementer work — only
//!   what changed during its turn, not what a human left in the worktree before (F5)
//! - item 9: the verify feedback names the exit code in words
//! - item 10: a relative verify program runs from the verify checkout
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use toryo::engine::store::{self, AttemptRow, AttemptStatus};
use toryo::engine::PipelineState as St;

fn stage(env: &Env, id: &str, stage: &str) -> Vec<AttemptRow> {
    attempts(env, id)
        .into_iter()
        .filter(|a| a.stage == stage)
        .collect()
}

/// The fake calls of `key` (`planner`, `reviewer`, …) in the latest run of `id`.
fn calls_of(env: &Env, id: &str, key: &str) -> Vec<Value> {
    calls(env, id)
        .into_iter()
        .filter(|c| c["key"] == key)
        .collect()
}

/// Every transition of `id`: `{from, to, event: {event: <name>, …}}`, in order.
fn transitions(env: &Env, id: &str) -> Vec<Value> {
    let i = id.to_string();
    db(env)
        .call(move |tx| store::events(tx, &i))
        .unwrap()
        .into_iter()
        .filter(|e| e.1 == "transition")
        .map(|e| e.2)
        .collect()
}

fn named(env: &Env, id: &str, event: &str) -> Vec<Value> {
    transitions(env, id)
        .into_iter()
        .filter(|t| t["event"]["event"] == event)
        .collect()
}

/// `(gate, occurrence, status)` of every approval row.
fn approvals(env: &Env, id: &str) -> Vec<(String, u32, String)> {
    let i = id.to_string();
    db(env)
        .call(move |tx| store::approvals(tx, &i))
        .unwrap()
        .into_iter()
        .map(|a| (a.gate, a.occurrence, a.status))
        .collect()
}

fn reason(env: &Env, id: &str) -> String {
    task(env, id).machine.reason().unwrap_or("").to_string()
}

fn crash() -> Value {
    turn(vec![json!({"kind":"crash"})])
}

fn deny() -> Value {
    json!({"kind":"deny","tool":"Read","detail":"C:\\Users\\me\\.ssh\\id_rsa"})
}

/// The usual first three stages, and `reviewer` as the review turns.
fn to_review(reviewer: Vec<Value>) -> Value {
    json!({
        "planner": [turn(vec![plan()])],
        "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
        "reviewer": reviewer,
    })
}

/// Run `id` to the final gate: `run`, then `approve --gate plan --continue`.
fn to_final_gate(env: &Env, id: &str, s: &str) {
    let (code, err) = toryo(env, &["run", id, "--fake-agents", s], &[]);
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
            s,
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(env, id).state, St::WaitingFinalApproval, "{err}");
}

// ── EC-01: a failed read-only turn never yields a review or a plan ───────────────────

/// A real Claude stream (the shape of fixtures/captured) whose result carries a valid review
/// AND a permission denial: the real parser turns it into a failed turn that keeps the payload.
fn denied_review_stream(env: &Env) -> String {
    let review = json!({"decision":"approved","summary":"s","acceptance_criteria":[
        {"id":"AC-1","met":true,"evidence":"e1"},{"id":"AC-2","met":true,"evidence":"e2"},{"id":"AC-3","met":true,"evidence":"e3"}],"issues":[]});
    let init = json!({"type":"system","subtype":"init","session_id":"s-deny","tools":["Read","Grep","Glob"]});
    let result = json!({"type":"result","subtype":"success","is_error":false,"session_id":"s-deny","result":"done",
        "structured_output": review,
        "usage":{"input_tokens":10,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":5},
        "permission_denials":[{"tool_name":"Read","tool_input":{"file_path":"C:\\Users\\me\\.ssh\\id_rsa"}}]});
    let p = env.root.join("denied-review.jsonl");
    std::fs::write(&p, format!("{init}\n{result}\n")).unwrap();
    p.display().to_string()
}

/// Probe `probe_reviewer_permission_denial_is_accepted`, inverted: plan §6.2 "any denial
/// fails the stage" — the denied review escalates at once with its class, no blind retry.
#[test]
fn ec01_a_denied_reviewer_turn_never_approves_the_task() {
    let env = setup("codex-openai");
    create(&env, "P-1");
    let stream = denied_review_stream(&env);
    let replay = json!({"kind":"replay","agent":"claude","file": stream});
    let s = script(
        &env,
        "p1.json",
        to_review(vec![turn(vec![replay]), turn(vec![approved()])]),
    );
    let (code, err) = toryo(&env, &["run", "P-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "P-1");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert!(
        reason(&env, "P-1").starts_with(
            "PermissionDenied: the reviewer turn failed (1 permission denial(s)); a failed turn is never used"
        ),
        "{}",
        reason(&env, "P-1")
    );
    let review = stage(&env, "P-1", "review");
    assert_eq!(review.len(), 1);
    assert_eq!(review[0].status, AttemptStatus::Failed);
    assert_eq!(review[0].error_class.as_deref(), Some("PermissionDenied"));
    assert_eq!(review[0].external_session_id.as_deref(), Some("s-deny"));
    assert_eq!(calls_of(&env, "P-1", "reviewer").len(), 1, "no blind retry");
    assert!(
        !t.artifacts().join("review.json").exists(),
        "the payload of a failed turn is never stored as the review"
    );
    assert!(
        approvals(&env, "P-1").iter().all(|a| a.0 == "plan"),
        "no final gate"
    );
}

/// The same for the fake (a `deny` step fails the turn like the real parser) and a policy
/// refusal (a refused Codex preflight): escalated at once with the real class.
#[test]
fn ec01_ec14_denials_and_policy_refusals_escalate_at_once() {
    let refused = fail(
        "PolicyRefusal",
        "preflight: sandbox_mode is `danger-full-access`",
    );
    for (id, first, class) in [
        ("P-2", turn(vec![deny(), approved()]), "PermissionDenied"),
        ("P-3", turn(vec![refused]), "PolicyRefusal"),
    ] {
        let env = setup("codex-openai");
        create(&env, id);
        let s = script(
            &env,
            "p.json",
            to_review(vec![first, turn(vec![approved()])]),
        );
        let (code, err) = toryo(&env, &["run", id, "--yes", "--fake-agents", &s], &[]);
        assert_eq!(code, 1, "{err}");
        assert_eq!(task(&env, id).state, St::Escalated, "{err}");
        assert!(reason(&env, id).starts_with(&format!("{class}: the reviewer turn failed")));
        let review = stage(&env, id, "review");
        assert_eq!(
            (
                review.len(),
                review[0].status,
                review[0].error_class.as_deref()
            ),
            (1, AttemptStatus::Failed, Some(class))
        );
        assert_eq!(calls_of(&env, id, "reviewer").len(), 1, "{class}: at once");
    }
}

/// A denied Planner turn never yields a plan either: no PLAN.md, no plan gate.
#[test]
fn ec01_a_denied_planner_turn_never_opens_the_plan_gate() {
    let env = setup("codex-openai");
    create(&env, "P-4");
    let s = script(
        &env,
        "p4.json",
        json!({"planner": [turn(vec![deny(), plan()]), turn(vec![plan()])]}),
    );
    let (code, err) = toryo(&env, &["run", "P-4", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "P-4");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert!(
        reason(&env, "P-4").starts_with("PermissionDenied: the planner turn failed"),
        "{}",
        reason(&env, "P-4")
    );
    let p = stage(&env, "P-4", "plan");
    assert_eq!(
        (p.len(), p[0].status, p[0].error_class.as_deref()),
        (1, AttemptStatus::Failed, Some("PermissionDenied"))
    );
    assert_eq!(calls_of(&env, "P-4", "planner").len(), 1);
    assert!(approvals(&env, "P-4").is_empty(), "no plan gate");
    assert!(!t.artifacts().join("PLAN.md").exists());
}

// ── EC-14: crashes retry the same turn, then escalate with the real class ───────────

/// Probe `probe_reviewer_crashes_are_treated_as_invalid_output`, inverted.
#[test]
fn ec14_reviewer_crashes_retry_the_same_turn_then_escalate_with_their_class() {
    let env = setup("codex-openai");
    create(&env, "R-1");
    let s = script(
        &env,
        "r1.json",
        to_review(vec![crash(), crash(), crash(), turn(vec![approved()])]),
    );
    let (code, err) = toryo(&env, &["run", "R-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    assert_eq!(task(&env, "R-1").state, St::Escalated, "{err}");
    let why = reason(&env, "R-1");
    assert!(
        why.starts_with("ProcessCrashed: the reviewer turn failed (event stream ended without a terminal event), also after 2 retries"),
        "{why}"
    );
    assert!(!why.contains("InvalidOutput"), "{why}");
    let turns = calls_of(&env, "R-1", "reviewer");
    assert_eq!(
        turns.len(),
        3,
        "the turn and two retries — the 4th is never asked"
    );
    for (n, c) in turns.iter().enumerate() {
        assert_eq!(c["prompt"], turns[0]["prompt"], "the same prompt: {c}");
        assert_eq!(c["label"], format!("review-1-{n}"));
    }
    // F7 (plan §11): resume within the stage once, then a new attempt in a fresh session
    // (the fake numbers its sessions: planner 1, implementer 2, the first review turn 3)
    let resumed: Vec<&Value> = turns.iter().map(|c| &c["resume"]).collect();
    assert_eq!(
        resumed,
        [&Value::Null, &json!("fake-claude-max-3"), &Value::Null],
        "fresh, resumed, fresh"
    );
    assert!(turns[0]["prompt"]
        .as_str()
        .unwrap()
        .starts_with("Review the checkpoint"));
    let review = stage(&env, "R-1", "review");
    assert_eq!(review.len(), 1);
    assert_eq!(
        (
            review[0].status,
            review[0].error_class.as_deref(),
            review[0].correction_turns
        ),
        (AttemptStatus::Failed, Some("ProcessCrashed"), 0)
    );
}

/// A crash, then a valid review: the retry is not a correction turn.
#[test]
fn ec14_a_crashed_review_turn_is_retried_not_corrected() {
    let env = setup("codex-openai");
    create(&env, "R-2");
    let s = script(
        &env,
        "r2.json",
        to_review(vec![crash(), turn(vec![approved()])]),
    );
    let (code, err) = toryo(&env, &["run", "R-2", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "R-2").state, St::Completed, "{err}");
    let turns = calls_of(&env, "R-2", "reviewer");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1]["prompt"], turns[0]["prompt"]);
    assert!(!turns[1]["prompt"]
        .as_str()
        .unwrap()
        .contains("was rejected and was not used"));
    let review = stage(&env, "R-2", "review");
    assert_eq!(
        (review.len(), review[0].status, review[0].correction_turns),
        (1, AttemptStatus::Succeeded, 0)
    );
}

/// The Planner gets the same retry: its crashed turn's session, resumed (F7, plan §11).
#[test]
fn ec14_a_crashed_planner_turn_is_retried_with_the_same_prompt() {
    let env = setup("codex-openai");
    create(&env, "R-3");
    let s = script(
        &env,
        "r3.json",
        json!({"planner": [crash(), turn(vec![plan()])]}),
    );
    let (code, err) = toryo(&env, &["run", "R-3", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "R-3").state, St::WaitingPlanApproval, "{err}");
    let turns = calls_of(&env, "R-3", "planner");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1]["prompt"], turns[0]["prompt"]);
    assert_eq!(
        (turns[0]["label"].as_str(), turns[1]["label"].as_str()),
        (Some("plan-1"), Some("plan-1-1"))
    );
    assert_eq!(
        (&turns[0]["resume"], &turns[1]["resume"]),
        (&Value::Null, &json!("fake-claude-max-1")),
        "resumed within the stage"
    );
    let p = stage(&env, "R-3", "plan");
    assert_eq!((p.len(), p[0].status), (1, AttemptStatus::Succeeded));
}

// ── F7: plan §11 per class — how often a failed review turn runs again, and how ──────

/// A failing review turn of `class`.
fn failing(class: &str) -> Value {
    turn(vec![fail(class, &format!("{class} detail"))])
}

#[test]
fn f7_a_failed_review_turn_runs_again_as_plan_section_11_says_for_its_class() {
    // (task, the failing turns before an approval, whether each reviewer turn resumed the
    // previous session, how it ends)
    let cases = [
        // Timeout: a new attempt once, then escalated (was: the same turn, two retries)
        (
            "F-1",
            vec!["Timeout", "Timeout"],
            vec![false, false],
            Some("Timeout: the reviewer turn failed (Timeout detail), also after 1 retry (plan §11)"),
        ),
        // Unknown: like ProcessCrashed, once
        (
            "F-2",
            vec!["Unknown", "Unknown"],
            vec![false, true],
            Some("Unknown: the reviewer turn failed (Unknown detail), also after 1 retry (plan §11)"),
        ),
        // ContextWindowExceeded: a new session once, then escalated (was: escalated at once)
        (
            "F-3",
            vec!["ContextWindowExceeded", "ContextWindowExceeded"],
            vec![false, false],
            Some("ContextWindowExceeded: the reviewer turn failed (ContextWindowExceeded detail), also after 1 retry (plan §11)"),
        ),
        (
            "F-4",
            vec!["ContextWindowExceeded"],
            vec![false, false],
            None,
        ),
        // Network: like ProcessCrashed — resumed, then fresh, then escalated
        (
            "F-5",
            vec!["Network", "Network", "Network"],
            vec![false, true, false],
            Some("Network: the reviewer turn failed (Network detail), also after 2 retries (plan §11)"),
        ),
        // mixed: each class its own allowance, the attempt at most two retries
        (
            "F-6",
            vec!["Timeout", "ProcessCrashed", "ProcessCrashed"],
            vec![false, false, true],
            Some("ProcessCrashed: the reviewer turn failed (ProcessCrashed detail), also after 2 retries (plan §11)"),
        ),
    ];
    for (id, fails, resumed, escalation) in cases {
        let env = setup("codex-openai");
        create(&env, id);
        let mut reviewer: Vec<Value> = fails.iter().map(|c| failing(c)).collect();
        reviewer.push(turn(vec![approved()]));
        let s = script(&env, "f.json", to_review(reviewer));
        let (code, err) = toryo(&env, &["run", id, "--yes", "--fake-agents", &s], &[]);
        let turns = calls_of(&env, id, "reviewer");
        let got: Vec<bool> = turns.iter().map(|c| c["resume"].is_string()).collect();
        assert_eq!(got, resumed, "{id} {fails:?}: {err}");
        // a resumed turn is asked the same again; a fresh one the review prompt
        assert!(
            turns.iter().all(|c| c["prompt"] == turns[0]["prompt"]),
            "{id}: never a correction prompt"
        );
        let review = stage(&env, id, "review");
        assert_eq!(review.len(), 1, "{id}: one attempt");
        match escalation {
            Some(why) => {
                assert_eq!(code, 1, "{id}: {err}");
                assert_eq!(task(&env, id).state, St::Escalated, "{id}: {err}");
                assert_eq!(reason(&env, id), why, "{id}");
                assert_eq!(
                    (review[0].status, review[0].error_class.as_deref()),
                    (AttemptStatus::Failed, why.split(':').next()),
                    "{id}"
                );
            }
            None => {
                assert_eq!(code, 0, "{id}: {err}");
                assert_eq!(task(&env, id).state, St::Completed, "{id}: {err}");
                assert_eq!(review[0].status, AttemptStatus::Succeeded, "{id}");
            }
        }
    }
}

// ── EC-25: structured outputs are validated against their schemas ────────────────────

/// Probe `probe_plan_schema_is_not_validated_in_code`, inverted: a plan that matches none of
/// plan.schema.json is no plan — no PLAN.md, no plan gate.
#[test]
fn ec25_a_plan_that_is_not_a_plan_escalates() {
    let env = setup("codex-openai");
    create(&env, "V-1");
    let s = script(
        &env,
        "v1.json",
        json!({"planner": [turn(vec![json!({"kind":"complete","structured":{"note":"not a plan at all"}})])]}),
    );
    let (code, err) = toryo(&env, &["run", "V-1", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "V-1");
    assert_eq!(t.state, St::Escalated, "{err}");
    let why = reason(&env, "V-1");
    assert!(
        why.starts_with("InvalidOutput: the planner returned no valid plan — it does not match plan.schema.json: $: missing required property `understanding`"),
        "{why}"
    );
    let p = stage(&env, "V-1", "plan");
    assert_eq!(
        (p[0].status, p[0].error_class.as_deref()),
        (AttemptStatus::Failed, Some("InvalidOutput"))
    );
    assert!(approvals(&env, "V-1").is_empty(), "no plan gate");
    assert!(!t.artifacts().join("PLAN.md").exists());
}

/// A review that passes the checks in code (R28) but not review.schema.json (the old TS-03
/// fixtures: issue id `R-1`, severity `major`) gets a correction turn quoting the schema.
#[test]
fn ec25_a_review_that_violates_its_schema_gets_a_correction_turn() {
    let env = setup("codex-openai");
    create(&env, "V-2");
    let off_schema = json!({"kind":"complete","structured":{"decision":"changes_requested","summary":"s","acceptance_criteria":[
        {"id":"AC-1","met":true,"evidence":"e1"},{"id":"AC-2","met":false,"evidence":"no zero test"},{"id":"AC-3","met":true,"evidence":"e3"}],
        "issues":[{"id":"R-1","severity":"major","blocking":true,"description":"add the zero test"}]}});
    let s = script(
        &env,
        "v2.json",
        to_review(vec![turn(vec![off_schema]), turn(vec![approved()])]),
    );
    let (code, err) = toryo(&env, &["run", "V-2", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "V-2");
    assert_eq!(t.state, St::Completed, "{err}");
    assert_eq!(t.fix_cycle, 0, "the off-schema review was never used");
    let turns = calls_of(&env, "V-2", "reviewer");
    assert_eq!(turns.len(), 2);
    let p = turns[1]["prompt"].as_str().unwrap();
    assert!(
        p.starts_with("Your previous final response was rejected and was not used.")
            && p.contains("$.issues[0].id: \"R-1\" does not match ^I-[1-9][0-9]*$")
            && p.contains("$.issues[0].severity: \"major\" is not one of"),
        "{p}"
    );
    let review = stage(&env, "V-2", "review");
    assert_eq!(
        (review.len(), review[0].status, review[0].correction_turns),
        (1, AttemptStatus::Succeeded, 1)
    );
}

// ── EC-13: a correction turn refused by the budget pauses the task once ─────────────

/// Probe `a_review_correction_over_budget`, inverted: the review attempt is closed, the task
/// is `Paused(budget)` like any admission pause, and it resumes after `budget set`.
#[test]
fn ec13_a_correction_turn_over_the_budget_pauses_once() {
    let env = setup_with(
        "codex-openai",
        "pipeline:\n  budget_tokens: 1000\n  budget_live_interrupt: false\n",
    );
    create(&env, "A-1");
    let tokens = |input: u64, output: u64| json!({"kind":"usage","input":input,"output":output});
    let bad = json!({"kind":"complete","structured":{"decision":"approved","summary":"s","acceptance_criteria":[],"issues":[]}});
    let s = script(
        &env,
        "a1.json",
        json!({
            "planner": [turn(vec![tokens(100, 50), plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), tokens(300, 100), implementation()])],
            "reviewer": [turn(vec![tokens(400, 100), bad]), turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "A-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "a pause, not an error: {err}");
    assert!(!err.contains("state machine refused"), "{err}");
    let t = task(&env, "A-1");
    assert_eq!(t.state, St::Paused, "{err}");
    assert_eq!(t.machine.reason(), Some("budget"));
    assert_eq!(t.machine.paused_from(), Some(St::Reviewing));
    assert_eq!(named(&env, "A-1", "budget_exceeded").len(), 1);
    let review = stage(&env, "A-1", "review");
    assert_eq!(review.len(), 1);
    assert_eq!(
        (review[0].status, review[0].error_class.as_deref()),
        (AttemptStatus::Failed, Some("budget")),
        "the attempt is closed, never left Running"
    );
    assert!(review[0].ended_at.is_some());
    assert_eq!(
        calls_of(&env, "A-1", "reviewer").len(),
        1,
        "the correction never ran"
    );
    // like any budget pause: raise the cap, resume — a fresh review attempt
    let (code, err) = toryo(&env, &["budget", "set", "A-1", "--tokens", "100000"], &[]);
    assert_eq!(code, 0, "{err}");
    let s2 = script(
        &env,
        "a2.json",
        json!({"reviewer": [turn(vec![approved()])]}),
    );
    let (code, err) = toryo(&env, &["resume", "A-1", "--yes", "--fake-agents", &s2], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "A-1").state, St::Completed, "{err}");
    let review = stage(&env, "A-1", "review");
    assert_eq!(review.len(), 2);
    assert_eq!(review[1].status, AttemptStatus::Succeeded);
}

// ── EC-10: a verify result that is no longer current is never reviewed or approved ───

/// Run `id` until the reviewer's quota pauses it (in Reviewing), then reload `toryo.yaml`
/// with `extra` added to `commands.verify`.
fn paused_in_review_then_reload(env: &Env, id: &str, extra: &str) {
    create(env, id);
    let s = script(
        env,
        "q.json",
        to_review(vec![turn(vec![fail(
            "QuotaExceeded",
            "You've hit your usage limit",
        )])]),
    );
    let (code, err) = toryo(env, &["run", id, "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    let t = task(env, id);
    assert_eq!(
        (t.state, t.machine.paused_from()),
        (St::Paused, Some(St::Reviewing)),
        "{err}"
    );
    let cfg = std::fs::read_to_string(&env.config).unwrap();
    std::fs::write(
        &env.config,
        cfg.replacen("  verify:\n", &format!("  verify:\n    - {extra}\n"), 1),
    )
    .unwrap();
    let (code, err) = toryo(env, &["task", "config", id, "--reload"], &[]);
    assert_eq!(code, 0, "{err}");
    let vr: Value = serde_json::from_slice(
        &std::fs::read(task(env, id).artifacts().join("VERIFY_RESULT.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(vr["invalidated"], json!(true), "precondition: {vr}");
}

/// Probe `b_reload_while_paused_skips_reverify`, inverted: after a reload while Paused (from
/// Reviewing), the review stage verifies the checkpoint again first — with the new command,
/// which fails: the reviewer is never asked and the task never reaches the final gate.
#[test]
fn ec10_a_reload_while_paused_verifies_again_before_the_review() {
    let env = setup("codex-openai");
    paused_in_review_then_reload(
        &env,
        "B-1",
        r#"{ name: must-fail, program: git, args: ["grep", "-q", "NO_SUCH_STRING_ANYWHERE", "--", "src"] }"#,
    );
    let s2 = script(
        &env,
        "b2.json",
        json!({"reviewer": [turn(vec![approved()])]}),
    );
    let (_code, err) = toryo(&env, &["resume", "B-1", "--fake-agents", &s2], &[]);
    let t = task(&env, "B-1");
    assert!(
        !matches!(t.state, St::WaitingFinalApproval | St::Completed),
        "{:?}: {err}",
        t.state
    );
    let verify = stage(&env, "B-1", "verify");
    assert_eq!(verify.len(), 2, "verified again: {err}");
    assert_eq!(verify[1].status, AttemptStatus::Failed);
    assert!(
        calls_of(&env, "B-1", "reviewer").is_empty(),
        "the stale verify result was never reviewed"
    );
    let back = named(&env, "B-1", "reverify");
    assert_eq!(back.len(), 1, "{back:?}");
    assert_eq!(
        (back[0]["from"].as_str(), back[0]["to"].as_str()),
        (Some("Reviewing"), Some("Verifying"))
    );
    assert!(
        err.contains("it is verified again before the review"),
        "{err}"
    );
}

/// With a passing new command the checkpoint is verified again, reviewed and approved.
#[test]
fn ec10_after_the_reverify_the_task_completes_on_the_new_checks() {
    let env = setup("codex-openai");
    paused_in_review_then_reload(
        &env,
        "B-2",
        r#"{ name: divide-again, program: git, args: ["grep", "-q", "Divide-Numbers", "--", "src"] }"#,
    );
    let s2 = script(
        &env,
        "b2.json",
        json!({"reviewer": [turn(vec![approved()])]}),
    );
    let (code, err) = toryo(&env, &["resume", "B-2", "--fake-agents", &s2], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "B-2");
    assert_eq!(t.state, St::WaitingFinalApproval, "{err}");
    let vr: Value =
        serde_json::from_slice(&std::fs::read(t.artifacts().join("VERIFY_RESULT.json")).unwrap())
            .unwrap();
    let names: Vec<&str> = vr["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["divide-again", "divide-present"], "{vr}");
    assert_eq!(vr["pass"], json!(true));
    let (code, err) = toryo(&env, &["approve", "B-2", "--gate", "final"], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "B-2").state, St::Completed);
    assert_eq!(stage(&env, "B-2", "verify").len(), 2);
}

/// The final approval itself refuses a verify result that is not current (e.g. invalidated by
/// a reload that did not withdraw the gate): the gate is withdrawn, the checkpoint verified
/// again by the next `toryo run`.
#[test]
fn ec10_the_final_approval_refuses_a_verify_result_that_is_not_current() {
    let env = setup("codex-openai");
    create(&env, "B-3");
    let s = script(&env, "b3.json", to_review(vec![turn(vec![approved()])]));
    to_final_gate(&env, "B-3", &s);
    let t = task(&env, "B-3");
    toryo::engine::artifacts::put(
        &db(&env),
        &t,
        None,
        "verify-result",
        "json",
        "verify_result",
        br#"{"invalidated": true, "reason": "test: the verify configuration changed"}"#,
        Some("VERIFY_RESULT.json"),
    )
    .unwrap();
    let (code, err) = toryo(&env, &["approve", "B-3", "--gate", "final"], &[]);
    assert_eq!(code, 2, "refused: {err}");
    assert!(
        err.contains("has no current passing verify result")
            && err.contains("nothing was approved")
            && err.contains("`toryo run B-3`"),
        "{err}"
    );
    let t = task(&env, "B-3");
    assert_eq!(t.state, St::Verifying);
    assert_eq!(t.machine.verified_head(), None);
    assert_eq!(
        approvals(&env, "B-3"),
        [
            ("plan".to_string(), 1, "approved".to_string()),
            ("final".to_string(), 2, "superseded".to_string()),
        ]
    );
    // `toryo run` verifies again and reopens the gate
    let (code, err) = toryo(&env, &["run", "B-3", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "B-3").state, St::WaitingFinalApproval, "{err}");
    let (code, err) = toryo(&env, &["approve", "B-3", "--gate", "final"], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "B-3").state, St::Completed);
    assert_eq!(stage(&env, "B-3", "verify").len(), 2);
}

// ── EC-11: the final approval approves the checkpoint, not whatever the branch holds ──

/// Probe `c_final_approval_ignores_new_branch_commits`, inverted: §11 `approval_mismatch`.
#[test]
fn ec11_the_final_approval_refuses_a_moved_branch() {
    let env = setup("codex-openai");
    create(&env, "C-1");
    let s = script(&env, "c1.json", to_review(vec![turn(vec![approved()])]));
    to_final_gate(&env, "C-1", &s);
    let cp = task(&env, "C-1").machine.checkpoint().unwrap().to_string();
    let w = wt(&env, "C-1");
    std::fs::OpenOptions::new()
        .append(true)
        .open(w.join("src/Calculator.psm1"))
        .and_then(|mut f| {
            std::io::Write::write_all(
                &mut f,
                b"\nRemove-Item -Recurse -Force $HOME # never reviewed\n",
            )
        })
        .unwrap();
    git(&w, &["add", "-A"]);
    git(
        &w,
        &[
            "-c",
            "user.name=x",
            "-c",
            "user.email=x@x",
            "commit",
            "-q",
            "-m",
            "unreviewed",
        ],
    );
    let head = git(&w, &["rev-parse", "HEAD"]).trim().to_string();
    assert_ne!(
        head, cp,
        "precondition: the branch moved after the gate opened"
    );
    let (code, err) = toryo(&env, &["approve", "C-1", "--gate", "final"], &[]);
    assert_eq!(code, 2, "refused: {err}");
    let t = task(&env, "C-1");
    assert_eq!(t.state, St::Escalated, "{err}");
    let why = reason(&env, "C-1");
    assert_eq!(
        why,
        format!(
            "approval_mismatch: the branch toryo/C-1 is at {}, not at the approved checkpoint {} — nothing was approved (R32)",
            &head[..12],
            &cp[..12]
        )
    );
    assert!(err.contains(&why), "{err}");
    assert_eq!(
        approvals(&env, "C-1").last().cloned(),
        Some(("final".to_string(), 2, "superseded".to_string()))
    );
    assert_eq!(
        git(&w, &["rev-parse", "HEAD"]).trim(),
        head,
        "nothing was reset"
    );
    // the human drops the unreviewed commit and opens the gate again
    git(&w, &["reset", "-q", "--hard", &cp]);
    let (code, err) = toryo(&env, &["approve", "C-1", "--gate", "final", "--force"], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = toryo(&env, &["approve", "C-1", "--gate", "final"], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "C-1").state, St::Completed);
}

/// F6 (owner decision): uncommitted changes in the worktree at the final gate — an IDE folder
/// such as Visual Studio's `.vs/` — are not part of the approved checkpoint: a one-line note,
/// and the approval goes through (was: `approval_mismatch` → Escalated).
#[test]
fn ec11_uncommitted_changes_at_the_final_approval_are_noted_not_refused() {
    let env = setup("codex-openai");
    create(&env, "C-2");
    let s = script(&env, "c2.json", to_review(vec![turn(vec![approved()])]));
    to_final_gate(&env, "C-2", &s);
    let cp = task(&env, "C-2").machine.checkpoint().unwrap().to_string();
    let w = wt(&env, "C-2");
    std::fs::create_dir_all(w.join(".vs/calc/v17")).unwrap();
    std::fs::write(w.join(".vs/calc/v17/.suo"), [0u8, 1, 2, 3]).unwrap();
    std::fs::write(w.join("src/Extra.ps1"), "# not reviewed\n").unwrap();
    let (code, err) = toryo(&env, &["approve", "C-2", "--gate", "final"], &[]);
    assert_eq!(code, 0, "approved: {err}");
    assert_eq!(task(&env, "C-2").state, St::Completed, "{err}");
    let note: Vec<&str> = err.lines().filter(|l| l.starts_with("note: ")).collect();
    assert_eq!(
        note,
        [format!(
            "note: the worktree has uncommitted changes (.vs/calc/v17/.suo, src/Extra.ps1) — they are not part of the approved checkpoint {} and stay uncommitted",
            &cp[..12]
        )],
        "{err}"
    );
    // the branch is the approved checkpoint; the changes stay in the worktree, uncommitted
    assert_eq!(git(&w, &["rev-parse", "refs/heads/toryo/C-2"]).trim(), cp);
    assert!(w.join("src/Extra.ps1").exists() && w.join(".vs/calc/v17/.suo").exists());
    assert!(!clean(&w));
}

// ── EC-30: the planner never resumes another profile's session ───────────────────────

/// Probe `l_plan_resumes_foreign_session_after_planner_switch`, inverted.
#[test]
fn ec30_a_switched_planner_starts_a_fresh_session() {
    let env = setup("codex-openai");
    create(&env, "L-1");
    let s = script(&env, "l1.json", json!({"planner": [turn(vec![plan()])]}));
    let (code, err) = toryo(&env, &["run", "L-1", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let cfg = std::fs::read_to_string(&env.config).unwrap();
    std::fs::write(
        &env.config,
        cfg.replacen(
            "planner: { profile: claude-max }",
            "planner: { profile: codex-openai }\n  reviewer: { profile: claude-max }",
            1,
        ),
    )
    .unwrap();
    let (code, err) = toryo(&env, &["task", "config", "L-1", "--reload"], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = toryo(
        &env,
        &[
            "reject",
            "L-1",
            "--gate",
            "plan",
            "--reason",
            "use a smaller change",
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let (code, err) = toryo(&env, &["run", "L-1", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let p1 = stage(&env, "L-1", "plan")[0].clone();
    assert_eq!(p1.profile.as_deref(), Some("claude-max"));
    assert!(p1.external_session_id.is_some());
    let replan = calls_of(&env, "L-1", "planner");
    assert_eq!(replan.len(), 1);
    assert_eq!(replan[0]["profile"], "codex-openai");
    assert!(
        replan[0]["resume"].is_null(),
        "the Codex planner is never handed the Claude session: {}",
        replan[0]
    );
    assert!(replan[0]["prompt"]
        .as_str()
        .unwrap()
        .contains("use a smaller change"));
}

// ── EC-31: a read-only role writes nothing ───────────────────────────────────────────

/// Probe `h_reviewer_write_is_committed_as_implementer_work`, inverted: the reviewer's write
/// escalates as a policy violation and never reaches a checkpoint.
#[test]
fn ec31_a_reviewer_write_is_a_policy_violation() {
    let env = setup("codex-openai");
    create(&env, "H-1");
    let s = script(
        &env,
        "h1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]), turn(vec![implementation()])],
            "reviewer": [turn(vec![write("src/Calculator.psm1", "\n# REVIEWER-WROTE-THIS\n"), changes()]), turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "H-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "H-1");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(
        reason(&env, "H-1"),
        "policy_violation: the worktree changed during a read-only stage: [\"src/Calculator.psm1\"]"
    );
    let review = stage(&env, "H-1", "review");
    assert_eq!(
        (
            review.len(),
            review[0].status,
            review[0].error_class.as_deref()
        ),
        (1, AttemptStatus::Failed, Some("policy_violation"))
    );
    assert_eq!(
        calls_of(&env, "H-1", "implementer").len(),
        1,
        "no fix cycle"
    );
    assert_eq!(t.fix_cycle, 0);
    let log = git(
        &t.worktree,
        &[
            "log",
            "--format=%h %s",
            "-G",
            "REVIEWER-WROTE-THIS",
            "main..HEAD",
        ],
    );
    assert!(log.trim().is_empty(), "never committed: {log}");
    // left in place for the human, uncommitted
    assert!(git(&t.worktree, &["status", "--porcelain"]).contains("src/Calculator.psm1"));
}

/// Run `id` until the reviewer's quota pauses it, then leave `review-notes.txt` in the
/// worktree — a human's file, written while the task waits.
fn paused_in_review_with_a_human_file(env: &Env, id: &str) -> std::path::PathBuf {
    create(env, id);
    let s = script(
        env,
        "q.json",
        to_review(vec![turn(vec![fail(
            "QuotaExceeded",
            "You've hit your usage limit",
        )])]),
    );
    let (code, err) = toryo(env, &["run", id, "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    assert_eq!(task(env, id).state, St::Paused, "{err}");
    let w = wt(env, id);
    std::fs::write(w.join("review-notes.txt"), "what to look at\n").unwrap();
    w
}

/// F5: a file that was in the worktree before the review turn is not the reviewer's write
/// — only what changes during the turn is (was: `policy_violation` → Escalated).
#[test]
fn ec31_a_file_left_before_the_review_turn_is_not_the_reviewers_write() {
    let env = setup("codex-openai");
    let w = paused_in_review_with_a_human_file(&env, "H-3");
    let s2 = script(
        &env,
        "h3.json",
        json!({"reviewer": [turn(vec![approved()])]}),
    );
    let (code, err) = toryo(&env, &["resume", "H-3", "--yes", "--fake-agents", &s2], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "H-3").state, St::Completed, "{err}");
    assert!(!err.contains("policy_violation"), "{err}");
    let review = stage(&env, "H-3", "review");
    assert_eq!(review.last().unwrap().status, AttemptStatus::Succeeded);
    // the human's file is left alone: still there, never committed
    assert_eq!(
        std::fs::read_to_string(w.join("review-notes.txt")).unwrap(),
        "what to look at\n"
    );
    let log = git(
        &w,
        &["log", "--format=%h", "main..HEAD", "--", "review-notes.txt"],
    );
    assert!(log.trim().is_empty(), "{log}");
}

/// … but the reviewer changing that file during its turn is a write like any other.
#[test]
fn ec31_a_reviewer_that_changes_a_file_left_before_its_turn_escalates() {
    let env = setup("codex-openai");
    let w = paused_in_review_with_a_human_file(&env, "H-4");
    let s2 = script(
        &env,
        "h4.json",
        json!({"reviewer": [turn(vec![write("review-notes.txt", "reviewer was here\n"), approved()])]}),
    );
    let (code, err) = toryo(&env, &["resume", "H-4", "--yes", "--fake-agents", &s2], &[]);
    assert_eq!(code, 1, "{err}");
    assert_eq!(task(&env, "H-4").state, St::Escalated, "{err}");
    assert_eq!(
        reason(&env, "H-4"),
        "policy_violation: the worktree changed during a read-only stage: [\"review-notes.txt\"]"
    );
    assert!(std::fs::read_to_string(w.join("review-notes.txt"))
        .unwrap()
        .ends_with("reviewer was here\n"));
}

/// The Planner likewise: a file it wrote stops the task before the plan gate.
#[test]
fn ec31_a_planner_write_is_a_policy_violation() {
    let env = setup("codex-openai");
    create(&env, "H-2");
    let draft = json!({"kind":"write_file","path":"notes/draft.md","content":"# plan draft\n"});
    let s = script(
        &env,
        "h2.json",
        json!({"planner": [turn(vec![draft, plan()])]}),
    );
    let (code, err) = toryo(&env, &["run", "H-2", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    assert_eq!(task(&env, "H-2").state, St::Escalated, "{err}");
    assert_eq!(
        reason(&env, "H-2"),
        "policy_violation: the worktree changed during a read-only stage: [\"notes/draft.md\"]"
    );
    let p = stage(&env, "H-2", "plan");
    assert_eq!(
        (p[0].status, p[0].error_class.as_deref()),
        (AttemptStatus::Failed, Some("policy_violation"))
    );
    assert!(approvals(&env, "H-2").is_empty(), "no plan gate");
}

// ── item 9: the verify feedback names the exit code in words ─────────────────────────

#[test]
fn item9_the_verify_feedback_shows_the_exit_code() {
    let env = setup("codex-openai");
    create(&env, "E-9");
    let s = script(
        &env,
        "e9.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![implementation()]), turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "E-9", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "E-9").state, St::Completed, "{err}");
    let fix = &calls_of(&env, "E-9", "implementer")[1];
    let p = fix["prompt"].as_str().unwrap();
    assert!(
        p.contains("- VERIFY FAILED: `divide-present` exited 1. Last output:"),
        "{p}"
    );
    assert!(!p.contains("Number(") && !p.contains("Null"), "{p}");
}

// ── item 10: a relative verify program is a file of the checkout ─────────────────────

/// `scripts/gate.sh` runs from the verify checkout of the checkpoint — never a file with the
/// same relative path in toryo's own working directory (what Windows would resolve).
#[test]
#[cfg_attr(not(unix), ignore = "the verify program is an sh script")]
fn item10_a_relative_verify_program_runs_from_the_checkout() {
    let env = setup("codex-openai");
    let gate = env.repo.join("scripts").join("gate.sh");
    std::fs::write(&gate, "#!/bin/sh\ngit grep -q Divide-Numbers -- src\n").unwrap();
    // a decoy at the same relative path under toryo's working directory (`env.root`)
    let mark = env.root.join("decoy-ran");
    let decoy = env.root.join("scripts").join("gate.sh");
    std::fs::create_dir_all(decoy.parent().unwrap()).unwrap();
    std::fs::write(
        &decoy,
        format!("#!/bin/sh\necho ran > '{}'\nexit 1\n", mark.display()),
    )
    .unwrap();
    #[cfg(unix)]
    for f in [&gate, &decoy] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    commit_repo(&env, &[], "add gate");
    let cfg = std::fs::read_to_string(&env.config).unwrap().replace(
        r#"{ name: divide-present, program: git, args: ["grep", "-q", "Divide-Numbers", "--", "src"] }"#,
        r#"{ name: gate, program: scripts/gate.sh, args: [] }"#,
    );
    std::fs::write(&env.config, cfg).unwrap();
    create(&env, "G-1");
    let s = script(&env, "g1.json", to_review(vec![turn(vec![approved()])]));
    let (code, err) = toryo(&env, &["run", "G-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "G-1").state, St::Completed, "{err}");
    let vr: Value = serde_json::from_slice(
        &std::fs::read(task(&env, "G-1").artifacts().join("VERIFY_RESULT.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(vr["commands"][0]["exit"], json!(0), "{vr}");
    assert!(
        !mark.exists(),
        "the decoy in toryo's working directory never ran"
    );
}
