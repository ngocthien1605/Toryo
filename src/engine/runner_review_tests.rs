//! The Planner/Reviewer turn decisions, the gates and the verify helpers of the 2026-09-29
//! review fixes (WP-C2b) and their follow-ups — §11 per class (F7), writes during a read-only
//! turn only (F5), a dirty worktree at the final approval is a note (F6): pure functions, no
//! store, git or agents. The end-to-end regression tests are in `tests/regress_engine.rs`.

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
        // EC-14: crash, timeout, network, unknown → run again as §11 says (bounded)
        (Some(C::ProcessCrashed), Retry(C::ProcessCrashed)),
        (Some(C::Timeout), Retry(C::Timeout)),
        (Some(C::Network), Retry(C::Network)),
        (Some(C::Unknown), Retry(C::Unknown)),
        (None, Retry(C::Unknown)),
        // F7: a context overflow gets a new session for the stage once (was: escalated)
        (
            Some(C::ContextWindowExceeded),
            Retry(C::ContextWindowExceeded),
        ),
        // no blind retry: escalate with the real class (§11)
        (Some(C::PermissionDenied), Failed(C::PermissionDenied)),
        (Some(C::PolicyRefusal), Failed(C::PolicyRefusal)),
        (Some(C::Auth), Failed(C::Auth)),
        (Some(C::BadRequest), Failed(C::BadRequest)),
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
        "ProcessCrashed: the reviewer turn failed (event stream ended without a terminal event), also after 2 retries (plan §11)"
    );
    assert_eq!(
        read_only_failure("planner", C::Timeout, Some(""), 1),
        "Timeout: the planner turn failed (no detail), also after 1 retry (plan §11)"
    );
    assert!(read_only_failure("planner", C::PolicyRefusal, None, 0)
        .starts_with("PolicyRefusal: the planner turn failed (no detail);"));
    let r = turn(false, Some(C::Network), Some("stream disconnected"));
    assert_eq!(
        retry_note("planner", C::Network, &r, Rerun::Resume, 1),
        "!! planner turn failed: Network (stream disconnected) — the same turn again, resuming its session (1/2, plan §11)"
    );
    assert_eq!(
        retry_note("reviewer", C::Timeout, &turn(false, Some(C::Timeout), None), Rerun::Fresh, 1),
        "!! reviewer turn failed: Timeout (no detail) — the stage again in a fresh session (1/1, plan §11)"
    );
}

// ── F7: plan §11 per class — how often, and resumed or fresh ─────────────────────────

#[test]
fn each_class_is_retried_as_plan_section_11_says() {
    use Rerun::{Fresh, Resume};
    // (class, earlier retries of the class, retries of the attempt, what happens)
    let cases = [
        // ProcessCrashed and Network: resume within the stage once, then a new attempt, then
        // escalate
        (C::ProcessCrashed, 0, 0, Some(Resume)),
        (C::ProcessCrashed, 1, 1, Some(Fresh)),
        (C::ProcessCrashed, 2, 2, None),
        (C::Network, 0, 0, Some(Resume)),
        (C::Network, 1, 1, Some(Fresh)),
        (C::Network, 2, 2, None),
        // Unknown: like ProcessCrashed, once
        (C::Unknown, 0, 0, Some(Resume)),
        (C::Unknown, 1, 1, None),
        // Timeout: a new attempt once (was: the same turn, twice)
        (C::Timeout, 0, 0, Some(Fresh)),
        (C::Timeout, 1, 1, None),
        // ContextWindowExceeded: a new session for the same stage, no resume, once (was: none)
        (C::ContextWindowExceeded, 0, 0, Some(Fresh)),
        (C::ContextWindowExceeded, 1, 1, None),
        // no retry at all
        (C::PermissionDenied, 0, 0, None),
        (C::Auth, 0, 0, None),
        (C::BadRequest, 0, 0, None),
        (C::PolicyRefusal, 0, 0, None),
        (C::InvalidOutput, 0, 0, None),
        // mixed classes: each has its own allowance, the attempt at most two retries
        (C::Timeout, 0, 1, Some(Fresh)),
        (C::ProcessCrashed, 0, 1, Some(Resume)),
        (C::ProcessCrashed, 1, 2, None),
        (C::Unknown, 0, 2, None),
    ];
    for (class, earlier, total, want) in cases {
        assert_eq!(
            rerun(class, earlier, total),
            want,
            "{class:?} after {earlier} of its own, {total} in all"
        );
    }
    let most = [
        C::ProcessCrashed,
        C::Network,
        C::Unknown,
        C::Timeout,
        C::ContextWindowExceeded,
    ]
    .map(class_retries)
    .into_iter()
    .max();
    assert_eq!(
        most,
        Some(MAX_TURN_RETRIES),
        "the attempt bound is the largest allowance"
    );
}

