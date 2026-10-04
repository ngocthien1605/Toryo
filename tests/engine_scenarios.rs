//! The prototype's scenarios on the engine (TS-06, R52: `spike pipeline` and its fake-agent
//! tests are the executable spec for the engine). Each test ports one scenario of
//! `tests/pipeline_fake.rs` (or `spike_e2e.rs`) to `toryo run` / `approve` / `reject` /
//! `resume` with fake agents, and asserts what the plan specifies (§5.2, §9, §11, R22, R28,
//! R44a, R49):
//!
//! - `final_gate_rejection_is_a_fix_cycle` (pipeline_fake: same name)
//! - `max_fix_cycles_escalates` (same name)
//! - `automatic_replans_are_bounded` (`automatic_plan_rejections_are_bounded`)
//! - `human_plan_rejections_are_unlimited` (same name)
//! - `blocked_review_escalates` (same name)
//! - `partial_ac_review_gets_a_correction_turn` (`rate_limit_retry_verify_failure_and_review_correction`)
//! - `exhausted_fallback_chain_pauses_and_keeps_partial_work` (`all_implementers_out_of_quota_…`)
//! - `a_task_id_is_never_reused` (`spike_e2e::pipeline_plan_gate_fix_cycle_and_approval`, end)
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use toryo::engine::store::{self, AttemptRow, AttemptStatus};
use toryo::engine::PipelineState as St;
use toryo::engine::MAX_PLAN_AUTO_REJECTIONS;

/// The states the task went through (`to` of every transition), in order.
fn states(env: &Env, id: &str) -> Vec<String> {
    transitions(env, id)
        .iter()
        .map(|t| t["to"].as_str().unwrap_or("").to_string())
        .collect()
}

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

