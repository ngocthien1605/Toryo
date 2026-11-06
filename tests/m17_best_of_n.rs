//! M17 Part B — best-of-N implementer candidates (`docs/tasks/M17/TASK.md`): the first
//! Implementing stage of a task whose implementer seat lists `candidates` fans out into 2..=4
//! candidates, each in its own worktree; the check gates decide first, then one read-only compare
//! turn of the reviewer seat, then the deterministic score; the winner becomes the task's
//! checkpoint and goes through the normal flow. End to end through the `toryo` binary with fake
//! agents only.
#![cfg(feature = "m0-kit")]

mod common;

use common::m17::*;
use common::*;
use serde_json::{json, Value};
use toryo::engine::PipelineState as St;

// ── B1: configuration (AC-12, AC-13) ──────────────────────────────────────────────────────

/// `toryo task create` of the fixture's TASK.md as `id` → (exit code, stderr).
fn create_out(env: &Env, id: &str) -> (i32, String) {
    toryo(
        env,
        &["task", "create", "--file", "TASK.md", "--id", id],
        &[],
    )
}

/// The org chart of the M17 fixture with `planner`, `implementer` and `reviewer` seat lines.
fn org_seats(planner: &str, implementer: &str, reviewer: &str) -> String {
    format!(
        "toryo: 1\nteam: calc-team\ngates:\n  divide-present: {{ kind: check, program: git, args: [grep, -q, Divide-Numbers, --, src], timeout: 5m }}\nseats:\n  planner: {{ {planner} }}\n  implementer: {{ {implementer} }}\n  reviewer: {{ {reviewer} }}\n"
    )
}

/// AC-12: `seats.implementer.candidates` takes 2..=4 machine profiles (a profile may repeat); it
/// is frozen with the task and shown by `config show` (text and JSON), `task config` and `doctor`.
#[test]
fn m17_ac12_candidates_on_the_implementer_seat_are_frozen_and_shown() {
    let env = setup_bon("[codex-openai, claude-max, codex-openai]", "", &[]);
    let (code, out, err) = run_out(&env, &["config", "show"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("candidates (best-of-N, first Implementing stage): c1 codex-openai, c2 claude-max, c3 codex-openai"),
        "{out}"
    );
    let (code, out, err) = run_out(&env, &["config", "show", "--json"]);
    assert_eq!(code, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    let seat = |name: &str| {
        v["seats"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["seat"] == name)
            .cloned()
            .unwrap()
    };
    assert_eq!(
        seat("implementer")["candidates"],
        json!(["codex-openai", "claude-max", "codex-openai"])
    );
    assert!(seat("reviewer").get("candidates").is_none(), "{v}");
    let (code, err) = create_out(&env, "CF-1");
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = run_out(&env, &["task", "config", "CF-1"]);
    assert_eq!(code, 0, "{err}");
    let frozen: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        frozen["repo"]["roles"]["implementer"]["candidates"],
        json!(["codex-openai", "claude-max", "codex-openai"]),
        "{out}"
    );
    let o = command(&env, &["doctor"], &[]).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains(
            "implementer → codex-openai · candidates codex-openai, claude-max, codex-openai"
        ),
        "{out}"
    );
}

/// AC-12: `candidates` is refused on the planner and reviewer seats, with fewer than 2 or more
/// than 4 entries, and with an unknown profile — each by its key, and no task is created.
#[test]
fn m17_ac12_candidates_are_refused_off_the_implementer_with_a_bad_count_or_an_unknown_profile() {
    let base = "profile: codex-openai";
    let cases: [(String, &str); 5] = [
        (
            org_seats("profile: claude-max, candidates: [claude-max, codex-openai]", base, "profile: claude-max"),
            "seats.planner.candidates: only the implementer seat may have candidates — the planner runs one turn; remove it",
        ),
        (
            org_seats("profile: claude-max", base, "profile: claude-max, candidates: [claude-max, claude-max]"),
            "seats.reviewer.candidates: only the implementer seat may have candidates — the reviewer runs one turn; remove it",
        ),
        (
            org_seats("profile: claude-max", "profile: codex-openai, candidates: [codex-openai]", "profile: claude-max"),
            "seats.implementer.candidates: 1 profile(s) — best-of-N takes 2 to 4 candidates (a profile may repeat)",
        ),
        (
            org_seats("profile: claude-max", "profile: codex-openai, candidates: [codex-openai, codex-openai, claude-max, claude-max, codex-openai]", "profile: claude-max"),
            "seats.implementer.candidates: 5 profile(s) — best-of-N takes 2 to 4 candidates",
        ),
        (
            org_seats("profile: claude-max", "profile: codex-openai, candidates: [codex-openai, codex-nope]", "profile: claude-max"),
            "seats.implementer.candidates: profile 'codex-nope' is not defined in the machine config",
        ),
    ];
    for (n, (org, want)) in cases.iter().enumerate() {
        let env = setup_orgchart("codex-openai", "", &[(".toryo/toryo-orgchart.yaml", org)]);
        let id = format!("CR-{n}");
        let (code, err) = create_out(&env, &id);
        assert_ne!(code, 0, "case {n}: {err}");
        assert!(err.contains(want), "case {n}: `{want}` not in {err}");
        let i = id.clone();
        assert!(
            !db(&env)
                .call(move |tx| Ok(toryo::engine::store::get_task(tx, &i).is_ok()))
                .unwrap(),
            "case {n}: no task"
        );
    }
}

/// AC-12 in toryo.yaml mode: `roles.implementer.candidates` is accepted and frozen; on another
/// role it is refused by its `roles.` key.
#[test]
fn m17_ac12_candidates_in_toryo_yaml_mode() {
    let env = setup_with("codex-openai", "");
    let yaml = std::fs::read_to_string(&env.config).unwrap();
    let with = |from: &str, to: &str| {
        assert!(yaml.contains(from), "{yaml}");
        std::fs::write(&env.config, yaml.replace(from, to)).unwrap();
    };
    with(
        "implementer: { profile: codex-openai }",
        "implementer: { profile: codex-openai, candidates: [codex-openai, claude-max] }",
    );
    let (code, err) = create_out(&env, "TY-1");
    assert_eq!(code, 0, "{err}");
    let (_, out, _) = run_out(&env, &["task", "config", "TY-1"]);
    let frozen: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        frozen["repo"]["roles"]["implementer"]["candidates"],
        json!(["codex-openai", "claude-max"])
    );
    with(
        "planner: { profile: claude-max }",
        "planner: { profile: claude-max, candidates: [claude-max, claude-max] }",
    );
    let (code, err) = create_out(&env, "TY-2");
    assert_ne!(code, 0, "{err}");
    assert!(
        err.contains("roles.planner.candidates: only the implementer seat may have candidates"),
        "{err}"
    );
}

/// AC-13: without `candidates` the configuration keeps its shape — no `candidates` key in
/// `config show --json` or in the frozen configuration (so its digest is the one before M17).
#[test]
fn m17_ac13_a_configuration_without_candidates_keeps_its_shape() {
    let env = setup_rg("", "", &[]);
    let (code, out, err) = run_out(&env, &["config", "show", "--json"]);
    assert_eq!(code, 0, "{err}");
    assert!(!out.contains("candidates"), "{out}");
    let (code, err) = create_out(&env, "NC-1");
    assert_eq!(code, 0, "{err}");
    let (_, out, _) = run_out(&env, &["task", "config", "NC-1"]);
    assert!(!out.contains("candidates"), "{out}");
}

/// AC-12: the R51 evidence check of a drive covers every candidate profile — a candidate profile
/// that would run its real CLI without isolation evidence stops the run before anything starts.
#[test]
fn m17_ac12_the_r51_evidence_check_covers_every_candidate_profile() {
    let env = setup_bon("[codex-openai, codex-b]", "", &[]);
    std::fs::write(
        &env.machine,
        format!("{MACHINE_PROFILES}  codex-b: {{ kind: codex, billing: subscription, exe: codex-not-installed }}\n{MACHINE_FLOOR}"),
    )
    .unwrap();
    create(&env, "EV-1");
    // only the planner, the primary and the reviewer are fake: `codex-b` would run its real CLI
    let p = env.root.join("ev.json");
    std::fs::write(
        &p,
        serde_json::to_vec(&json!({
            "turns": {"planner": [turn(vec![plan()])]},
            "only_profiles": ["claude-max", "codex-openai"],
        }))
        .unwrap(),
    )
    .unwrap();
    let (code, err) = toryo(
        &env,
        &["run", "EV-1", "--yes", "--fake-agents", p.to_str().unwrap()],
        &[],
    );
    assert_ne!(code, 0, "{err}");
    assert!(err.contains("unverified profiles (R51):"), "{err}");
    assert!(err.contains("  - codex-b: "), "{err}");
    assert_eq!(task(&env, "EV-1").state, St::New, "nothing ran: {err}");
}

/// Edge case 35 (before the fan-out): `task config --reload` may change `candidates` while the
/// fan-out has not started — the new list is frozen.
#[test]
fn m17_ec35_reload_changes_candidates_before_the_fan_out() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "RL-1");
    let s = script(&env, "rl.json", json!({"planner": [turn(vec![plan()])]}));
    // up to the plan gate: no fan-out yet
    let (code, err) = toryo(&env, &["run", "RL-1", "--fake-agents", &s], &[]);
    assert_eq!(
        task(&env, "RL-1").state,
        St::WaitingPlanApproval,
        "{code} {err}"
    );
    let org = org_rg("", ", candidates: [codex-openai, claude-max]");
    commit_repo(&env, &[(".toryo/toryo-orgchart.yaml", &org)], "two vendors");
    let (code, err) = toryo(&env, &["task", "config", "RL-1", "--reload"], &[]);
    assert_eq!(code, 0, "{err}");
    let (_, out, _) = run_out(&env, &["task", "config", "RL-1"]);
    let frozen: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        frozen["repo"]["roles"]["implementer"]["candidates"],
        json!(["codex-openai", "claude-max"]),
        "{out}"
    );
}

// ── B2: the store (AC-26) ──────────────────────────────────────────────────────────────────

/// Edge case 40, AC-26: a version-3 database with an existing task opens and is raised to 4; the
/// task (no candidates) finishes on the old path.
#[test]
fn m17_ec40_a_version_3_database_with_a_task_is_raised_and_the_task_finishes() {
    let env = setup_rg("", "", &[]);
    create(&env, "V3-1");
    let p = env.data.join("toryo.db");
    rusqlite::Connection::open(&p)
        .unwrap()
        .execute_batch(
            "DROP TABLE candidates; DROP TABLE fanouts; UPDATE meta SET value='3' WHERE key='schema_version';",
        )
        .unwrap();
    let s = script(
        &env,
        "v3.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "V3-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "V3-1").state, St::Completed, "{err}");
    let v: String = rusqlite::Connection::open(&p)
        .unwrap()
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(v, "4");
    assert!(fanout(&env, "V3-1").is_none(), "the old path: no fan-out");
}

// ── B3: the fake agent per candidate ───────────────────────────────────────────────────────

/// AC-15 (the fake agent): a candidate's turn is scripted by `role@profile#c<k>` first, then
/// `role#c<k>`, then the profile's and the role's script.
#[test]
fn m17_b3_the_fake_agent_addresses_a_candidate_turn_by_its_label() {
    use toryo::agents::fake::candidate_of;
    assert_eq!(candidate_of("implement#c2-1"), Some(2));
    assert_eq!(candidate_of("implement#c12-3"), Some(12));
    for none in [
        "implement-1",
        "fix-1-2",
        "select-1-0",
        "implement#c0-1",
        "implement#c-1",
    ] {
        assert_eq!(candidate_of(none), None, "{none}");
    }
}

