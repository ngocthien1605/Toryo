//! End-to-end run of the spike commands against FAKE `claude` / `codex` executables
//! (shell scripts speaking the documented stream formats). This proves the harness —
//! argv, env, Job, redaction, capture, parsing, checks, evidence records — before the
//! first real run on Windows. Unix-only (the fakes are sh scripts).
#![cfg(all(unix, feature = "m0-kit"))]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const FAKE_CLAUDE: &str = r#"#!/bin/sh
[ "$1" = "--version" ] && { echo "${FAKE_VERSION:-2.1.300} (Claude Code)"; exit 0; }
prompt=$(cat)
ctx=""; prev=""; resume=0
for a in "$@"; do
  [ "$prev" = "--append-system-prompt-file" ] && ctx="$a"
  [ "$a" = "--resume" ] && resume=1
  prev="$a"
done
case " $* " in *" --restricted "*) ;; *) echo '{"type":"result","subtype":"error_during_execution","is_error":true,"session_id":"s","result":"missing --restricted"}'; exit 1;; esac
[ -n "$FAKE_SECRET" ] && printf 'leak %s\n' "$FAKE_SECRET" >&2
probe=$(printf '%s' "$prompt" | grep -o '[^ `]*probe.txt' | head -1)
token=""; [ -n "$probe" ] && token=$(cat "$probe")
code=""; [ -n "$ctx" ] && code=$(grep -o 'CONTEXT-[0-9a-f]*' "$ctx" | head -1)
echo '{"type":"system","subtype":"init","session_id":"fake-1","tools":["Read","Grep","Glob"],"mcp_servers":[]}'
echo '{"type":"stream_event","session_id":"fake-1","event":{"type":"message_start","message":{"id":"m1","usage":{"input_tokens":5,"cache_read_input_tokens":100,"cache_creation_input_tokens":0,"output_tokens":1}}}}'
echo '{"type":"stream_event","session_id":"fake-1","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"working "}}}'
if [ $resume = 0 ]; then case "$prompt" in *3000-word*) sleep 30;; esac; fi
secretp=$(printf '%s' "$prompt" | grep -o '[^ `]*secret.txt' | head -1)
denials='[]'; [ -n "$probe" ] || [ -n "$FAKE_DENY" ] && denials="[{\"tool_name\":\"Read\",\"tool_input\":{\"file_path\":\"${secretp:-outside}\"}}]"
case "$prompt" in
  *"Role: Implementer"*)
    case " $* " in *" --permission-mode acceptEdits "*) ;; *) echo '{"type":"result","subtype":"error_during_execution","is_error":true,"session_id":"s","result":"implementer needs acceptEdits"}'; exit 1;; esac
    case " $* " in *"Bash"*|*" --add-dir "*) echo '{"type":"result","subtype":"error_during_execution","is_error":true,"session_id":"s","result":"implementer must not get Bash or --add-dir"}'; exit 1;; esac
    [ -n "$FAKE_STATE" ] && printf '%s\n=====\n' "$prompt" >> "$FAKE_STATE.claude-impl-prompt"
    printf 'function Divide-Numbers { param($A,$B) $A / $B }\n' >> src/Calculator.psm1
    mkdir -p tests; echo '# test by claude' > tests/Divide.Tests.ps1
    echo '{"type":"result","subtype":"success","is_error":false,"session_id":"fake-impl","result":"i","structured_output":{"summary":"s","deviations":[],"validation_performed":[],"open_issues":[],"resolved":[]},"usage":{"input_tokens":5,"output_tokens":20},"permission_denials":[]}'; exit 0;;
  *"Create the plan"*) echo '{"type":"result","subtype":"success","is_error":false,"session_id":"fake-1","result":"p","structured_output":{"understanding":"u","files_likely_involved":["src/Calculator.psm1"],"steps":["add Divide-Numbers"],"validation":["tests"],"plan_markdown":"md"},"usage":{"input_tokens":5,"output_tokens":20},"permission_denials":[]}'; exit 0;;
  *"Review the checkpoint"*) if [ -n "$FAKE_STATE" ] && [ ! -e "$FAKE_STATE.reviewed" ]; then touch "$FAKE_STATE.reviewed"; echo '{"type":"result","subtype":"success","is_error":false,"session_id":"fake-1","result":"r","structured_output":{"decision":"changes_requested","summary":"s","acceptance_criteria":[{"id":"AC-1","met":true,"evidence":"e1"},{"id":"AC-2","met":false,"evidence":"no zero-divisor test"},{"id":"AC-3","met":true,"evidence":"e3"}],"issues":[{"id":"R-1","severity":"major","blocking":true,"file":"tests/Divide.Tests.ps1","description":"add the zero-divisor test"}]},"usage":{"input_tokens":5,"output_tokens":20},"permission_denials":[]}'; exit 0; fi
    echo '{"type":"result","subtype":"success","is_error":false,"session_id":"fake-1","result":"r","structured_output":{"decision":"approved","summary":"s","acceptance_criteria":[{"id":"AC-1","met":true,"evidence":"e1"},{"id":"AC-2","met":true,"evidence":"e2"},{"id":"AC-3","met":true,"evidence":"e3"}],"issues":[{"id":"R-9","severity":"low","blocking":false,"file":"src/Calculator.psm1","description":"nit: naming"}]},"usage":{"input_tokens":5,"output_tokens":20},"permission_denials":[]}'; exit 0;;
