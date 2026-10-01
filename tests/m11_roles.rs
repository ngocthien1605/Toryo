//! M11 group C end to end, on FakeAgent: the implementer's write fence (C2), the
//! reviewer-differs-from-builder check (C3) and the split retry limits (C4).
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use toryo::engine::PipelineState as St;

const IMPL: &str = ".toryo/roles/implementer/toryo-role.yaml";

/// The implementer's role file with a fence.
fn fenced_role(writes: &str, never: &str) -> String {
    format!(
        "toryo: 1\npurpose: Change the code.\nwrites: {writes}\nnever: {never}\nbefore_handoff: [divide-present]\n"
    )
}

fn plan_listing(files: &[&str]) -> Value {
    json!({"kind":"complete","structured":{"understanding":"u","files_likely_involved":files,"steps":["add"],"validation":["tests"],"plan_markdown":"md"}})
}

fn prompts(env: &Env, id: &str, key: &str) -> Vec<String> {
    calls(env, id)
        .iter()
        .filter(|c| c["key"].as_str().is_some_and(|k| k.starts_with(key)))
        .map(|c| c["prompt"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// M11-C2 AC-1: a plan listing `docs/x.md` with `writes: [src/**]` is re-planned
/// automatically with the fence in the feedback; the next plan, inside the fence, opens the
/// plan gate.
#[test]
fn a_plan_outside_the_write_fence_is_replanned() {
    let role = fenced_role("[src/**]", "['**/Descriptor/*.xml']");
    let env = setup_orgchart("codex-openai", "", &[(IMPL, &role)]);
    create(&env, "WF-1");
    let s = script(
        &env,
        "wf1.json",
        json!({
            "planner": [turn(vec![plan_listing(&["docs/x.md", "src/Calculator.psm1"])]), turn(vec![plan_listing(&["src/Calculator.psm1"])])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "WF-1", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "WF-1").state, St::WaitingPlanApproval, "{err}");
    assert!(
        err.contains("the plan lists paths the implementer may not change [\"docs/x.md\"]"),
        "{err}"
    );
    let p = prompts(&env, "WF-1", "planner");
    assert_eq!(p.len(), 2, "{p:?}");
    assert!(
        p[1].contains("`docs/x.md` — outside the implementer's write fence (`writes: [src/**]`)"),
        "{}",
        p[1]
    );
}

/// M11-C2 AC-2: a `never` path the implementer writes is restored and the fix note names
/// `never`; with the path under `## Allow protected` in TASK.md it is accepted.
#[test]
fn a_never_path_is_restored_unless_the_task_allows_it() {
    const MODEL: &str = "Metadata/Pkg/Model/Descriptor/Model.xml";
    let role = fenced_role("[src/**, Metadata/**]", "['**/Descriptor/*.xml']");
    let env = setup_orgchart("codex-openai", "", &[(IMPL, &role)]);
    let turns = |id: &str| {
        script(
            &env,
            &format!("{id}.json"),
            json!({
                "planner": [turn(vec![plan()])],
                "implementer": [
                    turn(vec![write("src/Calculator.psm1", DIVIDE), write(MODEL, "<AxModel/>\n"), implementation()]),
                    turn(vec![implementation()]),
                ],
                "reviewer": [turn(vec![approved()])],
            }),
        )
    };
    create(&env, "WF-2");
    let s = turns("wf2");
    let (code, err) = toryo(&env, &["run", "WF-2", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "WF-2");
    assert_eq!(t.state, St::Completed, "{err}");
    assert_eq!(t.fix_cycle, 1, "one fix cycle for the fence violation");
    assert!(!wt(&env, "WF-2").join(MODEL).exists(), "restored (removed)");
    let fix = prompts(&env, "WF-2", "implementer");
    assert!(
        fix[1].contains(&format!(
            "`{MODEL}` (touched a never path (`never: [**/Descriptor/*.xml]`))"
        )),
        "{}",
        fix[1]
    );
    let files = git(&wt(&env, "WF-2"), &["diff", "--name-only", "main..HEAD"]);
    assert!(!files.contains("Descriptor"), "{files}");

    // the same with `## Allow protected` in TASK.md: accepted, committed
    let task_md = std::fs::read_to_string(env.repo.join("TASK.md")).unwrap();
    let allow = env.root.join("TASK-allow.md");
    std::fs::write(
        &allow,
        format!("{task_md}\n## Allow protected\n- {MODEL}\n"),
    )
    .unwrap();
    let (code, err) = toryo(
        &env,
        &[
            "task",
            "create",
            "--file",
            allow.to_str().unwrap(),
            "--id",
            "WF-3",
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let s = turns("wf3");
    let (code, err) = toryo(&env, &["run", "WF-3", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "WF-3");
    assert_eq!(t.state, St::Completed, "{err}");
    assert_eq!(t.fix_cycle, 0, "{err}");
    let files = git(&wt(&env, "WF-3"), &["diff", "--name-only", "main..HEAD"]);
    assert!(files.contains(MODEL), "{files}");
}