// ── B4–B9: the whole flow ──────────────────────────────────────────────────────────────────

/// AC-14, AC-15, AC-18, AC-19, AC-22, edge case 21: two candidates of the same profile get their
/// own scripts and sessions, both checkpoint and pass, the compare turn picks c2, and `toryo/<ID>`
/// is moved to exactly c2's checkpoint, which the normal flow verifies, reviews and completes.
#[test]
fn m17_ec21_two_candidates_of_one_profile_compare_and_promote() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "BN-1");
    let s = script(
        &env,
        "bn.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer#c1": [candidate_turn("# one\n")],
            "implementer#c2": [candidate_turn("# two\n")],
            "reviewer": [turn(vec![select("c2", &["c2", "c1"])]), turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "BN-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "BN-1").state, St::Completed, "{err}");
    let f = fanout(&env, "BN-1").expect("a fan-out");
    assert_eq!(
        (f.phase.as_str(), f.winner, f.by.as_deref()),
        ("promoted", Some(2), Some("compare")),
        "{f:?}"
    );
    let c = candidates(&env, "BN-1");
    assert_eq!(c.len(), 2);
    assert!(
        c.iter()
            .all(|c| c.outcome.as_deref() == Some("checkpointed") && c.verify_pass == Some(true)),
        "{c:?}"
    );
    let head = git(&wt(&env, "BN-1"), &["rev-parse", "HEAD"]);
    assert_eq!(
        head.trim(),
        c[1].head.as_deref().unwrap(),
        "the task branch is exactly c2's checkpoint"
    );
    let msg = git(&wt(&env, "BN-1"), &["log", "-1", "--format=%B"]);
    assert!(msg.contains("Toryo-Candidate: c2"), "{msg}");
    // both candidates' branches are kept; their folders are gone
    for k in 1..=2 {
        assert!(!cwt(&env, "BN-1", k).exists(), "c{k} folder removed");
        git(
            &env.repo,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/toryo-candidate/BN-1/c{k}"),
            ],
        );
    }
    // the same profile, two scripts, two sessions
    let calls = calls(&env, "BN-1");
    let keys: Vec<&str> = calls.iter().filter_map(|c| c["key"].as_str()).collect();
    assert!(
        keys.contains(&"implementer#c1") && keys.contains(&"implementer#c2"),
        "{keys:?}"
    );
    let a = attempts(&env, "BN-1");
    let sessions: Vec<Option<String>> = a
        .iter()
        .filter(|a| a.stage.starts_with("implement#c"))
        .map(|a| a.external_session_id.clone())
        .collect();
    assert_eq!(sessions.len(), 2, "{a:?}");
    assert_ne!(sessions[0], sessions[1]);
}

/// The candidates' outcomes of task `id`, by `k`.
fn outcomes(env: &Env, id: &str) -> Vec<String> {
    candidates(env, id)
        .into_iter()
        .map(|c| c.outcome.unwrap_or_else(|| format!("({})", c.state)))
        .collect()
}

/// The attempts of task `id` in `stage` with their profiles.
fn stage_profiles(env: &Env, id: &str, stage: &str) -> Vec<String> {
    attempts(env, id)
        .into_iter()
        .filter(|a| a.stage == stage)
        .map(|a| a.profile.unwrap_or_default())
        .collect()
}

/// A run of the fixture task `id` with `turns` (planner and the approving review added unless
/// given) → (exit code, stderr).
fn run_with(env: &Env, id: &str, turns: Value, extra: &[(&str, &str)]) -> (i32, String) {
    let mut t = turns;
    if t.get("planner").is_none() {
        t["planner"] = json!([turn(vec![plan()])]);
    }
    let s = script(env, &format!("{id}.json"), t);
    toryo(env, &["run", id, "--yes", "--fake-agents", &s], extra)
}

/// Edge case 22 (three tests), AC-16: one candidate's turn fails — quota, a crash, invalid output
/// — and the other passes: the passing one wins `by: only`; the failed candidate never switches to
/// the fallback chain.
fn one_candidate_fails(id: &str, failure: Value, want: &str) {
    let env = setup_bon(
        "[codex-openai, codex-openai], fallback: [claude-max]",
        "",
        &[],
    );
    create(&env, id);
    let (code, err) = run_with(
        &env,
        id,
        json!({
            "implementer#c1": [turn(vec![write("src/Calculator.psm1", "# half\n"), failure])],
            "implementer#c2": [candidate_turn("")],
            "reviewer": [turn(vec![approved()])],
        }),
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, id).state, St::Completed, "{err}");
    assert_eq!(
        outcomes(&env, id),
        vec!["failed_turn", "checkpointed"],
        "{err}"
    );
    let c = candidates(&env, id);
    assert!(
        c[0].detail.as_deref().unwrap_or_default().contains(want),
        "{:?}",
        c[0]
    );
    let f = fanout(&env, id).unwrap();
    assert_eq!((f.winner, f.by.as_deref()), (Some(2), Some("only")));
    // no fallback for a candidate: claude-max never implemented
    assert_eq!(
        stage_profiles(&env, id, "implement#c1"),
        vec!["codex-openai"]
    );
    assert!(
        attempts(&env, id)
            .iter()
            .all(|a| !(a.stage.starts_with("implement")
                && a.profile.as_deref() == Some("claude-max")))
    );
    assert!(!err.contains("switching to"), "{err}");
}

#[test]
fn m17_ec22_a_candidate_out_of_quota_loses_without_a_fallback() {
    one_candidate_fails(
        "Q-1",
        fail("QuotaExceeded", "weekly limit"),
        "QuotaExceeded",
    );
}

#[test]
fn m17_ec22_a_crashed_candidate_loses_without_a_fallback() {
    one_candidate_fails("Q-2", json!({"kind":"crash"}), "ProcessCrashed");
}

#[test]
fn m17_ec22_a_candidate_with_invalid_output_loses_without_a_fallback() {
    one_candidate_fails(
        "Q-3",
        fail("InvalidOutput", "not the schema"),
        "InvalidOutput",
    );
}

