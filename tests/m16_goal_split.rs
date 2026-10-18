//! M16-03 acceptance: `toryo goal split` turns a goal into task files `task create` accepts,
//! through the `toryo` binary with a fake planner. The planner works in a throwaway checkout
//! that is removed on every exit; nothing is written when its answer is not usable (X38).
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};

fn out(env: &Env, args: &[&str]) -> (i32, String, String) {
    let o = command(env, args, &[]).output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

const GOAL: &str = "# Goal: a calculator people can trust\n\nAdd division (with a clear error for zero) and document the module.\n";

fn split_answer() -> Value {
    json!({
        "summary": "Division first, then the docs that describe it.",
        "tasks": [
            {
                "title": "Add Divide-Numbers",
                "description": "Add `Divide-Numbers` to `src/Calculator.psm1`.",
                "acceptance_criteria": ["Divide-Numbers 6 3 returns 2", "Dividing by zero throws 'Division by zero'"],
                "depends_on": [],
                "files_likely_involved": ["src/Calculator.psm1"]
            },
            {
                "title": "Document the module",
                "description": "Describe every function in README.md.",
                "acceptance_criteria": ["README.md lists Divide-Numbers"],
                "depends_on": [1]
            }
        ],
        "open_questions": ["Should division round?"]
    })
}

fn planner(env: &Env, name: &str, steps: Vec<Value>) -> String {
    script(env, name, json!({ "planner": [turn(steps)] }))
}

fn complete(v: Value) -> Value {
    json!({ "kind": "complete", "structured": v })
}

/// The worktrees Git knows for the fixture repository (the main checkout included).
fn worktrees(env: &Env) -> usize {
    git(&env.repo, &["worktree", "list", "--porcelain"])
        .lines()
        .filter(|l| l.starts_with("worktree "))
        .count()
}

fn goals_dir_is_clean(env: &Env) {
    for e in std::fs::read_dir(env.data.join("goals"))
        .into_iter()
        .flatten()
        .flatten()
    {
        assert!(
            !e.path().join("worktree").exists(),
            "a throwaway checkout was left: {}",
            e.path().display()
        );
    }
}

#[test]
fn a_goal_becomes_task_files_that_task_create_accepts() {
    let env = setup("codex-openai");
    std::fs::write(env.root.join("GOAL.md"), GOAL).unwrap();
    let s = planner(&env, "split.json", vec![complete(split_answer())]);
    let before = worktrees(&env);
    let (code, stdout, stderr) = out(
        &env,
        &[
            "goal",
            "split",
            "--file",
            "GOAL.md",
            "--out",
            "tasks",
            "--prefix",
            "CALC",
            "--fake-agents",
            &s,
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    let dir = env.root.join("tasks");
    // stdout: one path per file (for a script); stderr: what was made and what to type
    assert_eq!(stdout.lines().count(), 2, "{stdout}");
    assert!(
        stdout.contains("CALC-1.md") && stdout.contains("CALC-2.md"),
        "{stdout}"
    );
    assert!(
        stderr.contains("✔ GOAL.md split into 2 task(s)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("CALC-2  Document the module  (after CALC-1)"),
        "{stderr}"
    );
    assert!(stderr.contains("  - Should division round?"), "{stderr}");
    assert!(stderr.contains("then `toryo run CALC-1`"), "{stderr}");
    assert!(
        stderr.contains("after the tasks it depends on are merged"),
        "{stderr}"
    );
    let t1 = std::fs::read_to_string(dir.join("CALC-1.md")).unwrap();
    assert!(t1.starts_with("# CALC-1 — Add Divide-Numbers\n"), "{t1}");
    assert!(
        t1.contains("- AC-2: Dividing by zero throws 'Division by zero'"),
        "{t1}"
    );
    let t2 = std::fs::read_to_string(dir.join("CALC-2.md")).unwrap();
    assert!(
        t2.contains("Depends on CALC-1 (Add Divide-Numbers)"),
        "{t2}"
    );
    let m: Value =
        serde_json::from_slice(&std::fs::read(dir.join("GOAL-SPLIT.json")).unwrap()).unwrap();
    assert_eq!(m["waves"], json!([["CALC-1"], ["CALC-2"]]));
    assert_eq!(m["planner_profile"], "claude-max");
    assert_eq!(m["tasks"][1]["depends_on"], json!(["CALC-1"]));
    assert_eq!(m["base_commit"].as_str().unwrap().len(), 40);
    // the planner read a throwaway checkout, which is gone; the goal is in its prompt and the
    // splitter's role text in its context
    assert_eq!(worktrees(&env), before);
    goals_dir_is_clean(&env);
    assert!(clean(&env.repo));
    let calls: Vec<Value> = std::fs::read_dir(env.data.join("goals"))
        .unwrap()
        .flatten()
        .filter_map(|e| std::fs::read(e.path().join("fake-calls.json")).ok())
        .flat_map(|b| serde_json::from_slice::<Vec<Value>>(&b).unwrap())
        .collect();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(calls[0]["prompt"]
        .as_str()
        .unwrap()
        .contains("a calculator people can trust"));
    let ctx = calls[0]["context"].as_str().unwrap();
    assert!(ctx.contains("Role: Goal splitter"), "{ctx}");
    assert!(!ctx.contains("Role: Planner"), "{ctx}");
    // the files are tasks: `task create` takes them as they are
    let (code, err) = toryo(&env, &["task", "create", "--file", "tasks/CALC-1.md"], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "CALC-1");
    assert_eq!(t.acceptance_ids, ["AC-1", "AC-2"]);
    assert_eq!(t.title, "CALC-1 — Add Divide-Numbers");
    // and a second split under the same prefix is refused before the planner runs
    let s2 = planner(&env, "split2.json", vec![complete(split_answer())]);
    let (code, _, stderr) = out(
        &env,
        &[
            "goal",
            "split",
            "--file",
            "GOAL.md",
            "--out",
            "tasks2",
            "--prefix",
            "CALC",
            "--fake-agents",
            &s2,
        ],
    );
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("CALC-1 already exists — choose another --prefix"),
        "{stderr}"
    );
    assert!(!env.root.join("tasks2").exists());
}

#[test]
fn what_the_planner_writes_in_its_checkout_is_discarded() {
    let env = setup("codex-openai");
    std::fs::write(env.root.join("GOAL.md"), GOAL).unwrap();
    let s = planner(
        &env,
        "writer.json",
        vec![
            write("src/Calculator.psm1", DIVIDE),
            complete(split_answer()),
        ],
    );
    let (code, _, stderr) = out(
        &env,
        &[
            "goal",
            "split",
            "--file",
            "GOAL.md",
            "--out",
            "t",
            "--prefix",
            "W",
            "--fake-agents",
            &s,
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("!! the planner changed 1 file(s) in its throwaway checkout — discarded"),
        "{stderr}"
    );
    assert!(clean(&env.repo), "the owner's checkout is untouched");
    goals_dir_is_clean(&env);
}

#[test]
fn nothing_is_written_when_the_split_is_not_usable() {
    let env = setup("codex-openai");
    std::fs::write(env.root.join("GOAL.md"), GOAL).unwrap();
    let mut later = split_answer();
    later["tasks"][0]["depends_on"] = json!([2]);
    for (name, steps, want) in [
        (
            "later.json",
            vec![complete(later)],
            "can depend only on an earlier task",
        ),
        (
            "schema.json",
            vec![complete(json!({ "summary": "s" }))],
            "does not match goal-split.schema.json",
        ),
        (
            "fail.json",
            vec![fail("RateLimited", "slow down")],
            "the split turn failed",
        ),
    ] {
        let s = planner(&env, name, steps);
        let (code, _, stderr) = out(
            &env,
            &[
                "goal",
                "split",
                "--file",
                "GOAL.md",
                "--out",
                "never",
                "--prefix",
                "N",
                "--fake-agents",
                &s,
            ],
        );
        assert_eq!(code, 2, "{name}: {stderr}");
        assert!(stderr.contains(want), "{name}: {stderr}");
        assert!(
            !env.root.join("never").exists(),
            "{name}: a file was written"
        );
        goals_dir_is_clean(&env);
    }
    assert!(clean(&env.repo));
}

#[test]
fn the_inputs_are_checked_before_the_planner_runs() {
    let env = setup("codex-openai");
    std::fs::write(env.root.join("GOAL.md"), GOAL).unwrap();
    std::fs::write(env.root.join("EMPTY.md"), "  \n").unwrap();
    std::fs::create_dir_all(env.root.join("full")).unwrap();
    std::fs::write(env.root.join("full").join("x.md"), "x").unwrap();
    let s = planner(&env, "split.json", vec![complete(split_answer())]);
    for (args, want) in [
        (
            vec!["--file", "GOAL.md", "--out", "full", "--prefix", "A"],
            "is not an empty folder",
        ),
        (
            vec!["--file", "GOAL.md", "--out", "o", "--prefix", "1A"],
            "is not a task-id prefix",
        ),
        (
            vec!["--file", "EMPTY.md", "--out", "o", "--prefix", "A"],
            "is empty",
        ),
        (
            vec![
                "--file",
                "GOAL.md",
                "--out",
                "o",
                "--prefix",
                "A",
                "--profile",
                "nope",
            ],
            "nope",
        ),
        (
            vec![
                "--file",
                "GOAL.md",
                "--out",
                "o",
                "--prefix",
                "A",
                "--max-tasks",
                "21",
            ],
            "21",
        ),
    ] {
        let mut a = vec!["goal", "split"];
        a.extend(args.iter());
        a.extend(["--fake-agents", &s]);
        let (code, _, stderr) = out(&env, &a);
        assert_eq!(code, 2, "{args:?}: {stderr}");
        assert!(stderr.contains(want), "{args:?}: {stderr}");
    }
    assert!(
        !env.data.join("goals").exists()
            || std::fs::read_dir(env.data.join("goals")).unwrap().count() == 0
    );
}

#[test]
fn in_org_chart_mode_the_planner_seat_splits() {
    let env = setup_orgchart("codex-openai", "", &[]);
    std::fs::write(env.root.join("GOAL.md"), GOAL).unwrap();
    let s = planner(&env, "split.json", vec![complete(split_answer())]);
    let (code, stdout, stderr) = out(
        &env,
        &[
            "goal",
            "split",
            "--file",
            "GOAL.md",
            "--out",
            "org",
            "--prefix",
            "ORG",
            "--fake-agents",
            &s,
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("── GOAL SPLIT (claude-max, base "),
        "{stderr}"
    );
    assert_eq!(stdout.lines().count(), 2, "{stdout}");
    let (code, err) = toryo(&env, &["task", "create", "--file", "org/ORG-2.md"], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "ORG-2").acceptance_ids, ["AC-1"]);
    goals_dir_is_clean(&env);
}

/// M16-R2: the base is chosen as `task create` chooses it — a `project.base_branch` that does
/// not exist is an error naming the key, before the planner runs.
#[test]
fn a_missing_base_branch_is_named_like_task_create_names_it() {
    let env = setup("codex-openai");
    std::fs::write(env.root.join("GOAL.md"), GOAL).unwrap();
    let cfg = std::fs::read_to_string(&env.config).unwrap();
    std::fs::write(
        &env.config,
        cfg.replacen("project:\n", "project:\n  base_branch: no-such-branch\n", 1),
    )
    .unwrap();
    let s = planner(&env, "split.json", vec![complete(split_answer())]);
    let (code, _, stderr) = out(
        &env,
        &[
            "goal",
            "split",
            "--file",
            "GOAL.md",
            "--out",
            "o",
            "--prefix",
            "B",
            "--fake-agents",
            &s,
        ],
    );
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("project.base_branch: the branch `no-such-branch` does not exist"),
        "{stderr}"
    );
    assert!(!env.root.join("o").exists());
    goals_dir_is_clean(&env);
}
