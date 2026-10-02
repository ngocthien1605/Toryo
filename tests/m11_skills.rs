//! M11 group D end to end, on FakeAgent: the implementer's skill menu, the plan's `skills`
//! field and its checks (D2), skills delivered through the context loader (D3).
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use toryo::engine::PipelineState as St;

const BATCH: &str = "---\nname: xpp-batch\ndescription: Write a SysOperation batch job.\n---\n# Batch jobs\nBATCH-SKILL-BODY\n";
const AXCLASS: &str = "---\nname: xpp-axclass\ndescription: Create or change an X++ class.\n---\n# AxClass\nAXCLASS-SKILL-BODY\n";
const CHECKLIST: &str = "# Review checklist\nREVIEW-SKILL-BODY\n";

/// The fixture in org chart mode: the implementer's menu `[xpp-axclass, xpp-batch]` (both in
/// `.toryo/skills/`), the reviewer's skill `review-checklist`; `extra`: toryo.yaml additions.
fn skills_env(extra: &str, files: &[(&str, &str)]) -> Env {
    let mut all: Vec<(&str, &str)> = vec![
        (
            ".toryo/roles/implementer/toryo-role.yaml",
            "toryo: 1\npurpose: Change the code.\nskills: [xpp-axclass, xpp-batch]\nbefore_handoff: [divide-present]\n",
        ),
        (
            ".toryo/roles/reviewer/toryo-role.yaml",
            "toryo: 1\npurpose: Review the checkpoint.\nskills: [review-checklist]\n",
        ),
        (".toryo/skills/xpp-batch/SKILL.md", BATCH),
        (".toryo/skills/xpp-axclass/SKILL.md", AXCLASS),
        (".toryo/skills/review-checklist/SKILL.md", CHECKLIST),
    ];
    all.extend_from_slice(files);
    setup_orgchart("codex-openai", extra, &all)
}

fn plan_with(skills: Value) -> Value {
    json!({"kind":"complete","structured":{"understanding":"u","files_likely_involved":["src/Calculator.psm1"],"steps":["add"],"validation":["tests"],"plan_markdown":"md","skills":skills}})
}

fn calls_of(env: &Env, id: &str, key: &str) -> Vec<Value> {
    calls(env, id)
        .into_iter()
        .filter(|c| c["key"].as_str().is_some_and(|k| k.starts_with(key)))
        .collect()
}

