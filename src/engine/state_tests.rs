//! M2 acceptance (plan §17): invalid transitions rejected; implement blocked before plan
//! approval; verify failure → Fixing, never Review; max cycles → Escalated; Escalated →
//! Fixing with a note; Paused → previous state; a rejected gate occurrence never passes.

use super::*;
use PipelineEvent as E;
use PipelineState as S;

const C1: &str = "c1";

/// A machine forced into `s`, with a context in which every guard can pass.
fn at(s: S) -> Machine {
    let mut m = Machine::new(2);
    m.state = s;
    m.plan_attempt = 1;
    if !matches!(s, S::New | S::Planning | S::WaitingPlanApproval) {
        m.plan_approved = true;
        m.checkpoint = Some(C1.into());
        m.verified_head = Some(C1.into());
    }
    match s {
        S::WaitingPlanApproval => m.open_gate(GateKind::Plan, "p".into()),
        S::WaitingFinalApproval => m.open_gate(GateKind::Final, C1.into()),
        S::Paused => m.paused_from = Some(S::Implementing),
        S::Escalated => m.escalated_from = Some(S::Implementing),
        _ => {}
    }
    m
}

fn samples() -> Vec<E> {
    let s = |x: &str| x.to_string();
    vec![
        E::Start,
        E::PlanPolicyViolation {
            paths: vec![s("toryo.yaml")],
        },
        E::PlanReady { subject: s("p") },
        E::PlanApproved {
            occurrence: 1,
            subject: s("p"),
        },
        E::PlanRejected {
            occurrence: 1,
            reason: s("r"),
        },
        E::ImplementationDone,
        E::PathPolicyViolation {
            paths: vec![s("AGENTS.md")],
        },
        E::Checkpointed { head: s(C1) },
        E::VerifyPassed { head: s(C1) },
        E::VerifyFailed { head: s(C1) },
        E::ReviewApproved { head: s(C1) },
        E::ChangesRequested { head: s(C1) },
        E::ReviewBlocked { head: s(C1) },
        E::FinalApproved {
            occurrence: 1,
            subject: s(C1),
        },
        E::FinalRejected {
            occurrence: 1,
            reason: s("r"),
        },
        E::ResumeWithNote { note: s("n") },
        E::ForceFinal,
        E::ResumeStage,
        E::BudgetExceeded,
        E::Pause { reason: s("quota") },
        E::Unpause,
        E::Reverify,
        E::Escalate { reason: s("x") },
        E::Cancel,
        E::Fail { error: s("x") },
    ]
}

/// The transition table: every (state, event) pair not listed here must be rejected.
fn allowed(s: S) -> &'static [&'static str] {
    match s {
        S::New => &["start", "escalate", "cancel", "fail"],
        S::Planning => &[
            "plan_policy_violation",
            "plan_ready",
            "budget_exceeded",
            "pause",
            "escalate",
            "cancel",
            "fail",
        ],
        S::WaitingPlanApproval => &[
            "plan_approved",
            "plan_rejected",
            "escalate",
            "cancel",
            "fail",
        ],
        S::Implementing | S::Fixing => &[
            "implementation_done",
            "path_policy_violation",
            "budget_exceeded",
            "pause",
            "escalate",
            "cancel",
            "fail",
        ],
        S::Checkpointing => &[
            "checkpointed",
            "budget_exceeded",
            "pause",
            "escalate",
            "cancel",
            "fail",
        ],
        S::Verifying => &[
            "verify_passed",
            "verify_failed",
            "budget_exceeded",
            "pause",
            "escalate",
            "cancel",
            "fail",
        ],
        S::Reviewing => &[
            "review_approved",
            "changes_requested",
            "review_blocked",
            "budget_exceeded",
            "pause",
            "reverify",
            "escalate",
            "cancel",
            "fail",
        ],
        S::WaitingFinalApproval => &[
            "final_approved",
            "final_rejected",
            "reverify",
            "escalate",
            "cancel",
            "fail",
        ],
        S::Escalated => &[
            "resume_with_note",
            "force_final",
            "resume_stage",
            "reverify",
            "cancel",
            "fail",
        ],
        S::Paused => &["unpause", "escalate", "cancel", "fail"],
        S::Completed | S::Cancelled | S::Failed => &[],
    }
}

