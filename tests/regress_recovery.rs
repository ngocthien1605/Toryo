//! Regression tests for crash recovery of the Implementer path (plan §10; follow-ups of the
//! 2026-09-29 review). The owner dies at a `TORYO_TEST_CRASH_AT` point; the next `toryo run` /
//! `resume` recovers the task.
//!
//! - F4 (EC-18, R12, R25) recovery runs the HEAD guard of the Implementer turn before anything
//!   else: another branch, a detached HEAD or history rewritten below the base escalates with
//!   nothing reset, snapshotted or committed — the teammate's branch the agent committed on
//!   keeps its commits; an agent commit on the task branch is still undone and its work kept
//!   in a recovery checkpoint, as before; an unfinished Git operation keeps recovery's own path
//!   (no reset during it, a snapshot, `recovery_policy_violation`, `--discard-partial` clears it)
//! - F8 (EC-15) a crash while a failed turn's work is being kept (before or after its recovery
//!   checkpoint is committed) ends the attempt as its owner decided: `failed` with the turn's
//!   class, counted as a failed turn, and a denial or a second failure still escalates
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};
use toryo::engine::store::{self, AttemptRow, AttemptStatus};
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

fn rev(dir: &Path, rev: &str) -> String {
    git(dir, &["rev-parse", rev]).trim().to_string()
}

fn reason(env: &Env, id: &str) -> String {
    task(env, id).machine.reason().unwrap_or("").to_string()
}

/// The task worktree's own Git dir (`<repo>/.git/worktrees/<id>`): its `HEAD` and operation
/// markers.
fn git_dir(w: &Path) -> PathBuf {
    PathBuf::from(git(w, &["rev-parse", "--absolute-git-dir"]).trim())
}

/// A commit of `from`'s tree with `files` replaced, on `parent` (none: a root commit), written
/// through a private index: the worktree and its index stay as they are — what an agent's
/// `git commit` leaves in the object store.
fn commit_of(
    env: &Env,
    w: &Path,
    from: &str,
    parent: Option<&str>,
    files: &[(&str, &str)],
    msg: &str,
) -> String {
    let index = env.root.join("agent.index");
    let with_index = |args: &[&str]| -> String {
        let o = Command::new("git")
            .arg("-C")
            .arg(w)
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
    with_index(&["read-tree", from]);
    for (path, content) in files {
        let f = env.root.join("agent.blob");
        std::fs::write(&f, content).unwrap();
        let blob = git(w, &["hash-object", "-w", f.to_str().unwrap()])
            .trim()
            .to_string();
        with_index(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{blob},{path}"),
        ]);
    }
    let tree = with_index(&["write-tree"]);
    let mut args = vec![
        "-c",
        "user.name=agent",
        "-c",
        "user.email=a@a",
        "commit-tree",
        &tree,
        "-m",
        msg,
    ];
    if let Some(p) = parent {
        args.extend(["-p", p]);
    }
    git(w, &args).trim().to_string()
}

/// A fake step that overwrites `path` (absolute: Git's own files) with `content`.
fn put(path: &Path, content: &str) -> Value {
    json!({"kind":"write_file","path":path,"content":content})
}

/// The Calculator module at `rev` with the division added (the agent's work).
fn with_divide(w: &Path, rev: &str) -> String {
    git(w, &["show", &format!("{rev}:src/Calculator.psm1")]) + DIVIDE
}

/// Every commit on any branch that carries `Toryo-Partial: true`.
fn partial_commits(w: &Path) -> Vec<String> {
    git(
        w,
        &[
            "log",
            "--all",
            "--format=%H %(trailers:key=Toryo-Partial,valueonly,separator=)",
        ],
    )
    .lines()
    .filter(|l| l.ends_with(" true"))
    .map(|l| l[..40].to_string())
    .collect()
}

