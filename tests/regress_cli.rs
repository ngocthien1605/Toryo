//! Regression tests for the CLI / configuration / recovery / store fixes of the 2026-09-29
//! review (the review's probes, inverted):
//!
//! - EC-12: a crash inside `task config --reload` leaves the old configuration in force (the
//!   frozen config is the immutable version whose SHA-256 is `tasks.config_sha256`, never the
//!   view), the reload can be repeated, and `task cleanup` works without a readable config
//! - EC-19: `resume --discard-partial` snapshots the worktree again before the reset
//! - EC-23 / DES-05: `fallback: []` means no fallback; typo'd role keys and enum-like values
//!   are refused at load
//! - ARCH-05 (follow-up review of the branch): the Codex checks refuse only what toryo sets
//!   itself or what switches the sandbox, approval or environment policy
//! - EC-28: `%ProgramFiles(x86)%` is expanded (or refused), never left in place
//! - CODE-09: an early error return after taking the task lock clears its owner columns
//!
//! EC-27 (`doctor` version pin), CODE-08 (exit status) and the store actor half of CODE-09
//! are unit tests next to the private code (`doctor.rs`, `main.rs`, `engine/store.rs`).

#[cfg(feature = "m0-kit")]
mod common;

use toryo::config::{expand_vars, Effective};

/// Load `yaml` in the R60 layout: its `profiles:` go to a machine config.
fn load(yaml: &str) -> anyhow::Result<Effective> {
    let d = tempfile::tempdir().unwrap();
    let mut doc: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
    let mut machine = serde_yaml::Mapping::new();
    if let Some(v) = doc.as_mapping_mut().unwrap().remove("profiles") {
        machine.insert("profiles".into(), v);
    }
    let (p, m) = (d.path().join("toryo.yaml"), d.path().join("machine.yaml"));
    std::fs::write(&p, serde_yaml::to_string(&doc).unwrap()).unwrap();
    std::fs::write(&m, serde_yaml::to_string(&machine).unwrap()).unwrap();
    Effective::load(&p, Some(&m))
}

const TWO_PROFILES: &str = "version: 1\nproject: { root: r, worktrees_root: w }\nprofiles:\n  claude-max: { kind: claude, billing: subscription, exe: c }\n  codex-openai: { kind: codex, billing: subscription, exe: x }\nroles:\n  planner: { profile: claude-max }\n";

/// EC-23 (probe `implicit_implementer_fallback`, inverted): `fallback: []` is no fallback; an
/// absent `fallback` keeps the default (the planner's profile); an absent reviewer is the planner.
#[test]
fn ec23_empty_fallback_means_no_fallback() {
    let chain = |implementer: &str| {
        let cfg = load(&format!("{TWO_PROFILES}  implementer: {implementer}\n")).unwrap();
        toryo::engine::task::roles(&cfg).unwrap()
    };
    let (_, c, reviewer) = chain("{ profile: codex-openai, fallback: [] }");
    assert_eq!(c, vec!["codex-openai"], "`fallback: []` adds nothing");
    assert_eq!(reviewer, "claude-max");
    let (_, c, _) = chain("{ profile: codex-openai }");
    assert_eq!(c, vec!["codex-openai", "claude-max"], "absent: the planner");
    let (_, c, _) = chain("{ profile: codex-openai, fallback: [claude-max] }");
    assert_eq!(c, vec!["codex-openai", "claude-max"]);
}

/// EC-23 / DES-05 (probe `unknown_role_key_is_accepted`, inverted).
#[test]
fn ec23_unknown_role_keys_and_enum_values_are_refused() {
    let e = load(
        "version: 1\nproject: { root: r, worktrees_root: w, runs_root: 'D:\\toryo-runs' }\nprofiles:\n  claude-max: { kind: claude, billing: subscription, exe: c }\nroles:\n  planner: { profile: claude-max }\n  implementor: { profile: claude-max }\n  reviwer: { profile: claude-max }\nverify: { boundary: lowintegrity, checkout: tracked, final_clean: true }\n",
    )
    .expect_err("typos in role names / enum-like strings are refused");
    let e = format!("{e:#}");
    for want in [
        "roles.implementor: unknown role (valid: planner, implementer, reviewer)",
        "roles.reviwer: unknown role",
        "verify.boundary: `lowintegrity` is not a valid value (valid: low_integrity, none)",
    ] {
        assert!(e.contains(want), "{want:?} missing in:\n{e}");
    }
}

