//! Regression tests for the Implementer-path findings of the 2026-09-29 review: the probes
//! that pinned each defect (`probes/probe_engine_flows.rs`, `probe_engine_core.rs`),
//! inverted to pin the fix, and the edges around them.
//!
//! - EC-18 the HEAD guard runs on every exit path of an Implementer turn: an agent commit in
//!   a turn that ends in a quota switch is undone before the switch (the denied file never
//!   reaches the branch); an unfinished Git operation, another branch or a detached HEAD
//!   escalates without a reset, whether the turn completes, hits a quota or a rate limit, is
//!   cut short by the budget or stopped
//! - EC-15 a failed turn that left changes is `Failed` with its class; its work is kept in a
//!   recovery checkpoint (`Toryo-Partial`) and the stage runs again from it once — a second
//!   such failure, or a denial, escalates with the work kept
//! - EC-16 `--no-fallback` after a sticky switch continues with the current implementer and
//!   forbids only a new switch
//! - EC-17 `toryo stop` during a rate-limit backoff is answered within the R38 bound
//! - CODE-03 a switch records the turn's real error class and detail
//! - F9 the hand-over after a switch names the partial work the stage already committed
//!
//! Crash consistency of the EC-15 decision (F8) is in `regress_recovery.rs`.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use toryo::engine::store::{self, AttemptRow, AttemptStatus, Store};
use toryo::engine::PipelineState as St;

fn attempt(env: &Env, id: &str) -> AttemptRow {
    let task_id = id.split('/').next().unwrap();
    attempts(env, task_id)
        .into_iter()
        .find(|a| a.id == id)
        .unwrap_or_else(|| panic!("no attempt {id}"))
}

fn events(env: &Env, id: &str, kind: &str) -> Vec<Value> {
    let i = id.to_string();
    db(env)
        .call(move |tx| store::events(tx, &i))
        .unwrap()
        .into_iter()
        .filter(|e| e.1 == kind)
        .map(|e| e.2)
        .collect()
}

/// The prompts the fake implementers got in the latest run.
fn implementer_prompts(env: &Env, id: &str) -> Vec<String> {
    calls(env, id)
        .into_iter()
        .filter(|c| c["key"].as_str().unwrap_or("").starts_with("implementer"))
        .map(|c| c["prompt"].as_str().unwrap_or("").to_string())
        .collect()
}

fn tokens(input: u64, output: u64) -> Value {
    json!({"kind":"usage","input":input,"output":output})
}

fn head_of(dir: &Path, rev: &str) -> String {
    git(dir, &["rev-parse", rev]).trim().to_string()
}

fn wait_for_running(env: &Env, id: &str, stage: &str) {
    let (data, i, s) = (env.data.clone(), id.to_string(), stage.to_string());
    wait_until(&format!("{id} {stage} to run"), 60, || {
        let st = Store::open(&data.join("toryo.db")).unwrap();
        let (i, s) = (i.clone(), s.clone());
        st.call(move |tx| store::last_attempt(tx, &i, &s))
            .ok()
            .flatten()
            .is_some_and(|a| a.status == AttemptStatus::Running && a.external_session_id.is_some())
    });
}

// ── EC-18 (was probe_engine_core::k_commit_during_quota_switch_escapes_head_guard) ──────

