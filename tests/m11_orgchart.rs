//! M11 org chart mode end to end, on FakeAgent: `.toryo/` in the fixture repository, the
//! loader (B2), `toryo config show`, a task created, frozen and run to the final gate.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use toryo::engine::PipelineState as St;

fn show_json(env: &Env) -> Value {
    let o = command(env, &["config", "show", "--json"], &[])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

/// M11-B2 AC-1 / AC-4: the org chart loads, `config show` names the mode, the files and the
/// origins; a task created from it freezes the `team` section, reloads to the same digest,
/// and a fake run reaches the final gate with the check gate as its verify command.
#[test]
fn a_fake_run_in_org_chart_mode_reaches_the_final_gate() {
    let env = setup_orgchart("codex-openai", "", &[]);
    let v = show_json(&env);
    assert_eq!(v["schema"], 1);
    assert_eq!(v["mode"], "org chart");
    assert_eq!(v["team"], "calc-team");
    let files: Vec<&str> = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["what"].as_str().unwrap())
        .collect();
    assert_eq!(
        files,
        [
            "toryo.yaml",
            "machine config",
            "org chart",
            "role planner",
            "role implementer",
            "role reviewer"
        ]
    );
    assert!(v["files"][2]["sha256"].as_str().unwrap().len() == 64);
    let seat = |s: &str| {
        v["seats"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["seat"] == s)
            .unwrap()
            .clone()
    };
    assert_eq!(seat("implementer")["fallback"], json!([]));
    assert_eq!(seat("implementer")["source"], "repo");
    assert_eq!(v["profiles"][0]["source"], "machine");
    assert_eq!(v["gates"][0]["name"], "divide-present");
    assert_eq!(v["gates"][0]["source"], "repo");
    assert_eq!(v["before_handoff"], json!(["divide-present"]));
    assert_eq!(v["verify"]["min_boundary"]["source"], "machine");
    // the text form says the same
    let o = command(&env, &["config", "show"], &[]).output().unwrap();
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(
        text.starts_with("configuration: org chart (team calc-team)"),
        "{text}"
    );
    assert!(text.contains("implementer  codex-openai — repo"), "{text}");
    assert!(text.contains("before_handoff: divide-present"), "{text}");

    create(&env, "OC-1");
    let t = task(&env, "OC-1");
    // an omitted implementer fallback is none: a chain of one (not the planner's profile)
    assert_eq!(t.implementers, ["codex-openai"]);
    assert_eq!(t.config_sha256, v["config_sha256"].as_str().unwrap());
    let frozen: Value =
        serde_json::from_slice(&std::fs::read(t.artifacts().join("config.json")).unwrap()).unwrap();
    assert_eq!(frozen["team"]["name"], "calc-team");
    assert_eq!(frozen["repo"]["commands"]["verify"][0]["timeout_sec"], 300);
    let s = script(
        &env,
        "oc.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "OC-1", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "OC-1").state, St::WaitingPlanApproval, "{err}");
    let (code, err) = toryo(
        &env,
        &[
            "approve",
            "OC-1",
            "--gate",
            "plan",
            "--continue",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "OC-1").state, St::WaitingFinalApproval, "{err}");
    let vr: Value =
        serde_json::from_slice(&std::fs::read(t.artifacts().join("VERIFY_RESULT.json")).unwrap())
            .unwrap();
    assert_eq!(vr["commands"][0]["name"], "divide-present");
    assert_eq!(vr["pass"], true);
    // the same files load to the same configuration: a reload changes nothing
    let (code, err) = toryo(&env, &["task", "config", "OC-1", "--reload"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains("the effective configuration is unchanged"),
        "{err}"
    );
}

/// The fixture in org chart mode with a culture file and instructions for every seat.
fn with_instructions() -> Env {
    let org = ORGCHART.replace("IMPLEMENTER", "codex-openai").replace(
        "team: calc-team\n",
        "team: calc-team\nculture: ./CULTURE.md\n",
    );
    let mut files: Vec<(String, String)> = vec![
        (".toryo/toryo-orgchart.yaml".into(), org),
        (
            "CULTURE.md".into(),
            "CULTURE-NORMS: evidence first\n".into(),
        ),
    ];
    for (seat, extra) in [
        ("planner", ""),
        ("implementer", "before_handoff: [divide-present]\n"),
        ("reviewer", ""),
    ] {
        files.push((
            format!(".toryo/roles/{seat}/toryo-role.yaml"),
            format!("toryo: 1\npurpose: the {seat}\ninstructions: ./instructions.md\n{extra}"),
        ));
        files.push((
            format!(".toryo/roles/{seat}/instructions.md"),
            format!("TEAM-{seat}-RULES\n"),
        ));
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    setup_orgchart("codex-openai", "", &refs)
}

/// M11-C1 AC-1: `toryo context show` for each role lists the embedded template, then the
/// role's `instructions.md`, then the task pack; the embedded text is there too.
#[test]
fn context_show_layers_the_team_instructions_after_the_embedded_template() {
    let env = with_instructions();
    create(&env, "OC-2");
    for (stage, role) in [
        ("plan", "planner"),
        ("implement", "implementer"),
        ("review", "reviewer"),
    ] {
        let o = command(&env, &["context", "show", "OC-2", "--stage", stage], &[])
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let (text, err) = (
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr),
        );
        let at = |s: &str, x: &str| s.find(x).unwrap_or_else(|| panic!("{stage}: {x}\n{s}"));
        assert!(at(&err, "culture (CULTURE.md)") < at(&err, &format!("embedded {role}.md")));
        assert!(
            at(&err, &format!("embedded {role}.md"))
                < at(&err, &format!(".toryo/roles/{role}/instructions.md"))
        );
        assert!(at(&err, &format!(".toryo/roles/{role}/instructions.md")) < at(&err, "TASK.md"));
        assert!(at(&text, &format!("# Role: {role}")) < at(&text, "# Role instructions (team)"));
        assert!(at(&text, &format!("TEAM-{role}-RULES")) < at(&text, "# Task pack"));
        assert!(text.contains("CULTURE-NORMS"));
    }
}

/// M11-B2 AC-2: without `.toryo/toryo-orgchart.yaml`, `config show` says `toryo.yaml only`
/// and lists no team.
#[test]
fn without_an_org_chart_the_mode_is_toryo_yaml_only() {
    let env = setup("codex-openai");
    let v = show_json(&env);
    assert_eq!(v["mode"], "toryo.yaml only");
    assert_eq!(v["team"], Value::Null);
    assert_eq!(v["gates"][0]["name"], "divide-present");
    assert_eq!(v["files"].as_array().unwrap().len(), 2);
}

/// M11-R8: in org chart mode `task create --repo X` is refused unless X is `project.root` —
/// the org chart was validated from there, and the instructions, culture and skills are read
/// from the task's repository. `--repo` naming project.root itself is fine.
#[test]
fn task_create_repo_must_be_the_project_root_in_org_chart_mode() {
    let env = setup_orgchart("codex-openai", "", &[]);
    let other = env.root.join("other");
    std::fs::create_dir_all(&other).unwrap();
    let (code, err) = toryo(
        &env,
        &[
            "task",
            "create",
            "--file",
            "TASK.md",
            "--id",
            "RP-1",
            "--repo",
            other.to_str().unwrap(),
        ],
        &[],
    );
    assert_ne!(code, 0);
    assert!(
        err.contains("in org chart mode the task's repository must be project.root")
            && err.contains("change project.root in")
            && err.contains("or leave --repo out"),
        "{err}"
    );
    assert!(!env.data.join("runs").join("RP-1").exists());
    let (code, err) = toryo(
        &env,
        &[
            "task",
            "create",
            "--file",
            "TASK.md",
            "--id",
            "RP-2",
            "--repo",
            env.repo.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
}
