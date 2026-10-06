//! The Implementer path's decisions without the engine (no store, no agents): the HEAD guard
//! on every exit path (EC-18), a failed turn that left changes (EC-15) and the decision
//! recovery reads back after a crash (F8), `--no-fallback` after a sticky switch (EC-16) and
//! the interruptible rate-limit backoff (EC-17).

use super::*;
use crate::engine::state::Machine;
use std::time::Instant;
use ErrorClass as C;
use ImplementOutcome as O;

const BEFORE: &str = "1111111111111111111111111111111111111111";
const MOVED: &str = "2222222222222222222222222222222222222222";
const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const TASK_REF: &str = "refs/heads/toryo/T-1";

fn outcome(success: bool, class: Option<ErrorClass>, detail: Option<&str>) -> StageOutcome {
    StageOutcome {
        success,
        error_class: class,
        error_detail: detail.map(str::to_string),
        ..Default::default()
    }
}

fn attempt(stage: &str, n: u32, status: AttemptStatus, partial: Option<&str>) -> AttemptRow {
    AttemptRow {
        id: format!("T-1/{stage}/{n}"),
        task_id: "T-1".into(),
        stage: stage.into(),
        attempt: n,
        kind: "agent".into(),
        fallback_from: None,
        profile: Some("codex-openai".into()),
        external_session_id: None,
        pid: None,
        pid_created_at: None,
        status,
        error_class: None,
        error_detail: None,
        git_head_before: None,
        git_head_after: None,
        subject_commit: None,
        partial_commit: partial.map(str::to_string),
        checkpoint_kind: None,
        partial_snapshot_id: None,
        correction_turns: 0,
        started_at: String::new(),
        ended_at: None,
        context_sha256: None,
    }
}

fn task(implementers: &[&str], index: usize) -> TaskRow {
    let machine = Machine::new(3);
    TaskRow {
        id: "T-1".into(),
        title: "t".into(),
        state: machine.state(),
        task_sha256: String::new(),
        acceptance_ids: vec!["AC-1".into()],
        run_store: PathBuf::from("runs").join("T-1"),
        config_sha256: String::new(),
        lock_generation: 1,
        fix_cycle: 0,
        implementer_index: index,
        worktree: PathBuf::from("wts").join("T-1"),
        branch: "toryo/T-1".into(),
        base_commit: BASE.into(),
        last_reviewed_head: None,
        spent_tokens: 0,
        reserved_tokens: 0,
        run_lock_owner: None,
        run_lock_at: None,
        created_at: String::new(),
        updated_at: String::new(),
        repo: PathBuf::from("repo"),
        task_file: PathBuf::from("repo").join("TASK.md"),
        machine,
        implementers: implementers.iter().map(|s| s.to_string()).collect(),
        planner: "claude-max".into(),
        reviewer: "claude-max".into(),
        feedback: None,
        handoff: None,
        budget_tokens: None,
    }
}

fn git(unfinished: Option<&str>, head_ref: Option<&str>, head: Option<&str>) -> GitAfterTurn {
    GitAfterTurn {
        unfinished: unfinished.map(str::to_string),
        head_ref: head_ref.map(str::to_string),
        head: head.map(str::to_string),
        descends_from_base: true,
    }
}

fn violation(kind: &'static str, what: &str, fix: &str) -> HeadGuard {
    HeadGuard::Violation {
        kind,
        reason: format!("policy_violation: after the Implementer turn {what}; nothing was reset or committed{fix}"),
    }
}

// ── EC-18: the HEAD guard ──────────────────────────────────────────────────────────────

