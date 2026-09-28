//! M4 acceptance (plan §7, §17): the ProjectContextLoader in the engine.
//!
//! - every agent attempt gets one assembled context, stored as `context/<stage>-<attempt>`
//!   with its SHA-256 in `stage_attempts.context_sha256`
//! - fixed source order; Codex does not get AGENTS.md/CLAUDE.md (reads AGENTS.md natively)
//! - the Claude implementer gets the task pack inline (no run-store access, R49)
//! - the budget trims with a warning, never the policy files
//! - `toryo context show` previews exactly what the next attempt receives, and re-checks a
//!   stored attempt's hash
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use toryo::engine::store;
use toryo::engine::PipelineState as St;

fn add_sources(env: &Env) {
    std::fs::write(env.root.join("global.md"), "GLOBAL-RULE: answer in English\n").unwrap();
    std::fs::write(env.repo.join("CLAUDE.md"), "CLAUDE-RULE: prefer small functions\n").unwrap();
    std::fs::create_dir_all(env.repo.join(".toryo/context")).unwrap();
    std::fs::write(env.repo.join(".toryo/context/10-models.md"), "POLICY-MODELS: ZZZ is local-only\n").unwrap();
    std::fs::write(env.repo.join(".toryo/context/20-naming.md"), "POLICY-NAMING: Verb-Noun\n").unwrap();
}

fn full_run(env: &Env, id: &str) -> Vec<Value> {
    create(env, id);
    let s = script(env, &format!("{id}.json"), json!({
        "planner": [turn(vec![plan()])],
        "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
        "reviewer": [turn(vec![approved()])],
    }));
    let (code, err) = toryo(env, &["run", id, "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(env, id).state, St::Completed, "{err}");
    calls(env, id)
}

fn ctx_of<'a>(calls: &'a [Value], key: &str) -> &'a str {
    calls.iter().find(|c| c["key"] == key).and_then(|c| c["context"].as_str()).unwrap_or_else(|| panic!("no context for {key}"))
}

#[test]
fn every_attempt_gets_a_stored_hashed_context_in_fixed_order() {
    let env = setup("codex-openai");
    add_sources(&env);
    let calls = full_run(&env, "C-1");

    // Claude planner: 1 global → 2 AGENTS.md, CLAUDE.md → 3 policy (sorted) → 4 role → 5 pack
    let p = ctx_of(&calls, "planner");
    let pos = |x: &str| p.find(x).unwrap_or_else(|| panic!("`{x}` missing from the planner context:\n{p}"));
    assert!(pos("GLOBAL-RULE") < pos("Calculator fixture — agent instructions"));
    assert!(pos("Calculator fixture — agent instructions") < pos("CLAUDE-RULE"));
    assert!(pos("CLAUDE-RULE") < pos("POLICY-MODELS"));
    assert!(pos("POLICY-MODELS") < pos("POLICY-NAMING"));
    assert!(pos("POLICY-NAMING") < pos("# Role: planner"));
    assert!(pos("# Role: planner") < pos("CALC-001"), "TASK.md is small: inlined");

    // Codex implementer: no source 2 (Codex reads AGENTS.md itself, R20); PLAN.md inlined
    let i = ctx_of(&calls, "implementer");
    assert!(i.contains("GLOBAL-RULE") && i.contains("POLICY-MODELS") && i.contains("# Role: implementer"));
    assert!(!i.contains("CLAUDE-RULE") && !i.contains("Calculator fixture — agent instructions"), "{i}");
    assert!(i.contains("## PLAN.md (task pack, inlined)"));
    let prompt = calls.iter().find(|c| c["key"] == "implementer").unwrap()["prompt"].as_str().unwrap();
    assert!(!prompt.contains("# Role: implementer"), "the role now comes from the context, not the prompt");

    // Reviewer: the diff and the verify result are in its pack
    let r = ctx_of(&calls, "reviewer");
    assert!(r.contains("# Role: reviewer") && r.contains("DIFF.patch") && r.contains("VERIFY_RESULT.json"));

    // stored per attempt, hash recorded, artifact registered, event logged
    let t = task(&env, "C-1");
    let st = db(&env);
    for a in attempts(&env, "C-1").into_iter().filter(|a| a.kind == "agent") {
        let path = t.artifacts().join("context").join(format!("{}-{}.md", a.stage, a.attempt));
        let bytes = std::fs::read(&path).unwrap_or_else(|_| panic!("{} has no stored context", a.id));
        assert_eq!(a.context_sha256.as_deref(), Some(toryo::engine::task::sha256_hex(&bytes).as_str()), "{}", a.id);
    }
    let evs = st.call(|tx| store::events(tx, "C-1")).unwrap();
    assert_eq!(evs.iter().filter(|e| e.1 == "context_assembled").count(), 3);
    let logical = st
        .call(|tx| Ok(tx.query_row("SELECT COUNT(*) FROM artifacts WHERE task_id = 'C-1' AND kind = 'context'", [], |r| r.get::<_, i64>(0))?))
        .unwrap();
    assert_eq!(logical, 3);

    // `context show --attempt` prints the stored bytes and re-checks the hash
    let out = command(&env, &["context", "show", "C-1", "--stage", "review", "--attempt", "1"], &[]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), r);
    // the view is not evidence (R34): editing it changes nothing; the immutable version is checked
    std::fs::write(t.artifacts().join("context").join("review-1.md"), "edited view\n").unwrap();
    let out = command(&env, &["context", "show", "C-1", "--stage", "review", "--attempt", "1"], &[]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let v = toryo::engine::artifacts::head(&st, &t, "context/review-1").unwrap().unwrap();
    assert!(v.path.starts_with(t.artifacts().join("artifacts").join("context")), "{}", v.path.display());
    assert!(r.contains("DIFF.patch: ") && r.contains(&t.artifacts().join("artifacts").join("diff").display().to_string()), "agents get version paths: {r}");
    std::fs::write(&v.path, format!("{r}\nINJECTED\n")).unwrap();
    let out = command(&env, &["context", "show", "C-1", "--stage", "review", "--attempt", "1"], &[]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("artifact_tampered"));
}