/// Run `id` into an Implementer turn made of `steps` and let the owner die right after it
/// (`implement:after_turn`): the in-process HEAD guard never ran.
fn crash_after_the_turn(env: &Env, id: &str, steps: Vec<Value>) {
    let s = script(
        env,
        "crash.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(steps)],
        }),
    );
    let (code, err) = toryo(
        env,
        &["run", id, "--yes", "--fake-agents", &s],
        &[("TORYO_TEST_CRASH_AT", "implement:after_turn")],
    );
    assert_eq!(code, 86, "{err}");
    assert_eq!(
        attempt(env, &format!("{id}/implement/1")).status,
        AttemptStatus::Running
    );
}

/// The next `toryo run` after the crash (it recovers first), with agents that would complete
/// the task if they ran.
fn run_after_the_crash(env: &Env, id: &str) -> (i32, String) {
    let s = script(
        env,
        "after.json",
        json!({
            "implementer": [turn(vec![implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    toryo(env, &["run", id, "--yes", "--fake-agents", &s], &[])
}

/// Recovery escalated `id` with the HEAD guard's `reason`: the attempt is interrupted with
/// `policy_violation`, no agent ran, nothing was snapshotted or committed as partial work.
fn assert_escalated_untouched(env: &Env, id: &str, code: i32, err: &str, reason_is: &str) {
    assert_eq!(code, 1, "{err}");
    let t = task(env, id);
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(reason(env, id), reason_is, "{err}");
    assert!(
        err.contains(&format!(
            "↺ recovery: {id}/implement/1: {reason_is} — escalated"
        )),
        "{err}"
    );
    let a = attempt(env, &format!("{id}/implement/1"));
    assert_eq!(
        (
            a.status,
            a.error_class.as_deref(),
            a.error_detail.as_deref(),
            a.partial_commit.as_deref(),
            a.partial_snapshot_id.as_deref()
        ),
        (
            AttemptStatus::Interrupted,
            Some("policy_violation"),
            Some(reason_is),
            None,
            None
        )
    );
    assert_eq!(
        attempts(env, id).len(),
        2,
        "plan/1, implement/1: no agent ran"
    );
    // R41: the killed turn still counts (unknown usage with a reservation)
    let aid = format!("{id}/implement/1");
    assert!(db(env).call(move |tx| store::has_usage(tx, &aid)).unwrap());
    let pv = events(env, id, "policy_violation");
    assert_eq!(pv.len(), 1, "{pv:?}");
    assert_eq!(
        (&pv[0]["action"], &pv[0]["during"], &pv[0]["reason"]),
        (&json!("escalate"), &json!("recovery"), &json!(reason_is))
    );
    assert!(
        partial_commits(&wt(env, id)).is_empty(),
        "no recovery checkpoint anywhere"
    );
}

// ── F4: the HEAD guard runs first in recovery ─────────────────────────────────────────────

/// The review's scenario: the agent checks out a teammate's branch `other` and commits on it;
/// its owner dies after the turn. Was: recovery soft-reset `other` to the task base (the
/// teammate's commit gone from it) and committed a recovery checkpoint onto `other`; only the
/// next turn's guard escalated, saying "nothing was reset or committed".
#[test]
fn f4_recovery_never_resets_or_commits_on_the_branch_the_agent_switched_to() {
    let env = setup("codex-openai");
    create(&env, "B-1");
    let w = wt(&env, "B-1");
    let base = rev(&w, "HEAD");
    // a teammate's branch, one commit ahead of the base
    let teammate = commit_of(
        &env,
        &w,
        &base,
        Some(&base),
        &[("team/notes.md", "teammate work\n")],
        "teammate",
    );
    git(&w, &["update-ref", "refs/heads/other", &teammate]);
    let other_ref = env.repo.join(".git/refs/heads/other");
    assert!(other_ref.is_file(), "a loose ref");
    // `git checkout other && git commit -a` by the agent, without timing: its commit is
    // written up front, the turn points HEAD at `other` and moves `other` to it
    let agent = commit_of(
        &env,
        &w,
        &teammate,
        Some(&teammate),
        &[("src/Calculator.psm1", &with_divide(&w, &base))],
        "agent commit",
    );
    crash_after_the_turn(
        &env,
        "B-1",
        vec![
            write("src/Calculator.psm1", DIVIDE),
            put(&git_dir(&w).join("HEAD"), "ref: refs/heads/other\n"),
            put(&other_ref, &format!("{agent}\n")),
            implementation(),
        ],
    );
    let (code, err) = run_after_the_crash(&env, "B-1");
    assert_escalated_untouched(
        &env,
        "B-1",
        code,
        &err,
        "policy_violation: after the Implementer turn HEAD is on refs/heads/other, not on the task branch toryo/B-1 (R12); nothing was reset or committed — check out toryo/B-1 again first",
    );
    // … which is true: the teammate's branch holds the teammate's and the agent's commits,
    // the task branch its base, the worktree the agent's edit
    assert_eq!(rev(&w, "refs/heads/other"), agent);
    assert_eq!(rev(&w, "refs/heads/other^"), teammate);
    assert_eq!(rev(&w, "refs/heads/toryo/B-1"), base);
    assert_eq!(
        git(&w, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/other"
    );
    assert!(std::fs::read_to_string(w.join("src/Calculator.psm1"))
        .unwrap()
        .contains("Divide-Numbers"));
}

/// The other states a soft reset cannot repair escalate the same way, before anything is
/// reset, snapshotted or committed.
#[test]
fn f4_recovery_escalates_an_unrepairable_git_state_with_nothing_reset() {
    for how in ["detached", "rewritten"] {
        let env = setup("codex-openai");
        let id = format!("G-{how}");
        create(&env, &id);
        let w = wt(&env, &id);
        let base = rev(&w, "HEAD");
        let branch = env.repo.join(format!(".git/refs/heads/toryo/{id}"));
        assert!(branch.is_file(), "a loose ref");
        let gd = git_dir(&w);
        let work = with_divide(&w, &base);
        // what the agent leaves behind, and what recovery must leave as it is
        let (steps, why, branch_at) = match how {
            // detached at the base: HEAD did not move, but a recovery checkpoint would not
            // land on the task branch (was: committed on the detached HEAD)
            "detached" => (
                vec![put(&gd.join("HEAD"), &format!("{base}\n"))],
                format!("HEAD is detached at {}, not on the task branch toryo/{id} (R12); nothing was reset or committed — check out toryo/{id} again first", &base[..12]),
                base.clone(),
            ),
            // the task branch at a commit that does not descend from the base (was: a
            // recovery checkpoint on top of it)
            _ => {
                let orphan = commit_of(&env, &w, &base, None, &[("src/Calculator.psm1", &work)], "rewritten");
                (
                    vec![put(&branch, &format!("{orphan}\n"))],
                    format!(
                        "HEAD {} no longer descends from the task base {} (history rewritten, R25); nothing was reset or committed",
                        &orphan[..12],
                        &base[..12]
                    ),
                    orphan,
                )
            }
        };
        let mut turn_steps = vec![write("src/Calculator.psm1", DIVIDE)];
        turn_steps.extend(steps);
        turn_steps.push(implementation());
        crash_after_the_turn(&env, &id, turn_steps);
        let (code, err) = run_after_the_crash(&env, &id);
        assert_escalated_untouched(
            &env,
            &id,
            code,
            &err,
            &format!("policy_violation: after the Implementer turn {why}"),
        );
        assert_eq!(
            rev(&w, &format!("refs/heads/toryo/{id}")),
            branch_at,
            "{how}: not reset"
        );
        assert!(
            std::fs::read_to_string(w.join("src/Calculator.psm1"))
                .unwrap()
                .contains("Divide-Numbers"),
            "{how}: the work stays in the worktree"
        );
    }
}

/// An unfinished Git operation (a merge in progress) and an agent commit on the task branch:
/// no soft reset during the merge (was: reset, then snapshot), a snapshot, and the escalation
/// `resume --discard-partial` can clear (not the HEAD guard's `policy_violation`, which only
/// `resume --note` — a new fix cycle — could leave).
#[test]
fn f4_an_unfinished_operation_keeps_the_recovery_snapshot_path() {
    let env = setup("codex-openai");
    let id = "G-merge";
    create(&env, id);
    let w = wt(&env, id);
    let base = rev(&w, "HEAD");
    let branch = env.repo.join(format!(".git/refs/heads/toryo/{id}"));
    assert!(branch.is_file(), "a loose ref");
    let gd = git_dir(&w);
    let work = with_divide(&w, &base);
    let agent = commit_of(
        &env,
        &w,
        &base,
        Some(&base),
        &[("src/Calculator.psm1", &work)],
        "agent",
    );
    crash_after_the_turn(
        &env,
        id,
        vec![
            write("src/Calculator.psm1", DIVIDE),
            put(&branch, &format!("{agent}\n")),
            put(&gd.join("MERGE_HEAD"), &format!("{base}\n")),
            implementation(),
        ],
    );
    let (code, err) = run_after_the_crash(&env, id);
    assert_eq!(code, 1, "{err}");
    let t = task(&env, id);
    assert_eq!(t.state, St::Escalated, "{err}");
    let why = reason(&env, id);
    assert!(
        why.starts_with("recovery_policy_violation: the interrupted attempt G-merge/implement/1 left an unfinished Git operation (MERGE_HEAD)"),
        "{why}"
    );
    let a = attempt(&env, &format!("{id}/implement/1"));
    assert_eq!(a.status, AttemptStatus::Interrupted);
    assert!(a.partial_snapshot_id.is_some(), "snapshot first (R39)");
    assert_eq!(
        rev(&w, &format!("refs/heads/toryo/{id}")),
        agent,
        "no reset during the merge"
    );
    assert!(partial_commits(&w).is_empty(), "nothing committed");
    // the way out that recovery_policy_violation offers: discard (hard reset, merge cleared)
    let s = script(
        &env,
        "discard.json",
        json!({
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(
        &env,
        &[
            "resume",
            id,
            "--discard-partial",
            "--yes",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, id).state, St::Completed, "{err}");
    assert!(!gd.join("MERGE_HEAD").exists(), "the merge is gone");
    let history = git(&w, &["log", "--format=%H", "main..HEAD"]);
    assert!(!history.contains(&agent), "{history}");
}

/// A moved HEAD on the task branch keeps the recovery it had: the agent commit is undone, its
/// work becomes a recovery checkpoint on the base, and the stage runs again from it.
#[test]
fn f4_an_agent_commit_on_the_task_branch_is_still_undone_and_its_work_kept() {
    let env = setup("codex-openai");
    create(&env, "B-2");
    let w = wt(&env, "B-2");
    let base = rev(&w, "HEAD");
    let branch = env.repo.join(".git/refs/heads/toryo/B-2");
    let agent = commit_of(
        &env,
        &w,
        &base,
        Some(&base),
        &[("src/Calculator.psm1", &with_divide(&w, &base))],
        "agent commit",
    );
    crash_after_the_turn(
        &env,
        "B-2",
        vec![
            write("src/Calculator.psm1", DIVIDE),
            put(&branch, &format!("{agent}\n")),
            implementation(),
        ],
    );
    let (code, err) = run_after_the_crash(&env, "B-2");
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "B-2").state, St::Completed, "{err}");
    assert!(
        err.contains(&format!(
            "↺ recovery: B-2/implement/1: the agent had moved HEAD — soft reset to {}",
            &base[..12]
        )),
        "{err}"
    );
    let a1 = attempt(&env, "B-2/implement/1");
    assert_eq!(a1.status, AttemptStatus::Interrupted);
    let partial = a1.partial_commit.expect("a recovery checkpoint");
    assert_eq!(
        rev(&w, &format!("{partial}^")),
        base,
        "on the base, not on the agent commit"
    );
    assert!(
        git(&w, &["show", &format!("{partial}:src/Calculator.psm1")]).contains("Divide-Numbers")
    );
    let log = git(&w, &["log", "--format=%H", "main..HEAD"]);
    assert!(!log.contains(&agent), "{log}");
    let a2 = attempt(&env, "B-2/implement/2");
    assert_eq!(
        (a2.git_head_before.as_deref(), a2.checkpoint_kind.as_deref()),
        (Some(partial.as_str()), Some("no_change"))
    );
    assert!(clean(&w));
}

// ── F8: the EC-15 decision survives a crash ───────────────────────────────────────────────

/// A denied turn left changes (EC-15: kept, no blind retry → Escalated); its owner dies after
/// the recovery checkpoint is committed, before the attempt ends. Was: recovery took it for an
/// interrupted attempt and ran the stage again.
#[test]
fn f8_a_denied_turn_that_crashed_after_its_partial_commit_still_escalates() {
    let env = setup("codex-openai");
    create(&env, "D-1");
    let s = script(
        &env,
        "d1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), json!({"kind":"deny","tool":"Edit","detail":".claude/settings.json"}), implementation()]),
            ],
        }),
    );
    let (code, err) = toryo(
        &env,
        &["run", "D-1", "--yes", "--fake-agents", &s],
        &[("TORYO_TEST_CRASH_AT", "implement:after_partial_commit")],
    );
    assert_eq!(code, 86, "{err}");
    let w = wt(&env, "D-1");
    let partial = rev(&w, "HEAD");
    assert_eq!(partial_commits(&w), std::slice::from_ref(&partial));
    assert_eq!(
        attempt(&env, "D-1/implement/1").status,
        AttemptStatus::Running
    );
    let (code, err) = run_after_the_crash(&env, "D-1");
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "D-1");
    assert_eq!(t.state, St::Escalated, "{err}");
    let why = format!(
        "implementer failed: Some(PermissionDenied) 1 permission denial(s); its partial work is kept in the recovery checkpoint {} (Toryo-Partial)",
        &partial[..12]
    );
    assert_eq!(
        reason(&env, "D-1"),
        why,
        "the reason it escalates with without a crash"
    );
    let a = attempt(&env, "D-1/implement/1");
    assert_eq!(
        (
            a.status,
            a.error_class.as_deref(),
            a.error_detail.as_deref(),
            a.partial_commit.as_deref()
        ),
        (
            AttemptStatus::Failed,
            Some("PermissionDenied"),
            Some("1 permission denial(s)"),
            Some(partial.as_str())
        )
    );
    assert_eq!(
        attempts(&env, "D-1").len(),
        2,
        "§11: no blind retry after a denial"
    );
    assert!(t
        .handoff
        .unwrap()
        .contains("failed before it finished (PermissionDenied: 1 permission denial(s))"));
    assert_eq!(rev(&w, "HEAD"), partial, "nothing else committed");
    assert!(clean(&w));
}

/// Two failed turns with changes, each crashing after its recovery checkpoint: the first is
/// counted as a failed turn — so the second escalates (§11: the stage runs again once). Was:
/// both taken for interrupted attempts, so the stage kept running again.
#[test]
fn f8_a_crashed_failed_turn_counts_so_the_second_failure_escalates() {
    let env = setup("codex-openai");
    create(&env, "D-2");
    let crash = [("TORYO_TEST_CRASH_AT", "implement:after_partial_commit")];
    let s1 = script(
        &env,
        "d2a.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Partial.ps1", "# first\n"), fail("Unknown", "first")])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "D-2", "--yes", "--fake-agents", &s1], &crash);
    assert_eq!(code, 86, "{err}");
    let w = wt(&env, "D-2");
    let p1 = rev(&w, "HEAD");
    // recovery ends implement/1 as decided (the stage runs again), then implement/2 fails
    // with changes and its owner dies after its recovery checkpoint too
    let s2 = script(
        &env,
        "d2b.json",
        json!({"implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), fail("Timeout", "second")])]}),
    );
    let (code, err) = toryo(
        &env,
        &["resume", "D-2", "--yes", "--fake-agents", &s2],
        &crash,
    );
    assert_eq!(code, 86, "{err}");
    assert!(
        err.contains(&format!("↺ recovery: D-2/implement/1: implementer failed: Some(Unknown) first; its partial work is kept in the recovery checkpoint {} (Toryo-Partial) — failed, the stage runs again from it (EC-15)", &p1[..12])),
        "{err}"
    );
    let a1 = attempt(&env, "D-2/implement/1");
    assert_eq!(
        (
            a1.status,
            a1.error_class.as_deref(),
            a1.partial_commit.as_deref()
        ),
        (AttemptStatus::Failed, Some("Unknown"), Some(p1.as_str()))
    );
    // the crashed run wrote no call record: the prompt as the fake kept it
    let prompt =
        std::fs::read_to_string(task(&env, "D-2").captures().join("implement-2.prompt.md"))
            .unwrap();
    assert!(
        prompt.contains("failed before it finished (Unknown: first)") && prompt.contains(&p1[..12]),
        "{prompt}"
    );
    let p2 = rev(&w, "HEAD");
    assert_eq!(rev(&w, &format!("{p2}^")), p1);
    let (code, err) = run_after_the_crash(&env, "D-2");
    assert_eq!(code, 1, "{err}");
    assert_eq!(task(&env, "D-2").state, St::Escalated, "{err}");
    assert_eq!(
        reason(&env, "D-2"),
        format!(
            "implementer failed: Some(Timeout) second; its partial work is kept in the recovery checkpoint {} (Toryo-Partial)",
            &p2[..12]
        )
    );
    let a2 = attempt(&env, "D-2/implement/2");
    assert_eq!(
        (
            a2.status,
            a2.error_class.as_deref(),
            a2.partial_commit.as_deref()
        ),
        (AttemptStatus::Failed, Some("Timeout"), Some(p2.as_str()))
    );
    assert_eq!(
        attempts(&env, "D-2").len(),
        3,
        "plan/1, implement/1, implement/2: §11 once"
    );
    assert_eq!(
        trailers(&env, "D-2").trim(),
        "toryo(D-2): implement (recovery checkpoint of failed attempt 2)|implement|2|true|\ntoryo(D-2): implement (recovery checkpoint of failed attempt 1)|implement|1|true|"
    );
    assert!(clean(&w));
}

/// The owner dies after the decision is recorded but before the recovery checkpoint is
/// committed: recovery commits the work and ends the attempt as decided — the stage runs
/// again from it, told that the turn failed.
#[test]
fn f8_a_failed_turn_that_crashed_before_its_partial_commit_ends_as_decided() {
    let env = setup("codex-openai");
    create(&env, "D-3");
    let s = script(
        &env,
        "d3.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), fail("Unknown", "the agent died half way")])],
        }),
    );
    let (code, err) = toryo(
        &env,
        &["run", "D-3", "--yes", "--fake-agents", &s],
        &[("TORYO_TEST_CRASH_AT", "implement:before_partial_commit")],
    );
    assert_eq!(code, 86, "{err}");
    let w = wt(&env, "D-3");
    assert!(
        partial_commits(&w).is_empty(),
        "the crash came before the commit"
    );
    assert!(!clean(&w));
    let (code, err) = run_after_the_crash(&env, "D-3");
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "D-3").state, St::Completed, "{err}");
    let a1 = attempt(&env, "D-3/implement/1");
    let partial = a1.partial_commit.clone().expect("a recovery checkpoint");
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
    assert!(a1.partial_snapshot_id.is_some(), "snapshot first (R39)");
    let prompt = calls(&env, "D-3")
        .into_iter()
        .find(|c| c["key"] == "implementer")
        .unwrap()["prompt"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        prompt.contains("failed before it finished (Unknown: the agent died half way)")
            && prompt.contains(&partial[..12]),
        "{prompt}"
    );
    let a2 = attempt(&env, "D-3/implement/2");
    assert_eq!(
        (a2.git_head_before.as_deref(), a2.checkpoint_kind.as_deref()),
        (Some(partial.as_str()), Some("no_change"))
    );
    assert_eq!(
        trailers(&env, "D-3").trim(),
        "toryo(D-3): implement|implement|2||true\ntoryo(D-3): implement (recovery checkpoint of failed attempt 1)|implement|1|true|"
    );
}
