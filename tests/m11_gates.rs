//! M11 group E end to end, on FakeAgent: check gates and their variables, M18 (E1), verify
//! output redaction (E2), environment gates (E3). Gate scripts are `pwsh` on Windows and `sh`
//! elsewhere, written into the fixture repository or a temp folder outside it.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use toryo::engine::PipelineState as St;

/// A check gate running the repository script `scripts/<name>.ps1` (Windows) or `.sh`; the
/// gate's YAML and the script file to commit.
fn script_gate(name: &str, ps1: &str, sh: &str) -> (String, (String, String)) {
    if cfg!(windows) {
        (
            format!("  {name}: {{ kind: check, program: pwsh, args: [-NoProfile, -NonInteractive, -File, scripts/{name}.ps1], timeout: 2m }}\n"),
            (format!("scripts/{name}.ps1"), ps1.to_string()),
        )
    } else {
        (
            format!("  {name}: {{ kind: check, program: sh, args: [scripts/{name}.sh], timeout: 2m }}\n"),
            (format!("scripts/{name}.sh"), sh.to_string()),
        )
    }
}

/// The fixture's org chart with the gates `decl` added and `tail` appended.
fn org(decl: &str, tail: &str) -> String {
    ORGCHART.replace("IMPLEMENTER", "codex-openai").replacen(
        "gates:\n",
        &format!("gates:\n{decl}"),
        1,
    ) + tail
}

fn role(handoff: &str) -> String {
    format!("toryo: 1\npurpose: Change the code.\nbefore_handoff: {handoff}\n")
}

fn full_script(env: &Env, name: &str) -> String {
    script(
        env,
        name,
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    )
}

