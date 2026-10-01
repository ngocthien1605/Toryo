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