#[test]
fn the_head_guard_resets_only_an_agent_commit_on_the_task_branch() {
    // M13-01: the way out names the resume that undoes the commits made since the anchor
    let back = " — check out toryo/T-1 again; `toryo resume T-1 --note \"…\"` then undoes the commits made since 111111111111 into the tree and checks them";
    // M13-R8: after the owner checks the task branch out HEAD is at the anchor again and nothing
    // is undone — what the agent committed on the other branch stays there
    let other = " — check out toryo/T-1 again; `toryo resume T-1 --note \"…\"` then starts at 111111111111 (what was committed on the other branch stays there)";
    let rewritten = GitAfterTurn {
        descends_from_base: false,
        ..git(None, Some(TASK_REF), Some(MOVED))
    };
    let cases = [
        (git(None, Some(TASK_REF), Some(BEFORE)), HeadGuard::Unchanged),
        // R12: an agent commit on the task branch → soft reset, the changes stay
        (
            git(None, Some(TASK_REF), Some(MOVED)),
            HeadGuard::Reset { head: MOVED.into() },
        ),
        // R25: history rewritten below the base cannot be repaired by a reset
        (
            rewritten,
            violation(
                "history rewritten",
                "HEAD 222222222222 no longer descends from the task base bbbbbbbbbbbb (history rewritten, R25)",
                "",
            ),
        ),
        // R25: an unfinished operation comes first — whatever HEAD and the branch are
        (
            git(Some("MERGE_HEAD"), Some(TASK_REF), Some(BEFORE)),
            violation(
                "unfinished git operation",
                "an unfinished Git operation (MERGE_HEAD) is in the task worktree (R25)",
                " — finish or abort it first",
            ),
        ),
        (
            git(Some("rebase-merge"), None, Some(MOVED)),
            violation(
                "unfinished git operation",
                "an unfinished Git operation (rebase-merge) is in the task worktree (R25)",
                " — finish or abort it first",
            ),
        ),
        // R12: another branch is never reset (the reset would move that branch), moved or not
        (
            git(None, Some("refs/heads/evil"), Some(MOVED)),
            violation(
                "branch changed",
                "HEAD is on refs/heads/evil, not on the task branch toryo/T-1 (R12)",
                other,
            ),
        ),
        (
            git(None, Some("refs/heads/evil"), Some(BEFORE)),
            violation(
                "branch changed",
                "HEAD is on refs/heads/evil, not on the task branch toryo/T-1 (R12)",
                other,
            ),
        ),
        // a branch whose name merely starts with the task branch's is another branch
        (
            git(None, Some("refs/heads/toryo/T-10"), Some(BEFORE)),
            violation(
                "branch changed",
                "HEAD is on refs/heads/toryo/T-10, not on the task branch toryo/T-1 (R12)",
                other,
            ),
        ),
        (
            git(None, None, Some(BEFORE)),
            violation(
                "detached HEAD",
                "HEAD is detached at 111111111111, not on the task branch toryo/T-1 (R12)",
                back,
            ),
        ),
        (
            git(None, None, None),
            violation(
                "detached HEAD",
                "HEAD is detached at ?, not on the task branch toryo/T-1 (R12)",
                back,
            ),
        ),
        (
            git(None, Some(TASK_REF), None),
            violation(
                "unresolved HEAD",
                "the task branch toryo/T-1 does not resolve to a commit",
                "",
            ),
        ),
    ];
    for (g, want) in cases {
        assert_eq!(HeadGuard::of(&g, "toryo/T-1", BASE, BEFORE), want, "{g:?}");
    }
}

#[test]
fn a_git_violation_is_an_escalation_of_the_policy_violation_class() {
    let HeadGuard::Violation { reason, .. } = HeadGuard::of(
        &git(Some("MERGE_HEAD"), None, None),
        "toryo/T-1",
        BASE,
        BEFORE,
    ) else {
        panic!("a violation");
    };
    // the attempt's class and the escalation both read `policy_violation`
    assert_eq!(reason_class(&reason, "x"), "policy_violation");
    assert_eq!(
        O::GitViolation { reason }.retries_after(2),
        0,
        "a violation starts the rate-limit count over"
    );
}

// ── EC-15: a failed turn that left changes ─────────────────────────────────────────────

