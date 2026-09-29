//! M9 group B — X++ verify on the UDE toolchain (plan v0.8 R59), end to end through the
//! `toryo` binary with fake agents and a **fake MSBuild**: a script that appends its argv to a
//! log, prints a prepared output and exits 1 for one project (`.cmd` on Windows, `sh` on Unix).
//! No real MSBuild ever runs. The pure parts (packages, argv, diagnostics) are unit-tested in
//! `src/engine/xpp.rs`.
#![cfg(feature = "m0-kit")]

mod common;

use std::path::{Path, PathBuf};

use common::*;
use serde_json::{json, Value};
use toryo::engine::store::AttemptStatus;
use toryo::engine::xpp::PACKAGES;
use toryo::engine::PipelineState as St;

struct Xpp {
    env: Env,
    /// One line per MSBuild run: its arguments.
    log: PathBuf,
    nugets: PathBuf,
}

/// The fake MSBuild: logs its arguments, prints `errors`, then `output`, then `errors` again
/// (MSBuild's summary), and exits 1 when its first argument (the project) has the stem
/// `fail_stem` — unless the checkout has `src/FIXED` (a fix). In `errors`, `{CWD}` is the
/// working directory (the verify checkout) and `{SEP}` the path separator.
fn fake_msbuild(root: &Path, fail_stem: &str, output: &str, errors: &[&str]) -> (PathBuf, PathBuf) {
    let log = root.join("msbuild-argv.log");
    let out = root.join("msbuild-output.txt");
    std::fs::write(&out, output).unwrap();
    #[cfg(windows)]
    let exe = {
        let p = root.join("msbuild.cmd");
        let echo: String = errors
            .iter()
            .map(|e| {
                format!(
                    "echo {}\r\n",
                    e.replace("{CWD}", "%CD%").replace("{SEP}", "\\")
                )
            })
            .collect();
        std::fs::write(
            &p,
            format!(
                "@echo off\r\n>>\"{log}\" echo %*\r\n{echo}type \"{out}\"\r\n{echo}if exist \"src\\FIXED\" exit /b 0\r\nif /I \"%~n1\"==\"{fail_stem}\" exit /b 1\r\nexit /b 0\r\n",
                log = log.display(),
                out = out.display()
            ),
        )
        .unwrap();
        p
    };
    #[cfg(not(windows))]
    let exe = {
        use std::os::unix::fs::PermissionsExt;
        let p = root.join("msbuild.sh");
        let echo: String = errors
            .iter()
            .map(|e| {
                format!(
                    "echo \"{}\"\n",
                    e.replace("{CWD}", "$PWD").replace("{SEP}", "/")
                )
            })
            .collect();
        std::fs::write(
            &p,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{log}'\n{echo}cat '{out}'\n{echo}if [ -f src/FIXED ]; then exit 0; fi\ncase \"$(basename \"$1\")\" in {fail_stem}.*) exit 1;; esac\nexit 0\n",
                log = log.display(),
                out = out.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    };
    (exe, log)
}

fn fwd(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// The calculator fixture with committed build projects `Projects/Build/<p>.rnrproj`, the five
/// packages under `<root>/nugets`, `toolchains.xpp` in the machine config and a `tool: xpp`
/// verify command over `projects` (after the fixture's `divide-present` program command when
/// `with_program`).
fn setup_xpp(projects: &[&str], fail_stem: &str, output: &str, with_program: bool) -> Xpp {
    setup_xpp_with(projects, fail_stem, output, &[], with_program)
}

/// [`setup_xpp`] with error lines the fake MSBuild prints (see [`fake_msbuild`]).
fn setup_xpp_with(
    projects: &[&str],
    fail_stem: &str,
    output: &str,
    errors: &[&str],
    with_program: bool,
) -> Xpp {
    let env = setup("codex-openai");
    let files: Vec<(String, String)> = projects
        .iter()
        .map(|p| {
            (
                format!("Projects/Build/{p}.rnrproj"),
                "<Project />\n".into(),
            )
        })
        .collect();
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    commit_repo(&env, &refs, "build projects");
    let nugets = env.root.join("nugets");
    for id in PACKAGES {
        std::fs::create_dir_all(
            nugets
                .join(format!("{id}.7.0.7198.40"))
                .join("ref")
                .join("net40"),
        )
        .unwrap();
    }
    let (msbuild, log) = fake_msbuild(&env.root, fail_stem, output, errors);
    let machine = std::fs::read_to_string(&env.machine).unwrap();
    std::fs::write(
        &env.machine,
        format!(
            "{machine}toolchains:\n  xpp:\n    msbuild: \"{}\"\n    nugets: \"{}\"\n",
            fwd(&msbuild),
            fwd(&nugets)
        ),
    )
    .unwrap();
    let list: Vec<String> = projects
        .iter()
        .map(|p| format!("Projects/Build/{p}.rnrproj"))
        .collect();
    let item = format!(
        "{{ name: build, tool: xpp, metadata: src, projects: [{}], timeout_min: 5 }}",
        list.join(", ")
    );
    let divide = r#"{ name: divide-present, program: git, args: ["grep", "-q", "Divide-Numbers", "--", "src"] }"#;
    let cfg = std::fs::read_to_string(&env.config).unwrap();
    let with = if with_program {
        format!("{divide}\n    - {item}")
    } else {
        item
    };
    std::fs::write(&env.config, cfg.replace(divide, &with)).unwrap();
    Xpp { env, log, nugets }
}

/// The logged MSBuild runs: the project file name of each, in order.
fn built(x: &Xpp) -> Vec<String> {
    std::fs::read_to_string(&x.log)
        .unwrap_or_default()
        .lines()
        .map(|l| {
            let first = l.split_whitespace().next().unwrap_or("").trim_matches('"');
            first.rsplit(['/', '\\']).next().unwrap_or("").to_string()
        })
        .collect()
}

fn verify_result(env: &Env, id: &str) -> Value {
    let t = task(env, id);
    serde_json::from_slice(&std::fs::read(t.artifacts().join("VERIFY_RESULT.json")).unwrap())
        .unwrap()
}

/// M9-B2 AC-3/AC-5: a passing build passes verify; the projects build in order, each with
/// its own `VERIFY_RESULT.json` entry (`kind: xpp`, the toolchain, `scope: compile`), and a
/// `program` command next to it works as before.
#[test]
fn a_passing_xpp_build_passes_verify() {
    let x = setup_xpp(
        &["Calc.Build", "Ext.Build"],
        "nothing",
        "Build succeeded.\n",
        true,
    );
    let env = &x.env;
    create(env, "X-1");
    let s = script(
        env,
        "x1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(env, &["run", "X-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(env, "X-1").state, St::Completed, "{err}");
    assert_eq!(built(&x), ["Calc.Build.rnrproj", "Ext.Build.rnrproj"]);
    assert!(
        err.contains("✔ build/Calc.Build") && err.contains("✔ build/Ext.Build"),
        "{err}"
    );

    let vr = verify_result(env, "X-1");
    assert_eq!(vr["pass"], true);
    assert_eq!(vr["scope"], "compile");
    let cmds = vr["commands"].as_array().unwrap();
    assert_eq!(cmds.len(), 3);
    assert_eq!(cmds[0]["name"], "divide-present");
    assert_eq!(cmds[0]["argv"][0], "git", "a program command is unchanged");
    assert!(cmds[0].get("kind").is_none());
    for (c, stem) in cmds[1..].iter().zip(["Calc.Build", "Ext.Build"]) {
        assert_eq!(c["name"], format!("build/{stem}"));
        assert_eq!(c["kind"], "xpp");
        assert_eq!(c["scope"], "compile");
        assert_eq!(c["exit"], 0);
        assert_eq!(c["project"], format!("Projects/Build/{stem}.rnrproj"));
        let argv: Vec<&str> = c["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        assert!(argv[0].contains("msbuild"), "{argv:?}");
        assert!(argv[1].ends_with(&format!("{stem}.rnrproj")));
        assert!(argv.contains(&"-noAutoResponse") && argv.contains(&"-nodeReuse:false"));
        let out = argv
            .iter()
            .find_map(|a| a.strip_prefix("/p:OutputDirectory="))
            .unwrap();
        assert!(Path::new(out).ends_with("X-1/build/xpp"), "{out}");
        assert!(Path::new(out).is_dir(), "the output folder is created");
        let pk = &c["toolchain"]["packages"];
        assert_eq!(pk.as_array().unwrap().len(), 5);
        assert_eq!(pk[0]["id"], PACKAGES[0]);
        assert_eq!(pk[0]["version"], "7.0.7198.40");
    }
    // the build projects are verify files: hashed like verify scripts
    assert!(
        vr["scripts"]["Projects/Build/Calc.Build.rnrproj"].is_string(),
        "{vr}"
    );
}

/// M9-B2 AC-3: a failing build goes to Fixing with the feedback; the projects stop at the
/// first failure; the fixed checkpoint builds.
#[test]
fn a_failing_xpp_build_goes_to_fixing() {
    let x = setup_xpp(
        &["Calc.Build", "Ext.Build", "Third.Build"],
        "Ext.Build",
        "Ext.Build: compiling\nBuild FAILED.\n",
        false,
    );
    let env = &x.env;
    create(env, "X-2");
    let s = script(
        env,
        "x2.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]),
                turn(vec![write("src/FIXED", "1"), implementation()]),
            ],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(env, &["run", "X-2", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(env, "X-2");
    assert_eq!(t.state, St::Completed, "{err}");
    assert_eq!(t.fix_cycle, 1);
    assert_eq!(
        built(&x),
        [
            // first verify: stops at the failing project, Third.Build never runs
            "Calc.Build.rnrproj",
            "Ext.Build.rnrproj",
            // after the fix: all three
            "Calc.Build.rnrproj",
            "Ext.Build.rnrproj",
            "Third.Build.rnrproj",
        ]
    );
    let v1 = attempts(env, "X-2")
        .into_iter()
        .find(|a| a.id == "X-2/verify/1")
        .unwrap();
    assert_eq!(v1.status, AttemptStatus::Failed);
    let fix = calls(env, "X-2")
        .into_iter()
        .filter(|c| c["key"] == "implementer")
        .nth(1)
        .unwrap();
    let prompt = fix["prompt"].as_str().unwrap();
    assert!(
        prompt.contains("VERIFY FAILED: `build/Ext.Build` exited 1"),
        "{prompt}"
    );
    assert!(
        prompt.contains("<untrusted_data source=\"verify\">"),
        "{prompt}"
    );
}

/// M9-B2 AC-4 (`xpp_gate_integrity`): the implementer may not change what is built. An edit of
/// the (tracked) build project is a path-policy violation — restored, never committed, sent
/// back; a new `Directory.Build.props` is an untracked protected file, which R35 escalates
/// (`protected_config_changed`) — never committed either.
#[test]
fn xpp_gate_integrity() {
    let x = setup_xpp(&["Calc.Build"], "nothing", "Build succeeded.\n", false);
    let env = &x.env;
    create(env, "X-3");
    let t = task(env, "X-3");
    let cfg = toryo::engine::task::frozen_config(&t).unwrap();
    let (_, prot, _) = toryo::engine::runner::task_policy(&t, &cfg).unwrap();
    for p in [
        "Projects/Build/Calc.Build.rnrproj",
        "Directory.Build.props",
        "src/Directory.Build.targets",
        "Projects/Build/Directory.Build.rsp",
    ] {
        assert!(prot.violates(p), "{p}");
    }
    let s = script(
        env,
        "x3.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write("Projects/Build/Calc.Build.rnrproj", "<!-- skip the build -->\n"), implementation()]),
                turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]),
            ],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(env, &["run", "X-3", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(env, "X-3");
    assert_eq!(t.state, St::Completed, "{err}");
    assert_eq!(t.fix_cycle, 1);
    let a = attempts(env, "X-3")
        .into_iter()
        .find(|a| a.id == "X-3/implement/1")
        .unwrap();
    assert_eq!(a.status, AttemptStatus::Failed);
    assert_eq!(a.error_class.as_deref(), Some("policy_violation"));
    assert!(a
        .error_detail
        .unwrap()
        .contains("Projects/Build/Calc.Build.rnrproj"));
    let changed = git(
        &t.worktree,
        &["log", "--name-only", "--format=", "main..HEAD"],
    );
    assert!(!changed.contains("rnrproj"), "{changed}");
    assert_eq!(
        built(&x),
        ["Calc.Build.rnrproj"],
        "one verify, of the clean checkpoint"
    );

    create(env, "X-3b");
    let s = script(
        env,
        "x3b.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![
                json!({"kind":"write_file","path":"src/Directory.Build.props","content":"<Project />\n"}),
                write("src/Calculator.psm1", DIVIDE),
                implementation(),
            ])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(env, &["run", "X-3b", "--yes", "--fake-agents", &s], &[]);
    assert_ne!(code, 0, "{err}");
    let t = task(env, "X-3b");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert!(
        t.machine
            .reason()
            .unwrap_or("")
            .starts_with("protected_config_changed"),
        "{:?}",
        t.machine.reason()
    );
    assert!(t
        .machine
        .reason()
        .unwrap()
        .contains("src/Directory.Build.props"));
    let changed = git(
        &t.worktree,
        &["log", "--name-only", "--format=", "main..HEAD"],
    );
    assert!(!changed.contains("Directory.Build"), "{changed}");
    assert_eq!(built(&x), ["Calc.Build.rnrproj"], "nothing was verified");
}

/// R59 admission: a missing package refuses `task create`; a toolchain that breaks after it
/// escalates the verify stage (`xpp_toolchain_unavailable`) instead of starting a fix cycle.
#[test]
fn a_broken_xpp_toolchain_refuses_admission_or_escalates() {
    let x = setup_xpp(&["Calc.Build"], "nothing", "Build succeeded.\n", false);
    let env = &x.env;
    let suite = x.nugets.join(format!("{}.7.0.7198.40", PACKAGES[4]));
    let aside = x.nugets.join("aside");
    std::fs::rename(&suite, &aside).unwrap();
    let (code, err) = toryo(
        env,
        &["task", "create", "--file", "TASK.md", "--id", "X-4"],
        &[],
    );
    assert_ne!(code, 0);
    assert!(err.contains("xpp_toolchain_unavailable"), "{err}");
    assert!(
        err.contains(&format!("{}: not found", PACKAGES[4])),
        "{err}"
    );

    std::fs::rename(&aside, &suite).unwrap();
    create(env, "X-4");
    let s = script(
        env,
        "x4.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    std::fs::rename(&suite, &aside).unwrap();
    let (code, err) = toryo(env, &["run", "X-4", "--yes", "--fake-agents", &s], &[]);
    assert_ne!(code, 0);
    let t = task(env, "X-4");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(t.fix_cycle, 0, "not the implementer's fault");
    let v = attempts(env, "X-4")
        .into_iter()
        .find(|a| a.stage == "verify")
        .unwrap();
    assert_eq!(v.error_class.as_deref(), Some("xpp_toolchain_unavailable"));
    assert!(built(&x).is_empty());
}

/// M9-B3 AC-2/AC-3 (`xpp_diagnostics`, plan §16): the fake MSBuild prints 3 000 lines with two
/// errors near the top (repeated in its summary). Both errors are in `diagnostics` of the
/// failed verify, once each, with paths relative to the checkout; the fixer's feedback starts
/// with them; `status` and the final gate say that verify was compile only.
#[test]
fn xpp_diagnostics() {
    let errors = [
        "{CWD}{SEP}src{SEP}Calculator.xml(12,5): error XPP0101: 'amount' is not declared. [{CWD}{SEP}Projects{SEP}Build{SEP}Calc.Build.rnrproj]",
        "{CWD}{SEP}src{SEP}Tax.xml(40): error XPP0202: Method calc not found.",
    ];
    let filler: String = (1..=3000)
        .map(|i| format!("  compiling object {i} of 3000\n"))
        .collect();
    let x = setup_xpp_with(&["Calc.Build"], "Calc.Build", &filler, &errors, false);
    let env = &x.env;
    create(env, "X-5");
    let s = script(
        env,
        "x5.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]),
                turn(vec![write("src/FIXED", "1"), implementation()]),
            ],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(env, &["run", "X-5", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = toryo(
        env,
        &[
            "approve",
            "X-5",
            "--gate",
            "plan",
            "--continue",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(env, "X-5");
    assert_eq!(t.state, St::WaitingFinalApproval, "{err}");
    assert_eq!(t.fix_cycle, 1);
    const NOTE: &str = "verify: compile only (X++: no DB sync, no unit tests)";
    assert!(err.contains(&format!("final gate — {NOTE}")), "{err}");
    assert!(status_out(env, "X-5").contains(NOTE));
    let summary = std::fs::read_to_string(t.artifacts().join("SUMMARY.md")).unwrap();
    assert!(summary.contains(NOTE), "{summary}");

    // the failed verify: version 1 of VERIFY_RESULT.json
    let dir = toryo::engine::artifacts::root(&t).join("verify-result");
    let first = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with("1-"))
        .unwrap();
    let vr: Value = serde_json::from_slice(&std::fs::read(first.path()).unwrap()).unwrap();
    assert_eq!(vr["pass"], false);
    assert_eq!(vr["scope"], "compile");
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
    assert!(
        !c["output_tail"]
            .as_str()
            .unwrap()
            .contains("compiling object 100 of"),
        "the tail is the end of the output"
    );
    // the fixer reads the errors first, inside the verify data block
    let fix = calls(env, "X-5")
        .into_iter()
        .filter(|c| c["key"] == "implementer")
        .nth(1)
        .unwrap();
    let prompt = fix["prompt"].as_str().unwrap();
    let block = prompt
        .split("<untrusted_data source=\"verify\">\n")
        .nth(1)
        .unwrap();
    assert!(
        block.starts_with("- VERIFY FAILED: `build/Calc.Build` exited 1. X++ build errors (2 of 2):\n  - src/Calculator.xml(12,5): error XPP0101: 'amount' is not declared.\n  - src/Tax.xml(40): error XPP0202: Method calc not found.\nLast output:"),
        "{block}"
    );
}