fn stage(env: &Env, id: &str, stage: &str) -> Vec<AttemptRow> {
    attempts(env, id)
        .into_iter()
        .filter(|a| a.stage == stage)
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

fn decisions(env: &Env, id: &str) -> String {
    std::fs::read_to_string(task(env, id).artifacts().join("DECISIONS.md")).unwrap_or_default()
}

fn prompts_of(env: &Env, id: &str, key: &str) -> Vec<Value> {
    calls(env, id)
        .into_iter()
        .filter(|c| c["key"] == key)
        .collect()
}

fn blocked() -> Value {
    json!({"kind":"complete","structured":{"decision":"blocked","summary":"the task contradicts AGENTS.md","acceptance_criteria":[
        {"id":"AC-1","met":false,"evidence":"e1"},{"id":"AC-2","met":false,"evidence":"e2"},{"id":"AC-3","met":false,"evidence":"e3"}],
        "issues":[{"id":"I-1","severity":"high","blocking":true,"description":"TASK.md contradicts AGENTS.md"}]}})
}

fn plan_touching(path: &str) -> Value {
    json!({"kind":"complete","structured":{"understanding":"u","files_likely_involved":[path],"steps":["x"],"validation":["tests"],"plan_markdown":"md"}})
}

fn ok(env: &Env, args: &[&str]) {
    let (code, err) = toryo(env, args, &[]);
    assert_eq!(code, 0, "toryo {args:?}: {err}");
}

// ── 1. final-gate rejection (§5.2: WaitingFinalApproval ─reject→ Fixing) ─────────────

#[test]
fn final_gate_rejection_is_a_fix_cycle() {
    let env = setup("codex-openai");
    create(&env, "S-F");
    let s = script(
        &env,
        "f1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    ok(&env, &["run", "S-F", "--fake-agents", &s]);
    ok(
        &env,
        &[
            "approve",
            "S-F",
            "--gate",
            "plan",
            "--continue",
            "--fake-agents",
            &s,
        ],
    );
    let t = task(&env, "S-F");
    assert_eq!(t.state, St::WaitingFinalApproval);
    let first = t.machine.checkpoint().unwrap().to_string();
    let reason = "rename the parameters to -Dividend/-Divisor";
    ok(
        &env,
        &["reject", "S-F", "--gate", "final", "--reason", reason],
    );
    let t = task(&env, "S-F");
    assert_eq!((t.state, t.fix_cycle), (St::Fixing, 1));
    // the fix turn carries the human's reason; the new checkpoint is verified and reviewed
    let s2 = script(
        &env,
        "f2.json",
        json!({
            "implementer": [turn(vec![write("src/Calculator.psm1", "# -Dividend/-Divisor\n"), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    ok(&env, &["run", "S-F", "--fake-agents", &s2]);
    let t = task(&env, "S-F");
    assert_eq!(t.state, St::WaitingFinalApproval);
    assert_ne!(t.machine.checkpoint(), Some(first.as_str()));
    let fix = &prompts_of(&env, "S-F", "implementer")[0];
    assert!(
        fix["prompt"]
            .as_str()
            .unwrap()
            .contains(&format!("HUMAN (final gate): {reason}")),
        "{fix}"
    );
    ok(&env, &["approve", "S-F", "--gate", "final"]);
    assert_eq!(task(&env, "S-F").state, St::Completed);
    assert_eq!(
        states(&env, "S-F"),
        [
            "Planning",
            "WaitingPlanApproval",
            "Implementing",
            "Checkpointing",
            "Verifying",
            "Reviewing",
            "WaitingFinalApproval",
            "Fixing",
            "Checkpointing",
            "Verifying",
            "Reviewing",
            "WaitingFinalApproval",
            "Completed",
        ]
    );
    assert!(transitions(&env, "S-F")
        .iter()
        .any(|t| t["event"]["event"] == "final_rejected" && t["event"]["reason"] == reason));
    // R44a: the rejected occurrence stays rejected; the approval is a new occurrence
    assert_eq!(
        approvals(&env, "S-F"),
        [
            ("plan".to_string(), 1, "approved".to_string()),
            ("final".to_string(), 2, "rejected".to_string()),
            ("final".to_string(), 3, "approved".to_string()),
        ]
    );
    let dec = decisions(&env, "S-F");
    assert!(
        dec.contains("final gate rejected") && dec.contains(reason),
        "{dec}"
    );
    let log = trailers(&env, "S-F");
    assert!(
        log.contains("|implement|1||") && log.contains("|fix-1|1||"),
        "{log}"
    );
}

// ── 2. max fix cycles → Escalated (§5.2, `max_fix_cycles` is the single bound) ───────

#[test]
fn max_fix_cycles_escalates() {
    let env = setup("codex-openai");
    ok(
        &env,
        &[
            "task",
            "create",
            "--file",
            "TASK.md",
            "--id",
            "S-M",
            "--max-fix-cycles",
            "1",
        ],
    );
    let s = script(
        &env,
        "m.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]),
                turn(vec![write("tests/Divide.Tests.ps1", "# t\n"), implementation()]),
                turn(vec![write("tests/Divide.Tests.ps1", "# never\n"), implementation()]),
            ],
            "reviewer": [turn(vec![changes()]), turn(vec![changes()]), turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "S-M", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "S-M");
    assert_eq!(t.state, St::Escalated, "{err}");
    let why = t.machine.reason().unwrap();
    assert!(
        why.contains("not accepted after 1 fix cycle(s)") && why.contains("changes requested"),
        "{why}"
    );
    assert_eq!(t.fix_cycle, 1);
    assert_eq!(
        prompts_of(&env, "S-M", "implementer").len(),
        2,
        "implement + 1 fix, then stop"
    );
    assert_eq!(stage(&env, "S-M", "review").len(), 2);
    assert!(
        approvals(&env, "S-M").iter().all(|a| a.0 == "plan"),
        "no final gate"
    );
    assert_eq!(states(&env, "S-M").last().unwrap(), "Escalated");
    // M11-R17 (R11 AC-3, at the CLI): the fix-cycle bound is not a machine-caused stop — a
    // plain `toryo resume` does nothing; the way on is `--note`
    assert_eq!(t.machine.reason_class(), None);
    let before = (states(&env, "S-M").len(), attempts(&env, "S-M").len());
    let (_, err) = toryo(&env, &["resume", "S-M", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "S-M");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(t.fix_cycle, 1);
    assert_eq!(
        (states(&env, "S-M").len(), attempts(&env, "S-M").len()),
        before,
        "no transition, no turn: {err}"
    );
    assert!(
        err.contains("`toryo resume S-M --note \"…\"` (another fix cycle)"),
        "{err}"
    );
    let (_, err) = toryo(
        &env,
        &[
            "resume",
            "S-M",
            "--note",
            "one more",
            "--yes",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    let t = task(&env, "S-M");
    assert_eq!(t.fix_cycle, 2, "the note grants a cycle: {err}");
}

// ── 3. the automatic re-plan bound (owner decision M2, MAX_PLAN_AUTO_REJECTIONS) ─────

#[test]
fn automatic_replans_are_bounded() {
    let env = setup("codex-openai");
    create(&env, "S-A");
    // TASK.md is the contract: a plan that intends to change it is rejected at plan time
    let s = script(
        &env,
        "a.json",
        json!({
            "planner": (0..4).map(|_| turn(vec![plan_touching("TASK.md")])).collect::<Vec<_>>(),
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "S-A", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "S-A");
    assert_eq!(t.state, St::Escalated, "{err}");
    let why = t.machine.reason().unwrap();
    assert!(
        why.contains("TASK.md")
            && why.contains(&format!(
                "after {MAX_PLAN_AUTO_REJECTIONS} automatic re-plans"
            )),
        "{why}"
    );
    let planner = prompts_of(&env, "S-A", "planner");
    assert_eq!(
        planner.len() as u32,
        MAX_PLAN_AUTO_REJECTIONS + 1,
        "the first plan and the bounded re-plans"
    );
    for p in &planner[1..] {
        assert!(
            p["prompt"]
                .as_str()
                .unwrap()
                .contains("protected or denied paths [\"TASK.md\"]"),
            "{p}"
        );
    }
    assert!(prompts_of(&env, "S-A", "implementer").is_empty());
    assert!(approvals(&env, "S-A").is_empty(), "no plan gate was opened");
}

// ── 4. human plan rejections are unlimited (owner decision M2) ───────────────────────

#[test]
fn human_plan_rejections_are_unlimited() {
    let env = setup("codex-openai");
    create(&env, "S-H");
    let s = script(
        &env,
        "h.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    ok(&env, &["run", "S-H", "--fake-agents", &s]);
    // four rejections (the prototype used to stop at three), each answered by a revision
    for n in 1..=4 {
        let reason = format!("revision {n}: also cover negative numbers");
        ok(
            &env,
            &["reject", "S-H", "--gate", "plan", "--reason", &reason],
        );
        assert_eq!(task(&env, "S-H").state, St::Planning);
        ok(&env, &["run", "S-H", "--fake-agents", &s]);
        assert_eq!(task(&env, "S-H").state, St::WaitingPlanApproval);
        let p = &prompts_of(&env, "S-H", "planner")[0];
        assert!(p["prompt"].as_str().unwrap().contains(&reason), "{p}");
        assert!(
            p["resume"].is_string(),
            "the Planner revises in its session (R48): {p}"
        );
    }
    ok(
        &env,
        &[
            "approve",
            "S-H",
            "--gate",
            "plan",
            "--continue",
            "--fake-agents",
            &s,
        ],
    );
    ok(&env, &["approve", "S-H", "--gate", "final"]);
    let t = task(&env, "S-H");
    assert_eq!(t.state, St::Completed);
    assert_eq!(t.machine.plan_attempt(), 5);
    assert_eq!(stage(&env, "S-H", "plan").len(), 5);
    let plan_gates: Vec<(u32, String)> = approvals(&env, "S-H")
        .into_iter()
        .filter(|a| a.0 == "plan")
        .map(|a| (a.1, a.2))
        .collect();
    assert_eq!(
        plan_gates,
        [
            (1, "rejected".to_string()),
            (2, "rejected".to_string()),
            (3, "rejected".to_string()),
            (4, "rejected".to_string()),
            (5, "approved".to_string()),
        ]
    );
}

// ── 5. a blocked review escalates (§9 rule 6) ────────────────────────────────────────

#[test]
fn blocked_review_escalates() {
    let env = setup("codex-openai");
    create(&env, "S-B");
    let s = script(
        &env,
        "b.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]), turn(vec![implementation()])],
            "reviewer": [turn(vec![blocked()]), turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "S-B", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "S-B");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(t.machine.reason(), Some("the reviewer blocked the task"));
    assert_eq!(t.fix_cycle, 0, "blocked is not a fix cycle");
    let review = stage(&env, "S-B", "review");
    assert_eq!(review.len(), 1);
    assert_eq!(review[0].status, AttemptStatus::Succeeded);
    assert_eq!(prompts_of(&env, "S-B", "implementer").len(), 1);
    // the review is attached to the escalation
    let md = std::fs::read_to_string(t.artifacts().join("REVIEW.md")).unwrap();
    assert!(md.contains("TASK.md contradicts AGENTS.md"), "{md}");
    assert!(
        approvals(&env, "S-B").iter().all(|a| a.0 == "plan"),
        "no final gate"
    );
}

// ── 6. a partial AC set gets a correction turn (R22, R28) ────────────────────────────

#[test]
fn partial_ac_review_gets_a_correction_turn() {
    let env = setup("codex-openai");
    create(&env, "S-C");
    let missing_ac3 = json!({"kind":"complete","structured":{"decision":"approved","summary":"s","acceptance_criteria":[
        {"id":"AC-1","met":true,"evidence":"e1"},{"id":"AC-2","met":true,"evidence":"e2"}],"issues":[]}});
    let s = script(
        &env,
        "c.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![missing_ac3]), turn(vec![approved()])],
        }),
    );
    ok(&env, &["run", "S-C", "--yes", "--fake-agents", &s]);
    assert_eq!(task(&env, "S-C").state, St::Completed);
    let review = stage(&env, "S-C", "review");
    assert_eq!(
        review.len(),
        1,
        "the correction is a turn, not a new attempt"
    );
    assert_eq!(review[0].status, AttemptStatus::Succeeded);
    assert_eq!(review[0].correction_turns, 1);
    let turns = prompts_of(&env, "S-C", "reviewer");
    assert_eq!(turns.len(), 2);
    assert!(turns[0]["resume"].is_null());
    assert_eq!(
        turns[1]["resume"].as_str(),
        review[0].external_session_id.as_deref(),
        "the correction resumes the reviewer's session"
    );
    let p = turns[1]["prompt"].as_str().unwrap();
    assert!(
        p.contains("was rejected and was not used")
            && p.contains("missing acceptance criteria")
            && p.contains("AC-3"),
        "{p}"
    );
    assert_eq!(turns[1]["label"], "review-1-1");
}

// ── 7. the fallback chain exhausted, without --no-fallback (§11, R49, R58 M5) ────────

#[test]
fn exhausted_fallback_chain_pauses_and_keeps_partial_work() {
    let env = setup("codex-openai");
    create(&env, "S-X");
    let t = task(&env, "S-X");
    // no configured fallback: the planner's Claude profile is the implicit one
    assert_eq!(t.implementers, ["codex-openai", "claude-max"]);
    let s = script(
        &env,
        "x.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer@codex-openai": [turn(vec![write("src/Partial.ps1", "# codex\n"), fail("QuotaExceeded", "You've hit your usage limit")])],
            "implementer@claude-max": [turn(vec![write("src/Partial2.ps1", "# claude\n"), fail("QuotaExceeded", "usage limit reached")])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "S-X", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "S-X");
    assert_eq!(t.state, St::Paused, "{err}");
    assert!(
        t.machine
            .reason()
            .unwrap()
            .starts_with("implementer_quota: no implementer left"),
        "{:?}",
        t.machine.reason()
    );
    assert_eq!(t.machine.paused_from(), Some(St::Implementing));
    let imp: Vec<(Option<String>, AttemptStatus, Option<String>)> = stage(&env, "S-X", "implement")
        .into_iter()
        .map(|a| (a.profile, a.status, a.error_class))
        .collect();
    let quota = (AttemptStatus::Failed, Some("QuotaExceeded".to_string()));
    assert_eq!(
        imp,
        [
            (Some("codex-openai".into()), quota.0, quota.1.clone()),
            (Some("claude-max".into()), quota.0, quota.1.clone()),
        ]
    );
    // the partial work of both is kept, uncommitted
    let w = wt(&env, "S-X");
    assert!(w.join("src/Partial.ps1").exists() && w.join("src/Partial2.ps1").exists());
    assert!(git(&w, &["log", "--oneline", "main..HEAD"])
        .trim()
        .is_empty());
    // the quota is back: `resume` restarts the chain from the primary, with the hand-over
    let s2 = script(
        &env,
        "x2.json",
        json!({
            "implementer@codex-openai": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    ok(&env, &["resume", "S-X", "--yes", "--fake-agents", &s2]);
    let t = task(&env, "S-X");
    assert_eq!(t.state, St::Completed);
    let imp = prompts_of(&env, "S-X", "implementer@codex-openai");
    assert_eq!(imp.len(), 1);
    let p = imp[0]["prompt"].as_str().unwrap();
    assert!(
        p.contains("Hand-over") && p.contains("src/Partial2.ps1"),
        "{p}"
    );
    let files = git(&w, &["log", "--name-only", "--format=", "main..HEAD"]);
    assert!(
        files.contains("src/Partial.ps1") && files.contains("src/Partial2.ps1"),
        "the partial work was kept: {files}"
    );
}

// ── 8. a task id is never reused (spike_e2e: "refuses to clobber the worktree") ─────

#[test]
fn a_task_id_is_never_reused() {
    let env = setup("codex-openai");
    create(&env, "S-R");
    let s = script(&env, "r.json", json!({ "planner": [turn(vec![plan()])] }));
    ok(&env, &["run", "S-R", "--fake-agents", &s]);
    let before = task(&env, "S-R");
    let head = git(&before.worktree, &["rev-parse", "HEAD"]);
    let create_again = |id: &str| {
        toryo(
            &env,
            &["task", "create", "--file", "TASK.md", "--id", id],
            &[],
        )
    };
    let (code, err) = create_again("S-R");
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("task S-R already exists"), "{err}");
    let after = task(&env, "S-R");
    assert_eq!(after.state, St::WaitingPlanApproval, "nothing changed");
    assert_eq!(after.run_store, before.run_store);
    assert!(after.worktree.join("TASK.md").exists());
    assert_eq!(git(&after.worktree, &["rev-parse", "HEAD"]), head);
    // a branch `toryo/<id>` left by an earlier data directory is not clobbered either …
    git(&env.repo, &["branch", "toryo/S-Z"]);
    let (code, err) = create_again("S-Z");
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("branch toryo/S-Z already exists"), "{err}");
    // … nor a leftover worktree directory; and a refused create leaves nothing behind
    std::fs::create_dir_all(wt(&env, "S-Y")).unwrap();
    let (code, err) = create_again("S-Y");
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("already exists"), "{err}");
    for id in ["S-Z", "S-Y"] {
        let i = id.to_string();
        assert!(
            db(&env).call(move |tx| store::get_task(tx, &i)).is_err(),
            "{id}: no task row"
        );
        assert!(
            !env.data.join("runs").join(id).exists(),
            "{id}: no run store"
        );
    }
    assert!(!wt(&env, "S-Z").exists());
    assert!(git(&env.repo, &["branch", "--list", "toryo/S-Y"])
        .trim()
        .is_empty());
}
