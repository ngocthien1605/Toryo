//! M3 acceptance (plan §17): the engine with SQLite state, the task lock and crash recovery,
//! on `FakeAgentAdapter` (no agent processes). Crashes are real process deaths: a killed
//! `toryo` (the fake agent hangs), or `TORYO_TEST_CRASH_AT=<point>` (exits abruptly).
//!
//! - kill during any stage → `toryo resume` reaches the right state with a clean tree (R31)
//! - trailer-matched normal, partial and no-change commits (R40)
//! - policy-violating partial → Escalated → `--discard-partial` / `--keep-partial`
//! - `lock_first_recovery` (R38) and `partial_snapshot_roundtrip` (R39)
//! - `recovery.resume`: Claude resumes its session, Codex starts fresh with a hand-over (R48)
#![cfg(feature = "m0-kit")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use toryo::engine::store::{self, AttemptStatus, Store};
use toryo::engine::PipelineState as St;

const DIVIDE: &str = "\nfunction Divide-Numbers { param($A,$B) if ($B -eq 0) { throw 'Division by zero' }; $A / $B }\n";

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
    data: PathBuf,
    repo: PathBuf,
}

fn setup(implementer: &str) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let repo = root.join("calc");
    let data = root.join("data");
    let config = root.join("toryo.yaml");
    let fwd = |p: &Path| p.display().to_string().replace('\\', "/");
    std::fs::write(
        &config,
        format!(
            r#"version: 1
project:
  root: "{repo}"
  worktrees_root: "{wts}"
profiles:
  claude-max: {{ kind: claude, billing: subscription, exe: claude-not-installed }}
  codex-openai: {{ kind: codex, billing: subscription, exe: codex-not-installed }}
roles:
  planner: {{ profile: claude-max }}
  implementer: {{ profile: {implementer} }}
commands:
  verify:
    - {{ name: divide-present, program: git, args: ["grep", "-q", "Divide-Numbers", "--", "src"] }}
"#,
            repo = fwd(&repo),
            wts = fwd(&root.join("wts")),
        ),
    )
    .unwrap();
    let env = Env { _tmp: tmp, root, config, data, repo };
    let (code, err) = toryo(&env, &["spike", "init-fixture", "--into", env.repo.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{err}");
    env
}

fn command(env: &Env, args: &[&str], extra: &[(&str, &str)]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_toryo"));
    c.arg("--config")
        .arg(&env.config)
        .arg("--machine-config")
        .arg(env.root.join("none.yaml"))
        .args(args)
        .env("TORYO_DATA_DIR", &env.data)
        .env("TORYO_RETRY_BASE_SECS", "0")
        .current_dir(&env.root)
        .stdin(Stdio::null());
    for (k, v) in extra {
        c.env(k, v);
    }
    c
}

fn toryo(env: &Env, args: &[&str], extra: &[(&str, &str)]) -> (i32, String) {
    let o = command(env, args, extra).output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    if std::env::var("SHOW").is_ok() {
        eprintln!("$ toryo {}\n{err}", args.join(" "));
    }
    (o.status.code().unwrap_or(-1), err)
}

fn spawn(env: &Env, args: &[&str], log: &str) -> Child {
    let f = std::fs::File::create(env.root.join(log)).unwrap();
    command(env, args, &[]).stdout(Stdio::null()).stderr(f).spawn().unwrap()
}

fn db(env: &Env) -> Store {
    Store::open(&env.data.join("toryo.db")).unwrap()
}

fn task(env: &Env, id: &str) -> store::TaskRow {
    let i = id.to_string();
    db(env).call(move |tx| store::get_task(tx, &i)).unwrap()
}

fn attempts(env: &Env, id: &str) -> Vec<store::AttemptRow> {
    let i = id.to_string();
    db(env).call(move |tx| store::attempts(tx, &i)).unwrap()
}

fn wait_until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(secs) {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for {what}");
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn clean(dir: &Path) -> bool {
    git(dir, &["status", "--porcelain", "--untracked-files=all"]).trim().is_empty()
}

fn script(env: &Env, name: &str, turns: Value) -> String {
    let p = env.root.join(name);
    std::fs::write(&p, serde_json::to_vec_pretty(&json!({ "turns": turns })).unwrap()).unwrap();
    p.display().to_string()
}

fn turn(steps: Vec<Value>) -> Value {
    json!({ "steps": steps })
}
fn plan() -> Value {
    json!({"kind":"complete","structured":{"understanding":"u","files_likely_involved":["src/Calculator.psm1"],"steps":["add"],"validation":["tests"],"plan_markdown":"md"}})
}
fn implementation() -> Value {
    json!({"kind":"complete","structured":{"summary":"s","deviations":[],"validation_performed":["read"],"open_issues":[],"resolved":[]}})
}
fn write(path: &str, content: &str) -> Value {
    json!({"kind":"write_file","path":path,"content":content,"append":true})
}
fn hang() -> Value {
    json!({"kind":"hang"})
}
fn approved() -> Value {
    json!({"kind":"complete","structured":{"decision":"approved","summary":"s","acceptance_criteria":[
        {"id":"AC-1","met":true,"evidence":"e1"},{"id":"AC-2","met":true,"evidence":"e2"},{"id":"AC-3","met":true,"evidence":"e3"}],"issues":[]}})
}
fn changes() -> Value {
    json!({"kind":"complete","structured":{"decision":"changes_requested","summary":"s","acceptance_criteria":[
        {"id":"AC-1","met":true,"evidence":"e1"},{"id":"AC-2","met":false,"evidence":"no zero test"},{"id":"AC-3","met":true,"evidence":"e3"}],
        "issues":[{"id":"R-1","severity":"major","blocking":true,"description":"add the zero test"}]}})
}

fn create(env: &Env, id: &str) {
    let (code, err) = toryo(env, &["task", "create", "--file", "TASK.md", "--id", id], &[]);
    assert_eq!(code, 0, "{err}");
}

fn wt(env: &Env, id: &str) -> PathBuf {
    env.root.join("wts").join(id)
}

fn trailers(env: &Env, id: &str) -> String {
    git(
        &wt(env, id),
        &["log", "--format=%s|%(trailers:key=Toryo-Stage,valueonly,separator=)|%(trailers:key=Toryo-Attempt,valueonly,separator=)|%(trailers:key=Toryo-Partial,valueonly,separator=)|%(trailers:key=Toryo-No-Change,valueonly,separator=)", "main..HEAD"],
    )
}

// ── the engine with its gates ────────────────────────────────────────────────────────

#[test]
fn gates_persist_and_the_task_completes() {
    let env = setup("codex-openai");
    create(&env, "E-1");
    assert_eq!(task(&env, "E-1").state, St::New);
    let s = script(&env, "a.json", json!({
        "planner": [turn(vec![plan()]), turn(vec![plan()])],
        "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]), turn(vec![write("tests/Divide.Tests.ps1", "# t\n"), implementation()])],
        "reviewer": [turn(vec![changes()]), turn(vec![approved()])],
    }));
    // run stops at the plan gate (exit 0), the gate is a persisted occurrence
    let (code, err) = toryo(&env, &["run", "E-1", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "E-1").state, St::WaitingPlanApproval);
    let (_, out) = toryo(&env, &["status", "E-1"], &[]);
    let _ = out;
    // --expect with a wrong digest is refused, nothing changes
    let (code, err) = toryo(&env, &["approve", "E-1", "--gate", "plan", "--expect", "ffffffff"], &[]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("subject_changed"), "{err}");
    let (code, _) = toryo(&env, &["reject", "E-1", "--gate", "plan", "--reason", "also negative numbers"], &[]);
    assert_eq!(code, 0);
    assert_eq!(task(&env, "E-1").state, St::Planning);
    // the revision (planner resumes its session, R48) reopens the gate: occurrence 2
    let s2 = script(&env, "b.json", json!({
        "planner": [turn(vec![plan()])],
        "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]), turn(vec![write("tests/Divide.Tests.ps1", "# t\n"), implementation()])],
        "reviewer": [turn(vec![changes()]), turn(vec![approved()])],
    }));
    let (code, err) = toryo(&env, &["run", "E-1", "--fake-agents", &s2], &[]);
    assert_eq!(code, 0, "{err}");
    let calls: Value = serde_json::from_slice(&std::fs::read(task(&env, "E-1").captures().join(format!("fake-calls-{}.json", task(&env, "E-1").lock_generation))).unwrap()).unwrap();
    assert!(calls[0]["resume"].is_string(), "the planner resumed its session: {calls}");
    assert!(calls[0]["prompt"].as_str().unwrap().contains("also negative numbers"));
    // approve --continue runs to the final gate (one fix cycle on the way)
    let (code, err) = toryo(&env, &["approve", "E-1", "--gate", "plan", "--continue", "--fake-agents", &s2], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "E-1");
    assert_eq!(t.state, St::WaitingFinalApproval, "{err}");
    assert_eq!(t.fix_cycle, 1);
    let (code, err) = toryo(&env, &["approve", "E-1", "--gate", "final"], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "E-1").state, St::Completed);
    let ap = db(&env).call(|tx| store::approvals(tx, "E-1")).unwrap();
    let st: Vec<(String, u32, String)> = ap.into_iter().map(|a| (a.gate, a.occurrence, a.status)).collect();
    assert_eq!(st, [("plan".into(), 1, "rejected".into()), ("plan".into(), 2, "approved".into()), ("final".into(), 3, "approved".into())]);
    // checkpoints carry (task, stage, attempt) trailers
    let log = trailers(&env, "E-1");
    assert!(log.contains("toryo(E-1): implement|implement|1||"), "{log}");
    assert!(log.contains("toryo(E-1): fix-1|fix-1|1||"), "{log}");
    assert!(clean(&wt(&env, "E-1")));
    assert!(t.run_store.join("runstore").join("SUMMARY.md").exists());
}

// ── crashes at every stage ───────────────────────────────────────────────────────────

fn full_script(env: &Env, name: &str) -> String {
    script(env, name, json!({
        "planner": [turn(vec![plan()])],
        "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
        "reviewer": [turn(vec![approved()])],
    }))
}

#[test]
fn a_crash_in_any_stage_resumes_to_completion_with_a_clean_tree() {
    for point in ["plan:after_begin", "implement:after_begin", "implement:after_turn", "checkpoint:after_commit", "verify:after_begin", "review:after_begin"] {
        let env = setup("codex-openai");
        create(&env, "C-1");
        let s = full_script(&env, "s.json");
        let (code, err) = toryo(&env, &["run", "C-1", "--yes", "--fake-agents", &s], &[("TORYO_TEST_CRASH_AT", point)]);
        assert_eq!(code, 86, "{point}: {err}");
        assert!(attempts(&env, "C-1").iter().any(|a| a.status == AttemptStatus::Running) || point == "checkpoint:after_commit", "{point}: a running attempt is left behind");
        // after `implement:after_turn` the resumed Implementer finds the partial work and
        // changes nothing (R40 a); before it, it does the work
        let imp = if point.starts_with("plan") || point == "implement:after_begin" {
            turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])
        } else {
            turn(vec![implementation()])
        };
        let s2 = script(&env, "s2.json", json!({
            "planner": [turn(vec![plan()])],
            "implementer": [imp],
            "reviewer": [turn(vec![approved()])],
        }));
        let (code, err) = toryo(&env, &["resume", "C-1", "--yes", "--fake-agents", &s2], &[]);
        assert_eq!(code, 0, "{point}: {err}");
        let t = task(&env, "C-1");
        assert_eq!(t.state, St::Completed, "{point}: {err}");
        assert!(clean(&t.worktree), "{point}: clean tree");
        let atts = attempts(&env, "C-1");
        assert!(atts.iter().all(|a| a.status != AttemptStatus::Running), "{point}: {atts:?}");
        let log = trailers(&env, "C-1");
        match point {
            "implement:after_turn" => {
                // the agent's edit became a recovery checkpoint, then a no-change checkpoint on top
                assert!(log.contains("implement|1|true|"), "{point}: {log}");
                assert!(log.contains("implement|2||true"), "{point}: {log}");
                assert!(err.contains("recovery checkpoint"), "{err}");
            }
            "checkpoint:after_commit" => {
                // found by its trailers: no second commit
                assert_eq!(log.lines().count(), 1, "{point}: {log}");
                assert!(err.contains("already committed"), "{err}");
            }
            _ => {
                assert!(!log.contains("|true|"), "{point}: no partial commit: {log}");
            }
        }
    }
}