#[test]
fn the_retries_of_an_attempt_count_per_class_and_in_all() {
    use Rerun::{Fresh, Resume};
    let run = |classes: &[ErrorClass]| {
        let mut r = TurnRetries::default();
        let got: Vec<Option<Rerun>> = classes.iter().map(|c| r.next(*c)).collect();
        (got, r.total())
    };
    // the reviewer's three crashes: resumed, fresh, then escalated with the class
    assert_eq!(
        run(&[C::ProcessCrashed, C::ProcessCrashed, C::ProcessCrashed]),
        (vec![Some(Resume), Some(Fresh), None], 2)
    );
    assert_eq!(run(&[C::Timeout, C::Timeout]), (vec![Some(Fresh), None], 1));
    assert_eq!(
        run(&[C::Unknown, C::Unknown]),
        (vec![Some(Resume), None], 1)
    );
    assert_eq!(
        run(&[C::ContextWindowExceeded, C::ContextWindowExceeded]),
        (vec![Some(Fresh), None], 1)
    );
    // a crash after a timeout still resumes first; a third failure escalates
    assert_eq!(
        run(&[C::Timeout, C::ProcessCrashed, C::Network]),
        (vec![Some(Fresh), Some(Resume), None], 2)
    );
    // a refused retry is not counted
    assert_eq!(run(&[C::Auth, C::Network]), (vec![None, Some(Resume)], 1));
}

