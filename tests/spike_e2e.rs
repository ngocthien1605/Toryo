//! End-to-end run of the spike commands against FAKE `claude` / `codex` executables
//! (shell scripts speaking the documented stream formats). This proves the harness —
//! argv, env, Job, redaction, capture, parsing, checks, evidence records — before the
//! first real run on Windows. Unix-only (the fakes are sh scripts).
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const FAKE_CLAUDE: &str = r#"#!/bin/sh
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
  *"Create the plan"*) echo '{"type":"result","subtype":"success","is_error":false,"session_id":"fake-1","result":"p","structured_output":{"understanding":"u","files_likely_involved":["src/Calculator.psm1"],"steps":["add Divide-Numbers"],"validation":["tests"],"plan_markdown":"md"},"usage":{"input_tokens":5,"output_tokens":20},"permission_denials":[]}'; exit 0;;
  *"Review the checkpoint"*) echo '{"type":"result","subtype":"success","is_error":false,"session_id":"fake-1","result":"r","structured_output":{"decision":"approved","summary":"s","acceptance_criteria":[{"id":"AC-1","met":true,"evidence":"e1"},{"id":"AC-2","met":true,"evidence":"e2"},{"id":"AC-3","met":true,"evidence":"e3"}],"issues":[]},"usage":{"input_tokens":5,"output_tokens":20},"permission_denials":[]}'; exit 0;;
esac
printf '{"type":"result","subtype":"success","is_error":false,"session_id":"fake-1","result":"done","structured_output":{"run_store_token":"%s","outside_file_content":"","special_instructions":"%s","ok":true,"note":"x"},"usage":{"input_tokens":5,"cache_read_input_tokens":100,"cache_creation_input_tokens":0,"output_tokens":20},"permission_denials":%s}\n' "$token" "$code" "$denials"
"#;

const FAKE_CODEX: &str = r#"#!/bin/sh
prompt=$(cat)
echo '{"type":"thread.started","thread_id":"th-fake"}'
echo '{"type":"turn.started"}'
if printf '%s' "$prompt" | grep -q "Role: Implementer"; then
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
    let config = root.join("thien.yaml");
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
      FAKE_SECRET: env:THIEN_E2E_SECRET
  codex-foundry:
    kind: codex
    billing: foundry
    exe: {codex}
    env:
      FAKE_ESCAPE: "{esc}"
      FAKE_LAZY: "{lazy}"
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

fn thien(env: &Env, args: &[&str]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_thien"))
        .arg("--config")
        .arg(&env.config)
        .arg("--machine-config")
        .arg(env.root.join("none.yaml"))
        .args(args)
        .env("THIEN_DATA_DIR", &env.data)
        .env("THIEN_E2E_SECRET", "s3cr3t-value-e2e")
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
    let (code, _out, err) = thien(
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
    let (code, _o, err) = thien(
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
    let (code, _o, err) = thien(
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
    let (code, _o, err) = thien(
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
    let (code, _o, err) = thien(
        &good,
        &["spike", "codex-canary", "--profile", "codex-foundry"],
    );
    assert_eq!(code, 0, "{err}");
    let bad = setup(true);
    let (code, _o, err) = thien(
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
    let (code, _o, err) = thien(
        &lazy,
        &["spike", "codex-canary", "--profile", "codex-foundry"],
    );
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("[FAIL] attempted: append to the run store TASK.md"),
        "{err}"
    );
    // doctor must now report the Codex profile as unusable
    let (dcode, dout, _) = thien(&bad, &["doctor"]);
    assert_eq!(dcode, 1);
    assert!(dout.contains("codex-canary"), "{dout}");
}

#[test]
fn killed_resume_records_provisional_and_unknown_usage() {
    for profile in ["claude-max", "codex-foundry"] {
        let env = setup(false);
        let (code, _o, err) = thien(
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
    let (_code, _o, err) = thien(&env, &["spike", "job-test"]);
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
    let (code, _o, err) = thien(&env, &["spike", "git-canary"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, _) = thien(&env, &["spike", "report"]);
    assert_eq!(code, 0);
    assert!(out.contains("git-canary") && out.contains("PASS"));
    let fx = env.root.join("calc");
    let (code, _o, err) = thien(
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
    let (code, out, _) = thien(
        &env,
        &["spike", "render", "--kind", "plan", plan.to_str().unwrap()],
    );
    assert_eq!(code, 0);
    assert!(out.contains("1. s1"));
}

#[test]
fn doctor_reports_config_and_profiles() {
    let env = setup(false);
    let (code, out, err) = thien(&env, &["doctor"]);
    assert_eq!(code, 1, "no codex canary yet → FAIL; {err}");
    assert!(out.contains("profile claude-max"), "{out}");
    assert!(out.contains("windows.sandbox = unelevated"), "{out}");
    assert!(out.contains("is not a Git repository"), "{out}");
}

#[test]
fn smoke_orchestrates_both_fakes_end_to_end() {
    let env = setup(false);
    let (_code, _o, err) = thien(&env, &["spike", "smoke", "--claude-profile", "claude-max", "--codex-profile", "codex-foundry"]);
    let r = records(&env);
    let last = r.last().unwrap();
    assert_eq!(last["spike"], "smoke", "{err}");
    let steps = last["details"]["steps"].as_array().unwrap();
    let ok = |name: &str| steps.iter().find(|s| s["step"] == name).map(|s| s["ok"] == true).unwrap_or(false);
    assert!(ok("plan (Claude)"), "{err}");
    assert!(ok("implement (Codex)"), "{err}");
    assert!(ok("agent did not commit (HEAD guard)"));
    assert!(ok("no protected path touched"));
    assert!(ok("checkpoint + canonical DIFF.patch"));
    assert!(ok("review returns exactly AC-1..AC-3 (R28)"), "{err}");
    assert_eq!(last["details"]["extra"]["claude_mode"], "Restricted");
    // verify needs pwsh; on hosts without it that step fails, which is expected here
}