/// ARCH-05 (the follow-up review's probes, inverted): configurations that loaded before the
/// checks keep loading — more settings of the `codex.provider`'s provider,
/// `sandbox_workspace_write.*`, a `read-only` sandbox — while overrides that re-set what toryo
/// sets and sandbox modes weaker than workspace-write are refused.
#[test]
fn arch05_codex_checks_refuse_only_what_toryo_sets() {
    let codex = |block: &str| {
        format!(
            "version: 1\nproject: {{ root: r, worktrees_root: w }}\nprofiles:\n  codex-foundry:\n    kind: codex\n    billing: foundry\n    exe: x\n    codex:\n      windows_sandbox: unelevated\n      provider: {{ id: foundry, name: Foundry, base_url: 'https://f.openai.azure.com/openai/v1', env_key: AZURE_OPENAI_API_KEY }}\n{block}roles:\n  planner: {{ profile: codex-foundry }}\n"
        )
    };
    for block in [
        "",
        "      overrides: { 'model_providers.foundry.query_params.api-version': '\"2025-04-01-preview\"', 'model_providers.foundry.request_max_retries': '4' }\n",
        "      overrides: { 'sandbox_workspace_write.network_access': 'true', 'sandbox_workspace_write.writable_roots': '[\"D:/cache\"]' }\n",
        "      sandbox_mode: read-only\n",
    ] {
        load(&codex(block)).unwrap_or_else(|e| panic!("{block}: {e:#}"));
    }
    for (block, want) in [
        (
            "      sandbox_mode: danger-full-access\n",
            "profiles.codex-foundry.codex.sandbox_mode: `danger-full-access` is not a valid value (valid: workspace-write, read-only)",
        ),
        (
            "      overrides: { sandbox_mode: '\"danger-full-access\"' }\n",
            "profiles.codex-foundry.codex.overrides.sandbox_mode: re-sets the sandbox",
        ),
        (
            "      overrides: { sandbox_permissions: '[\"disk-full-read-access\"]' }\n",
            "codex.overrides.sandbox_permissions: re-sets the sandbox",
        ),
        (
            "      overrides: { 'model_providers.foundry.base_url': '\"https://elsewhere\"' }\n",
            "codex.overrides.model_providers.foundry.base_url: re-sets the provider of codex.provider",
        ),
        (
            "      overrides: { model_provider: '\"openai\"' }\n",
            "codex.overrides.model_provider: re-sets the provider of codex.provider",
        ),
    ] {
        let e = format!("{:#}", load(&codex(block)).expect_err(block));
        assert!(e.contains(want), "{want:?} missing in:\n{e}");
    }
}

/// EC-28 (probe `expand_vars_parenthesised_names`, inverted): never left unexpanded.
#[test]
fn ec28_parenthesised_variable_names_are_expanded_or_refused() {
    match expand_vars("%ProgramFiles(x86)%\\Git\\bin\\git.exe") {
        Ok(s) => assert!(!s.contains('%'), "expanded: {s}"),
        Err(e) => assert!(
            format!("{e:#}").contains("%ProgramFiles(x86)% is not set"),
            "{e:#}"
        ),
    }
}

#[cfg(feature = "m0-kit")]
mod e2e {
    use super::common::*;
    use serde_json::{json, Value};
    use std::path::Path;
    use std::time::Duration;
    use toryo::engine::store::{self, AttemptStatus, Store};
    use toryo::engine::PipelineState as St;