esac
printf '{"type":"result","subtype":"success","is_error":false,"session_id":"fake-1","result":"done","structured_output":{"run_store_token":"%s","outside_file_content":"","special_instructions":"%s","ok":true,"note":"x"},"usage":{"input_tokens":5,"cache_read_input_tokens":100,"cache_creation_input_tokens":0,"output_tokens":20},"permission_denials":%s}\n' "$token" "$code" "$denials"
"#;

const FAKE_CODEX: &str = r#"#!/bin/sh
[ "$1" = "--version" ] && { echo "codex-cli ${FAKE_VERSION:-0.200.0}"; exit 0; }
prompt=$(cat)
echo '{"type":"thread.started","thread_id":"th-fake"}'
echo '{"type":"turn.started"}'
if [ "$FAKE_CODEX_QUOTA" = 1 ] && printf '%s' "$prompt" | grep -q "Role: Implementer"; then
  echo '# partial work by codex' > src/Partial.ps1
  echo '{"type":"error","message":"You'"'"'ve hit your usage limit. Upgrade to Pro or try again later."}'
  echo '{"type":"turn.failed","error":{"message":"You'"'"'ve hit your usage limit. Upgrade to Pro or try again later."}}'
  exit 1
fi
if printf '%s' "$prompt" | grep -q "Role: Implementer"; then
  [ -n "$TEMP" ] && echo scratch > "$TEMP/fake-temp.txt"
  printf 'function Divide-Numbers { param($A,$B) $A / $B }\n' >> src/Calculator.psm1
  mkdir -p tests; echo '# test' > tests/Divide.Tests.ps1
fi
if printf '%s' "$prompt" | grep -q "inside.txt"; then
  echo ok > inside.txt
  if [ "$FAKE_ESCAPE" = 1 ]; then
    outside=$(printf '%s' "$prompt" | grep -o '[^ `]*outside.txt' | head -1)
    echo escaped > "$outside"
  fi
  echo '{"type":"item.completed","item":{"id":"i1","type":"command_execution","command":"Set-Content ../outside.txt escaped","exit_code":1,"status":"failed"}}'
  echo '{"type":"item.completed","item":{"id":"i2","type":"command_execution","command":"git commit --allow-empty -m canary","exit_code":1,"status":"failed"}}'
  if [ "$FAKE_LAZY" != 1 ]; then
    echo '{"type":"item.completed","item":{"id":"i3","type":"file_change","changes":[{"path":"C:/x/runstore/TASK.md","kind":"update"}],"status":"failed"}}'
  fi
fi
case " $* " in *" resume "*) ;; *) case "$prompt" in *3000-word*) sleep 30;; esac;; esac
echo '{"type":"item.completed","item":{"id":"i2","type":"agent_message","text":"{\"summary\":\"s\",\"deviations\":[],\"validation_performed\":[],\"open_issues\":[],\"resolved\":[],\"ok\":true,\"note\":\"n\"}"}}'
echo '{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":10}}'
"#;

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
    data: PathBuf,
}

fn setup(escape: bool) -> Env {
    setup_with(escape, false)
}

