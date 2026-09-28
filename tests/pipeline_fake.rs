//! The kit's pipeline e2e scenarios on `FakeAgentAdapter` (plan §17 M1): no agent
//! processes, no shell scripts, so they run on Windows and Linux alike. The agents are
//! scripted per role (`role` or `role@profile`); verification is a real command (`git grep`)
//! run through `LocalRunner`.

#![cfg(feature = "m0-kit")]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
    data: PathBuf,
    repo: PathBuf,
}

const DIVIDE: &str = "\nfunction Divide-Numbers { param($A,$B) if ($B -eq 0) { throw 'Division by zero' }; $A / $B }\n";

fn plan() -> Value {
    json!({"kind":"complete","structured":{"understanding":"u","files_likely_involved":["src/Calculator.psm1"],"steps":["add Divide-Numbers"],"validation":["tests"],"plan_markdown":"## Plan\n- add Divide-Numbers"}})
}

fn implementation(resolved: &[&str]) -> Value {
    json!({"kind":"complete","structured":{"summary":"s","deviations":[],"validation_performed":["read the module"],"open_issues":[],"resolved":resolved}})
}

fn review_changes() -> Value {
    json!({"kind":"complete","structured":{"decision":"changes_requested","summary":"s","acceptance_criteria":[
        {"id":"AC-1","met":true,"evidence":"e1"},{"id":"AC-2","met":false,"evidence":"no zero-divisor test"},{"id":"AC-3","met":true,"evidence":"e3"}],
        "issues":[{"id":"R-1","severity":"major","blocking":true,"file":"tests/Divide.Tests.ps1","description":"add the zero-divisor test"}]}})
}

fn review_approved() -> Value {
    json!({"kind":"complete","structured":{"decision":"approved","summary":"s","acceptance_criteria":[
        {"id":"AC-1","met":true,"evidence":"e1"},{"id":"AC-2","met":true,"evidence":"e2"},{"id":"AC-3","met":true,"evidence":"e3"}],
        "issues":[{"id":"R-9","severity":"low","blocking":false,"file":"src/Calculator.psm1","description":"nit: naming"}]}})
}

fn write(path: &str, content: &str) -> Value {
    json!({"kind":"write_file","path":path,"content":content,"append":true})
}

fn usage() -> Value {
    json!({"kind":"usage","input":100,"cache_read":40,"output":10})
}

fn turn(steps: Vec<Value>) -> Value {
    json!({ "steps": steps })
}

fn toryo(env: &Env, args: &[&str], stdin: &str, extra_env: &[(&str, &str)]) -> (i32, String) {
    use std::io::Write;
    let mut c = Command::new(env!("CARGO_BIN_EXE_toryo"));
    c.arg("--config")
        .arg(&env.config)
        .arg("--machine-config")
        .arg(env.root.join("none.yaml"))
        .args(args)
        .env("TORYO_DATA_DIR", &env.data)
        .current_dir(&env.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra_env {
        c.env(k, v);
    }
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let o = child.wait_with_output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    if std::env::var("SHOW").is_ok() {
        eprintln!("{err}");
    }
    (o.status.code().unwrap_or(-1), err)
}

/// A calculator fixture repo and a config whose verify step is `git grep Divide-Numbers`.
/// `reviewer` sets `roles.reviewer`; `fallback` sets `roles.implementer.fallback`.
fn setup(reviewer: Option<&str>, fallback: &[&str]) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let repo = root.join("calc");
    let data = root.join("data");
    let config = root.join("toryo.yaml");
    let fwd = |p: &Path| p.display().to_string().replace('\\', "/");
    let mut yaml = format!(
        r#"version: 1
project:
  root: "{repo}"
  worktrees_root: "{wts}"
profiles:
  claude-max: {{ kind: claude, billing: subscription, exe: claude-not-installed }}
  claude-review: {{ kind: claude, billing: subscription, exe: claude-not-installed }}
  codex-openai: {{ kind: codex, billing: subscription, exe: codex-not-installed }}
roles:
  planner: {{ profile: claude-max }}
  implementer: {{ profile: codex-openai, fallback: [{fb}] }}
commands:
  verify:
    - {{ name: divide-present, program: git, args: ["grep", "-q", "Divide-Numbers", "--", "src"] }}
"#,
        repo = fwd(&repo),
        wts = fwd(&root.join("wts")),
        fb = fallback.join(", "),
    );
    if let Some(r) = reviewer {
        yaml = yaml.replace(
            "commands:",
            &format!("  reviewer: {{ profile: {r} }}\ncommands:"),
        );
    }
    std::fs::write(&config, yaml).unwrap();
    let env = Env {
        _tmp: tmp,
        root,
        config,
        data,
        repo,
    };
    let (code, err) = toryo(
        &env,
        &["spike", "init-fixture", "--into", env.repo.to_str().unwrap()],
        "",
        &[],
    );
    assert_eq!(code, 0, "{err}");
    env
}