/// Edge case 23, AC-21: no candidate changes anything — the single-implementer path runs once,
/// with a note, and there is no second fan-out.
#[test]
fn m17_ec23_no_candidate_changes_anything_the_single_path_runs_once() {
    let env = setup_bon("[codex-openai, claude-max]", "", &[]);
    create(&env, "NO-1");
    let (code, err) = run_with(
        &env,
        "NO-1",
        json!({
            "implementer#c1": [turn(vec![implementation()])],
            "implementer#c2": [turn(vec![implementation()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "NO-1").state, St::Completed, "{err}");
    assert_eq!(outcomes(&env, "NO-1"), vec!["no_change", "no_change"]);
    assert_eq!(fanout(&env, "NO-1").unwrap().phase, "single");
    assert!(
        err.contains("note: best-of-N: no candidate made a checkpoint (c1 no_change, c2 no_change) — the single implementer path runs (the primary and its fallback chain), once"),
        "{err}"
    );
    assert_eq!(
        stage_profiles(&env, "NO-1", "implement"),
        vec!["codex-openai"]
    );
    assert_eq!(
        err.matches("best-of-N: 2 candidates").count(),
        1,
        "one fan-out: {err}"
    );
    for k in 1..=2 {
        assert!(!cwt(&env, "NO-1", k).exists());
    }
}

/// Edge case 24, AC-21, AC-20 (rung 3): every candidate fails the check gates — the best by the
/// score (fewer changed lines) wins and the task enters Fixing with its verify feedback as one gate
/// retry; `status` shows the rung; the fix cycle passes.
#[test]
fn m17_ec24_all_candidates_fail_the_gates_the_best_by_score_enters_fixing() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "AF-1");
    let s = script(
        &env,
        "af.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer#c1": [turn(vec![write("src/Calculator.psm1", "# a\n# b\n# c\n"), implementation()])],
            "implementer#c2": [turn(vec![write("src/Calculator.psm1", "# a\n"), implementation()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    // stop at the first owner decision after the fan-out: no --yes for the final gate
    let (code, err) = toryo(&env, &["run", "AF-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "AF-1");
    assert_eq!(t.state, St::Completed, "{err}");
    let f = fanout(&env, "AF-1").unwrap();
    assert_eq!(
        (f.winner, f.by.as_deref()),
        (Some(2), Some("score")),
        "{f:?}"
    );
    assert_eq!(
        f.reason.as_deref(),
        Some("score: c2 before c1 — fewer changed lines (1 vs 3)")
    );
    assert!(
        err.contains("[state] Verifying --verify_failed--> Fixing")
            || err.contains("--verify_failed--> Fixing"),
        "{err}"
    );
    // one gate retry: the fix cycle ran on the winner's profile, from c2's checkpoint
    assert_eq!(t.fix_cycle, 1);
    assert_eq!(stage_profiles(&env, "AF-1", "fix-1"), vec!["codex-openai"]);
    let out = status_out(&env, "AF-1");
    assert!(
        out.contains("winner c2 (codex-openai) by score — score: c2 before c1 — fewer changed lines (1 vs 3)"),
        "{out}"
    );
}

/// Edge case 25, AC-20 (rung 5): two candidates equal on every rung — the lower index wins.
#[test]
fn m17_ec25_an_exact_tie_goes_to_the_lower_index() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "TIE-1");
    let (code, err) = run_with(
        &env,
        "TIE-1",
        json!({
            "implementer#c1": [turn(vec![write("src/Calculator.psm1", "# same\n"), implementation()])],
            "implementer#c2": [turn(vec![write("src/Calculator.psm1", "# same\n"), implementation()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let f = fanout(&env, "TIE-1").unwrap();
    assert_eq!(
        (f.winner, f.by.as_deref()),
        (Some(1), Some("score")),
        "{err}"
    );
    assert_eq!(
        f.reason.as_deref(),
        Some("score: c1 before c2 — lower candidate index (c1 vs c2)")
    );
}

// ── B8: the compare turn (AC-19) ───────────────────────────────────────────────────────────

/// Two (or three) passing candidates whose compare turn answers with `answers` (each one turn of
/// the reviewer, then the approving review): the fan-out row after the run.
fn compare_with(id: &str, n: usize, answers: Vec<Value>) -> (Env, String, store_row::Fan) {
    let profiles = vec!["codex-openai"; n].join(", ");
    let env = setup_bon(&format!("[{profiles}]"), "", &[]);
    create(&env, id);
    let mut turns = json!({
        "implementer#c1": [candidate_turn("# one\n")],
        "implementer#c2": [candidate_turn("# two two\n# two\n")],
    });
    if n == 3 {
        // c3 fails the gate
        turns["implementer#c3"] = json!([turn(vec![
            write("src/Calculator.psm1", "# three\n"),
            implementation()
        ])]);
    }
    let mut reviewer: Vec<Value> = answers.into_iter().map(|a| turn(vec![a])).collect();
    reviewer.push(turn(vec![approved()]));
    turns["reviewer"] = json!(reviewer);
    let (code, err) = run_with(&env, id, turns, &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, id).state, St::Completed, "{err}");
    let f = fanout(&env, id).unwrap();
    (env, err, f)
}

mod store_row {
    pub type Fan = toryo::engine::store::FanoutRow;
}

/// The prompts of the compare turn(s) of `id`.
fn select_prompts(env: &Env, id: &str) -> Vec<String> {
    calls(env, id)
        .into_iter()
        .filter(|c| {
            c["label"]
                .as_str()
                .is_some_and(|l| l.starts_with("select-"))
        })
        .map(|c| c["prompt"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// AC-19: the compare turn gets every passing candidate's DIFF.patch, IMPLEMENTATION.json and
/// verify summary as untrusted data — and not the failing one's.
#[test]
fn m17_ac19_the_compare_turn_gets_the_passing_candidates_as_data() {
    let (env, _, f) = compare_with("CMP-1", 3, vec![select("c1", &["c1", "c2"])]);
    assert_eq!((f.winner, f.by.as_deref()), (Some(1), Some("compare")));
    let p = select_prompts(&env, "CMP-1");
    assert_eq!(p.len(), 1);
    for want in [
        "Compare the implementations of the candidates c1, c2",
        "c1 DIFF.patch",
        "c2 IMPLEMENTATION.json",
        "c2 verify summary",
        "every check gate passed",
        "+# two two",
        "untrusted",
    ] {
        assert!(p[0].contains(want), "`{want}` not in {}", p[0]);
    }
    assert!(
        !p[0].contains("c3 DIFF.patch") && !p[0].contains("# three"),
        "{}",
        p[0]
    );
    let c = candidates(&env, "CMP-1");
    assert_eq!(c[2].verify_pass, Some(false));
}

/// Edge case 26: an answer naming an unknown id gets one correction turn; a correction naming the
/// failing candidate is not valid either, and the score decides (`by: score`).
#[test]
fn m17_ec26_an_unknown_id_then_a_failing_candidate_fall_to_the_score() {
    let (env, err, f) = compare_with(
        "CMP-2",
        3,
        vec![select("c9", &["c9", "c1"]), select("c3", &["c3", "c1"])],
    );
    assert_eq!(f.by.as_deref(), Some("score"), "{err}");
    // c1 changed fewer lines than c2
    assert_eq!(f.winner, Some(1));
    assert_eq!(
        f.reason.as_deref(),
        Some("the compare turn's answer was not valid after one correction; score: c1 before c2 — fewer changed lines (3 vs 4)")
    );
    let p = select_prompts(&env, "CMP-2");
    assert_eq!(p.len(), 2, "one correction turn");
    assert!(
        p[1].contains("`winner` `c9` is not one of the candidates to choose from (c1, c2)"),
        "{}",
        p[1]
    );
    let a: Vec<_> = attempts(&env, "CMP-2")
        .into_iter()
        .filter(|a| a.stage == "select")
        .collect();
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].correction_turns, 1);
    assert!(
        a[0].error_detail
            .as_deref()
            .unwrap_or_default()
            .contains("`c3` is not one of the candidates"),
        "{:?}",
        a[0]
    );
}

/// Edge case 26: malformed answers (a missing `ranking`, then no structured answer at all) fall to
/// the score after one correction; a valid correction is used (`by: compare`).
#[test]
fn m17_ec26_malformed_answers_fall_to_the_score_a_valid_correction_is_used() {
    let malformed = json!({"kind":"complete","structured":{"winner":"c2","reason":"r"}});
    let (_, err, f) = compare_with(
        "CMP-3",
        2,
        vec![malformed.clone(), json!({"kind":"complete","text":"c2"})],
    );
    assert_eq!(
        (f.winner, f.by.as_deref()),
        (Some(1), Some("score")),
        "{err}"
    );
    let (_, err, f) = compare_with("CMP-4", 2, vec![malformed, select("c2", &["c2", "c1"])]);
    assert_eq!(
        (f.winner, f.by.as_deref()),
        (Some(2), Some("compare")),
        "{err}"
    );
}

/// Edge case 26: a compare turn that fails outright falls to the score.
#[test]
fn m17_ec26_a_failed_compare_turn_falls_to_the_score() {
    let (_, err, f) = compare_with("CMP-5", 2, vec![fail("Overloaded", "busy")]);
    assert_eq!(
        (f.winner, f.by.as_deref()),
        (Some(1), Some("score")),
        "{err}"
    );
    assert!(
        f.reason
            .as_deref()
            .unwrap_or_default()
            .starts_with("the compare turn failed (Overloaded); score: c1 before c2"),
        "{f:?}"
    );
}

/// Edge case 26, AC-19: what the read-only compare turn writes is put back, and its answer is not
/// used (`by: score`).
#[test]
fn m17_ec26_a_compare_turn_that_writes_is_put_back() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "CMP-6");
    let (code, err) = run_with(
        &env,
        "CMP-6",
        json!({
            "implementer#c1": [candidate_turn("# one\n")],
            "implementer#c2": [candidate_turn("# two two\n# two\n")],
            "reviewer": [
                turn(vec![write("NOTES.md", "the reviewer wrote\n"), select("c2", &["c2", "c1"])]),
                turn(vec![approved()]),
            ],
        }),
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "CMP-6").state, St::Completed, "{err}");
    let f = fanout(&env, "CMP-6").unwrap();
    assert_eq!(
        (f.winner, f.by.as_deref()),
        (Some(1), Some("score")),
        "{err}"
    );
    assert!(
        f.reason.as_deref().unwrap_or_default().starts_with(
            "the compare turn wrote into the worktree (put back, its answer not used)"
        ),
        "{f:?}"
    );
    assert!(!wt(&env, "CMP-6").join("NOTES.md").exists(), "put back");
    assert!(git(&wt(&env, "CMP-6"), &["log", "--format=%B"]).contains("Toryo-Candidate: c1"));
}

/// Edge case 27, AC-22: the compare turn picks c2 and the later full review asks for changes —
/// the fix cycle runs on c2's profile, in a fresh session (X55).
#[test]
fn m17_ec27_the_fix_cycle_after_the_compare_runs_on_the_winners_profile() {
    let env = setup_bon("[codex-openai, claude-max]", "", &[]);
    create(&env, "FX-1");
    let (code, err) = run_with(
        &env,
        "FX-1",
        json!({
            "implementer#c1": [candidate_turn("# one\n")],
            "implementer#c2": [candidate_turn("# two\n")],
            "implementer@claude-max": [turn(vec![write("src/Calculator.psm1", "# fixed\n"), implementation()])],
            "reviewer": [turn(vec![select("c2", &["c2", "c1"])]), turn(vec![changes()]), turn(vec![approved()])],
        }),
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "FX-1");
    assert_eq!(t.state, St::Completed, "{err}");
    assert_eq!(
        t.implementers.first().map(String::as_str),
        Some("claude-max")
    );
    assert_eq!(stage_profiles(&env, "FX-1", "fix-1"), vec!["claude-max"]);
    let fix: Vec<Value> = calls(&env, "FX-1")
        .into_iter()
        .filter(|c| c["label"].as_str().is_some_and(|l| l.starts_with("fix-1")))
        .collect();
    assert_eq!(fix.len(), 1);
    assert!(
        fix[0]["resume"].is_null(),
        "a fresh session (X55): {}",
        fix[0]
    );
}

// ── B6: the post-turn path and the escalation policy (AC-16, AC-17) ────────────────────────

/// Edge case 28, AC-17: a candidate writes a protected file — the task escalates
/// `protected_config_changed` and the other candidate's running turn is stopped (`agent_killed`).
#[test]
fn m17_ec28_a_protected_write_escalates_and_stops_the_other_turn() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "PR-1");
    let (code, err) = run_with(
        &env,
        "PR-1",
        json!({
            "implementer#c1": [turn(vec![
                write("src/Calculator.psm1", DIVIDE),
                write(".claude/settings.local.json", "{}\n"),
                implementation(),
            ])],
            "implementer#c2": [turn(vec![hang()])],
        }),
        &[],
    );
    let t = task(&env, "PR-1");
    assert_eq!(t.state, St::Escalated, "{code} {err}");
    let why = t.machine.reason().unwrap_or_default().to_string();
    assert!(why.starts_with("protected_config_changed"), "{why}");
    assert!(
        why.contains(".claude/settings.local.json") && why.contains("candidate c1"),
        "{why}"
    );
    let ev = events(&env, "PR-1");
    assert!(
        ev.iter()
            .any(|e| e["type"] == "agent_killed" && e["stage"] == "implement#c2"),
        "{ev:?}"
    );
    let a: Vec<_> = attempts(&env, "PR-1")
        .into_iter()
        .filter(|a| a.stage == "implement#c2")
        .collect();
    assert_eq!(
        a[0].status,
        toryo::engine::store::AttemptStatus::Interrupted
    );
}

/// Edge case 29, AC-17: a candidate writes outside the implementer's write fence — only that
/// candidate is disqualified; the other wins.
#[test]
fn m17_ec29_a_fence_violation_disqualifies_only_that_candidate() {
    let role =
        "toryo: 1\npurpose: Change the code.\nwrites: [src/**]\nbefore_handoff: [divide-present]\n";
    let env = setup_bon(
        "[codex-openai, codex-openai]",
        "",
        &[(".toryo/roles/implementer/toryo-role.yaml", role)],
    );
    create(&env, "FN-1");
    let (code, err) = run_with(
        &env,
        "FN-1",
        json!({
            "implementer#c1": [turn(vec![write("src/Calculator.psm1", DIVIDE), write("docs/x.md", "x\n"), implementation()])],
            "implementer#c2": [candidate_turn("")],
            "reviewer": [turn(vec![approved()])],
        }),
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "FN-1").state, St::Completed, "{err}");
    assert_eq!(
        outcomes(&env, "FN-1"),
        vec!["disqualified:policy_violation", "checkpointed"]
    );
    let f = fanout(&env, "FN-1").unwrap();
    assert_eq!((f.winner, f.by.as_deref()), (Some(2), Some("only")));
    let c = candidates(&env, "FN-1");
    assert!(
        c[0].detail
            .as_deref()
            .unwrap_or_default()
            .contains("docs/x.md"),
        "{:?}",
        c[0]
    );
    let out = status_out(&env, "FN-1");
    assert!(
        out.contains("c1 codex-openai disqualified(policy_violation)"),
        "{out}"
    );
}

// ── B5: the turns run at the same time (AC-15) ─────────────────────────────────────────────

/// AC-15: the candidates' turns overlap — each waits for a file the other writes (one after the
/// other, the first would wait in vain and fail).
#[test]
fn m17_ac15_candidate_turns_run_at_the_same_time() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "OV-1");
    let flag = |n: &str| env.root.join(n).display().to_string();
    let (code, err) = run_with(
        &env,
        "OV-1",
        json!({
            "implementer#c1": [turn(vec![
                write(&flag("c1-up"), "1"),
                json!({"kind":"wait_for","path": flag("c2-up"), "ms": 20000}),
                write("src/Calculator.psm1", DIVIDE),
                implementation(),
            ])],
            "implementer#c2": [turn(vec![
                write(&flag("c2-up"), "1"),
                json!({"kind":"wait_for","path": flag("c1-up"), "ms": 20000}),
                write("src/Calculator.psm1", &format!("{DIVIDE}# two\n")),
                implementation(),
            ])],
            "reviewer": [turn(vec![select("c1", &["c1", "c2"])]), turn(vec![approved()])],
        }),
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        outcomes(&env, "OV-1"),
        vec!["checkpointed", "checkpointed"],
        "{err}"
    );
    assert_eq!(task(&env, "OV-1").state, St::Completed);
    // each turn ran in its own worktree, with its own TEMP
    let ctx: Vec<Value> = calls(&env, "OV-1")
        .into_iter()
        .filter(|c| {
            c["label"]
                .as_str()
                .is_some_and(|l| l.starts_with("implement#c"))
        })
        .collect();
    assert_eq!(ctx.len(), 2);
}

// ── B7: the candidates' check gates (AC-18) ────────────────────────────────────────────────

/// AC-18: each checkpointed candidate runs the check gates in `<verify_root>/<task>/c<k>` with a
/// verify result of its own; an environment gate never runs for a candidate (it runs once, in the
/// task's Verifying of the winner).
#[test]
fn m17_ac18_candidates_run_the_check_gates_only_each_in_its_own_checkout() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    // an environment gate after the check gate, defined by the machine: it logs each run
    let dir = env.root.join("gates");
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir
        .join("sync.log")
        .display()
        .to_string()
        .replace('\\', "/");
    let (program, args) = if cfg!(windows) {
        let f = dir.join("dbsync.ps1");
        std::fs::write(
            &f,
            format!("Add-Content -Path '{log}' -Value $env:TORYO_SHA\nexit 0\n"),
        )
        .unwrap();
        (
            "pwsh".to_string(),
            format!(
                "[-NoProfile, -NonInteractive, -File, '{}']",
                f.display().to_string().replace('\\', "/")
            ),
        )
    } else {
        let f = dir.join("dbsync.sh");
        std::fs::write(&f, format!("echo \"$TORYO_SHA\" >> '{log}'\nexit 0\n")).unwrap();
        (
            "sh".to_string(),
            format!("['{}']", f.display().to_string().replace('\\', "/")),
        )
    };
    let m = std::fs::read_to_string(&env.machine).unwrap();
    std::fs::write(
        &env.machine,
        format!("{m}environment_gates:\n  dbsync:\n    program: {program}\n    args: {args}\n    lock: d365-env\n"),
    )
    .unwrap();
    let org = org_rg("", ", candidates: [codex-openai, codex-openai]").replacen(
        "gates:\n",
        "gates:\n  dbsync: { kind: environment, timeout: 2m, if_missing: error }\n",
        1,
    );
    commit_repo(
        &env,
        &[
            (".toryo/toryo-orgchart.yaml", &org),
            (
                ".toryo/roles/implementer/toryo-role.yaml",
                "toryo: 1\npurpose: Change the code.\nbefore_handoff: [divide-present, dbsync]\n",
            ),
        ],
        "an environment gate",
    );
    create(&env, "VF-1");
    let (code, err) = run_with(
        &env,
        "VF-1",
        json!({
            "implementer#c1": [candidate_turn("# one\n")],
            "implementer#c2": [candidate_turn("# two\n")],
            "reviewer": [turn(vec![select("c2", &["c2", "c1"])]), turn(vec![approved()])],
        }),
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "VF-1");
    assert_eq!(t.state, St::Completed, "{err}");
    let c = candidates(&env, "VF-1");
    for k in 1..=2u32 {
        let vr: Value = serde_json::from_slice(
            &std::fs::read(
                t.artifacts()
                    .join(format!("candidates/c{k}/VERIFY_RESULT.json")),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(vr["candidate"], format!("c{k}"));
        assert_eq!(vr["head"].as_str(), c[k as usize - 1].head.as_deref());
        assert_eq!(vr["pass"], true);
        let names: Vec<&str> = vr["commands"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x["name"].as_str())
            .collect();
        assert_eq!(names, vec!["divide-present"], "check gates only: {vr}");
        assert!(
            err.contains(&format!(
                "verify{}VF-1{}c{k}{}VF-1.c{k}.verify",
                std::path::MAIN_SEPARATOR,
                std::path::MAIN_SEPARATOR,
                std::path::MAIN_SEPARATOR
            )) || err.contains(&format!("verify/VF-1/c{k}/VF-1.c{k}.verify")),
            "c{k}'s own checkout: {err}"
        );
    }
    // the environment gate ran once: on the promoted winner
    let runs: Vec<String> = std::fs::read_to_string(env.root.join("gates").join("sync.log"))
        .unwrap()
        .lines()
        .map(|l| l.trim().to_string())
        .collect();
    assert_eq!(runs, vec![c[1].head.clone().unwrap()], "{err}");
    // the candidates' verify checkouts stay (as the task's own) until `task cleanup`
    assert!(env
        .root
        .join("verify")
        .join("VF-1")
        .join("c1")
        .join("VF-1.c1.verify")
        .exists());
    let (code, err) = toryo(&env, &["task", "cleanup", "VF-1"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(!env.root.join("verify").join("VF-1").exists(), "{err}");
    let list = git(&env.repo, &["worktree", "list", "--porcelain"]);
    assert!(!list.contains("VF-1"), "{list}");
}

/// Edge case 30, AC-17: a candidate's check-gate code mutates its verify checkout — that
/// candidate is disqualified (`verify_mutated_tree`); the other wins.
#[test]
fn m17_ec30_a_gate_that_mutates_a_candidates_checkout_disqualifies_it() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    let gate = gate_program(
        &env.root,
        "mutating-gate",
        "git grep -q -F -e \"# one\" -- src\nif errorlevel 1 goto check\necho mutated>>README.md\n:check\ngit grep -q Divide-Numbers -- src\nexit /b %errorlevel%\n",
        "if git grep -q -F -e '# one' -- src; then echo mutated >> README.md; fi\ngit grep -q Divide-Numbers -- src\n",
    );
    commit_repo(
        &env,
        &[(
            ".toryo/toryo-orgchart.yaml",
            &org_gate(&gate, ", candidates: [codex-openai, codex-openai]"),
        )],
        "a gate that mutates",
    );
    create(&env, "MU-1");
    let (code, err) = run_with(
        &env,
        "MU-1",
        json!({
            "implementer#c1": [candidate_turn("# one\n")],
            "implementer#c2": [candidate_turn("# two\n")],
            "reviewer": [turn(vec![approved()])],
        }),
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "MU-1").state, St::Completed, "{err}");
    assert_eq!(
        outcomes(&env, "MU-1"),
        vec!["disqualified:verify_mutated_tree", "checkpointed"],
        "{err}"
    );
    let f = fanout(&env, "MU-1").unwrap();
    assert_eq!((f.winner, f.by.as_deref()), (Some(2), Some("only")));
}

/// Edge case 33, AC-17: a machine-caused stop in a candidate's check gates (the gate program is
/// missing: `gate_unavailable`) escalates the task; a plain `resume` continues the fan-out without
/// running a finished candidate turn again.
#[test]
fn m17_ec33_a_machine_stop_in_a_candidate_verify_resumes_without_new_turns() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    let gate = gate_program(
        &env.root,
        "present-gate",
        "git grep -q Divide-Numbers -- src\nexit /b %errorlevel%\n",
        "git grep -q Divide-Numbers -- src\n",
    );
    commit_repo(
        &env,
        &[(
            ".toryo/toryo-orgchart.yaml",
            &org_gate(&gate, ", candidates: [codex-openai, codex-openai]"),
        )],
        "a gate program outside the repository",
    );
    create(&env, "MS-1");
    let saved = std::fs::read(&gate).unwrap();
    std::fs::remove_file(&gate).unwrap();
    let (_, err) = run_with(
        &env,
        "MS-1",
        json!({
            "implementer#c1": [candidate_turn("# one\n")],
            "implementer#c2": [candidate_turn("# two\n")],
        }),
        &[],
    );
    let t = task(&env, "MS-1");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(t.machine.reason_class(), Some("gate_unavailable"), "{err}");
    assert_eq!(fanout(&env, "MS-1").unwrap().phase, "verify");
    std::fs::write(&gate, saved).unwrap();
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gate, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let s = script(
        &env,
        "ms2.json",
        json!({"reviewer": [turn(vec![select("c2", &["c2", "c1"])]), turn(vec![approved()])]}),
    );
    let (code, err) = toryo(&env, &["resume", "MS-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "MS-1").state, St::Completed, "{err}");
    for k in 1..=2 {
        assert_eq!(
            stage_profiles(&env, "MS-1", &format!("implement#c{k}")).len(),
            1,
            "c{k}: one turn"
        );
    }
    assert_eq!(fanout(&env, "MS-1").unwrap().winner, Some(2));
}

// ── B4: the candidates' worktrees and branches (AC-14) ─────────────────────────────────────

/// The candidates' folders and branches of task `id` that exist.
fn leftovers(env: &Env, id: &str) -> (Vec<u32>, Vec<String>) {
    let folders = (1..=4).filter(|k| cwt(env, id, *k).exists()).collect();
    let branches = refs(&env.repo)
        .into_iter()
        .filter(|r| r.starts_with(&format!("refs/heads/toryo-candidate/{id}/")))
        .collect();
    (folders, branches)
}

/// The turns of a passing two-candidate run (c2 chosen by the compare turn).
fn passing_turns() -> Value {
    json!({
        "implementer#c1": [candidate_turn("# one\n")],
        "implementer#c2": [candidate_turn("# two\n")],
        "reviewer": [turn(vec![select("c2", &["c2", "c1"])]), turn(vec![approved()])],
    })
}

/// AC-14: a failure while the worktrees are made takes back every one made and the record; the
/// next `resume` makes them again and the task completes.
#[test]
fn m17_ac14_a_failure_while_the_worktrees_are_made_takes_them_back() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "WT-1");
    let (code, err) = run_with(
        &env,
        "WT-1",
        passing_turns(),
        &[("TORYO_TEST_FAIL_AT", "fanout:after_worktree")],
    );
    assert_ne!(code, 0, "{err}");
    assert!(
        err.contains("removed: the worktree")
            && err.contains("removed: the branch toryo-candidate/WT-1/c1"),
        "{err}"
    );
    assert_eq!(leftovers(&env, "WT-1"), (vec![], vec![]), "{err}");
    assert!(fanout(&env, "WT-1").is_none());
    assert!(candidates(&env, "WT-1").is_empty());
    let s = script(&env, "wt1.json", passing_turns());
    let (code, err) = toryo(&env, &["resume", "WT-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "WT-1").state, St::Completed, "{err}");
}

/// Edge case 37, AC-14: a candidate folder or branch the store did not record is refused by name
/// (`candidate_unavailable`, a machine stop) and nothing is deleted; once removed by hand, a plain
/// `resume` continues.
#[test]
fn m17_ec37_a_leftover_folder_or_branch_is_refused_by_name_and_kept() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "LO-1");
    git(&env.repo, &["branch", "toryo-candidate/LO-1/c1", "main"]);
    std::fs::create_dir_all(cwt(&env, "LO-1", 2)).unwrap();
    std::fs::write(cwt(&env, "LO-1", 2).join("mine.txt"), "the owner's\n").unwrap();
    let (_, err) = run_with(&env, "LO-1", passing_turns(), &[]);
    let t = task(&env, "LO-1");
    assert_eq!(t.state, St::Escalated, "{err}");
    assert_eq!(t.machine.reason_class(), Some("candidate_unavailable"));
    let why = t.machine.reason().unwrap_or_default().to_string();
    assert!(why.contains("the branch toryo-candidate/LO-1/c1"), "{why}");
    // (the folder is `<worktrees_root>` as configured, joined with the platform's separator)
    let folder = cwt(&env, "LO-1", 2)
        .display()
        .to_string()
        .replace('\\', "/");
    assert!(
        why.replace('\\', "/")
            .contains(&format!("the folder {folder} already exist")),
        "{why}"
    );
    assert!(
        why.contains("Toryo never deletes what it did not make"),
        "{why}"
    );
    // nothing deleted, nothing recorded
    assert!(cwt(&env, "LO-1", 2).join("mine.txt").exists());
    assert_eq!(
        leftovers(&env, "LO-1").1,
        vec!["refs/heads/toryo-candidate/LO-1/c1"]
    );
    assert!(fanout(&env, "LO-1").is_none());
    // removed by hand: a plain resume continues
    git(&env.repo, &["branch", "-D", "toryo-candidate/LO-1/c1"]);
    std::fs::remove_dir_all(cwt(&env, "LO-1", 2)).unwrap();
    let s = script(&env, "lo2.json", passing_turns());
    let (code, err) = toryo(&env, &["resume", "LO-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "LO-1").state, St::Completed, "{err}");
}

// ── B5, B12: Ctrl+C and `toryo stop` (AC-15, AC-25, edge case 31) ──────────────────────────

/// Edge case 31: Ctrl+C while the worktrees are made — what was made is taken back; `resume`
/// starts the fan-out again.
#[test]
fn m17_ec31_ctrl_c_during_worktree_creation_takes_back() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "CC-1");
    let (_, err) = run_with(
        &env,
        "CC-1",
        passing_turns(),
        &[("TORYO_TEST_INTERRUPT_AT", "fanout:after_worktree")],
    );
    assert!(
        err.contains("stop request handled during the creation of the candidates' worktrees"),
        "{err}"
    );
    assert_eq!(leftovers(&env, "CC-1"), (vec![], vec![]), "{err}");
    assert!(fanout(&env, "CC-1").is_none());
    assert_eq!(task(&env, "CC-1").state, St::Implementing);
    let s = script(&env, "cc1.json", passing_turns());
    let (code, err) = toryo(&env, &["resume", "CC-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "CC-1").state, St::Completed, "{err}");
}

/// Edge case 31, AC-15: Ctrl+C while the candidates' turns run ends every one of them; `resume`
/// runs the unfinished turns again.
#[test]
fn m17_ec31_ctrl_c_during_the_turns_ends_every_candidate_turn() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "CC-2");
    let (_, err) = run_with(
        &env,
        "CC-2",
        json!({"implementer#c1": [turn(vec![hang()])], "implementer#c2": [turn(vec![hang()])]}),
        &[("TORYO_TEST_INTERRUPT_AT", "fanout:turns")],
    );
    let a: Vec<_> = attempts(&env, "CC-2")
        .into_iter()
        .filter(|a| a.stage.starts_with("implement#c"))
        .collect();
    assert_eq!(a.len(), 2, "{err}");
    assert!(
        a.iter()
            .all(|a| a.status == toryo::engine::store::AttemptStatus::Interrupted),
        "{a:?}\n{err}"
    );
    let ev = events(&env, "CC-2");
    assert_eq!(
        ev.iter().filter(|e| e["type"] == "agent_killed").count(),
        2,
        "{err}"
    );
    assert_eq!(fanout(&env, "CC-2").unwrap().phase, "turns");
    let s = script(&env, "cc2.json", passing_turns());
    let (code, err) = toryo(&env, &["resume", "CC-2", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "CC-2").state, St::Completed, "{err}");
    assert_eq!(stage_profiles(&env, "CC-2", "implement#c1").len(), 2);
}

/// Edge case 31, AC-25: a second Ctrl+C while the turns are being ended ends the process at once;
/// the next `resume` recovers (the turns were interrupted) and completes.
#[test]
fn m17_ec31_a_second_ctrl_c_ends_the_process_and_resume_recovers() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "CC-3");
    let (code, err) = run_with(
        &env,
        "CC-3",
        json!({"implementer#c1": [turn(vec![hang()])], "implementer#c2": [turn(vec![hang()])]}),
        &[("TORYO_TEST_INTERRUPT_AT", "fanout:turns,fanout:stopping")],
    );
    assert_eq!(code, 130, "{err}");
    let s = script(&env, "cc3.json", passing_turns());
    let (code, err) = toryo(&env, &["resume", "CC-3", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "CC-3").state, St::Completed, "{err}");
    assert!(attempts(&env, "CC-3")
        .iter()
        .filter(|a| a.stage == "implement#c1")
        .any(|a| a.status == toryo::engine::store::AttemptStatus::Interrupted));
}

/// Edge case 31: `toryo stop` from another process during the fan-out ends every candidate turn.
#[test]
fn m17_ec31_toryo_stop_from_another_process_ends_the_fan_out() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "ST-1");
    let s = script(
        &env,
        "st1.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer#c1": [turn(vec![hang()])],
            "implementer#c2": [turn(vec![hang()])],
        }),
    );
    let mut child = spawn(
        &env,
        &["run", "ST-1", "--yes", "--fake-agents", &s],
        "st1.log",
    );
    wait_until("both candidate turns", 60, || {
        events(&env, "ST-1")
            .iter()
            .filter(|e| e["type"] == "agent_spawned")
            .count()
            >= 3
    });
    let (code, err) = toryo(&env, &["stop", "ST-1"], &[]);
    assert_eq!(code, 0, "{err}");
    let st = child.wait().unwrap();
    let log = std::fs::read_to_string(env.root.join("st1.log")).unwrap();
    assert!(st.code().is_some(), "{log}");
    let a: Vec<_> = attempts(&env, "ST-1")
        .into_iter()
        .filter(|a| a.stage.starts_with("implement#c"))
        .collect();
    assert!(
        a.iter()
            .all(|a| a.status == toryo::engine::store::AttemptStatus::Interrupted),
        "{a:?}\n{log}"
    );
    assert_eq!(task(&env, "ST-1").state, St::Implementing, "{log}");
    let s = script(&env, "st2.json", passing_turns());
    let (code, err) = toryo(&env, &["resume", "ST-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "ST-1").state, St::Completed, "{err}");
}

// ── B9: promotion (AC-22) ──────────────────────────────────────────────────────────────────

/// Edge case 38, AC-22, X60: the task worktree is dirty, or its HEAD left the task branch or the
/// base, before the promotion — the task stops (`candidate_unavailable`, a machine stop) and no
/// branch moves; put back by hand, a plain `resume` promotes.
#[test]
fn m17_ec38_a_dirty_task_worktree_or_a_moved_head_stops_the_promotion() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "PM-1");
    let (code, err) = run_with(
        &env,
        "PM-1",
        passing_turns(),
        &[("TORYO_TEST_CRASH_AT", "fanout:after_select")],
    );
    assert_eq!(code, 86, "{err}");
    let w = wt(&env, "PM-1");
    let base = task(&env, "PM-1").base_commit;
    let s = script(
        &env,
        "pm1.json",
        json!({"reviewer": [turn(vec![approved()])]}),
    );
    let refused = |want: &str| {
        let before = git(&w, &["rev-parse", "toryo/PM-1"]);
        let (_, err) = toryo(&env, &["resume", "PM-1", "--yes", "--fake-agents", &s], &[]);
        let t = task(&env, "PM-1");
        assert_eq!(t.state, St::Escalated, "{err}");
        assert_eq!(
            t.machine.reason_class(),
            Some("candidate_unavailable"),
            "{err}"
        );
        let why = t.machine.reason().unwrap_or_default().to_string();
        assert!(
            why.contains("the winner c2 (codex-openai) cannot be promoted") && why.contains(want),
            "`{want}` not in {why}"
        );
        assert!(why.ends_with("then `toryo resume PM-1`"), "{why}");
        assert_eq!(
            git(&w, &["rev-parse", "toryo/PM-1"]),
            before,
            "no branch moved"
        );
    };
    // dirty
    std::fs::write(w.join("stray.txt"), "x\n").unwrap();
    refused("the worktree is not clean ([\"stray.txt\"])");
    std::fs::remove_file(w.join("stray.txt")).unwrap();
    // a commit on top of the base
    git(
        &w,
        &[
            "-c",
            "user.name=o",
            "-c",
            "user.email=o@o",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "owner",
        ],
    );
    refused("not at the base");
    git(&w, &["reset", "-q", "--hard", &base]);
    // another branch
    git(&w, &["checkout", "-q", "-b", "elsewhere"]);
    refused("HEAD is not on toryo/PM-1");
    git(&w, &["checkout", "-q", "toryo/PM-1"]);
    // put back: a plain resume promotes
    let (code, err) = toryo(&env, &["resume", "PM-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "PM-1").state, St::Completed, "{err}");
    let c = candidates(&env, "PM-1");
    assert_eq!(
        git(&w, &["rev-parse", "HEAD"]).trim(),
        c[1].head.as_deref().unwrap()
    );
}

// ── B10: losers and cleanup (AC-23, edge case 36) ──────────────────────────────────────────

/// Edge case 36, AC-23: `task cleanup` in the middle of a fan-out (the task not active) removes the
/// candidate worktrees the store recorded — `--force` is needed while one holds unsaved work — and
/// keeps their branches.
#[test]
fn m17_ec36_task_cleanup_in_the_middle_of_a_fan_out() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    let gate = gate_program(
        &env.root,
        "present-gate",
        "git grep -q Divide-Numbers -- src\nexit /b %errorlevel%\n",
        "git grep -q Divide-Numbers -- src\n",
    );
    commit_repo(
        &env,
        &[(
            ".toryo/toryo-orgchart.yaml",
            &org_gate(&gate, ", candidates: [codex-openai, codex-openai]"),
        )],
        "gate",
    );
    create(&env, "CL-1");
    std::fs::remove_file(&gate).unwrap();
    let (_, err) = run_with(&env, "CL-1", passing_turns(), &[]);
    assert_eq!(task(&env, "CL-1").state, St::Escalated, "{err}");
    assert_eq!(leftovers(&env, "CL-1").0, vec![1, 2], "{err}");
    // unsaved work in c1: refused without --force, nothing deleted
    std::fs::write(cwt(&env, "CL-1", 1).join("wip.txt"), "unsaved\n").unwrap();
    let (code, err) = toryo(&env, &["task", "cleanup", "CL-1"], &[]);
    assert_ne!(code, 0, "{err}");
    assert!(
        err.contains("uncommitted change(s) in the worktree") && err.contains("of candidate c1"),
        "{err}"
    );
    assert_eq!(leftovers(&env, "CL-1").0, vec![1, 2]);
    let (code, err) = toryo(&env, &["task", "cleanup", "CL-1", "--force"], &[]);
    assert_eq!(code, 0, "{err}");
    let (folders, branches) = leftovers(&env, "CL-1");
    assert!(folders.is_empty(), "{err}");
    assert_eq!(branches.len(), 2, "branches kept: {err}");
    assert!(
        err.contains(
            "the candidates' branches are kept: toryo-candidate/CL-1/c1, toryo-candidate/CL-1/c2"
        ),
        "{err}"
    );
    assert!(!git(&env.repo, &["worktree", "list"]).contains("CL-1"));
}

