//! M1: the adapter layer — replay of real M0 streams through the real parsers, the
//! scripted FakeAgent, `run_to_completion` limits, and the LocalRunner.

use std::path::PathBuf;
use std::time::Duration;

use toryo::agents::adapter::{run_to_completion, AgentAdapter};
use toryo::agents::events::replay;
use toryo::agents::fake::{FakeAgentAdapter, FakeScript};
use toryo::config::ProfileKind;
use toryo::domain::{AgentEvent, ErrorClass, OpenSession, Role, Tri, TurnInput, TurnLimits};
use toryo::usage::Accounting;

fn fixture(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/captured")
        .join(name);
    std::fs::read_to_string(p).unwrap()
}

fn terminal(events: &[AgentEvent]) -> &AgentEvent {
    let t: Vec<&AgentEvent> = events.iter().filter(|e| e.is_terminal()).collect();
    assert_eq!(t.len(), 1, "exactly one terminal event");
    assert!(
        events.last().unwrap().is_terminal(),
        "terminal event comes last"
    );
    t[0]
}

#[test]
fn replay_claude_review_changes_requested() {
    let ev = replay(
        ProfileKind::Claude,
        &fixture("claude-review-changes-requested.jsonl"),
        Some(0),
    );
    assert!(matches!(ev.first(), Some(AgentEvent::SessionIdentified(_))));
    // the reviewer reads the run store (TASK.md, PLAN.md, DIFF.patch …) with Read only
    assert!(ev
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolActivity { name, .. } if name == "Read")));
    assert!(ev.iter().any(
        |e| matches!(e, AgentEvent::Usage(u) if u.accounting == Accounting::Exact && u.total() > 0)
    ));
    match terminal(&ev) {
        AgentEvent::TurnCompleted {
            structured: Some(v),
            ..
        } => {
            assert_eq!(v["decision"], "changes_requested");
            let unmet: Vec<&str> = v["acceptance_criteria"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|a| a["met"] == false)
                .filter_map(|a| a["id"].as_str())
                .collect();
            assert_eq!(unmet, ["AC-2", "AC-4"]);
        }
        other => panic!("unexpected terminal {other:?}"),
    }
    assert!(!ev
        .iter()
        .any(|e| matches!(e, AgentEvent::PermissionDenied { .. })));
}

#[test]
fn replay_claude_implementer_used_edit_and_write() {
    let ev = replay(
        ProfileKind::Claude,
        &fixture("claude-implementer.jsonl"),
        Some(0),
    );
    let tools: Vec<&str> = ev
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolActivity { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        tools.contains(&"Edit") && tools.contains(&"Write"),
        "{tools:?}"
    );
    assert!(!tools.iter().any(|t| *t == "Bash" || *t == "PowerShell"));
    assert!(matches!(
        terminal(&ev),
        AgentEvent::TurnCompleted {
            structured: Some(_),
            ..
        }
    ));
}

#[test]
fn replay_codex_implement_and_fix() {
    let ev = replay(
        ProfileKind::Codex,
        &fixture("codex-implement.jsonl"),
        Some(0),
    );
    assert!(matches!(ev.first(), Some(AgentEvent::SessionIdentified(_))));
    assert!(ev
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolActivity { name, .. } if name == "file_change")));
    assert!(ev
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolActivity { name, .. } if name == "command")));
    assert!(ev
        .iter()
        .any(|e| matches!(e, AgentEvent::Usage(u) if u.accounting == Accounting::Exact)));
    assert!(
        matches!(terminal(&ev), AgentEvent::TurnCompleted { structured: Some(v), .. } if v["summary"].is_string())
    );

    let ev = replay(ProfileKind::Codex, &fixture("codex-fix.jsonl"), Some(0));
    match terminal(&ev) {
        AgentEvent::TurnCompleted {
            structured: Some(v),
            ..
        } => {
            let resolved: Vec<&str> = v["resolved"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|x| x.as_str())
                .collect();
            assert!(
                resolved.contains(&"I-1") && resolved.contains(&"I-2"),
                "{resolved:?}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn truncated_stream_fails_as_crash() {
    let full = fixture("codex-implement.jsonl");
    let cut: String = full.lines().take(3).collect::<Vec<_>>().join("\n");
    let ev = replay(ProfileKind::Codex, &cut, Some(1));
    assert!(
        matches!(
            terminal(&ev),
            AgentEvent::TurnFailed {
                class: ErrorClass::ProcessCrashed,
                ..
            }
        ),
        "{ev:?}"
    );
    assert!(ev.iter().any(|e| matches!(e, AgentEvent::Usage(u) if u.accounting == Accounting::Unknown && u.buckets.is_none())));
}

fn open(tmp: &std::path::Path, role: Role, profile: &str) -> OpenSession {
    OpenSession {
        profile: profile.into(),
        role,
        cwd: tmp.to_path_buf(),
        run_store: None,
        capture_dir: tmp.join("capture"),
        schema: None,
        context_file: None,
        model: None,
        resume: None,
    }
}

fn script(json: &str) -> FakeAgentAdapter {
    FakeAgentAdapter::new(serde_json::from_str::<FakeScript>(json).unwrap())
}

fn input(label: &str) -> TurnInput {
    TurnInput {
        prompt: format!("prompt for {label}"),
        label: label.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn fake_success_usage_session_and_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let f = script(
        r#"{"turns":{"planner":[
        {"steps":[{"kind":"text","text":"thinking"},{"kind":"usage","input":10,"output":5},{"kind":"complete","structured":{"ok":true}}]},
        {"steps":[{"kind":"complete","text":"second"}]}]}}"#,
    );
    let a = f.for_profile("claude-max", ProfileKind::Claude);
    assert_eq!(a.usage_capabilities().cumulative_on_resume, Tri::No);
    let mut s = a
        .open(open(tmp.path(), Role::Planner, "claude-max"))
        .await
        .unwrap();
    let o = run_to_completion(s.as_mut(), input("plan-1"), TurnLimits::default())
        .await
        .unwrap();
    assert!(o.success);
    assert_eq!(o.structured.unwrap()["ok"], true);
    assert_eq!(o.usage.unwrap().total(), 15);
    let id = o.session_id.clone().unwrap();
    assert!(tmp.path().join("capture/plan-1.prompt.md").exists());
    // a second turn on the same session resumes it
    let o2 = run_to_completion(s.as_mut(), input("plan-2"), TurnLimits::default())
        .await
        .unwrap();
    assert_eq!(o2.final_text, "second");
    let calls = f.calls();
    assert_eq!(calls[0].resume, None);
    assert_eq!(calls[1].resume.as_deref(), Some(id.as_str()));
}