#[test]
fn no_change_implementation_still_checkpoints_and_verifies() {
    let env = setup("codex-openai");
    // the task is already satisfied: Divide-Numbers exists on main
    std::fs::OpenOptions::new().append(true).open(env.repo.join("src/Calculator.psm1")).and_then(|mut f| std::io::Write::write_all(&mut f, DIVIDE.as_bytes())).unwrap();
    git(&env.repo, &["commit", "-qam", "already there"]);
    create(&env, "N-1");
    let s = script(&env, "s.json", json!({
        "planner": [turn(vec![plan()])],
        "implementer": [turn(vec![implementation()])],
        "reviewer": [turn(vec![approved()])],
    }));
    // crash right after the empty checkpoint commit (R40 c)
    let (code, _) = toryo(&env, &["run", "N-1", "--yes", "--fake-agents", &s], &[("TORYO_TEST_CRASH_AT", "checkpoint:after_commit")]);
    assert_eq!(code, 86);
    let s2 = script(&env, "s2.json", json!({ "reviewer": [turn(vec![approved()])] }));
    let (code, err) = toryo(&env, &["resume", "N-1", "--yes", "--fake-agents", &s2], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "N-1").state, St::Completed);
    let log = trailers(&env, "N-1");
    assert_eq!(log.trim(), "toryo(N-1): implement|implement|1||true", "{log}");
    let a = attempts(&env, "N-1").into_iter().find(|a| a.stage == "implement").unwrap();
    assert_eq!((a.status, a.checkpoint_kind.as_deref()), (AttemptStatus::Succeeded, Some("no_change")));
    assert!(attempts(&env, "N-1").iter().any(|a| a.stage == "verify" && a.status == AttemptStatus::Succeeded));
}

