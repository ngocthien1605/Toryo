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

/// The org chart with the implementer `codex-openai` falling back to `claude-max`, the
/// reviewer `claude-max`, and `reviewer-differs-from-builder: <level>`.
fn vendor_env(level: &str, fallback: bool) -> Env {
    let org = ORGCHART.replace("IMPLEMENTER", "codex-openai").replace(
        "implementer: { profile: codex-openai }",
        if fallback {
            "implementer: { profile: codex-openai, fallback: [claude-max] }"
        } else {
            "implementer: { profile: codex-openai }"
        },
    ) + &format!("checks:\n  reviewer-differs-from-builder: {level}\n");
    setup_orgchart("codex-openai", "", &[(".toryo/toryo-orgchart.yaml", &org)])
}

/// Plan gate → `approve --continue`; returns the stderr of the second command.
fn run_to_the_end(env: &Env, id: &str, quota: bool) -> String {
    let implementer = if quota {
        json!({
            "implementer": [turn(vec![fail("QuotaExceeded", "You've hit your usage limit")])],
            "implementer@claude-max": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
        })
    } else {
        json!({"implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])]})
    };
    let mut turns = json!({
        "planner": [turn(vec![plan()])],
        "reviewer": [turn(vec![approved()])],
    });
    for (k, v) in implementer.as_object().unwrap() {
        turns[k] = v.clone();
    }
    let s = script(env, &format!("{id}.json"), turns);
    create(env, id);
    let (code, err) = toryo(env, &["run", id, "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let (_, err) = toryo(
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
    err
}

fn events(env: &Env, id: &str) -> Vec<Value> {
    std::fs::read_to_string(task(env, id).run_store.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// M11-C3 AC-1: the implementer falls back from `codex-openai` (quota) to `claude-max`, the
/// reviewer is `claude-max`. `warn` → the same-vendor note at the final gate; `off` → no
/// note; `error` → `Escalated` with `same_vendor_review` before any review turn.
#[test]
fn reviewer_differs_from_builder_off_warn_error() {
    let env = vendor_env("warn", true);
    let err = run_to_the_end(&env, "SV-1", true);
    assert_eq!(task(&env, "SV-1").state, St::WaitingFinalApproval, "{err}");
    assert!(
        err.contains("final gate — same vendor: claude-max implemented and claude-max reviewed (Claude both)"),
        "{err}"
    );

    let env = vendor_env("off", true);
    let err = run_to_the_end(&env, "SV-2", true);
    assert_eq!(task(&env, "SV-2").state, St::WaitingFinalApproval, "{err}");
    assert!(!err.contains("same vendor"), "{err}");

    let env = vendor_env("error", true);
    let err = run_to_the_end(&env, "SV-3", true);
    let t = task(&env, "SV-3");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert!(
        err.contains("same_vendor_review: claude-max built this checkpoint and claude-max would review it (Claude both)"),
        "{err}"
    );
    assert!(
        attempts(&env, "SV-3").iter().all(|a| a.stage != "review"),
        "no review turn"
    );
    let ev = events(&env, "SV-3");
    assert!(
        ev.iter()
            .any(|e| e["type"] == "escalated" && e["reason_class"] == "same_vendor_review"),
        "{ev:?}"
    );
}

/// M11-C3 AC-2: no fallback (Codex builds, Claude reviews) → no note and no escalation at any
/// level.
#[test]
fn different_vendors_pass_at_every_level() {
    for (i, level) in ["off", "warn", "error"].iter().enumerate() {
        let env = vendor_env(level, false);
        let id = format!("SV-{}", 10 + i);
        let err = run_to_the_end(&env, &id, false);
        assert_eq!(
            task(&env, &id).state,
            St::WaitingFinalApproval,
            "{level}: {err}"
        );
        assert!(
            !err.contains("same vendor") && !err.contains("same_vendor"),
            "{level}: {err}"
        );
    }
}
