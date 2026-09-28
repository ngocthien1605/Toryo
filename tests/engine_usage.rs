//! M7 acceptance (plan §12, §17): usage accounting and the admission budget (R41),
//! DECISIONS.md and the final gate (R53).
//!
//! - `usage_accounting` (a) a Codex turn larger than the remaining budget is admitted and
//!   recorded, the next turn is refused → `Paused(Budget)`; `budget set` + resume continues
//!   (b) a killed Codex turn → `unknown` with a visible reservation, never 0
//!   (c) a resumed Codex thread reporting cumulative totals → exact deltas against the
//!   durable baseline, `includes_unreported`; reconciled only when the adapter is proven to
//!   include the killed turn (c') a killed Claude turn's provisional tokens are never
//!   counted twice
//! - the Claude live interrupt at 100 % of the budget
//! - `review_catches_agreeing_test`: replay of the recorded BR-002 streams — green verify,
//!   review `changes_requested` → fix → approved with a non-blocking issue → the final gate
//!   lists it and DECISIONS.md has it
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use std::path::PathBuf;
use toryo::engine::store::{self, AttemptStatus};
use toryo::engine::PipelineState as St;

type Row = (i64, String, String, String, Option<i64>, Option<i64>, Option<i64>, bool);

fn usage(env: &Env, id: &str) -> Vec<Row> {
    let i = id.to_string();
    db(env).call(move |tx| store::usage_rows(tx, &i)).unwrap()
}

fn tokens(input: u64, output: u64) -> Value {
    json!({"kind":"usage","input":input,"output":output})
}

fn with_reviewer(env: &Env, profile: &str) {
    let text = std::fs::read_to_string(&env.config).unwrap();
    let text = text.replacen("  implementer: {", &format!("  reviewer: {{ profile: {profile} }}\n  implementer: {{"), 1);
    std::fs::write(&env.config, text).unwrap();
}

fn stop_when_running(env: &Env, id: &str, stage: &str, child: &mut std::process::Child) {
    wait_until(&format!("{stage} to run"), 60, || {
        attempts(env, id).iter().any(|a| a.stage == stage && a.status == AttemptStatus::Running && a.external_session_id.is_some())
    });
    let (code, err) = toryo(env, &["stop", id], &[]);
    assert_eq!(code, 0, "{err}");
    child.wait().unwrap();
}

