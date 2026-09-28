//! M6 acceptance (plan §8, §9, §13, §17): immutable artifacts and the verify checkout.
//!
//! - `artifact_crash_points` (R34): a crash before publish / after publish / after the DB
//!   commit of a second `review` version; resume reconciles (staging removed, orphan moved,
//!   never `artifact_tampered`); a deleted or edited version → Escalated
//! - the verify checkout (R29/R45/R47): a detached checkout of the checkpoint under
//!   `verify_root`; ignored junk in the task worktree never reaches Verify; a verify command
//!   that edits a tracked file → `verify_mutated_tree`; `ansi_stripped` (R50)
//! - `verify_root_label_probe` (R45): admission refused with `verify_boundary_unavailable`
//! - `verify_boundary_writes` (R36, Windows): Low-IL verify cannot write toryo's state
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use toryo::engine::artifacts;
use toryo::engine::store;
use toryo::engine::PipelineState as St;

fn versions(env: &Env, id: &str, logical: &str) -> Vec<(u32, String)> {
    let (i, l) = (id.to_string(), logical.to_string());
    db(env)
        .call(move |tx| {
            let mut q = tx.prepare("SELECT version, path FROM artifacts WHERE task_id = ?1 AND logical_name = ?2 ORDER BY version")?;
            let v = q.query_map([&i, &l], |r| Ok((r.get::<_, i64>(0)? as u32, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(v)
        })
        .unwrap()
}

fn crash_run(point: &str) -> (Env, String) {
    let env = setup("codex-openai");
    let id = "A-1";
    create(&env, id);
    let s = script(&env, "a.json", json!({
        "planner": [turn(vec![plan()])],
        "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()]), turn(vec![write("tests/Divide.Tests.ps1", "# t\n"), implementation()])],
        "reviewer": [turn(vec![changes()]), turn(vec![approved()])],
    }));
    let (code, err) = toryo(&env, &["run", id, "--yes", "--fake-agents", &s], &[("TORYO_TEST_CRASH_AT", point)]);
    assert_eq!(code, 86, "crashed at {point}: {err}");
    (env, id.to_string())
}

fn resume_to_completion(env: &Env, id: &str) -> String {
    let s = script(env, "b.json", json!({ "reviewer": [turn(vec![approved()])] }));
    let (code, err) = toryo(env, &["resume", id, "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(env, id).state, St::Completed, "{err}");
    err
}

#[test]
fn artifact_crash_points() {
    // (a) crash before publish: only a staging file → removed; v1 stays the head
    let (env, id) = crash_run("artifact:review@2:before_publish");
    let t = task(&env, &id);
    assert_eq!(versions(&env, &id, "review").len(), 1);
    assert_eq!(std::fs::read_dir(t.artifacts().join("artifacts/.staging")).unwrap().count(), 1);
    let err = resume_to_completion(&env, &id);
    assert!(err.contains("staging leftover removed"), "{err}");
    assert_eq!(versions(&env, &id, "review").iter().map(|v| v.0).collect::<Vec<_>>(), vec![1, 2], "the re-run review is v2");

    // (b) crash after publish, before the DB commit: an orphan, never artifact_tampered
    let (env, id) = crash_run("artifact:review@2:after_publish");
    let t = task(&env, &id);
    assert_eq!(versions(&env, &id, "review").len(), 1);
    let err = resume_to_completion(&env, &id);
    assert!(err.contains("orphan") && !err.contains("artifact_tampered"), "{err}");
    let orphans: Vec<_> = std::fs::read_dir(t.artifacts().join("orphans/review")).unwrap().flatten().collect();
    assert_eq!(orphans.len(), 1);
    assert!(db(&env).call(|tx| store::events(tx, "A-1")).unwrap().iter().any(|e| e.1 == "artifact_orphan"));
    assert_eq!(versions(&env, &id, "review").len(), 2);

    // (c) crash after the DB commit: v2 is registered and stays; the review re-runs as v3
    let (env, id) = crash_run("artifact:review@2:after_commit");
    assert_eq!(versions(&env, &id, "review").len(), 2);
    resume_to_completion(&env, &id);
    assert_eq!(versions(&env, &id, "review").iter().map(|v| v.0).collect::<Vec<_>>(), vec![1, 2, 3]);
    // every registered version is readable and hash-checks; old versions are never overwritten
    let t = task(&env, &id);
    for logical in ["review", "plan", "diff", "verify-result", "task", "config", "protected"] {
        let st = db(&env);
        for (_, p) in versions(&env, &id, logical) {
            let (l, pp) = (logical.to_string(), p.clone());
            let sha: String = st.call(move |tx| Ok(tx.query_row("SELECT sha256 FROM artifacts WHERE task_id='A-1' AND logical_name=?1 AND path=?2", [&l, &pp], |r| r.get(0))?)).unwrap();
            assert_eq!(toryo::engine::task::sha256_hex(&std::fs::read(t.run_store.join(&p)).unwrap()), sha, "{p}");
        }
    }
}

#[test]
fn deleted_or_edited_versions_escalate() {
    let env = setup("codex-openai");
    create(&env, "A-2");
    let s = script(&env, "a2.json", json!({ "planner": [turn(vec![plan()])] }));
    toryo(&env, &["run", "A-2", "--fake-agents", &s], &[]);
    let t = task(&env, "A-2");
    assert_eq!(t.state, St::WaitingPlanApproval);
    // the view is not evidence: editing PLAN.md at the root changes nothing
    std::fs::write(t.artifacts().join("PLAN.md"), "edited view").unwrap();
    // an edited immutable version is caught at the next resume (reconciliation)
    let v = artifacts::head(&db(&env), &t, "plan-md").unwrap().unwrap();
    std::fs::write(&v.path, "tampered").unwrap();
    let s2 = script(&env, "a2b.json", json!({ "implementer": [turn(vec![implementation()])] }));
    let (_, err) = toryo(&env, &["approve", "A-2", "--gate", "plan", "--continue", "--fake-agents", &s2], &[]);
    let t = task(&env, "A-2");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert!(t.machine.reason().unwrap().starts_with("artifact_tampered"), "{:?}", t.machine.reason());
    // a deleted version → artifact_missing
    let env = setup("codex-openai");
    create(&env, "A-3");
    let t = task(&env, "A-3");
    let v = artifacts::head(&db(&env), &t, "task").unwrap().unwrap();
    std::fs::remove_file(&v.path).unwrap();
    let s = script(&env, "a3.json", json!({ "planner": [turn(vec![plan()])] }));
    toryo(&env, &["run", "A-3", "--fake-agents", &s], &[]);
    let t = task(&env, "A-3");
    assert_eq!(t.state, St::Escalated);
    assert!(t.machine.reason().unwrap().starts_with("artifact_missing"), "{:?}", t.machine.reason());
}

// ── the verify checkout (R29, R45, R47, R50) ─────────────────────────────────────────

fn with_verify_commands(env: &Env, cmds: &str) {
    let text = std::fs::read_to_string(&env.config).unwrap();
    let start = text.find("commands:").unwrap();
    let end = text[start..].find("verify: {").map(|e| start + e).unwrap_or(text.len());
    std::fs::write(&env.config, format!("{}commands:\n  verify:\n{cmds}{}", &text[..start], &text[end..])).unwrap();
}

#[test]
fn verify_runs_in_a_clean_checkout_of_the_checkpoint() {
    let env = setup("codex-openai");
    with_verify_commands(
        &env,
        concat!(
            "    - { name: divide-present, program: git, args: [\"grep\", \"-q\", \"Divide-Numbers\", \"--\", \"src\"] }\n",
            "    - { name: listing, program: git, args: [\"status\", \"--porcelain\", \"--ignored\", \"--untracked-files=all\"] }\n",
            "    - { name: ansi, program: git, args: [\"log\", \"-1\", \"--format=%x1b[31mRED%x1b[0m %x1b]0;TITLE%x07DONE\"] }\n",
        ),
    );
    create(&env, "V-1");
    // ignored junk in the task worktree (a stale build output, a local override)
    std::fs::create_dir_all(wt(&env, "V-1").join(".toryo-tmp")).unwrap();
    std::fs::write(wt(&env, "V-1").join(".toryo-tmp/JUNK-IN-WORKTREE.txt"), "junk").unwrap();
    let s = script(&env, "v1.json", json!({
        "planner": [turn(vec![plan()])],
        "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
        "reviewer": [turn(vec![approved()])],
    }));
    let (code, err) = toryo(&env, &["run", "V-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "V-1");
    assert_eq!(t.state, St::Completed, "{err}");
    let vr: Value = serde_json::from_slice(&std::fs::read(t.artifacts().join("VERIFY_RESULT.json")).unwrap()).unwrap();
    let head = t.machine.checkpoint().unwrap();
    assert_eq!(vr["head"].as_str(), Some(head));
    assert_eq!(vr["tree"].as_str().unwrap().len(), 40);
    assert!(vr["boundary"].as_str().unwrap().contains("not isolated"), "boundary none is stated: {vr}");
    let checkout = std::path::PathBuf::from(vr["verify_checkout"].as_str().unwrap());
    assert!(checkout.starts_with(env.root.join("verify").join("V-1")), "{}", checkout.display());
    let listing = vr["commands"][1]["output_tail"].as_str().unwrap();
    assert!(!listing.contains("JUNK"), "ignored junk of the worktree never reaches Verify: {listing}");
    // R50: ANSI (CSI + OSC) stripped, text otherwise unchanged
    let ansi = vr["commands"][2]["output_tail"].as_str().unwrap();
    assert_eq!(ansi.trim(), "RED DONE", "{ansi:?}");
    assert!(!serde_json::to_string(&vr).unwrap().contains("\\u001b"));
    // cleanup removes the verify checkout too
    let (code, err) = toryo(&env, &["task", "cleanup", "V-1"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(!env.root.join("verify").join("V-1").exists());
    assert!(!git(&env.repo, &["worktree", "list"]).contains("V-1.verify"));
}

#[test]
fn a_verify_command_that_edits_a_tracked_file_is_verify_mutated_tree() {
    let env = setup("codex-openai");
    with_verify_commands(
        &env,
        "    - { name: sneaky, program: git, args: [\"config\", \"-f\", \"app.ini\", \"toryo.injected\", \"yes\"] }\n",
    );
    commit_repo(&env, &[("app.ini", "[app]\n\tname = calc\n")], "tracked ini");
    create(&env, "V-2");
    let s = script(&env, "v2.json", json!({
        "planner": [turn(vec![plan()])],
        "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
    }));
    let (_, err) = toryo(&env, &["run", "V-2", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "V-2");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert!(t.machine.reason().unwrap().starts_with("verify_mutated_tree"), "{:?}", t.machine.reason());
    let vr: Value = serde_json::from_slice(&std::fs::read(t.artifacts().join("VERIFY_RESULT.json")).unwrap()).unwrap();
    assert_eq!(vr["pass"], json!(false));
    assert!(vr["mutated"].as_str().unwrap().contains("app.ini"));
    assert!(clean(&wt(&env, "V-2")), "the task worktree was never touched");
}

// ── R45: the verify boundary must be available at admission ─────────────────────────

#[test]
fn verify_root_label_probe() {
    let root_hint = "verify: { verify_root: \"VERIFY_ROOT\" }\n";
    let env = setup_with("codex-openai", root_hint);
    let text = std::fs::read_to_string(&env.config).unwrap().replace("VERIFY_ROOT", &env.root.join("vr").display().to_string().replace('\\', "/"));
    std::fs::write(&env.config, text).unwrap();
    if cfg!(windows) {
        // no WRITE_OWNER on verify_root (injected error 5): refused, the cause is named
        let (code, err) = toryo(&env, &["task", "create", "--file", "TASK.md", "--id", "L-0"], &[("TORYO_TEST_LABEL_ERROR", "5")]);
        assert_ne!(code, 0);
        assert!(err.contains("verify_boundary_unavailable") && err.contains("WRITE_OWNER"), "{err}");
        assert!(!env.root.join("wts").join("L-0").exists(), "nothing was created");
        // the default case passes; the checkout is created under verify_root, verify runs at Low IL
        create(&env, "L-1");
        let s = script(&env, "l1.json", json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }));
        let (code, err) = toryo(&env, &["run", "L-1", "--yes", "--fake-agents", &s], &[]);
        assert_eq!(code, 0, "{err}");
        let t = task(&env, "L-1");
        let vr: Value = serde_json::from_slice(&std::fs::read(t.artifacts().join("VERIFY_RESULT.json")).unwrap()).unwrap();
        assert_eq!(vr["boundary"], json!("low_integrity"), "{vr}");
        assert!(env.root.join("vr").join("L-1").join("L-1.verify").exists());
    } else {
        let (code, err) = toryo(&env, &["task", "create", "--file", "TASK.md", "--id", "L-0"], &[]);
        assert_ne!(code, 0);
        assert!(err.contains("verify_boundary_unavailable") && err.contains("only on Windows"), "{err}");
        assert!(!env.root.join("wts").join("L-0").exists(), "nothing was created");
    }
}

// ── R36: what Low-IL verify code can write (Windows) ─────────────────────────────────

#[cfg(windows)]
#[test]
fn verify_boundary_writes() {
    const PROBE: &str = r#"param([string[]]$Targets)
foreach ($t in $Targets) {
  try { Set-Content -Path (Join-Path $t 'lowil-marker.txt') -Value hit -ErrorAction Stop; "WROTE $t" } catch { "DENIED $t" }
}
try { Set-Content -Path (Join-Path $env:TORYO_VERIFY_BUILD 'out.txt') -Value ok -ErrorAction Stop; 'BUILD ok' } catch { 'BUILD denied' }
try { Set-Content -Path '.git' -Value "gitdir: $env:TEMP/fake" -ErrorAction Stop; 'POINTER rewritten' } catch { 'POINTER denied' }
exit 0
"#;
    for (id, boundary) in [("W-1", "low_integrity"), ("W-2", "none")] {
        let env = setup_with("codex-openai", &format!("verify: {{ boundary: {boundary}, verify_root: \"VR\" }}\n"));
        let fwd = |p: &std::path::Path| p.display().to_string().replace('\\', "/");
        let vr_root = env.root.join("vr");
        let rs = env.data.join("runs").join(id).join("runstore");
        let targets = [rs.clone(), env.data.clone(), wt(&env, id), env.repo.join(".git"), env.root.clone()];
        let args: Vec<String> = ["-NoProfile", "-NonInteractive", "-File", "scripts/probe.ps1"].iter().map(|s| format!("\"{s}\"")).chain(targets.iter().map(|t| format!("\"{}\"", fwd(t)))).collect();
        let text = std::fs::read_to_string(&env.config).unwrap().replace("\"VR\"", &format!("\"{}\"", fwd(&vr_root)));
        std::fs::write(&env.config, text).unwrap();
        with_verify_commands(&env, &format!("    - {{ name: probe, program: pwsh, args: [{}] }}\n", args.join(", ")));
        commit_repo(&env, &[("scripts/probe.ps1", PROBE)], "probe");
        create(&env, id);
        let s = script(&env, "w.json", json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
        }));
        let (_, err) = toryo(&env, &["run", id, "--yes", "--fake-agents", &s], &[]);
        let t = task(&env, id);
        let vr: Value = serde_json::from_slice(&std::fs::read(t.artifacts().join("VERIFY_RESULT.json")).unwrap()).unwrap();
        let out = vr["commands"][0]["output_tail"].as_str().unwrap_or("").to_string();
        let markers: Vec<bool> = targets.iter().map(|d| d.join("lowil-marker.txt").exists()).collect();
        if boundary == "low_integrity" {
            assert_eq!(markers, vec![false; 5], "Low IL wrote toryo state: {out}\n{err}");
            assert!(out.contains("BUILD ok"), "{out}");
            assert_eq!(vr["boundary"], json!("low_integrity"));
            // (4c) the pointer file is writable at Low → detected, toryo kept its admin dir
            assert_eq!(t.state, St::Escalated, "{err}");
            assert!(t.machine.reason().unwrap().contains("pointer"), "{:?}", t.machine.reason());
        } else {
            assert_eq!(markers, vec![true; 5], "negative control: {out}");
            assert!(vr["boundary"].as_str().unwrap().contains("not isolated"));
        }
    }
}
