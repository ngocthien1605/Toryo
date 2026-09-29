//! Regression tests for the agents layer (code review 2026-09-29, package WP-A): turn
//! outcomes, error classification and liveness. Most started as the review's probes, which
//! pinned the defects; here they are inverted to pin the fixes.
//!
//! | Item | Tests |
//! |---|---|
//! | EC-02 Codex `error` notices are warnings | `codex_error_notice_then_completed_is_a_success`, `codex_turn_failure_is_decided_after_the_stream` |
//! | EC-03 Azure 429 vs quota | `codex_throttle_is_rate_limited_and_quota_texts_stay_quota` |
//! | EC-04 Claude classification | `claude_failure_texts_use_word_boundaries`, `claude_structured_fields_decide_before_the_text`, `claude_recovered_api_retry_does_not_decide_the_class`, `claude_retry_decides_only_while_still_retrying` |
//! | EC-05 live usage | `claude_replay_reports_live_cumulative_usage`, `unix::live_usage_arrives_while_the_claude_turn_runs`, `unix::usage_hook_interrupts_the_claude_turn_mid_turn` |
//! | EC-06 output is activity | `every_stdout_line_is_activity`, `unix::stall_does_not_fire_while_claude_streams_thinking`, `unix::stall_still_fires_when_the_agent_is_silent` |
//! | EC-32 no busy wait | `linux::wait_does_not_spin_after_both_pipes_close` |
//! | TS-01 fake parity | `fake_denial_then_complete_fails_like_the_real_parser`, `fake_unreadable_replay_file_fails_loudly` |
//! | ARCH-05 fail-closed preflight | `codex_preflight_refuses_the_turn_without_a_process`, `unix::codex_turn_runs_when_the_preflight_passes` |
//! | CODE-07 capture errors | `linux::capture_write_failure_fails_the_turn` |

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use toryo::agents::adapter::{run_to_completion, AgentAdapter};
use toryo::agents::claude::{ClaudeParser, LaunchMode};
use toryo::agents::cli::capabilities;
use toryo::agents::codex::{classify_message, CodexParser};
use toryo::agents::events::{line_events, replay, LiveEvents};
use toryo::agents::fake::{FakeAgentAdapter, FakeScript};
use toryo::agents::hub::AgentHub;
use toryo::config::{CodexSettings, Effective, ProfileKind};
use toryo::domain::{
    permission_denials_detail, AgentEvent, ErrorClass, OpenSession, Role, StageOutcome, TurnInput,
    TurnLimits,
};
use toryo::usage::{Accounting, CodexSubsets};

// ── shared stream lines ──────────────────────────────────────────────────────────────

const INIT: &str =
    r#"{"type":"system","subtype":"init","session_id":"s1","tools":["Read","Grep","Glob"]}"#;
const START_900K: &str = r#"{"type":"stream_event","session_id":"s1","event":{"type":"message_start","message":{"id":"m1","usage":{"input_tokens":900000,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":1}}}}"#;
// real Claude Code 2.1.283 line shapes (fixtures/captured/claude-implementer.jsonl)
const THINKING_TOKENS: &str = r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":50,"estimated_tokens_delta":50,"session_id":"s1"}"#;
const THINKING_DELTA: &str = r#"{"type":"stream_event","session_id":"s1","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}}"#;
const INPUT_JSON_DELTA: &str = r#"{"type":"stream_event","session_id":"s1","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"file_path\":"}}}"#;
const RESULT: &str = r#"{"type":"result","subtype":"success","is_error":false,"session_id":"s1","result":"{\"ok\":true}","usage":{"input_tokens":900000,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":50}}"#;

const CODEX_DONE: &[&str] = &[
    r#"{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"{\"summary\":\"ok\",\"deviations\":[],\"validation_performed\":[],\"open_issues\":[],\"resolved\":[]}"}}"#,
    r#"{"type":"turn.completed","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5}}"#,
];
const CODEX_START: &[&str] = &[
    r#"{"type":"thread.started","thread_id":"th_1"}"#,
    r#"{"type":"turn.started"}"#,
];

fn codex_error(message: &str) -> String {
    json!({"type":"error","message":message}).to_string()
}

fn codex(lines: &[&str], exit: Option<i32>) -> toryo::agents::TurnOutcome {
    let mut p = CodexParser::new();
    for l in lines {
        p.feed_line(l.as_bytes());
    }
    p.finish(exit, CodexSubsets::default())
}

fn claude(lines: &[&str], exit: Option<i32>) -> toryo::agents::TurnOutcome {
    let mut p = ClaudeParser::new();
    for l in lines {
        p.feed_line(l.as_bytes());
    }
    p.finish(exit)
}

/// The class of a failed Claude result carrying only `text`.
fn claude_text(text: &str) -> Option<ErrorClass> {
    let line =
        json!({"type":"result","subtype":"success","is_error":true,"session_id":"s","result":text})
            .to_string();
    claude(&[INIT, &line], Some(1)).error_class
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/captured")
        .join(name)
}