    /// (exit code, stdout, stderr)
    fn toryo_out(env: &Env, args: &[&str]) -> (i32, String, String) {
        let o = command(env, args, &[]).output().unwrap();
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).to_string(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        )
    }

    fn tail(s: &str) -> &str {
        s.lines().last().unwrap_or("")
    }

    /// Run `id` until its implementer hangs after `ready()`, then kill the owner.
    fn kill_during_implement(env: &Env, id: &str, steps: Vec<Value>, ready: impl Fn() -> bool) {
        let s = script(
            env,
            "hang.json",
            json!({ "planner": [turn(vec![plan()])], "implementer": [turn(steps)] }),
        );
        let mut child = spawn(env, &["run", id, "--yes", "--fake-agents", &s], "hang.log");
        let (data, i) = (env.data.clone(), id.to_string());
        wait_until("the implementer to write and hang", 90, || {
            let st = Store::open(&data.join("toryo.db")).unwrap();
            let i = i.clone();
            let running = st
                .call(move |tx| store::last_attempt(tx, &i, "implement"))
                .ok()
                .flatten()
                .is_some_and(|a| a.status == AttemptStatus::Running);
            running && ready()
        });
        std::thread::sleep(Duration::from_millis(200));
        child.kill().unwrap();
        child.wait().unwrap();
    }

    fn contains(p: &Path, needle: &str) -> bool {
        std::fs::read_to_string(p).is_ok_and(|s| s.contains(needle))
    }

    /// Every file under `dir` whose bytes contain `needle`.
    fn files_with(dir: &Path, needle: &str, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                files_with(&p, needle, out);
            } else if std::fs::read(&p).is_ok_and(|b| String::from_utf8_lossy(&b).contains(needle))
            {
                out.push(p);
            }
        }
    }

    /// EC-12 (probe `e_reload_crash_bricks_the_task`, inverted): a crash between publishing the
    /// new versions and switching `tasks.config_sha256` leaves the old configuration in force;
    /// run / show / cleanup keep working and the reload can simply be repeated.
    #[test]
    fn ec12_reload_crash_leaves_the_old_config_in_force() {
        let env = setup("codex-openai");
        create(&env, "E-1");
        let s = script(
            &env,
            "e1.json",
            json!({
                "planner": [turn(vec![plan()])],
                "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
                "reviewer": [turn(vec![approved()])],
            }),
        );
        let (code, err) = toryo(&env, &["run", "E-1", "--fake-agents", &s], &[]);
        assert_eq!(code, 0, "{err}");
        let (code, err) = toryo(
            &env,
            &[
                "approve",
                "E-1",
                "--gate",
                "plan",
                "--continue",
                "--fake-agents",
                &s,
            ],
            &[],
        );
        assert_eq!(code, 0, "{err}");
        let before = task(&env, "E-1");
        assert_eq!(before.state, St::WaitingFinalApproval);
        let cfg = std::fs::read_to_string(&env.config).unwrap().replacen(
            "  verify:\n",
            "  verify:\n    - { name: extra, program: git, args: [\"status\"] }\n",
            1,
        );
        std::fs::write(&env.config, cfg).unwrap();
        let (code, err) = toryo(
            &env,
            &["task", "config", "E-1", "--reload"],
            &[(
                "TORYO_TEST_CRASH_AT",
                "artifact:verify-result@2:before_publish",
            )],
        );
        assert_eq!(code, 86, "precondition: the crash hook fired: {err}");
        let t = task(&env, "E-1");
        assert_eq!(
            t.config_sha256, before.config_sha256,
            "the hash never moved"
        );
        assert_eq!(t.state, St::WaitingFinalApproval);
        // the old configuration is still the task's: no artifact_tampered anywhere
        let (code, out, err) = toryo_out(&env, &["task", "config", "E-1"]);
        assert_eq!(code, 0, "{err}");
        assert!(!out.contains("\"extra\""), "the old config is shown: {out}");
        let (code, err) = toryo(&env, &["run", "E-1", "--fake-agents", &s], &[]);
        assert_eq!(code, 0, "run after the crash: {err}");
        assert!(!err.contains("artifact_tampered"), "{err}");
        assert_eq!(task(&env, "E-1").state, St::WaitingFinalApproval);
        // the reload is repeated and applies
        let (code, err) = toryo(&env, &["task", "config", "E-1", "--reload"], &[]);
        assert_eq!(code, 0, "{err}");
        let t = task(&env, "E-1");
        assert_ne!(t.config_sha256, before.config_sha256);
        assert_eq!(t.state, St::Verifying, "the final gate was withdrawn");
        let vr: Value = serde_json::from_slice(
            &std::fs::read(t.artifacts().join("VERIFY_RESULT.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(vr["invalidated"], json!(true), "{vr}");
        let (code, out, err) = toryo_out(&env, &["task", "config", "E-1"]);
        assert_eq!(code, 0, "{err}");
        assert!(
            out.contains("\"extra\""),
            "the new config is in force: {out}"
        );
        let (code, err) = toryo(&env, &["task", "cleanup", "E-1", "--force"], &[]);
        assert_eq!(code, 0, "cleanup: {}", tail(&err));
        let t = task(&env, "E-1");
        assert_eq!(t.state, St::Cancelled);
        assert!(!t.worktree.exists() && !t.run_store.exists());
    }

    /// EC-12: `task cleanup` works when no version of the frozen configuration can be read
    /// (everything else still refuses such a task).
    #[test]
    fn ec12_cleanup_works_without_a_readable_config() {
        let env = setup("codex-openai");
        create(&env, "C-1");
        let t = task(&env, "C-1");
        let mut tampered = vec![t.artifacts().join("config.json")];
        for e in std::fs::read_dir(t.artifacts().join("artifacts/config"))
            .unwrap()
            .flatten()
        {
            tampered.push(e.path());
        }
        assert!(tampered.len() >= 2, "{tampered:?}");
        for p in &tampered {
            std::fs::write(p, "{\"tampered\": true}").unwrap();
        }
        let s = script(&env, "c1.json", json!({ "planner": [turn(vec![plan()])] }));
        let (code, err) = toryo(&env, &["run", "C-1", "--fake-agents", &s], &[]);
        assert_eq!(code, 2, "{err}");
        assert!(err.contains("artifact_tampered"), "{err}");
        let (code, err) = toryo(&env, &["task", "cleanup", "C-1", "--force"], &[]);
        assert_eq!(code, 0, "{err}");
        assert!(
            err.contains("WARNING") && err.contains("artifact_tampered"),
            "{err}"
        );
        let t = task(&env, "C-1");
        assert_eq!(t.state, St::Cancelled);
        assert!(!t.worktree.exists(), "the worktree is removed");
        assert!(!t.run_store.exists(), "the run store is removed");
        assert!(
            !git(&env.repo, &["worktree", "list"]).contains("C-1"),
            "and unregistered"
        );
    }

    /// EC-19 (probe `d_discard_partial_loses_post_snapshot_edits`, inverted): the discard takes a
    /// new snapshot first, so what the human wrote after the recovery snapshot survives in it
    /// and `partial export` restores it.
    #[test]
    fn ec19_discard_partial_snapshots_the_worktree_first() {
        let env = setup("codex-openai");
        create(&env, "D-1");
        let w = wt(&env, "D-1");
        let (wt1, wt2) = (w.clone(), w.clone());
        kill_during_implement(
            &env,
            "D-1",
            vec![
                write("TASK.md", "- AC-9: sneaky\n"),
                write("src/Calculator.psm1", DIVIDE),
                hang(),
            ],
            move || {
                contains(&wt1.join("TASK.md"), "AC-9")
                    && contains(&wt2.join("src/Calculator.psm1"), "Divide-Numbers")
            },
        );
        let (_, err) = toryo(&env, &["resume", "D-1"], &[]);
        let t = task(&env, "D-1");
        assert_eq!(
            t.state,
            St::Escalated,
            "precondition (recovery escalates on the TASK.md edit): {err}"
        );
        // the human repairs the tree by hand and writes new work, then decides to discard
        git(&w, &["checkout", "--", "TASK.md"]);
        std::fs::create_dir_all(w.join("notes")).unwrap();
        std::fs::write(
            w.join("notes/human-design.md"),
            "HUMAN-WORK-7f3a: two hours of notes\n",
        )
        .unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(w.join("src/Calculator.psm1"))
            .and_then(|mut f| std::io::Write::write_all(&mut f, b"\n# HUMAN-EDIT-7f3a\n"))
            .unwrap();
        let s2 = script(
            &env,
            "d2.json",
            json!({
                "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
                "reviewer": [turn(vec![approved()])],
            }),
        );
        let (code, err) = toryo(
            &env,
            &[
                "resume",
                "D-1",
                "--discard-partial",
                "--yes",
                "--fake-agents",
                &s2,
            ],
            &[],
        );
        assert_eq!(code, 0, "{err}");
        assert!(err.contains("discarded"), "{err}");
        assert_eq!(task(&env, "D-1").state, St::Completed, "{err}");
        assert!(
            !w.join("notes/human-design.md").exists(),
            "the discard reset the tree"
        );
        let snaps = t.run_store.join("artifacts/partial/implement-1");
        let versions = std::fs::read_dir(&snaps)
            .unwrap()
            .flatten()
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .count();
        assert_eq!(versions, 2, "a new snapshot was taken before the discard");
        let mut found = Vec::new();
        files_with(&snaps, "7f3a", &mut found);
        assert!(
            found.iter().any(|p| p.ends_with("notes/human-design.md"))
                && found.iter().any(|p| p.ends_with("src/Calculator.psm1")),
            "the snapshot holds the human's bytes: {found:?}"
        );
        // `partial export` restores what was discarded
        let into = env.root.join("exported");
        let (code, err) = toryo(
            &env,
            &[
                "partial",
                "export",
                "D-1",
                "--attempt",
                "1",
                "--stage",
                "implement",
                "--into",
                into.to_str().unwrap(),
            ],
            &[],
        );
        assert_eq!(code, 0, "{err}");
        assert!(contains(
            &into.join("notes/human-design.md"),
            "HUMAN-WORK-7f3a"
        ));
        assert!(contains(
            &into.join("src/Calculator.psm1"),
            "HUMAN-EDIT-7f3a"
        ));
    }

    /// EC-23 end to end: the frozen chain of a task follows `fallback: []`; a typo'd role key
    /// is refused by `task create` and reported by `doctor`.
    #[test]
    fn ec23_task_create_honours_empty_fallback_and_refuses_typos() {
        let env = setup("codex-openai");
        create(&env, "F-0");
        assert_eq!(
            task(&env, "F-0").implementers,
            vec!["codex-openai", "claude-max"],
            "absent fallback: the planner's profile, as before"
        );
        let text = std::fs::read_to_string(&env.config).unwrap();
        let empty = text.replace(
            "implementer: { profile: codex-openai }",
            "implementer: { profile: codex-openai, fallback: [] }",
        );
        assert_ne!(empty, text);
        std::fs::write(&env.config, &empty).unwrap();
        create(&env, "F-1");
        assert_eq!(task(&env, "F-1").implementers, vec!["codex-openai"]);
        std::fs::write(
            &env.config,
            empty.replace(
                "  planner: { profile: claude-max }\n",
                "  planner: { profile: claude-max }\n  reviwer: { profile: codex-openai }\n",
            ),
        )
        .unwrap();
        let (code, err) = toryo(
            &env,
            &["task", "create", "--file", "TASK.md", "--id", "F-2"],
            &[],
        );
        assert_eq!(code, 2, "{err}");
        assert!(err.contains("roles.reviwer: unknown role"), "{err}");
        let (code, out, _) = toryo_out(&env, &["doctor"]);
        assert_eq!(code, 1, "{out}");
        assert!(
            out.lines()
                .any(|l| l.starts_with("[FAIL] config") && l.contains("invalid configuration")),
            "{out}"
        );
    }

    /// CODE-09: a command that fails after taking the task lock (here `task config --reload` in
    /// a non-waiting state) leaves no owner behind in the store.
    #[test]
    fn code09_an_early_error_clears_the_lock_owner() {
        let env = setup("codex-openai");
        create(&env, "L-1");
        let (code, err) = toryo(&env, &["task", "config", "L-1", "--reload"], &[]);
        assert_eq!(code, 2, "{err}");
        assert!(err.contains("only in a waiting state"), "{err}");
        let t = task(&env, "L-1");
        assert_eq!(t.lock_generation, 1, "the lock was taken");
        assert_eq!(t.run_lock_owner, None, "and its owner cleared");
        assert_eq!(t.run_lock_at, None);
    }
}