fn script(env: &Env, turns: Value) -> PathBuf {
    let p = env.root.join("agents.json");
    std::fs::write(&p, serde_json::to_vec_pretty(&json!({ "turns": turns })).unwrap()).unwrap();
    p
}

fn pipeline(env: &Env, script: &Path, id: &str, extra: &[&str], stdin: &str, extra_env: &[(&str, &str)]) -> (i32, String) {
    let task = env.repo.join("TASK.md");
    let mut args = vec![
        "spike",
        "pipeline",
        "--repo",
        env.repo.to_str().unwrap(),
        "--task",
        task.to_str().unwrap(),
        "--id",
        id,
        "--fake-agents",
        script.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    toryo(env, &args, stdin, extra_env)
}

/// (pipeline record, its steps, the fake calls)
fn outcome(env: &Env) -> (Value, Vec<Value>, Vec<Value>) {
    let text = std::fs::read_to_string(env.data.join("m0").join("results.jsonl")).unwrap_or_default();
    let last: Value = text
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|r| r["spike"] == "pipeline")
        .expect("a pipeline record");
    let steps = last["details"]["steps"].as_array().cloned().unwrap_or_default();
    let run = PathBuf::from(last["run_dir"].as_str().unwrap());
    let calls: Vec<Value> =
        serde_json::from_slice(&std::fs::read(run.join("fake-calls.json")).unwrap()).unwrap();
    (last, steps, calls)
}

fn calls_for<'a>(calls: &'a [Value], key: &str) -> Vec<&'a Value> {
    calls.iter().filter(|c| c["key"] == key).collect()
}

fn git(repo: &Path, args: &[&str]) -> String {
    let o = Command::new("git").arg("-C").arg(repo).args(args).output().unwrap();
    String::from_utf8_lossy(&o.stdout).to_string()
}