fn open(dir: &Path, profile: &str, role: Role) -> OpenSession {
    OpenSession {
        profile: profile.into(),
        role,
        cwd: dir.to_path_buf(),
        run_store: Some(dir.to_path_buf()),
        capture_dir: dir.join("cap"),
        schema: None,
        context_file: None,
        model: None,
        resume: None,
    }
}

fn input(label: &str) -> TurnInput {
    TurnInput {
        prompt: format!("prompt for {label}"),
        label: label.into(),
        ..Default::default()
    }
}

/// A config with a Claude and a Codex profile; `exe` is used for both.
fn config(dir: &Path, exe: &Path) -> Effective {
    let y = dir.join("toryo.yaml");
    let fwd = |p: &Path| p.display().to_string().replace('\\', "/");
    std::fs::write(
        &y,
        format!(
            "version: 1\nproject: {{ root: '{d}', worktrees_root: '{d}/wts' }}\nprofiles:\n  claude-max: {{ kind: claude, billing: subscription, exe: '{e}' }}\n  codex-openai: {{ kind: codex, billing: subscription, exe: '{e}' }}\nroles:\n  planner: {{ profile: claude-max }}\n",
            d = fwd(dir),
            e = fwd(exe)
        ),
    )
    .unwrap();
    Effective::load(&y, None).unwrap()
}

/// Give the Codex profile these sandbox settings (set in code, past any config-load
/// validation: the adapter must refuse on its own).
fn with_codex_sandbox(
    mut cfg: Effective,
    sandbox_mode: &str,
    windows_sandbox: Option<&str>,
) -> Effective {
    cfg.repo.profiles.get_mut("codex-openai").unwrap().codex = Some(CodexSettings {
        sandbox_mode: sandbox_mode.into(),
        windows_sandbox: windows_sandbox.map(str::to_string),
        ..Default::default()
    });
    cfg
}

// ── EC-02: a Codex `error` line is a notice; the turn's end decides ───────────────────

#[test]
fn codex_error_notice_then_completed_is_a_success() {
    for notice in [
        "stream disconnected before completion: retrying 1/5 in 200ms",
        "Reconnecting... 1/5 (stream disconnected before completion)",
    ] {
        let error = codex_error(notice);
        let lines: Vec<&str> = CODEX_START
            .iter()
            .copied()
            .chain([error.as_str()])
            .chain(CODEX_DONE.iter().copied())
            .collect();
        let o = codex(&lines, Some(0));
        assert!(
            o.success,
            "{notice}: {:?} {:?}",
            o.error_class, o.error_detail
        );
        assert_eq!(o.error_class, None);
        assert_eq!(o.structured.as_ref().unwrap()["summary"], "ok");
        assert_eq!(o.warnings, vec![notice.to_string()], "kept as a warning");
        assert_eq!(o.accounting, Accounting::Exact);
        // the same through the event layer (live turns and replays)
        let ev = replay(ProfileKind::Codex, &lines.join("\n"), Some(0));
        assert!(
            matches!(
                ev.last(),
                Some(AgentEvent::TurnCompleted {
                    structured: Some(_),
                    ..
                })
            ),
            "{ev:?}"
        );
    }
}

#[test]
fn codex_turn_failure_is_decided_after_the_stream() {
    let quota = "You've hit your usage limit. Upgrade to Pro or try again later.";
    // a fatal error: `error` then `turn.failed` (codex-cli 0.157.x)
    let error = codex_error(quota);
    let failed = json!({"type":"turn.failed","error":{"message":quota}}).to_string();
    let o = codex(&[CODEX_START[0], CODEX_START[1], &error, &failed], Some(1));
    assert!(!o.success);
    assert_eq!(o.error_class, Some(ErrorClass::QuotaExceeded));
    assert_eq!(o.error_detail.as_deref(), Some(quota));
    // no turn.completed after an `error` line: the error is the best cause
    let o = codex(&[CODEX_START[0], CODEX_START[1], &error], Some(1));
    assert!(!o.success);
    assert_eq!(o.error_class, Some(ErrorClass::QuotaExceeded));
    // a notice, then the stream just ends: still a failure (never a success)
    let notice = codex_error("Reconnecting... 5/5 (stream disconnected before completion)");
    let o = codex(&[CODEX_START[0], CODEX_START[1], &notice], Some(1));
    assert!(!o.success);
    assert_eq!(o.error_class, Some(ErrorClass::Network));
    // nothing at all: a crash, as before
    let o = codex(CODEX_START, None);
    assert_eq!(o.error_class, Some(ErrorClass::ProcessCrashed));
    // turn.failed wins over a completed turn in the same stream
    let lines: Vec<&str> = CODEX_START
        .iter()
        .copied()
        .chain(CODEX_DONE.iter().copied())
        .chain([failed.as_str()])
        .collect();
    assert!(!codex(&lines, Some(1)).success);
}

// ── EC-03: a 429 throttle is not quota exhaustion ────────────────────────────────────