fn verify_result(env: &Env, id: &str) -> Value {
    let p = task(env, id).artifacts().join("VERIFY_RESULT.json");
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

/// M11-E1 AC-1: a check gate script prints the four gate variables; VERIFY_RESULT.json shows
/// the task id, the checkpoint SHA, the checkout and the build folder.
#[test]
fn a_check_gate_gets_the_gate_variables() {
    let (decl, file) = script_gate(
        "vars",
        "\"TASK=$env:TORYO_TASK\"\n\"SHA=$env:TORYO_SHA\"\n\"CHECKOUT=$env:TORYO_VERIFY_CHECKOUT\"\n\"BUILD=$env:TORYO_VERIFY_BUILD\"\nexit 0\n",
        "echo \"TASK=$TORYO_TASK\"\necho \"SHA=$TORYO_SHA\"\necho \"CHECKOUT=$TORYO_VERIFY_CHECKOUT\"\necho \"BUILD=$TORYO_VERIFY_BUILD\"\nexit 0\n",
    );
    let o = org(&decl, "");
    let env = setup_orgchart(
        "codex-openai",
        "",
        &[
            (".toryo/toryo-orgchart.yaml", &o),
            (
                ".toryo/roles/implementer/toryo-role.yaml",
                &role("[divide-present, vars]"),
            ),
            (&file.0, &file.1),
        ],
    );
    create(&env, "GV-1");
    let s = full_script(&env, "gv.json");
    let (code, err) = toryo(&env, &["run", "GV-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "GV-1").state, St::Completed, "{err}");
    let vr = verify_result(&env, "GV-1");
    let gate = vr["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "vars")
        .unwrap();
    let tail = gate["output_tail"].as_str().unwrap();
    let head = vr["head"].as_str().unwrap();
    let checkout = vr["verify_checkout"].as_str().unwrap();
    assert!(tail.contains("TASK=GV-1"), "{tail}");
    assert!(tail.contains(&format!("SHA={head}")), "{tail}");
    assert!(tail.contains(&format!("CHECKOUT={checkout}")), "{tail}");
    let build = std::path::Path::new(checkout)
        .parent()
        .unwrap()
        .join("build");
    assert!(
        tail.contains(&format!("BUILD={}", build.display())),
        "{tail}"
    );
}

/// M11-E1 AC-2: a gate whose `-File` script is not in the base commit is refused at
/// admission, naming the gate — no task, no turn.
#[test]
fn a_gate_script_missing_from_the_base_commit_is_refused_at_admission() {
    let (decl, _file) = script_gate("later", "exit 0\n", "exit 0\n");
    let o = org(&decl, "");
    let env = setup_orgchart(
        "codex-openai",
        "",
        &[
            (".toryo/toryo-orgchart.yaml", &o),
            (
                ".toryo/roles/implementer/toryo-role.yaml",
                &role("[divide-present, later]"),
            ),
        ],
    );
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "M18-1"],
        &[],
    );
    assert_ne!(code, 0);
    let script = if cfg!(windows) {
        "scripts/later.ps1"
    } else {
        "scripts/later.sh"
    };
    assert!(
        err.contains(&format!(
            "verify_script_missing: gate `later` runs `{script}`, which is not in the base commit"
        )),
        "{err}"
    );
    assert!(!env.data.join("runs").join("M18-1").exists());
    assert!(!wt(&env, "M18-1").exists());
}

/// Every file under `dir`, recursively.
fn files_under(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

const E2_SECRET: &str = "m11-gate-s3cr3t-Q7";

/// M11-E2 AC-1 (review M2): a verify command that prints a configured secret value — the
/// value appears in no `VERIFY_RESULT.json` version, no fix prompt, no reviewer pack, not in
/// `events.jsonl`, the captures or anything else in the run store, nor on stderr.
#[test]
fn verify_output_is_redacted_with_the_task_secrets() {
    let env = setup_with("codex-openai", "");
    // the implementer's profile carries the secret (env:, so the test needs no Credential Manager)
    std::fs::write(
        &env.machine,
        MACHINE_PROFILES.replace(
            "exe: codex-not-installed }",
            "exe: codex-not-installed, secret_env: { TORYO_M11_TOKEN: \"env:TORYO_M11_E2_SECRET\" } }",
        ) + MACHINE_FLOOR,
    )
    .unwrap();
    // a verify command that prints the secret value, failing until Divide-Numbers exists
    let (prog, args, file, body) = if cfg!(windows) {
        (
            "pwsh",
            "[-NoProfile, -NonInteractive, -File, scripts/leak.ps1]",
            "scripts/leak.ps1",
            format!("\"leaked: {E2_SECRET}\"\nif (Select-String -Quiet -Pattern Divide-Numbers -Path src/Calculator.psm1) {{ exit 0 }} else {{ exit 1 }}\n"),
        )
    } else {
        (
            "sh",
            "[scripts/leak.sh]",
            "scripts/leak.sh",
            format!("echo \"leaked: {E2_SECRET}\"\ngrep -q Divide-Numbers src/Calculator.psm1\n"),
        )
    };
    commit_repo(&env, &[(file, &body)], "leak");
    let cfg = std::fs::read_to_string(&env.config).unwrap().replace(
        "    - { name: divide-present, program: git, args: [\"grep\", \"-q\", \"Divide-Numbers\", \"--\", \"src\"] }\n",
        &format!("    - {{ name: leak, program: {prog}, args: {args} }}\n"),
    );
    assert!(cfg.contains("name: leak"), "{cfg}");
    std::fs::write(&env.config, cfg).unwrap();
    create(&env, "RD-1");
    let s = script(
        &env,
        "rd.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![implementation()]), turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(
        &env,
        &["run", "RD-1", "--yes", "--fake-agents", &s],
        &[("TORYO_M11_E2_SECRET", E2_SECRET)],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "RD-1");
    assert_eq!(t.state, St::Completed, "{err}");
    assert_eq!(t.fix_cycle, 1, "the first verify failed: {err}");
    assert!(!err.contains(E2_SECRET), "stderr: {err}");
    let vr = verify_result(&env, "RD-1");
    assert!(
        vr["commands"][0]["output_tail"]
            .as_str()
            .unwrap()
            .contains("leaked: [REDACTED:TORYO_M11_TOKEN]"),
        "{vr}"
    );
    let calls = calls(&env, "RD-1");
    assert!(
        calls[2]["prompt"]
            .as_str()
            .unwrap()
            .contains("VERIFY FAILED"),
        "the fix prompt carries the verify output: {}",
        calls[2]
    );
    let mut checked = 0;
    for f in files_under(&t.run_store) {
        let bytes = std::fs::read(&f).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains(E2_SECRET),
            "{} leaks the secret",
            f.display()
        );
        checked += 1;
    }
    assert!(checked > 10, "{checked} files checked");
}

/// M11-E2 AC-2: output without a secret is byte-identical whether or not a redaction set is
/// given.
#[test]
fn output_without_secrets_is_unchanged_by_redaction() {
    use toryo::process::redact::{Redactions, Secret};
    use toryo::runner::{CommandRunner, LocalRunner, RunSpec};
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let run = |redact: Redactions| {
        let spec = RunSpec {
            name: "git".into(),
            program: toryo::engine::checks::resolve_program("git"),
            args: vec!["--version".into()],
            cwd: std::env::temp_dir(),
            env: std::env::vars().collect(),
            boundary: toryo::process::Boundary::None,
            timeout: std::time::Duration::from_secs(60),
            cancel: None,
            redact,
        };
        rt.block_on(LocalRunner.run(spec, None)).unwrap().output
    };
    let plain = run(Redactions::empty());
    let redacted = run(Redactions::new(&[Secret {
        name: "X".into(),
        value: "never-printed-secret-value".into(),
    }]));
    assert!(plain.contains("git version"), "{plain}");
    assert_eq!(plain.as_bytes(), redacted.as_bytes());
}

/// Group E review (E2 AC-2): with a secret configured that the command never prints, stdout and
/// stderr keep their order — the whole output is redacted at the end, no stream holds bytes back.
#[test]
fn a_configured_secret_does_not_reorder_stdout_and_stderr() {
    use toryo::process::redact::{Redactions, Secret};
    use toryo::runner::{CommandRunner, LocalRunner, RunSpec};
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (program, args): (&str, Vec<String>) = if cfg!(windows) {
        (
            "pwsh",
            vec![
                "-NoProfile".into(),
                "-Command".into(),
                "[Console]::Out.WriteLine('o1'); Start-Sleep -Milliseconds 400; [Console]::Error.WriteLine('e1'); Start-Sleep -Milliseconds 400; [Console]::Out.WriteLine('o2')".into(),
            ],
        )
    } else {
        (
            "sh",
            vec![
                "-c".into(),
                "echo o1; sleep 0.4; echo e1 >&2; sleep 0.4; echo o2".into(),
            ],
        )
    };
    let spec = RunSpec {
        name: "order".into(),
        program: toryo::engine::checks::resolve_program(program),
        args,
        cwd: std::env::temp_dir(),
        env: std::env::vars().collect(),
        boundary: toryo::process::Boundary::None,
        timeout: std::time::Duration::from_secs(60),
        cancel: None,
        redact: Redactions::new(&[Secret {
            name: "X".into(),
            value: "a-secret-value-long-enough-to-hold-bytes-back".into(),
        }]),
    };
    let out = rt.block_on(LocalRunner.run(spec, None)).unwrap().output;
    let order: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(order, ["o1", "e1", "o2"], "{out:?}");
}

/// M11-R6 AC-2 (applies to everyone — a toryo.yaml-only repository here): `bash -o pipefail
/// scripts/x.sh` with the script at the base commit is admitted (`pipefail` was taken for the
/// script and refused); `pwsh scripts/new.ps1` (positional, no `-File`) missing at the base
/// commit is refused, naming the command.
#[test]
fn script_detection_admits_flag_values_and_refuses_a_positional_script() {
    let with_command = |env: &Env, line: &str| {
        let y = std::fs::read_to_string(&env.config).unwrap();
        let old = r#"    - { name: divide-present, program: git, args: ["grep", "-q", "Divide-Numbers", "--", "src"] }"#;
        assert!(y.contains(old), "{y}");
        std::fs::write(&env.config, y.replace(old, line)).unwrap();
    };
    let env = setup("codex-openai");
    commit_repo(&env, &[("scripts/x.sh", "exit 0\n")], "a gate script");
    with_command(
        &env,
        r#"    - { name: strict, program: bash, args: ["-o", "pipefail", "scripts/x.sh"] }"#,
    );
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "R6-1"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let env = setup("codex-openai");
    with_command(
        &env,
        r#"    - { name: newgate, program: pwsh, args: ["-NoProfile", "scripts/new.ps1"] }"#,
    );
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "R6-2"],
        &[],
    );
    assert_ne!(code, 0);
    assert!(
        err.contains(
            "verify_script_missing: verify command `newgate` runs `scripts/new.ps1`, which is not in the base commit"
        ),
        "{err}"
    );
    // the net: a script passed to a program outside the table — M11-R10 (X7, changed): never
    // a refusal, a note
    let env = setup("codex-openai");
    with_command(
        &env,
        r#"    - { name: wrapped, program: dotnet, args: ["tool", "run", "runner", "scripts/gen.ps1"] }"#,
    );
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "R6-3"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains("note: verify command `wrapped` passes `scripts/gen.ps1`, which is not in the base commit — fine for build output"),
        "{err}"
    );
    // R4–R8 review: a glob, an output and a command line are no scripts — admitted
    let env = setup("codex-openai");
    with_command(
        &env,
        r#"    - { name: lint, program: git, args: ["ls-files", "src/**/*.psm1", "--output", "build/x.js", "-c", "cd src && ./t.sh"] }"#,
    );
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "R6-4"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
}