#[test]
fn transition_table_is_exhaustive() {
    let mut checked = 0;
    for s in S::ALL {
        for e in samples() {
            let ok = allowed(s).contains(&e.name());
            let r = at(s).transition(&e);
            assert_eq!(r.is_ok(), ok, "{s:?} + {} → {r:?}", e.name());
            if s.is_terminal() {
                assert_eq!(r, Err(PipelineError::Terminal(s)));
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 14 * 25);
}

#[test]
fn a_rejected_transition_changes_nothing() {
    let mut m = at(S::Verifying);
    let before = m.clone();
    assert!(m.apply(E::ReviewApproved { head: C1.into() }).is_err());
    assert_eq!(m, before);
}

fn walk(m: &mut Machine, events: Vec<E>) -> Vec<S> {
    events
        .into_iter()
        .map(|e| {
            let n = e.name();
            m.apply(e).unwrap_or_else(|err| panic!("{n}: {err}"))
        })
        .collect()
}

fn occ(m: &Machine) -> u32 {
    m.gate().unwrap().occurrence
}

/// Plan → approve → implement → verify → review → final approve.
fn happy(m: &mut Machine) {
    m.apply(E::Start).unwrap();
    m.apply(E::PlanReady {
        subject: "p1".into(),
    })
    .unwrap();
    m.apply(E::PlanApproved {
        occurrence: occ(m),
        subject: "p1".into(),
    })
    .unwrap();
    walk(
        m,
        vec![
            E::ImplementationDone,
            E::Checkpointed { head: C1.into() },
            E::VerifyPassed { head: C1.into() },
            E::ReviewApproved { head: C1.into() },
        ],
    );
    let o = occ(m);
    assert_eq!(
        m.apply(E::FinalApproved {
            occurrence: o,
            subject: C1.into()
        }),
        Ok(S::Completed)
    );
}

#[test]
fn happy_path_completes() {
    let mut m = Machine::new(2);
    happy(&mut m);
    assert_eq!(m.fix_cycle(), 0);
    assert!(m.gate().is_none());
    assert!(m.apply(E::Cancel).is_err(), "terminal");
}

#[test]
fn implement_is_blocked_before_plan_approval() {
    let mut m = Machine::new(2);
    m.apply(E::Start).unwrap();
    assert!(m.expect(S::Implementing).is_err());
    assert!(m.apply(E::ImplementationDone).is_err());
    m.apply(E::PlanReady {
        subject: "p".into(),
    })
    .unwrap();
    assert!(
        m.apply(E::ImplementationDone).is_err(),
        "gate open, not approved"
    );
    assert!(!m.plan_approved());
}

#[test]
fn verify_failure_goes_to_fixing_never_to_review() {
    let mut m = at(S::Verifying);
    assert_eq!(m.apply(E::VerifyFailed { head: C1.into() }), Ok(S::Fixing));
    assert_eq!(m.fix_cycle(), 1);
    // a stale verify result (another head) is rejected
    let mut m = at(S::Verifying);
    assert!(matches!(
        m.apply(E::VerifyPassed { head: "old".into() }),
        Err(PipelineError::StaleHead { .. })
    ));
}

#[test]
fn review_needs_a_verified_current_checkpoint() {
    // R29: the verify pass was for an older checkpoint
    let mut m = at(S::Reviewing);
    m.verified_head = Some("c0".into());
    assert!(matches!(
        m.apply(E::ReviewApproved { head: C1.into() }),
        Err(PipelineError::NotVerified { .. })
    ));
    // a review of a stale head is ignored
    let mut m = at(S::Reviewing);
    assert!(matches!(
        m.apply(E::ChangesRequested { head: "c0".into() }),
        Err(PipelineError::StaleHead { .. })
    ));
}

#[test]
fn every_entry_into_fixing_counts_and_max_cycles_escalates() {
    let mut m = Machine::new(2);
    m.apply(E::Start).unwrap();
    m.apply(E::PlanReady {
        subject: "p".into(),
    })
    .unwrap();
    m.apply(E::PlanApproved {
        occurrence: occ(&m),
        subject: "p".into(),
    })
    .unwrap();
    // cycle 1: path policy (owner decision: counts as a fix cycle)
    assert_eq!(
        m.apply(E::PathPolicyViolation {
            paths: vec!["AGENTS.md".into()]
        }),
        Ok(S::Fixing)
    );
    assert_eq!(m.fix_cycle(), 1);
    // cycle 2: verify failure
    walk(
        &mut m,
        vec![E::ImplementationDone, E::Checkpointed { head: "c2".into() }],
    );
    assert_eq!(
        m.apply(E::VerifyFailed { head: "c2".into() }),
        Ok(S::Fixing)
    );
    assert_eq!(m.fix_cycle(), 2);
    // no cycle left: changes requested → Escalated
    walk(
        &mut m,
        vec![
            E::ImplementationDone,
            E::Checkpointed { head: "c3".into() },
            E::VerifyPassed { head: "c3".into() },
        ],
    );
    assert_eq!(
        m.apply(E::ChangesRequested { head: "c3".into() }),
        Ok(S::Escalated)
    );
    assert_eq!(m.fix_cycle(), 2);
    assert!(
        m.reason().unwrap().contains("after 2 fix cycle(s)"),
        "{:?}",
        m.reason()
    );
}

#[test]
fn final_rejection_is_a_fix_cycle() {
    let mut m = at(S::WaitingFinalApproval);
    let o = occ(&m);
    assert_eq!(
        m.apply(E::FinalRejected {
            occurrence: o,
            reason: "rename the flag".into()
        }),
        Ok(S::Fixing)
    );
    assert_eq!(m.fix_cycle(), 1);
    assert!(m.reason().unwrap().contains("rename the flag"));
}

#[test]
fn escalated_to_fixing_needs_a_note() {
    let mut m = at(S::Reviewing);
    m.apply(E::ReviewBlocked { head: C1.into() }).unwrap();
    assert_eq!(m.state(), S::Escalated);
    assert_eq!(
        m.apply(E::ResumeWithNote { note: "  ".into() }),
        Err(PipelineError::EmptyNote)
    );
    assert_eq!(
        m.apply(E::ResumeWithNote {
            note: "use the v2 API".into()
        }),
        Ok(S::Fixing)
    );
    assert_eq!(m.reason(), Some("use the v2 API"));
}

#[test]
fn escalated_before_plan_approval_resumes_planning_not_fixing() {
    let mut m = Machine::new(2);
    m.apply(E::Start).unwrap();
    m.apply(E::Escalate {
        reason: "planner InvalidOutput".into(),
    })
    .unwrap();
    assert_eq!(
        m.apply(E::ResumeWithNote {
            note: "keep it small".into()
        }),
        Ok(S::Planning)
    );
    assert!(m.clone().apply(E::ForceFinal).is_err());
}

#[test]
fn force_final_from_escalated_needs_a_verified_checkpoint() {
    let mut m = at(S::Escalated);
    assert_eq!(m.apply(E::ForceFinal), Ok(S::WaitingFinalApproval));
    assert_eq!(m.gate().unwrap().subject, C1);
    let mut m = at(S::Escalated);
    m.verified_head = None;
    assert!(matches!(
        m.apply(E::ForceFinal),
        Err(PipelineError::NotVerified { .. })
    ));
}

#[test]
fn paused_returns_to_the_previous_state() {
    for s in [
        S::Planning,
        S::Implementing,
        S::Checkpointing,
        S::Verifying,
        S::Fixing,
        S::Reviewing,
    ] {
        let mut m = at(s);
        assert_eq!(m.apply(E::BudgetExceeded), Ok(S::Paused));
        assert_eq!(m.paused_from(), Some(s));
        assert_eq!(m.apply(E::Unpause), Ok(s));
        assert_eq!(m.paused_from(), None);
    }
    // waiting states are not paused by a budget
    assert!(at(S::WaitingPlanApproval).apply(E::BudgetExceeded).is_err());
}

#[test]
fn a_rejected_gate_occurrence_never_passes() {
    let mut m = Machine::new(2);
    m.apply(E::Start).unwrap();
    m.apply(E::PlanReady {
        subject: "p1".into(),
    })
    .unwrap();
    let first = occ(&m);
    m.apply(E::PlanRejected {
        occurrence: first,
        reason: "no".into(),
    })
    .unwrap();
    m.apply(E::PlanReady {
        subject: "p2".into(),
    })
    .unwrap();
    let second = occ(&m);
    assert_ne!(first, second);
    // an approval of the resolved (rejected) occurrence is refused, whatever the subject
    for subject in ["p1", "p2"] {
        assert!(matches!(
            m.clone().apply(E::PlanApproved {
                occurrence: first,
                subject: subject.into()
            }),
            Err(PipelineError::StaleOccurrence { .. })
        ));
    }
    // R32: approving the open occurrence with the old plan's digest is refused too
    assert!(matches!(
        m.clone().apply(E::PlanApproved {
            occurrence: second,
            subject: "p1".into()
        }),
        Err(PipelineError::SubjectMismatch { .. })
    ));
    assert_eq!(
        m.apply(E::PlanApproved {
            occurrence: second,
            subject: "p2".into()
        }),
        Ok(S::Implementing)
    );
    // the gate is closed: deciding it again fails
    assert!(m
        .apply(E::PlanApproved {
            occurrence: second,
            subject: "p2".into()
        })
        .is_err());
}

#[test]
fn human_plan_rejections_are_unlimited() {
    let mut m = Machine::new(2);
    m.apply(E::Start).unwrap();
    for i in 0..10 {
        m.apply(E::PlanReady {
            subject: format!("p{i}"),
        })
        .unwrap();
        let o = occ(&m);
        assert_eq!(
            m.apply(E::PlanRejected {
                occurrence: o,
                reason: "again".into()
            }),
            Ok(S::Planning)
        );
    }
    assert_eq!(m.plan_attempt(), 11);
}

#[test]
fn automatic_plan_rejections_are_bounded() {
    let mut m = Machine::new(2);
    m.apply(E::Start).unwrap();
    let v = || E::PlanPolicyViolation {
        paths: vec!["toryo.yaml".into()],
    };
    for _ in 0..MAX_PLAN_AUTO_REJECTIONS {
        assert_eq!(m.apply(v()), Ok(S::Planning));
    }
    assert_eq!(m.apply(v()), Ok(S::Escalated));
    assert!(m.reason().unwrap().contains("protected paths"));
}

#[test]
fn no_change_checkpoint_is_the_only_way_on() {
    // R40: there is no "nothing to commit" event; an empty checkpoint is Checkpointed
    let mut m = at(S::Implementing);
    walk(
        &mut m,
        vec![E::ImplementationDone, E::Checkpointed { head: C1.into() }],
    );
    assert_eq!(m.state(), S::Verifying);
}

#[test]
fn machine_roundtrips_through_json() {
    let mut m = Machine::new(3);
    happy(&mut m);
    let text = serde_json::to_string(&m).unwrap();
    let back: Machine = serde_json::from_str(&text).unwrap();
    assert_eq!(back, m);
    let e: E =
        serde_json::from_str(r#"{"event":"plan_rejected","occurrence":2,"reason":"x"}"#).unwrap();
    assert_eq!(e.name(), "plan_rejected");
}

#[test]
fn resume_stage_returns_to_the_escalated_stage_without_a_cycle() {
    let mut m = at(S::Implementing);
    m.apply(E::Escalate {
        reason: "recovery_policy_violation".into(),
    })
    .unwrap();
    assert_eq!(m.escalated_from(), Some(S::Implementing));
    assert_eq!(m.apply(E::ResumeStage), Ok(S::Implementing));
    assert_eq!(m.fix_cycle(), 0);
    // an escalation from a waiting state has no stage to go back to
    let mut m = at(S::WaitingPlanApproval);
    m.apply(E::Escalate { reason: "x".into() }).unwrap();
    assert!(m.apply(E::ResumeStage).is_err());
}

#[test]
fn pause_keeps_the_stage_and_reverify_clears_the_verified_head() {
    // R49 (M5): quota / --no-fallback pause the running stage; resume returns to it
    let mut m = at(S::Reviewing);
    assert_eq!(
        m.apply(E::Pause {
            reason: "reviewer out of quota".into()
        }),
        Ok(S::Paused)
    );
    assert_eq!(m.reason(), Some("reviewer out of quota"));
    assert_eq!(m.apply(E::Unpause), Ok(S::Reviewing));
    // R35: a reloaded verify config sends the checkpoint back to Verify
    let mut m = at(S::WaitingFinalApproval);
    assert_eq!(m.apply(E::Reverify), Ok(S::Verifying));
    assert!(m.gate().is_none() && m.verified_head().is_none());
    assert!(m.apply(E::ReviewApproved { head: C1.into() }).is_err());
    let mut m = at(S::Escalated);
    assert_eq!(m.apply(E::Reverify), Ok(S::Verifying));
    assert_eq!(m.apply(E::ForceFinal).map_err(|_| ()), Err(()));
    // never before a checkpoint exists
    let mut m = at(S::Escalated);
    m.checkpoint = None;
    assert!(m.apply(E::Reverify).is_err());
}

#[test]
fn reverify_before_the_review_takes_the_checkpoint_back_to_verify() {
    // EC-10: a reload while the task waited (Paused from Reviewing, Escalated, …) invalidated
    // the verify result; the review stage sends the checkpoint back to Verify first
    let mut m = at(S::Reviewing);
    assert_eq!(m.apply(E::Reverify), Ok(S::Verifying));
    assert_eq!((m.checkpoint(), m.verified_head()), (Some(C1), None));
    assert_eq!(m.fix_cycle(), 0, "not a fix cycle");
    // the old verify pass no longer counts: the review of C1 needs a new one
    assert!(matches!(
        m.clone().apply(E::ReviewApproved { head: C1.into() }),
        Err(PipelineError::InvalidTransition { .. })
    ));
    assert_eq!(
        m.apply(E::VerifyPassed { head: C1.into() }),
        Ok(S::Reviewing)
    );
    assert_eq!(
        m.apply(E::ReviewApproved { head: C1.into() }),
        Ok(S::WaitingFinalApproval)
    );
}

// ── M11-C4: split limits ─────────────────────────────────────────────────────────────

/// [`at`] with the split limits of an org chart (M11-C4).
fn at_split(s: S) -> Machine {
    let m = at(s);
    Machine {
        split: Machine::with_limits(2, 2).split,
        ..m
    }
}

/// The exhaustive table holds unchanged with split limits: the bounds change which
/// counter a bounce uses, never which transitions exist.
#[test]
fn split_limits_keep_the_transition_table() {
    let mut checked = 0;
    for s in S::ALL {
        for e in samples() {
            let ok = allowed(s).contains(&e.name());
            let r = at_split(s).transition(&e);
            assert_eq!(r.is_ok(), ok, "{s:?} + {} → {r:?}", e.name());
            checked += 1;
        }
    }
    assert_eq!(checked, 14 * 25);
}

/// Plan approved, implemented, checkpoint `h`: in Verifying.
fn to_verifying(m: &mut Machine, h: &str) {
    if m.state() == S::New {
        m.apply(E::Start).unwrap();
        m.apply(E::PlanReady {
            subject: "p".into(),
        })
        .unwrap();
        let o = occ(m);
        m.apply(E::PlanApproved {
            occurrence: o,
            subject: "p".into(),
        })
        .unwrap();
    }
    walk(
        m,
        vec![E::ImplementationDone, E::Checkpointed { head: h.into() }],
    );
}

/// M11-C4 AC-1: `max_gate_retries: 2` — two failing verifies are fixed, the third escalates
/// naming the gate limit; review rejections do not count against it.
#[test]
fn gate_retries_are_bounded_on_their_own() {
    let mut m = Machine::with_limits(2, 5);
    to_verifying(&mut m, "c1");
    // a review rejection first: counts against the review rounds only
    m.apply(E::VerifyPassed { head: "c1".into() }).unwrap();
    assert_eq!(
        m.apply(E::ChangesRequested { head: "c1".into() }),
        Ok(S::Fixing)
    );
    for (n, h) in [(1, "c2"), (2, "c3")] {
        to_verifying(&mut m, h);
        assert_eq!(m.apply(E::VerifyFailed { head: h.into() }), Ok(S::Fixing));
        assert_eq!(m.split_limits().unwrap().gate_retries, n);
    }
    to_verifying(&mut m, "c4");
    assert_eq!(
        m.apply(E::VerifyFailed { head: "c4".into() }),
        Ok(S::Escalated)
    );
    let l = m.split_limits().unwrap();
    assert_eq!((l.gate_retries, l.review_rounds), (2, 1));
    assert_eq!(m.fix_cycle(), 3, "fix_cycle counts every entry into Fixing");
    assert!(
        m.reason().unwrap().starts_with(
            "2 gate retries used (limits.max_gate_retries) — not accepted (last: verify failed)"
        ),
        "{:?}",
        m.reason()
    );
    // a path-policy violation is a build-stage bounce too
    let mut m = Machine::with_limits(0, 5);
    to_verifying(&mut m, "c1");
    let mut m2 = m.clone();
    m2.state = S::Fixing;
    assert_eq!(
        m2.apply(E::PathPolicyViolation {
            paths: vec!["AGENTS.md".into()]
        }),
        Ok(S::Escalated)
    );
    assert!(m2.reason().unwrap().contains("limits.max_gate_retries"));
}

/// M11-C4 AC-2: `max_review_rounds: 1` — the second rejection escalates naming the review
/// limit; a human's final rejection is a review round too.
#[test]
fn review_rounds_are_bounded_on_their_own() {
    let mut m = Machine::with_limits(5, 1);
    to_verifying(&mut m, "c1");
    m.apply(E::VerifyPassed { head: "c1".into() }).unwrap();
    assert_eq!(
        m.apply(E::ChangesRequested { head: "c1".into() }),
        Ok(S::Fixing)
    );
    to_verifying(&mut m, "c2");
    m.apply(E::VerifyFailed { head: "c2".into() }).unwrap();
    to_verifying(&mut m, "c3");
    m.apply(E::VerifyPassed { head: "c3".into() }).unwrap();
    assert_eq!(
        m.apply(E::ChangesRequested { head: "c3".into() }),
        Ok(S::Escalated)
    );
    assert!(
        m.reason().unwrap().starts_with("1 review rounds used (limits.max_review_rounds) — not accepted (last: changes requested)"),
        "{:?}",
        m.reason()
    );
    let mut f = at_split(S::WaitingFinalApproval);
    f.split.as_mut().unwrap().review_rounds = 2;
    let o = occ(&f);
    assert_eq!(
        f.apply(E::FinalRejected {
            occurrence: o,
            reason: "no".into()
        }),
        Ok(S::Escalated)
    );
    assert!(f.reason().unwrap().contains("limits.max_review_rounds"));
}

/// M11-C4 AC-3: a machine stored by M10 (the single counter, no `split` key) deserializes
/// and escalates at the same cycle as before; a single-bound machine still serialises
/// without the new key.
#[test]
fn an_m10_machine_reads_back_and_behaves_the_same() {
    let m10 = r#"{"state":"Reviewing","max_fix_cycles":2,"fix_cycle":2,"plan_attempt":1,"plan_auto_rejections":0,"plan_approved":true,"occurrences":1,"gate":null,"checkpoint":"c3","verified_head":"c3","paused_from":null,"escalated_from":null,"reason":"verify failed"}"#;
    let mut m: Machine = serde_json::from_str(m10).unwrap();
    assert_eq!(m.split_limits(), None);
    assert_eq!(
        serde_json::to_string(&m).unwrap(),
        m10,
        "the same JSON back"
    );
    assert_eq!(
        m.apply(E::ChangesRequested { head: "c3".into() }),
        Ok(S::Escalated)
    );
    assert!(
        m.reason().unwrap().contains("after 2 fix cycle(s)"),
        "{:?}",
        m.reason()
    );
    // the split machine round-trips with its counters
    let mut s = Machine::with_limits(3, 1);
    to_verifying(&mut s, "c1");
    s.apply(E::VerifyFailed { head: "c1".into() }).unwrap();
    let text = serde_json::to_string(&s).unwrap();
    assert!(text.contains(r#""split":{"max_gate_retries":3,"max_review_rounds":1,"gate_retries":1,"review_rounds":0}"#), "{text}");
    assert_eq!(serde_json::from_str::<Machine>(&text).unwrap(), s);
    assert_eq!(s.max_fix_cycles(), 4, "the sum, for display");
}
