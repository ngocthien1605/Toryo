//! M14 — check gates beyond X++: `diagnostics: msbuild` on a `program` gate, and `toryo gate
//! list|run` (a gate outside a task). End to end through the `toryo` binary with fake agents and
//! a **fake build tool**: a script that prints MSBuild-format error lines and exits 1 until the
//! checkout has `src/FIXED`. No real compiler ever runs.
#![cfg(feature = "m0-kit")]

mod common;

use std::path::{Path, PathBuf};

use common::*;
use serde_json::{json, Value};
use toryo::engine::PipelineState as St;

fn fwd(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// The fake build tool: prints two MSBuild diagnostics and `Build FAILED.`, exits 1 — unless the
/// working directory has `src/FIXED`. When `marker` is given it also writes that file in the
/// working directory first (what a gate that ran leaves behind).
fn fake_build(root: &Path, marker: Option<&str>) -> PathBuf {
    #[cfg(windows)]
    {
        let p = root.join("fake-build.cmd");
        let mark = marker
            .map(|m| format!(">\"{m}\" echo ran\r\n"))
            .unwrap_or_default();
        std::fs::write(
            &p,
            format!(
                "@echo off\r\n{mark}echo src/Calculator.xml(12,5): error XPP0101: 'amount' is not declared.\r\necho src/Tax.xml(40): error XPP0202: Method calc not found.\r\necho Build FAILED.\r\nif exist \"src\\FIXED\" exit /b 0\r\nexit /b 1\r\n"
            ),
        )
        .unwrap();
        p
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let p = root.join("fake-build.sh");
        let mark = marker
            .map(|m| format!("echo ran > '{m}'\n"))
            .unwrap_or_default();
        std::fs::write(
            &p,
            format!(
                "#!/bin/sh\n{mark}echo \"src/Calculator.xml(12,5): error XPP0101: 'amount' is not declared.\"\necho \"src/Tax.xml(40): error XPP0202: Method calc not found.\"\necho 'Build FAILED.'\nif [ -f src/FIXED ]; then exit 0; fi\nexit 1\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }
}

/// The calculator fixture whose only verify command is `build`: the fake build tool, with
/// `extra` (e.g. `diagnostics: msbuild`) after its program.
fn setup_build(extra: &str, marker: Option<&str>) -> Env {
    let env = setup("codex-openai");
    let tool = fake_build(&env.root, marker);
    let divide = r#"{ name: divide-present, program: git, args: ["grep", "-q", "Divide-Numbers", "--", "src"] }"#;
    let item = format!(
        "{{ name: build, program: \"{}\"{extra}, timeout_min: 5 }}",
        fwd(&tool)
    );
    let cfg = std::fs::read_to_string(&env.config).unwrap();
    std::fs::write(&env.config, cfg.replace(divide, &item)).unwrap();
    env
}

/// Run a task whose first verify fails (the fake build tool) and whose second turn writes
/// `src/FIXED`; returns the fix-cycle prompt's `verify` data block and the failed
/// `VERIFY_RESULT.json` (version 1).
fn failing_run(env: &Env, id: &str) -> (String, Value) {
    create(env, id);
    let s = script(
        env,
        "fix.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]),
                turn(vec![write("src/FIXED", "1"), implementation()]),
            ],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(env, &["run", id, "--fake-agents", &s], &[]);
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
            &s,
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(env, id);
    assert_eq!(t.state, St::WaitingFinalApproval, "{err}");
    assert_eq!(t.fix_cycle, 1);
    let dir = toryo::engine::artifacts::root(&t).join("verify-result");
    let first = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with("1-"))
        .unwrap();
    let vr: Value = serde_json::from_slice(&std::fs::read(first.path()).unwrap()).unwrap();
    let fix = calls(env, id)
        .into_iter()
        .filter(|c| c["key"] == "implementer")
        .nth(1)
        .unwrap();
    let block = fix["prompt"]
        .as_str()
        .unwrap()
        .split("<untrusted_data source=\"verify\">\n")
        .nth(1)
        .unwrap()
        .to_string();
    (block, vr)
}

/// A `program` gate with `diagnostics: msbuild`: the fixer reads the errors first, headed
/// `Build errors`, and the failed verify's entry carries them; nothing claims "compile".
#[test]
fn a_program_gate_with_msbuild_diagnostics_sends_file_line_and_code() {
    let env = setup_build(", diagnostics: msbuild", None);
    let (block, vr) = failing_run(&env, "G-1");
    assert!(
        block.starts_with("- VERIFY FAILED: `build` exited 1. Build errors (2 of 2):\n  - src/Calculator.xml(12,5): error XPP0101: 'amount' is not declared.\n  - src/Tax.xml(40): error XPP0202: Method calc not found.\nLast output:"),
        "{block}"
    );
    assert_eq!(vr["pass"], false);
    assert_ne!(vr["scope"], "compile", "a program gate is opaque");
    let c = &vr["commands"][0];
    assert_eq!(c["errors_total"], 2);
    assert_eq!(c["warnings_total"], 0);
    assert_eq!(
        c["diagnostics"],
        json!([
            {"file": "src/Calculator.xml", "line": 12, "column": 5, "severity": "error", "code": "XPP0101", "message": "'amount' is not declared."},
            {"file": "src/Tax.xml", "line": 40, "severity": "error", "code": "XPP0202", "message": "Method calc not found."},
        ])
    );
    assert!(c.get("kind").is_none(), "{c}");
}

/// Without the key nothing changes: the feedback is the command and its last output.
#[test]
fn a_program_gate_without_diagnostics_sends_the_tail_only() {
    let env = setup_build("", None);
    let (block, vr) = failing_run(&env, "G-2");
    assert!(
        block.starts_with("- VERIFY FAILED: `build` exited 1. Last output:\n```\n"),
        "{block}"
    );
    let c = &vr["commands"][0];
    for k in ["diagnostics", "errors_total", "warnings_total"] {
        assert!(c.get(k).is_none(), "{k} in {c}");
    }
}

/// Run `toryo …` for a test env; returns (exit code, stdout, stderr).
fn run_out(env: &Env, args: &[&str]) -> (i32, String, String) {
    let o = command(env, args, &[]).output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

/// `toryo gate list` shows what Verify runs: the name, the limit, how its output is read, the
/// command.
#[test]
fn gate_list_shows_the_check_gates() {
    let env = setup_build(", diagnostics: msbuild", None);
    let (code, stdout, stderr) = run_out(&env, &["gate", "list"]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(
        stdout.contains("check gates (run by Verify, in this order):"),
        "{stdout}"
    );
    assert!(
        stdout.contains("  build  [5 min, diagnostics: msbuild]  ")
            && stdout.contains("fake-build"),
        "{stdout}"
    );
}

/// `gate run NAME --print-only` prints the command and the TORYO_* variables and runs nothing;
/// without the flag the gate runs in the repository with your own rights: the diagnostics are
/// printed, a failure exits 1, the fixed tree exits 0. An unknown name, and `--low` on a program
/// gate, are refused.
#[test]
fn gate_run_prints_runs_and_refuses() {
    let env = setup_build(", diagnostics: msbuild", Some("ran.marker"));
    let marker = env.repo.join("ran.marker");
    let (code, stdout, stderr) = run_out(&env, &["gate", "run", "build", "--print-only"]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(stdout.contains("#   TORYO_TASK=gate-run"), "{stdout}");
    assert!(stdout.contains("#   TORYO_SHA=working-tree"), "{stdout}");
    assert!(
        stdout
            .lines()
            .any(|l| l.starts_with("& '") && l.contains("fake-build")),
        "{stdout}"
    );
    assert!(!marker.exists(), "--print-only runs nothing");

    let (code, stdout, stderr) = run_out(&env, &["gate", "run", "build"]);
    assert_eq!(code, 1, "{stdout}\n{stderr}");
    assert!(marker.exists(), "the gate ran in the repository");
    assert!(
        stdout.contains("✘ build (exit 1") && stdout.contains("2 error(s), 0 warning(s)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("  src/Calculator.xml(12,5): error XPP0101: 'amount' is not declared."),
        "{stdout}"
    );
    let log = stdout
        .split("full output: ")
        .nth(1)
        .unwrap()
        .lines()
        .next()
        .unwrap();
    assert!(std::fs::read_to_string(log.trim())
        .unwrap()
        .contains("Build FAILED."));
    std::fs::write(env.repo.join("src").join("FIXED"), "1").unwrap();
    let (code, stdout, stderr) = run_out(&env, &["gate", "run", "build"]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(stdout.contains("✔ build (exit 0"), "{stdout}");

    let (code, _, stderr) = run_out(&env, &["gate", "run", "nope"]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("no gate `nope` — the check gates are: build"),
        "{stderr}"
    );
    let (code, _, stderr) = run_out(&env, &["gate", "run", "build", "--low"]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("--low is for `tool: xpp` gates"),
        "{stderr}"
    );
}

/// An environment gate is listed, never run: it changes your environment, so only a task runs it.
#[test]
fn an_environment_gate_is_listed_and_never_run_here() {
    let org = ORGCHART.replace("IMPLEMENTER", "codex-openai").replace(
        "seats:",
        "  dbsync: { kind: environment, timeout: 5m }\nseats:",
    );
    let env = setup_orgchart(
        "codex-openai",
        "",
        &[
            (".toryo/toryo-orgchart.yaml", &org),
            (
                ".toryo/roles/implementer/toryo-role.yaml",
                "toryo: 1\npurpose: Change the code.\nbefore_handoff: [divide-present, dbsync]\n",
            ),
        ],
    );
    let machine = std::fs::read_to_string(&env.machine).unwrap();
    std::fs::write(
        &env.machine,
        format!("{machine}environment_gates:\n  dbsync: {{ program: git, args: [--version], lock: d365-env }}\n"),
    )
    .unwrap();
    let (code, stdout, stderr) = run_out(&env, &["gate", "list"]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(
        stdout.contains("  divide-present  [5 min]  git grep"),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "  dbsync  (environment gate — runs with your full rights; not run by this command)"
        ),
        "{stdout}"
    );
    let (code, _, stderr) = run_out(&env, &["gate", "run", "dbsync"]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("`dbsync` is an environment gate"),
        "{stderr}"
    );
}

/// A `program` gate **without** `diagnostics: msbuild`: `gate run` reads nothing out of the
/// output — no error count, no parsed lines — and a failure shows the output's last lines
/// instead (where `Build FAILED.` stands); a pass shows no output at all.
#[test]
fn gate_run_without_diagnostics_shows_the_last_output_of_a_failure() {
    let env = setup_build("", None);
    let (code, stdout, stderr) = run_out(&env, &["gate", "run", "build"]);
    assert_eq!(code, 1, "{stdout}\n{stderr}");
    assert!(stdout.contains("✘ build (exit 1"), "{stdout}");
    assert!(
        !stdout.contains("error(s)"),
        "no count without diagnostics: {stdout}"
    );
    let tail = stdout
        .split("last output:\n")
        .nth(1)
        .unwrap_or_else(|| panic!("no last output: {stdout}"));
    assert!(tail.contains("Build FAILED."), "{stdout}");
    assert!(
        tail.contains("src/Calculator.xml(12,5): error XPP0101"),
        "the raw lines, as printed: {stdout}"
    );
    std::fs::write(env.repo.join("src").join("FIXED"), "1").unwrap();
    let (code, stdout, stderr) = run_out(&env, &["gate", "run", "build"]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(stdout.contains("✔ build (exit 0"), "{stdout}");
    assert!(!stdout.contains("last output:"), "{stdout}");
}

/// `gate list` says a gate's limit as written: a limit that is not whole minutes in seconds
/// (`90s` → `90 s`), whole minutes in minutes.
#[test]
fn gate_list_says_a_limit_that_is_not_whole_minutes_in_seconds() {
    let org = ORGCHART
        .replace("IMPLEMENTER", "codex-openai")
        .replace("timeout: 5m", "timeout: 90s")
        .replace(
            "seats:",
            "  quick: { kind: check, program: git, args: [--version], timeout: 2m }\nseats:",
        );
    let env = setup_orgchart(
        "codex-openai",
        "",
        &[
            (".toryo/toryo-orgchart.yaml", &org),
            (
                ".toryo/roles/implementer/toryo-role.yaml",
                "toryo: 1\npurpose: Change the code.\nbefore_handoff: [divide-present, quick]\n",
            ),
        ],
    );
    let (code, stdout, stderr) = run_out(&env, &["gate", "list"]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(
        stdout.contains("  divide-present  [90 s]  git grep"),
        "{stdout}"
    );
    assert!(
        stdout.contains("  quick  [2 min]  git --version"),
        "{stdout}"
    );
    // listed in the order Verify runs them (before_handoff)
    let (a, b) = (
        stdout.find("divide-present").unwrap(),
        stdout.find("quick").unwrap(),
    );
    assert!(a < b, "{stdout}");
}

/// No check gate at all — no `commands.verify` and a repository without `scripts/build.ps1` or
/// `scripts/test.ps1` (the fallback): `gate list` says Verify has nothing to run and how to add a
/// gate, and `gate run` refuses any name with that same fact (not a list of zero names). (An org
/// chart cannot get here: it must declare a check gate.)
#[test]
fn with_no_check_gate_list_says_so_and_run_refuses_every_name() {
    let env = setup("codex-openai");
    let bare = env.root.join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    let cfg = std::fs::read_to_string(&env.config).unwrap();
    let divide = "commands:\n  verify:\n    - { name: divide-present, program: git, args: [\"grep\", \"-q\", \"Divide-Numbers\", \"--\", \"src\"] }\n";
    let root = format!("root: \"{}\"", fwd(&env.repo));
    assert!(cfg.contains(divide) && cfg.contains(&root), "{cfg}");
    std::fs::write(
        &env.config,
        cfg.replace(divide, "")
            .replace(&root, &format!("root: \"{}\"", fwd(&bare))),
    )
    .unwrap();
    let (code, stdout, stderr) = run_out(&env, &["gate", "list"]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(
        stdout.contains("no check gate configured — Verify has nothing to run (skipped)"),
        "{stdout}"
    );
    let (code, _, stderr) = run_out(&env, &["gate", "run", "build"]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("no gate `build`: no check gate is configured (`toryo gate list`)"),
        "{stderr}"
    );
}

/// `gate run --repo` names the folder the gate runs in: one that is not a folder is refused
/// before anything runs (the fake build tool would leave its marker).
#[test]
fn gate_run_refuses_a_repo_that_is_not_a_folder() {
    let env = setup_build("", Some("ran.marker"));
    let missing = env.root.join("no-such-repo");
    let (code, _, stderr) = run_out(
        &env,
        &["gate", "run", "build", "--repo", missing.to_str().unwrap()],
    );
    assert_ne!(code, 0);
    assert!(stderr.contains("is not a folder (--repo)"), "{stderr}");
    assert!(!env.repo.join("ran.marker").exists());
    assert!(!missing.exists());
}
