//! M5 acceptance (plan §8, §16, §17): the Git lifecycle hardening of the engine.
//!
//! - `protected_ignored_files` (R35): ignored / newly-ignored protected files written by an
//!   agent are caught before anything else runs; `resume` re-compares (restore) or accepts
//!   (`--accept-protected-change`, new baseline, DECISIONS.md)
//! - frozen configuration: a changed toryo.yaml has no effect until `task config --reload`
//! - `policy.deny_paths` at plan time and at the path policy / checkpoint
//! - `implementer_temp_scoped` (R46)
//! - `implementer_fallback` a–d (R49): quota switch with hand-over + `Toryo-Agent`, 429
//!   backoff then switch, `--no-fallback` → Paused, reviewer quota → Paused
//! - `git_filter_textconv_canary` (R37) in the engine: trusted filter runs, the repo's
//!   filter/textconv never do; untrusted filter → `untrusted_git_filter` at task create
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use std::path::Path;
use toryo::engine::store::{self, AttemptStatus};
use toryo::engine::PipelineState as St;

fn log_files(env: &Env, id: &str) -> String {
    git(
        &wt(env, id),
        &["log", "--name-only", "--format=--%H", "main..HEAD"],
    )
}

fn agents(env: &Env, id: &str) -> Vec<String> {
    git(
        &wt(env, id),
        &[
            "log",
            "--format=%(trailers:key=Toryo-Agent,valueonly,separator=)",
            "main..HEAD",
        ],
    )
    .lines()
    .map(|l| l.trim().to_string())
    .filter(|l| !l.is_empty())
    .collect()
}

fn run_ok(env: &Env, args: &[&str]) -> String {
    let (code, err) = toryo(env, args, &[]);
    assert!(code == 0 || code == 1, "{err}");
    err
}

// ── R35: protected state on the filesystem ───────────────────────────────────────────

/// Review 2026-10-23 (performance): accepting a protected change walked the worktree and
/// hashed the protected state twice — `protect::check`, then `take_baseline` — with nothing
/// changed in between. The new baseline is now made from the scan the check just compared.
#[test]
fn accepting_a_protected_change_scans_the_protected_state_once() {
    use toryo::engine::protect;
    let env = setup("codex-openai");
    commit_repo(&env, &[(".gitignore", ".claude/\n")], "ignore local config");
    create(&env, "P-5");
    const SETTINGS: &str = ".claude/settings.local.json";
    let s = script(
        &env,
        "p5.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![
                write("src/Calculator.psm1", DIVIDE),
                write(SETTINGS, "{\"permissions\":{}}\n"),
                implementation(),
            ])],
        }),
    );
    let err = run_ok(&env, &["run", "P-5", "--yes", "--fake-agents", &s]);
    let t = task(&env, "P-5");
    assert_eq!(t.state, St::Escalated, "{err}");

    let st = db(&env);
    let before = protect::scans_on_this_thread();
    let ok = toryo::engine::commands::resume_protected(&st, &t, &[SETTINGS.to_string()]).unwrap();
    let scans = protect::scans_on_this_thread() - before;
    eprintln!("accept a protected change: {scans} scan(s) of the protected state");
    assert!(ok, "accepted");
    assert_eq!(scans, 1, "check and the new baseline share one scan");
    // the new baseline holds the state the check saw
    let m: Value =
        serde_json::from_slice(&std::fs::read(t.artifacts().join("protected/2.json")).unwrap())
            .unwrap();
    let want =
        toryo::engine::task::sha256_hex(&std::fs::read(wt(&env, "P-5").join(SETTINGS)).unwrap());
    assert_eq!(m["entries"][SETTINGS]["sha256"], want.as_str(), "{m:#}");
}