#[test]
fn codex_throttle_is_rate_limited_and_quota_texts_stay_quota() {
    let azure = "exceeded retry limit, last status: 429 Too Many Requests: Requests to the ChatCompletions_Create Operation under Azure OpenAI API version 2024-10-21 have exceeded token rate limit of your current OpenAI S0 pricing tier. Please retry after 6 seconds. Please go here: https://aka.ms/oai/quotaincrease if you would like to further increase the default rate limit.";
    assert_eq!(classify_message(azure), ErrorClass::RateLimited);
    let openai_tpm = "Rate limit reached for gpt-5 in organization org-x on tokens per min (TPM): Limit 30000, Used 29000, Requested 2000. Please try again in 2s. Add a payment method at https://platform.openai.com/account/billing to increase your rate limit.";
    assert_eq!(classify_message(openai_tpm), ErrorClass::RateLimited);
    assert_eq!(
        classify_message("stream error: 429 Too Many Requests"),
        ErrorClass::RateLimited
    );
    // genuine exhaustion stays QuotaExceeded (R21/R49), even when it comes with a 429
    for quota in [
        "You've hit your usage limit. Upgrade to Pro or try again in 1 day 3 hours.",
        "exceeded retry limit, last status: 429 Too Many Requests, request id: r1: You exceeded your current quota, please check your plan and billing details.",
        "insufficient_quota",
        "Quota exceeded. Check your plan and billing details.",
        "billing hard limit has been reached",
    ] {
        assert_eq!(classify_message(quota), ErrorClass::QuotaExceeded, "{quota}");
    }
    assert_eq!(
        classify_message("unexpected status 401 Unauthorized"),
        ErrorClass::Auth
    );
    assert_eq!(
        classify_message("input exceeds the context window"),
        ErrorClass::ContextWindowExceeded
    );
}

// ── EC-04: Claude failures, structured fields first, text with word boundaries ────────

#[test]
fn claude_failure_texts_use_word_boundaries() {
    for (text, want) in [
        (
            "API Error: 429 rate limit reached for requests",
            ErrorClass::RateLimited,
        ),
        (
            "API Error: 400 prompt is too long: 214030 tokens > 200000 maximum",
            ErrorClass::ContextWindowExceeded,
        ),
        ("Prompt is too long", ErrorClass::ContextWindowExceeded),
        (
            "Context limit reached · /compact or /clear to continue",
            ErrorClass::ContextWindowExceeded,
        ),
        ("API Error: 529 Overloaded", ErrorClass::Overloaded),
        ("Invalid API key · Please run /login", ErrorClass::Auth),
        // exhaustion texts keep switching the implementer (R49)
        (
            "Claude AI usage limit reached|1790000000",
            ErrorClass::QuotaExceeded,
        ),
        ("Credit balance is too low", ErrorClass::QuotaExceeded),
        (
            "5-hour limit reached ∙ resets 3pm",
            ErrorClass::QuotaExceeded,
        ),
        ("API Error: 429 rate limit", ErrorClass::RateLimited),
        ("something odd happened", ErrorClass::Unknown),
    ] {
        assert_eq!(claude_text(text), Some(want), "{text}");
    }
}

#[test]
fn claude_structured_fields_decide_before_the_text() {
    let assistant = |id: &str, error: Option<&str>, text: &str| {
        let mut v = json!({"type":"assistant","session_id":"s1","message":{"id":id,"role":"assistant","content":[{"type":"text","text":text}]}});
        if let Some(e) = error {
            v["error"] = json!(e);
        }
        v.to_string()
    };
    let result = |text: &str, status: Option<u64>| {
        json!({"type":"result","subtype":"success","is_error":true,"session_id":"s1","result":text,"api_error_status":status}).to_string()
    };
    let cases: Vec<(Vec<String>, ErrorClass)> = vec![
        // the category wins over a text that says nothing
        (
            vec![
                assistant("m1", Some("authentication_failed"), "API Error"),
                result("API Error", None),
            ],
            ErrorClass::Auth,
        ),
        (
            vec![
                assistant("m1", Some("server_error"), "API Error: 500"),
                result("API Error: 500", Some(500)),
            ],
            ErrorClass::Overloaded,
        ),
        // … and over a text that misleads (alone, "authentication" would read as Auth)
        (
            vec![
                assistant("m1", Some("invalid_request"), "API Error: 400"),
                result(
                    "API Error: 400 messages: text content blocks must be non-empty; see https://docs.anthropic.com/en/api/errors#authentication",
                    Some(400),
                ),
            ],
            ErrorClass::BadRequest,
        ),
        // the text refines a bad request into a context overflow …
        (
            vec![
                assistant("m1", Some("invalid_request"), "Prompt is too long"),
                result("Prompt is too long", Some(400)),
            ],
            ErrorClass::ContextWindowExceeded,
        ),
        // … and a 429 into an exhausted subscription window
        (
            vec![
                assistant("m1", Some("rate_limit"), "usage limit"),
                result("Claude AI usage limit reached|1790000000", Some(429)),
            ],
            ErrorClass::QuotaExceeded,
        ),
        (
            vec![
                assistant("m1", Some("rate_limit"), "429"),
                result("API Error: 429 rate limit reached for requests", Some(429)),
            ],
            ErrorClass::RateLimited,
        ),
        // the HTTP status without a category
        (
            vec![result("request failed", Some(529))],
            ErrorClass::Overloaded,
        ),
        (vec![result("request failed", Some(401))], ErrorClass::Auth),
        // an error on an earlier message is not the turn's
        (
            vec![
                assistant("m1", Some("rate_limit"), "API Error: 429"),
                assistant("m2", None, "working"),
                result("Invalid API key · Please run /login", None),
            ],
            ErrorClass::Auth,
        ),
    ];
    for (lines, want) in cases {
        let refs: Vec<&str> = std::iter::once(INIT)
            .chain(lines.iter().map(String::as_str))
            .collect();
        let o = claude(&refs, Some(1));
        assert!(!o.success);
        assert_eq!(o.error_class, Some(want), "{lines:?}");
    }
    let max_turns =
        json!({"type":"result","subtype":"error_max_turns","is_error":true,"session_id":"s1"})
            .to_string();
    assert_eq!(
        claude(&[INIT, &max_turns], Some(1)).error_class,
        Some(ErrorClass::InvalidOutput)
    );
}