#[test]
fn ec18_an_agent_commit_in_a_turn_that_ends_in_a_quota_switch_is_undone() {
    let env = setup_with("codex-openai", "policy:\n  deny_paths: [\"**/ZZZ/**\"]\n");
    create(&env, "K-9");
    let w = wt(&env, "K-9");
    let base = head_of(&w, "HEAD");
    // An agent's `git add ZZZ/secret.txt && git commit` (a shell and a misconfigured
    // sandbox), without timing: the commit object is written up front through a private
    // index, so the task worktree stays clean (a dirty tree after the planner's read-only
    // turn is itself a policy violation, EC-31); the Codex turn then writes the file and
    // moves the task branch to that commit, as `git commit` does.
    let secret = env.root.join("secret.txt");
    std::fs::write(&secret, "denied\n").unwrap();
    let blob = git(&w, &["hash-object", "-w", secret.to_str().unwrap()])
        .trim()
        .to_string();
    let index = env.root.join("agent.index");
    let with_index = |args: &[&str]| -> String {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(&w)
            .args(args)
            .env("GIT_INDEX_FILE", &index)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    with_index(&["read-tree", &base]);
    with_index(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("100644,{blob},ZZZ/secret.txt"),
    ]);
    let tree = with_index(&["write-tree"]);
    let agent = git(
        &w,
        &[
            "-c",
            "user.name=agent",
            "-c",
            "user.email=a@a",
            "commit-tree",
            &tree,
            "-p",
            &base,
            "-m",
            "agent commit",
        ],
    )
    .trim()
    .to_string();
    let branch = env.repo.join(".git/refs/heads/toryo/K-9");
    assert!(branch.is_file(), "the task branch is a loose ref");
    let commit = json!({"kind":"write_file","path":branch,"content":format!("{agent}\n")});
    let s = script(
        &env,
        "k1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), write("ZZZ/secret.txt", "denied\n"), commit, fail("QuotaExceeded", "You've hit your usage limit")])],
            "implementer@claude-max": [turn(vec![implementation()]), turn(vec![implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "K-9", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "K-9");
    assert_eq!(t.state, St::Completed, "{err}");
    // the guard ran before the switch: soft reset, recorded (was: 0 policy_violation events)
    let pv = events(&env, "K-9", "policy_violation");
    assert_eq!(pv.len(), 1, "{pv:?}");
    assert_eq!(
        (
            &pv[0]["kind"],
            &pv[0]["action"],
            &pv[0]["head"],
            &pv[0]["reset_to"]
        ),
        (
            &json!("agent commit"),
            &json!("soft reset"),
            &json!(agent),
            &json!(base)
        )
    );
    let a1 = attempt(&env, "K-9/implement/1");
    assert_eq!(
        (
            a1.status,
            a1.error_class.as_deref(),
            a1.git_head_before.as_deref()
        ),
        (
            AttemptStatus::Failed,
            Some("QuotaExceeded"),
            Some(base.as_str())
        )
    );
    // the fallback began where the Codex turn began, not at the agent commit
    let a2 = attempt(&env, "K-9/implement/2");
    assert_eq!(a2.profile.as_deref(), Some("claude-max"));
    assert_eq!(a2.git_head_before.as_deref(), Some(base.as_str()));
    // the undone commit's file was handed over uncommitted, then caught by the path policy
    let prompts = implementer_prompts(&env, "K-9");
    assert!(prompts[1].contains("ZZZ/secret.txt"), "{}", prompts[1]);
    assert_eq!(a2.error_class.as_deref(), Some("policy_violation"));
    assert!(a2.error_detail.unwrap().contains("ZZZ/secret.txt"));
    assert!(
        !w.join("ZZZ/secret.txt").exists(),
        "restored: the denied file is gone"
    );
    assert_eq!(t.fix_cycle, 1);
    // nothing of it reaches the task branch (was: `ZZZ/secret.txt` at HEAD)
    let history = git(&w, &["log", "--format=%H %s", "--name-only", "main..HEAD"]);
    assert!(!history.contains("ZZZ/"), "{history}");
    assert!(!history.contains(&agent), "{history}");
    assert!(git(&w, &["show", "HEAD:src/Calculator.psm1"]).contains("Divide-Numbers"));
    assert!(clean(&w));
}

#[test]
fn ec18_an_agent_commit_in_a_stopped_turn_is_undone_before_recovery() {
    let env = setup_with("codex-openai", "policy:\n  deny_paths: [\"**/ZZZ/**\"]\n");
    create(&env, "K-2");
    let s = script(
        &env,
        "k2.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), hang()])],
        }),
    );
    let mut child = spawn(
        &env,
        &["run", "K-2", "--yes", "--fake-agents", &s],
        "k2.log",
    );
    wait_for_running(&env, "K-2", "implement");
    let w = wt(&env, "K-2");
    let base = head_of(&w, "HEAD");
    std::fs::create_dir_all(w.join("ZZZ")).unwrap();
    std::fs::write(w.join("ZZZ/secret.txt"), "denied\n").unwrap();
    git(&w, &["add", "ZZZ/secret.txt"]);
    git(
        &w,
        &[
            "-c",
            "user.name=agent",
            "-c",
            "user.email=a@a",
            "commit",
            "-q",
            "-m",
            "agent commit",
        ],
    );
    let (code, err) = toryo(&env, &["stop", "K-2", "--wait-secs", "30"], &[]);
    assert_eq!(code, 0, "{err}");
    child.wait().unwrap();
    // the stopped turn's commit was undone before the attempt ended (was: kept, so the
    // recovery saw a clean tree and the next attempt started from the agent commit)
    let a1 = attempt(&env, "K-2/implement/1");
    assert_eq!(a1.status, AttemptStatus::Interrupted);
    assert_eq!(head_of(&w, "HEAD"), base);
    assert_eq!(
        events(&env, "K-2", "policy_violation")[0]["action"],
        json!("soft reset")
    );
    // … so the recovery finds the denied file among the leftovers and commits nothing
    let s2 = script(
        &env,
        "k2b.json",
        json!({"implementer": [turn(vec![implementation()])]}),
    );
    let (code, err) = toryo(&env, &["resume", "K-2", "--yes", "--fake-agents", &s2], &[]);
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "K-2");
    assert_eq!(t.state, St::Escalated, "{err}");
    let why = t.machine.reason().unwrap();
    assert!(
        why.starts_with("recovery_policy_violation") && why.contains("ZZZ/secret.txt"),
        "{why}"
    );
    assert_eq!(head_of(&w, "HEAD"), base, "nothing was committed");
    assert_eq!(
        attempts(&env, "K-2").len(),
        2,
        "plan/1 and implement/1: no agent ran"
    );
}