#[test]
fn protected_ignored_files() {
    let env = setup_with(
        "codex-openai",
        "policy:\n  protected_paths: [\"build-cfg/**\"]\n",
    );
    commit_repo(
        &env,
        &[(".gitignore", ".claude/\n*.local.yaml\n")],
        "ignore local config",
    );
    create(&env, "P-1");
    let s = script(
        &env,
        "p1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![
                    write("src/Calculator.psm1", DIVIDE),
                    write(".claude/settings.local.json", "{\"permissions\":{\"allow\":[\"Bash(*)\"]}}\n"),
                    write("toryo.local.yaml", "roles: {}\n"),
                    write(".gitignore", "build-cfg/\n"),
                    write("build-cfg/tool.yaml", "evil: true\n"),
                    implementation(),
                ]),
                turn(vec![implementation()]),
            ],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let err = run_ok(&env, &["run", "P-1", "--yes", "--fake-agents", &s]);
    let t = task(&env, "P-1");
    assert_eq!(t.state, St::Escalated, "{err}");
    let why = t.machine.reason().unwrap().to_string();
    for p in [
        ".claude/settings.local.json",
        "toryo.local.yaml",
        "build-cfg/tool.yaml",
    ] {
        assert!(why.contains(p), "{p} not detected: {why}");
    }
    assert!(why.starts_with("protected_config_changed"));
    let a = attempts(&env, "P-1")
        .into_iter()
        .find(|a| a.stage == "implement")
        .unwrap();
    assert_eq!(a.status, AttemptStatus::Failed);
    assert_eq!(a.error_class.as_deref(), Some("protected_config_changed"));
    assert!(a.context_sha256.is_some());
    assert!(
        git(&wt(&env, "P-1"), &["log", "--oneline", "main..HEAD"])
            .trim()
            .is_empty(),
        "nothing was committed"
    );
    assert!(
        calls(&env, "P-1").iter().all(|c| c["key"] != "reviewer"),
        "no further process started"
    );
    let diff = std::fs::read_dir(t.artifacts())
        .unwrap()
        .flatten()
        .find(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("protected-changes-")
        })
        .unwrap()
        .path();
    let d = std::fs::read_to_string(&diff).unwrap();
    assert!(d.contains("+evil: true") && d.contains("Bash(*)"), "{d}");

    // plain resume: still changed → stays Escalated
    let (code, err) = toryo(&env, &["resume", "P-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    assert_eq!(task(&env, "P-1").state, St::Escalated);
    // restore two, accept one — an accept list that misses a change is refused
    std::fs::remove_file(wt(&env, "P-1").join("toryo.local.yaml")).unwrap();
    std::fs::remove_dir_all(wt(&env, "P-1").join("build-cfg")).unwrap();
    std::fs::write(wt(&env, "P-1").join("toryo.local.yaml"), "x: 1\n").unwrap();
    let (code, err) = toryo(
        &env,
        &[
            "resume",
            "P-1",
            "--accept-protected-change",
            ".claude/settings.local.json",
            "--yes",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    assert_eq!(code, 1);
    assert!(
        err.contains("not accepted: added toryo.local.yaml"),
        "{err}"
    );
    std::fs::remove_file(wt(&env, "P-1").join("toryo.local.yaml")).unwrap();
    let s2 = script(
        &env,
        "p1b.json",
        json!({
            "implementer": [turn(vec![implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(
        &env,
        &[
            "resume",
            "P-1",
            "--accept-protected-change",
            ".claude/settings.local.json",
            "--yes",
            "--fake-agents",
            &s2,
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "P-1");
    assert_eq!(t.state, St::Completed, "{err}");
    let dec = std::fs::read_to_string(t.artifacts().join("DECISIONS.md")).unwrap();
    assert!(
        dec.contains("protected change accepted") && dec.contains(".claude/settings.local.json"),
        "{dec}"
    );
    assert!(
        t.artifacts().join("protected/2.json").exists(),
        "a new baseline version"
    );
    // every attempt after the accept compared against v2
    let st = db(&env);
    let ids: Vec<Option<String>> = st
        .call(|tx| {
            let mut q = tx.prepare("SELECT protected_manifest_id FROM stage_attempts WHERE task_id = 'P-1' ORDER BY started_at")?;
            let v = q.query_map([], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?;
            Ok(v)
        })
        .unwrap();
    assert_eq!(
        ids.first().cloned().flatten().as_deref(),
        Some("P-1/protected/1")
    );
    assert_eq!(
        ids.last().cloned().flatten().as_deref(),
        Some("P-1/protected/2")
    );
    assert!(!log_files(&env, "P-1").contains("build-cfg"));
}

#[test]
fn git_metadata_change_escalates_before_the_next_stage() {
    let env = setup("codex-openai");
    create(&env, "P-2");
    let s = script(&env, "p2.json", json!({ "planner": [turn(vec![plan()])] }));
    run_ok(&env, &["run", "P-2", "--fake-agents", &s]);
    assert_eq!(task(&env, "P-2").state, St::WaitingPlanApproval);
    // someone adds a hook-ish config to the repo's .git/config while the task waits
    let cfgp = env.repo.join(".git/config");
    let mut c = std::fs::read_to_string(&cfgp).unwrap();
    c.push_str("[core]\n\tfsmonitor = ./evil.sh\n");
    std::fs::write(&cfgp, c).unwrap();
    let s2 = script(
        &env,
        "p2b.json",
        json!({ "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])] }),
    );
    let (_, err) = toryo(
        &env,
        &[
            "approve",
            "P-2",
            "--gate",
            "plan",
            "--continue",
            "--fake-agents",
            &s2,
        ],
        &[],
    );
    let t = task(&env, "P-2");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert!(t.machine.reason().unwrap().contains("@git/config"));
    assert!(
        attempts(&env, "P-2").iter().all(|a| a.stage == "plan"),
        "the implementer never started: {err}"
    );
    let s3 = script(
        &env,
        "p2c.json",
        json!({
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(
        &env,
        &[
            "resume",
            "P-2",
            "--accept-protected-change",
            "@git/config",
            "--yes",
            "--fake-agents",
            &s3,
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "P-2").state, St::Completed, "{err}");
}

// ── frozen configuration + task config --reload (R35) ────────────────────────────────

#[test]
fn config_is_frozen_until_reload() {
    let env = setup("codex-openai");
    create(&env, "F-1");
    let s = script(
        &env,
        "f1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]), turn(vec![write("src/Calculator.psm1", "\n# NOPE-MARKER\n"), implementation()])],
            "reviewer": [turn(vec![approved()]), turn(vec![approved()])],
        }),
    );
    run_ok(&env, &["run", "F-1", "--fake-agents", &s]);
    // a toryo.yaml edit while the task runs: a verify command that cannot pass yet
    let text = std::fs::read_to_string(&env.config).unwrap();
    std::fs::write(
        &env.config,
        text.replace("args: [\"grep\", \"-q\", \"Divide-Numbers\", \"--\", \"src\"] }", "args: [\"grep\", \"-q\", \"Divide-Numbers\", \"--\", \"src\"] }\n    - { name: nope-marker, program: git, args: [\"grep\", \"-q\", \"NOPE-MARKER\", \"--\", \"src\"] }"),
    )
    .unwrap();
    // no effect on the running task: it reaches the final gate with the frozen verify list
    let (code, err) = toryo(
        &env,
        &[
            "approve",
            "F-1",
            "--gate",
            "plan",
            "--continue",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "F-1");
    assert_eq!(t.state, St::WaitingFinalApproval, "{err}");
    let vr: Value =
        serde_json::from_slice(&std::fs::read(t.artifacts().join("VERIFY_RESULT.json")).unwrap())
            .unwrap();
    assert_eq!(vr["commands"].as_array().unwrap().len(), 1);
    // --reload with a wrong --expect changes nothing
    let (code, err) = toryo(
        &env,
        &["task", "config", "F-1", "--reload", "--expect", "ffffffff"],
        &[],
    );
    assert_eq!(code, 2, "{err}");
    assert!(
        err.contains("subject_changed") && err.contains("nope-marker"),
        "{err}"
    );
    assert_eq!(task(&env, "F-1").config_sha256, t.config_sha256);
    // R23-24: and it leaves no history pair behind (was: `1-old-…`, `1-new-…`)
    let history = |t: &toryo::engine::store::TaskRow| {
        std::fs::read_dir(t.artifacts().join("config-history"))
            .map(|d| d.count())
            .unwrap_or(0)
    };
    assert_eq!(history(&t), 0, "a refused reload is no history");
    // --reload applies: diff shown, VERIFY_RESULT invalidated, the checkpoint is verified again
    let (code, err) = toryo(&env, &["task", "config", "F-1", "--reload"], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(history(&task(&env, "F-1")), 2, "the pair of the reload");
    let t = task(&env, "F-1");
    assert_eq!(t.state, St::Verifying);
    let vr: Value =
        serde_json::from_slice(&std::fs::read(t.artifacts().join("VERIFY_RESULT.json")).unwrap())
            .unwrap();
    assert_eq!(
        vr["invalidated"],
        json!(true),
        "a new version marks the old result invalid"
    );
    let (code, err) = toryo(&env, &["approve", "F-1", "--gate", "final"], &[]);
    assert_ne!(code, 0, "no final gate while re-verifying: {err}");
    assert!(std::fs::read_to_string(t.artifacts().join("DECISIONS.md"))
        .unwrap()
        .contains("configuration reloaded"));
    // verify now fails → fix cycle → passes → review → final gate
    let s2 = script(
        &env,
        "f1b.json",
        json!({
            "implementer": [turn(vec![write("src/Calculator.psm1", "\n# NOPE-MARKER\n"), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "F-1", "--fake-agents", &s2], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "F-1");
    assert_eq!(t.state, St::WaitingFinalApproval, "{err}");
    assert_eq!(t.fix_cycle, 1);
    // not in a running state? refused outside waiting states is covered by the lock + state check
    let (code, _) = toryo(&env, &["task", "config", "F-1"], &[]);
    assert_eq!(code, 0);
}

// ── policy.deny_paths (§8) ───────────────────────────────────────────────────────────

#[test]
fn deny_paths_at_plan_time_and_at_the_path_policy() {
    let env = setup_with("codex-openai", "policy:\n  deny_paths: [\"**/ZZZ/**\"]\n");
    create(&env, "D-1");
    let zzz_plan = json!({"kind":"complete","structured":{"understanding":"u","files_likely_involved":["Metadata/ZZZ/Calc.xml","src/Calculator.psm1"],"steps":["add"],"validation":["tests"],"plan_markdown":"md"}});
    let s = script(
        &env,
        "d1.json",
        json!({
            "planner": [turn(vec![zzz_plan]), turn(vec![plan()])],
            "implementer": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), write("Metadata/zzz/Calc.xml", "<local/>"), implementation()]),
                turn(vec![implementation()]),
            ],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "D-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "D-1");
    assert_eq!(t.state, St::Completed, "{err}");
    let c = calls(&env, "D-1");
    assert!(
        c[1]["prompt"]
            .as_str()
            .unwrap()
            .contains("Metadata/ZZZ/Calc.xml"),
        "re-plan feedback names the denied path"
    );
    let fix = c
        .iter()
        .filter(|x| x["key"] == "implementer")
        .nth(1)
        .unwrap();
    assert!(
        fix["prompt"]
            .as_str()
            .unwrap()
            .contains("Metadata/zzz/Calc.xml"),
        "the fix turn is told what was restored"
    );
    assert!(
        !log_files(&env, "D-1").to_lowercase().contains("zzz"),
        "never committed"
    );
    assert!(
        !wt(&env, "D-1").join("Metadata/zzz/Calc.xml").exists(),
        "restored (removed)"
    );
}

// ── R46: per-attempt TEMP inside the worktree ───────────────────────────────────────

#[test]
fn implementer_temp_scoped() {
    let env = setup("codex-openai");
    create(&env, "T-1");
    let t = task(&env, "T-1");
    let s = script(
        &env,
        "t1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), write("$TEMP/first.txt", "scratch"), implementation()]),
                turn(vec![write("tests/Divide.Tests.ps1", "# t\n"), write("$TEMP/second.txt", "scratch"), implementation()]),
            ],
            "reviewer": [turn(vec![changes()]), turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "T-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "T-1").state, St::Completed, "{err}");
    let tmp = wt(&env, "T-1").join(".toryo-tmp");
    assert!(
        tmp.join("second.txt").exists(),
        "the TEMP write landed in <worktree>/.toryo-tmp"
    );
    assert!(
        !tmp.join("first.txt").exists(),
        "recreated empty before the next attempt"
    );
    assert!(
        !log_files(&env, "T-1").contains(".toryo-tmp"),
        "never committed"
    );
    // exactly one toryo line in the common info/exclude, written before protected baseline v1
    let excl = env.repo.join(".git/info/exclude");
    let text = std::fs::read_to_string(&excl).unwrap();
    assert_eq!(
        text.lines().filter(|l| l.contains("toryo")).count(),
        1,
        "{text}"
    );
    let v1: Value =
        serde_json::from_slice(&std::fs::read(t.artifacts().join("protected/1.json")).unwrap())
            .unwrap();
    assert_eq!(
        v1["entries"]["@git/info/exclude"]["sha256"]
            .as_str()
            .unwrap(),
        toryo::engine::task::sha256_hex(text.as_bytes())
    );
    assert!(v1["entries"]
        .as_object()
        .unwrap()
        .keys()
        .all(|k| !k.starts_with(".toryo-tmp")));
}

// ── R49: implementer fallback ────────────────────────────────────────────────────────

#[test]
fn implementer_fallback_a_quota_switches_sticky_with_handover() {
    let env = setup("codex-openai");
    create(&env, "Q-1");
    let s = script(
        &env,
        "q1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Partial.ps1", "# partial\n"), fail("QuotaExceeded", "You've hit your usage limit")])],
            "implementer@claude-max": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]),
                turn(vec![write("tests/Divide.Tests.ps1", "# t\n"), implementation()]),
            ],
            "reviewer": [turn(vec![changes()]), turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "Q-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "Q-1").state, St::Completed, "{err}");
    let c = calls(&env, "Q-1");
    let claude: Vec<&Value> = c
        .iter()
        .filter(|x| x["key"] == "implementer@claude-max")
        .collect();
    assert_eq!(claude.len(), 2, "the fix cycle stayed on Claude (sticky)");
    let p = claude[0]["prompt"].as_str().unwrap();
    assert!(
        p.contains("Hand-over") && p.contains("src/Partial.ps1") && p.contains("NO shell"),
        "{p}"
    );
    assert!(claude[0]["context"]
        .as_str()
        .unwrap()
        .contains("not accessible in this session"));
    assert_eq!(
        agents(&env, "Q-1"),
        vec!["claude-max", "claude-max"],
        "Toryo-Agent names the profile"
    );
    let a = attempts(&env, "Q-1");
    let imp2 = a.iter().find(|x| x.id == "Q-1/implement/2").unwrap();
    assert_eq!(imp2.fallback_from.as_deref(), Some("codex-openai"));
    assert!(
        log_files(&env, "Q-1").contains("src/Partial.ps1"),
        "the partial work was kept"
    );
}

#[test]
fn implementer_fallback_b_rate_limit_backs_off_then_switches() {
    let env = setup("codex-openai");
    create(&env, "Q-2");
    let rl = || turn(vec![fail("RateLimited", "429 Too Many Requests")]);
    let s = script(
        &env,
        "q2.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [rl(), rl(), rl()],
            "implementer@claude-max": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "Q-2", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains("retry 1/2") && err.contains("retry 2/2"),
        "{err}"
    );
    let imp: Vec<_> = attempts(&env, "Q-2")
        .into_iter()
        .filter(|a| a.stage == "implement")
        .collect();
    assert_eq!(
        imp.iter()
            .map(|a| a.profile.clone().unwrap())
            .collect::<Vec<_>>(),
        vec!["codex-openai", "codex-openai", "codex-openai", "claude-max"]
    );
    let evs = db(&env).call(|tx| store::events(tx, "Q-2")).unwrap();
    assert!(evs.iter().any(|e| e.1 == "implementer_switch"
        && e.2["reason"].as_str().unwrap().contains("after 2 retries")));
}

#[test]
fn implementer_fallback_c_no_fallback_pauses_and_keeps_partial_work() {
    let env = setup("codex-openai");
    create(&env, "Q-3");
    let s = script(
        &env,
        "q3.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Partial.ps1", "# partial\n"), fail("QuotaExceeded", "usage limit")])],
        }),
    );
    let (code, err) = toryo(
        &env,
        &["run", "Q-3", "--yes", "--no-fallback", "--fake-agents", &s],
        &[],
    );
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "Q-3");
    assert_eq!(t.state, St::Paused, "{err}");
    assert!(
        t.machine.reason().unwrap().starts_with("implementer_quota"),
        "{:?}",
        t.machine.reason()
    );
    assert!(
        wt(&env, "Q-3").join("src/Partial.ps1").exists(),
        "partial work kept"
    );
    let out = status_out(&env, "Q-3");
    assert!(out.contains("Q-3") && out.contains("Paused"), "{out}");
    // the quota is back: resume starts the chain again from the primary
    let s2 = script(
        &env,
        "q3b.json",
        json!({
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(
        &env,
        &[
            "resume",
            "Q-3",
            "--yes",
            "--no-fallback",
            "--fake-agents",
            &s2,
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "Q-3").state, St::Completed, "{err}");
    assert_eq!(agents(&env, "Q-3"), vec!["codex-openai"]);
    let c = calls(&env, "Q-3");
    assert!(
        c.iter().find(|x| x["key"] == "implementer").unwrap()["prompt"]
            .as_str()
            .unwrap()
            .contains("src/Partial.ps1")
    );
}

#[test]
fn implementer_fallback_d_reviewer_quota_pauses() {
    let env = setup("codex-openai");
    create(&env, "Q-4");
    let s = script(
        &env,
        "q4.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![fail("QuotaExceeded", "You've hit your limit · resets 5pm")])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "Q-4", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "Q-4");
    assert_eq!(t.state, St::Paused);
    assert!(t.machine.reason().unwrap().starts_with("reviewer_quota"));
    assert_eq!(t.machine.paused_from(), Some(St::Reviewing));
    let s2 = script(
        &env,
        "q4b.json",
        json!({ "reviewer": [turn(vec![approved()])] }),
    );
    let (code, err) = toryo(&env, &["resume", "Q-4", "--yes", "--fake-agents", &s2], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "Q-4").state, St::Completed);
    // the reviewer never changes because of a fallback (owner decision 15)
    assert!(attempts(&env, "Q-4")
        .iter()
        .filter(|a| a.stage == "review")
        .all(|a| a.profile.as_deref() == Some("claude-max")));
}

// ── R37: filters and textconv in the engine ──────────────────────────────────────────

fn marker_cmd(marker: &Path) -> String {
    format!(
        "sh -c 'echo hit >> \"{}\"; cat'",
        marker.display().to_string().replace('\\', "/")
    )
}

#[test]
fn git_filter_textconv_canary() {
    let env = setup("codex-openai");
    let repo_marker = env.root.join("repo-filter-ran");
    commit_repo(
        &env,
        &[
            (".gitattributes", "*.bin filter=lfs diff=hex\n"),
            ("data/a.bin", "\u{0}\u{1}\u{2}old"),
        ],
        "binary data",
    );
    let set = |k: &str, v: &str| {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&env.repo)
            .args(["config", k, v])
            .status()
            .unwrap()
            .success())
    };
    set("filter.lfs.clean", &marker_cmd(&repo_marker));
    set("filter.lfs.smudge", &marker_cmd(&repo_marker));
    set("filter.lfs.process", &marker_cmd(&repo_marker));
    set("diff.hex.textconv", &marker_cmd(&repo_marker));
    // without a trusted `lfs`: admission refuses the task
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "G-0"],
        &[],
    );
    assert_ne!(code, 0);
    assert!(err.contains("untrusted_git_filter"), "{err}");
    // with `lfs` trusted on this machine: the trusted process runs, the repo's never does
    let trusted_marker = env.root.join("trusted-filter-ran");
    let exe = env!("CARGO_BIN_EXE_toryo").replace('\\', "/");
    std::fs::write(
        &env.machine,
        format!(
            "{}{}git:\n  trusted_filters:\n    lfs:\n      process: \"'{exe}' spike filter-process --marker '{}'\"\n      required: true\n",
            common::MACHINE_PROFILES,
            common::MACHINE_FLOOR,
            trusted_marker.display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();
    create(&env, "G-1");
    let s = script(
        &env,
        "g1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), json!({"kind":"write_file","path":"data/a.bin","content":"\u{0}\u{3}\u{4}new"}), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "G-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "G-1").state, St::Completed, "{err}");
    let patch = std::fs::read(task(&env, "G-1").artifacts().join("DIFF.patch")).unwrap();
    let p = String::from_utf8_lossy(&patch);
    assert!(
        p.contains("GIT binary patch") || p.contains("Binary files"),
        "raw binary hunk, no textconv: {p}"
    );
    assert!(
        !repo_marker.exists(),
        "the repo's filter/textconv never ran"
    );
    assert!(trusted_marker.exists(), "the trusted filter process ran");
}
