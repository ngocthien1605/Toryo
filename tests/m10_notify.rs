//! M10-D1 acceptance: toasts when the owner is needed, through the `toryo` binary with fake
//! agents. The toasts go to a log (`TORYO_TEST_TOAST_LOG`), never to the desktop, except in the
//! ignored `notify_test_shows_a_real_toast`.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use toryo::engine::PipelineState as St;

fn toasts(log: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn happy(env: &Env) -> String {
    script(
        env,
        "happy.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    )
}

/// `notify.toast` in the machine config turns toasts on; `--no-notify` and `--notify` override
/// it; the plan gate, the final gate and the end of a run each raise one.
#[test]
fn toasts_at_the_gates_follow_the_machine_setting_and_the_flags() {
    let env = setup("codex-openai");
    let s = happy(&env);
    let log = env.root.join("toasts.jsonl");
    let l = log.to_str().unwrap();

    // off by default
    create(&env, "N-1");
    let (code, err) = toryo(
        &env,
        &["run", "N-1", "--fake-agents", &s],
        &[("TORYO_TEST_TOAST_LOG", l)],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "N-1").state, St::WaitingPlanApproval);
    assert!(toasts(&log).is_empty(), "default off");

    // --notify: the next stop (the final gate) raises one
    let (code, err) = toryo(
        &env,
        &[
            "approve",
            "N-1",
            "--gate",
            "plan",
            "--continue",
            "--notify",
            "--fake-agents",
            &s,
        ],
        &[("TORYO_TEST_TOAST_LOG", l)],
    );
    assert_eq!(code, 0, "{err}");
    let t = toasts(&log);
    assert_eq!(t.len(), 1, "{t:?}");
    assert_eq!(t[0]["title"], "toryo — N-1");
    let body = t[0]["body"].as_str().unwrap();
    assert!(
        body.starts_with("WaitingFinalApproval — ")
            && body.contains("toryo approve N-1 --gate final"),
        "{body}"
    );
    assert!(body.chars().count() <= 200);

    // the machine setting on, --no-notify wins
    std::fs::write(
        &env.machine,
        format!("{MACHINE_PROFILES}notify: {{ toast: true }}\n"),
    )
    .unwrap();
    create(&env, "N-2");
    let (code, err) = toryo(
        &env,
        &["run", "N-2", "--no-notify", "--fake-agents", &s],
        &[("TORYO_TEST_TOAST_LOG", l)],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(toasts(&log).len(), 1, "nothing with --no-notify");

    // the machine setting on: running a task that already waits opens no new gate (no toast)
    let (code, err) = toryo(
        &env,
        &["run", "N-2", "--fake-agents", &s],
        &[("TORYO_TEST_TOAST_LOG", l)],
    );
    assert_eq!(code, 0, "{err}");
    let t = toasts(&log);
    assert_eq!(
        t.len(),
        1,
        "a waiting task that is run again opens no new gate: {t:?}"
    );
    create(&env, "N-3");
    let (code, err) = toryo(
        &env,
        &["run", "N-3", "--fake-agents", &s],
        &[("TORYO_TEST_TOAST_LOG", l)],
    );
    assert_eq!(code, 0, "{err}");
    let t = toasts(&log);
    assert_eq!(t.len(), 2, "{t:?}");
    assert_eq!(t[1]["title"], "toryo — N-3");
    assert!(t[1]["body"]
        .as_str()
        .unwrap()
        .starts_with("WaitingPlanApproval — "));
    // approving without --continue raises nothing (the owner is at the keyboard)
    let (code, err) = toryo(
        &env,
        &["approve", "N-3", "--gate", "plan"],
        &[("TORYO_TEST_TOAST_LOG", l)],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(toasts(&log).len(), 2);
    let (code, err) = toryo(
        &env,
        &["run", "N-3", "--yes", "--fake-agents", &s],
        &[("TORYO_TEST_TOAST_LOG", l)],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "N-3").state, St::Completed);
    let t = toasts(&log);
    let bodies: Vec<&str> = t[2..].iter().map(|x| x["body"].as_str().unwrap()).collect();
    assert_eq!(bodies.len(), 2, "{bodies:?}");
    assert!(
        bodies[0].starts_with("WaitingFinalApproval — "),
        "{bodies:?}"
    );
    assert!(
        bodies[1].starts_with("Completed — done — merge"),
        "{bodies:?}"
    );
}

/// An escalation raises a toast whose body carries the (redacted) reason.
#[test]
fn an_escalation_raises_a_toast() {
    let env = setup("codex-openai");
    create(&env, "N-4");
    let s = script(
        &env,
        "f.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![fail("Unknown", "boom `whoami` $(x) \"q\"")])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let log = env.root.join("toasts.jsonl");
    let (_, err) = toryo(
        &env,
        &["run", "N-4", "--yes", "--notify", "--fake-agents", &s],
        &[("TORYO_TEST_TOAST_LOG", log.to_str().unwrap())],
    );
    assert_eq!(task(&env, "N-4").state, St::Escalated, "{err}");
    let t = toasts(&log);
    let esc: Vec<&Value> = t
        .iter()
        .filter(|x| x["body"].as_str().unwrap().starts_with("Escalated"))
        .collect();
    assert_eq!(esc.len(), 1, "{t:?}");
    assert!(
        esc[0]["body"]
            .as_str()
            .unwrap()
            .contains("boom `whoami` $(x) \"q\""),
        "{t:?}"
    );
}