#[test]
fn a_failed_turn_that_left_changes_runs_its_stage_again_once() {
    let protect = Protected::new(vec![], &[]).with_policy(&[], &["**/ZZZ/**".to_string()]);
    let changed = vec![(" M".to_string(), "src/a.ps1".to_string())];
    let failed = |class: Option<ErrorClass>, failed_before: bool| {
        O::after_checks(
            &outcome(false, class, Some("d")),
            &changed,
            &protect,
            failed_before,
        )
    };
    let retried = [
        None,
        Some(C::Unknown),
        Some(C::ProcessCrashed),
        Some(C::Timeout),
        Some(C::ContextWindowExceeded),
        Some(C::Network),
        Some(C::InvalidOutput),
    ];
    for class in retried {
        let reason = match class {
            Some(c) => format!("implementer failed: {c:?} — d"),
            None => "implementer failed: unknown error — d".to_string(),
        };
        assert_eq!(
            failed(class, false),
            O::FailedWithChanges {
                reason: reason.clone(),
                retry: true
            },
            "{class:?}"
        );
        // the stage already ran again after such a failure (§11: once) → escalate
        assert_eq!(
            failed(class, true),
            O::FailedWithChanges {
                reason,
                retry: false
            },
            "{class:?} again"
        );
    }
    // §11: no blind retry after a denial, no retry after auth / bad request / refusal
    for class in [
        C::PermissionDenied,
        C::Auth,
        C::BadRequest,
        C::PolicyRefusal,
    ] {
        assert_eq!(
            failed(Some(class), false),
            O::FailedWithChanges {
                reason: format!("implementer failed: {class:?} — d"),
                retry: false
            },
            "{class:?}"
        );
    }
    // unchanged: a failed turn without changes escalates, whatever came before; the path
    // policy comes first; a successful turn goes on to the checkpoint
    for failed_before in [false, true] {
        assert_eq!(
            O::after_checks(
                &outcome(false, Some(C::Unknown), Some("d")),
                &[],
                &protect,
                failed_before
            ),
            O::Failed {
                reason: "implementer failed: Unknown — d".into()
            }
        );
        let denied = vec![("??".to_string(), "x/ZZZ/s.txt".to_string())];
        assert_eq!(
            O::after_checks(
                &outcome(false, Some(C::Unknown), None),
                &denied,
                &protect,
                failed_before
            ),
            O::PathPolicy {
                paths: vec!["x/ZZZ/s.txt".into()]
            }
        );
        assert_eq!(
            O::after_checks(
                &outcome(true, None, None),
                &changed,
                &protect,
                failed_before
            ),
            O::Done
        );
    }
    assert_eq!(
        O::FailedWithChanges {
            reason: "r".into(),
            retry: true
        }
        .retries_after(1),
        0
    );
}