/// AC-23: `task cleanup` after a crash in the middle of the turns removes the candidate worktrees.
#[test]
fn m17_ac23_task_cleanup_after_a_crash_in_the_middle_of_a_fan_out() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "CL-2");
    let (code, err) = run_with(
        &env,
        "CL-2",
        passing_turns(),
        &[("TORYO_TEST_CRASH_AT", "fanout:after_checkpoint")],
    );
    assert_eq!(code, 86, "{err}");
    assert!(!leftovers(&env, "CL-2").0.is_empty());
    let (code, err) = toryo(&env, &["task", "cleanup", "CL-2", "--force"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(leftovers(&env, "CL-2").0.is_empty(), "{err}");
    assert!(!git(&env.repo, &["worktree", "list"]).contains("CL-2"));
}

// ── B12: a crash at every point of the fan-out (AC-25, edge case 32) ───────────────────────

/// A run of the passing two-candidate fan-out that crashes at `point`, then a `resume` (whose
/// reviewer answers the compare turn again only when `compare_again`) → the env and the task id
/// after the resume, which must have completed with c2, chosen by the compare turn, promoted
/// once.
fn crash_and_resume(id: &str, point: &str, compare_again: bool) -> (Env, String) {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, id);
    let (code, err) = run_with(&env, id, passing_turns(), &[("TORYO_TEST_CRASH_AT", point)]);
    assert_eq!(code, 86, "{point}: {err}");
    let mut turns = passing_turns();
    if !compare_again {
        turns["reviewer"] = json!([turn(vec![approved()])]);
    }
    let s = script(&env, &format!("{id}-resume.json"), turns);
    let (code, out) = toryo(&env, &["resume", id, "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{point}: {out}");
    let t = task(&env, id);
    assert_eq!(t.state, St::Completed, "{point}: {out}");
    let f = fanout(&env, id).unwrap();
    assert_eq!(
        (f.winner, f.by.as_deref(), f.phase.as_str()),
        (Some(2), Some("compare"), "promoted"),
        "{point}"
    );
    let c = candidates(&env, id);
    assert_eq!(
        git(&wt(&env, id), &["rev-parse", "HEAD"]).trim(),
        c[1].head.as_deref().unwrap(),
        "{point}: the task branch is c2's checkpoint"
    );
    assert_eq!(
        stage_profiles(&env, id, "implement").len(),
        1,
        "{point}: one promotion"
    );
    // no commit Toryo did not make: each candidate branch is one checkpoint on the base
    for k in 1..=2 {
        let log = git(
            &env.repo,
            &[
                "log",
                "--format=%(trailers:key=Toryo-Candidate,valueonly)",
                &format!("{}..toryo-candidate/{id}/c{k}", t.base_commit),
            ],
        );
        assert_eq!(log.trim(), format!("c{k}"), "{point}: c{k}'s branch");
    }
    for k in 1..=2 {
        assert!(!cwt(&env, id, k).exists(), "{point}: c{k}'s folder removed");
    }
    (env, format!("{err}\n{out}"))
}

/// The number of turns candidate `k` of `id` had.
fn turns_of(env: &Env, id: &str, k: u32) -> usize {
    stage_profiles(env, id, &format!("implement#c{k}")).len()
}

/// Edge case 32: a crash after the first candidate's checkpoint, before the second's — `resume`
/// takes that checkpoint by its trailers (no new turn for it) and completes with the same winner.
#[test]
fn m17_ec32_a_crash_after_the_first_checkpoint_before_the_second() {
    let (env, out) = crash_and_resume("CR-1", "fanout:after_checkpoint", true);
    assert!(out.contains("(already committed: trailer match)"), "{out}");
    let done: Vec<u32> = (1..=2)
        .filter(|k| turns_of(&env, "CR-1", *k) == 1)
        .collect();
    assert!(
        !done.is_empty(),
        "the checkpointed candidate's turn did not run again: {out}"
    );
}

/// Edge case 32: a crash after the selection, before the promotion — the same winner, no second
/// compare turn, one promotion, no turn again.
#[test]
fn m17_ec32_a_crash_after_the_selection_before_the_promotion() {
    let (env, out) = crash_and_resume("CR-2", "fanout:after_select", false);
    assert_eq!(
        attempts(&env, "CR-2")
            .iter()
            .filter(|a| a.stage == "select")
            .count(),
        1,
        "{out}"
    );
    assert_eq!(
        (turns_of(&env, "CR-2", 1), turns_of(&env, "CR-2", 2)),
        (1, 1)
    );
}

/// Edge case 32: a crash after the branch moved to the winner, before the state change — one
/// promotion (no second reset, no new commit), no turn again.
#[test]
fn m17_ec32_a_crash_after_the_promotion_before_the_state_change() {
    let (env, out) = crash_and_resume("CR-3", "fanout:after_promote_ref", false);
    assert_eq!(
        (turns_of(&env, "CR-3", 1), turns_of(&env, "CR-3", 2)),
        (1, 1),
        "{out}"
    );
    let ev = events(&env, "CR-3");
    let found: Vec<&Value> = ev
        .iter()
        .filter(|e| e["type"] == "checkpoint_created")
        .collect();
    assert_eq!(found.len(), 1, "{ev:?}");
    assert_eq!(
        found[0]["found"], true,
        "the moved branch was found, not moved again"
    );
}

/// AC-25: a crash at each other point of the fan-out — while the worktrees are made, when a turn
/// began or ended, between the checkpoints and the gates, inside a candidate's gates, after the
/// compare turn, after the promotion's record — resumes to the same result.
#[test]
fn m17_ac25_a_crash_at_every_point_of_the_fan_out_resumes_to_the_same_result() {
    for (n, (point, compare_again, turns_kept)) in [
        ("fanout:after_worktree", true, false),
        ("fanout:after_begin", true, false),
        ("fanout:after_turn", true, false),
        ("fanout:before_verify", true, true),
        ("fanout:after_gates", true, true),
        ("fanout:after_compare_turn", true, true),
        ("fanout:after_promote", false, true),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("CM-{n}");
        let (env, out) = crash_and_resume(&id, point, compare_again);
        if turns_kept {
            assert_eq!(
                (turns_of(&env, &id, 1), turns_of(&env, &id, 2)),
                (1, 1),
                "{point}: {out}"
            );
        }
    }
}

// ── Z1: red-green and best-of-N together (AC-29, edge case 43) ─────────────────────────────

/// A candidate turn of the red-green fixture: the division, its test file, and `proof`.
fn rg_candidate(proof: Value) -> Value {
    turn(vec![
        write("src/Calculator.psm1", DIVIDE),
        write(
            "tests/Divide.Tests.ps1",
            "Describe 'Divide' { It 'divides' { Divide-Numbers -A 6 -B 3 | Should -Be 2 } }\n",
        ),
        implementation_with(proof),
    ])
}

/// Edge case 43, AC-29, AC-20 (rung 2): two candidates pass a red-green gate, the compare turn
/// fails, and the one with more acceptance criteria proven wins on the score; the task's own
/// Verifying proves the winner again with the winner's proof map (X51).
#[test]
fn m17_ec43_red_green_and_best_of_n_more_proven_wins_on_the_score() {
    let env = setup_bon("[codex-openai, codex-openai]", RG_SRC, &[]);
    let text = std::fs::read_to_string(env.repo.join("TASK.md")).unwrap();
    commit_repo(
        &env,
        &[(
            "TASK.md",
            &format!("{text}\n## Proof exempt\n- AC-3: the export list is read by the reviewer\n"),
        )],
        "AC-3 exempt",
    );
    create(&env, "RB-1");
    let (code, err) = run_with(
        &env,
        "RB-1",
        json!({
            "implementer#c1": [rg_candidate(json!([
                {"ac": "AC-1", "tests": ["Divide-Numbers"]},
                {"ac": "AC-2", "tests": ["Division"]},
            ]))],
            "implementer#c2": [rg_candidate(full_proof())],
            "reviewer": [turn(vec![fail("Overloaded", "busy")]), turn(vec![approved()])],
        }),
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "RB-1").state, St::Completed, "{err}");
    let c = candidates(&env, "RB-1");
    assert_eq!(
        (c[0].verify_pass, c[1].verify_pass),
        (Some(true), Some(true)),
        "{err}"
    );
    assert_eq!((c[0].proven, c[1].proven), (Some(2), Some(3)), "{err}");
    let f = fanout(&env, "RB-1").unwrap();
    assert_eq!(
        (f.winner, f.by.as_deref()),
        (Some(2), Some("score")),
        "{err}"
    );
    assert_eq!(
        f.reason.as_deref(),
        Some("the compare turn failed (Overloaded); score: c2 before c1 — more acceptance criteria proven red-green (3 vs 2)")
    );
    // the task's Verifying of the promoted winner proved its criteria with c2's map
    let vr = verify_result(&env, "RB-1");
    assert_eq!(vr["red_green"]["proven"], 3, "{vr}");
    assert_eq!(vr["head"].as_str(), c[1].head.as_deref());
}