#[test]
fn claude_recovered_api_retry_does_not_decide_the_class() {
    let retry = r#"{"type":"system","subtype":"api_retry","session_id":"s1","attempt":1,"retry_delay_ms":500,"error":"overloaded","error_status":529}"#;
    for (result, want) in [
        (
            json!({"type":"result","subtype":"error_max_turns","is_error":true,"session_id":"s1"}),
            ErrorClass::InvalidOutput,
        ),
        (
            json!({"type":"result","subtype":"success","is_error":true,"session_id":"s1","result":"Prompt is too long: context window exceeded"}),
            ErrorClass::ContextWindowExceeded,
        ),
        (
            json!({"type":"result","subtype":"success","is_error":true,"session_id":"s1","result":"Invalid API key · Please run /login"}),
            ErrorClass::Auth,
        ),
    ] {
        let line = result.to_string();
        let without = claude(&[INIT, &line], Some(1));
        // retried once, then the API answered normally and the turn failed for another reason
        let with = claude(&[INIT, retry, START_900K, &line], Some(1));
        assert_eq!(without.error_class, Some(want));
        assert_eq!(
            with.error_class,
            Some(want),
            "the old retry decided: {line}"
        );
        // even without any API answer in between, a terminal result is the final word
        let direct = claude(&[INIT, retry, &line], Some(1));
        assert_eq!(direct.error_class, Some(want));
    }
}

#[test]
fn claude_retry_decides_only_while_still_retrying() {
    let retry = r#"{"type":"system","subtype":"api_retry","session_id":"s1","attempt":3,"retry_delay_ms":8000,"error":"rate_limit","error_status":429}"#;
    // the stream ended while the CLI was still retrying: the retry category is the cause
    let o = claude(&[INIT, START_900K, retry], None);
    assert_eq!(o.error_class, Some(ErrorClass::RateLimited));
    assert!(o.retryable);
    // the retry recovered (the API answered), then the process died: a crash
    let o = claude(&[INIT, retry, START_900K, THINKING_DELTA], None);
    assert_eq!(o.error_class, Some(ErrorClass::ProcessCrashed));
    assert!(o.retryable, "a retry was seen (R17)");
    assert_eq!(o.accounting, Accounting::Provisional);
}

// ── EC-05: live, cumulative usage for Claude; the terminal value is what counts ───────

#[test]
fn claude_replay_reports_live_cumulative_usage() {
    assert!(capabilities(ProfileKind::Claude).live);
    assert!(!capabilities(ProfileKind::Codex).live);
    for f in [
        "claude-review-changes-requested.jsonl",
        "claude-implementer.jsonl",
    ] {
        let ev = replay(
            ProfileKind::Claude,
            &std::fs::read_to_string(fixture(f)).unwrap(),
            Some(0),
        );
        let usage: Vec<(usize, Accounting, u64)> = ev
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match e {
                AgentEvent::Usage(u) => Some((i, u.accounting, u.total())),
                _ => None,
            })
            .collect();
        let (last, rest) = usage.split_last().unwrap();
        assert!(rest.len() >= 4, "{f}: live usage per message: {usage:?}");
        assert!(
            rest[0].0 < ev.len() / 4,
            "{f}: the first usage comes early in the turn"
        );
        assert!(rest.iter().all(|u| u.1 == Accounting::Provisional));
        assert!(
            usage.windows(2).all(|w| w[0].2 <= w[1].2),
            "{f}: cumulative for the turn: {usage:?}"
        );
        assert_eq!(
            (last.0, last.1),
            (ev.len() - 2, Accounting::Exact),
            "{f}: the terminal usage, just before the terminal event"
        );
        assert_eq!(rest.last().unwrap().2, last.2, "{f}: the stream adds up");
        // the outcome keeps the final value: nothing is counted twice
        let mut o = StageOutcome::default();
        for e in &ev {
            o.apply(e);
        }
        let u = o.usage.unwrap();
        assert_eq!((u.accounting, u.total()), (Accounting::Exact, last.2));
    }
    // Codex exec has no live usage: only the terminal one
    let ev = replay(
        ProfileKind::Codex,
        &std::fs::read_to_string(fixture("codex-implement.jsonl")).unwrap(),
        Some(0),
    );
    let n = ev
        .iter()
        .filter(|e| matches!(e, AgentEvent::Usage(_)))
        .count();
    assert_eq!(n, 1);
}