/// The fixture's verify command replaced by `line` (a toryo.yaml-only repository).
fn with_verify_command(env: &Env, line: &str) {
    let y = std::fs::read_to_string(&env.config).unwrap();
    let old = r#"    - { name: divide-present, program: git, args: ["grep", "-q", "Divide-Numbers", "--", "src"] }"#;
    assert!(y.contains(old), "{y}");
    std::fs::write(&env.config, y.replace(old, line)).unwrap();
}

fn create_as(env: &Env, id: &str) -> (i32, String) {
    toryo(
        env,
        &["task", "create", "--file", "TASK.md", "--id", id],
        &[],
    )
}

/// M11-R10 AC-2 (X7, applies to everyone): admission refuses only an entry script the
/// implementer could write — `pwsh -Command "msbuild.exe App.sln"` is admitted; `make
/// SCRIPT=ci/x.sh test` with the file at base is admitted and `ci/x.sh` protected; `node
/// dist/cli.js` is admitted when `dist/` is ignored and refused when it is not; `pwsh -File
/// scripts/smoke.ps1 -Exe build/app.exe` is admitted with a note; `bash -ec "./ci/new.sh"`
/// missing at base is refused.
#[test]
fn admission_refuses_only_entry_scripts_the_implementer_could_write() {
    let env = setup("codex-openai");
    with_verify_command(
        &env,
        r#"    - { name: build, program: pwsh, args: ["-Command", "msbuild.exe App.sln"] }"#,
    );
    let (code, err) = create_as(&env, "R10-1");
    assert_eq!(code, 0, "{err}");

    let env = setup("codex-openai");
    commit_repo(&env, &[("ci/x.sh", "exit 0\n")], "a script");
    with_verify_command(
        &env,
        r#"    - { name: mk, program: make, args: ["SCRIPT=ci/x.sh", "test"] }"#,
    );
    let (code, err) = create_as(&env, "R10-2");
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "R10-2");
    let cfgv = toryo::engine::task::frozen_config(&t).unwrap();
    let (_, prot, _) = toryo::engine::runner::task_policy(&t, &cfgv).unwrap();
    assert!(
        prot.violates("ci/x.sh"),
        "the value of SCRIPT= is protected"
    );

    let node = r#"    - { name: cli, program: node, args: ["dist/cli.js"] }"#;
    let env = setup("codex-openai");
    commit_repo(
        &env,
        &[(".gitignore", "dist/\n")],
        "build output is ignored",
    );
    with_verify_command(&env, node);
    let (code, err) = create_as(&env, "R10-3");
    assert_eq!(code, 0, "ignored build output is admitted: {err}");
    let t = task(&env, "R10-3");
    let cfgv = toryo::engine::task::frozen_config(&t).unwrap();
    let (_, prot, _) = toryo::engine::runner::task_policy(&t, &cfgv).unwrap();
    assert!(prot.violates("dist/cli.js"), "and stays protected");
    let env = setup("codex-openai");
    with_verify_command(&env, node);
    let (code, err) = create_as(&env, "R10-4");
    assert_ne!(code, 0);
    assert!(
        err.contains("verify_script_missing: verify command `cli` runs `dist/cli.js`, which is not in the base commit"),
        "{err}"
    );

    let env = setup("codex-openai");
    commit_repo(&env, &[("scripts/smoke.ps1", "exit 0\n")], "a smoke script");
    with_verify_command(
        &env,
        r#"    - { name: smoke, program: pwsh, args: ["-File", "scripts/smoke.ps1", "-Exe", "build/app.exe"] }"#,
    );
    let (code, err) = create_as(&env, "R10-5");
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains("note: verify command `smoke` passes `build/app.exe`, which is not in the base commit — fine for build output; if the verify command runs it as its script, commit it first"),
        "{err}"
    );
    // `doctor` says the same
    let o = command(&env, &["doctor"], &[]).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains(
            "note: verify command `smoke` passes `build/app.exe`, which is not in the base commit"
        ),
        "{out}"
    );

    let env = setup("codex-openai");
    with_verify_command(
        &env,
        r#"    - { name: fast, program: bash, args: ["-ec", "./ci/new.sh"] }"#,
    );
    let (code, err) = create_as(&env, "R10-6");
    assert_ne!(code, 0);
    assert!(
        err.contains("verify_script_missing: verify command `fast` runs `ci/new.sh`, which is not in the base commit"),
        "{err}"
    );
}