// ── EC-18: an unrepairable Git state escalates, whatever ends the turn ────────────────

/// Put the worktree into a state a soft reset cannot repair, before the run.
fn break_git(w: &Path, how: &str) {
    let gd = std::path::PathBuf::from(git(w, &["rev-parse", "--absolute-git-dir"]).trim());
    let head = head_of(w, "HEAD");
    match how {
        "merge" => std::fs::write(gd.join("MERGE_HEAD"), format!("{head}\n")).unwrap(),
        "cherry-pick" => std::fs::write(gd.join("CHERRY_PICK_HEAD"), format!("{head}\n")).unwrap(),
        "rebase" => std::fs::create_dir_all(gd.join("rebase-merge")).unwrap(),
        "branch" => {
            git(w, &["checkout", "-q", "-b", "evil"]);
        }
        "detach" => {
            git(w, &["checkout", "-q", "--detach"]);
        }
        _ => unreachable!(),
    }
}

#[test]
fn ec18_an_unrepairable_git_state_escalates_whatever_ends_the_turn() {
    // (the turn's end, what the "agent" left, the implementer, extra config, the turn, the
    // escalation reason)
    let cases = [
        (
            "quota switch",
            "merge",
            "codex-openai",
            "",
            vec![
                write("src/Calculator.psm1", DIVIDE),
                fail("QuotaExceeded", "usage limit"),
            ],
            "an unfinished Git operation (MERGE_HEAD) is in the task worktree (R25)",
        ),
        (
            "rate-limit retry",
            "branch",
            "codex-openai",
            "",
            vec![
                write("src/Calculator.psm1", DIVIDE),
                fail("RateLimited", "429 Too Many Requests"),
            ],
            "HEAD is on refs/heads/evil, not on the task branch toryo/X-2 (R12)",
        ),
        (
            "live budget interrupt",
            "detach",
            "claude-max",
            "pipeline:\n  budget_tokens: 1000\n",
            vec![
                write("src/Calculator.psm1", DIVIDE),
                tokens(5000, 10),
                hang(),
            ],
            "HEAD is detached at",
        ),
        (
            "a completed turn",
            "rebase",
            "codex-openai",
            "",
            vec![write("src/Calculator.psm1", DIVIDE), implementation()],
            "an unfinished Git operation (rebase-merge) is in the task worktree (R25)",
        ),
    ];
    for (n, (end, how, implementer, extra, steps, why)) in cases.into_iter().enumerate() {
        let id = format!("X-{}", n + 1);
        let env = setup_with(implementer, extra);
        create(&env, &id);
        let w = wt(&env, &id);
        let base = head_of(&w, "HEAD");
        break_git(&w, how);
        let s = script(
            &env,
            "x.json",
            json!({
                "planner": [turn(vec![tokens(100, 0), plan()])],
                "implementer": [turn(steps), turn(vec![implementation()])],
                "reviewer": [turn(vec![approved()])],
            }),
        );
        let (code, err) = toryo(&env, &["run", &id, "--yes", "--fake-agents", &s], &[]);
        assert_eq!(code, 1, "{end}: {err}");
        let t = task(&env, &id);
        assert_eq!(t.state, St::Escalated, "{end}: {err}");
        let reason = t.machine.reason().unwrap();
        assert!(
            reason.starts_with("policy_violation: after the Implementer turn ")
                && reason.contains(why)
                && reason.contains("nothing was reset or committed"),
            "{end}: {reason}"
        );
        // the stage failed on the violation: no retry, no switch, no pause, no checkpoint
        let a = attempt(&env, &format!("{id}/implement/1"));
        assert_eq!(
            (
                a.status,
                a.error_class.as_deref(),
                a.error_detail.as_deref()
            ),
            (
                AttemptStatus::Failed,
                Some("policy_violation"),
                Some(reason)
            ),
            "{end}"
        );
        assert_eq!(implementer_prompts(&env, &id).len(), 1, "{end}");
        assert!(events(&env, &id, "implementer_switch").is_empty(), "{end}");
        let pv = events(&env, &id, "policy_violation");
        assert_eq!(pv.len(), 1, "{end}: {pv:?}");
        assert_eq!(pv[0]["action"], json!("escalate"), "{end}");
        // nothing was reset or committed, on any branch
        assert_eq!(head_of(&w, &format!("toryo/{id}")), base, "{end}");
        assert_eq!(head_of(&w, "HEAD"), base, "{end}");
        if how == "branch" {
            assert_eq!(head_of(&w, "evil"), base);
        }
        assert!(
            std::fs::read_to_string(w.join("src/Calculator.psm1"))
                .unwrap()
                .contains("Divide-Numbers"),
            "{end}: the work stays in the worktree"
        );
    }
}

