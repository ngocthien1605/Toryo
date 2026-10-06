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
/// fails (Git's `info/exclude` cannot be written). Was: branch, worktree and run store stayed;
/// the retry was refused. Now all three are removed, one line each. (M13-R2: the failure is the
/// test hook `TORYO_TEST_FAIL_AT=create:after_worktree`, not a folder at `info/exclude` —
/// Git 2.43 refuses such a folder before anything is created, Git 2.55 only warns.)
#[test]
fn m13_11_a_create_that_fails_after_the_worktree_removes_branch_and_worktree() {
    let env = setup("codex-openai");
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "N-2"],
        &[("TORYO_TEST_FAIL_AT", "create:after_worktree")],
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
    create(&env, "N-2");
    assert_eq!(task(&env, "N-2").state, St::New);
}

// ── M13-12 (X29): Ctrl+C on `toryo run` is a stop request ─────────────────────────────────

/// `toryo run <id>` that presses Ctrl+C on itself at `presses` (milliseconds after its start;
/// `TORYO_TEST_INTERRUPT_AFTER_MS`). On Windows it gets a console of its own, without a window
/// — the key goes to every process of a console, and the test must not get it. Returns the
/// exit code (`None`: ended by a signal) and what it printed to stderr.
fn run_and_press_ctrl_c(env: &Env, id: &str, script: &str, presses: &str) -> (Option<i32>, String) {
    let mut c = command(
        env,
        &["run", id, "--yes", "--fake-agents", script],
        &[("TORYO_TEST_INTERRUPT_AFTER_MS", presses)],
    );
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let log = env.root.join(format!("{id}-ctrl-c.log"));
    c.stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap());
    let mut child = c.spawn().unwrap();
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if started.elapsed() > std::time::Duration::from_secs(90) {
            let _ = child.kill();
            panic!(
                "`toryo run {id}` did not end after Ctrl+C:\n{}",
                std::fs::read_to_string(&log).unwrap_or_default()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    (
        status.code(),
        std::fs::read_to_string(&log).unwrap_or_default(),
    )
}

fn event_types(env: &Env, id: &str) -> Vec<String> {
    std::fs::read_to_string(task(env, id).run_store.join("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v["type"].as_str().map(str::to_string))
        .collect()
}

/// AC-1: one Ctrl+C while the planner's turn runs. Was: the process was killed — no
/// `run_ended`, the attempt left `Running`, recovery had to clean up. Now it is what
/// `toryo stop` is: the turn is ended, the attempt is `Interrupted`, the state stays (R38),
/// `run_ended` is written, and the first press says what the second one does.
#[test]
fn m13_12_ctrl_c_on_run_is_a_stop_request() {
    let env = setup("codex-openai");
    create(&env, "K-1");
    let s = script(&env, "k1.json", json!({"planner": [turn(vec![hang()])]}));
    let (code, err) = run_and_press_ctrl_c(&env, "K-1", &s, "6000");
    assert!(
        err.contains("stop requested — the running turn or gate is ended at the next boundary; press Ctrl+C again to end now"),
        "{err}"
    );
    assert!(
        err.contains("stop request handled"),
        "as after `toryo stop`: {err}"
    );
    assert!(code.is_some(), "the process ended by itself: {err}");
    let a = attempts(&env, "K-1")
        .into_iter()
        .find(|a| a.id == "K-1/plan/1")
        .expect("the plan attempt");
    assert_eq!(
        a.status,
        toryo::engine::store::AttemptStatus::Interrupted,
        "{err}"
    );
    assert_eq!(
        task(&env, "K-1").state,
        St::Planning,
        "a stop leaves the state (R38)"
    );
    assert_eq!(
        event_types(&env, "K-1").last().map(String::as_str),
        Some("run_ended"),
        "{err}"
    );
    // and `resume` goes on, as after `toryo stop`
    let s2 = script(
        &env,
        "k1b.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["resume", "K-1", "--yes", "--fake-agents", &s2], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "K-1").state, St::Completed, "{err}");
}

/// AC-2: two presses, one right after the other — the second ends the process at once, as
/// Ctrl+C always did: nothing is recorded (the attempt stays `Running`, no `run_ended`), and
/// the next `resume` recovers as today.
#[test]
fn m13_12_a_second_ctrl_c_ends_the_process_at_once() {
    let env = setup("codex-openai");
    create(&env, "K-2");
    let s = script(&env, "k2.json", json!({"planner": [turn(vec![hang()])]}));
    let (code, err) = run_and_press_ctrl_c(&env, "K-2", &s, "6000,6000");
    assert_ne!(code, Some(0), "killed, not ended: {err}");
    let a = attempts(&env, "K-2")
        .into_iter()
        .find(|a| a.id == "K-2/plan/1")
        .expect("the plan attempt");
    assert_eq!(
        a.status,
        toryo::engine::store::AttemptStatus::Running,
        "nothing was recorded: {err}"
    );
    assert_ne!(
        event_types(&env, "K-2").last().map(String::as_str),
        Some("run_ended"),
        "{err}"
    );
    let s2 = script(
        &env,
        "k2b.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["resume", "K-2", "--yes", "--fake-agents", &s2], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("recovery:"), "{err}");
    assert_eq!(task(&env, "K-2").state, St::Completed, "{err}");
}

// ── M13-13: a launch error before the agent starts does not reserve a budget ──────────────

/// AC-1: the planner's CLI cannot be started (the fixture's profile names a program that is
/// not on any machine) and the task has a small budget. Real adapters: no `--fake-agents` —
/// and nothing is started. Was: the turn that never ran reserved the tokens of a turn with
/// unknown usage (600 000), and the task paused on "budget" without a word about the program.
/// Now nothing is reserved and the task escalates with the launch error in the reason.
#[test]
fn m13_13_a_launch_error_reserves_nothing_and_says_what_is_wrong() {
    let env = setup_with("codex-openai", "pipeline:\n  budget_tokens: 100000\n");
    create(&env, "L-1");
    let (code, err) = toryo(&env, &["run", "L-1", "--yes", "--accept-unverified"], &[]);
    assert_ne!(code, 0, "{err}");
    let t = task(&env, "L-1");
    assert_eq!(t.state, St::Escalated, "not paused on the budget: {err}");
    let why = t.machine.reason().unwrap_or_default().to_string();
    assert!(
        why.contains("ProcessCrashed") && why.contains("claude-not-installed"),
        "the launch error is the reason: {why}"
    );
    assert_eq!(t.reserved_tokens, 0, "{err}");
    let reserved: i64 = db(&env)
        .call(|tx| {
            Ok(tx.query_row(
                "SELECT COALESCE(SUM(reservation_tokens), 0) FROM usage WHERE task_id = 'L-1'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(reserved, 0, "no usage row holds a reservation: {err}");
}

// ── M13-14 (8): a task id is checked on every command; device names are no ids ────────────

/// `status`, `task show`, `task cleanup` and `approve` refuse an id that is no id before they
/// touch anything; an id from a `TASK.md` title that is longer than 64 characters is refused;
/// a Windows device name (`CON`, `NUL`, `COM1`, … — also before a dot, in any case) and a
/// name that ends in a dot are no task ids (they are no file names on Windows: the lock file,
/// the run store and the worktree are named after the id).
#[test]
fn m13_14_every_command_refuses_what_is_not_a_task_id() {
    let env = setup("codex-openai");
    for bad in ["../victim-x", "a/b", "..", "x y"] {
        for args in [
            vec!["status", bad],
            vec!["task", "show", bad],
            vec!["task", "cleanup", bad],
            vec!["approve", bad, "--gate", "plan"],
        ] {
            let (code, err) = toryo(&env, &args, &[]);
            assert_ne!(code, 0, "{args:?}: {err}");
            assert!(err.contains("is not a task id"), "{args:?}: {err}");
        }
    }
    assert!(!env.data.join("victim-x.lock").exists());
    // the id taken from the title (`# <ID> — …`): 67 characters
    let long = format!("T{}-12345", "a".repeat(60));
    let file = env.root.join("long.md");
    std::fs::write(
        &file,
        format!("# {long} — a long id\n\n## Acceptance criteria\n- AC-1: one\n"),
    )
    .unwrap();
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", file.to_str().unwrap()],
        &[],
    );
    assert_ne!(code, 0, "{err}");
    assert!(
        err.contains("is not a task id") && err.contains("at most 64"),
        "{err}"
    );
    // device names, and a trailing dot
    for bad in [
        "CON", "nul", "Aux", "prn", "COM1", "lpt9", "con.task", "NUL.1", "T-1.",
    ] {
        assert!(
            toryo::engine::task::valid_task_id(bad).is_err(),
            "`{bad}` was taken for a task id"
        );
        let (code, err) = toryo(
            &env,
            &["task", "create", "--file", "TASK.md", "--id", bad],
            &[],
        );
        assert_ne!(code, 0, "{bad}: {err}");
        assert!(err.contains("is not a task id"), "{bad}: {err}");
    }
    for ok in ["CONSOLE-1", "com10", "T-1.2", "nullable", "a"] {
        assert!(toryo::engine::task::valid_task_id(ok).is_ok(), "{ok}");
    }
}