/// M11-R10 AC-3: `sh check` (no separator, no extension) missing at the base commit is
/// admitted — and protected: a fake implementer that creates `check` cannot pass its own gate.
/// (Deviation, recorded: a new untracked protected file is caught by the R35 protected-state
/// snapshot — `protected_config_changed: added check`, escalated, nothing committed — not by the
/// path-policy restore with a fix note the list describes.)
#[test]
fn an_extensionless_entry_script_is_admitted_and_protected() {
    let env = setup("codex-openai");
    with_verify_command(
        &env,
        r#"    - { name: check, program: sh, args: ["check"] }"#,
    );
    let (code, err) = create_as(&env, "R10-C");
    assert_eq!(code, 0, "{err}");
    let s = script(
        &env,
        "r10c.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), write("check", "exit 0\n"), implementation()]),
                turn(vec![implementation()]),
            ],
        }),
    );
    let (_, err) = toryo(&env, &["run", "R10-C", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "R10-C");
    assert_eq!(t.state, St::Escalated, "{err}");
    let why = t.machine.reason().unwrap_or_default().to_string();
    assert!(
        why.starts_with("protected_config_changed: added check"),
        "{why}"
    );
    let files = git(&wt(&env, "R10-C"), &["diff", "--name-only", "main..HEAD"]);
    assert!(
        !files.lines().any(|f| f == "check"),
        "never committed: {files}"
    );
    // without the protection the same run would commit `check` and pass its own gate
}

/// M11-R14: a toryo.yaml-only repository whose second verify command `smoke` runs a script that
/// is **build output** — `dist/check.ps1` (`.sh`), missing at the base commit, `dist/` ignored
/// — which the first command `build` creates in the verify checkout. The built script passes
/// when the checkout has `Divide-Numbers`.
fn build_output_repo() -> (Env, &'static str) {
    let env = setup("codex-openai");
    let (mk, body, entry, lines) = if cfg!(windows) {
        (
            "scripts/mk.ps1",
            "New-Item -ItemType Directory -Force -Path dist | Out-Null\nSet-Content -Path dist/check.ps1 -Value 'git grep -q Divide-Numbers -- src; exit $LASTEXITCODE'\nexit 0\n",
            "dist/check.ps1",
            "    - { name: build, program: pwsh, args: [\"-NoProfile\", \"-File\", \"scripts/mk.ps1\"] }\n    - { name: smoke, program: pwsh, args: [\"-NoProfile\", \"-File\", \"dist/check.ps1\"] }",
        )
    } else {
        (
            "scripts/mk.sh",
            "mkdir -p dist\nprintf 'git grep -q Divide-Numbers -- src\\n' > dist/check.sh\nexit 0\n",
            "dist/check.sh",
            "    - { name: build, program: sh, args: [\"scripts/mk.sh\"] }\n    - { name: smoke, program: sh, args: [\"dist/check.sh\"] }",
        )
    };
    commit_repo(
        &env,
        &[(mk, body), (".gitignore", "dist/\nbuild/\n")],
        "a build step; build output is ignored",
    );
    with_verify_command(&env, lines);
    (env, entry)
}