#[test]
fn gate_reject_resumes_planner_then_fix_cycle_and_approval() {
    let env = setup(None, &[]);
    let s = script(
        &env,
        json!({
            "planner": [turn(vec![usage(), plan()]), turn(vec![usage(), plan()])],
            "implementer": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), usage(), implementation(&[])]),
                turn(vec![write("tests/Divide.Tests.ps1", "# zero-divisor test\n"), implementation(&["R-1"])]),
            ],
            "reviewer": [turn(vec![review_changes()]), turn(vec![review_approved()])],
        }),
    );
    let (code, err) = pipeline(&env, &s, "CALC-001", &[], "n\nplease also cover negative numbers\ny\n", &[]);
    assert_eq!(code, 0, "{err}");
    let (rec, steps, calls) = outcome(&env);
    assert_eq!(rec["pass"], true, "{err}");
    let has = |f: &dyn Fn(&Value) -> bool| steps.iter().any(f);
    assert!(has(&|s| s["step"] == "plan-gate" && s["decision"] == "rejected" && s["reason"] == "please also cover negative numbers"));
    assert!(has(&|s| s["step"] == "plan-gate" && s["decision"] == "approved"));
    assert!(has(&|s| s["step"] == "review-0" && s["decision"] == "changes_requested"));
    assert!(has(&|s| s["step"] == "fix-1" && s["verify"] == true));
    assert!(has(&|s| s["step"] == "review-1" && s["decision"] == "approved"));

    // the rejected plan is revised in the same planner session, with the reason
    let planner = calls_for(&calls, "planner");
    assert_eq!(planner.len(), 2);
    assert!(planner[0]["resume"].is_null());
    assert!(planner[1]["resume"].is_string(), "{calls:?}");
    assert!(planner[1]["prompt"].as_str().unwrap().contains("please also cover negative numbers"));
    // every reviewer turn is a fresh session (correction turns aside)
    assert!(calls_for(&calls, "reviewer").iter().all(|c| c["resume"].is_null()));

    // checkpoints on the task branch with trailers; the fix prompt carries the review
    let log = git(&env.repo, &["log", "--format=%s|%(trailers:key=Toryo-Stage,valueonly)", "main..toryo/CALC-001"]);
    assert!(log.contains("toryo(CALC-001): implement|implement"), "{log}");
    assert!(log.contains("toryo(CALC-001): fix-1|fix-1"), "{log}");
    let run = PathBuf::from(rec["run_dir"].as_str().unwrap());
    let fix_prompt = std::fs::read_to_string(run.join("fix-1.prompt.md")).unwrap();
    assert!(fix_prompt.contains("R-1") && fix_prompt.contains("AC-2 NOT MET"), "{fix_prompt}");
    for f in ["TASK.md", "PLAN.md", "DIFF.patch", "DIFF-incremental.patch", "VERIFY_RESULT.json", "REVIEW.md", "SUMMARY.md"] {
        assert!(run.join("runstore").join(f).exists(), "missing {f}");
    }
    let summary = std::fs::read_to_string(run.join("runstore").join("SUMMARY.md")).unwrap();
    assert!(summary.contains("R-9"), "{summary}");
    // usage flowed through the events: 2 planner turns + 1 implementer turn
    assert_eq!(rec["details"]["tokens"]["output"], 30, "{}", rec["details"]["tokens"]);
}

#[test]
fn fallback_is_sticky_and_keeps_the_configured_reviewer() {
    let env = setup(Some("claude-review"), &["claude-max"]);
    let s = script(
        &env,
        json!({
            "planner": [turn(vec![plan()])],
            "implementer@codex-openai": [turn(vec![
                write("src/Partial.ps1", "# partial work by codex\n"),
                json!({"kind":"fail","class":"QuotaExceeded","detail":"You've hit your usage limit."}),
            ])],
            "implementer@claude-max": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), implementation(&[])]),
                turn(vec![write("tests/Divide.Tests.ps1", "# zero-divisor test\n"), implementation(&["R-1"])]),
            ],
            "reviewer": [turn(vec![review_changes()]), turn(vec![review_approved()])],
        }),
    );
    let (code, err) = pipeline(&env, &s, "CALC-FB", &["--yes"], "", &[]);
    assert_eq!(code, 0, "{err}");
    let (rec, steps, calls) = outcome(&env);
    assert_eq!(rec["pass"], true);
    // Codex once; Claude for implement and the fix cycle (sticky)
    assert_eq!(calls_for(&calls, "implementer@codex-openai").len(), 1);
    let claude = calls_for(&calls, "implementer@claude-max");
    assert_eq!(claude.len(), 2, "{calls:?}");
    assert_eq!(claude[0]["label"], "implement-2");
    // the hand-over names the partial work left in the worktree
    assert!(claude[0]["prompt"].as_str().unwrap().contains("Partial.ps1"));
    // owner decision 15: the reviewer is the configured one, never the fallback
    let reviewers = calls_for(&calls, "reviewer");
    assert_eq!(reviewers.len(), 2);
    assert!(reviewers.iter().all(|c| c["profile"] == "claude-review"), "{reviewers:?}");
    assert!(steps.iter().any(|s| s.get("implementer_switch").is_some()));
    // the partial Codex file is part of the first checkpoint
    let files = git(&env.repo, &["log", "--name-only", "--format=", "main..toryo/CALC-FB"]);
    assert!(files.contains("src/Partial.ps1"), "{files}");
    let run = PathBuf::from(rec["run_dir"].as_str().unwrap());
    let summary = std::fs::read_to_string(run.join("runstore").join("SUMMARY.md")).unwrap();
    assert!(summary.contains("Implementer fallback"), "{summary}");
}