// ── B11: the budget and the usage (AC-24, AC-26) ───────────────────────────────────────────

/// `{"kind":"usage", …}` of a fake turn.
fn tokens(input: u64, output: u64) -> Value {
    json!({"kind":"usage","input":input,"output":output})
}

/// Edge case 34, AC-24: a budget that admits one candidate turn of two (each estimated at the
/// default reservation, nothing reported yet) takes the single path with a note.
#[test]
fn m17_ec34_a_budget_for_one_candidate_takes_the_single_path() {
    let env = setup_bon("[codex-openai, claude-max]", "", &[]);
    create(&env, "BU-1");
    // the plan turn reports 10 tokens: 10 + one estimate (the default reservation) reaches it
    let (code, err) = toryo(&env, &["budget", "set", "BU-1", "--tokens", "200000"], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = run_with(
        &env,
        "BU-1",
        json!({
            "planner": [turn(vec![tokens(10, 0), plan()])],
            "implementer": [turn(vec![tokens(10, 0), write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![tokens(10, 0), approved()])],
        }),
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "BU-1").state, St::Completed, "{err}");
    let f = fanout(&env, "BU-1").unwrap();
    assert_eq!((f.phase.as_str(), f.n), ("single", 0));
    assert!(
        err.contains("note: best-of-N: the budget admits 1 of 2 candidate turns (~200000 tokens each) — the single implementer path runs"),
        "{err}"
    );
    assert!(candidates(&env, "BU-1").is_empty());
    assert_eq!(
        stage_profiles(&env, "BU-1", "implement"),
        vec!["codex-openai"]
    );
}

/// Edge case 34, AC-24: a budget that admits two candidate turns of three runs c1 and c2, with a
/// note.
#[test]
fn m17_ec34_a_budget_for_two_of_three_runs_two_candidates() {
    let env = setup_bon("[codex-openai, codex-openai, claude-max]", "", &[]);
    create(&env, "BU-2");
    // 10 (the plan) + 2 estimates reach 300000, 10 + 1 does not
    let (code, err) = toryo(&env, &["budget", "set", "BU-2", "--tokens", "300000"], &[]);
    assert_eq!(code, 0, "{err}");
    let mut turns = json!({
        "planner": [turn(vec![tokens(10, 0), plan()])],
        "implementer#c1": [turn(vec![tokens(10, 0), write("src/Calculator.psm1", &format!("{DIVIDE}# one\n")), implementation()])],
        "implementer#c2": [turn(vec![tokens(10, 0), write("src/Calculator.psm1", &format!("{DIVIDE}# two\n")), implementation()])],
    });
    turns["reviewer"] = json!([
        turn(vec![tokens(10, 0), select("c2", &["c2", "c1"])]),
        turn(vec![tokens(10, 0), approved()])
    ]);
    let (code, err) = run_with(&env, "BU-2", turns, &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "BU-2").state, St::Completed, "{err}");
    assert!(
        err.contains("note: best-of-N: the budget admits 2 of 3 candidate turns (~200000 tokens each) — c1 … c2 run"),
        "{err}"
    );
    assert_eq!(candidates(&env, "BU-2").len(), 2);
    assert_eq!(fanout(&env, "BU-2").unwrap().n, 2);
    assert!(attempts(&env, "BU-2")
        .iter()
        .all(|a| a.stage != "implement#c3"));
}

