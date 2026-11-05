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