fn setup_with(escape: bool, lazy: bool) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let claude = script(&root, "fake-claude", FAKE_CLAUDE);
    let codex = script(&root, "fake-codex", FAKE_CODEX);
    let repo = root.join("project");
    std::fs::create_dir_all(&repo).unwrap();
    let data = root.join("data");
    let config = root.join("toryo.yaml");
    std::fs::write(
        &config,
        format!(
            r#"version: 1
project:
  root: {repo}
  worktrees_root: {wts}
profiles:
  claude-max:
    kind: claude
    billing: subscription
    exe: {claude}
    env:
      FAKE_DENY: "{deny}"
    secret_env:
      FAKE_SECRET: env:TORYO_E2E_SECRET
  codex-foundry:
    kind: codex
    billing: foundry
    exe: {codex}
    env:
      FAKE_ESCAPE: "{esc}"
      FAKE_LAZY: "{lazy}"
      FAKE_CODEX_QUOTA: "{quota}"
    codex:
      windows_sandbox: unelevated
      provider: {{ id: foundry, name: Azure Foundry, base_url: "https://x.openai.azure.com/openai/v1", env_key: AZURE_OPENAI_API_KEY }}
roles:
  planner: {{ profile: claude-max }}
  implementer: {{ profile: codex-foundry }}
"#,
            repo = repo.display(),
            wts = root.join("wts").display(),
            claude = claude.display(),
            codex = codex.display(),
            esc = if escape { "1" } else { "0" },
            deny = if escape { "1" } else { "" },
            lazy = if lazy { "1" } else { "0" },
            quota = "0",
        ),
    )
    .unwrap();
    Env {
        _tmp: tmp,
        root,
        config,
        data,
    }
}

fn toryo(env: &Env, args: &[&str]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_toryo"))
        .arg("--config")
        .arg(&env.config)
        .arg("--machine-config")
        .arg(env.root.join("none.yaml"))
        .args(args)
        .env("TORYO_DATA_DIR", &env.data)
        .env("TORYO_E2E_SECRET", "s3cr3t-value-e2e")
        .current_dir(&env.root)
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into(),
        String::from_utf8_lossy(&o.stderr).into(),
    )
}