/// M11-R14 AC-1 (X9, applies to everyone): a gate whose entry script is ignored build output is
/// admitted, and the pipeline runs — the fake implementer writes source **and** its own
/// `dist/check.*` (ignored), with no `protected_config_changed`; the checkpoint does not hold
/// it; verify builds it in the clean checkout and runs it. The entry is frozen with the task
/// (`build_entries`), outside the R35 snapshot and still barred from a change list.
#[test]
fn a_build_output_entry_script_may_exist_in_the_worktree() {
    let (env, entry) = build_output_repo();
    let (code, err) = create_as(&env, "R14-1");
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "R14-1");
    let cfgv = toryo::engine::task::frozen_config(&t).unwrap();
    assert_eq!(
        cfgv.build_entries
            .iter()
            .map(|e| (e.gate.as_str(), e.path.as_str()))
            .collect::<Vec<_>>(),
        [("smoke", entry)]
    );
    let (_, prot, _) = toryo::engine::runner::task_policy(&t, &cfgv).unwrap();
    assert!(!prot.matches(entry), "not in the R35 snapshot");
    assert!(
        prot.violates(entry) && prot.build_entry(entry) == Some("smoke"),
        "barred from a change list"
    );
    let s = script(
        &env,
        "r14a.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![
                write("src/Calculator.psm1", DIVIDE),
                write(entry, "exit 0\n"),
                implementation(),
            ])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "R14-1", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "R14-1");
    assert_eq!(t.state, St::Completed, "{err}");
    assert!(!err.contains("protected_config_changed"), "{err}");
    assert_eq!(t.fix_cycle, 0);
    let vr: Value =
        serde_json::from_slice(&std::fs::read(t.artifacts().join("VERIFY_RESULT.json")).unwrap())
            .unwrap();
    assert_eq!(vr["pass"], true, "{vr}");
    let names: Vec<&str> = vr["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["build", "smoke"]);
    assert!(
        wt(&env, "R14-1").join(entry).exists(),
        "left in the worktree"
    );
    let files = git(&wt(&env, "R14-1"), &["diff", "--name-only", "main..HEAD"]);
    assert!(
        !files.lines().any(|f| f.starts_with("dist/")),
        "never committed: {files}"
    );
    // a repository without such an entry freezes no `build_entries` (its hash is unchanged)
    let env = setup("codex-openai");
    create(&env, "R14-0");
    let t = task(&env, "R14-0");
    let (_, text) = toryo::engine::task::frozen_config_text(&t).unwrap();
    assert!(!text.contains("build_entries"), "{text}");
}

/// M11-R14 AC-2: the implementer un-ignores `dist/`, so its copy of the entry script reaches
/// the change list → a path-policy violation: the file is removed, nothing is committed, and
/// the fix note names the verify command; the next turn goes on.
#[test]
fn an_unignored_build_output_entry_is_a_path_policy_violation() {
    let (env, entry) = build_output_repo();
    let (code, err) = create_as(&env, "R14-2");
    assert_eq!(code, 0, "{err}");
    let s = script(
        &env,
        "r14b.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![
                    // overwritten (the `write` helper appends): `dist/` is no longer ignored
                    json!({"kind": "write_file", "path": ".gitignore", "content": "build/\n", "append": false}),
                    write(entry, "exit 0\n"),
                    write("src/Calculator.psm1", DIVIDE),
                    implementation(),
                ]),
                turn(vec![implementation()]),
            ],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "R14-2", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "R14-2");
    assert_eq!(t.state, St::Completed, "{err}");
    assert!(
        err.contains("build output that a gate runs as its script reached the change list")
            && !err.contains("protected_config_changed"),
        "{err}"
    );
    assert_eq!(t.fix_cycle, 1, "one fix cycle: the violation");
    assert!(!wt(&env, "R14-2").join(entry).exists(), "removed");
    let files = git(&wt(&env, "R14-2"), &["diff", "--name-only", "main..HEAD"]);
    assert!(
        !files.lines().any(|f| f.starts_with("dist/")) && files.contains("src/Calculator.psm1"),
        "{files}"
    );
    let fix = calls(&env, "R14-2")
        .into_iter()
        .rev()
        .find(|c| {
            c["key"]
                .as_str()
                .unwrap_or_default()
                .starts_with("implementer")
        })
        .unwrap();
    let prompt = fix["prompt"].as_str().unwrap();
    assert!(
        prompt.contains("build output must not be committed")
            && prompt.contains(&format!(
                "`{entry}` (the script verify command `smoke` runs)"
            )),
        "{prompt}"
    );
}

