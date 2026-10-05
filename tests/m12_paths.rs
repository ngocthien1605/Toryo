//! M12 path tests: long paths on Windows (M12-13), and the other path behaviour of the
//! milestone as it comes.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::json;
use toryo::engine::PipelineState as St;

/// Commit a file whose path inside the repository is `depth` folders of 30 characters deep —
/// longer than 260 characters once the worktree or the verify checkout path is in front. The
/// commit is made with `-c core.longpaths=true`, as the owner's repository would have been.
fn commit_a_deep_file(env: &Env, depth: usize) -> String {
    let rel = (0..depth)
        .map(|i| format!("deep-folder-of-thirty-chars-{i:02}"))
        .collect::<Vec<_>>()
        .join("/");
    let dir = env.repo.join(&rel);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Model.xml"), "<Model/>\n").unwrap();
    let git = |a: &[&str]| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(&env.repo)
            .args([
                "-c",
                "core.longpaths=true",
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
            ])
            .args(a)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{a:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    };
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a deep file"]);
    format!("{rel}/Model.xml")
}

/// AC-1: a file whose path in the worktree and in the verify checkout passes 260 characters
/// goes through `task create` and one verify run.
#[test]
fn m12_13_a_path_over_260_characters_goes_through_create_and_verify() {
    let env = setup("codex-openai");
    let rel = commit_a_deep_file(&env, 9);
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "L-1"],
        &[],
    );
    assert_eq!(code, 0, "create: {err}");
    let t = task(&env, "L-1");
    let in_worktree = t.worktree.join(&rel).display().to_string();
    assert!(
        in_worktree.len() > 260,
        "the path must be long: {}",
        in_worktree.len()
    );
    let s = script(
        &env,
        "l1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "L-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(task(&env, "L-1").state, St::Completed, "{err}");
}

/// `doctor` prints the lengths of `worktrees_root` and `verify_root`, and warns (on Windows)
/// about a root of more than 70 characters with the way out.
#[test]
fn m12_13_doctor_prints_the_root_lengths() {
    let env = setup("codex-openai");
    let o = command(&env, &["doctor"], &[]).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    let line = out
        .lines()
        .find(|l| l.contains("worktrees_root is"))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(
        line.contains("characters")
            && line.contains("verify_root is")
            && line.contains("short enough"),
        "{line}"
    );
    let y = std::fs::read_to_string(&env.config).unwrap();
    let long = format!(
        "{}/wts-{}",
        env.root
            .display()
            .to_string()
            .replace(std::path::MAIN_SEPARATOR, "/"),
        "x".repeat(60)
    );
    let wts = env
        .root
        .join("wts")
        .display()
        .to_string()
        .replace(std::path::MAIN_SEPARATOR, "/");
    assert!(y.contains(&wts), "{y}");
    std::fs::write(&env.config, y.replace(&wts, &long)).unwrap();
    let o = command(&env, &["doctor"], &[]).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    let line = out
        .lines()
        .find(|l| l.contains("worktrees_root is"))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(line.contains("keep them short"), "{line}");
    if cfg!(windows) {
        assert!(line.contains("WARN"), "{line}");
    }
}