#[test]
fn claude_implementer_gets_the_task_pack_inline() {
    let env = setup("claude-max");
    let calls = full_run(&env, "C-2");
    let i = ctx_of(&calls, "implementer");
    assert!(i.contains("not accessible in this session"), "{i}");
    assert!(i.contains("CALC-001") && i.contains("## PLAN.md"), "TASK.md and PLAN.md are inlined");
    assert!(i.contains("Calculator fixture — agent instructions"), "Claude gets AGENTS.md from the loader (safe mode skips it)");
    assert!(!i.contains("- TASK.md: "), "no run-store paths for the Claude implementer");
    let prompt = calls.iter().find(|c| c["key"] == "implementer").unwrap()["prompt"].as_str().unwrap();
    assert!(prompt.contains("NO shell"));
}

#[test]
fn context_show_previews_the_next_attempt_and_the_budget_trims() {
    let env = setup("codex-openai");
    add_sources(&env);
    std::fs::write(env.root.join("global.md"), format!("GLOBAL-START\n{}\n", "g".repeat(60_000))).unwrap();
    create(&env, "C-3");
    // preview before anything ran: exactly what the planner will get
    let out = command(&env, &["context", "show", "C-3", "--stage", "plan"], &[]).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(err.contains("global.md") && err.contains("trimmed") && err.contains("context budget"), "{err}");
    let preview = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(preview.len() <= 40 * 1024);
    assert!(preview.contains("POLICY-MODELS") && preview.contains("GLOBAL-START"));
    let s = script(&env, "c3.json", json!({ "planner": [turn(vec![plan()])] }));
    let (code, err) = toryo(&env, &["run", "C-3", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("context budget: global.md truncated"), "{err}");
    assert_eq!(ctx_of(&calls(&env, "C-3"), "planner"), preview, "`context show` printed exactly what the planner received");
    // a preview for another profile: Codex would not get AGENTS.md/CLAUDE.md
    let out = command(&env, &["context", "show", "C-3", "--stage", "implement", "--profile", "codex-openai"], &[]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("# Role: implementer") && !text.contains("CLAUDE-RULE"));
}
