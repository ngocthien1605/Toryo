//! M12 configuration and CLI tests: `project.base_branch` (M12-08) and the keys that do
//! nothing, and the other config behaviour of the milestone as it comes.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::Value;

/// The fixture repository with `feature/wip` checked out, one commit ahead of `main`.
fn on_a_feature_branch(env: &Env) -> (String, String) {
    let main = git(&env.repo, &["rev-parse", "main"]).trim().to_string();
    git(&env.repo, &["checkout", "-q", "-b", "feature/wip"]);
    commit_repo(env, &[("wip.txt", "work in progress\n")], "wip");
    let feature = git(&env.repo, &["rev-parse", "HEAD"]).trim().to_string();
    assert_ne!(main, feature);
    (main, feature)
}

fn set_project_key(env: &Env, line: &str) {
    let y = std::fs::read_to_string(&env.config).unwrap();
    assert!(y.contains("  worktrees_root:"), "{y}");
    std::fs::write(
        &env.config,
        y.replacen(
            "  worktrees_root:",
            &format!("  {line}\n  worktrees_root:"),
            1,
        )
        .replace("    base_branch", "  base_branch")
        .replace("    runs_root", "  runs_root"),
    )
    .unwrap();
}

/// AC-1: `project.base_branch: main` with `feature/wip` checked out — the task branches from
/// `main`, says so, and notes that another branch is checked out. (Was: it branched from the
/// feature commit; the key was read nowhere.)
#[test]
fn m12_08_base_branch_is_the_base_of_task_create() {
    let env = setup("codex-openai");
    let (main, _feature) = on_a_feature_branch(&env);
    set_project_key(&env, "base_branch: main");
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "B-1"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "B-1").base_commit, main);
    assert!(
        err.contains(&format!("base: main @ {}", &main[..12])),
        "{err}"
    );
    assert!(
        err.contains("note: feature/wip is checked out") && err.contains("project.base_branch"),
        "{err}"
    );
    // `--base` still wins
    let (code, err) = toryo(
        &env,
        &[
            "task",
            "create",
            "--file",
            "TASK.md",
            "--id",
            "B-1b",
            "--base",
            "feature/wip",
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        task(&env, "B-1b").base_commit,
        git(&env.repo, &["rev-parse", "feature/wip"]).trim()
    );
    // a branch that does not exist is an error that names the key
    let y = std::fs::read_to_string(&env.config).unwrap();
    assert!(y.contains("  base_branch: main\n"), "{y}");
    std::fs::write(
        &env.config,
        y.replacen("  base_branch: main\n", "  base_branch: nope\n", 1),
    )
    .unwrap();
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "B-1c"],
        &[],
    );
    assert_ne!(code, 0);
    assert!(
        err.contains("project.base_branch: the branch `nope` does not exist"),
        "{err}"
    );
}

/// AC-2: no key — today's behaviour: the checked-out branch, and no note about another one.
#[test]
fn m12_08_without_the_key_the_checked_out_branch_is_the_base() {
    let env = setup("codex-openai");
    let (_main, feature) = on_a_feature_branch(&env);
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "B-2"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "B-2").base_commit, feature);
    assert!(err.contains("base: feature/wip @"), "{err}");
    assert!(!err.contains("is checked out"), "{err}");
}

/// AC-3: a key that is accepted and read nowhere says so in `config show`, `doctor` and
/// `task create`; a configuration without them says nothing.
#[test]
fn m12_08_keys_without_effect_say_so() {
    let env = setup("codex-openai");
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "K-0"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains("has no effect"), "{err}");
    set_project_key(&env, "runs_root: 'C:/somewhere/else'");
    let y = std::fs::read_to_string(&env.config).unwrap();
    std::fs::write(
        &env.config,
        format!("{y}pipeline:\n  budget_usd_est: 5.0\n").replace(
            "verify: { boundary: none,",
            "verify: { final_clean: false, boundary: none,",
        ),
    )
    .unwrap();
    let lines = [
        "`project.runs_root` is accepted but has no effect in this version",
        "`verify.final_clean` is accepted but has no effect in this version",
        "`pipeline.budget_usd_est` is accepted but has no effect in this version",
    ];
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "K-1"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    for l in lines {
        assert!(err.contains(l), "create: {l}\n{err}");
    }
    let o = command(&env, &["doctor"], &[]).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    for l in lines {
        assert!(out.contains(l), "doctor: {l}\n{out}");
    }
    let o = command(&env, &["config", "show"], &[]).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    for l in lines {
        assert!(out.contains(l), "config show: {l}\n{out}");
    }
    let o = command(&env, &["config", "show", "--json"], &[])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        v["no_effect"],
        serde_json::json!([
            "project.runs_root",
            "verify.final_clean",
            "pipeline.budget_usd_est"
        ])
    );
}
