//! Shared helpers for the engine acceptance tests: a calculator fixture repo, a config,
//! `toryo` as a child process with fake agents, and store accessors.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use toryo::engine::store::{self, Store};

pub const DIVIDE: &str = "\nfunction Divide-Numbers { param($A,$B) if ($B -eq 0) { throw 'Division by zero' }; $A / $B }\n";

pub struct Env {
    pub _tmp: tempfile::TempDir,
    pub root: PathBuf,
    /// The repository `toryo.yaml` (no profiles, R60).
    pub config: PathBuf,
    /// The machine config passed with `--machine-config`: the profiles (R60).
    pub machine: PathBuf,
    pub data: PathBuf,
    pub repo: PathBuf,
}

/// The fake profiles of the fixture, in the machine config (R60).
pub const MACHINE_PROFILES: &str = "profiles:\n  claude-max: { kind: claude, billing: subscription, exe: claude-not-installed }\n  codex-openai: { kind: codex, billing: subscription, exe: codex-not-installed }\n";

pub fn setup(implementer: &str) -> Env {
    setup_with(implementer, "")
}

/// `setup` with extra top-level YAML appended to toryo.yaml (e.g. a `policy:` block).
pub fn setup_with(implementer: &str, extra: &str) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let repo = root.join("calc");
    let data = root.join("data");
    let config = root.join("toryo.yaml");
    let machine = root.join("machine.yaml");
    std::fs::write(&machine, MACHINE_PROFILES).unwrap();
    let fwd = |p: &Path| p.display().to_string().replace('\\', "/");
    std::fs::write(
        &config,
        format!(
            r#"version: 1
project:
  root: "{repo}"
  worktrees_root: "{wts}"
roles:
  planner: {{ profile: claude-max }}
  implementer: {{ profile: {implementer} }}
commands:
  verify:
    - {{ name: divide-present, program: git, args: ["grep", "-q", "Divide-Numbers", "--", "src"] }}
{verify}{extra}"#,
            repo = fwd(&repo),
            verify = if extra.contains("verify:") {
                String::new()
            } else {
                format!(
                    "verify: {{ boundary: none, verify_root: \"{}\" }}\n",
                    fwd(&root.join("verify"))
                )
            },
            wts = fwd(&root.join("wts")),
        ),
    )
    .unwrap();
    let env = Env {
        _tmp: tmp,
        root,
        config,
        machine,
        data,
        repo,
    };
    let (code, err) = toryo(
        &env,
        &[
            "spike",
            "init-fixture",
            "--into",
            env.repo.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    env
}

pub fn command(env: &Env, args: &[&str], extra: &[(&str, &str)]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_toryo"));
    c.arg("--config")
        .arg(&env.config)
        .arg("--machine-config")
        .arg(&env.machine)
        .args(args)
        .env("TORYO_DATA_DIR", &env.data)
        .env("TORYO_RETRY_BASE_SECS", "0")
        .env("TORYO_GLOBAL_CONTEXT", env.root.join("global.md"))
        .current_dir(&env.root)
        .stdin(Stdio::null());
    for (k, v) in extra {
        c.env(k, v);
    }
    c
}

/// `toryo status <id>`: must succeed; returns stdout (where `status` prints).
pub fn status_out(env: &Env, id: &str) -> String {
    let o = command(env, &["status", id], &[]).output().unwrap();
    assert!(
        o.status.success(),
        "toryo status {id}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).to_string()
}

pub fn toryo(env: &Env, args: &[&str], extra: &[(&str, &str)]) -> (i32, String) {
    let o = command(env, args, extra).output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    if std::env::var("SHOW").is_ok() {
        eprintln!("$ toryo {}\n{err}", args.join(" "));
    }
    (o.status.code().unwrap_or(-1), err)
}

pub fn spawn(env: &Env, args: &[&str], log: &str) -> Child {
    let f = std::fs::File::create(env.root.join(log)).unwrap();
    command(env, args, &[])
        .stdout(Stdio::null())
        .stderr(f)
        .spawn()
        .unwrap()
}

pub fn db(env: &Env) -> Store {
    Store::open(&env.data.join("toryo.db")).unwrap()
}

pub fn task(env: &Env, id: &str) -> store::TaskRow {
    let i = id.to_string();
    db(env).call(move |tx| store::get_task(tx, &i)).unwrap()
}

pub fn attempts(env: &Env, id: &str) -> Vec<store::AttemptRow> {
    let i = id.to_string();
    db(env).call(move |tx| store::attempts(tx, &i)).unwrap()
}

pub fn wait_until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(secs) {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for {what}");
}

/// Runs git in `dir` and returns its stdout. A failing git command fails the test, so a
/// helper such as `clean()` can never pass because git itself failed.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).to_string()
}

pub fn clean(dir: &Path) -> bool {
    git(dir, &["status", "--porcelain", "--untracked-files=all"])
        .trim()
        .is_empty()
}

pub fn script(env: &Env, name: &str, turns: Value) -> String {
    let p = env.root.join(name);
    std::fs::write(
        &p,
        serde_json::to_vec_pretty(&json!({ "turns": turns })).unwrap(),
    )
    .unwrap();
    p.display().to_string()
}

pub fn turn(steps: Vec<Value>) -> Value {
    json!({ "steps": steps })
}
pub fn plan() -> Value {
    json!({"kind":"complete","structured":{"understanding":"u","files_likely_involved":["src/Calculator.psm1"],"steps":["add"],"validation":["tests"],"plan_markdown":"md"}})
}
pub fn implementation() -> Value {
    json!({"kind":"complete","structured":{"summary":"s","deviations":[],"validation_performed":["read"],"open_issues":[],"resolved":[]}})
}
pub fn write(path: &str, content: &str) -> Value {
    json!({"kind":"write_file","path":path,"content":content,"append":true})
}
pub fn hang() -> Value {
    json!({"kind":"hang"})
}
pub fn approved() -> Value {
    json!({"kind":"complete","structured":{"decision":"approved","summary":"s","acceptance_criteria":[
        {"id":"AC-1","met":true,"evidence":"e1"},{"id":"AC-2","met":true,"evidence":"e2"},{"id":"AC-3","met":true,"evidence":"e3"}],"issues":[]}})
}
pub fn changes() -> Value {
    json!({"kind":"complete","structured":{"decision":"changes_requested","summary":"s","acceptance_criteria":[
        {"id":"AC-1","met":true,"evidence":"e1"},{"id":"AC-2","met":false,"evidence":"no zero test"},{"id":"AC-3","met":true,"evidence":"e3"}],
        "issues":[{"id":"I-1","severity":"high","blocking":true,"description":"add the zero test"}]}})
}

pub fn create(env: &Env, id: &str) {
    let (code, err) = toryo(
        env,
        &["task", "create", "--file", "TASK.md", "--id", id],
        &[],
    );
    assert_eq!(code, 0, "{err}");
}

pub fn wt(env: &Env, id: &str) -> PathBuf {
    env.root.join("wts").join(id)
}

pub fn trailers(env: &Env, id: &str) -> String {
    git(
        &wt(env, id),
        &["log", "--format=%s|%(trailers:key=Toryo-Stage,valueonly,separator=)|%(trailers:key=Toryo-Attempt,valueonly,separator=)|%(trailers:key=Toryo-Partial,valueonly,separator=)|%(trailers:key=Toryo-No-Change,valueonly,separator=)", "main..HEAD"],
    )
}

/// The fake agent calls recorded by the latest run of a task.
pub fn calls(env: &Env, id: &str) -> Vec<Value> {
    let t = task(env, id);
    let v: Value = serde_json::from_slice(
        &std::fs::read(
            t.captures()
                .join(format!("fake-calls-{}.json", t.lock_generation)),
        )
        .unwrap(),
    )
    .unwrap();
    v.as_array().cloned().unwrap_or_default()
}

pub fn fail(class: &str, detail: &str) -> Value {
    json!({"kind":"fail","class":class,"detail":detail})
}

/// Commit files into the fixture repo (on main), before a task is created.
pub fn commit_repo(env: &Env, files: &[(&str, &str)], msg: &str) {
    for (p, c) in files {
        let f = env.repo.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, c).unwrap();
    }
    let run = |a: &[&str]| {
        assert!(Command::new("git")
            .arg("-C")
            .arg(&env.repo)
            .args(a)
            .status()
            .unwrap()
            .success())
    };
    run(&["add", "-A"]);
    run(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "-m",
        msg,
    ]);
}