#[tokio::test]
async fn fake_failure_classes_and_limits() {
    let tmp = tempfile::tempdir().unwrap();
    let f = script(
        r#"{"turns":{
        "implementer@codex-openai":[{"steps":[{"kind":"write_file","path":"src/partial.txt","content":"half"},{"kind":"fail","class":"QuotaExceeded","detail":"You've hit your usage limit"}]}],
        "implementer":[{"steps":[{"kind":"fail","class":"RateLimited","detail":"429"}]}],
        "reviewer":[{"steps":[{"kind":"hang"}]},{"steps":[{"kind":"crash"}]},{"steps":[{"kind":"deny","tool":"Read","detail":"C:/outside"},{"kind":"complete"}]}]}}"#,
    );
    let limits = TurnLimits {
        max: Duration::from_secs(5),
        stall: Duration::from_millis(200),
    };

    let a = f.for_profile("codex-openai", ProfileKind::Codex);
    let mut s = a
        .open(open(tmp.path(), Role::Implementer, "codex-openai"))
        .await
        .unwrap();
    let o = run_to_completion(s.as_mut(), input("implement"), limits)
        .await
        .unwrap();
    assert_eq!(o.error_class, Some(ErrorClass::QuotaExceeded));
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("src/partial.txt")).unwrap(),
        "half"
    );

    // profile-specific key first, then the role key
    let a2 = f.for_profile("claude-max", ProfileKind::Claude);
    let mut s = a2
        .open(open(tmp.path(), Role::Implementer, "claude-max"))
        .await
        .unwrap();
    assert_eq!(
        run_to_completion(s.as_mut(), input("implement-2"), limits)
            .await
            .unwrap()
            .error_class,
        Some(ErrorClass::RateLimited)
    );

    let mut s = a2
        .open(open(tmp.path(), Role::Reviewer, "claude-max"))
        .await
        .unwrap();
    let o = run_to_completion(s.as_mut(), input("review-hang"), limits)
        .await
        .unwrap();
    assert!(
        o.timed_out && o.error_class == Some(ErrorClass::Timeout),
        "{o:?}"
    );
    let o = run_to_completion(s.as_mut(), input("review-crash"), limits)
        .await
        .unwrap();
    assert_eq!(o.error_class, Some(ErrorClass::ProcessCrashed));
    let o = run_to_completion(s.as_mut(), input("review-deny"), limits)
        .await
        .unwrap();
    assert_eq!(o.permission_denials.len(), 1);
    // no turn left → a clear failure, never a hang
    let o = run_to_completion(s.as_mut(), input("review-extra"), limits)
        .await
        .unwrap();
    assert!(o.error_detail.unwrap().contains("no turn left"));
}

#[tokio::test]
async fn fake_replay_step_uses_real_parser() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/captured");
    let json = format!(
        r#"{{"turns":{{"reviewer":[{{"steps":[{{"kind":"replay","agent":"claude","file":"{}"}}]}}]}}}}"#,
        dir.join("claude-review-changes-requested.jsonl")
            .display()
            .to_string()
            .replace('\\', "/")
    );
    let f = script(&json);
    let a = f.for_profile("claude-max", ProfileKind::Claude);
    let mut s = a
        .open(open(tmp.path(), Role::Reviewer, "claude-max"))
        .await
        .unwrap();
    let o = run_to_completion(s.as_mut(), input("review-0-0"), TurnLimits::default())
        .await
        .unwrap();
    assert!(o.success);
    assert_eq!(o.structured.unwrap()["decision"], "changes_requested");
    assert!(o.session_id.is_some());
}

#[cfg(unix)]
#[tokio::test]
async fn local_runner_captures_plain_text_and_exit() {
    use toryo::runner::{CommandRunner, LocalRunner, RunSpec};
    let tmp = tempfile::tempdir().unwrap();
    let r = LocalRunner
        .run(
            RunSpec {
                name: "t".into(),
                program: "/bin/sh".into(),
                args: vec![
                    "-c".into(),
                    r"printf '\033[33;1mWARNING\033[0m ok\n'; exit 3".into(),
                ],
                cwd: tmp.path().to_path_buf(),
                env: std::env::vars().collect(),
                boundary: toryo::process::Boundary::None,
                timeout: Duration::from_secs(20),
                cancel: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(r.exit, Some(3));
    assert_eq!(r.output.trim(), "WARNING ok");
}