// ── kills + R48 resume policy ────────────────────────────────────────────────────────

fn kill_during_implement(env: &Env, id: &str, steps: Vec<Value>) {
    let s = script(env, "hang.json", json!({
        "planner": [turn(vec![plan()])],
        "implementer": [turn(steps)],
    }));
    let mut child = spawn(env, &["run", id, "--yes", "--fake-agents", &s], "hang.log");
    let (e2, i2) = (env.data.clone(), id.to_string());
    wait_until("the implementer to hang", 60, || {
        let st = Store::open(&e2.join("toryo.db")).unwrap();
        let i = i2.clone();
        st.call(move |tx| store::last_attempt(tx, &i, "implement")).ok().flatten().is_some_and(|a| a.status == AttemptStatus::Running && a.external_session_id.is_some())
    });
    std::thread::sleep(Duration::from_millis(300));
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn killed_codex_implementer_restarts_fresh_with_a_handover() {
    let env = setup("codex-openai");
    create(&env, "K-1");
    kill_during_implement(&env, "K-1", vec![write("src/Calculator.psm1", DIVIDE), hang()]);
    let s2 = script(&env, "s2.json", json!({
        "implementer": [turn(vec![write("tests/Divide.Tests.ps1", "# t\n"), implementation()])],
        "reviewer": [turn(vec![approved()])],
    }));
    let (code, err) = toryo(&env, &["resume", "K-1", "--yes", "--fake-agents", &s2], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "K-1");
    assert_eq!(t.state, St::Completed);
    let a1 = attempts(&env, "K-1").into_iter().find(|a| a.id == "K-1/implement/1").unwrap();
    assert_eq!(a1.status, AttemptStatus::Interrupted);
    let partial = a1.partial_commit.clone().expect("recovery checkpoint");
    let calls: Value = serde_json::from_slice(&std::fs::read(t.captures().join(format!("fake-calls-{}.json", t.lock_generation))).unwrap()).unwrap();
    let imp = calls.as_array().unwrap().iter().find(|c| c["key"] == "implementer").unwrap();
    assert!(imp["resume"].is_null(), "Codex starts a fresh session (R48)");
    assert!(imp["prompt"].as_str().unwrap().contains(&partial[..12]), "the hand-over names the partial commit");
    // R41: the killed turn is an `unknown` usage row with a reservation, never zero
    assert!(db(&env).call(|tx| store::has_usage(tx, "K-1/implement/1")).unwrap(), "the killed turn has a usage row");
    // plan, the killed implement turn, implement 2 and the review: fakes report no usage
    assert_eq!(t.reserved_tokens, 4 * store::DEFAULT_UNKNOWN_RESERVATION);
    assert!(clean(&t.worktree));
}

#[test]
fn killed_claude_implementer_resumes_its_session() {
    let env = setup("claude-max");
    create(&env, "K-2");
    kill_during_implement(&env, "K-2", vec![write("src/Calculator.psm1", DIVIDE), hang()]);
    let sid = attempts(&env, "K-2").into_iter().find(|a| a.id == "K-2/implement/1").unwrap().external_session_id.unwrap();
    let s2 = script(&env, "s2.json", json!({
        "implementer": [turn(vec![implementation()])],
        "reviewer": [turn(vec![approved()])],
    }));
    let (code, err) = toryo(&env, &["resume", "K-2", "--yes", "--fake-agents", &s2], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "K-2");
    let calls: Value = serde_json::from_slice(&std::fs::read(t.captures().join(format!("fake-calls-{}.json", t.lock_generation))).unwrap()).unwrap();
    let imp = calls.as_array().unwrap().iter().find(|c| c["key"] == "implementer").unwrap();
    assert_eq!(imp["resume"].as_str(), Some(sid.as_str()), "Claude resumes the interrupted session (R48)");
    assert!(imp["prompt"].as_str().unwrap().contains("## Recovery"));
    assert_eq!(t.state, St::Completed);
}

// ── policy-violating partials ────────────────────────────────────────────────────────

#[test]
fn policy_violating_partial_escalates_then_discard_or_keep() {
    for mode in ["discard", "keep"] {
        let env = setup("codex-openai");
        create(&env, "P-1");
        kill_during_implement(&env, "P-1", vec![write("TASK.md", "- AC-9: sneaky\n"), write("src/Calculator.psm1", DIVIDE), hang()]);
        let s2 = script(&env, "s2.json", json!({
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }));
        let (code, err) = toryo(&env, &["resume", "P-1", "--yes", "--fake-agents", &s2], &[]);
        assert_eq!(code, 1, "{err}");
        let t = task(&env, "P-1");
        assert_eq!(t.state, St::Escalated);
        assert!(t.machine.reason().unwrap().starts_with("recovery_policy_violation"), "{:?}", t.machine.reason());
        assert!(git(&t.worktree, &["log", "--format=%s", "main..HEAD"]).trim().is_empty(), "nothing committed");
        let w = t.worktree.clone();
        match mode {
            "discard" => {
                let (code, err) = toryo(&env, &["resume", "P-1", "--discard-partial", "--yes", "--fake-agents", &s2], &[]);
                assert_eq!(code, 0, "{err}");
                assert!(err.contains("discarded"), "{err}");
            }
            _ => {
                // refused while the protected change is still there
                let (code, err) = toryo(&env, &["resume", "P-1", "--keep-partial", "--yes", "--fake-agents", &s2], &[]);
                assert_eq!(code, 2, "{err}");
                assert!(err.contains("protected paths still changed"), "{err}");
                git(&w, &["checkout", "--", "TASK.md"]);
                let (code, err) = toryo(&env, &["resume", "P-1", "--keep-partial", "--yes", "--fake-agents", &s2], &[]);
                assert_eq!(code, 0, "{err}");
                assert!(trailers(&env, "P-1").contains("implement|1|true|"));
            }
        }
        let t = task(&env, "P-1");
        assert_eq!(t.state, St::Completed, "{mode}");
        assert!(clean(&w));
        assert!(!git(&w, &["log", "-p", "main..HEAD"]).contains("sneaky"), "{mode}: the protected change never reached a commit");
    }
}

#[test]
fn partial_snapshot_roundtrip() {
    let env = setup("codex-openai");
    create(&env, "S-1");
    kill_during_implement(&env, "S-1", vec![write("src/Calculator.psm1", DIVIDE), hang()]);
    let w = wt(&env, "S-1");
    // four kinds of state + a policy violation
    std::fs::write(w.join("README.md"), "staged edit\n").unwrap();
    git(&w, &["add", "README.md"]);
    std::fs::write(w.join("src").join("logo.bin"), [0u8, 159, 146, 150, 255, 0, 1]).unwrap();
    git(&w, &["add", "src/logo.bin"]);
    std::fs::create_dir_all(w.join("notes")).unwrap();
    std::fs::write(w.join("notes").join("new.txt"), "untracked\n").unwrap();
    std::fs::OpenOptions::new().append(true).open(w.join("TASK.md")).and_then(|mut f| std::io::Write::write_all(&mut f, b"- AC-9: x\n")).unwrap();
    let files = ["src/Calculator.psm1", "README.md", "src/logo.bin", "notes/new.txt", "TASK.md"];
    let hash = |root: &Path, f: &str| toryo::engine::task::sha256_hex(&std::fs::read(root.join(f)).unwrap());
    let originals: Vec<String> = files.iter().map(|f| hash(&w, f)).collect();
    let (code, err) = toryo(&env, &["resume", "S-1"], &[]);
    assert_eq!(code, 1, "{err}");
    assert_eq!(task(&env, "S-1").state, St::Escalated);
    // export re-creates every file byte for byte
    let into = env.root.join("exported");
    let (code, err) = toryo(&env, &["partial", "export", "S-1", "--attempt", "1", "--into", into.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{err}");
    for (f, h) in files.iter().zip(&originals) {
        assert_eq!(&hash(&into, f), h, "{f}");
    }
    // a corrupted snapshot blocks the discard: no reset
    let a = attempts(&env, "S-1").into_iter().find(|a| a.id == "S-1/implement/1").unwrap();
    let snaps = task(&env, "S-1").run_store.join("artifacts").join("partial").join("implement-1");
    let dir = std::fs::read_dir(&snaps).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).find(|p| p.join("manifest.json").exists()).unwrap();
    assert!(a.partial_snapshot_id.is_some());
    let victim = dir.join("files").join("notes").join("new.txt");
    std::fs::write(&victim, "tampered\n").unwrap();
    let (code, err) = toryo(&env, &["resume", "S-1", "--discard-partial"], &[]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("snapshot_incomplete"), "{err}");
    assert!(!clean(&w), "nothing was reset");
    assert_eq!(hash(&w, "notes/new.txt"), originals[3]);
    // repaired → the discard resets to the attempt's start
    std::fs::write(&victim, "untracked\n").unwrap();
    let s2 = script(&env, "s2.json", json!({
        "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
        "reviewer": [turn(vec![approved()])],
    }));
    let (code, err) = toryo(&env, &["resume", "S-1", "--discard-partial", "--yes", "--fake-agents", &s2], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "S-1").state, St::Completed);
    assert!(clean(&w));
}

// ── R38: lock first ──────────────────────────────────────────────────────────────────

#[test]
fn lock_first_recovery() {
    let env = setup("codex-openai");
    create(&env, "A-1");
    let (code, err) = toryo(&env, &["task", "create", "--file", "TASK.md", "--id", "B-1"], &[]);
    assert_eq!(code, 0, "{err}");
    // A runs a long stage
    let hang_s = script(&env, "hang.json", json!({ "planner": [turn(vec![plan()])], "implementer": [turn(vec![hang()])] }));
    let mut a = spawn(&env, &["run", "A-1", "--yes", "--fake-agents", &hang_s], "a.log");
    wait_until("A to be in its implement stage", 60, || attempts(&env, "A-1").iter().any(|x| x.stage == "implement" && x.status == AttemptStatus::Running));
    // B: `resume` without an id skips A and runs B
    let b_s = script(&env, "b.json", json!({ "planner": [turn(vec![plan()])] }));
    let (_, err) = toryo(&env, &["resume", "--fake-agents", &b_s], &[]);
    assert!(err.contains("A-1 is active") && err.contains("skipped"), "{err}");
    assert_eq!(task(&env, "B-1").state, St::WaitingPlanApproval, "{err}");
    // every mutating command on A refuses with task_active, touching nothing
    for args in [vec!["resume", "A-1"], vec!["approve", "A-1", "--gate", "plan"], vec!["task", "cleanup", "A-1", "--force"], vec!["reject", "A-1", "--gate", "plan", "--reason", "x"]] {
        let (code, err) = toryo(&env, &args, &[]);
        assert_eq!(code, 3, "{args:?}: {err}");
        assert!(err.contains("task_active"), "{err}");
    }
    assert!(a.try_wait().unwrap().is_none(), "A is still running");
    assert!(attempts(&env, "A-1").iter().any(|x| x.stage == "implement" && x.status == AttemptStatus::Running));
    // stop: the attempt is interrupted within a few seconds and A exits
    let t0 = Instant::now();
    let (code, err) = toryo(&env, &["stop", "A-1"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("stopped"), "{err}");
    assert!(t0.elapsed() < Duration::from_secs(8), "stop took {:?}", t0.elapsed());
    a.wait().unwrap();
    let imp = attempts(&env, "A-1").into_iter().find(|x| x.stage == "implement").unwrap();
    assert_eq!(imp.status, AttemptStatus::Interrupted);
    // a stop with no owner says so
    let (code, err) = toryo(&env, &["stop", "A-1"], &[]);
    assert_eq!(code, 0);
    assert!(err.contains("no_owner"), "{err}");
    // kill the next owner; two concurrent resumes: exactly one reconciles
    let hang2 = script(&env, "hang2.json", json!({ "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), hang()])] }));
    let mut a2 = spawn(&env, &["resume", "A-1", "--yes", "--fake-agents", &hang2], "a2.log");
    wait_until("A's second owner to run", 60, || attempts(&env, "A-1").iter().any(|x| x.id == "A-1/implement/2" && x.status == AttemptStatus::Running));
    // a leftover request for this owner: the owner dies before handling it → expired for the next
    let st = db(&env);
    let g = task(&env, "A-1").lock_generation;
    a2.kill().unwrap();
    a2.wait().unwrap();
    let leftover = st.call(move |tx| store::insert_stop(tx, "A-1", g, "test")).unwrap();
    let ok = script(&env, "ok.json", json!({ "implementer": [turn(vec![implementation()])], "reviewer": [turn(vec![approved()])] }));
    let r1 = command(&env, &["resume", "A-1", "--yes", "--fake-agents", &ok], &[]).stderr(Stdio::piped()).spawn().unwrap();
    let r2 = command(&env, &["resume", "A-1", "--yes", "--fake-agents", &ok], &[]).stderr(Stdio::piped()).spawn().unwrap();
    let (o1, o2) = (r1.wait_with_output().unwrap(), r2.wait_with_output().unwrap());
    let mut codes = vec![o1.status.code().unwrap(), o2.status.code().unwrap()];
    codes.sort();
    assert_eq!(codes, [0, 3], "{}\n{}", String::from_utf8_lossy(&o1.stderr), String::from_utf8_lossy(&o2.stderr));
    assert_eq!(task(&env, "A-1").state, St::Completed);
    let row = st.call(move |tx| store::control_request(tx, leftover)).unwrap();
    assert_eq!(row.outcome.as_deref(), Some("expired"), "a request never acts on a later owner");
    assert!(clean(&wt(&env, "A-1")));
}