#[test]
fn ec18_an_unrepairable_git_state_escalates_after_a_stopped_turn() {
    let env = setup("codex-openai");
    create(&env, "X-5");
    let w = wt(&env, "X-5");
    let base = head_of(&w, "HEAD");
    break_git(&w, "cherry-pick");
    let s = script(
        &env,
        "x5.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), hang()])],
        }),
    );
    let mut child = spawn(
        &env,
        &["run", "X-5", "--yes", "--fake-agents", &s],
        "x5.log",
    );
    wait_for_running(&env, "X-5", "implement");
    let (code, err) = toryo(&env, &["stop", "X-5", "--wait-secs", "30"], &[]);
    assert_eq!(code, 0, "{err}");
    child.wait().unwrap();
    let log = std::fs::read_to_string(env.root.join("x5.log")).unwrap();
    let t = task(&env, "X-5");
    assert_eq!(t.state, St::Escalated, "{log}");
    assert!(
        t.machine
            .reason()
            .unwrap()
            .contains("an unfinished Git operation (CHERRY_PICK_HEAD)"),
        "{log}"
    );
    let a = attempt(&env, "X-5/implement/1");
    assert_eq!(
        (a.status, a.error_class.as_deref()),
        (AttemptStatus::Failed, Some("policy_violation"))
    );
    assert_eq!(head_of(&w, "HEAD"), base);
}

// ── EC-15 (was probe_engine_flows::probe_failed_implementer_with_changes_is_checkpointed_as_success)