#[test]
fn quit_at_the_gate_runs_no_implementer() {
    let env = setup(None, &[]);
    let s = script(&env, json!({ "planner": [turn(vec![plan()])] }));
    let (code, err) = pipeline(&env, &s, "CALC-Q", &[], "q\n", &[]);
    assert_ne!(code, 0);
    let (rec, _, calls) = outcome(&env);
    assert_eq!(rec["pass"], false);
    assert!(rec["summary"].as_str().unwrap().contains("cancelled at the plan gate"), "{err}");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["key"], "planner");
}

#[test]
fn rate_limit_retry_verify_failure_and_review_correction() {
    let env = setup(None, &[]);
    let s = script(
        &env,
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![json!({"kind":"fail","class":"RateLimited","detail":"429"})]),
                // forgets the function: verify fails and goes to Fixing, not Review
                turn(vec![write("notes.md", "wip\n"), implementation(&[])]),
                turn(vec![write("src/Calculator.psm1", DIVIDE), implementation(&[])]),
            ],
            "reviewer": [
                // AC-3 missing: rejected by the validator, corrected in the same session
                turn(vec![json!({"kind":"complete","structured":{"decision":"approved","summary":"s","acceptance_criteria":[
                    {"id":"AC-1","met":true,"evidence":"e"},{"id":"AC-2","met":true,"evidence":"e"}],"issues":[]}})]),
                turn(vec![review_approved()]),
            ],
        }),
    );
    let (code, err) = pipeline(&env, &s, "CALC-R", &["--yes"], "", &[("TORYO_RETRY_BASE_SECS", "0")]);
    assert_eq!(code, 0, "{err}");
    let (rec, steps, calls) = outcome(&env);
    assert_eq!(rec["pass"], true);
    assert!(steps.iter().any(|s| s["retry"] == 1 && s["error"] == "RateLimited"), "{steps:?}");
    assert!(steps.iter().any(|s| s["step"] == "implement" && s["verify"] == false), "{steps:?}");
    assert!(steps.iter().any(|s| s["step"] == "fix-1" && s["verify"] == true), "{steps:?}");
    let run = PathBuf::from(rec["run_dir"].as_str().unwrap());
    let fix_prompt = std::fs::read_to_string(run.join("fix-1.prompt.md")).unwrap();
    assert!(fix_prompt.contains("VERIFY FAILED"), "{fix_prompt}");
    let reviewers = calls_for(&calls, "reviewer");
    assert_eq!(reviewers.len(), 2);
    assert_eq!(reviewers[1]["label"], "review-1-1");
    assert!(reviewers[1]["resume"].is_string(), "correction resumes the reviewer session");
    assert!(reviewers[1]["prompt"].as_str().unwrap().contains("AC-3"));
}

#[test]
fn all_implementers_out_of_quota_stops_and_keeps_partial_work() {
    let env = setup(None, &[]);
    let quota = || turn(vec![write("src/Partial.ps1", "# partial\n"), json!({"kind":"fail","class":"QuotaExceeded","detail":"usage limit"})]);
    let s = script(
        &env,
        json!({
            "planner": [turn(vec![plan()])],
            "implementer@codex-openai": [quota()],
            "implementer@claude-max": [quota()],
        }),
    );
    // no configured fallback: the planner's Claude profile is the default fallback
    let (code, err) = pipeline(&env, &s, "CALC-X", &["--yes"], "", &[]);
    assert_ne!(code, 0);
    let (rec, _, calls) = outcome(&env);
    assert!(rec["summary"].as_str().unwrap().contains("out of quota"), "{err}");
    assert_eq!(calls.iter().filter(|c| c["key"].as_str().unwrap().starts_with("implementer")).count(), 2);
    assert!(env.root.join("wts").join("CALC-X").join("src").join("Partial.ps1").exists());
}