// ── EC-06: any output line is activity; heartbeats change nothing else ───────────────

#[test]
fn every_stdout_line_is_activity() {
    for (kind, lines) in [
        (
            ProfileKind::Claude,
            vec![
                THINKING_TOKENS,
                THINKING_DELTA,
                INPUT_JSON_DELTA,
                "not json",
                // its usage is reported once, by the terminal events (EC-05)
                RESULT,
            ],
        ),
        (
            ProfileKind::Codex,
            vec![
                r#"{"type":"turn.started"}"#,
                r#"{"type":"item.started","item":{"id":"i1","type":"command_execution","command":"pwsh -File scripts/build.ps1","status":"in_progress"}}"#,
                r#"{"type":"item.completed","item":{"id":"i2","type":"reasoning","text":"**Planning**"}}"#,
                r#"{"type":"item.completed","item":{"id":"i3","type":"agent_message","text":"I'll run the build now."}}"#,
                r#"{"type":"error","message":"Reconnecting... 1/5"}"#,
            ],
        ),
    ] {
        let mut live = LiveEvents::new(kind);
        for l in lines {
            assert!(line_events(kind, l.as_bytes()).is_empty(), "{l}");
            assert_eq!(live.feed(l.as_bytes()), vec![AgentEvent::Heartbeat], "{l}");
        }
    }
    // a line with its own event needs no heartbeat
    let mut live = LiveEvents::new(ProfileKind::Claude);
    assert_eq!(
        live.feed(INIT.as_bytes()),
        vec![AgentEvent::SessionIdentified("s1".into())]
    );
    // consumers see nothing of it
    let mut o = StageOutcome::default();
    o.apply(&AgentEvent::Heartbeat);
    assert_eq!(
        serde_json::to_value(&o).unwrap(),
        serde_json::to_value(StageOutcome::default()).unwrap()
    );
}

// ── TS-01: the fake follows the real contract ────────────────────────────────────────

#[tokio::test]
async fn fake_denial_then_complete_fails_like_the_real_parser() {
    let tmp = tempfile::tempdir().unwrap();
    let f = FakeAgentAdapter::new(
        serde_json::from_str::<FakeScript>(r#"{"turns":{"reviewer":[{"steps":[{"kind":"deny","tool":"Read","detail":"C:/outside"},{"kind":"complete","structured":{"decision":"approved"}}]}]}}"#).unwrap(),
    );
    let a = f.for_profile("claude-max", ProfileKind::Claude);
    let mut s = a
        .open(open(tmp.path(), "claude-max", Role::Reviewer))
        .await
        .unwrap();
    let fake = run_to_completion(s.as_mut(), input("r"), TurnLimits::default())
        .await
        .unwrap();
    // the real parser, same situation
    let real_events = replay(
        ProfileKind::Claude,
        r#"{"type":"result","subtype":"success","is_error":false,"session_id":"s","result":"x","structured_output":{"decision":"approved"},"permission_denials":[{"tool_name":"Read","tool_input":{"file_path":"C:/outside"}}]}"#,
        Some(0),
    );
    let mut real = StageOutcome::default();
    for e in &real_events {
        real.apply(e);
    }
    for o in [&fake, &real] {
        assert!(!o.success, "{o:?}");
        assert_eq!(o.error_class, Some(ErrorClass::PermissionDenied));
        assert_eq!(
            o.error_detail.as_deref(),
            Some(permission_denials_detail(1).as_str())
        );
        assert_eq!(o.permission_denials.len(), 1);
        assert_eq!(
            o.structured,
            Some(json!({"decision":"approved"})),
            "payload kept"
        );
    }
}

#[tokio::test]
async fn fake_unreadable_replay_file_fails_loudly() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no-such-fixture.jsonl");
    let script = json!({"turns":{"implementer":[{"steps":[{"kind":"replay","agent":"codex","file":missing}]}]}});
    let f = FakeAgentAdapter::new(serde_json::from_value::<FakeScript>(script).unwrap());
    let a = f.for_profile("codex-openai", ProfileKind::Codex);
    let mut s = a
        .open(open(tmp.path(), "codex-openai", Role::Implementer))
        .await
        .unwrap();
    let limits = TurnLimits {
        max: Duration::from_secs(5),
        stall: Duration::from_secs(2),
    };
    let o = run_to_completion(s.as_mut(), input("i"), limits)
        .await
        .unwrap();
    assert!(!o.success);
    assert_ne!(
        o.error_class,
        Some(ErrorClass::ProcessCrashed),
        "not a scripted crash"
    );
    let detail = o.error_detail.unwrap();
    assert!(
        detail.contains("replay") && detail.contains("no-such-fixture.jsonl"),
        "{detail}"
    );
}