#[test]
fn ec15_a_failed_turn_with_changes_is_kept_as_a_recovery_checkpoint_and_run_again() {
    let env = setup("codex-openai");
    create(&env, "P-3");
    let s = script(
        &env,
        "p3.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), fail("Unknown", "the agent died half way")]),
                turn(vec![implementation()]),
            ],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "P-3", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "P-3");
    assert_eq!(t.state, St::Completed, "{err}");
    // the history tells the truth (was: Succeeded / Unknown, checkpointed like a normal turn)
    let a1 = attempt(&env, "P-3/implement/1");
    assert_eq!(
        (
            a1.status,
            a1.error_class.as_deref(),
            a1.error_detail.as_deref()
        ),
        (
            AttemptStatus::Failed,
            Some("Unknown"),
            Some("the agent died half way")
        )
    );
    assert_eq!((a1.git_head_after, a1.checkpoint_kind), (None, None));
    // … and the work is kept in a recovery checkpoint (R31), never verified on its own
    let partial = a1.partial_commit.expect("a recovery checkpoint");
    let w = wt(&env, "P-3");
    assert!(
        git(&w, &["show", &format!("{partial}:src/Calculator.psm1")]).contains("Divide-Numbers")
    );
    assert_eq!(
        trailers(&env, "P-3").trim(),
        "toryo(P-3): implement|implement|2||true\ntoryo(P-3): implement (recovery checkpoint of failed attempt 1)|implement|1|true|"
    );
    // the stage ran again from it, told why; its no-change checkpoint is what was verified
    let a2 = attempt(&env, "P-3/implement/2");
    assert_eq!(a2.status, AttemptStatus::Succeeded);
    assert_eq!(a2.git_head_before.as_deref(), Some(partial.as_str()));
    assert_eq!(a2.checkpoint_kind.as_deref(), Some("no_change"));
    assert_eq!(t.machine.checkpoint(), a2.git_head_after.as_deref());
    let p = implementer_prompts(&env, "P-3");
    assert_eq!(p.len(), 2);
    assert!(!p[0].contains("## Recovery"), "{}", p[0]);
    assert!(
        p[1].contains("The previous attempt (P-3/implement/1 by `codex-openai`) failed before it finished (Unknown: the agent died half way)")
            && p[1].contains(&format!("recovery checkpoint {}", &partial[..12])),
        "{}",
        p[1]
    );
    assert!(t.handoff.is_none(), "the note is used once");
    assert!(err.contains("the stage runs again from it"), "{err}");
    assert!(clean(&w));
}