#[test]
fn usage_accounting_a_admission_cap_pauses_before_the_next_turn() {
    let env = setup_with("codex-openai", "pipeline:\n  budget_tokens: 1000\n");
    create(&env, "U-1");
    assert_eq!(task(&env, "U-1").budget_tokens, Some(1000));
    let s = script(&env, "u1.json", json!({
        "planner": [turn(vec![tokens(100, 50), plan()])],
        "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), tokens(2000, 500), implementation()])],
        "reviewer": [turn(vec![approved()])],
    }));
    let (code, err) = toryo(&env, &["run", "U-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "U-1");
    assert_eq!(t.state, St::Paused, "{err}");
    assert_eq!(t.machine.reason(), Some("budget"));
    assert_eq!(t.machine.paused_from(), Some(St::Reviewing));
    assert_eq!(t.spent_tokens, 2650, "the admitted turn's overshoot is recorded");
    assert!(err.contains("admission cap reached"), "{err}");
    assert!(calls(&env, "U-1").iter().all(|c| c["key"] != "reviewer"), "the refused turn never started");
    let (_, out) = toryo(&env, &["status", "U-1"], &[]);
    let _ = out;
    let o = command(&env, &["status", "U-1"], &[]).output().unwrap();
    let status = String::from_utf8_lossy(&o.stdout);
    assert!(status.contains("admission cap (may overshoot by one turn)"), "{status}");
    // resume without a new budget pauses again at once
    let (code, _) = toryo(&env, &["resume", "U-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 1);
    assert_eq!(task(&env, "U-1").state, St::Paused);
    let (code, err) = toryo(&env, &["budget", "set", "U-1", "--tokens", "10000"], &[]);
    assert_eq!(code, 0, "{err}");
    let s2 = script(&env, "u1b.json", json!({ "reviewer": [turn(vec![tokens(300, 30), approved()])] }));
    let (code, err) = toryo(&env, &["resume", "U-1", "--yes", "--fake-agents", &s2], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "U-1");
    assert_eq!(t.state, St::Completed);
    assert_eq!(t.spent_tokens, 2980);
    let dec = std::fs::read_to_string(t.artifacts().join("DECISIONS.md")).unwrap();
    assert!(dec.contains("budget set") && dec.contains("1000 → 10000"), "{dec}");
    assert!(dec.contains("final gate approved"), "{dec}");
}

#[test]
fn usage_accounting_b_c_killed_codex_turn_and_cumulative_resume() {
    for proven in [false, true] {
        let env = setup("codex-openai");
        with_reviewer(&env, "codex-openai");
        let id = "U-2";
        create(&env, id);
        let caps = if proven {
            json!({"codex-openai": {"live": false, "terminal": true, "cumulative_on_resume": "Yes", "killed_turn_in_resume": "Yes"}})
        } else {
            json!({})
        };
        let p = env.root.join("u2.json");
        std::fs::write(
            &p,
            serde_json::to_vec(&json!({"capabilities": caps, "turns": {
                "planner": [turn(vec![tokens(10, 0), plan()])],
                "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), tokens(1000, 100), implementation()])],
                "reviewer": [turn(vec![hang()])],
            }}))
            .unwrap(),
        )
        .unwrap();
        let mut child = spawn(&env, &["run", id, "--yes", "--fake-agents", p.to_str().unwrap()], "u2.log");
        stop_when_running(&env, id, "review", &mut child);
        // (b) the killed Codex turn: unknown, with a visible reservation (the largest exact
        // turn of the same stage, else 200 000) — never 0
        let rows = usage(&env, id);
        let killed = rows.iter().find(|r| r.1 == "U-2/review/1").unwrap();
        assert_eq!((killed.3.as_str(), killed.5), ("unknown", Some(200_000)));
        assert_eq!(task(&env, id).reserved_tokens, 200_000, "{:?}", usage(&env, id));
        let o = command(&env, &["task", "show", id], &[]).output().unwrap();
        assert!(String::from_utf8_lossy(&o.stdout).contains("~200000"));
        // (c) resume on the same thread: cumulative totals → deltas against the baseline
        let invalid = json!({"kind":"complete","structured":{"decision":"approved","summary":"s","acceptance_criteria":[],"issues":[]}});
        let p2 = env.root.join("u2b.json");
        std::fs::write(
            &p2,
            serde_json::to_vec(&json!({"capabilities": caps, "turns": {
                "reviewer": [turn(vec![tokens(3000, 300), invalid]), turn(vec![tokens(4000, 400), approved()])],
            }}))
            .unwrap(),
        )
        .unwrap();
        let (code, err) = toryo(&env, &["resume", id, "--yes", "--fake-agents", p2.to_str().unwrap()], &[]);
        assert_eq!(code, 0, "{err}");
        let t = task(&env, id);
        assert_eq!(t.state, St::Completed, "{err}");
        let rows = usage(&env, id);
        let review2: Vec<&Row> = rows.iter().filter(|r| r.1 == "U-2/review/2").collect();
        assert_eq!(review2.len(), 2, "{rows:?}");
        assert_eq!((review2[0].3.as_str(), review2[0].4, review2[0].7), ("exact", Some(3300), true), "first resumed total includes the killed turn");
        assert_eq!((review2[1].3.as_str(), review2[1].4, review2[1].7), ("exact", Some(1100), false), "the correction turn is the delta against the baseline");
        let killed = rows.iter().find(|r| r.1 == "U-2/review/1").unwrap();
        if proven {
            assert_eq!(killed.6, Some(review2[0].0), "reconciled by the cumulative row");
            assert_eq!(t.reserved_tokens, 0, "the reservation is released");
            assert_eq!(t.spent_tokens, 10 + 1100 + 3300 + 1100, "no loss, no double count");
        } else {
            assert_eq!(killed.6, None);
            assert_eq!(t.reserved_tokens, 200_000, "not proven: the reservation keeps counting (~)");
        }
    }
}

#[test]
fn usage_accounting_c_prime_claude_provisional_never_counted_twice() {
    for cumulative in [false, true] {
        let env = setup("codex-openai");
        let id = "U-3";
        create(&env, id);
        let caps = if cumulative {
            json!({"claude-max": {"live": true, "terminal": true, "cumulative_on_resume": "Yes", "killed_turn_in_resume": "Yes"}})
        } else {
            json!({})
        };
        let p = env.root.join("u3.json");
        std::fs::write(
            &p,
            serde_json::to_vec(&json!({"capabilities": caps, "turns": {
                "planner": [turn(vec![plan()])],
                "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
                "reviewer": [turn(vec![tokens(500, 100), hang()])],
            }}))
            .unwrap(),
        )
        .unwrap();
        let mut child = spawn(&env, &["run", id, "--yes", "--fake-agents", p.to_str().unwrap()], "u3.log");
        wait_until("streamed usage", 60, || std::fs::read_to_string(env.root.join("u3.log")).is_ok_and(|l| l.contains("REVIEW")));
        std::thread::sleep(std::time::Duration::from_millis(500));
        stop_when_running(&env, id, "review", &mut child);
        let killed = usage(&env, id).into_iter().find(|r| r.1 == "U-3/review/1").unwrap();
        assert_eq!((killed.3.as_str(), killed.4), ("provisional", Some(600)));
        let spent_before = task(&env, id).spent_tokens;
        // per-run fixture: the resumed turn reports only its own tokens; cumulative fixture:
        // the session total, which includes the killed turn
        let resumed = if cumulative { tokens(1500, 300) } else { tokens(1000, 200) };
        let p2 = env.root.join("u3b.json");
        std::fs::write(&p2, serde_json::to_vec(&json!({"capabilities": caps, "turns": {"reviewer": [turn(vec![resumed, approved()])]}})).unwrap()).unwrap();
        let (code, err) = toryo(&env, &["resume", id, "--yes", "--fake-agents", p2.to_str().unwrap()], &[]);
        assert_eq!(code, 0, "{err}");
        let t = task(&env, id);
        // per run: +1200 (the resumed turn alone; the provisional 600 stands);
        // cumulative: +1800 total − 600 reconciled provisional = +1200. Either way 600 counts once.
        assert_eq!(t.spent_tokens - spent_before, 1200, "the provisional 600 counted exactly once (cumulative={cumulative})");
    }
}

#[test]
fn claude_live_interrupt_at_the_budget() {
    let env = setup_with("codex-openai", "pipeline:\n  budget_tokens: 1000\n");
    create(&env, "U-4");
    let s = script(&env, "u4.json", json!({
        "planner": [turn(vec![tokens(100, 0), plan()])],
        "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), tokens(100, 0), implementation()])],
        "reviewer": [turn(vec![tokens(5000, 10), hang()])],
    }));
    let (code, err) = toryo(&env, &["run", "U-4", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "U-4");
    assert_eq!(t.state, St::Paused, "{err}");
    assert!(err.contains("live interrupt"), "{err}");
    let a = attempts(&env, "U-4").into_iter().find(|a| a.stage == "review").unwrap();
    assert_eq!((a.status, a.error_class.as_deref()), (AttemptStatus::Interrupted, Some("budget")));
    let r = usage(&env, "U-4").into_iter().find(|r| r.1 == "U-4/review/1").unwrap();
    assert_eq!((r.3.as_str(), r.4), ("provisional", Some(5010)));
}

// ── R53: the review catches a test that agrees with a bug ─────────────────────────────

#[test]
fn review_catches_agreeing_test() {
    let env = setup("codex-openai");
    // the BatchRename sample app, where BR-002 was recorded
    std::fs::remove_dir_all(&env.repo).unwrap();
    let (code, err) = toryo(&env, &["spike", "init-fixture", "--app", "batch-rename", "--into", env.repo.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{err}");
    let text = std::fs::read_to_string(&env.config)
        .unwrap()
        .replace("args: [\"grep\", \"-q\", \"Divide-Numbers\", \"--\", \"src\"]", "args: [\"grep\", \"-q\", \"ConvertTo-SafeFileName\", \"--\", \"src\"]");
    std::fs::write(&env.config, text).unwrap();
    let (code, err) = toryo(&env, &["task", "create", "--file", "tasks/BR-002.md"], &[]);
    assert_eq!(code, 0, "{err}");
    let cap = |f: &str| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/captured").join(f).display().to_string();
    let br_plan = json!({"kind":"complete","structured":{"understanding":"u","files_likely_involved":["src/BatchRename.psm1","tests/Sequence.Tests.ps1"],"steps":["sequence"],"validation":["tests"],"plan_markdown":"md"}});
    let approved_with_followup = json!({"kind":"complete","structured":{"decision":"approved","summary":"AC-2 is now shown by a case that separates ordinal from culture sorting.","acceptance_criteria":[
        {"id":"AC-1","met":true,"evidence":"Sequence tests pass"},{"id":"AC-2","met":true,"evidence":"the new '_first' vs 'Zeta' case"},
        {"id":"AC-3","met":true,"evidence":"literal {n} test"},{"id":"AC-4","met":true,"evidence":"all BR-001 tests pass"}],
        "issues":[{"id":"I-1","severity":"low","blocking":false,"file":"src/BatchRename.psm1","description":"-Pad above 9 is silently clamped; document it"}]}});
    let s = script(&env, "br.json", json!({
        "planner": [turn(vec![br_plan])],
        "implementer": [
            turn(vec![json!({"kind":"replay","agent":"codex","file":cap("codex-implement.jsonl")})]),
            turn(vec![json!({"kind":"replay","agent":"codex","file":cap("codex-fix.jsonl")})]),
        ],
        "reviewer": [
            turn(vec![json!({"kind":"replay","agent":"claude","file":cap("claude-review-changes-requested.jsonl")})]),
            turn(vec![approved_with_followup]),
        ],
    }));
    let (code, err) = toryo(&env, &["run", "BR-002", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = toryo(&env, &["approve", "BR-002", "--gate", "plan", "--continue", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "BR-002");
    assert_eq!(t.state, St::WaitingFinalApproval, "{err}");
    // verify was green both times, yet the first review sent it back (unmet AC-2/AC-4)
    let verifies: Vec<_> = attempts(&env, "BR-002").into_iter().filter(|a| a.stage == "verify").collect();
    assert_eq!(verifies.len(), 2);
    assert!(verifies.iter().all(|a| a.status == AttemptStatus::Succeeded));
    assert_eq!(t.fix_cycle, 1, "the green verify did not skip the review (R53)");
    // the final gate lists the non-blocking issue; DECISIONS.md keeps it as a follow-up
    assert!(err.contains("final gate — non-blocking issues") && err.contains("I-1 [low]"), "{err}");
    let dec = std::fs::read_to_string(t.artifacts().join("DECISIONS.md")).unwrap();
    assert!(dec.contains("plan gate approved") && dec.contains("follow-ups") && dec.contains("I-1"), "{dec}");
    let summary = std::fs::read_to_string(t.artifacts().join("SUMMARY.md")).unwrap();
    assert!(summary.contains("## Final gate") && summary.contains("I-1"), "{summary}");
    // the recorded Codex usage (with cached input) lands in disjoint buckets
    let rows = usage(&env, "BR-002");
    assert!(rows.iter().filter(|r| r.2 == "codex-openai").all(|r| r.3 == "exact" && r.4.unwrap_or(0) > 0), "{rows:?}");
}

#[test]
fn same_vendor_note_at_the_final_gate() {
    let env = setup("codex-openai");
    create(&env, "U-5");
    let s = script(&env, "u5.json", json!({
        "planner": [turn(vec![plan()])],
        "implementer": [turn(vec![fail("QuotaExceeded", "limit")])],
        "implementer@claude-max": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
        "reviewer": [turn(vec![approved()])],
    }));
    toryo(&env, &["run", "U-5", "--fake-agents", &s], &[]);
    let (code, err) = toryo(&env, &["approve", "U-5", "--gate", "plan", "--continue", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "U-5").state, St::WaitingFinalApproval);
    assert!(err.contains("same vendor: claude-max implemented and claude-max reviewed"), "{err}");
}