/// Edge case 34, AC-24, X57: the live interrupt counts the candidates' running turns together —
/// at the budget every candidate turn is cut and the task pauses; `budget set` + `resume` runs only
/// the unfinished candidate again.
#[test]
fn m17_ec34_a_live_interrupt_in_the_turns_pauses_and_resume_runs_the_unfinished() {
    let env = setup_bon("[claude-max, claude-max]", "", &[]);
    create(&env, "BU-3");
    let (code, err) = toryo(&env, &["budget", "set", "BU-3", "--tokens", "250000"], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = run_with(
        &env,
        "BU-3",
        json!({
            "planner": [turn(vec![tokens(10, 0), plan()])],
            "implementer#c1": [turn(vec![tokens(100, 0), write("src/Calculator.psm1", DIVIDE), implementation()])],
            "implementer#c2": [turn(vec![json!({"kind":"delay","ms":500}), tokens(300_000, 10), hang()])],
        }),
        &[],
    );
    assert_eq!(code, 1, "{err}");
    let t = task(&env, "BU-3");
    assert_eq!(t.state, St::Paused, "{err}");
    assert!(
        err.contains("budget reached during the candidates' turns"),
        "{err}"
    );
    assert_eq!(
        outcomes(&env, "BU-3"),
        vec!["checkpointed".to_string(), "(pending)".to_string()],
        "{err}"
    );
    let a: Vec<_> = attempts(&env, "BU-3")
        .into_iter()
        .filter(|a| a.stage == "implement#c2")
        .collect();
    assert_eq!(
        (a[0].status, a[0].error_class.as_deref()),
        (
            toryo::engine::store::AttemptStatus::Interrupted,
            Some("budget")
        )
    );
    let ev = events(&env, "BU-3");
    assert!(
        ev.iter().any(|e| e["type"] == "agent_killed"
            && e["stage"] == "implement#c2"
            && e["reason"] == "budget"),
        "{ev:?}"
    );
    let (code, err) = toryo(
        &env,
        &["budget", "set", "BU-3", "--tokens", "100000000"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let s = script(
        &env,
        "bu3b.json",
        json!({
            "implementer#c2": [candidate_turn("# two\n")],
            "reviewer": [turn(vec![select("c1", &["c1", "c2"])]), turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["resume", "BU-3", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "BU-3").state, St::Completed, "{err}");
    assert_eq!(
        (turns_of(&env, "BU-3", 1), turns_of(&env, "BU-3", 2)),
        (1, 2),
        "only c2 ran again"
    );
}

/// Edge case 42, AC-26: `toryo usage` lists the candidates' turns as their own stages
/// (`implement#c1`, `implement#c2`) and the compare turn (`select`); the task total is the sum.
#[test]
fn m17_ec42_usage_lists_each_candidate_and_the_total_is_the_sum() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "US-1");
    let (code, err) = run_with(
        &env,
        "US-1",
        json!({
            "planner": [turn(vec![tokens(10, 1), plan()])],
            "implementer#c1": [turn(vec![tokens(100, 10), write("src/Calculator.psm1", DIVIDE), implementation()])],
            "implementer#c2": [turn(vec![tokens(200, 20), write("src/Calculator.psm1", &format!("{DIVIDE}# two\n")), implementation()])],
            "reviewer": [turn(vec![tokens(5, 0), select("c2", &["c2", "c1"])]), turn(vec![tokens(7, 0), approved()])],
        }),
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let prices = env.root.join("prices.toml");
    std::fs::write(&prices, "version = \"t\"\nsafety_factor = 1.0\n").unwrap();
    let (code, raw, err) = run_out(
        &env,
        &[
            "usage",
            "US-1",
            "--json",
            "--prices",
            prices.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "{err}");
    let j: Value = serde_json::from_str(&raw).unwrap();
    let rows = j["rows"].as_array().unwrap();
    let exact = |stage: &str| -> i64 {
        rows.iter()
            .filter(|r| r["stage"] == stage)
            .map(|r| r["exact_tokens"].as_i64().unwrap())
            .sum()
    };
    assert_eq!(exact("implement#c1"), 110, "{j}");
    assert_eq!(exact("implement#c2"), 220, "{j}");
    assert_eq!(exact("select"), 5, "{j}");
    let sum: i64 = rows
        .iter()
        .map(|r| r["exact_tokens"].as_i64().unwrap())
        .sum();
    assert_eq!(j["totals"]["exact_tokens"].as_i64(), Some(sum));
    assert_eq!(sum, 10 + 1 + 110 + 220 + 5 + 7);
    assert_eq!(task(&env, "US-1").spent_tokens, sum);
    let (code, text, _) = run_out(
        &env,
        &["usage", "US-1", "--prices", prices.to_str().unwrap()],
    );
    assert_eq!(code, 0);
    assert!(
        text.lines().any(|l| l.starts_with("implement#c1")),
        "{text}"
    );
    assert!(
        text.lines().any(|l| l.starts_with("implement#c2")),
        "{text}"
    );
    // the candidates' tokens in the view
    let c = candidates(&env, "US-1");
    assert_eq!(c.len(), 2);
    let (_, raw, _) = run_out(&env, &["status", "US-1", "--json"]);
    let v: Value = serde_json::from_str(&raw).unwrap();
    let list = v["tasks"][0]["candidates"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        list.iter()
            .map(|c| c["tokens"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![110, 220],
        "{v}"
    );
}

// ── B13: events, views, dashboard (AC-27, edge case 41) ────────────────────────────────────

/// AC-27: the three fan-out events are recorded and printed by `watch`; `status` and the final
/// gate name the winner, how it was chosen and the losers' branches, in Toryo's words.
#[test]
fn m17_ac27_the_fan_out_events_status_and_the_final_gate() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "OB-1");
    let s = script(&env, "ob1.json", {
        let mut t = passing_turns();
        t["planner"] = json!([turn(vec![plan()])]);
        t["reviewer"] = json!([
            turn(vec![select("c2", &["c2", "c1"])]),
            turn(vec![approved()])
        ]);
        t
    });
    let (code, err) = toryo(&env, &["run", "OB-1", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let (_, err) = toryo(
        &env,
        &[
            "approve",
            "OB-1",
            "--gate",
            "plan",
            "--continue",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    assert_eq!(task(&env, "OB-1").state, St::WaitingFinalApproval, "{err}");
    assert!(err.contains("final gate — best-of-N: c1 codex-openai checkpointed, check gates passed · c2 codex-openai checkpointed, check gates passed"), "{err}");
    assert!(err.contains("final gate — winner c2 (codex-openai) by compare · the other candidates' branches are kept: toryo-candidate/OB-1/c1"), "{err}");
    assert!(
        !err.contains("c2 is the clearest")
            || err.contains("winner: candidate c2 (codex-openai) by compare — c2 is the clearest"),
        "the compare reason only in the selection line: {err}"
    );
    let ev = events(&env, "OB-1");
    let started = ev
        .iter()
        .find(|e| e["type"] == "candidates_started")
        .expect("candidates_started");
    assert_eq!(started["n"], 2);
    assert_eq!(started["profiles"], json!(["codex-openai", "codex-openai"]));
    let finished: Vec<&Value> = ev
        .iter()
        .filter(|e| e["type"] == "candidate_finished")
        .collect();
    assert!(
        finished
            .iter()
            .any(|e| e["k"] == 1 && e["outcome"] == "checkpointed" && e["gates_passed"] == true),
        "{finished:?}"
    );
    let sel = ev
        .iter()
        .find(|e| e["type"] == "candidate_selected")
        .expect("candidate_selected");
    assert_eq!(
        (sel["k"].clone(), sel["by"].clone()),
        (json!(2), json!("compare"))
    );
    let (code, text, err) = run_out(&env, &["watch", "OB-1", "--from-start", "--no-follow"]);
    assert_eq!(code, 0, "{err}");
    for want in [
        "best-of-N: 2 candidates — c1 codex-openai, c2 codex-openai",
        "candidate c1: checkpointed",
        "check gates passed",
        "✔ candidate c2 selected (by compare)",
    ] {
        assert!(text.contains(want), "`{want}` not in {text}");
    }
    let out = status_out(&env, "OB-1");
    assert!(out.contains("winner c2 (codex-openai) by compare · the other candidates' branches are kept: toryo-candidate/OB-1/c1"), "{out}");
    assert!(!out.contains("clearest"), "no agent text in status: {out}");
}

/// Edge case 41, AC-13, AC-27: `status --json`, `task show --json` and `dashboard --json` carry
/// `candidates` — `null` without a fan-out, an array of `{ id, profile, outcome, head, verify,
/// tokens, winner, by, branch }` with one.
#[test]
fn m17_ec41_json_documents_with_and_without_candidates() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "JS-1");
    create(&env, "JS-2");
    let (code, err) = run_with(&env, "JS-1", passing_turns(), &[]);
    assert_eq!(code, 0, "{err}");
    let doc = |args: &[&str]| -> Value {
        let (code, raw, err) = run_out(&env, args);
        assert_eq!(code, 0, "{err}");
        serde_json::from_str(&raw).unwrap()
    };
    let find = |v: &Value, id: &str| -> Value {
        v["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .cloned()
            .unwrap()
    };
    let status = doc(&["status", "--json"]);
    let dash = doc(&["dashboard", "--json"]);
    let show = doc(&["task", "show", "JS-1", "--json"]);
    // without a fan-out (JS-2 never ran): null, and the key is there
    assert!(
        find(&status, "JS-2")
            .get("candidates")
            .is_some_and(Value::is_null),
        "{status}"
    );
    assert!(
        find(&dash, "JS-2")
            .get("candidates")
            .is_some_and(Value::is_null),
        "{dash}"
    );
    let c = candidates(&env, "JS-1");
    for list in [
        find(&status, "JS-1")["candidates"].clone(),
        find(&dash, "JS-1")["candidates"].clone(),
        show["candidates"].clone(),
    ] {
        let list = list
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("an array: {status}"));
        assert_eq!(list.len(), 2);
        assert_eq!(list[1]["id"], "c2");
        assert_eq!(list[1]["profile"], "codex-openai");
        assert_eq!(list[1]["outcome"], "checkpointed");
        assert_eq!(list[1]["head"].as_str(), c[1].head.as_deref());
        assert_eq!(list[1]["verify"], true);
        assert_eq!(
            (list[1]["winner"].clone(), list[1]["by"].clone()),
            (json!(true), json!("compare"))
        );
        assert_eq!(
            (list[0]["winner"].clone(), list[0]["by"].clone()),
            (json!(false), Value::Null)
        );
        assert_eq!(list[0]["branch"], "toryo-candidate/JS-1/c1");
        assert!(list[0]["tokens"].is_i64());
    }
}

/// AC-27: while a process drives the fan-out, the dashboard's `now:` line shows its progress in
/// Toryo's words (`candidates 0/2 verifying`).
#[test]
fn m17_ac27_the_dashboard_now_line_shows_the_fan_out() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    let gate = gate_program(
        &env.root,
        "slow-gate",
        "ping -n 6 127.0.0.1 >nul\ngit grep -q Divide-Numbers -- src\nexit /b %errorlevel%\n",
        "sleep 5\ngit grep -q Divide-Numbers -- src\n",
    );
    commit_repo(
        &env,
        &[(
            ".toryo/toryo-orgchart.yaml",
            &org_gate(&gate, ", candidates: [codex-openai, codex-openai]"),
        )],
        "a slow gate",
    );
    create(&env, "DB-1");
    let s = script(&env, "db1.json", {
        let mut t = passing_turns();
        t["planner"] = json!([turn(vec![plan()])]);
        t
    });
    let mut child = spawn(
        &env,
        &["run", "DB-1", "--yes", "--fake-agents", &s],
        "db1.log",
    );
    wait_until("a candidate's gates to run", 90, || {
        events(&env, "DB-1")
            .iter()
            .any(|e| e["type"] == "stage_started" && e["stage"] == "verify#c1")
    });
    let (code, text, err) = run_out(&env, &["dashboard", "--once"]);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(code, 0, "{err}");
    assert!(
        text.contains("now: candidates 0/2 verifying · verify#c1 attempt 1"),
        "{text}"
    );
}

// ── B14: two tasks with candidates at once (AC-28, edge case 39) ───────────────────────────

/// Edge case 39, AC-28: `toryo run A B --jobs 2` with candidates in both tasks completes, and the
/// machine verify lock serialises every candidate's check gates (the gate notes a second run
/// starting while one runs).
#[test]
fn m17_ec39_two_tasks_with_candidates_run_with_two_jobs() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    // the native spelling for cmd (its `del` can read a `/` as a switch), `/` for sh
    let busy_native = env.root.join("BUSY").display().to_string();
    let overlap_native = env.root.join("OVERLAP").display().to_string();
    let busy = busy_native.replace('\\', "/");
    let overlap = overlap_native.replace('\\', "/");
    let gate = gate_program(
        &env.root,
        "lock-gate",
        &format!("if exist \"{busy_native}\" echo x>>\"{overlap_native}\"\n(echo busy)>\"{busy_native}\"\nping -n 2 127.0.0.1 >nul\ndel \"{busy_native}\"\ngit grep -q Divide-Numbers -- src\nexit /b %errorlevel%\n"),
        &format!("[ -e '{busy}' ] && echo x >> '{overlap}'\ntouch '{busy}'\nsleep 1\nrm -f '{busy}'\ngit grep -q Divide-Numbers -- src\n"),
    );
    commit_repo(
        &env,
        &[(
            ".toryo/toryo-orgchart.yaml",
            &org_gate(&gate, ", candidates: [codex-openai, codex-openai]"),
        )],
        "a gate that notes overlaps",
    );
    create(&env, "MJ-A");
    create(&env, "MJ-B");
    let s = script(&env, "mj.json", {
        let mut t = passing_turns();
        t["planner"] = json!([turn(vec![plan()])]);
        t
    });
    let (code, err) = toryo(
        &env,
        &[
            "run",
            "MJ-A",
            "MJ-B",
            "--jobs",
            "2",
            "--yes",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    for id in ["MJ-A", "MJ-B"] {
        assert_eq!(task(&env, id).state, St::Completed, "{id}: {err}");
        assert_eq!(fanout(&env, id).unwrap().winner, Some(2), "{id}");
    }
    assert!(
        !env.root.join("OVERLAP").exists(),
        "two gates ran at once: {err}"
    );
}

// ── B9: the builder the review judges, and no foreign commit (AC-22, AC-16) ────────────────

/// AC-22: `reviewer-differs-from-builder` judges the winner's profile — a codex winner reviewed by
/// claude passes `error`, though a claude candidate implemented too; a claude winner is refused.
#[test]
fn m17_ac22_reviewer_differs_from_builder_judges_the_winner() {
    for (n, (winner, refused)) in [("c1", false), ("c2", true)].into_iter().enumerate() {
        let org = org_rg("", ", candidates: [codex-openai, claude-max]")
            + "checks:\n  reviewer-differs-from-builder: error\n";
        let env = setup_rg("", "", &[(".toryo/toryo-orgchart.yaml", &org)]);
        let id = format!("SV-{n}");
        create(&env, &id);
        let (_, err) = run_with(
            &env,
            &id,
            json!({
                "implementer#c1": [candidate_turn("# one\n")],
                "implementer#c2": [candidate_turn("# two\n")],
                "reviewer": [turn(vec![select(winner, &[winner, if winner == "c1" { "c2" } else { "c1" }])]), turn(vec![approved()])],
            }),
            &[],
        );
        let t = task(&env, &id);
        if refused {
            assert_eq!(t.state, St::Escalated, "{err}");
            assert!(err.contains("same_vendor_review: claude-max built this checkpoint and claude-max would review it"), "{err}");
        } else {
            assert_eq!(t.state, St::Completed, "{err}");
            assert!(!err.contains("same_vendor_review"), "{err}");
        }
    }
}

/// AC-16: recovery never accepts a commit Toryo did not make — after a crash right after a
/// candidate's turn, a commit in its worktree that carries Toryo's trailers is undone into the tree
/// and checked like any change; the candidate's checkpoint is Toryo's own commit.
#[test]
fn m17_ac16_a_forged_candidate_commit_is_never_accepted() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "FG-1");
    let (code, err) = run_with(
        &env,
        "FG-1",
        passing_turns(),
        &[("TORYO_TEST_CRASH_AT", "fanout:after_turn")],
    );
    assert_eq!(code, 86, "{err}");
    let w1 = cwt(&env, "FG-1", 1);
    let base = task(&env, "FG-1").base_commit;
    std::fs::write(
        w1.join("src").join("Calculator.psm1"),
        format!("{DIVIDE}# forged\n"),
    )
    .unwrap();
    git(&w1, &["add", "-A"]);
    git(
        &w1,
        &[
            "-c", "user.name=agent", "-c", "user.email=a@a", "commit", "-q", "-m",
            "toryo(FG-1): implement#c1 (candidate c1)\n\nToryo-Task: FG-1\nToryo-Stage: implement#c1\nToryo-Attempt: 1\nToryo-Agent: codex-openai\nToryo-Candidate: c1",
        ],
    );
    let forged = git(&w1, &["rev-parse", "HEAD"]).trim().to_string();
    let s = script(&env, "fg2.json", passing_turns());
    let (code, out) = toryo(&env, &["resume", "FG-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(task(&env, "FG-1").state, St::Completed, "{out}");
    let c = candidates(&env, "FG-1");
    assert_ne!(c[0].head.as_deref(), Some(forged.as_str()), "{out}");
    let parent = git(
        &env.repo,
        &["rev-parse", &format!("{}^", c[0].head.as_deref().unwrap())],
    );
    assert_eq!(
        parent.trim(),
        base,
        "c1's checkpoint is one commit on the base"
    );
}

/// Edge case 35 (after the fan-out), AC-12: once the fan-out started, `task config --reload` that
/// changes `candidates` is refused with a message, and the frozen configuration stays.
#[test]
fn m17_ec35_reload_after_the_fan_out_is_refused() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    let gate = gate_program(
        &env.root,
        "present-gate",
        "git grep -q Divide-Numbers -- src\nexit /b %errorlevel%\n",
        "git grep -q Divide-Numbers -- src\n",
    );
    commit_repo(
        &env,
        &[(
            ".toryo/toryo-orgchart.yaml",
            &org_gate(&gate, ", candidates: [codex-openai, codex-openai]"),
        )],
        "gate",
    );
    create(&env, "RL-2");
    std::fs::remove_file(&gate).unwrap();
    let (_, err) = run_with(&env, "RL-2", passing_turns(), &[]);
    // stopped (a machine stop) in the middle of the fan-out: a waiting state, so a reload may run
    assert_eq!(task(&env, "RL-2").state, St::Escalated, "{err}");
    assert!(fanout(&env, "RL-2").is_some());
    let (_, before, _) = run_out(&env, &["task", "config", "RL-2"]);
    commit_repo(
        &env,
        &[(
            ".toryo/toryo-orgchart.yaml",
            &org_gate(&gate, ", candidates: [codex-openai, claude-max]"),
        )],
        "two vendors",
    );
    let (code, err) = toryo(&env, &["task", "config", "RL-2", "--reload"], &[]);
    assert_ne!(code, 0, "{err}");
    assert!(
        err.contains("RL-2: `candidates` cannot change — the best-of-N fan-out already started (phase verify); nothing was changed"),
        "{err}"
    );
    let (_, after, _) = run_out(&env, &["task", "config", "RL-2"]);
    assert_eq!(before, after, "the frozen configuration stays");
}

// ── Z3: the review's findings ──────────────────────────────────────────────────────────────

/// X63 (review): a losing candidate whose turn left uncommitted work (here a crashed turn) keeps
/// its folder — that work exists nowhere else — and says so; `task cleanup` needs `--force` for it.
#[test]
fn m17_x63_a_candidate_folder_with_uncommitted_work_is_kept() {
    let env = setup_bon("[codex-openai, codex-openai]", "", &[]);
    create(&env, "KP-1");
    let (code, err) = run_with(
        &env,
        "KP-1",
        json!({
            "implementer#c1": [turn(vec![write("src/Calculator.psm1", "# half\n"), json!({"kind":"crash"})])],
            "implementer#c2": [candidate_turn("")],
            "reviewer": [turn(vec![approved()])],
        }),
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "KP-1").state, St::Completed, "{err}");
    assert!(cwt(&env, "KP-1", 1).exists(), "kept: {err}");
    assert!(
        !cwt(&env, "KP-1", 2).exists(),
        "the winner's folder is removed"
    );
    assert!(
        err.contains("kept: candidate c1's worktree") && err.contains("1 uncommitted change(s)"),
        "{err}"
    );
    let (code, err) = toryo(&env, &["task", "cleanup", "KP-1"], &[]);
    assert_ne!(code, 0, "{err}");
    assert!(err.contains("of candidate c1"), "{err}");
    let (code, err) = toryo(&env, &["task", "cleanup", "KP-1", "--force"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(!cwt(&env, "KP-1", 1).exists(), "{err}");
}

/// Review: a candidate turn that ended on its own after a sibling's usage crossed the budget is
/// checked and checkpointed as any turn — only the cut turn runs again after `budget set`.
#[test]
fn m17_review_a_turn_that_ended_after_the_budget_was_hit_is_kept() {
    let env = setup_bon("[claude-max, claude-max]", "", &[]);
    create(&env, "BU-4");
    let (code, err) = toryo(&env, &["budget", "set", "BU-4", "--tokens", "250000"], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = run_with(
        &env,
        "BU-4",
        json!({
            "planner": [turn(vec![tokens(10, 0), plan()])],
            // c1 crosses the budget itself and ends at once; c2 runs on until it is cut
            "implementer#c1": [turn(vec![tokens(300_000, 0), write("src/Calculator.psm1", DIVIDE), implementation()])],
            "implementer#c2": [turn(vec![hang()])],
        }),
        &[],
    );
    assert_eq!(code, 1, "{err}");
    assert_eq!(task(&env, "BU-4").state, St::Paused, "{err}");
    assert_eq!(outcomes(&env, "BU-4")[0], "checkpointed", "{err}");
    let (code, err) = toryo(
        &env,
        &["budget", "set", "BU-4", "--tokens", "100000000"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let s = script(
        &env,
        "bu4b.json",
        json!({
            "implementer#c2": [candidate_turn("# two\n")],
            "reviewer": [turn(vec![select("c1", &["c1", "c2"])]), turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["resume", "BU-4", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "BU-4").state, St::Completed, "{err}");
    assert_eq!(
        (turns_of(&env, "BU-4", 1), turns_of(&env, "BU-4", 2)),
        (1, 2)
    );
}