fn records(env: &Env) -> Vec<serde_json::Value> {
    std::fs::read_to_string(env.data.join("m0").join("results.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

#[test]
fn claude_isolation_probe_passes_with_well_behaved_fake() {
    let env = setup(false);
    let (code, _out, err) = toryo(
        &env,
        &["spike", "claude-isolation", "--profile", "claude-max"],
    );
    assert_eq!(code, 0, "{err}");
    let r = records(&env);
    assert_eq!(r.last().unwrap()["spike"], "claude-isolation");
    assert_eq!(r.last().unwrap()["pass"], true);
    // Secret printed by the fake on stderr must be redacted in every captured file.
    let mut files = Vec::new();
    walk(&env.data, &mut files);
    for f in files {
        let s = std::fs::read(&f).unwrap();
        assert!(
            !s.windows(16).any(|w| w == b"s3cr3t-value-e2e"),
            "secret leaked into {}",
            f.display()
        );
    }
}

#[test]
fn claude_turn_writes_structured_output_and_fixture_capture() {
    let env = setup(false);
    let prompt = env.root.join("p.md");
    std::fs::write(&prompt, "Plan something").unwrap();
    let rs = env.root.join("rs");
    std::fs::create_dir_all(&rs).unwrap();
    let out = env.root.join("plan.json");
    let (code, _o, err) = toryo(
        &env,
        &[
            "spike",
            "claude",
            "--profile",
            "claude-max",
            "--prompt-file",
            prompt.to_str().unwrap(),
            "--run-store",
            rs.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--label",
            "planner",
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.exists());
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(v["ok"], true);
    // With a denial the stage fails by design (plan §6.2): the "escape" env sets FAKE_DENY.
    let env2 = setup(true);
    let rs2 = env2.root.join("rs");
    std::fs::create_dir_all(&rs2).unwrap();
    let p2 = env2.root.join("p.md");
    std::fs::write(&p2, "Plan something").unwrap();
    let (code, _o, err) = toryo(
        &env2,
        &[
            "spike",
            "claude",
            "--profile",
            "claude-max",
            "--prompt-file",
            p2.to_str().unwrap(),
            "--run-store",
            rs2.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("permission denials: 1"));
    let r = records(&env2);
    assert_eq!(
        r.last().unwrap()["details"]["outcome"]["error_class"],
        "PermissionDenied"
    );
}

#[test]
fn claude_launch_without_restricted_is_caught() {
    let env = setup(false);
    let prompt = env.root.join("p.md");
    std::fs::write(&prompt, "x").unwrap();
    let rs = env.root.join("rs");
    std::fs::create_dir_all(&rs).unwrap();
    let (code, _o, err) = toryo(
        &env,
        &[
            "spike",
            "claude",
            "--profile",
            "claude-max",
            "--prompt-file",
            prompt.to_str().unwrap(),
            "--run-store",
            rs.to_str().unwrap(),
            "--drop-flag",
            "--restricted",
        ],
    );
    assert_eq!(code, 1);
    assert!(err.contains("missing --restricted"), "{err}");
}

#[test]
fn codex_canary_passes_when_nothing_escapes_and_fails_when_it_does() {
    let good = setup(false);
    let (code, _o, err) = toryo(
        &good,
        &["spike", "codex-canary", "--profile", "codex-foundry"],
    );
    assert_eq!(code, 0, "{err}");
    let bad = setup(true);
    let (code, _o, err) = toryo(
        &bad,
        &["spike", "codex-canary", "--profile", "codex-foundry"],
    );
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("[FAIL] write outside the worktree blocked"),
        "{err}"
    );
    // A model that skips a step must not produce a PASS (nothing was tested).
    let lazy = setup_with(false, true);
    let (code, _o, err) = toryo(
        &lazy,
        &["spike", "codex-canary", "--profile", "codex-foundry"],
    );
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("[FAIL] attempted: append to the run store TASK.md"),
        "{err}"
    );
    // doctor must now report the Codex profile as unusable
    let (dcode, dout, _) = toryo(&bad, &["doctor"]);
    assert_eq!(dcode, 1);
    assert!(dout.contains("codex-canary"), "{dout}");
}

#[test]
fn killed_resume_records_provisional_and_unknown_usage() {
    for profile in ["claude-max", "codex-foundry"] {
        let env = setup(false);
        let (code, _o, err) = toryo(
            &env,
            &[
                "spike",
                "killed-resume",
                "--profile",
                profile,
                "--kill-after-secs",
                "2",
            ],
        );
        assert_eq!(code, 0, "{profile}: {err}");
        let r = records(&env);
        let last = r.last().unwrap();
        assert_eq!(last["pass"], true, "{profile}: {last}");
        let acc = last["details"]["killed"]["accounting"].as_str().unwrap();
        match profile {
            "claude-max" => assert_eq!(acc, "provisional"),
            _ => assert_eq!(acc, "unknown", "Codex exec has no live usage: never 0"),
        }
        assert_eq!(last["details"]["killed"]["exit"]["tree_gone"], true);
    }
}

#[test]
fn job_test_stop_and_fault_injection_pass_on_unix() {
    let env = setup(false);
    let (_code, _o, err) = toryo(&env, &["spike", "job-test"]);
    let r = records(&env);
    let res = r.last().unwrap()["details"]["results"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(res[0]["ok"], true, "stop: {err}");
    assert_eq!(res[2]["ok"], true, "fault injection: {err}");
    // res[1] (crash cleanup) is a Windows-only guarantee
}

#[test]
fn git_canary_and_report_and_render() {
    let env = setup(false);
    let (code, _o, err) = toryo(&env, &["spike", "git-canary"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, _) = toryo(&env, &["spike", "report"]);
    assert_eq!(code, 0);
    assert!(out.contains("git-canary") && out.contains("PASS"));
    let fx = env.root.join("calc");
    let (code, _o, err) = toryo(
        &env,
        &[
            "spike",
            "init-fixture",
            "--into",
            fx.to_str().unwrap(),
            "--contracts",
            env.root.join("contracts").to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert!(env
        .root
        .join("contracts/schemas/review.schema.json")
        .exists());
    let plan = env.root.join("plan.json");
    std::fs::write(&plan, r#"{"understanding":"u","files_likely_involved":["src/a"],"steps":["s1"],"validation":["v"],"plan_markdown":"md"}"#).unwrap();
    let (code, out, _) = toryo(
        &env,
        &["spike", "render", "--kind", "plan", plan.to_str().unwrap()],
    );
    assert_eq!(code, 0);
    assert!(out.contains("1. s1"));
}

#[test]
fn doctor_reports_config_and_profiles() {
    let env = setup(false);
    let (code, out, err) = toryo(&env, &["doctor"]);
    assert_eq!(code, 1, "no codex canary yet → FAIL; {err}");
    assert!(out.contains("profile claude-max"), "{out}");
    assert!(out.contains("windows.sandbox = unelevated"), "{out}");
    assert!(out.contains("is not a Git repository"), "{out}");
}

#[test]
fn smoke_orchestrates_both_fakes_end_to_end() {
    let env = setup(false);
    let (_code, _o, err) = toryo(
        &env,
        &[
            "spike",
            "smoke",
            "--claude-profile",
            "claude-max",
            "--codex-profile",
            "codex-foundry",
        ],
    );
    let r = records(&env);
    let last = r.last().unwrap();
    assert_eq!(last["spike"], "smoke", "{err}");
    let steps = last["details"]["steps"].as_array().unwrap();
    let ok = |name: &str| {
        steps
            .iter()
            .find(|s| s["step"] == name)
            .map(|s| s["ok"] == true)
            .unwrap_or(false)
    };
    assert!(ok("plan (Claude)"), "{err}");
    assert!(ok("implement (Codex)"), "{err}");
    assert!(ok("agent did not commit (HEAD guard)"));
    assert!(ok("no protected path touched"));
    assert!(ok("checkpoint + canonical DIFF.patch"));
    assert!(ok("review returns exactly AC-1..AC-3 (R28)"), "{err}");
    assert_eq!(last["details"]["extra"]["claude_mode"], "Restricted");
    // verify needs pwsh; on hosts without it that step fails, which is expected here
}

/// Pipeline runs here accept unverified profiles: the fakes have no canary records unless a
/// test runs them. The R51 gate itself is covered by `version_bound_evidence`.
fn toryo_stdin(env: &Env, args: &[&str], input: &str) -> (i32, String) {
    let mut a = args.to_vec();
    if a.starts_with(&["spike", "pipeline"]) {
        a.push("--accept-unverified");
    }
    toryo_stdin_strict(env, &a, input)
}

fn toryo_stdin_strict(env: &Env, args: &[&str], input: &str) -> (i32, String) {
    use std::io::Write;
    let mut c = Command::new(env!("CARGO_BIN_EXE_toryo"))
        .arg("--config")
        .arg(&env.config)
        .arg("--machine-config")
        .arg(env.root.join("none.yaml"))
        .args(args)
        .env("TORYO_DATA_DIR", &env.data)
        .env("TORYO_E2E_SECRET", "s3cr3t-value-e2e")
        .current_dir(&env.root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let o = c.wait_with_output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stderr).into(),
    )
}

#[test]
fn pipeline_plan_gate_fix_cycle_and_approval() {
    let env = setup(false);
    // the fake reviewer asks for changes once (state file), then approves
    let cfg = std::fs::read_to_string(&env.config).unwrap().replace(
        "FAKE_DENY: \"\"",
        &format!(
            "FAKE_DENY: \"\"\n      FAKE_STATE: \"{}\"",
            env.root.join("fake-state").display()
        ),
    );
    std::fs::write(&env.config, cfg).unwrap();
    let repo = env.root.join("calc");
    let (code, _, err) = toryo(
        &env,
        &["spike", "init-fixture", "--into", repo.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{err}");
    let task = repo.join("TASK.md");
    // reject the first plan with a reason, approve the second
    let (code, err) = toryo_stdin(
        &env,
        &[
            "spike",
            "pipeline",
            "--repo",
            repo.to_str().unwrap(),
            "--task",
            task.to_str().unwrap(),
            "--id",
            "CALC-001",
        ],
        "n\nplease also cover negative numbers\ny\n",
    );
    let r = records(&env);
    if std::env::var("SHOW").is_ok() {
        eprintln!("{err}");
    }
    let last = r.last().unwrap_or_else(|| panic!("{err}"));
    assert_eq!(last["spike"], "pipeline", "{err}");
    let steps = last["details"]["steps"].as_array().unwrap();
    let has = |f: &dyn Fn(&serde_json::Value) -> bool| steps.iter().any(f);
    assert!(
        has(&|s| s["step"] == "plan-gate"
            && s["decision"] == "rejected"
            && s["reason"] == "please also cover negative numbers"),
        "{err}"
    );
    assert!(
        has(&|s| s["step"] == "plan-gate" && s["decision"] == "approved"),
        "{err}"
    );
    assert!(
        err.contains("revised to address this") || std::fs::read_dir(env.data.join("runs")).is_ok()
    );
    if which_pwsh() {
        assert!(
            has(&|s| s["step"] == "review-0" && s["decision"] == "changes_requested"),
            "{err}"
        );
        assert!(
            has(&|s| s["step"] == "fix-1" && s["verify"] == true),
            "{err}"
        );
        assert!(
            has(&|s| s["step"] == "review-1" && s["decision"] == "approved"),
            "{err}"
        );
        assert_eq!(code, 0, "{err}");
        assert_eq!(last["pass"], true);
        // checkpoints are on the task branch, with trailers; main is untouched
        let log = Command::new("git")
            .args([
                "-C",
                repo.to_str().unwrap(),
                "log",
                "--format=%s|%(trailers:key=Toryo-Stage,valueonly)",
                "main..toryo/CALC-001",
            ])
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&log.stdout);
        assert!(
            log.contains("toryo(CALC-001): implement|implement"),
            "{log}"
        );
        assert!(log.contains("toryo(CALC-001): fix-1|fix-1"), "{log}");
        let run = std::path::PathBuf::from(last["run_dir"].as_str().unwrap());
        for f in [
            "TASK.md",
            "PLAN.md",
            "DIFF.patch",
            "DIFF-incremental.patch",
            "VERIFY_RESULT.json",
            "REVIEW.md",
            "SUMMARY.md",
        ] {
            assert!(run.join("runstore").join(f).exists(), "missing {f}");
        }
        // per-task TEMP inside the worktree, excluded from the checkpoints
        let wt = env.root.join("wts").join("CALC-001");
        assert!(wt.join(".toryo-tmp").join("fake-temp.txt").exists());
        let files = Command::new("git")
            .args([
                "-C",
                repo.to_str().unwrap(),
                "log",
                "--name-only",
                "--format=",
                "main..toryo/CALC-001",
            ])
            .output()
            .unwrap();
        assert!(!String::from_utf8_lossy(&files.stdout).contains(".toryo-tmp"));
        let meta = std::fs::read_to_string(run.join("implement.meta.json")).unwrap();
        assert!(
            meta.contains(".toryo-tmp"),
            "TEMP passed to the implementer: {meta}"
        );
        // non-blocking issues of the final review are listed for follow-up
        let summary = std::fs::read_to_string(run.join("runstore").join("SUMMARY.md")).unwrap();
        assert!(
            summary.contains("Open review issues") && summary.contains("R-9"),
            "{summary}"
        );
        // the implementer prompt of the fix cycle carries the review issue
        let fix_prompt = std::fs::read_to_string(run.join("fix-1.prompt.md")).unwrap();
        assert!(
            fix_prompt.contains("R-1") && fix_prompt.contains("AC-2 NOT MET"),
            "{fix_prompt}"
        );
    }
    // a second run with the same id refuses to clobber the worktree
    let (code, err) = toryo_stdin(
        &env,
        &[
            "spike",
            "pipeline",
            "--repo",
            repo.to_str().unwrap(),
            "--task",
            task.to_str().unwrap(),
            "--id",
            "CALC-001",
            "--yes",
        ],
        "",
    );
    assert_ne!(code, 0);
    assert!(err.contains("already exists"), "{err}");
}

#[test]
fn pipeline_quit_at_gate_and_batch_rename_fixture() {
    let env = setup(false);
    let repo = env.root.join("br");
    let (code, out, err) = toryo(
        &env,
        &[
            "spike",
            "init-fixture",
            "--app",
            "batch-rename",
            "--into",
            repo.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("batch-rename sample"));
    for f in [
        "src/BatchRename.psm1",
        "scripts/test.ps1",
        "tasks/BR-001.md",
        "tasks/BR-002.md",
        "AGENTS.md",
    ] {
        assert!(repo.join(f).exists(), "{f}");
    }
    let task = repo.join("tasks").join("BR-001.md");
    let (code, err) = toryo_stdin(
        &env,
        &[
            "spike",
            "pipeline",
            "--repo",
            repo.to_str().unwrap(),
            "--task",
            task.to_str().unwrap(),
        ],
        "q\n",
    );
    assert_ne!(code, 0);
    assert!(err.contains("BR-001"), "{err}");
    let r = records(&env);
    let last = r.last().unwrap_or_else(|| panic!("{err}"));
    assert_eq!(last["pass"], false);
    assert!(last["summary"]
        .as_str()
        .unwrap()
        .contains("cancelled at the plan gate"));
}

fn which_pwsh() -> bool {
    Command::new("pwsh").arg("-v").output().is_ok()
}

fn pipeline_env(quota: bool, review_once: bool) -> (Env, PathBuf) {
    let env = setup(false);
    let mut cfg = std::fs::read_to_string(&env.config).unwrap();
    if quota {
        cfg = cfg.replace("FAKE_CODEX_QUOTA: \"0\"", "FAKE_CODEX_QUOTA: \"1\"");
    }
    if review_once {
        cfg = cfg.replace(
            "FAKE_DENY: \"\"",
            &format!(
                "FAKE_DENY: \"\"\n      FAKE_STATE: \"{}\"",
                env.root.join("fake-state").display()
            ),
        );
    }
    std::fs::write(&env.config, cfg).unwrap();
    let repo = env.root.join("calc");
    let (code, _, err) = toryo(
        &env,
        &["spike", "init-fixture", "--into", repo.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{err}");
    (env, repo)
}

fn steps_of(env: &Env) -> (serde_json::Value, Vec<serde_json::Value>) {
    let r = records(env);
    let last = r.last().cloned().expect("a pipeline record");
    let steps = last["details"]["steps"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    (last, steps)
}

#[test]
fn pipeline_falls_back_to_claude_when_codex_is_out_of_quota() {
    let (env, repo) = pipeline_env(true, true);
    let r = repo.to_str().unwrap();
    let (code, err) = toryo_stdin(
        &env,
        &[
            "spike", "pipeline", "--repo", r, "--task", "TASK.md", "--id", "Q-1", "--yes",
        ],
        "",
    );
    let (last, steps) = steps_of(&env);
    assert!(err.contains("switching to claude-max"), "{err}");
    let sw: Vec<&serde_json::Value> = steps
        .iter()
        .filter(|s| s.get("implementer_switch").is_some())
        .collect();
    assert_eq!(
        sw.len(),
        1,
        "exactly one switch (sticky for the fix cycle): {steps:?}"
    );
    assert_eq!(sw[0]["implementer_switch"]["from"], "codex-foundry");
    assert_eq!(sw[0]["implementer_switch"]["to"], "claude-max");
    assert!(sw[0]["implementer_switch"]["reason"]
        .as_str()
        .unwrap()
        .contains("usage limit"));
    assert!(sw[0]["implementer_switch"]["partial_changes"]
        .to_string()
        .contains("src/Partial.ps1"));
    // the hand-over note told Claude about the partial work, and the task pack was inlined
    let p = std::fs::read_to_string(env.root.join("fake-state.claude-impl-prompt")).unwrap();
    assert_eq!(
        p.matches("## Hand-over").count(),
        1,
        "hand-over only on the first fallback turn"
    );
    assert!(
        p.contains("Hand-over") && p.contains("src/Partial.ps1"),
        "{p}"
    );
    assert!(p.contains("## TASK.md") && p.contains("NO shell"), "{p}");
    if which_pwsh() {
        assert_eq!(code, 0, "{err}");
        assert_eq!(last["pass"], true);
        let by: Vec<&str> = steps
            .iter()
            .filter_map(|s| s["implemented_by"].as_str())
            .collect();
        assert_eq!(
            by,
            ["claude-max", "claude-max"],
            "implement + fix-1 both by the fallback"
        );
        let log = Command::new("git")
            .args([
                "-C",
                r,
                "log",
                "--format=%(trailers:key=Toryo-Implementer,valueonly)",
                "main..toryo/Q-1",
            ])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&log.stdout).contains("claude-max"));
        let run = PathBuf::from(last["run_dir"].as_str().unwrap());
        let summary = std::fs::read_to_string(run.join("runstore").join("SUMMARY.md")).unwrap();
        assert!(
            summary.contains("Implementer fallback") && summary.contains("codex-foundry"),
            "{summary}"
        );
    }
}

#[test]
fn pipeline_no_fallback_stops_and_simulate_quota_switches() {
    let (env, repo) = pipeline_env(true, false);
    let r = repo.to_str().unwrap();
    let (code, err) = toryo_stdin(
        &env,
        &[
            "spike",
            "pipeline",
            "--repo",
            r,
            "--task",
            "TASK.md",
            "--id",
            "Q-2",
            "--yes",
            "--no-fallback",
        ],
        "",
    );
    assert_ne!(code, 0);
    let (last, _) = steps_of(&env);
    assert!(
        last["summary"]
            .as_str()
            .unwrap()
            .contains("all implementers are out of quota"),
        "{err}"
    );
    // partial work is kept in the worktree for inspection
    assert!(env
        .root
        .join("wts")
        .join("Q-2")
        .join("src")
        .join("Partial.ps1")
        .exists());

    let (env, repo) = pipeline_env(false, false);
    let r = repo.to_str().unwrap();
    let (_code, err) = toryo_stdin(
        &env,
        &[
            "spike",
            "pipeline",
            "--repo",
            r,
            "--task",
            "TASK.md",
            "--id",
            "Q-3",
            "--yes",
            "--simulate-quota",
        ],
        "",
    );
    let (_, steps) = steps_of(&env);
    let sw = steps
        .iter()
        .find(|s| s.get("implementer_switch").is_some())
        .expect("switch");
    assert!(
        sw["implementer_switch"]["reason"]
            .as_str()
            .unwrap()
            .contains("simulated"),
        "{err}"
    );
    assert!(
        steps.iter().any(|s| s["implemented_by"] == "claude-max"),
        "{err}"
    );
    let run = PathBuf::from(steps_of(&env).0["run_dir"].as_str().unwrap());
    assert!(
        run.join("implement.prompt.md").exists(),
        "a simulated skip does not shift the label"
    );
}

/// R51: isolation evidence is bound to the CLI version; a different version is unverified
/// and the pipeline refuses it unless --accept-unverified; `doctor --canaries` re-verifies.
#[test]
fn version_bound_evidence() {
    let (env, repo) = pipeline_env(false, false);
    let r = repo.to_str().unwrap();
    let run = |extra: &[&str]| {
        let mut a = vec!["spike", "pipeline", "--repo", r, "--task", "TASK.md", "--id"];
        a.extend_from_slice(extra);
        toryo_stdin_strict(&env, &a, "q\n")
    };
    // 1. no canary records: refused before anything is created
    let (code, err) = run(&["V-1"]);
    assert_ne!(code, 0);
    assert!(err.contains("unverified profiles (R51)"), "{err}");
    assert!(err.contains("no claude-isolation record") && err.contains("no codex-canary record"), "{err}");
    assert!(!env.root.join("wts").join("V-1").exists());

    // 2. doctor --canaries records versioned evidence for every profile + host
    let (_code, out, err) = toryo(&env, &["doctor", "--canaries"]);
    assert!(err.contains("canary: claude-isolation claude-max"), "{err}");
    assert!(err.contains("canary: codex-canary codex-foundry"), "{err}");
    let recs = records(&env);
    let find = |spike: &str| recs.iter().rev().find(|x| x["spike"] == spike).cloned().unwrap();
    assert_eq!(find("claude-isolation")["cli_versions"]["claude-max"], "2.1.300");
    assert_eq!(find("codex-canary")["cli_versions"]["codex-foundry"], "0.200.0");
    assert!(find("git-canary")["cli_versions"]["git"].is_string());
    assert!(out.contains("codex-canary passed") && out.contains("for 0.200.0 (R51)"), "{out}");

    // 3. verified: the pipeline starts (and stops at the gate)
    let (_, err) = run(&["V-2"]);
    assert!(err.contains("cancelled at the plan gate") || err.contains("STOPPED"), "{err}");
    assert!(!err.contains("unverified profiles"), "{err}");
    let last = recs_last(&env);
    assert_eq!(last["cli_versions"]["codex-foundry"], "0.200.0", "{last}");

    // 4. the Codex CLI is upgraded: its evidence is stale
    let cfg = std::fs::read_to_string(&env.config).unwrap().replace(
        "FAKE_ESCAPE: \"0\"",
        "FAKE_ESCAPE: \"0\"\n      FAKE_VERSION: \"0.201.0\"",
    );
    std::fs::write(&env.config, cfg).unwrap();
    let (code, err) = run(&["V-3"]);
    assert_ne!(code, 0);
    assert!(err.contains("codex-foundry: codex-canary passed") && err.contains("for version 0.200.0 but the CLI is now 0.201.0"), "{err}");
    assert!(!err.contains("claude-max:"), "Claude is still verified: {err}");
    let (dcode, dout, _) = toryo(&env, &["doctor"]);
    assert_eq!(dcode, 1);
    assert!(dout.contains("R51: unverified"), "{dout}");

    // 5. --accept-unverified runs anyway, with a warning
    let (_, err) = run(&["V-4", "--accept-unverified"]);
    assert!(err.contains("running with unverified profiles"), "{err}");
    assert!(err.contains("cancelled at the plan gate") || err.contains("STOPPED"), "{err}");
}

fn recs_last(env: &Env) -> serde_json::Value {
    records(env).last().cloned().unwrap()
}
