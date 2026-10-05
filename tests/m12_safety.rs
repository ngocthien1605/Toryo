//! M12 safety tests that do not belong to an older file.
//!
//! - M12-03 (H6): a read-only role (Planner, Reviewer) that wrote a file and whose turn then
//!   ended by quota, by a stop or by the live budget is a policy violation like on the normal
//!   path — the pause or stop does not hide the write.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use toryo::engine::store::AttemptStatus;
use toryo::engine::PipelineState as St;

fn tokens(input: u64, output: u64) -> Value {
    json!({"kind":"usage","input":input,"output":output})
}

fn reason(env: &Env, id: &str) -> String {
    task(env, id).machine.reason().unwrap_or("").to_string()
}

/// The file the read-only role wrote is in no commit of the task branch.
fn in_no_checkpoint(env: &Env, id: &str, file: &str) -> bool {
    let log = git(
        &wt(env, id),
        &["log", "--name-only", "--format=", "main..HEAD"],
    );
    !log.lines().any(|l| l.trim() == file)
}

const WROTE: &str = "policy_violation: the worktree changed during a read-only stage";

/// AC-1: the reviewer writes a file, then hits its quota. Was: `Paused(reviewer_quota)`, the
/// write unseen; after `resume` the file stayed in the tree and went on under the
/// implementer's name. Now: escalated as a policy violation, at once.
#[test]
fn m12_03_a_reviewer_that_wrote_and_then_hit_its_quota_is_a_policy_violation() {
    let env = setup("codex-openai");
    create(&env, "S-1");
    let s = script(
        &env,
        "s1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![
                write("src/Reviewed.txt", "the reviewer edited this\n"),
                fail("QuotaExceeded", "You've hit your limit"),
            ])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "S-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "S-1");
    assert_eq!(t.state, St::Escalated, "not paused: {err}");
    assert!(
        reason(&env, "S-1").starts_with(WROTE),
        "{}",
        reason(&env, "S-1")
    );
    assert!(in_no_checkpoint(&env, "S-1", "src/Reviewed.txt"));
    let a = attempts(&env, "S-1")
        .into_iter()
        .find(|a| a.stage == "review")
        .unwrap();
    assert_eq!(a.status, AttemptStatus::Failed);
    assert_eq!(a.error_class.as_deref(), Some("policy_violation"));
}

/// AC-3: the same for the planner.
#[test]
fn m12_03_a_planner_that_wrote_and_then_hit_its_quota_is_a_policy_violation() {
    let env = setup("codex-openai");
    create(&env, "S-3");
    let s = script(
        &env,
        "s3.json",
        json!({
            "planner": [turn(vec![
                write("src/Planned.txt", "the planner edited this\n"),
                fail("QuotaExceeded", "You've hit your limit"),
            ])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "S-3", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "S-3");
    assert_eq!(t.state, St::Escalated, "not paused: {err}");
    assert!(
        reason(&env, "S-3").starts_with(WROTE),
        "{}",
        reason(&env, "S-3")
    );
}

/// AC-2: the same when the live budget interrupts the reviewer's turn.
#[test]
fn m12_03_a_reviewer_that_wrote_and_was_interrupted_by_the_budget_is_a_policy_violation() {
    let env = setup_with("codex-openai", "pipeline:\n  budget_tokens: 1000\n");
    create(&env, "S-2b");
    let s = script(
        &env,
        "s2b.json",
        json!({
            "planner": [turn(vec![tokens(100, 0), plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), tokens(100, 0), implementation()])],
            "reviewer": [turn(vec![
                write("src/Reviewed.txt", "the reviewer edited this\n"),
                tokens(5000, 10),
                hang(),
            ])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "S-2b", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "S-2b");
    assert_eq!(t.state, St::Escalated, "not paused: {err}");
    assert!(
        reason(&env, "S-2b").starts_with(WROTE),
        "{}",
        reason(&env, "S-2b")
    );
    assert!(in_no_checkpoint(&env, "S-2b", "src/Reviewed.txt"));
}

/// AC-2: the same when `toryo stop` ends the reviewer's turn.
#[test]
fn m12_03_a_reviewer_that_wrote_and_was_stopped_is_a_policy_violation() {
    let env = setup("codex-openai");
    create(&env, "S-2s");
    let s = script(
        &env,
        "s2s.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![write("src/Reviewed.txt", "the reviewer edited this\n"), hang()])],
        }),
    );
    let mut child = spawn(
        &env,
        &["run", "S-2s", "--yes", "--fake-agents", &s],
        "s2s.log",
    );
    wait_until("the reviewer to run", 60, || {
        attempts(&env, "S-2s").iter().any(|a| {
            a.stage == "review"
                && a.status == AttemptStatus::Running
                && a.external_session_id.is_some()
        })
    });
    // the write must have landed before the stop
    wait_until("the reviewer's write", 30, || {
        wt(&env, "S-2s").join("src/Reviewed.txt").exists()
    });
    let (code, err) = toryo(&env, &["stop", "S-2s"], &[]);
    assert_eq!(code, 0, "{err}");
    child.wait().unwrap();
    let t = task(&env, "S-2s");
    assert_eq!(t.state, St::Escalated, "a stop does not hide the write");
    assert!(
        reason(&env, "S-2s").starts_with(WROTE),
        "{}",
        reason(&env, "S-2s")
    );
    assert!(in_no_checkpoint(&env, "S-2s", "src/Reviewed.txt"));
}