/// AC-4: `notify-test` runs the toast script (dry run: built, not shown) and exits 0.
#[cfg(windows)]
#[test]
fn notify_test_builds_the_toast() {
    let env = setup("codex-openai");
    let (code, err) = toryo(&env, &["notify-test", "--dry-run"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("the toast was built"), "{err}");
}

/// AC-4, for the owner on a desktop session: shows a real toast.
#[cfg(windows)]
#[test]
#[ignore = "shows a real toast on the desktop: `cargo test --test m10_notify -- --ignored`"]
fn notify_test_shows_a_real_toast() {
    let env = setup("codex-openai");
    let (code, err) = toryo(&env, &["notify-test"], &[]);
    assert_eq!(code, 0, "{err}");
}

#[cfg(not(windows))]
#[test]
fn notify_test_says_toasts_are_windows_only() {
    let env = setup("codex-openai");
    let (code, err) = toryo(&env, &["notify-test", "--dry-run"], &[]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("Windows only"), "{err}");
}

/// R1 AC-2: the end state toasts only when the drive reached it — not `run` on a task that is
/// already Completed, not `approve --gate final --continue` (the approval completed it); an error
/// end toasts whatever the state.
#[test]
fn no_second_completed_toast_and_an_error_end_toasts() {
    let env = setup("codex-openai");
    let s = happy(&env);
    let log = env.root.join("toasts.jsonl");
    let l = log.to_str().unwrap();
    create(&env, "N-5");
    let (code, err) = toryo(&env, &["run", "N-5", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = toryo(
        &env,
        &[
            "approve",
            "N-5",
            "--gate",
            "plan",
            "--continue",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "N-5").state, St::WaitingFinalApproval);
    let (code, err) = toryo(
        &env,
        &[
            "approve",
            "N-5",
            "--gate",
            "final",
            "--continue",
            "--notify",
            "--fake-agents",
            &s,
        ],
        &[("TORYO_TEST_TOAST_LOG", l)],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "N-5").state, St::Completed);
    let (code, err) = toryo(
        &env,
        &["run", "N-5", "--notify", "--fake-agents", &s],
        &[("TORYO_TEST_TOAST_LOG", l)],
    );
    assert_eq!(code, 0, "{err}");
    assert!(toasts(&log).is_empty(), "{:?}", toasts(&log));

    create(&env, "N-6");
    let missing = env.root.join("nope.json");
    let (code, _) = toryo(
        &env,
        &[
            "run",
            "N-6",
            "--notify",
            "--fake-agents",
            missing.to_str().unwrap(),
        ],
        &[("TORYO_TEST_TOAST_LOG", l)],
    );
    assert_eq!(code, 2);
    let t = toasts(&log);
    assert_eq!(t.len(), 1, "{t:?}");
    assert!(t[0]["body"]
        .as_str()
        .unwrap()
        .starts_with("New — stopped with an error: "));
}