#[test]
fn a_rerun_resumes_the_failed_turns_session_or_starts_the_stage_fresh() {
    let crashed = StageOutcome {
        session_id: Some("s-2".into()),
        ..turn(false, Some(C::ProcessCrashed), None)
    };
    let silent = turn(false, Some(C::ProcessCrashed), None);
    let ask = |how: Rerun, r: &StageOutcome, resume: Option<&str>| {
        how.ask(r, "the correction", resume, "the review")
    };
    // resumed: the same prompt, in the session the failed turn reported …
    assert_eq!(
        ask(Rerun::Resume, &crashed, Some("s-1")),
        ("the correction".to_string(), Some("s-2".to_string()))
    );
    // … or, if it never reported one, in the session it was asked in
    assert_eq!(
        ask(Rerun::Resume, &silent, Some("s-1")),
        ("the correction".to_string(), Some("s-1".to_string()))
    );
    assert_eq!(
        ask(Rerun::Resume, &silent, None),
        ("the correction".to_string(), None)
    );
    // fresh: no session, the stage's first prompt (never a correction without its session)
    assert_eq!(
        ask(Rerun::Fresh, &crashed, Some("s-1")),
        ("the review".to_string(), None)
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

/// A tree at `head` with `(path, status, hash)` entries.
fn tree(head: Option<&str>, entries: &[(&str, &str, &str)]) -> TreeState {
    TreeState {
        head: head.map(str::to_string),
        entries: entries
            .iter()
            .map(|(p, s, h)| (p.to_string(), (s.to_string(), h.to_string())))
            .collect(),
    }
}

#[test]
fn a_read_only_write_is_a_policy_violation() {
    let (a, b) = ("a".repeat(40), "b".repeat(40));
    let clean = tree(Some(&a), &[]);
    assert_eq!(read_only_write(&clean, &clean), None);
    assert_eq!(read_only_write(&tree(None, &[]), &tree(None, &[])), None);
    assert_eq!(
        read_only_write(
            &clean,
            &tree(
                Some(&a),
                &[("src/Calculator.psm1", " M", "h1"), ("notes.md", "??", "h2")]
            )
        ),
        Some("policy_violation: the worktree changed during a read-only stage: [\"notes.md\", \"src/Calculator.psm1\"]".into())
    );
    // a commit (the tree is clean again) moved HEAD
    assert_eq!(
        read_only_write(&clean, &tree(Some(&b), &[])),
        Some(
            "policy_violation: HEAD moved during a read-only stage (aaaaaaaaaaaa → bbbbbbbbbbbb)"
                .into()
        )
    );
    assert_eq!(
        read_only_write(&clean, &tree(None, &[])),
        Some("policy_violation: HEAD moved during a read-only stage (aaaaaaaaaaaa → ?)".into())
    );
    // both: the changed paths are named first
    assert!(
        read_only_write(&clean, &tree(Some(&b), &[("x", "??", "h")]))
            .unwrap()
            .contains("worktree changed")
    );
}

/// F5: what was in the worktree before the turn (a human's notes, an IDE folder) is not the
/// read-only role's write — only what is new, gone or different after it.
#[test]
fn only_what_changed_during_the_read_only_turn_counts() {
    let h = "a".repeat(40);
    let before = tree(
        Some(&h),
        &[
            ("review-notes.txt", "??", "n1"),
            (".vs/toryo/v17/.suo", "??", "v1"),
            ("src/Calculator.psm1", " M", "c1"),
        ],
    );
    // unchanged: no violation (was: `policy_violation` for every one of them)
    assert_eq!(read_only_write(&before, &before.clone()), None);
    let with = |path: &str, status: &str, hash: &str| {
        let mut t = before.clone();
        t.entries
            .insert(path.to_string(), (status.to_string(), hash.to_string()));
        t
    };
    let without = |path: &str| {
        let mut t = before.clone();
        t.entries.remove(path);
        t
    };
    let violation = |paths: &str| {
        Some(format!(
            "policy_violation: the worktree changed during a read-only stage: {paths}"
        ))
    };
    let cases = [
        // a new file
        (with("draft.md", "??", "d1"), violation("[\"draft.md\"]")),
        // a file the human left there, modified
        (
            with("review-notes.txt", "??", "n2"),
            violation("[\"review-notes.txt\"]"),
        ),
        // … staged (the status changed, the content did not)
        (
            with("review-notes.txt", "A ", "n1"),
            violation("[\"review-notes.txt\"]"),
        ),
        // … reverted or deleted: gone from the changed paths
        (
            without("src/Calculator.psm1"),
            violation("[\"src/Calculator.psm1\"]"),
        ),
    ];
    for (after, want) in cases {
        assert_eq!(read_only_write(&before, &after), want, "{after:?}");
    }
}

#[test]
fn a_changed_path_is_compared_by_its_content() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("notes.txt");
    std::fs::write(&f, "one\n").unwrap();
    let one = content_hash(&f);
    assert_eq!(one, sha256_hex(b"one\n"), "the SHA-256 of the bytes");
    std::fs::write(&f, "two\n").unwrap();
    assert_ne!(content_hash(&f), one);
    std::fs::write(&f, "one\n").unwrap();
    assert_eq!(content_hash(&f), one, "the same bytes, the same hash");
    assert_eq!(content_hash(&dir.path().join("gone.txt")), "absent");
    assert_eq!(content_hash(dir.path()), "directory");
    #[cfg(unix)]
    {
        let l = dir.path().join("link");
        std::os::unix::fs::symlink("notes.txt", &l).unwrap();
        assert_eq!(content_hash(&l), "link notes.txt", "never followed");
    }
}

// ── EC-11: the final approval names the branch it approves ───────────────────────────

#[test]
fn an_approval_mismatch_names_the_branch() {
    let (cp, other) = ("c".repeat(40), "f".repeat(40));
    assert_eq!(mismatch_reason("toryo/C-1", &cp, &cp), None);
    assert_eq!(
        mismatch_reason("toryo/C-1", &other, &cp),
        Some("approval_mismatch: the branch toryo/C-1 is at ffffffffffff, not at the approved checkpoint cccccccccccc — nothing was approved (R32)".into())
    );
    assert_eq!(
        mismatch_reason("toryo/C-1", "", &cp),
        Some("approval_mismatch: the branch toryo/C-1 is gone, not at the approved checkpoint cccccccccccc — nothing was approved (R32)".into())
    );
    // the reason is the escalation's §11 class
    assert_eq!(
        reason_class(&mismatch_reason("b", &other, &cp).unwrap(), ""),
        "approval_mismatch"
    );
}

/// F6 (owner decision): uncommitted changes at the final approval — an IDE folder in the
/// worktree — are noted in one line, never an `approval_mismatch`.
#[test]
fn uncommitted_changes_at_the_final_approval_are_a_note() {
    let cp = "c".repeat(40);
    assert_eq!(uncommitted_note(&[], &cp), None);
    assert_eq!(
        uncommitted_note(&strings(&["src/Extra.ps1"]), &cp),
        Some("note: the worktree has uncommitted changes (src/Extra.ps1) — they are not part of the approved checkpoint cccccccccccc and stay uncommitted".into())
    );
    let vs = strings(&[".vs/a.suo", ".vs/b.db", ".vs/c.json", ".vs/d.json", "x.txt"]);
    let note = uncommitted_note(&vs, &cp).unwrap();
    assert_eq!(
        note,
        "note: the worktree has uncommitted changes (.vs/a.suo, .vs/b.db, .vs/c.json, … (5 in all)) — they are not part of the approved checkpoint cccccccccccc and stay uncommitted"
    );
    assert!(!note.contains('\n'), "one line");
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

/// An IDE opened on the task worktree during a read-only turn (Visual Studio `.vs/`,
/// JetBrains `.idea/`) is not the role's write; anything else next to it still is.
#[test]
fn an_ide_state_folder_created_during_a_read_only_turn_is_not_a_write() {
    let h = "a".repeat(40);
    let before = tree(Some(&h), &[]);
    let ide = tree(
        Some(&h),
        &[
            (".vs/toryo/v17/.suo", "??", "v1"),
            (".VS/ProjectSettings.json", "??", "v2"),
            (".idea/workspace.xml", "??", "i1"),
        ],
    );
    assert_eq!(read_only_write(&before, &ide), None);
    let mixed = tree(
        Some(&h),
        &[(".vs/toryo/v17/.suo", "??", "v1"), ("notes.md", "??", "n1")],
    );
    assert_eq!(
        read_only_write(&before, &mixed),
        Some(
            "policy_violation: the worktree changed during a read-only stage: [\"notes.md\"]"
                .into()
        )
    );
    // a folder that only starts like one is not an IDE folder
    let other = tree(Some(&h), &[(".vscode-notes/x.md", "??", "x1")]);
    assert!(read_only_write(&before, &other).is_some());
    assert!(!ide_state(".vsix/a") && ide_state(".vs") && ide_state(".idea\\x"));
}

/// Visual Studio puts `.vs/` next to the `.sln`, often below the worktree root in a D365FO
/// repository: an IDE folder at any depth is not a read-only role's write.
#[test]
fn an_ide_state_folder_at_any_depth_is_not_a_write() {
    let h = "a".repeat(40);
    let before = tree(Some(&h), &[]);
    let nested = tree(
        Some(&h),
        &[
            (
                "Projects/ContosoModel/.vs/ContosoModel/v17/.suo",
                "??",
                "v1",
            ),
            ("src\\Tools\\.idea\\workspace.xml", "??", "i1"),
        ],
    );
    assert_eq!(read_only_write(&before, &nested), None);
    assert!(ide_state("a/b/.VS/c") && !ide_state("a/.vsx/c") && !ide_state("docs/vs/c"));
}

/// A planner turn retried in a new session (plan §11) names the plan being revised; without
/// a revision (or without a plan yet) it is the stage's first prompt.
#[test]
fn a_fresh_plan_revision_names_the_plan_being_revised() {
    let (plan, task) = (Path::new("R/PLAN.md"), Path::new("R/TASK.md"));
    let feedback = "The human reviewer rejected the plan. Reason: smaller\nCreate the plan again, revised to address this.";
    let p = plan_fresh_prompt(Some(feedback), Some(plan), task);
    assert!(p.starts_with(feedback), "{p}");
    assert!(
        p.contains(&plan.display().to_string()) && p.contains(&task.display().to_string()),
        "{p}"
    );
    assert_eq!(
        plan_fresh_prompt(Some(feedback), None, task),
        plan_prompt(Some(feedback), None, task)
    );
    assert_eq!(
        plan_fresh_prompt(None, Some(plan), task),
        plan_prompt(None, None, task)
    );
}

/// M13-06: the copy of what a read-only role wrote — a regular file of at most 8 MB is copied;
/// a larger one, a link (never followed) and a folder are not, and the reason says why; a path
/// that is gone is nothing to copy.
#[test]
fn the_copy_of_a_read_only_write_is_capped_and_never_follows_a_link() {
    let d = tempfile::tempdir().unwrap();
    let (from, to) = (d.path().join("from"), d.path().join("keep"));
    std::fs::create_dir_all(&from).unwrap();
    std::fs::write(from.join("small.txt"), "x").unwrap();
    assert_eq!(
        keep_copy(&from.join("small.txt"), &to.join("a/small.txt")),
        None
    );
    assert_eq!(
        std::fs::read_to_string(to.join("a/small.txt")).unwrap(),
        "x"
    );
    // 8 MB + 1 byte (a sparse file: only its length is set)
    let big = from.join("big.bin");
    std::fs::File::create(&big)
        .unwrap()
        .set_len(READ_ONLY_COPY_MAX + 1)
        .unwrap();
    let why = keep_copy(&big, &to.join("big.bin")).expect("not copied");
    assert!(why.contains("over the 8 MB"), "{why}");
    assert!(!to.join("big.bin").exists());
    assert_eq!(
        keep_copy(&from.join("gone.txt"), &to.join("gone.txt")),
        None
    );
    assert!(!to.join("gone.txt").exists());
    assert_eq!(
        keep_copy(&from, &to.join("dir")).as_deref(),
        Some("not a regular file")
    );
    // a link to a file outside: the link is not followed, nothing is copied
    let outside = d.path().join("outside.txt");
    std::fs::write(&outside, "the owner's").unwrap();
    let link = from.join("link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    #[cfg(windows)]
    if std::os::windows::fs::symlink_file(&outside, &link).is_err() {
        eprintln!("skipped the link case: this account may not create symbolic links");
        return;
    }
    assert_eq!(
        keep_copy(&link, &to.join("link")).as_deref(),
        Some("a link: not followed")
    );
    assert!(!to.join("link").exists());
    // the first link on the way to a path is found, the path itself is not asked
    let root = d.path();
    assert_eq!(link_on_the_way(root, "from/small.txt"), None);
    assert_eq!(link_on_the_way(root, "from/link"), None);
    assert_eq!(
        link_on_the_way(root, "from/link/inner.txt").as_deref(),
        Some("from/link")
    );
}