#[test]
fn ec15_a_second_failed_turn_with_changes_escalates_and_keeps_the_work() {
    let env = setup("codex-openai");
    create(&env, "P-7");
    let s = script(
        &env,
        "p7.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write("src/Partial.ps1", "# first\n"), fail("Unknown", "first")]),
                turn(vec![write("src/Calculator.psm1", DIVIDE), fail("Timeout", "second")]),
            ],
        }),
    );
    let (code, err) = toryo(&env, &["run", "P-7", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "P-7");
    assert_eq!(t.state, St::Escalated, "{err}");
    let (a1, a2) = (
        attempt(&env, "P-7/implement/1"),
        attempt(&env, "P-7/implement/2"),
    );
    let (p1, p2) = (a1.partial_commit.unwrap(), a2.partial_commit.unwrap());
    assert_eq!(
        (
            a2.status,
            a2.error_class.as_deref(),
            a2.git_head_before.as_deref()
        ),
        (AttemptStatus::Failed, Some("Timeout"), Some(p1.as_str()))
    );
    assert_eq!(
        t.machine.reason().unwrap(),
        format!(
            "implementer failed: Timeout — second; its partial work is kept in the recovery checkpoint {} (Toryo-Partial)",
            &p2[..12]
        )
    );
    assert_eq!(implementer_prompts(&env, "P-7").len(), 2, "§11: once");
    let w = wt(&env, "P-7");
    assert_eq!(head_of(&w, "HEAD"), p2);
    assert!(clean(&w));
    // the human resumes: the next fix cycle starts from the kept work, told about it
    let s2 = script(
        &env,
        "p7b.json",
        json!({
            "implementer": [turn(vec![implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(
        &env,
        &[
            "resume",
            "P-7",
            "--note",
            "finish it",
            "--yes",
            "--fake-agents",
            &s2,
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "P-7").state, St::Completed, "{err}");
    let fix = attempt(&env, "P-7/fix-1/1");
    assert_eq!(
        (
            fix.status,
            fix.git_head_before.as_deref(),
            fix.checkpoint_kind.as_deref()
        ),
        (
            AttemptStatus::Succeeded,
            Some(p2.as_str()),
            Some("no_change")
        )
    );
    let p = implementer_prompts(&env, "P-7");
    assert!(
        p[0].contains("finish it")
            && p[0].contains("failed before it finished (Timeout: second)")
            && p[0].contains(&p2[..12]),
        "{}",
        p[0]
    );
    let files = git(&w, &["show", "HEAD:src/Partial.ps1"]);
    assert_eq!(files, "# first\n", "nothing was lost");
}

#[test]
fn ec15_a_denied_turn_with_changes_escalates_at_once_and_keeps_the_work() {
    let env = setup("codex-openai");
    create(&env, "P-8");
    let s = script(
        &env,
        "p8.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), json!({"kind":"deny","tool":"Edit","detail":".claude/settings.json"}), implementation()]),
                turn(vec![implementation()]),
            ],
        }),
    );
    let (code, err) = toryo(&env, &["run", "P-8", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "P-8");
    assert_eq!(t.state, St::Escalated, "{err}");
    let a = attempt(&env, "P-8/implement/1");
    assert_eq!(
        (a.status, a.error_class.as_deref()),
        (AttemptStatus::Failed, Some("PermissionDenied"))
    );
    let partial = a.partial_commit.unwrap();
    assert!(t
        .machine
        .reason()
        .unwrap()
        .starts_with("implementer failed: PermissionDenied — 1 permission denial(s); its partial work is kept in the recovery checkpoint"));
    // §11: no blind retry after a denial
    assert_eq!(implementer_prompts(&env, "P-8").len(), 1);
    let w = wt(&env, "P-8");
    assert_eq!(head_of(&w, "HEAD"), partial);
    assert!(git(&w, &["show", "HEAD:src/Calculator.psm1"]).contains("Divide-Numbers"));
}

// ── F9: the hand-over after a switch names the committed partial work ─────────────────

/// A failed turn's work is kept in a recovery checkpoint and the stage runs again (EC-15);
/// the retry runs out of quota: the fallback implementer is told that the stage's work is
/// committed and the worktree starts from it (was: "stopped before producing any change …
/// Start the task from the plan", on top of the partial commit).
#[test]
fn f9_the_hand_over_after_a_kept_failed_turn_names_the_committed_work() {
    let env = setup("codex-openai");
    create(&env, "H-9");
    let s = script(
        &env,
        "h9.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), fail("Unknown", "the agent died half way")]),
                turn(vec![fail("QuotaExceeded", "You've hit your usage limit")]),
            ],
            "implementer@claude-max": [turn(vec![implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "H-9", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "H-9").state, St::Completed, "{err}");
    let partial = attempt(&env, "H-9/implement/1")
        .partial_commit
        .expect("a recovery checkpoint");
    let p = implementer_prompts(&env, "H-9");
    assert_eq!(p.len(), 3, "{p:?}");
    assert!(
        p[1].contains("## Recovery") && p[1].contains(&partial[..12]),
        "{}",
        p[1]
    );
    let handover = &p[2];
    assert!(
        !handover.contains("stopped before producing any change")
            && !handover.contains("Start the task from the plan"),
        "{handover}"
    );
    assert!(
        handover.contains(&format!(
            "The partial work of this stage so far is committed as the recovery checkpoint {} (trailer `Toryo-Partial: true`) — the worktree starts from it:\n- `M src/Calculator.psm1`\n",
            &partial[..12]
        )),
        "{handover}"
    );
    // the fallback started from the partial commit and changed nothing more (R40)
    let a3 = attempt(&env, "H-9/implement/3");
    assert_eq!(
        (
            a3.profile.as_deref(),
            a3.git_head_before.as_deref(),
            a3.checkpoint_kind.as_deref()
        ),
        (
            Some("claude-max"),
            Some(partial.as_str()),
            Some("no_change")
        )
    );
    let sw = events(&env, "H-9", "implementer_switch");
    assert_eq!(sw.len(), 1, "{sw:?}");
}

// ── EC-16 (was probe_engine_flows::probe_no_fallback_after_a_sticky_switch_pauses) ─────

#[test]
fn ec16_no_fallback_after_a_sticky_switch_continues_with_the_current_implementer() {
    let env = setup("codex-openai");
    create(&env, "P-4");
    let s = script(
        &env,
        "p4.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![fail("QuotaExceeded", "You've hit your usage limit")])],
            "implementer@claude-max": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]), turn(vec![implementation()])],
            "reviewer": [turn(vec![approved()]), turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "P-4", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = toryo(
        &env,
        &[
            "approve",
            "P-4",
            "--gate",
            "plan",
            "--continue",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "P-4").state, St::WaitingFinalApproval, "{err}");
    // CODE-03: the quota switch keeps the turn's detail too
    let a1 = attempt(&env, "P-4/implement/1");
    assert_eq!(
        (a1.error_class.as_deref(), a1.error_detail.as_deref()),
        (Some("QuotaExceeded"), Some("You've hit your usage limit"))
    );
    let (code, err) = toryo(
        &env,
        &[
            "reject",
            "P-4",
            "--gate",
            "final",
            "--reason",
            "rename the parameter",
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let (code, err) = toryo(
        &env,
        &["run", "P-4", "--no-fallback", "--fake-agents", &s],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "P-4");
    // was: Paused "implementer_quota: no implementer left (codex-openai → claude-max, --no-fallback)"
    assert_eq!(t.state, St::WaitingFinalApproval, "{err}");
    assert!(!err.contains("no implementer left"), "{err}");
    let fix = attempt(&env, "P-4/fix-1/1");
    assert_eq!(
        (fix.status, fix.profile.as_deref()),
        (AttemptStatus::Succeeded, Some("claude-max"))
    );
    assert!(
        calls(&env, "P-4").iter().all(|c| c["key"] != "implementer"),
        "Codex never ran again"
    );
}

#[test]
fn ec16_no_fallback_forbids_a_new_switch_after_a_sticky_one() {
    let env = setup("codex-openai");
    // R60: the new profile goes to the machine config, the chain to the repository file
    let machine = std::fs::read_to_string(&env.machine).unwrap().replacen(
        "  codex-openai: {",
        "  codex-backup: { kind: codex, billing: subscription, exe: codex-not-installed }\n  codex-openai: {",
        1,
    );
    std::fs::write(&env.machine, machine).unwrap();
    let cfg = std::fs::read_to_string(&env.config).unwrap().replacen(
        "implementer: { profile: codex-openai }",
        "implementer: { profile: codex-openai, fallback: [claude-max, codex-backup] }",
        1,
    );
    std::fs::write(&env.config, cfg).unwrap();
    create(&env, "P-9");
    assert_eq!(
        task(&env, "P-9").implementers,
        vec!["codex-openai", "claude-max", "codex-backup"]
    );
    let s = script(
        &env,
        "p9.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer@codex-openai": [turn(vec![fail("QuotaExceeded", "codex quota")])],
            "implementer@claude-max": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    // every `toryo` process starts the script over: the fix cycle's run gets its own
    let fix = script(
        &env,
        "p9b.json",
        json!({
            "implementer@claude-max": [turn(vec![write("src/Partial.ps1", "# p\n"), fail("QuotaExceeded", "claude quota")])],
            "implementer@codex-backup": [turn(vec![implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "P-9", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = toryo(
        &env,
        &[
            "approve",
            "P-9",
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
        &["reject", "P-9", "--gate", "final", "--reason", "more"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    // claude-max (sticky) runs the fix cycle, runs out of quota: no switch to codex-backup
    let (code, err) = toryo(
        &env,
        &["run", "P-9", "--no-fallback", "--fake-agents", &fix],
        &[],
    );
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "P-9");
    assert_eq!(t.state, St::Paused, "{err}");
    assert_eq!(
        t.machine.reason().unwrap(),
        "implementer_quota: no implementer left (quota exhausted: claude quota); the worktree keeps any partial changes — `toryo resume` when the quota is back"
    );
    let fix = attempt(&env, "P-9/fix-1/1");
    assert_eq!(
        (fix.profile.as_deref(), fix.error_class.as_deref()),
        (Some("claude-max"), Some("QuotaExceeded"))
    );
    let switches = events(&env, "P-9", "implementer_switch");
    assert_eq!(switches.last().unwrap()["to"], Value::Null);
    assert!(calls(&env, "P-9")
        .iter()
        .all(|c| c["key"] != "implementer@codex-backup"));
    assert!(
        wt(&env, "P-9").join("src/Partial.ps1").exists(),
        "partial work kept"
    );
}

// ── EC-17 (was probe_engine_flows::probe_stop_during_rate_limit_backoff_waits_for_the_sleep)

#[test]
fn ec17_stop_during_a_rate_limit_backoff_is_answered_at_once() {
    let env = setup("codex-openai");
    create(&env, "P-5");
    let s = script(
        &env,
        "p5.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![fail("RateLimited", "429 Too Many Requests")]), turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let log = std::fs::File::create(env.root.join("p5.log")).unwrap();
    let mut child = command(
        &env,
        &["run", "P-5", "--yes", "--fake-agents", &s],
        &[("TORYO_RETRY_BASE_SECS", "12")],
    )
    .stdout(std::process::Stdio::null())
    .stderr(log)
    .spawn()
    .unwrap();
    wait_until("the first implement attempt to fail", 60, || {
        attempts(&env, "P-5")
            .iter()
            .any(|a| a.stage == "implement" && a.status == AttemptStatus::Failed)
    });
    let t0 = Instant::now();
    let (code, err) = toryo(&env, &["stop", "P-5", "--wait-secs", "30"], &[]);
    let took = t0.elapsed();
    let status = child.wait().unwrap();
    // R38: ~1 s poll + 5 s drain (was: the whole 12 s backoff)
    assert_eq!(code, 0, "{err}");
    assert!(took < Duration::from_secs(6), "stop took {took:?}");
    assert!(err.contains("P-5: stopped_at_boundary"), "{err}");
    assert!(status.success());
    let log = std::fs::read_to_string(env.root.join("p5.log")).unwrap();
    assert!(
        log.contains("retry 1/2 in 12s")
            && log.contains("■ stop requested during the backoff: the retry does not start"),
        "{log}"
    );
    // the retry never started; the task stays in its stage and resumes normally
    let t = task(&env, "P-5");
    assert_eq!(t.state, St::Implementing);
    let imp: Vec<AttemptRow> = attempts(&env, "P-5")
        .into_iter()
        .filter(|a| a.stage == "implement")
        .collect();
    assert_eq!(imp.len(), 1);
    assert_eq!(imp[0].error_class.as_deref(), Some("RateLimited"));
    let s2 = script(
        &env,
        "p5b.json",
        json!({
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["resume", "P-5", "--yes", "--fake-agents", &s2], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "P-5").state, St::Completed, "{err}");
}

// ── CODE-03: a switch after rate-limit retries is not a quota ───────────────────────────

#[test]
fn code03_a_switch_after_rate_limit_retries_records_the_real_class_and_detail() {
    let env = setup("codex-openai");
    create(&env, "C-3");
    let s = script(
        &env,
        "c3.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![fail("RateLimited", "429 Too Many Requests")]),
                turn(vec![fail("RateLimited", "429 Too Many Requests")]),
                turn(vec![fail("Overloaded", "529 Overloaded")]),
            ],
            "implementer@claude-max": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "C-3", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "C-3").state, St::Completed, "{err}");
    let got: Vec<(String, AttemptStatus, Option<String>, Option<String>)> = attempts(&env, "C-3")
        .into_iter()
        .filter(|a| a.stage == "implement")
        .map(|a| (a.profile.unwrap(), a.status, a.error_class, a.error_detail))
        .collect();
    let rl = || {
        (
            Some("RateLimited".to_string()),
            Some("429 Too Many Requests".to_string()),
        )
    };
    let codex = |c: (Option<String>, Option<String>)| {
        ("codex-openai".to_string(), AttemptStatus::Failed, c.0, c.1)
    };
    assert_eq!(
        got,
        vec![
            codex(rl()),
            codex(rl()),
            // was: QuotaExceeded, no detail
            codex((Some("Overloaded".into()), Some("529 Overloaded".into()))),
            (
                "claude-max".to_string(),
                AttemptStatus::Succeeded,
                None,
                None
            ),
        ]
    );
    let sw = events(&env, "C-3", "implementer_switch");
    assert_eq!(
        (&sw[0]["from"], &sw[0]["to"], &sw[0]["reason"]),
        (
            &json!("codex-openai"),
            &json!("claude-max"),
            &json!("Overloaded after 2 retries")
        )
    );
}

// ── EC-25 (implementation.json): the report is validated, a mismatch is recorded ──────────

#[test]
fn ec25_an_invalid_implementation_report_is_recorded_not_fatal() {
    let env = setup("codex-openai");
    create(&env, "V-1");
    // missing `deviations`/`validation_performed`/…, and an extra key the schema forbids
    let report = json!({"kind":"complete","structured":{"summary":"s","ok":true}});
    let s = script(
        &env,
        "v1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), report])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "V-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "V-1").state, St::Completed, "{err}");
    assert!(
        err.contains("implementation.json does not match implementation.schema.json"),
        "{err}"
    );
    let v = events(&env, "V-1", "schema_violation");
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0]["schema"], json!("implementation.schema.json"));
    let errors = v[0]["errors"].to_string();
    assert!(
        errors.contains("deviations") && errors.contains("ok"),
        "{errors}"
    );
}
