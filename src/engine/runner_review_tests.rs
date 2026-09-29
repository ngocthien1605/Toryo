//! The Planner/Reviewer turn decisions, the gates and the verify helpers of the 2026-09-29
//! review fixes (WP-C2b): pure functions, no store, git or agents. The end-to-end regression
//! tests are in `tests/regress_engine.rs`.

use super::*;
use crate::domain::ErrorClass as C;

fn turn(success: bool, class: Option<ErrorClass>, detail: Option<&str>) -> StageOutcome {
    StageOutcome {
        success,
        error_class: class,
        error_detail: detail.map(str::to_string),
        ..Default::default()
    }
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn valid_plan() -> Value {
    json!({"understanding": "u", "files_likely_involved": ["src/a.ps1"], "steps": ["s"], "validation": ["v"], "plan_markdown": "md"})
}

// ── EC-01 / EC-14: what a finished read-only turn yields ─────────────────────────────

#[test]
fn a_failed_read_only_turn_never_yields_its_payload() {
    let review = json!({"decision": "approved"});
    // EC-01: the denied turn carries a valid-looking payload (a real Claude result does)
    let denied = StageOutcome {
        structured: Some(review.clone()),
        ..turn(
            false,
            Some(C::PermissionDenied),
            Some("1 permission denial(s)"),
        )
    };
    assert_eq!(
        ReadOnlyTurn::of(&denied),
        ReadOnlyTurn::Failed(C::PermissionDenied)
    );
    // a denial fails the turn even if an adapter reported success (plan §6.2)
    let reported_ok = StageOutcome {
        structured: Some(review.clone()),
        permission_denials: vec![("Read".into(), "C:/Users/me/.ssh/id_rsa".into())],
        ..turn(true, None, None)
    };
    assert_eq!(
        ReadOnlyTurn::of(&reported_ok),
        ReadOnlyTurn::Failed(C::PermissionDenied)
    );
    let crashed = StageOutcome {
        structured: Some(review.clone()),
        ..turn(false, Some(C::ProcessCrashed), None)
    };
    assert_eq!(
        ReadOnlyTurn::of(&crashed),
        ReadOnlyTurn::Retry(C::ProcessCrashed)
    );
    // only a successful turn has output — `Null` when it returned none
    let ok = StageOutcome {
        structured: Some(review.clone()),
        ..turn(true, None, None)
    };
    assert_eq!(ReadOnlyTurn::of(&ok), ReadOnlyTurn::Output(review));
    assert_eq!(
        ReadOnlyTurn::of(&turn(true, None, None)),
        ReadOnlyTurn::Output(Value::Null)
    );
}

#[test]
fn failed_turns_retry_escalate_or_count_as_invalid_output_by_class() {
    use ReadOnlyTurn::{Failed, Invalid, Retry};
    let cases = [
        // EC-14: crash, timeout, network, unknown → the same turn again (bounded)
        (Some(C::ProcessCrashed), Retry(C::ProcessCrashed)),
        (Some(C::Timeout), Retry(C::Timeout)),
        (Some(C::Network), Retry(C::Network)),
        (Some(C::Unknown), Retry(C::Unknown)),
        (None, Retry(C::Unknown)),
        // no blind retry: escalate with the real class (§11)
        (Some(C::PermissionDenied), Failed(C::PermissionDenied)),
        (Some(C::PolicyRefusal), Failed(C::PolicyRefusal)),
        (Some(C::Auth), Failed(C::Auth)),
        (Some(C::BadRequest), Failed(C::BadRequest)),
        (
            Some(C::ContextWindowExceeded),
            Failed(C::ContextWindowExceeded),
        ),
        // quota and rate limits pause before this decision; were they here, no retry either
        (Some(C::QuotaExceeded), Failed(C::QuotaExceeded)),
        (Some(C::RateLimited), Failed(C::RateLimited)),
        (Some(C::Overloaded), Failed(C::Overloaded)),
        // the output itself was unusable: invalid output → the correction loop (R22)
        (
            Some(C::InvalidOutput),
            Invalid("the final response could not be used: final message is not JSON".into()),
        ),
    ];
    for (class, want) in cases {
        let r = turn(false, class, Some("final message is not JSON"));
        assert_eq!(ReadOnlyTurn::of(&r), want, "{class:?}");
    }
    assert_eq!(
        ReadOnlyTurn::of(&turn(false, Some(C::InvalidOutput), None)),
        Invalid("the final response could not be used: InvalidOutput".into())
    );
}

#[test]
fn a_read_only_failure_is_named_by_its_real_class() {
    assert_eq!(
        read_only_failure("reviewer", C::PermissionDenied, Some("1 permission denial(s)"), 0),
        "PermissionDenied: the reviewer turn failed (1 permission denial(s)); a failed turn is never used and not retried blindly (plan §6.2, §11)"
    );
    // EC-14: after the retries, the class of the last failure — never `InvalidOutput`
    assert_eq!(
        read_only_failure(
            "reviewer",
            C::ProcessCrashed,
            Some("event stream ended without a terminal event"),
            MAX_TURN_RETRIES
        ),
        "ProcessCrashed: the reviewer turn failed (event stream ended without a terminal event), also after 2 retries of the same turn (plan §11)"
    );
    assert_eq!(
        read_only_failure("planner", C::Timeout, Some(""), 1),
        "Timeout: the planner turn failed (no detail), also after 1 retries of the same turn (plan §11)"
    );
    assert!(read_only_failure("planner", C::PolicyRefusal, None, 0)
        .starts_with("PolicyRefusal: the planner turn failed (no detail);"));
    let r = turn(false, Some(C::Network), Some("stream disconnected"));
    assert_eq!(
        retry_note("planner", C::Network, &r, 1),
        "!! planner turn failed: Network (stream disconnected) — the same turn again (1/2, EC-14)"
    );
}

// ── EC-25: plans and reviews are validated against their schemas ─────────────────────

#[test]
fn only_a_schema_valid_plan_is_a_plan() {
    assert_eq!(
        plan_output(ReadOnlyTurn::Output(valid_plan())),
        Ok(valid_plan())
    );
    assert_eq!(
        plan_output(ReadOnlyTurn::Output(Value::Null)),
        Err("InvalidOutput: the planner returned no structured plan".into())
    );
    // the probe's plan: "not a plan at all" never reaches the plan gate
    let e = plan_output(ReadOnlyTurn::Output(json!({"note": "not a plan at all"}))).unwrap_err();
    assert!(
        e.starts_with("InvalidOutput: the planner returned no valid plan — it does not match plan.schema.json: $: missing required property `understanding`; "),
        "{e}"
    );
    let mut wrong = valid_plan();
    wrong["steps"] = json!("one step");
    assert_eq!(
        plan_output(ReadOnlyTurn::Output(wrong)),
        Err("InvalidOutput: the planner returned no valid plan — it does not match plan.schema.json: $.steps: expected array, got string".into())
    );
    assert_eq!(
        plan_output(ReadOnlyTurn::Invalid("the final response could not be used: x".into())),
        Err("InvalidOutput: the planner returned no structured plan: the final response could not be used: x".into())
    );
    assert_eq!(
        plan_output(ReadOnlyTurn::Failed(C::PermissionDenied)),
        Err("PermissionDenied: the planner turn failed; a failed turn never yields a plan".into())
    );
}

#[test]
fn schema_problems_join_the_review_checks() {
    let ids = strings(&["AC-1", "AC-2"]);
    let ac = |id: &str| json!({"id": id, "met": true, "evidence": "e"});
    // R28-valid, schema-valid
    let ok = json!({"decision": "approved", "summary": "s", "acceptance_criteria": [ac("AC-1"), ac("AC-2")], "issues": []});
    assert_eq!(review_problems(&ok, &ids), Vec::<String>::new());
    // R28-valid, but the issue id and severity violate review.schema.json (the old TS-03
    // fixtures): a correction turn, not a silent pass
    let mut bad = ok.clone();
    bad["decision"] = json!("changes_requested");
    bad["issues"] =
        json!([{"id": "R-1", "severity": "major", "blocking": true, "description": "d"}]);
    assert_eq!(
        review_problems(&bad, &ids),
        [
            "$.issues[0].id: \"R-1\" does not match ^I-[1-9][0-9]*$",
            "$.issues[0].severity: \"major\" is not one of \"high\", \"medium\", \"low\"",
        ]
    );
    // the checks in code come first, then the schema
    let no_summary =
        json!({"decision": "approved", "acceptance_criteria": [ac("AC-1")], "issues": []});
    assert_eq!(
        review_problems(&no_summary, &ids),
        [
            "missing acceptance criteria: [\"AC-2\"]",
            "$: missing required property `summary`",
        ]
    );
    assert_eq!(
        review_problems(&Value::Null, &ids)
            .last()
            .map(String::as_str),
        Some("$: expected object, got null")
    );
}

// ── EC-31: a read-only role writes nothing ───────────────────────────────────────────

#[test]
fn a_read_only_write_is_a_policy_violation() {
    let (a, b) = ("a".repeat(40), "b".repeat(40));
    assert_eq!(read_only_write(Some(&a), &a, &[]), None);
    assert_eq!(read_only_write(None, &a, &[]), None);
    assert_eq!(
        read_only_write(Some(&a), &a, &strings(&["src/Calculator.psm1", "notes.md"])),
        Some("policy_violation: the worktree changed during a read-only stage: [\"src/Calculator.psm1\", \"notes.md\"]".into())
    );
    // a commit (the tree is clean again) moved HEAD
    assert_eq!(
        read_only_write(Some(&a), &b, &[]),
        Some(
            "policy_violation: HEAD moved during a read-only stage (aaaaaaaaaaaa → bbbbbbbbbbbb)"
                .into()
        )
    );
    // both: the changed paths are named first
    assert!(read_only_write(Some(&a), &b, &strings(&["x"]))
        .unwrap()
        .contains("worktree changed"));
}

// ── EC-11: the final approval names the branch it approves ───────────────────────────

#[test]
fn an_approval_mismatch_names_the_branch_or_the_changes() {
    let (cp, other) = ("c".repeat(40), "f".repeat(40));
    assert_eq!(mismatch_reason("toryo/C-1", &cp, &cp, &[]), None);
    assert_eq!(
        mismatch_reason("toryo/C-1", &other, &cp, &[]),
        Some("approval_mismatch: the branch toryo/C-1 is at ffffffffffff, not at the approved checkpoint cccccccccccc — nothing was approved (R32)".into())
    );
    assert_eq!(
        mismatch_reason("toryo/C-1", "", &cp, &[]),
        Some("approval_mismatch: the branch toryo/C-1 is gone, not at the approved checkpoint cccccccccccc — nothing was approved (R32)".into())
    );
    assert_eq!(
        mismatch_reason("toryo/C-1", &cp, &cp, &strings(&["src/Calculator.psm1"])),
        Some("approval_mismatch: the worktree has uncommitted changes [\"src/Calculator.psm1\"] on top of the approved checkpoint cccccccccccc — nothing was approved (R32)".into())
    );
    // the reason is the escalation's §11 class
    assert_eq!(
        reason_class(&mismatch_reason("b", &other, &cp, &[]).unwrap(), ""),
        "approval_mismatch"
    );
}

// ── item 10: a relative verify program is a file of the checkout ─────────────────────

#[test]
fn a_relative_verify_program_is_anchored_at_the_checkout() {
    let co = std::env::temp_dir().join("toryo-verify").join("T-1.verify");
    let under = |parts: &[&str]| parts.iter().fold(co.clone(), |p, c| p.join(c));
    assert_eq!(
        verify_program("scripts/gate.sh", &co),
        under(&["scripts", "gate.sh"])
    );
    assert_eq!(
        verify_program("scripts\\build.cmd", &co),
        under(&["scripts", "build.cmd"])
    );
    assert_eq!(verify_program("./gradlew", &co), under(&["gradlew"]));
    assert_eq!(
        verify_program(".\\tools\\x.exe", &co),
        under(&["tools", "x.exe"])
    );
    assert_eq!(
        verify_program("../shared/gate.sh", &co),
        under(&["..", "shared", "gate.sh"])
    );
    // absolute programs are unchanged
    let abs = std::env::temp_dir().join("tools").join("gate");
    assert_eq!(verify_program(&abs.display().to_string(), &co), abs);
    // a bare name is a PATH lookup, as before (git is on PATH wherever the tests run)
    let git = verify_program("git", &co);
    assert_eq!(git, resolve_program("git"));
    assert!(!git.starts_with(&co), "{}", git.display());
    assert_eq!(
        verify_program("no-such-program-toryo", &co),
        PathBuf::from("no-such-program-toryo")
    );
}

#[cfg(windows)]
#[test]
fn a_rooted_or_drive_relative_program_is_not_anchored() {
    let co = PathBuf::from(r"C:\verify\T-1.verify");
    assert_eq!(
        verify_program(r"\tools\x.exe", &co),
        PathBuf::from(r"\tools\x.exe")
    );
    assert_eq!(
        verify_program(r"D:tools\x.exe", &co),
        PathBuf::from(r"D:tools\x.exe")
    );
}