#[test]
fn only_a_failed_attempt_of_the_stage_with_a_partial_commit_counts_as_run_again() {
    use AttemptStatus::{Failed, Interrupted, Succeeded};
    let p = Some("3333333333333333333333333333333333333333");
    let cases = [
        (vec![], "implement", false),
        (vec![attempt("implement", 1, Failed, p)], "implement", true),
        // another stage's failure does not count
        (vec![attempt("implement", 1, Failed, p)], "fix-1", false),
        // a rate-limited or switched attempt left no partial commit
        (
            vec![attempt("implement", 1, Failed, None)],
            "implement",
            false,
        ),
        // a recovery checkpoint after a crash (§10) is not a failed turn
        (
            vec![attempt("implement", 1, Interrupted, p)],
            "implement",
            false,
        ),
        (
            vec![attempt("implement", 1, Succeeded, None)],
            "implement",
            false,
        ),
        // a rate-limited attempt in between does not hide the earlier failure
        (
            vec![
                attempt("implement", 1, Failed, p),
                attempt("implement", 2, Failed, None),
            ],
            "implement",
            true,
        ),
    ];
    for (atts, stage, want) in cases {
        assert_eq!(
            failed_with_changes_before(&atts, stage),
            want,
            "{stage} after {:?}",
            atts.iter()
                .map(|a| (a.id.as_str(), a.status, a.partial_commit.is_some()))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn the_failed_turn_note_names_the_error_and_the_recovery_checkpoint() {
    let a = attempt("implement", 1, AttemptStatus::Failed, None);
    let sha = "4444444444444444444444444444444444444444";
    let note = |why: &str| {
        format!("\n## Recovery\nThe previous attempt (T-1/implement/1 by `codex-openai`) failed before it finished ({why}). Its uncommitted work was saved as the recovery checkpoint 444444444444 (trailer `Toryo-Partial: true`) — the worktree starts from it. Check that work, keep what is correct, fix or complete the rest, and finish the task.\n")
    };
    let long = "x".repeat(400);
    let cases = [
        (
            Some("Unknown"),
            Some("the agent died half way"),
            note("Unknown: the agent died half way"),
        ),
        (Some("Timeout"), None, note("Timeout")),
        (Some("Timeout"), Some("  "), note("Timeout")),
        // only the first line of the detail, at most 300 characters
        (
            Some("Network"),
            Some("stream closed\n{\"json\": 1}"),
            note("Network: stream closed"),
        ),
        (
            Some("Unknown"),
            Some(long.as_str()),
            note(&format!("Unknown: {}", "x".repeat(300))),
        ),
        (None, Some("exit 3"), note("exit 3")),
        (None, None, note("no error class")),
    ];
    for (class, detail, want) in cases {
        assert_eq!(
            failed_turn_note(&a, class, detail, sha),
            want,
            "{class:?} {detail:?}"
        );
    }
}

// ── F8: the failed turn's decision survives a crash before the attempt ends ─────────────

#[test]
fn a_failed_turn_records_what_recovery_needs_to_end_its_attempt_the_same_way() {
    let a = attempt("implement", 2, AttemptStatus::Running, None);
    let sha = "5555555555555555555555555555555555555555";
    let failed = |retry: bool| FailedTurn {
        attempt: a.id.clone(),
        error_class: Some("Timeout".into()),
        error_detail: Some("second".into()),
        reason: "implementer failed: Timeout — second".into(),
        retry,
    };
    // the event payload round-trips (recovery reads it back)
    let f = failed(false);
    let payload = serde_json::to_value(&f).unwrap();
    assert_eq!(
        payload,
        json!({"attempt": "T-1/implement/2", "error_class": "Timeout", "error_detail": "second", "reason": "implementer failed: Timeout — second", "retry": false})
    );
    assert_eq!(serde_json::from_value::<FailedTurn>(payload).unwrap(), f);
    // what the attempt records, whoever ends it: `failed`, the class, the partial commit
    let end = f.end(sha);
    assert_eq!(
        (
            end.error_class.as_deref(),
            end.error_detail.as_deref(),
            end.partial_commit.as_deref(),
            end.head_after,
        ),
        (Some("Timeout"), Some("second"), Some(sha), None)
    );
    // the stage runs again, or the task escalates with the kept work named
    assert_eq!(failed(true).escalation(sha), None);
    assert_eq!(
        f.escalation(sha).as_deref(),
        Some("implementer failed: Timeout — second; its partial work is kept in the recovery checkpoint 555555555555 (Toryo-Partial)")
    );
    assert!(f.note(&a, sha).contains(
        "(T-1/implement/2 by `codex-openai`) failed before it finished (Timeout: second)"
    ));
    let t = task(&["codex-openai"], 0);
    assert_eq!(
        FailedTurn::message(&t, &a),
        "toryo(T-1): implement (recovery checkpoint of failed attempt 2)"
    );
    assert_eq!(
        partial_trailers(&t, &a),
        vec![
            (T_TASK, "T-1".to_string()),
            (T_STAGE, "implement".to_string()),
            (T_ATTEMPT, "2".to_string()),
            (T_AGENT, "codex-openai".to_string()),
            (T_PARTIAL, "true".to_string()),
        ]
    );
}

// ── EC-16: --no-fallback after a sticky switch ─────────────────────────────────────────

#[test]
fn no_fallback_keeps_the_sticky_implementer_and_forbids_the_next_switch() {
    let chain = ["codex-openai", "claude-max", "codex-backup"];
    // (current implementer_index, --no-fallback, implementer now, the one a switch goes to)
    let cases = [
        (0, false, Some("codex-openai"), Some("claude-max")),
        (0, true, Some("codex-openai"), None),
        (1, false, Some("claude-max"), Some("codex-backup")),
        (1, true, Some("claude-max"), None),
        (2, false, Some("codex-backup"), None),
        (2, true, Some("codex-backup"), None),
        (3, true, None, None),
    ];
    for (current, no_fallback, now, next) in cases {
        let t = task(&chain, current);
        assert_eq!(
            (
                implementer_at(&t, current, no_fallback).as_deref(),
                implementer_at(&t, current + 1, no_fallback).as_deref()
            ),
            (now, next),
            "current {current}, --no-fallback {no_fallback}"
        );
    }
}

// ── EC-17: the rate-limit backoff ends at a stop request ───────────────────────────────

#[test]
fn the_backoff_ends_as_soon_as_the_stop_flag_is_set() {
    let stop = Arc::new(AtomicBool::new(false));
    // no stop: the whole wait (and no wait at all for 0 s, the tests' TORYO_RETRY_BASE_SECS)
    let t0 = Instant::now();
    assert!(!backoff(&stop, Duration::ZERO));
    assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
    let t0 = Instant::now();
    assert!(!backoff(&stop, Duration::from_millis(150)));
    assert!(t0.elapsed() >= Duration::from_millis(150));
    // a stop during the wait ends it at once — even a wait `Instant` cannot represent
    let t0 = Instant::now();
    let flag = stop.clone();
    let setter = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        flag.store(true, Ordering::SeqCst);
    });
    assert!(backoff(&stop, Duration::MAX));
    let took = t0.elapsed();
    setter.join().unwrap();
    assert!(
        took >= Duration::from_millis(100) && took < Duration::from_secs(5),
        "{took:?}"
    );
    // a stop that came before the wait: no wait at all
    for wait in [Duration::from_secs(30), Duration::MAX] {
        let t0 = Instant::now();
        assert!(backoff(&stop, wait));
        assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
    }
}

// ── M13-01 (X24): the anchor ───────────────────────────────────────────────────────────

/// The anchor is the newest commit Toryo recorded for the task — a checkpoint or a recovery
/// checkpoint — else the task's base commit. Nothing else is read (no HEAD, no author).
#[test]
fn the_anchor_is_the_last_commit_toryo_recorded() {
    let t = task(&["codex-openai"], 0);
    assert_eq!(anchor_of(&t, &[]), t.base_commit);
    let plan = attempt("plan", 1, AttemptStatus::Succeeded, None);
    assert_eq!(anchor_of(&t, std::slice::from_ref(&plan)), t.base_commit);
    let mut first = attempt("implement", 1, AttemptStatus::Succeeded, None);
    first.git_head_after = Some(BEFORE.into());
    let verify = attempt("verify", 1, AttemptStatus::Failed, None);
    let atts = [plan.clone(), first.clone(), verify.clone()];
    assert_eq!(anchor_of(&t, &atts), BEFORE);
    // a later recovery checkpoint is newer than the checkpoint before it
    let partial = attempt("fix-1", 1, AttemptStatus::Interrupted, Some(MOVED));
    let running = attempt("fix-1", 2, AttemptStatus::Running, None);
    assert_eq!(
        anchor_of(&t, &[plan, first, verify, partial, running]),
        MOVED
    );
}