// ── ARCH-05: an unusable Codex profile never runs ────────────────────────────────────

#[test]
fn codex_preflight_refuses_the_turn_without_a_process() {
    let tmp = tempfile::tempdir().unwrap();
    // spawning this would fail differently (ProcessCrashed, "starting …")
    let exe = tmp.path().join("codex-not-installed");
    let cfg = with_codex_sandbox(config(tmp.path(), &exe), "read-only", None);
    let hub = AgentHub::new(&cfg, LaunchMode::Restricted, None).unwrap();
    let out = hub
        .turn(
            open(tmp.path(), "codex-openai", Role::Implementer),
            input("implement-1"),
            TurnLimits::default(),
        )
        .unwrap();
    assert!(!out.success);
    assert_eq!(out.error_class, Some(ErrorClass::PolicyRefusal), "{out:?}");
    let detail = out.error_detail.clone().unwrap();
    assert!(detail.starts_with("preflight: "), "{detail}");
    assert!(detail.contains("sandbox_mode is `read-only`"), "{detail}");
    if cfg!(windows) {
        assert!(detail.contains("windows_sandbox"), "{detail}");
    }
    // nothing ran, so nothing was used, and no reservation is held for it
    let u = out.usage.unwrap();
    assert_eq!((u.accounting, u.total()), (Accounting::Provisional, 0));
    let cap = tmp.path().join("cap");
    assert!(cap.join("implement-1.outcome.json").is_file());
    assert!(!cap.join("implement-1.stdout.jsonl").exists(), "no process");
    // a sound profile passes the preflight (on Windows, with windows_sandbox set)
    let cfg = with_codex_sandbox(cfg, "workspace-write", Some("unelevated"));
    let hub = AgentHub::new(&cfg, LaunchMode::Restricted, None).unwrap();
    let out = hub
        .turn(
            open(tmp.path(), "codex-openai", Role::Implementer),
            input("implement-2"),
            TurnLimits::default(),
        )
        .unwrap();
    assert_ne!(out.error_class, Some(ErrorClass::PolicyRefusal), "{out:?}");
}

// ── Unix: the real CLI adapter against sh fakes ──────────────────────────────────────

#[cfg(unix)]
mod unix {
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use toryo::agents::adapter::TurnHooks;
    use toryo::agents::claude::LaunchMode;
    use toryo::agents::hub::AgentHub;
    use toryo::domain::{ErrorClass, Role, TokenUsage, TurnInput, TurnLimits};
    use toryo::usage::Accounting;

    use super::{
        config, open, with_codex_sandbox, CODEX_DONE, CODEX_START, INIT, INPUT_JSON_DELTA, RESULT,
        START_900K, THINKING_DELTA, THINKING_TOKENS,
    };

    fn sh_quote(s: &str) -> String {
        format!("'{}'", s.replace('\'', r"'\''"))
    }

    /// An executable sh fake that consumes the prompt, then runs `body`.
    fn fake(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\ncat >/dev/null\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// Lines of thinking / tool-input output in [`busy_thinking`], one every 0.25 s.
    const THINKING_LINES: usize = 16;
    /// Every line [`busy_thinking`] prints: init, message_start, the thinking, the result.
    const ALL_LINES: usize = THINKING_LINES + 3;

    /// 4 s of thinking / tool-input output (none of it maps to an event of its own), then
    /// the result.
    fn busy_thinking() -> String {
        let mut b = format!("echo {}\necho {}\n", sh_quote(INIT), sh_quote(START_900K));
        for i in 0..THINKING_LINES {
            let l = [THINKING_TOKENS, THINKING_DELTA, INPUT_JSON_DELTA][i % 3];
            b.push_str(&format!("echo {}\nsleep 0.25\n", sh_quote(l)));
        }
        b.push_str(&format!("echo {}\n", sh_quote(RESULT)));
        b
    }

    fn turn_input(label: &str) -> TurnInput {
        TurnInput {
            prompt: "review".into(),
            label: label.into(),
            env: BTreeMap::new(),
            echo: false,
        }
    }

    fn captured_lines(dir: &Path, label: &str) -> usize {
        std::fs::read_to_string(dir.join("cap").join(format!("{label}.stdout.jsonl")))
            .unwrap_or_default()
            .lines()
            .count()
    }

    #[test]
    fn live_usage_arrives_while_the_claude_turn_runs() {
        let d = tempfile::tempdir().unwrap();
        let exe = fake(d.path(), "fake-claude.sh", &busy_thinking());
        let hub = AgentHub::new(&config(d.path(), &exe), LaunchMode::Restricted, None).unwrap();
        let t0 = Instant::now();
        let seen: Arc<Mutex<Vec<(u128, u64, Accounting)>>> = Arc::default();
        let s2 = seen.clone();
        let hooks = TurnHooks {
            on_usage: Some(Arc::new(move |u: &TokenUsage| {
                s2.lock()
                    .unwrap()
                    .push((t0.elapsed().as_millis(), u.total(), u.accounting));
            })),
            ..Default::default()
        };
        let limits = TurnLimits {
            max: Duration::from_secs(60),
            stall: Duration::from_secs(60),
        };
        let out = hub
            .turn_with(
                open(d.path(), "claude-max", Role::Reviewer),
                turn_input("live"),
                limits,
                hooks,
            )
            .unwrap();
        assert!(out.success, "{out:?}");
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen.len(),
            2,
            "message_start, then the terminal usage: {seen:?}"
        );
        // message_start's 900 000 input tokens are seen at once, ~4 s before the end
        assert_eq!((seen[0].1, seen[0].2), (900_001, Accounting::Provisional));
        assert!(seen[0].0 + 3000 <= seen[1].0, "{seen:?}");
        assert_eq!((seen[1].1, seen[1].2), (900_050, Accounting::Exact));
        let u = out.usage.unwrap();
        assert_eq!((u.accounting, u.total()), (Accounting::Exact, 900_050));
    }

