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