/// M11-D2 AC-1: the planner sees the menu; a plan with `skills: ["xpp-batch"]` opens the plan
/// gate and PLAN.md names the choice.
#[test]
fn a_plan_chooses_skills_from_the_menu() {
    let env = skills_env("", &[]);
    create(&env, "SK-1");
    let s = script(
        &env,
        "sk1.json",
        json!({"planner": [turn(vec![plan_with(json!(["xpp-batch"]))])]}),
    );
    let (code, err) = toryo(&env, &["run", "SK-1", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "SK-1");
    assert_eq!(t.state, St::WaitingPlanApproval, "{err}");
    let ctx = calls_of(&env, "SK-1", "planner")[0]["context"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        ctx.contains("# Skills available to the implementer"),
        "{ctx}"
    );
    assert!(
        ctx.contains("- xpp-axclass — Create or change an X++ class."),
        "{ctx}"
    );
    assert!(
        ctx.contains("- xpp-batch — Write a SysOperation batch job."),
        "{ctx}"
    );
    let md = std::fs::read_to_string(t.artifacts().join("PLAN.md")).unwrap();
    assert!(md.contains("Skills for the implementer: xpp-batch"), "{md}");
}

/// M11-D2 AC-2: `skills: ["xpp-bach"]` sends the plan back with the menu and a hint; after
/// `MAX_PLAN_AUTO_REJECTIONS` re-plans the task escalates as for protected paths.
#[test]
fn a_skill_off_the_menu_is_replanned_then_escalates() {
    let env = skills_env("", &[]);
    create(&env, "SK-2");
    let bad = || turn(vec![plan_with(json!(["xpp-bach"]))]);
    let s = script(&env, "sk2.json", json!({"planner": [bad(), bad(), bad()]}));
    let (_, err) = toryo(&env, &["run", "SK-2", "--fake-agents", &s], &[]);
    let t = task(&env, "SK-2");
    assert_eq!(t.state, St::Escalated, "{err}");
    let p = calls_of(&env, "SK-2", "planner");
    assert_eq!(p.len(), 3);
    let fb = p[1]["prompt"].as_str().unwrap();
    assert!(
        fb.contains("not on the implementer's menu: `xpp-bach` (did you mean `xpp-batch`?)"),
        "{fb}"
    );
    assert!(
        fb.contains("- xpp-batch — Write a SysOperation batch job."),
        "{fb}"
    );
    assert!(
        t.machine
            .reason()
            .unwrap()
            .contains("plan_skills: the plan lists skills that are not on the implementer's menu"),
        "{:?}",
        t.machine.reason()
    );
    assert!(t
        .machine
        .reason()
        .unwrap()
        .contains("after 2 automatic re-plans"));
}

/// M11-D2 AC-3: chosen skills over the implementer's budget → re-plan "choose fewer".
#[test]
fn skills_over_the_budget_are_replanned() {
    let big = format!(
        "---\ndescription: A long manual.\n---\n{}",
        "x".repeat(6 * 1024)
    );
    let env = skills_env(
        "context: { budget_kb: { implementer: 4 } }\n",
        &[(".toryo/skills/xpp-batch/SKILL.md", &big)],
    );
    create(&env, "SK-3");
    let s = script(
        &env,
        "sk3.json",
        json!({"planner": [turn(vec![plan_with(json!(["xpp-batch"]))]), turn(vec![plan_with(json!([]))])]}),
    );
    let (code, err) = toryo(&env, &["run", "SK-3", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "SK-3").state, St::WaitingPlanApproval, "{err}");
    let p = calls_of(&env, "SK-3", "planner");
    assert!(
        p[1]["prompt"]
            .as_str()
            .unwrap()
            .contains("Choose fewer skills"),
        "{}",
        p[1]["prompt"]
    );
}

/// M11-D2 AC-4: a plan without `skills` is valid and opens the plan gate.
#[test]
fn a_plan_without_skills_is_valid() {
    let env = skills_env("", &[]);
    create(&env, "SK-4");
    let s = script(&env, "sk4.json", json!({"planner": [turn(vec![plan()])]}));
    let (code, err) = toryo(&env, &["run", "SK-4", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "SK-4");
    assert_eq!(t.state, St::WaitingPlanApproval, "{err}");
    let md = std::fs::read_to_string(t.artifacts().join("PLAN.md")).unwrap();
    assert!(!md.contains("Skills for the implementer"), "{md}");
}

/// `toryo context show <id> --stage <stage>`: (stderr summary, stdout text).
fn context_show(env: &Env, id: &str, stage: &str) -> (String, String) {
    let o = command(env, &["context", "show", id, "--stage", stage], &[])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    (
        String::from_utf8_lossy(&o.stderr).to_string(),
        String::from_utf8_lossy(&o.stdout).to_string(),
    )
}

/// The task at the approved plan gate with `skills`.
fn approved_plan(env: &Env, id: &str, skills: Value) {
    create(env, id);
    let s = script(
        env,
        &format!("{id}.json"),
        json!({"planner": [turn(vec![plan_with(skills)])]}),
    );
    let (code, err) = toryo(env, &["run", id, "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = toryo(env, &["approve", id, "--gate", "plan"], &[]);
    assert_eq!(code, 0, "{err}");
}

/// M11-D3 AC-1: `context show` for the implementer lists exactly the planned skills (with
/// origin and SHA-256), for the reviewer its own list, in legacy mode none.
#[test]
fn context_show_lists_the_skills_of_each_role() {
    let env = skills_env("", &[]);
    approved_plan(&env, "SK-5", json!(["xpp-batch"]));
    let (err, text) = context_show(&env, "SK-5", "implement");
    assert!(err.contains("skill xpp-batch (repo)"), "{err}");
    assert!(!err.contains("skill xpp-axclass"), "{err}");
    let sha = toryo::engine::task::sha256_hex(BATCH.as_bytes());
    assert!(err.contains(&format!("sha256 {}", &sha[..12])), "{err}");
    assert!(text.contains("# Skill: xpp-batch (repo)") && text.contains("BATCH-SKILL-BODY"));
    assert!(!text.contains("AXCLASS-SKILL-BODY") && !text.contains("REVIEW-SKILL-BODY"));
    let (err, text) = context_show(&env, "SK-5", "review");
    assert!(err.contains("skill review-checklist (repo)"), "{err}");
    assert!(!err.contains("skill xpp-batch"), "{err}");
    assert!(text.contains("REVIEW-SKILL-BODY"));
    // the planner gets no skill of its own here, only the menu
    let (err, _) = context_show(&env, "SK-5", "plan");
    assert!(
        err.contains("implementer skill menu") && !err.contains("] skill "),
        "{err}"
    );
    // toryo.yaml-only mode: no skill parts at all
    let legacy = setup("codex-openai");
    create(&legacy, "SK-6");
    for stage in ["plan", "implement", "review"] {
        let (err, _) = context_show(&legacy, "SK-6", stage);
        assert!(!err.contains("skill"), "{stage}: {err}");
    }
}

/// M11-D3 AC-2: changing a SKILL.md between two attempts changes the next attempt's context
/// hash (the text is read per attempt, never frozen).
#[test]
fn a_changed_skill_changes_the_context_hash() {
    let env = skills_env("", &[]);
    approved_plan(&env, "SK-7", json!(["xpp-batch"]));
    let sha = |env: &Env| {
        let (err, _) = context_show(env, "SK-7", "implement");
        let head = err.lines().next().unwrap().to_string();
        head.split("sha256 ").nth(1).unwrap().trim().to_string()
    };
    let a = sha(&env);
    assert_eq!(sha(&env), a, "deterministic");
    std::fs::write(
        env.repo.join(".toryo/skills/xpp-batch/SKILL.md"),
        BATCH.replace("BATCH-SKILL-BODY", "BATCH-SKILL-BODY v2"),
    )
    .unwrap();
    assert_ne!(sha(&env), a, "the changed manual is a changed context");
}

/// M11-D3 AC-3: the Claude implementer gets the skill text in its context file (the system
/// prompt), never in its user prompt.
#[test]
fn the_claude_implementer_gets_skills_in_its_context_file() {
    let org = ORGCHART.replace("IMPLEMENTER", "claude-max");
    let env = skills_env("", &[(".toryo/toryo-orgchart.yaml", &org)]);
    approved_plan(&env, "SK-8", json!(["xpp-batch"]));
    let s = script(
        &env,
        "sk8.json",
        json!({
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "SK-8", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let imp = calls_of(&env, "SK-8", "implementer");
    let (ctx, prompt) = (
        imp[0]["context"].as_str().unwrap(),
        imp[0]["prompt"].as_str().unwrap(),
    );
    assert!(ctx.contains("# Skill: xpp-batch (repo)") && ctx.contains("BATCH-SKILL-BODY"));
    assert!(!prompt.contains("BATCH-SKILL-BODY"), "{prompt}");
    // the reviewer's own skill reached the reviewer
    let rev = calls_of(&env, "SK-8", "reviewer");
    assert!(rev[0]["context"]
        .as_str()
        .unwrap()
        .contains("REVIEW-SKILL-BODY"));
}

/// Group D review: a name is a menu entry exactly as written — `" xpp-batch"` is off the menu
/// (before, the check trimmed it and the delivery dropped it).
#[test]
fn a_skill_name_is_matched_exactly() {
    let env = skills_env("", &[]);
    create(&env, "SK-9");
    let s = script(
        &env,
        "sk9.json",
        json!({"planner": [turn(vec![plan_with(json!([" xpp-batch"]))]), turn(vec![plan_with(json!(["xpp-batch"]))])]}),
    );
    let (code, err) = toryo(&env, &["run", "SK-9", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "SK-9").state, St::WaitingPlanApproval, "{err}");
    let p = calls_of(&env, "SK-9", "planner");
    assert!(
        p[1]["prompt"]
            .as_str()
            .unwrap()
            .contains("not on the implementer's menu: ` xpp-batch` (did you mean `xpp-batch`?)"),
        "{}",
        p[1]["prompt"]
    );
}

/// Group D review: without an org chart a plan's `skills` means nothing — PLAN.md and plan.json
/// are as in M10.
#[test]
fn legacy_plans_ignore_skills() {
    let env = setup("codex-openai");
    create(&env, "SK-10");
    let s = script(
        &env,
        "sk10.json",
        json!({"planner": [turn(vec![plan_with(json!(["some-native-skill"]))])]}),
    );
    let (code, err) = toryo(&env, &["run", "SK-10", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "SK-10");
    assert_eq!(t.state, St::WaitingPlanApproval, "{err}");
    let md = std::fs::read_to_string(t.artifacts().join("PLAN.md")).unwrap();
    assert!(!md.contains("Skills for the implementer"), "{md}");
    let plan: Value =
        serde_json::from_slice(&std::fs::read(t.artifacts().join("plan.json")).unwrap()).unwrap();
    assert!(plan.get("skills").is_none(), "{plan}");
}

/// Group D review: a skill found only in the machine's `skills_dir` is delivered with origin
/// `machine`; one that is gone at attempt time is reported as skipped, and the planner's menu
/// says so.
#[test]
fn machine_skills_are_delivered_and_missing_ones_skipped() {
    let env = skills_env("", &[]);
    let shared = env.root.join("machine-skills").join("xpp-batch");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(
        shared.join("SKILL.md"),
        BATCH.replace("BATCH-SKILL-BODY", "MACHINE-BATCH-BODY"),
    )
    .unwrap();
    std::fs::remove_file(env.repo.join(".toryo/skills/xpp-batch/SKILL.md")).unwrap();
    let m = std::fs::read_to_string(&env.machine).unwrap();
    std::fs::write(
        &env.machine,
        format!(
            "{m}skills_dir: '{}'\n",
            env.root
                .join("machine-skills")
                .display()
                .to_string()
                .replace('\\', "/")
        ),
    )
    .unwrap();
    approved_plan(&env, "SK-11", json!(["xpp-batch"]));
    let (err, text) = context_show(&env, "SK-11", "implement");
    assert!(err.contains("skill xpp-batch (machine)"), "{err}");
    assert!(text.contains("MACHINE-BATCH-BODY"));
    std::fs::remove_file(shared.join("SKILL.md")).unwrap();
    let (err, text) = context_show(&env, "SK-11", "implement");
    assert!(
        err.contains("(skipped, not found) 4: skill 'xpp-batch' not found"),
        "{err}"
    );
    assert!(!text.contains("MACHINE-BATCH-BODY"));
    // M11-R5: the menu lists only the skills that resolve now (it said "(not found on this
    // machine)" before)
    let (_, plan_text) = context_show(&env, "SK-11", "plan");
    assert!(
        !plan_text.contains("- xpp-batch")
            && plan_text.contains("- xpp-axclass — Create or change an X++ class."),
        "{plan_text}"
    );
}

/// M11-R5 AC-1: an approved skill whose folder is gone before the implement turn → the task
/// escalates with `skill_unavailable` (both lookup paths named) and no implementer attempt
/// starts; once the folder is back, `resume --note` runs the turn with the skill.
#[test]
fn a_skill_that_is_gone_stops_the_turn_until_it_is_back() {
    let env = skills_env("", &[]);
    let shared = env.root.join("machine-skills");
    std::fs::create_dir_all(&shared).unwrap();
    let m = std::fs::read_to_string(&env.machine).unwrap();
    let shared_fwd = shared.display().to_string().replace('\\', "/");
    std::fs::write(&env.machine, format!("{m}skills_dir: '{shared_fwd}'\n")).unwrap();
    approved_plan(&env, "SK-R5", json!(["xpp-batch"]));
    let dir = env.repo.join(".toryo").join("skills").join("xpp-batch");
    let aside = env.root.join("xpp-batch-aside");
    std::fs::rename(&dir, &aside).unwrap();
    let turns = |name: &str| {
        script(
            &env,
            name,
            json!({
                "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
                "reviewer": [turn(vec![approved()])],
            }),
        )
    };
    let s = turns("r5a.json");
    let (_, err) = toryo(&env, &["run", "SK-R5", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "SK-R5");
    assert_eq!(t.state, St::Escalated, "{err}");
    let reason = t.machine.reason().unwrap_or_default().to_string();
    assert!(
        reason.starts_with("skill_unavailable: the implement turn (codex-openai) was not started")
            && reason.contains("skill 'xpp-batch' not found: neither")
            && reason.contains(&dir.join("SKILL.md").display().to_string())
            && reason.contains(" nor ")
            && reason.contains("machine-skills"),
        "{reason}"
    );
    assert!(
        attempts(&env, "SK-R5").iter().all(|a| a.stage == "plan"),
        "no implementer attempt"
    );
    assert!(calls_of(&env, "SK-R5", "implementer").is_empty());
    std::fs::rename(&aside, &dir).unwrap();
    let s = turns("r5b.json");
    let (code, err) = toryo(
        &env,
        &[
            "resume",
            "SK-R5",
            "--note",
            "the skill is back",
            "--yes",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "SK-R5").state, St::Completed, "{err}");
    let imp = calls_of(&env, "SK-R5", "implementer");
    assert!(
        imp[0]["context"]
            .as_str()
            .unwrap()
            .contains("BATCH-SKILL-BODY"),
        "the turn has the skill"
    );
}

/// M11-R5 AC-2: the plan-time budget check runs for every profile of the implementer chain —
/// skills that fit the Codex implementer (TASK.md and PLAN.md in the run store) but not the
/// Claude fallback (both inline, never trimmed) send the plan back naming the Claude profile.
#[test]
fn the_budget_check_covers_every_implementer_of_the_chain() {
    let big = format!(
        "---\ndescription: A long manual.\n---\n{}",
        "x".repeat(5 * 1024)
    );
    let org = ORGCHART.replace("IMPLEMENTER", "codex-openai").replace(
        "implementer: { profile: codex-openai }",
        "implementer: { profile: codex-openai, fallback: [claude-max] }",
    );
    let env = skills_env(
        "context: { budget_kb: { implementer: 8 } }\n",
        &[
            (".toryo/toryo-orgchart.yaml", &org),
            (".toryo/skills/xpp-batch/SKILL.md", &big),
        ],
    );
    let task_md = std::fs::read_to_string(env.repo.join("TASK.md")).unwrap();
    let long = env.root.join("TASK-long.md");
    std::fs::write(
        &long,
        format!(
            "{task_md}\n## Background\n\n{}\n",
            "Some background. ".repeat(220)
        ),
    )
    .unwrap();
    let (code, err) = toryo(
        &env,
        &[
            "task",
            "create",
            "--file",
            long.to_str().unwrap(),
            "--id",
            "SK-R5B",
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "SK-R5B");
    assert_eq!(t.implementers, ["codex-openai", "claude-max"]);
    let s = script(
        &env,
        "r5c.json",
        json!({"planner": [turn(vec![plan_with(json!(["xpp-batch"]))]), turn(vec![plan_with(json!([]))])]}),
    );
    let (code, err) = toryo(&env, &["run", "SK-R5B", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "SK-R5B").state, St::WaitingPlanApproval, "{err}");
    let p = calls_of(&env, "SK-R5B", "planner");
    assert_eq!(p.len(), 2, "re-planned once");
    let fb = p[1]["prompt"].as_str().unwrap();
    assert!(
        fb.contains("Choose fewer skills")
            && fb.contains("the implementer `claude-max`")
            && fb.contains("with TASK.md and PLAN.md inline")
            && !fb.contains("`codex-openai`"),
        "{fb}"
    );
}