    /// What the engine's R41 budget hook does: set the stop flag once the turn's usage so
    /// far reaches what is left of the budget.
    #[test]
    fn usage_hook_interrupts_the_claude_turn_mid_turn() {
        let d = tempfile::tempdir().unwrap();
        let exe = fake(d.path(), "fake-claude.sh", &busy_thinking());
        let hub = AgentHub::new(&config(d.path(), &exe), LaunchMode::Restricted, None).unwrap();
        let (flag, hit) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let (f2, h2) = (flag.clone(), hit.clone());
        let hooks = TurnHooks {
            cancel: Some(flag.clone()),
            on_usage: Some(Arc::new(move |u: &TokenUsage| {
                if u.total() >= 500_000 && !h2.swap(true, Ordering::SeqCst) {
                    f2.store(true, Ordering::SeqCst);
                }
            })),
            ..Default::default()
        };
        let limits = TurnLimits {
            max: Duration::from_secs(60),
            stall: Duration::from_secs(60),
        };
        let out = hub
            .turn_with(
                open(d.path(), "claude-max", Role::Reviewer),
                turn_input("budget"),
                limits,
                hooks,
            )
            .unwrap();
        assert!(hit.load(Ordering::SeqCst));
        assert!(out.cancelled && !out.success, "{out:?}");
        let lines = captured_lines(d.path(), "budget");
        assert!(
            lines < ALL_LINES,
            "killed before the stream's end: {lines} of {ALL_LINES} lines"
        );
        let u = out.usage.unwrap();
        assert_eq!(
            (u.accounting, u.total()),
            (Accounting::Provisional, 900_001)
        );
    }

    #[test]
    fn stall_does_not_fire_while_claude_streams_thinking() {
        let d = tempfile::tempdir().unwrap();
        let exe = fake(d.path(), "fake-claude.sh", &busy_thinking());
        let hub = AgentHub::new(&config(d.path(), &exe), LaunchMode::Restricted, None).unwrap();
        // shorter than the thinking (4 s), much longer than a gap between lines (0.25 s)
        let limits = TurnLimits {
            max: Duration::from_secs(60),
            stall: Duration::from_millis(2500),
        };
        let out = hub
            .turn(
                open(d.path(), "claude-max", Role::Reviewer),
                turn_input("stall"),
                limits,
            )
            .unwrap();
        assert!(!out.timed_out, "{out:?}");
        assert!(out.success, "{out:?}");
        assert_eq!(captured_lines(d.path(), "stall"), ALL_LINES);
    }

    #[test]
    fn stall_still_fires_when_the_agent_is_silent() {
        let d = tempfile::tempdir().unwrap();
        let body = format!(
            "echo {}\nsleep 30\necho {}\n",
            sh_quote(INIT),
            sh_quote(RESULT)
        );
        let exe = fake(d.path(), "fake-claude.sh", &body);
        let hub = AgentHub::new(&config(d.path(), &exe), LaunchMode::Restricted, None).unwrap();
        let limits = TurnLimits {
            max: Duration::from_secs(60),
            stall: Duration::from_millis(1000),
        };
        let t0 = Instant::now();
        let out = hub
            .turn(
                open(d.path(), "claude-max", Role::Reviewer),
                turn_input("silent"),
                limits,
            )
            .unwrap();
        assert!(out.timed_out, "{out:?}");
        assert_eq!(out.error_class, Some(ErrorClass::Timeout));
        assert!(t0.elapsed() < Duration::from_secs(20), "{:?}", t0.elapsed());
    }