/// M11-R14 AC-3 + AC-4 (X9): what is protected comes from the base commit. A first run leaves
/// an ignored `build/app.exe` — an argument of a verify command — in the worktree; the task's
/// protected set is the same before and after, `task config --reload` finds nothing changed,
/// and `toryo resume` goes on: no `protected_config_changed`.
#[test]
fn build_output_left_in_the_worktree_is_not_protected_on_resume() {
    let env = setup("codex-openai");
    let (smoke, body, line) = if cfg!(windows) {
        (
            "scripts/smoke.ps1",
            "if (Test-Path 'src/Fixed.txt') { exit 0 }\n'not fixed yet'\nexit 1\n",
            "    - { name: smoke, program: pwsh, args: [\"-NoProfile\", \"-File\", \"scripts/smoke.ps1\", \"-Exe\", \"build/app.exe\"] }",
        )
    } else {
        (
            "scripts/smoke.sh",
            "[ -f src/Fixed.txt ] && exit 0\necho 'not fixed yet'\nexit 1\n",
            "    - { name: smoke, program: sh, args: [\"scripts/smoke.sh\", \"build/app.exe\"] }",
        )
    };
    commit_repo(
        &env,
        &[(smoke, body), (".gitignore", "build/\n")],
        "a smoke script; build output is ignored",
    );
    with_verify_command(&env, line);
    let (code, err) = toryo(
        &env,
        &[
            "task",
            "create",
            "--file",
            "TASK.md",
            "--id",
            "R14-3",
            "--max-fix-cycles",
            "1",
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let protected = |env: &Env| {
        let t = task(env, "R14-3");
        let cfgv = toryo::engine::task::frozen_config(&t).unwrap();
        let (_, prot, _) = toryo::engine::runner::task_policy(&t, &cfgv).unwrap();
        (prot.exact_paths(), prot.matches("build/app.exe"))
    };
    let before = protected(&env);
    assert!(
        before.0.contains(&smoke.to_string()) && !before.1,
        "{before:?}"
    );
    // the first run: the implementer builds `build/app.exe`; verify fails twice → Escalated
    let s = script(
        &env,
        "r14c.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![
                    write("src/Calculator.psm1", DIVIDE),
                    write("build/app.exe", "MZ\n"),
                    implementation(),
                ]),
                turn(vec![implementation()]),
            ],
        }),
    );
    let (_, err) = toryo(&env, &["run", "R14-3", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "R14-3");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert!(
        t.machine
            .reason()
            .unwrap_or_default()
            .contains("not accepted after 1 fix cycle(s)"),
        "{:?}",
        t.machine.reason()
    );
    assert!(wt(&env, "R14-3").join("build").join("app.exe").exists());
    // AC-4: the same protected set, although the file exists now
    assert_eq!(protected(&env), before);
    // Do 3: a reload recomputes against the same base commit — nothing changed
    let (code, err) = toryo(&env, &["task", "config", "R14-3", "--reload"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains("the effective configuration is unchanged"),
        "{err}"
    );
    // AC-3: resume → the fix turn runs, verify passes
    let s = script(
        &env,
        "r14d.json",
        json!({
            "implementer": [turn(vec![write("src/Fixed.txt", "x\n"), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(
        &env,
        &[
            "resume",
            "R14-3",
            "--note",
            "add src/Fixed.txt",
            "--yes",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    let t = task(&env, "R14-3");
    assert_eq!(t.state, St::Completed, "{err}");
    assert!(!err.contains("protected_config_changed"), "{err}");
}

/// M11-R15 AC-2 (X10, applies to everyone): a `cd` in a command string is followed. The two
/// lines of the list are accepted with the script at the base commit — round 2 looked for
/// `x.sh` / `run.bat` in the repository root and refused them — and the script is protected
/// where the `cd` put it. A head that is missing there is refused by its resolved path; a `cd`
/// that cannot be followed refuses nothing.
#[test]
fn a_cd_in_a_command_string_is_followed_at_admission() {
    let env = setup("codex-openai");
    commit_repo(
        &env,
        &[("ci/x.sh", "exit 0\n"), ("sub/run.bat", "exit /b 0\n")],
        "scripts in sub-folders",
    );
    with_verify_command(
        &env,
        "    - { name: ci, program: bash, args: [\"-lc\", \"cd ci && ./x.sh\"] }\n    - { name: sub, program: cmd, args: [\"/c\", \"cd sub && run.bat\"] }",
    );
    let (code, err) = create_as(&env, "R15-1");
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "R15-1");
    let cfgv = toryo::engine::task::frozen_config(&t).unwrap();
    let (_, prot, _) = toryo::engine::runner::task_policy(&t, &cfgv).unwrap();
    for p in [
        "ci/x.sh",
        "sub/run.bat",
        "sub/run.bat.cmd",
        "sub/run.bat.exe",
    ] {
        assert!(prot.violates(p), "{p} is protected");
    }
    for p in ["x.sh", "run.bat", "ci/other.sh"] {
        assert!(!prot.violates(p), "{p} is not");
    }

    let env = setup("codex-openai");
    with_verify_command(
        &env,
        r#"    - { name: ci, program: bash, args: ["-lc", "cd ci && ./new.sh"] }"#,
    );
    let (code, err) = create_as(&env, "R15-2");
    assert_ne!(code, 0);
    assert!(
        err.contains(
            "verify_script_missing: verify command `ci` runs `ci/new.sh`, which is not in the base commit"
        ),
        "{err}"
    );

    // not followed (a variable): nothing is refused; an argument is never refused either
    let env = setup("codex-openai");
    with_verify_command(
        &env,
        "    - { name: var, program: bash, args: [\"-c\", \"cd $DIR && ./x.sh\"] }\n    - { name: out, program: bash, args: [\"-c\", \"tsc --outFile build/x.js\"] }\n    - { name: pester, program: pwsh, args: [\"-Command\", \"Invoke-Pester tests/New.Tests.ps1\"] }",
    );
    let (code, err) = create_as(&env, "R15-3");
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains("note: verify command `pester` passes `tests/New.Tests.ps1`, which is not in the base commit"),
        "the direct form's note: {err}"
    );
    assert!(!err.contains("build/x.js"), "an output file: {err}");
}

/// M11-R15 AC-2, end to end: the gate `cd`s into `ci/` and runs its script there. The
/// implementer edits that script → a path-policy violation: restored, nothing committed, a fix
/// note; then the gate runs the committed script and passes.
#[test]
fn a_script_behind_a_cd_is_protected_from_the_implementer() {
    let env = setup("codex-openai");
    let (file, line) = if cfg!(windows) {
        (
            "ci/x.ps1",
            r#"    - { name: ci, program: pwsh, args: ["-NoProfile", "-Command", "cd ci; ./x.ps1"] }"#,
        )
    } else {
        (
            "ci/x.sh",
            // sourced (`.`): the file needs no executable bit
            r#"    - { name: ci, program: sh, args: ["-c", "cd ci && . ./x.sh"] }"#,
        )
    };
    commit_repo(&env, &[(file, "exit 0\n")], "a gate script in ci/");
    with_verify_command(&env, line);
    let (code, err) = create_as(&env, "R15-E");
    assert_eq!(code, 0, "{err}");
    let s = script(
        &env,
        "r15e.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write(file, "# always green\n"), write("src/Calculator.psm1", DIVIDE), implementation()]),
                turn(vec![implementation()]),
            ],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "R15-E", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "R15-E");
    assert_eq!(t.state, St::Completed, "{err}");
    assert!(
        err.contains("protected or denied paths changed") && err.contains(file),
        "{err}"
    );
    assert_eq!(t.fix_cycle, 1);
    let files = git(&wt(&env, "R15-E"), &["diff", "--name-only", "main..HEAD"]);
    assert!(
        !files.lines().any(|f| f == file) && files.contains("src/Calculator.psm1"),
        "{files}"
    );
}

/// M11-R15 AC-3 (X10, X11): `cmd.exe` looks in the working directory before PATH, and a check
/// gate's working directory is the checkout root — so a bare head of a `cmd` string is
/// protected under every name `cmd` would try, whether or not a program of that name is on
/// PATH. A fake implementer that adds `npm.cmd` for `cmd /c "npm.cmd run build"`, or
/// `build.cmd` for `cmd /c build`, is stopped: `protected_config_changed`, nothing committed.
/// (The configuration is admitted: a bare head is never refused.)
#[test]
fn a_bare_cmd_name_cannot_be_planted_by_the_implementer() {
    for (id, line, planted) in [
        (
            "R15-N",
            r#"    - { name: build, program: cmd, args: ["/c", "npm.cmd run build"] }"#,
            "npm.cmd",
        ),
        (
            "R15-B",
            r#"    - { name: build, program: cmd, args: ["/c", "build"] }"#,
            "build.cmd",
        ),
    ] {
        let env = setup("codex-openai");
        with_verify_command(&env, line);
        let (code, err) = create_as(&env, id);
        assert_eq!(code, 0, "{id}: {err}");
        let s = script(
            &env,
            "r15c.json",
            json!({
                "planner": [turn(vec![plan()])],
                "implementer": [
                    turn(vec![write("src/Calculator.psm1", DIVIDE), write(planted, "@exit /b 0\n"), implementation()]),
                    turn(vec![implementation()]),
                ],
                "reviewer": [turn(vec![approved()])],
            }),
        );
        let (_, err) = toryo(&env, &["run", id, "--yes", "--fake-agents", &s], &[]);
        let t = task(&env, id);
        assert_eq!(t.state, St::Escalated, "{id}: {err}");
        let why = t.machine.reason().unwrap_or_default().to_string();
        assert!(
            why.starts_with(&format!("protected_config_changed: added {planted}")),
            "{id}: {why}"
        );
        let files = git(&wt(&env, id), &["diff", "--name-only", "main..HEAD"]);
        assert!(
            !files.lines().any(|f| f == planted),
            "{id}: never committed: {files}"
        );
    }
}

/// M11-R14 Do 3 (R13–R15 review): `task config --reload` recomputes the build-output entries
/// against the task's base commit. With nothing changed the digest is the same — it would
/// differ if the reload left them out; after the gate's script is renamed in the configuration
/// the frozen list follows.
#[test]
fn a_reload_recomputes_the_build_output_entries() {
    let (env, entry) = build_output_repo();
    let (code, err) = create_as(&env, "R14-R");
    assert_eq!(code, 0, "{err}");
    let s = script(
        &env,
        "r14r.json",
        json!({ "planner": [turn(vec![plan()])] }),
    );
    // to the plan gate: a waiting state, where a reload is allowed
    let (_, err) = toryo(&env, &["run", "R14-R", "--fake-agents", &s], &[]);
    assert_eq!(task(&env, "R14-R").state, St::WaitingPlanApproval, "{err}");
    let (code, err) = toryo(&env, &["task", "config", "R14-R", "--reload"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains("the effective configuration is unchanged"),
        "{err}"
    );
    let other = entry.replace("check", "other");
    let y = std::fs::read_to_string(&env.config).unwrap();
    std::fs::write(&env.config, y.replace(entry, &other)).unwrap();
    let (code, err) = toryo(&env, &["task", "config", "R14-R", "--reload"], &[]);
    assert_eq!(code, 0, "{err}");
    let cfgv = toryo::engine::task::frozen_config(&task(&env, "R14-R")).unwrap();
    assert_eq!(
        cfgv.build_entries
            .iter()
            .map(|e| (e.gate.as_str(), e.path.as_str()))
            .collect::<Vec<_>>(),
        [("smoke", other.as_str())]
    );
}

/// M11-R13 (R13–R15 review): a task that already has a verify checkout follows a changed
/// `verify.verify_root` after `task config --reload` — a new checkout is made under the new
/// root; the old folder is left alone.
#[test]
fn a_changed_verify_root_is_used_after_a_reload() {
    let env = setup("codex-openai");
    let (code, err) = toryo(
        &env,
        &[
            "task",
            "create",
            "--file",
            "TASK.md",
            "--id",
            "R13-V",
            "--max-fix-cycles",
            "1",
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    // two verify runs fail (no `Divide-Numbers` yet) → Escalated, with a checkout under `verify`
    let s = script(
        &env,
        "r13v.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![implementation()]), turn(vec![implementation()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "R13-V", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "R13-V");
    assert_eq!(t.state, St::Escalated, "{err}");
    let checkout = |env: &Env| -> std::path::PathBuf {
        let vr: Value = serde_json::from_slice(
            &std::fs::read(task(env, "R13-V").artifacts().join("VERIFY_RESULT.json")).unwrap(),
        )
        .unwrap();
        std::path::PathBuf::from(vr["verify_checkout"].as_str().unwrap())
    };
    let old = checkout(&env);
    assert!(
        old.starts_with(env.root.join("verify")),
        "{}",
        old.display()
    );
    // the owner moves the verify root
    let y = std::fs::read_to_string(&env.config).unwrap();
    assert!(y.contains("/verify\""), "{y}");
    std::fs::write(&env.config, y.replace("/verify\"", "/verify2\"")).unwrap();
    let (code, err) = toryo(&env, &["task", "config", "R13-V", "--reload"], &[]);
    assert_eq!(code, 0, "{err}");
    let s = script(
        &env,
        "r13v2.json",
        json!({
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(
        &env,
        &[
            "resume",
            "R13-V",
            "--note",
            "add Divide-Numbers",
            "--yes",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    assert_eq!(task(&env, "R13-V").state, St::Completed, "{err}");
    let new = checkout(&env);
    assert!(
        new.starts_with(env.root.join("verify2")),
        "{}",
        new.display()
    );
    assert!(old.exists(), "the old checkout is left alone");
}

/// M11-R15 (R13–R15 review): command lines the first version of the reader refused although
/// their script is in the base commit — a head without an extension in a `cmd` string
/// (`scripts\build` runs `scripts\build.cmd`), a `Push-Location`, a quoted string that is an
/// expression, a `cd` that is not the head of its command. All are admitted; what the gate
/// clearly runs is protected.
#[test]
fn lines_the_reader_must_not_refuse() {
    let env = setup("codex-openai");
    commit_repo(
        &env,
        &[
            ("scripts/build.cmd", "@exit /b 0\n"),
            ("ci/x.ps1", "exit 0\n"),
            ("run-tests.sh", "exit 0\n"),
        ],
        "gate scripts",
    );
    with_verify_command(
        &env,
        concat!(
            "    - { name: build, program: cmd, args: ['/c', 'scripts\\build'] }\n",
            "    - { name: ci, program: pwsh, args: ['-Command', 'Push-Location ci; ./x.ps1; Pop-Location'] }\n",
            "    - { name: pester, program: pwsh, args: ['-Command', \"Invoke-Pester -Path @('tests/unit','tests/int')\"] }\n",
            "    - { name: maybe, program: bash, args: ['-c', 'if [ -d frontend ]; then cd frontend; fi; ./run-tests.sh'] }\n",
            "    - { name: deno, program: deno, args: ['run', '-A', 'npm:@biomejs/biome', 'check'] }"
        ),
    );
    let (code, err) = create_as(&env, "R15-V");
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "R15-V");
    let cfgv = toryo::engine::task::frozen_config(&t).unwrap();
    let (_, prot, _) = toryo::engine::runner::task_policy(&t, &cfgv).unwrap();
    for p in ["scripts/build.cmd", "scripts/build.bat", "ci/x.ps1"] {
        assert!(prot.violates(p), "{p} is protected");
    }
    for p in ["x.ps1", "tests/unit", "frontend/run-tests.sh"] {
        assert!(!prot.violates(p), "{p} is not");
    }
}
