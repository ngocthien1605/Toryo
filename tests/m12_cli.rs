//! M12 CLI tests (M12-12): the three traps — `task cleanup` that deletes unsaved work, a
//! `resume` without an id that takes flags of one task, and a task id that is a path.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::json;
use toryo::engine::PipelineState as St;

/// An escalated task (`gate_unavailable`: the build gate's program is not there).
fn escalated(env: &Env, id: &str) {
    let y = std::fs::read_to_string(&env.config).unwrap();
    let old = r#"    - { name: divide-present, program: git, args: ["grep", "-q", "Divide-Numbers", "--", "src"] }"#;
    if y.contains(old) {
        std::fs::write(
            &env.config,
            y.replace(
                old,
                "    - { name: build, program: no-such-program, args: [] }",
            ),
        )
        .unwrap();
    }
    create(env, id);
    let s = script(
        env,
        &format!("{id}.json"),
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(env, &["run", id, "--yes", "--fake-agents", &s], &[]);
    assert_eq!(task(env, id).state, St::Escalated, "{err}");
}

/// (a) An escalated task whose worktree holds unsaved work, or whose run store holds a partial
/// snapshot, is not cleaned up without `--force`; the message says what would be lost. (Was:
/// no `--force` needed for `Escalated`, and the changes and snapshots were deleted.)
#[test]
fn m12_12_cleanup_asks_for_force_when_it_would_delete_unsaved_work() {
    let env = setup("codex-openai");
    escalated(&env, "C-1");
    let t = task(&env, "C-1");
    // unsaved work in the worktree
    let wip = t.worktree.join("unsaved.txt");
    std::fs::write(&wip, "not committed\n").unwrap();
    let (code, err) = toryo(&env, &["task", "cleanup", "C-1"], &[]);
    assert_ne!(code, 0, "{err}");
    assert!(
        err.contains("would delete 1 uncommitted change(s)") && err.contains("--force"),
        "{err}"
    );
    assert!(t.worktree.exists() && task(&env, "C-1").state == St::Escalated);
    // a partial snapshot in the run store
    std::fs::remove_file(&wip).unwrap();
    let snap = t.run_store.join("artifacts/partial/implement-1/0-abc");
    std::fs::create_dir_all(&snap).unwrap();
    let (code, err) = toryo(&env, &["task", "cleanup", "C-1"], &[]);
    assert_ne!(code, 0, "{err}");
    assert!(err.contains("partial snapshot(s) of 1 attempt(s)"), "{err}");
    assert!(snap.exists());
    // with --force it goes
    let (code, err) = toryo(&env, &["task", "cleanup", "C-1", "--force"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(!t.worktree.exists() && !t.run_store.exists());
}

/// (a) Nothing unsaved: an escalated task is cleaned up without `--force`, as before.
#[test]
fn m12_12_cleanup_of_a_clean_escalated_task_needs_no_force() {
    let env = setup("codex-openai");
    escalated(&env, "C-2");
    let (code, err) = toryo(&env, &["task", "cleanup", "C-2"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(!task(&env, "C-2").worktree.exists());
}

/// (b) Flags of one task need the id; without one, tasks in `New` are not started and a task
/// that fails does not stop the others (the exit code is non-zero).
#[test]
fn m12_12_resume_without_an_id() {
    let env = setup("codex-openai");
    let (code, err) = toryo(&env, &["resume", "--note", "x", "--yes"], &[]);
    assert_ne!(code, 0);
    assert!(
        err.contains("--note belongs to one task") && err.contains("toryo resume <id> --note"),
        "{err}"
    );
    // a New task is not started; a task that cannot be resumed does not stop the next one
    escalated(&env, "R-1");
    escalated(&env, "R-2");
    create(&env, "R-3");
    std::fs::remove_dir_all(task(&env, "R-1").worktree).unwrap();
    let (code, err) = toryo(&env, &["resume", "--yes"], &[]);
    assert_ne!(code, 0, "{err}");
    assert!(err.contains("R-1: error:"), "{err}");
    let (r1, r2) = (err.find("resume R-1").unwrap(), err.find("resume R-2"));
    assert!(r2.is_some_and(|r2| r2 > r1), "R-2 was still resumed: {err}");
    assert!(err.contains("R-3: not started"), "{err}");
    assert_eq!(task(&env, "R-3").state, St::New);
}

/// (c) A task id is a name, not a path: refused before any file is touched, on `run` and on
/// `task create`.
#[test]
fn m12_12_a_task_id_is_never_a_path() {
    let env = setup("codex-openai");
    for bad in ["../victim-x", "a/b", "..", "a..b", "_x", "x y"] {
        let (code, err) = toryo(&env, &["run", bad], &[]);
        assert_ne!(code, 0, "{bad}: {err}");
        assert!(err.contains("is not a task id"), "{bad}: {err}");
    }
    assert!(!env.data.join("victim-x.lock").exists());
    assert!(!env
        .data
        .join("locks")
        .join("..")
        .join("victim-x.lock")
        .exists());
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "../evil"],
        &[],
    );
    assert_ne!(code, 0);
    assert!(err.contains("is not a task id"), "{err}");
}

/// M12-21: `toryo --version` prints the version and the commit it was built from (`unknown` where
/// there was no Git).
#[test]
fn m12_21_version_names_the_commit() {
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_toryo"))
        .arg("--version")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout).trim().to_string();
    let re = regex::Regex::new(r"^toryo \d+\.\d+\.\d+ \(([0-9a-f]{7,40}|unknown)\)$").unwrap();
    assert!(re.is_match(&out), "{out}");
    assert!(out.contains(env!("CARGO_PKG_VERSION")), "{out}");
}

/// M13-07 (a): a worktree Git cannot look into (its `.git` file holds garbage) with a file
/// nobody committed. Was: a failing `git status` counted as "nothing to lose" and `task
/// cleanup` deleted the folder without `--force`. Now the cleanup that cannot look refuses.
#[test]
fn m13_07_cleanup_that_cannot_look_refuses_without_force() {
    let env = setup("codex-openai");
    escalated(&env, "C-7");
    let t = task(&env, "C-7");
    std::fs::write(t.worktree.join(".git"), "garbage\n").unwrap();
    let wip = t.worktree.join("unsaved.txt");
    std::fs::write(&wip, "not committed\n").unwrap();
    let (code, err) = toryo(&env, &["task", "cleanup", "C-7"], &[]);
    assert_ne!(code, 0, "{err}");
    assert!(
        err.contains("cannot tell what the worktree holds") && err.contains("--force"),
        "{err}"
    );
    assert!(wip.exists() && t.run_store.exists(), "{err}");
    assert_eq!(task(&env, "C-7").state, St::Escalated);
    // `--force` does what the message says, also where `git worktree remove` cannot
    let (code, err) = toryo(&env, &["task", "cleanup", "C-7", "--force"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(!t.worktree.exists() && !t.run_store.exists(), "{err}");
    let listed = git(&env.repo, &["worktree", "list"]);
    assert!(!listed.contains("C-7"), "{listed}");
}

// ── M13-11 (X28): `task create` is all or nothing ─────────────────────────────────────────

/// Nothing of the task `id` is left: no row, no run store, no worktree, no branch.
fn assert_nothing_left(env: &Env, id: &str, err: &str) {
    let i = id.to_string();
    let row = db(env).call(move |tx| Ok(toryo::engine::store::get_task(tx, &i).is_ok()));
    assert!(!row.unwrap(), "a row is left: {err}");
    assert!(
        !env.data.join("runs").join(id).exists(),
        "the run store is left: {err}"
    );
    assert!(!wt(env, id).exists(), "the worktree is left: {err}");
    let branches = git(&env.repo, &["branch", "--list", &format!("toryo/{id}")]);
    assert!(
        branches.trim().is_empty(),
        "the branch is left: {branches}\n{err}"
    );
    let listed = git(&env.repo, &["worktree", "list"]);
    assert!(!listed.contains(id), "{listed}");
}

/// AC-1: `task create` fails after the run store exists — a **file** where the worktrees
/// folder should be. Was: the run store stayed, and the retry was refused ("run store …
/// already exists"). Now what was made is removed, the error is the original one, and the same
/// `TASK.md` creates once the cause is gone.
#[test]
fn m13_11_a_create_that_fails_at_the_worktree_leaves_nothing() {
    let env = setup("codex-openai");
    let wts = env.root.join("wts");
    assert!(!wts.exists(), "the fixture does not make it");
    std::fs::write(&wts, "a file, not a folder\n").unwrap();
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "N-1"],
        &[],
    );
    assert_ne!(code, 0, "{err}");
    assert!(err.contains("removed: the run store"), "{err}");
    assert!(
        err.contains("error: creating project.worktrees_root"),
        "the error that ended the creation is the one reported: {err}"
    );
    assert_nothing_left(&env, "N-1", &err);
    // the cause is gone: the same TASK.md creates
    std::fs::remove_file(&wts).unwrap();
    create(&env, "N-1");
    assert!(wt(&env, "N-1").join("TASK.md").exists());
}

/// The same after the **branch and the worktree** exist: the step after `git worktree add`
/// fails (Git's `info/exclude` cannot be written — here it is a folder). Was: branch, worktree
/// and run store stayed; the retry was refused. Now all three are removed, one line each.
#[test]
fn m13_11_a_create_that_fails_after_the_worktree_removes_branch_and_worktree() {
    let env = setup("codex-openai");
    let exclude = env.repo.join(".git").join("info").join("exclude");
    let _ = std::fs::remove_file(&exclude);
    std::fs::create_dir_all(&exclude).unwrap();
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "N-2"],
        &[],
    );
    assert_ne!(code, 0, "{err}");
    assert!(
        err.contains("removed: the worktree")
            && err.contains("removed: the branch toryo/N-2")
            && err.contains("removed: the run store"),
        "{err}"
    );
    assert!(
        err.contains("exclude"),
        "the original error is the one reported: {err}"
    );
    assert_nothing_left(&env, "N-2", &err);
    std::fs::remove_dir(&exclude).unwrap();
    create(&env, "N-2");
    assert_eq!(task(&env, "N-2").state, St::New);
}