    #[test]
    fn codex_turn_runs_when_the_preflight_passes() {
        let d = tempfile::tempdir().unwrap();
        let marker = d.path().join("spawned");
        let mut body = format!("touch {}\n", sh_quote(&marker.display().to_string()));
        for l in CODEX_START
            .iter()
            .copied()
            .chain([r#"{"type":"error","message":"Reconnecting... 1/5 (stream disconnected before completion)"}"#])
            .chain(CODEX_DONE.iter().copied())
        {
            body.push_str(&format!("echo {}\n", sh_quote(l)));
        }
        let exe = fake(d.path(), "fake-codex.sh", &body);
        let refused = with_codex_sandbox(config(d.path(), &exe), "danger-full-access", None);
        let hub = AgentHub::new(&refused, LaunchMode::Restricted, None).unwrap();
        let out = hub
            .turn(
                open(d.path(), "codex-openai", Role::Implementer),
                turn_input("implement-1"),
                TurnLimits::default(),
            )
            .unwrap();
        assert_eq!(out.error_class, Some(ErrorClass::PolicyRefusal));
        assert!(!marker.exists(), "no process was spawned");
        let sound = with_codex_sandbox(refused, "workspace-write", Some("unelevated"));
        let hub = AgentHub::new(&sound, LaunchMode::Restricted, None).unwrap();
        let out = hub
            .turn(
                open(d.path(), "codex-openai", Role::Implementer),
                turn_input("implement-2"),
                TurnLimits::default(),
            )
            .unwrap();
        assert!(marker.exists());
        // EC-02 end to end: the reconnect notice does not fail the completed turn
        assert!(out.success, "{out:?}");
        assert_eq!(out.session_id.as_deref(), Some("th_1"));
        let outcome =
            std::fs::read_to_string(d.path().join("cap/implement-2.outcome.json")).unwrap();
        assert!(
            outcome.contains("Reconnecting... 1/5"),
            "kept as a warning: {outcome}"
        );
    }
}

// ── Linux: CPU accounting and a device that fails writes ─────────────────────────────

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    use toryo::agents::claude::LaunchMode;
    use toryo::agents::hub::AgentHub;
    use toryo::domain::{ErrorClass, Role, TurnInput, TurnLimits};
    use toryo::process::env::build_env;
    use toryo::process::redact::Redactions;
    use toryo::process::{spawn, Boundary, SpawnSpec, WaitOptions};
    use toryo::usage::Accounting;

    use super::{config, open, INIT, RESULT, START_900K};

    /// CPU time of the calling thread (the one that runs `Running::wait`).
    fn thread_cpu_ms() -> u128 {
        // SAFETY: an all-zero rusage is valid, and getrusage only writes into it.
        let ru = unsafe {
            let mut ru: libc::rusage = std::mem::zeroed();
            libc::getrusage(libc::RUSAGE_THREAD, &mut ru);
            ru
        };
        let us = |t: libc::timeval| t.tv_sec as u128 * 1_000_000 + t.tv_usec as u128;
        (us(ru.ru_utime) + us(ru.ru_stime)) / 1000
    }

    #[test]
    fn wait_does_not_spin_after_both_pipes_close() {
        let spec = SpawnSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "exec >/dev/null 2>/dev/null; sleep 2".into()],
            cwd: std::env::temp_dir(),
            env: build_env(std::env::vars(), &BTreeMap::new(), &BTreeMap::new()),
            stdin: None,
            boundary: Boundary::None,
        };
        let r = spawn(&spec, &Redactions::empty()).unwrap();
        let (c0, t0) = (thread_cpu_ms(), Instant::now());
        let info = r.wait(WaitOptions::default(), |_| {}).unwrap();
        let (cpu, wall) = (thread_cpu_ms() - c0, t0.elapsed().as_millis());
        assert_eq!(info.code, Some(0));
        assert!(wall >= 1500, "the child ran to its end: {wall} ms");
        assert!(cpu < 250, "{cpu} ms CPU over {wall} ms wall: a busy wait");
    }

    /// CODE-07: a capture that cannot be written fails the turn instead of silently
    /// truncating the evidence; the parsed usage is still reported.
    #[test]
    fn capture_write_failure_fails_the_turn() {
        let d = tempfile::tempdir().unwrap();
        let exe = d.path().join("fake-claude.sh");
        std::fs::write(
            &exe,
            format!(
                "#!/bin/sh\ncat >/dev/null\necho '{INIT}'\necho '{START_900K}'\necho '{RESULT}'\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cap = d.path().join("cap");
        std::fs::create_dir_all(&cap).unwrap();
        // every write to the stdout capture fails with ENOSPC ("disk full")
        std::os::unix::fs::symlink("/dev/full", cap.join("full.stdout.jsonl")).unwrap();
        let hub = AgentHub::new(&config(d.path(), &exe), LaunchMode::Restricted, None).unwrap();
        let input = TurnInput {
            prompt: "review".into(),
            label: "full".into(),
            env: BTreeMap::new(),
            echo: false,
        };
        let out = hub
            .turn(
                open(d.path(), "claude-max", Role::Reviewer),
                input,
                TurnLimits::default(),
            )
            .unwrap();
        assert!(!out.success, "{out:?}");
        assert_eq!(out.error_class, Some(ErrorClass::Unknown));
        let detail = out.error_detail.unwrap();
        assert!(
            detail.contains("capture incomplete") && detail.contains("full.stdout.jsonl"),
            "{detail}"
        );
        let u = out.usage.unwrap();
        assert_eq!((u.accounting, u.total()), (Accounting::Exact, 900_050));
    }
}
