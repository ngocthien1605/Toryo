//! M12-18: the D365FO template loads as shipped, and a whole run of it — with the two print-only
//! example scripts and fake agents — reaches the final gate.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use std::path::{Path, PathBuf};

use serde_json::json;
use toryo::config::Effective;
use toryo::engine::PipelineState as St;

fn template() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("templates")
        .join("d365fo")
}

fn fwd(p: &Path) -> String {
    p.display()
        .to_string()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

/// Every file under `dir`, as (path relative to `dir` with `/`, text).
fn tree(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = fwd(p.strip_prefix(dir).unwrap());
                out.push((rel, std::fs::read_to_string(&p).unwrap()));
            }
        }
    }
    out.sort();
    out
}

fn has_pwsh() -> bool {
    std::process::Command::new("pwsh")
        .args(["-NoProfile", "-Command", "exit 0"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// AC-1: the template — the org chart, the role files, the skills, the context, CULTURE.md and
/// `toryo.orgchart.yaml.example` as `toryo.yaml` — loads together with the machine example, the
/// `TODO(owner)` paths filled with test values: no error, and the seats' profiles exist. The
/// commented `never:` block of the implementer role loads too once uncommented (every pattern
/// is accepted by the fence's own syntax rules).
#[test]
fn m12_18_the_d365fo_template_loads_as_shipped() {
    let t = template();
    let d = tempfile::tempdir().unwrap();
    let tmp = fwd(d.path());
    let repo = d.path().join("repo");
    for (rel, text) in tree(&t.join(".toryo")) {
        let p = repo.join(".toryo").join(&rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    std::fs::copy(t.join("CULTURE.md"), repo.join("CULTURE.md")).unwrap();
    // the TODO(owner) paths of toryo.yaml
    let yaml = std::fs::read_to_string(t.join("toryo.orgchart.yaml.example"))
        .unwrap()
        .replace("D:\\D365-Git", &format!("{tmp}/repo"))
        .replace("D:\\D365-worktrees", &format!("{tmp}/wts"));
    let y = d.path().join("toryo.yaml");
    std::fs::write(&y, yaml).unwrap();
    // … and of the machine example: the user, the gate script (the example copied), the root
    let gates = d.path().join("gates");
    std::fs::create_dir_all(&gates).unwrap();
    std::fs::copy(
        t.join("machine").join("dbsync.ps1.example"),
        gates.join("dbsync.ps1"),
    )
    .unwrap();
    let machine = std::fs::read_to_string(t.join("toryo.local.yaml.example"))
        .unwrap()
        .replace("<you>", "tester")
        .replace(
            "C:\\toryo-gates\\dbsync.ps1",
            &format!("{tmp}/gates/dbsync.ps1"),
        )
        .replace("D:\\toryo-verify", &format!("{tmp}/verify"));
    let m = d.path().join("toryo.local.yaml");
    std::fs::write(&m, machine).unwrap();
    let cfg = Effective::load(&y, Some(&m)).unwrap_or_else(|e| panic!("{e:#}"));
    let team = cfg.team.as_ref().expect("org chart mode");
    assert_eq!(team.name, "d365fo-dev");
    for (seat, profile) in [
        ("planner", "claude-max"),
        ("implementer", "codex-openai"),
        ("reviewer", "claude-max"),
    ] {
        assert_eq!(cfg.repo.roles[seat].profile, profile, "{seat}");
        assert!(cfg.repo.profiles.contains_key(profile), "{profile}");
    }
    let dbsync = team.gates["dbsync"].machine.as_ref().expect("dbsync");
    assert_eq!(dbsync.confirm.as_str(), "first");
    assert_eq!(
        toryo::engine::verify::verify_root(&cfg),
        PathBuf::from(format!("{tmp}/verify"))
    );
    // no key that does nothing is set (M12-08)
    assert!(cfg.keys_without_effect().is_empty());
    // the commented `never:` block of the implementer role, uncommented
    let role = repo.join(".toryo/roles/implementer/toryo-role.yaml");
    let text = std::fs::read_to_string(&role)
        .unwrap()
        .replace("\r\n", "\n");
    let lines: Vec<&str> = text.lines().collect();
    let at = lines
        .iter()
        .position(|l| *l == "# never:")
        .expect("the commented never block");
    let mut block = vec!["never:".to_string()];
    block.extend(
        lines[at + 1..]
            .iter()
            .take_while(|l| l.starts_with("#   - "))
            .map(|l| l[2..].to_string()),
    );
    assert_eq!(block.len(), 13, "{block:?}");
    let mut out: Vec<String> = Vec::new();
    for l in &lines {
        if l.starts_with("never:") {
            out.extend(block.iter().cloned());
        } else {
            out.push((*l).to_string());
        }
    }
    std::fs::write(&role, out.join("\n") + "\n").unwrap();
    let cfg = Effective::load(&y, Some(&m)).unwrap_or_else(|e| panic!("{e:#}"));
    let never = &cfg
        .team
        .as_ref()
        .unwrap()
        .role("implementer")
        .unwrap()
        .never;
    assert!(
        never.iter().any(|p| p == "**/*.dll") && never.iter().any(|p| p == "**/bin/**"),
        "{never:?}"
    );
}

/// AC-2: with the two example scripts in place, a `--fake-agents` run of the template reaches
/// the final gate. The machine example's `confirm: first` asks before the deploy; `resume` is
/// the yes. (Script gates run through `pwsh`; where the runner has none the test says so.)
#[test]
fn m12_18_the_template_reaches_the_final_gate_with_the_example_scripts() {
    if !has_pwsh() {
        eprintln!("skipped: no pwsh on this runner — the example scripts are PowerShell");
        return;
    }
    let t = template();
    let mut files: Vec<(String, String)> = tree(&t.join(".toryo"))
        .into_iter()
        .map(|(rel, text)| (format!(".toryo/{rel}"), text))
        .collect();
    files.push((
        "CULTURE.md".into(),
        std::fs::read_to_string(t.join("CULTURE.md")).unwrap(),
    ));
    files.push((
        "scripts/build-model.ps1".into(),
        std::fs::read_to_string(t.join("scripts").join("build-model.ps1.example")).unwrap(),
    ));
    files.push((
        "TASK.md".into(),
        std::fs::read_to_string(t.join("TASK.example.md")).unwrap(),
    ));
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let env = setup_orgchart("codex-openai", "", &refs);
    // the machine's `dbsync`: the example script, run by pwsh, with the example's `confirm`
    let gate = env.root.join("gates").join("dbsync.ps1");
    std::fs::create_dir_all(gate.parent().unwrap()).unwrap();
    std::fs::copy(t.join("machine").join("dbsync.ps1.example"), &gate).unwrap();
    let m = std::fs::read_to_string(&env.machine).unwrap();
    std::fs::write(
        &env.machine,
        format!(
            "{m}environment_gates:\n  dbsync:\n    program: pwsh\n    args: [-NoProfile, -NonInteractive, -File, '{}']\n    lock: d365-env\n    confirm: first\n",
            fwd(&gate)
        ),
    )
    .unwrap();
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "SJP-1234"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let file = "Metadata/ContosoCore/ContosoCore/AxClass/SJPCreditHold.xml";
    let acs: Vec<serde_json::Value> = (1..=5)
        .map(|n| json!({"id": format!("AC-{n}"), "met": true, "evidence": "e"}))
        .collect();
    let s = script(
        &env,
        "tpl.json",
        json!({
            "planner": [turn(vec![json!({"kind":"complete","structured":{
                "understanding":"u","files_likely_involved":[file],"steps":["add"],
                "validation":["build"],"plan_markdown":"md"}})])],
            "implementer": [turn(vec![write(file, "<AxClass/>\n"), implementation()])],
            "reviewer": [turn(vec![json!({"kind":"complete","structured":{
                "decision":"approved","summary":"s","acceptance_criteria":acs,"issues":[]}})])],
        }),
    );
    let (_, err) = toryo(
        &env,
        &["run", "SJP-1234", "--yes", "--fake-agents", &s],
        &[],
    );
    let t1 = task(&env, "SJP-1234");
    // `confirm: first`: stopped before the deploy, the build gate (the example script) has passed
    assert_eq!(t1.state, St::Escalated, "{err}");
    assert_eq!(
        t1.machine.reason_class(),
        Some("environment_gate_confirm"),
        "{err}"
    );
    // the yes: a plain `resume` (`--yes` never confirms a deploy); the run goes on to the final gate
    let (code, err) = toryo(&env, &["resume", "SJP-1234", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        task(&env, "SJP-1234").state,
        St::WaitingFinalApproval,
        "{err}"
    );
    let vr: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            task(&env, "SJP-1234")
                .artifacts()
                .join("VERIFY_RESULT.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let cmds = vr["commands"].as_array().unwrap();
    let tail = |n: &str| {
        cmds.iter()
            .find(|c| c["name"] == n)
            .unwrap_or_else(|| panic!("{n} in {vr}"))["output_tail"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert!(tail("build").contains("print-only") && tail("build").contains("TORYO_SHA="));
    assert!(
        tail("dbsync").contains("print-only") && tail("dbsync").contains("TORYO_VERIFY_CHECKOUT=")
    );
}
