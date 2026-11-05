//! M17 Part A — the red-green proof gate (`docs/tasks/M17/TASK.md`): a check gate proves, per
//! acceptance criterion, that the named tests fail on the base commit with the task's test files
//! laid over it (red) and pass on the checkpoint (green). End to end through the `toryo` binary
//! with fake agents; the test runner is `git grep` (see `common::m17`), so no script runs.
#![cfg(feature = "m0-kit")]

mod common;

use common::m17::*;
use common::*;
use serde_json::{json, Value};

// ── A2: `## Proof exempt` (AC-5) ───────────────────────────────────────────────────────────

/// AC-5, edge case 6: `task create` refuses an unknown exempt id, a duplicate and an item without a
/// reason — with their line numbers, nothing created — and freezes a valid section with the task.
#[test]
fn m17_ac05_task_create_refuses_a_bad_proof_exempt_section() {
    let env = setup_rg(RG_SRC, "", &[]);
    let text = std::fs::read_to_string(env.repo.join("TASK.md")).unwrap();
    let with = |section: &str| format!("{text}\n## Proof exempt\n{section}");
    std::fs::write(
        env.repo.join("TASK-bad.md"),
        with("- AC-7: no such criterion\n- AC-2: the installer is checked by hand\n- AC-2: again\n- AC-3:\n"),
    )
    .unwrap();
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK-bad.md", "--id", "PE-1"],
        &[],
    );
    assert_ne!(code, 0, "{err}");
    let base = text.lines().count() + 2;
    for want in [
        format!("line {}: `AC-7` is not an acceptance criterion of this task (they are AC-1, AC-2, AC-3)", base + 1),
        format!("line {}: `AC-2` is listed twice", base + 3),
        format!("line {}: `AC-3` has no reason", base + 4),
    ] {
        assert!(err.contains(&want), "`{want}` not in {err}");
    }
    assert!(
        !db(&env)
            .call(|tx| Ok(toryo::engine::store::get_task(tx, "PE-1").is_ok()))
            .unwrap(),
        "no task"
    );
    // a valid section: created, and the snapshot holds it
    std::fs::write(
        env.repo.join("TASK-ok.md"),
        with("- AC-3: covered by the module manifest, no runnable test\n"),
    )
    .unwrap();
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK-ok.md", "--id", "PE-2"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "PE-2");
    let snap = std::fs::read_to_string(t.artifacts().join("TASK.md")).unwrap();
    let exempt = toryo::engine::redgreen::parse_proof_exempt(&snap, &t.acceptance_ids).unwrap();
    assert_eq!(exempt.len(), 1);
    assert_eq!(exempt[0].ac, "AC-3");
}

// ── A3: the proof map and the prompt (AC-3) ────────────────────────────────────────────────

/// The store's events of `kind` for task `id`.
fn store_events(env: &Env, id: &str, kind: &str) -> Vec<Value> {
    let i = id.to_string();
    db(env)
        .call(move |tx| toryo::engine::store::events(tx, &i))
        .unwrap()
        .into_iter()
        .filter(|e| e.1 == kind)
        .map(|e| e.2)
        .collect()
}

/// The prompts of the implementer's turns of the latest run of `id`.
fn implementer_prompts(env: &Env, id: &str) -> Vec<String> {
    calls(env, id)
        .into_iter()
        .filter(|c| {
            c["key"]
                .as_str()
                .is_some_and(|k| k.starts_with("implementer"))
        })
        .map(|c| c["prompt"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// AC-3, AC-13: without a red-green gate an implementation report without `proof` is accepted
/// (no schema violation), and the prompt says nothing about a proof map.
#[test]
fn m17_ac03_a_report_without_proof_passes_without_a_red_green_gate() {
    let env = setup_rg("", "", &[]);
    create(&env, "NP-1");
    let s = script(
        &env,
        "np.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "NP-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        task(&env, "NP-1").state,
        toryo::engine::PipelineState::Completed,
        "{err}"
    );
    assert!(
        store_events(&env, "NP-1", "schema_violation").is_empty(),
        "{err}"
    );
    assert!(
        !err.contains("does not match implementation.schema.json"),
        "{err}"
    );
    let p = implementer_prompts(&env, "NP-1");
    assert_eq!(p.len(), 1);
    assert!(!p[0].contains("Proof map"), "{}", p[0]);
    // and VERIFY_RESULT.json has no `red_green` key (AC-11, AC-13)
    assert!(verify_result(&env, "NP-1").get("red_green").is_none());
}

/// AC-3: with a red-green gate the implementer's prompt explains the proof rule — the gate, its
/// per-test command, the test globs and the exempt criteria with their reasons.
#[test]
fn m17_ac03_the_prompt_explains_the_proof_rule_with_a_red_green_gate() {
    let env = setup_rg(RG_SRC, "", &[]);
    let text = std::fs::read_to_string(env.repo.join("TASK.md")).unwrap();
    std::fs::write(
        env.repo.join("TASK.md"),
        format!("{text}\n## Proof exempt\n- AC-3: the export list is read by the reviewer\n"),
    )
    .unwrap();
    create(&env, "PR-1");
    let s = rg_script(
        &env,
        "pr.json",
        json!([
            {"ac": "AC-1", "tests": ["Divide-Numbers"]},
            {"ac": "AC-2", "tests": ["Division"]},
        ]),
    );
    let (code, err) = toryo(&env, &["run", "PR-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let p = implementer_prompts(&env, "PR-1");
    assert_eq!(p.len(), 1, "{err}");
    for want in [
        "## Proof map (red-green gate `divide-present`)",
        "every criterion of TASK.md (AC-1, AC-2, AC-3) except the exempt ones",
        "with `git grep -q -F -e {test} -- src`",
        "(the files matching `tests/**`)",
        "it must FAIL",
        "at most 32 distinct ids",
        "Exempt (no test required; a test you list for one still runs):\n- AC-3: the export list is read by the reviewer\n",
    ] {
        assert!(p[0].contains(want), "`{want}` not in\n{}", p[0]);
    }
    assert!(store_events(&env, "PR-1", "schema_violation").is_empty());
}

/// X48: one red-green gate per configuration — two in `before_handoff` are refused at load.
#[test]
fn m17_x48_two_red_green_gates_are_refused() {
    let org = org_rg(RG_SRC, "").replace(
        "  divide-present:",
        "  unit: { kind: check, program: git, args: [status], timeout: 5m, red_green: { tests: [\"tests/**\"], args: [\"{test}\"] } }\n  divide-present:",
    );
    let env = setup_rg(
        "",
        "",
        &[
            (".toryo/toryo-orgchart.yaml", &org),
            (
                ".toryo/roles/implementer/toryo-role.yaml",
                "toryo: 1\npurpose: Change the code.\nbefore_handoff: [divide-present, unit]\n",
            ),
        ],
    );
    let (code, _, err) = run_out(&env, &["config", "show"]);
    assert_ne!(code, 0);
    assert!(
        err.contains("red_green: 2 gates have a `red_green` block (divide-present, unit)"),
        "{err}"
    );
}

// ── A1: configuration and load refusals (AC-1, AC-2) ──────────────────────────────────────

/// AC-1: a `kind: check` gate of the org chart accepts the block; `config show` (text and
/// JSON), `gate list` and `doctor` show it; `gate run` runs only the gate's own args and says
/// that red-green runs only inside a task.
#[test]
fn m17_ac01_an_org_chart_gate_takes_red_green_and_shows_it() {
    let env = setup_rg(
        r#"red_green: { tests: ["tests/**"], args: [grep, -q, -F, -e, "{test}", --, src], green_output_must_match: '.*', max_tests: 8 }"#,
        "",
        &[],
    );
    let (code, out, err) = run_out(&env, &["config", "show"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(
        out.contains("red-green (inside a task only): per test `git grep -q -F -e {test} -- src` · tests tests/** · at most 8 test ids · a green run must match `.*`"),
        "{out}"
    );
    let (code, out, err) = run_out(&env, &["config", "show", "--json"]);
    assert_eq!(code, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    let gate = v["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["name"] == "divide-present")
        .unwrap();
    assert_eq!(gate["red_green"]["tests"], json!(["tests/**"]));
    assert_eq!(gate["red_green"]["max_tests"], 8);
    assert_eq!(gate["red_green"]["green_output_must_match"], ".*");
    let (code, out, err) = run_out(&env, &["gate", "list"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains(
            "      red-green (inside a task only): per test `git grep -q -F -e {test} -- src`"
        ),
        "{out}"
    );
    let (_, out, err) = run_out(&env, &["doctor"]);
    assert!(
        format!("{out}{err}")
            .contains("check gate `divide-present`: red-green (inside a task only)"),
        "{out}\n{err}"
    );
    // `gate run` runs the gate's own args only (the base has no division: exit 1) and says so
    let (code, out, err) = run_out(&env, &["gate", "run", "divide-present"]);
    assert_eq!(code, 1, "{out}\n{err}");
    assert!(
        err.contains("note: `divide-present` has a `red_green` block — red-green runs only inside a task (`toryo run`); this runs the gate's own args"),
        "{err}"
    );
    assert!(out.contains("git grep -q Divide-Numbers -- src"), "{out}");
}

/// AC-1, edge case 18: a `commands.verify` entry (toryo.yaml-only mode) takes the block too.
#[test]
fn m17_ec18_a_toryo_yaml_verify_command_takes_red_green() {
    let env = setup("codex-openai");
    let cfg = std::fs::read_to_string(&env.config).unwrap();
    let item = r#"{ name: divide-present, program: git, args: ["grep", "-q", "Divide-Numbers", "--", "src"] }"#;
    assert!(cfg.contains(item));
    std::fs::write(
        &env.config,
        cfg.replace(
            item,
            r#"{ name: divide-present, program: git, args: ["grep", "-q", "Divide-Numbers", "--", "src"], red_green: { tests: ["tests/**"], args: [grep, -q, -F, -e, "{test}", --, src] } }"#,
        ),
    )
    .unwrap();
    let (code, out, err) = run_out(&env, &["config", "show"]);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(
        out.contains("red-green (inside a task only): per test `git grep -q -F -e {test} -- src` · tests tests/** · at most 32 test ids · no green output rule"),
        "{out}"
    );
    let (code, out, _) = run_out(&env, &["gate", "list"]);
    assert_eq!(code, 0);
    assert!(out.contains("red-green (inside a task only)"), "{out}");
    // AC-2 in this mode: the refusal names the entry and the key
    std::fs::write(
        &env.config,
        cfg.replace(
            item,
            r#"{ name: divide-present, program: git, args: ["grep", "-q", "Divide-Numbers", "--", "src"], red_green: { tests: ["tests/**"], args: [grep, src], max_tests: 99 } }"#,
        ),
    )
    .unwrap();
    let (code, _, err) = run_out(&env, &["config", "show"]);
    assert_ne!(code, 0);
    assert!(
        err.contains("commands.verify[0] (divide-present).red_green.args: `{test}` is missing")
            && err.contains(
                "commands.verify[0] (divide-present).red_green.max_tests: `99` is out of range"
            ),
        "{err}"
    );
}

/// AC-2: every refusal names the gate and the key, and nothing loads.
#[test]
fn m17_ac02_load_refuses_a_bad_red_green_block_naming_gate_and_key() {
    let env = setup_rg("", "", &[]);
    let org_path = env.repo.join(".toryo/toryo-orgchart.yaml");
    let cases: Vec<(String, &[&str])> = vec![
        (
            org_rg(r#"red_green: { tests: [], args: ["{test}"] }"#, ""),
            &["gates.divide-present.red_green.tests: empty"],
        ),
        (
            org_rg(r#"red_green: { tests: ["tests/**"], args: [grep, src] }"#, ""),
            &["gates.divide-present.red_green.args: `{test}` is missing"],
        ),
        (
            org_rg(r#"red_green: { tests: ["tests/**"], args: ["{test}", "x{test}"] }"#, ""),
            &["gates.divide-present.red_green.args: `{test}` appears 2 times"],
        ),
        (
            org_rg(r#"red_green: { tests: ["/abs/**"], args: ["{test}"] }"#, ""),
            &["gates.divide-present.red_green.tests: `/abs/**` is not a valid glob"],
        ),
        (
            org_rg(r#"red_green: { tests: ["tests/{a,b}/**"], args: ["{test}"] }"#, ""),
            &["gates.divide-present.red_green.tests: `tests/{a,b}/**` is not a valid glob"],
        ),
        (
            org_rg(
                r#"red_green: { tests: ["tests/**"], args: ["{test}"], green_output_must_match: "(" }"#,
                "",
            ),
            &["gates.divide-present.red_green.green_output_must_match: `(` is not a valid regular expression"],
        ),
        (
            org_rg(r#"red_green: { tests: ["tests/**"], args: ["{test}"], max_tests: 0 }"#, ""),
            &["gates.divide-present.red_green.max_tests: `0` is out of range (1..=64)"],
        ),
        (
            org_rg(r#"red_green: { tests: ["tests/**"], args: ["{test}"], max_tests: 65 }"#, ""),
            &["gates.divide-present.red_green.max_tests: `65` is out of range (1..=64)"],
        ),
        (
            org_rg(r#"red_green: { tests: ["tests/**"], args: ["{test}"], extra: 1 }"#, ""),
            &["gates.divide-present.red_green.extra: unknown key 'extra'"],
        ),
        (
            org_rg(r#"red_green: { tests: ["tests/**"], args: ["{test}"] }"#, "")
                .replace("program: git", "program: \"{test}\""),
            &["gates.divide-present.program: `{test}` may not appear in `program`"],
        ),
        (
            org_rg("", "").replace(
                "  divide-present:",
                "  deploy: { kind: environment, timeout: 5m, if_missing: skip, red_green: { tests: [\"tests/**\"], args: [\"{test}\"] } }\n  divide-present:",
            ),
            &["gates.deploy.red_green: only a `kind: check` gate can prove its tests red-green"],
        ),
        (
            org_rg("", "").replace(
                "  divide-present:",
                "  xbuild: { kind: check, tool: xpp, metadata: Metadata, projects: [p.rnrproj], timeout: 5m, red_green: { tests: [\"tests/**\"], args: [\"{test}\"] } }\n  divide-present:",
            ),
            &["gates.xbuild.red_green: not with `tool: xpp`"],
        ),
    ];
    for (org, want) in cases {
        std::fs::write(&org_path, &org).unwrap();
        let (code, out, err) = run_out(&env, &["config", "show"]);
        assert_ne!(code, 0, "loaded:\n{org}\n{out}");
        for w in want {
            assert!(
                err.contains(w),
                "`{w}` not in:\n{err}\n--- org chart:\n{org}"
            );
        }
    }
}

/// AC-2: `red_green.args` gets the M18 entry-script protection of `args` — a script it runs is
/// refused at `task create` when the base commit lacks it, and protected when it is there; the
/// rules of `cmdline.rs` are only called.
#[test]
fn m17_ac02_red_green_args_get_the_entry_script_protection() {
    let rg = r#"red_green: { tests: ["tests/**"], args: [scripts/one-test.sh, "{test}"] }"#;
    let org = org_rg(rg, "").replace(
        "divide-present: { kind: check, program: git, args: [grep, -q, Divide-Numbers, --, src]",
        "divide-present: { kind: check, program: sh, args: [scripts/gate.sh]",
    );
    let env = setup_rg(
        "",
        "",
        &[
            (".toryo/toryo-orgchart.yaml", &org),
            ("scripts/gate.sh", "exit 0\n"),
        ],
    );
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "RG-M18"],
        &[],
    );
    assert_ne!(code, 0, "{err}");
    assert!(
        err.contains("verify_script_missing") && err.contains("scripts/one-test.sh"),
        "{err}"
    );
    // committed: admitted, and protected like the gate's own script
    commit_repo(
        &env,
        &[("scripts/one-test.sh", "exit 1\n")],
        "the per-test script",
    );
    create(&env, "RG-M18");
    let t = task(&env, "RG-M18");
    let cfg = toryo::engine::task::frozen_config(&t).unwrap();
    let (_, prot, _) = toryo::engine::runner::task_policy(&t, &cfg).unwrap();
    assert!(prot.violates("scripts/one-test.sh"), "protected");
    assert!(
        prot.violates("scripts/gate.sh"),
        "the gate's own script too"
    );
    assert!(
        !prot.violates("{test}") && !prot.violates(""),
        "`{{test}}` names no file"
    );
}

// ── A4–A7: the red-green run (AC-4, AC-6 … AC-11) ─────────────────────────────────────────

/// The verify results of a task, oldest first (every version of `verify-result`).
fn verify_results(env: &Env, id: &str) -> Vec<Value> {
    versions(env, id, "verify-result")
}

/// The `verify_command` events of a task's flight recorder.
fn gate_runs(env: &Env, id: &str) -> Vec<Value> {
    events(env, id)
        .into_iter()
        .filter(|e| e["type"] == "verify_command")
        .collect()
}

/// AC-6, AC-7, AC-11, edge cases 5 and 20: every criterion proven. One run per distinct test and
/// side (`Divide-Numbers` proves AC-1 and AC-3 — edge case 5); the red commit is the base tree
/// plus the test file only, its parent is the base, it is on no ref (edge case 20); the result is
/// in VERIFY_RESULT.json (relative paths only), `red_green_finished` is recorded and printed by
/// `watch`; `status`, `task show --json`, the reviewer's prompt and the final gate show it;
/// `task cleanup` leaves no red checkout behind.
#[test]
fn m17_ac06_ac07_ac11_a_red_green_gate_proves_every_criterion() {
    let env = setup_rg(RG_SRC, "", &[]);
    create(&env, "RG-1");
    let refs_before = refs(&env.repo);
    let s = rg_script(&env, "rg.json", full_proof());
    let (code, err) = toryo(&env, &["run", "RG-1", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = toryo(
        &env,
        &[
            "approve",
            "RG-1",
            "--gate",
            "plan",
            "--continue",
            "--fake-agents",
            &s,
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "RG-1");
    assert_eq!(
        t.state,
        toryo::engine::PipelineState::WaitingFinalApproval,
        "{err}"
    );
    // the console: one line per run and side, then the summary — and the final gate's note
    for want in [
        "  ✔ divide-present Divide-Numbers (exit 0, ",
        "s) — green side",
        "s) — red side: fails without the change",
        "  ✔ red-green divide-present: 3 of 3 acceptance criteria proven",
        "final gate — red-green: 3 of 3 acceptance criteria proven red-green",
    ] {
        assert!(err.contains(want), "`{want}` not in\n{err}");
    }
    assert!(!err.contains("full output:"), "no run went wrong: {err}");
    let vr = verify_result(&env, "RG-1");
    assert_eq!(vr["pass"], true);
    let rg = &vr["red_green"];
    assert_eq!(rg["gate"], "divide-present");
    assert_eq!(rg["base"], t.base_commit.as_str());
    assert_eq!(
        (
            rg["proven"].clone(),
            rg["total"].clone(),
            rg["exempt"].clone(),
            rg["pass"].clone()
        ),
        (json!(3), json!(3), json!(0), json!(true))
    );
    assert_eq!(rg["overlay"], json!(["tests/Divide.Tests.ps1"]));
    assert_eq!(rg["overlay_count"], 1);
    for a in rg["acs"].as_array().unwrap() {
        for x in a["tests"].as_array().unwrap() {
            assert_eq!(x["green"]["exit"], 0, "{a}");
            assert_eq!(x["red"]["exit"], 1, "{a}");
            assert_eq!(x["red"]["timed_out"], false, "{a}");
        }
    }
    // relative paths only: no absolute path of this machine in the red-green result
    let text = rg.to_string();
    assert!(
        !text.contains(&env.root.display().to_string().replace('\\', "\\\\")),
        "{text}"
    );
    assert!(!text.contains(&fwd(&env.root)), "{text}");
    // one run per distinct id and side (edge case 5)
    let runs = gate_runs(&env, "RG-1");
    let count = |name: &str, side: &str| {
        runs.iter()
            .filter(|e| e["name"] == name && e["side"] == side)
            .count()
    };
    assert_eq!(count("divide-present Divide-Numbers", "green"), 1);
    assert_eq!(count("divide-present Divide-Numbers", "red"), 1);
    assert_eq!(count("divide-present Division", "red"), 1);
    // the red commit: base + the checkpoint's test file; parent = base; on no ref
    let red = rg["red_commit"].as_str().unwrap();
    let wt = wt(&env, "RG-1");
    assert_eq!(
        git(&wt, &["rev-parse", &format!("{red}^")]).trim(),
        t.base_commit
    );
    assert_eq!(
        git(&wt, &["rev-list", "--parents", "-n", "1", red])
            .split_whitespace()
            .count(),
        2,
        "one parent"
    );
    let files = git(&wt, &["ls-tree", "-r", "--name-only", red]);
    assert!(
        files.lines().any(|l| l == "tests/Divide.Tests.ps1"),
        "{files}"
    );
    assert_eq!(
        git(&wt, &["rev-parse", &format!("{red}:src/Calculator.psm1")]),
        git(
            &wt,
            &[
                "rev-parse",
                &format!("{}:src/Calculator.psm1", t.base_commit)
            ]
        ),
        "src is the base's"
    );
    assert_eq!(
        git(
            &wt,
            &["rev-parse", &format!("{red}:tests/Divide.Tests.ps1")]
        ),
        git(&wt, &["rev-parse", "HEAD:tests/Divide.Tests.ps1"]),
        "the test file is the checkpoint's, byte for byte"
    );
    assert_eq!(refs(&env.repo), refs_before, "no new ref (edge case 20)");
    assert!(!git(&env.repo, &["branch", "-a", "--contains", red]).contains("toryo"));
    // the flight recorder and `watch`
    let ev = events(&env, "RG-1");
    let fin: Vec<&Value> = ev
        .iter()
        .filter(|e| e["type"] == "red_green_finished")
        .collect();
    assert_eq!(fin.len(), 1);
    assert_eq!(
        (
            fin[0]["gate"].clone(),
            fin[0]["proven"].clone(),
            fin[0]["total"].clone(),
            fin[0]["pass"].clone()
        ),
        (json!("divide-present"), json!(3), json!(3), json!(true))
    );
    let (_, out, _) = run_out(&env, &["watch", "RG-1", "--from-start", "--no-follow"]);
    assert!(
        out.contains("✔ red-green divide-present: 3 of 3 acceptance criteria proven"),
        "{out}"
    );
    // `status` and `task show --json`
    let (_, out, _) = run_out(&env, &["status", "RG-1"]);
    assert!(
        out.contains("red-green: 3 of 3 acceptance criteria proven red-green"),
        "{out}"
    );
    let (_, out, _) = run_out(&env, &["task", "show", "RG-1", "--json"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        v["red_green"],
        json!({"proven": 3, "total": 3, "exempt": 0, "pass": true})
    );
    let (_, out, _) = run_out(&env, &["status", "--json"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["tasks"][0]["red_green"]["proven"], 3);
    // the reviewer's prompt: the evidence per criterion, as data
    let review = calls(&env, "RG-1")
        .into_iter()
        .find(|c| c["key"] == "reviewer")
        .unwrap();
    let p = review["prompt"].as_str().unwrap();
    assert!(
        p.contains("## Red-green evidence (gate `divide-present`"),
        "{p}"
    );
    assert!(
        p.contains("answer every acceptance criterion yourself"),
        "{p}"
    );
    assert!(p.contains("<untrusted_data source=\"red-green\">"), "{p}");
    assert!(
        p.contains("- AC-1: proven — `Divide-Numbers`: green exit 0, red exit 1"),
        "{p}"
    );
    // `task cleanup` leaves no red checkout behind (edge case 20)
    let (code, err) = toryo(&env, &["approve", "RG-1", "--gate", "final"], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = toryo(&env, &["task", "cleanup", "RG-1"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(!env.root.join("verify").join("RG-1").exists());
    assert!(!git(&env.repo, &["worktree", "list"]).contains("RG-1.red"));
}

fn fwd(p: &std::path::Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// The fake turns of a task whose first implementer turn returns `first` and whose fix turn
/// returns `second` as `proof`.
fn two_turns(env: &Env, name: &str, first: Value, second: Value) -> String {
    script(
        env,
        name,
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![
                    write("src/Calculator.psm1", DIVIDE),
                    write("tests/Divide.Tests.ps1", "Describe 'Divide' { }\n"),
                    implementation_with(first),
                ]),
                turn(vec![write("tests/Divide.Tests.ps1", "# v2\n"), implementation_with(second)]),
            ],
            "reviewer": [turn(vec![approved()])],
        }),
    )
}

/// Edge cases 1 and 14, AC-8, AC-10: a declared test that passes on the red commit (the feature
/// existed: `Add-Numbers` is in the base) fails the gate — "passes without the change — it proves
/// nothing", naming the test, the criterion, the side, the exit code and the output tail — as one
/// gate retry. The fix cycle's checkpoint gets a new red commit; the old one is not reused.
#[test]
fn m17_ec01_ec14_a_test_that_passes_on_red_proves_nothing_and_the_fix_gets_a_new_red_commit() {
    let env = setup_rg(RG_SRC, "", &[]);
    create(&env, "RG-2");
    let bad = json!([
        {"ac": "AC-1", "tests": ["Add-Numbers"]},
        {"ac": "AC-2", "tests": ["Division"]},
        {"ac": "AC-3", "tests": ["Divide-Numbers"]},
    ]);
    let s = two_turns(&env, "rg2.json", bad, full_proof());
    let (code, err) = toryo(&env, &["run", "RG-2", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "RG-2");
    assert_eq!(t.state, toryo::engine::PipelineState::Completed, "{err}");
    assert_eq!(t.fix_cycle, 1, "one gate retry");
    assert!(
        err.contains("  ✘ divide-present Add-Numbers (exit 0, "),
        "{err}"
    );
    assert!(
        err.contains("— red side: passes without the change"),
        "{err}"
    );
    let vrs = verify_results(&env, "RG-2");
    assert_eq!(vrs.len(), 2);
    assert_eq!(vrs[0]["pass"], false);
    assert_eq!(vrs[0]["red_green"]["pass"], false);
    assert_eq!(
        vrs[0]["red_green"]["proven"], 2,
        "AC-2 and AC-3 were proven"
    );
    assert_eq!(vrs[1]["red_green"]["pass"], true);
    let (r1, r2) = (
        vrs[0]["red_green"]["red_commit"].as_str().unwrap(),
        vrs[1]["red_green"]["red_commit"].as_str().unwrap(),
    );
    assert_ne!(
        r1, r2,
        "a new red commit for the new checkpoint (edge case 14)"
    );
    assert_ne!(vrs[0]["head"], vrs[1]["head"]);
    // the fix prompt: data, naming test, criterion, side, exit code, output tail
    let fix = implementer_prompts(&env, "RG-2").pop().unwrap();
    let block = fix
        .split("<untrusted_data source=\"verify\">\n")
        .nth(1)
        .unwrap();
    assert!(
        block.starts_with("- RED-GREEN FAILED: gate `divide-present`, red side (the base commit with only your test files laid over it): it must fail.\n  - test `Add-Numbers` (AC-1) passes without the change — it proves nothing (exit 0). Last output:\n```"),
        "{block}"
    );
}

/// AC-4, edge case 3 (end to end): a proof map that is not valid — here every refused test id at
/// once, and AC-2 and AC-3 without a test — fails the gate with one feedback naming each problem;
/// no process starts, not even the gate's own args. The fix turn's valid map passes.
#[test]
fn m17_ac04_ec03_an_invalid_proof_map_starts_no_process() {
    let env = setup_rg(RG_SRC, "", &[]);
    create(&env, "RG-3");
    let long = "a".repeat(201);
    let bad_ids = json!([
        "--nocapture",
        "/x",
        "a b",
        "a;b",
        "a&b",
        "a|b",
        "%PATH%",
        "$(x)",
        "a`b",
        "a\"b",
        "a\nb",
        "café",
        long,
        ""
    ]);
    let bad = json!([{"ac": "AC-1", "tests": bad_ids}]);
    let s = two_turns(&env, "rg3.json", bad, full_proof());
    let (code, err) = toryo(&env, &["run", "RG-3", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        task(&env, "RG-3").state,
        toryo::engine::PipelineState::Completed,
        "{err}"
    );
    let vrs = verify_results(&env, "RG-3");
    let first = &vrs[0];
    assert_eq!(first["pass"], false);
    assert_eq!(first["commands"], json!([]), "no gate ran: {first}");
    assert!(first["red_green"]["red_commit"].is_null());
    let problems = first["red_green"]["problems"].to_string();
    for want in [
        "starts with `-`",
        "starts with `/`",
        "' '",
        "';'",
        "'&'",
        "'|'",
        "'%'",
        "'$'",
        "'`'",
        "'\\\"'",
        "'\\\\n'",
        "the non-ASCII character",
        "201 bytes long",
        "an empty test id",
        "no test proves `AC-2`, `AC-3`",
    ] {
        assert!(problems.contains(want), "`{want}` not in {problems}");
    }
    // no process of the first verify: its only events are the second verify's
    let first_verify_runs = gate_runs(&env, "RG-3")
        .into_iter()
        .filter(|e| e["name"] == "divide-present")
        .count();
    assert_eq!(
        first_verify_runs, 1,
        "the gate's own args ran once — in the second verify"
    );
    let fix = implementer_prompts(&env, "RG-3").pop().unwrap();
    assert!(
        fix.contains("- RED-GREEN FAILED: gate `divide-present` did not run — the proof map (`proof` in IMPLEMENTATION.json) is not valid:"),
        "{fix}"
    );
}

/// Edge case 2: a filter that matches no test. The test id is a pathspec of `git grep` that
/// matches no file, so it filters nothing: green exits 0 and the red commit (no division) fails —
/// without `green_output_must_match` the gate passes (the README warns about this); with it, the
/// green run's empty output does not match and the gate fails on the green side.
#[test]
fn m17_ec02_a_filter_that_matches_no_test_needs_the_output_rule() {
    let rg = r#"red_green: { tests: ["tests/**"], args: [grep, -q, -F, -e, Divide-Numbers, --, src, "{test}"] }"#;
    let env = setup_rg(rg, "", &[]);
    create(&env, "RG-4");
    let proof = json!([
        {"ac": "AC-1", "tests": ["no-such-test"]},
        {"ac": "AC-2", "tests": ["no-such-test"]},
        {"ac": "AC-3", "tests": ["no-such-test"]},
    ]);
    let s = rg_script(&env, "rg4.json", proof.clone());
    let (code, err) = toryo(&env, &["run", "RG-4", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let vr = verify_result(&env, "RG-4");
    assert_eq!(
        vr["red_green"]["pass"], true,
        "vacuous without the rule: {vr}"
    );
    assert_eq!(vr["red_green"]["acs"][0]["tests"][0]["green"]["exit"], 0);
    // with the rule
    let rg = r#"red_green: { tests: ["tests/**"], args: [grep, -q, -F, -e, Divide-Numbers, --, src, "{test}"], green_output_must_match: '\S' }"#;
    let env = setup_rg(rg, "", &[]);
    create(&env, "RG-5");
    let s = script(
        &env,
        "rg5.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [
                turn(vec![write("src/Calculator.psm1", DIVIDE), implementation_with(proof)]),
            ],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "RG-5", "--yes", "--fake-agents", &s], &[]);
    let vr = verify_result(&env, "RG-5");
    assert_eq!(vr["red_green"]["pass"], false, "{err}");
    assert!(
        vr["red_green"]["red_commit"].is_null(),
        "red never ran: {vr}"
    );
    let fb = task(&env, "RG-5").feedback.unwrap_or_default();
    assert!(
        fb.contains("green side (your checkpoint): it must pass")
            && fb.contains("exited 0, but its output does not match `green_output_must_match` (`\\S`) — did it run the test at all?"),
        "{fb}"
    );
}

/// Edge case 7: globs that are too wide (`src/**`) lay the change itself over the base — red is
/// green, and the gate fails "proves nothing".
#[test]
fn m17_ec07_globs_that_are_too_wide_prove_nothing() {
    let rg = r#"red_green: { tests: ["src/**", "tests/**"], args: [grep, -q, -F, -e, "{test}", --, src] }"#;
    let env = setup_rg(rg, "", &[]);
    create(&env, "RG-6");
    let s = rg_script(&env, "rg6.json", full_proof());
    let (_, err) = toryo(&env, &["run", "RG-6", "--yes", "--fake-agents", &s], &[]);
    let vr = verify_result(&env, "RG-6");
    assert_eq!(vr["red_green"]["pass"], false, "{err}");
    assert_eq!(vr["red_green"]["proven"], 0);
    assert!(
        vr["red_green"]["overlay"]
            .as_array()
            .unwrap()
            .contains(&json!("src/Calculator.psm1")),
        "{vr}"
    );
    assert!(
        task(&env, "RG-6")
            .feedback
            .unwrap_or_default()
            .contains("passes without the change — it proves nothing"),
        "{err}"
    );
}

/// Edge case 8: an empty overlay — the test file is already in the base and unchanged (a bug-fix
/// task) — passes: the red commit is the base tree.
#[test]
fn m17_ec08_an_empty_overlay_passes() {
    let env = setup_rg(
        RG_SRC,
        "",
        &[("tests/Divide.Tests.ps1", "Describe 'Divide' { }\n")],
    );
    create(&env, "RG-7");
    let s = script(
        &env,
        "rg7.json",
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation_with(full_proof())])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "RG-7", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let vr = verify_result(&env, "RG-7");
    let rg = &vr["red_green"];
    assert_eq!(rg["pass"], true, "{vr}");
    assert_eq!(rg["overlay_count"], 0);
    let t = task(&env, "RG-7");
    let red = rg["red_commit"].as_str().unwrap();
    assert_eq!(
        git(
            &wt(&env, "RG-7"),
            &["rev-parse", &format!("{red}^{{tree}}")]
        ),
        git(
            &wt(&env, "RG-7"),
            &["rev-parse", &format!("{}^{{tree}}", t.base_commit)]
        ),
        "the red tree is the base tree"
    );
}

/// Edge case 17: a task without a red-green gate whose implementer still writes `proof` — ignored:
/// no error, no schema violation, no `red_green` key.
#[test]
fn m17_ec17_a_proof_without_a_red_green_gate_is_ignored() {
    let env = setup_rg("", "", &[]);
    create(&env, "RG-8");
    let s = rg_script(&env, "rg8.json", full_proof());
    let (code, err) = toryo(&env, &["run", "RG-8", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        task(&env, "RG-8").state,
        toryo::engine::PipelineState::Completed
    );
    assert!(store_events(&env, "RG-8", "schema_violation").is_empty());
    assert!(verify_result(&env, "RG-8").get("red_green").is_none());
    assert!(events(&env, "RG-8")
        .iter()
        .all(|e| e["type"] != "red_green_finished"));
    let (_, out, _) = run_out(&env, &["task", "show", "RG-8", "--json"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v["red_green"].is_null(), "{v}");
}

/// Edge case 18 (end to end): a red-green `commands.verify` entry in toryo.yaml-only mode.
#[test]
fn m17_ec18_a_toryo_yaml_red_green_command_runs_end_to_end() {
    let env = setup("codex-openai");
    let cfg = std::fs::read_to_string(&env.config).unwrap();
    std::fs::write(
        &env.config,
        cfg.replace(
            r#"{ name: divide-present, program: git, args: ["grep", "-q", "Divide-Numbers", "--", "src"] }"#,
            r#"{ name: divide-present, program: git, args: ["grep", "-q", "Divide-Numbers", "--", "src"], red_green: { tests: ["tests/**"], args: [grep, -q, -F, -e, "{test}", --, src] } }"#,
        ),
    )
    .unwrap();
    create(&env, "RG-9");
    let s = rg_script(&env, "rg9.json", full_proof());
    let (code, err) = toryo(&env, &["run", "RG-9", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        task(&env, "RG-9").state,
        toryo::engine::PipelineState::Completed,
        "{err}"
    );
    assert_eq!(verify_result(&env, "RG-9")["red_green"]["proven"], 3);
}

/// A repository for the red-commit builder: Toryo's `GitCmd` over a fresh `git init`.
fn lib_repo() -> (tempfile::TempDir, toryo::git::GitCmd, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let g = toryo::git::GitCmd::new("git", d.path().join("hooks")).unwrap();
    let r = d.path().join("repo");
    std::fs::create_dir_all(&r).unwrap();
    git(&r, &["init", "-q", "-b", "main"]);
    (d, g, r)
}

fn commit_all(r: &std::path::Path, msg: &str) -> String {
    git(r, &["add", "-A"]);
    git(
        r,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            msg,
        ],
    );
    git(r, &["rev-parse", "HEAD"]).trim().to_string()
}

/// Edge case 7 (the overlay, AC-7): a test file added, modified, deleted and renamed on the
/// checkpoint is laid over the base exactly so — the rename is the old path deleted and the new
/// one added; a change outside the test globs is not overlaid; the parent is the base; no ref.
#[test]
fn m17_ec07_the_overlay_takes_added_modified_deleted_and_renamed_test_files() {
    let (d, g, r) = lib_repo();
    for (p, c) in [
        ("tests/keep.txt", "v1\n"),
        ("tests/del.txt", "gone soon\n"),
        ("tests/old.txt", "moved\n"),
        ("src/a.txt", "base\n"),
    ] {
        std::fs::create_dir_all(r.join(p).parent().unwrap()).unwrap();
        std::fs::write(r.join(p), c).unwrap();
    }
    let base = commit_all(&r, "base");
    std::fs::write(r.join("tests/keep.txt"), "v2\n").unwrap();
    std::fs::remove_file(r.join("tests/del.txt")).unwrap();
    std::fs::rename(r.join("tests/old.txt"), r.join("tests/new.txt")).unwrap();
    std::fs::write(r.join("tests/added.txt"), "new test\n").unwrap();
    std::fs::write(r.join("src/a.txt"), "the change\n").unwrap();
    std::fs::create_dir_all(r.join("docs")).unwrap();
    std::fs::write(r.join("docs/x.md"), "doc\n").unwrap();
    let head = commit_all(&r, "checkpoint");
    let refs_before = refs(&r);
    let globs = toryo::engine::checks::test_globs(&["tests/**".to_string()]).unwrap();
    let red = toryo::engine::redgreen::red_commit(
        &g,
        &toryo::git::Loc::Discover(r.clone()),
        &base,
        &head,
        &globs,
        &d.path().join("red.index"),
        "red",
    )
    .unwrap();
    assert_eq!(
        red.overlay,
        [
            "tests/added.txt",
            "tests/del.txt",
            "tests/keep.txt",
            "tests/new.txt",
            "tests/old.txt"
        ]
    );
    let files: Vec<String> = git(&r, &["ls-tree", "-r", "--name-only", &red.sha])
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        files,
        [
            "src/a.txt",
            "tests/added.txt",
            "tests/keep.txt",
            "tests/new.txt"
        ]
    );
    let show = |what: &str| git(&r, &["show", &format!("{}:{what}", red.sha)]);
    assert_eq!(show("tests/keep.txt"), "v2\n");
    assert_eq!(show("src/a.txt"), "base\n", "not a test file: the base's");
    assert_eq!(
        git(&r, &["rev-parse", &format!("{}^", red.sha)]).trim(),
        base
    );
    assert_eq!(refs(&r), refs_before, "on no ref");
    assert!(
        !d.path().join("red.index").exists(),
        "the private index is gone"
    );
    // an empty overlay (edge case 8): the base tree
    let none = toryo::engine::checks::test_globs(&["spec/**".to_string()]).unwrap();
    let red = toryo::engine::redgreen::red_commit(
        &g,
        &toryo::git::Loc::Discover(r.clone()),
        &base,
        &head,
        &none,
        &d.path().join("red.index"),
        "red",
    )
    .unwrap();
    assert!(red.overlay.is_empty());
    assert_eq!(
        git(&r, &["rev-parse", &format!("{}^{{tree}}", red.sha)]),
        git(&r, &["rev-parse", &format!("{base}^{{tree}}")])
    );
}

/// Edge case 9: CRLF bytes in a test file under `eol=crlf`, and an executable bit set on the
/// checkpoint — the red commit holds the checkpoint's blob and mode, byte for byte (it never
/// goes through a working tree or a filter).
#[test]
fn m17_ec09_crlf_bytes_and_the_executable_bit_are_byte_exact() {
    let (d, g, r) = lib_repo();
    std::fs::write(r.join(".gitattributes"), "*.txt eol=crlf\n").unwrap();
    std::fs::create_dir_all(r.join("tests")).unwrap();
    std::fs::write(r.join("tests/run.sh"), "echo run\n").unwrap();
    let base = commit_all(&r, "base");
    // a blob with CRLF bytes, written as it is (no filter), and the executable bit
    let crlf = r.join("crlf-bytes");
    std::fs::write(&crlf, b"line one\r\nline two\r\n").unwrap();
    let blob = git(
        &r,
        &["hash-object", "-w", "--no-filters", crlf.to_str().unwrap()],
    )
    .trim()
    .to_string();
    std::fs::remove_file(&crlf).unwrap();
    git(
        &r,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{blob},tests/win.txt"),
        ],
    );
    git(&r, &["update-index", "--chmod=+x", "tests/run.sh"]);
    git(
        &r,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "checkpoint",
        ],
    );
    let head = git(&r, &["rev-parse", "HEAD"]).trim().to_string();
    let globs = toryo::engine::checks::test_globs(&["tests/**".to_string()]).unwrap();
    let red = toryo::engine::redgreen::red_commit(
        &g,
        &toryo::git::Loc::Discover(r.clone()),
        &base,
        &head,
        &globs,
        &d.path().join("red.index"),
        "red",
    )
    .unwrap();
    let entry = |c: &str, p: &str| git(&r, &["ls-tree", c, "--", p]);
    assert_eq!(
        entry(&red.sha, "tests/win.txt"),
        entry(&head, "tests/win.txt")
    );
    assert_eq!(
        entry(&red.sha, "tests/run.sh"),
        entry(&head, "tests/run.sh")
    );
    assert!(
        entry(&red.sha, "tests/run.sh").starts_with("100755 "),
        "the executable bit"
    );
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(&r)
        .args(["cat-file", "blob", &format!("{}:tests/win.txt", red.sha)])
        .output()
        .unwrap();
    assert_eq!(o.stdout, b"line one\r\nline two\r\n", "CRLF bytes kept");
}

/// Edge case 10, AC-8: a red run that runs into the gate's timeout counts as red — `timed_out:
/// true` in VERIFY_RESULT.json — and the reviewer's evidence flags it.
#[test]
fn m17_ec10_a_red_run_that_times_out_counts_as_red_and_is_flagged() {
    let env = setup_rg("", "", &[]);
    let runner = rg_runner(
        &env.root,
        Runner {
            slow: true,
            ..Default::default()
        },
    );
    std::fs::write(env.root.join("SLOW"), "1").unwrap();
    let org = org_runner(&runner, "10s", "");
    commit_repo(
        &env,
        &[(".toryo/toryo-orgchart.yaml", &org)],
        "the runner gate",
    );
    create(&env, "RG-10");
    let s = rg_script(
        &env,
        "rg10.json",
        json!([{"ac": "AC-1", "tests": ["Divide-Numbers"]}, {"ac": "AC-2", "tests": ["Division"]}, {"ac": "AC-3", "tests": ["Divide-Numbers"]}]),
    );
    let (code, err) = toryo(&env, &["run", "RG-10", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let vr = verify_result(&env, "RG-10");
    assert_eq!(vr["red_green"]["pass"], true, "{err}");
    let t0 = &vr["red_green"]["acs"][0]["tests"][0];
    assert_eq!(t0["red"]["timed_out"], true, "{vr}");
    assert_eq!(t0["green"]["timed_out"], false);
    let review = calls(&env, "RG-10")
        .into_iter()
        .find(|c| c["key"] == "reviewer")
        .unwrap();
    assert!(
        review["prompt"]
            .as_str()
            .unwrap()
            .contains("the red run TIMED OUT: counted as failing"),
        "{review}"
    );
}

/// Edge case 11, AC-8: the program is gone when the red runs start — `gate_unavailable`, a machine
/// stop with no fix cycle; with the program back, a plain `resume` goes back into Verify.
#[test]
fn m17_ec11_a_program_missing_when_red_starts_is_gate_unavailable() {
    let env = setup_rg("", "", &[]);
    let runner = rg_runner(
        &env.root,
        Runner {
            move_once: true,
            ..Default::default()
        },
    );
    std::fs::write(env.root.join("MOVE_ONCE"), "1").unwrap();
    let org = org_runner(&runner, "5m", "");
    commit_repo(
        &env,
        &[(".toryo/toryo-orgchart.yaml", &org)],
        "the runner gate",
    );
    create(&env, "RG-11");
    let s = rg_script(
        &env,
        "rg11.json",
        json!([{"ac": "AC-1", "tests": ["Divide-Numbers"]}, {"ac": "AC-2", "tests": ["Divide-Numbers"]}, {"ac": "AC-3", "tests": ["Divide-Numbers"]}]),
    );
    let (_, err) = toryo(&env, &["run", "RG-11", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "RG-11");
    // (the first line says whether the runner moved itself away during its green run)
    assert_eq!(
        t.state,
        toryo::engine::PipelineState::Escalated,
        "MOVE_ONCE left: {}, runner moved: {}\n{err}",
        env.root.join("MOVE_ONCE").exists(),
        std::path::Path::new(&format!("{runner}.gone")).exists()
    );
    assert_eq!(t.machine.reason_class(), Some("gate_unavailable"), "{err}");
    assert_eq!(t.fix_cycle, 0, "no fix cycle");
    // put the program back: a plain resume re-enters Verify and passes
    std::fs::rename(format!("{runner}.gone"), &runner).unwrap();
    let (code, err) = toryo(
        &env,
        &["resume", "RG-11", "--yes", "--fake-agents", &s],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "RG-11");
    assert_eq!(t.state, toryo::engine::PipelineState::Completed, "{err}");
    assert_eq!(t.fix_cycle, 0);
}

/// Edge case 12, AC-9: test code that writes into the red checkout — `verify_mutated_tree`.
#[test]
fn m17_ec12_test_code_writing_into_the_red_checkout_is_verify_mutated_tree() {
    let env = setup_rg("", "", &[]);
    let runner = rg_runner(
        &env.root,
        Runner {
            mutate: true,
            ..Default::default()
        },
    );
    let org = org_runner(&runner, "5m", "");
    commit_repo(
        &env,
        &[(".toryo/toryo-orgchart.yaml", &org)],
        "the runner gate",
    );
    create(&env, "RG-12");
    let s = rg_script(&env, "rg12.json", full_proof());
    let (_, err) = toryo(&env, &["run", "RG-12", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "RG-12");
    assert_eq!(t.state, toryo::engine::PipelineState::Escalated, "{err}");
    assert_eq!(
        t.machine.reason_class(),
        Some("verify_mutated_tree"),
        "{err}"
    );
    let reason = t.machine.reason().unwrap_or_default();
    assert!(
        reason.contains("README.md")
            && reason.contains("the red checkout of gate `divide-present`"),
        "{reason}"
    );
    assert!(
        clean(&wt(&env, "RG-12")),
        "the task worktree was never touched"
    );
}

/// Edge case 13, AC-10: a crash point inside the red phase, and a stop request while a red run
/// runs — the verify attempt is interrupted and a plain `resume` runs verify again; no fix cycle.
#[test]
fn m17_ec13_a_crash_and_a_stop_during_the_red_runs() {
    // a crash after the first red run
    let env = setup_rg(RG_SRC, "", &[]);
    create(&env, "RG-13");
    let s = rg_script(&env, "rg13.json", full_proof());
    let (code, err) = toryo(
        &env,
        &["run", "RG-13", "--yes", "--fake-agents", &s],
        &[("TORYO_TEST_CRASH_AT", "red_green:after_red_run")],
    );
    assert_eq!(code, 86, "{err}");
    assert_eq!(
        task(&env, "RG-13").state,
        toryo::engine::PipelineState::Verifying
    );
    let (code, err) = toryo(
        &env,
        &["resume", "RG-13", "--yes", "--fake-agents", &s],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "RG-13");
    assert_eq!(t.state, toryo::engine::PipelineState::Completed, "{err}");
    assert_eq!(t.fix_cycle, 0);
    let verifies: Vec<_> = attempts(&env, "RG-13")
        .into_iter()
        .filter(|a| a.stage == "verify")
        .map(|a| a.status)
        .collect();
    assert_eq!(
        verifies,
        [
            toryo::engine::store::AttemptStatus::Interrupted,
            toryo::engine::store::AttemptStatus::Succeeded
        ]
    );
    // a stop while a red run waits
    let env = setup_rg("", "", &[]);
    let runner = rg_runner(
        &env.root,
        Runner {
            slow: true,
            ..Default::default()
        },
    );
    std::fs::write(env.root.join("SLOW"), "1").unwrap();
    let org = org_runner(&runner, "5m", "");
    commit_repo(
        &env,
        &[(".toryo/toryo-orgchart.yaml", &org)],
        "the runner gate",
    );
    create(&env, "RG-14");
    let s = rg_script(&env, "rg14.json", full_proof());
    let mut child = common::spawn(
        &env,
        &["run", "RG-14", "--yes", "--fake-agents", &s],
        "rg14.log",
    );
    wait_until("a red run", 60, || {
        events(&env, "RG-14").iter().any(|e| {
            e["type"] == "verify_command"
                && e["side"] == "green"
                && e["name"] == "divide-present Division"
        })
    });
    std::thread::sleep(std::time::Duration::from_millis(500));
    let (code, err) = toryo(&env, &["stop", "RG-14"], &[]);
    assert_eq!(code, 0, "{err}");
    child.wait().unwrap();
    let t = task(&env, "RG-14");
    assert_eq!(
        t.state,
        toryo::engine::PipelineState::Verifying,
        "the state is kept"
    );
    std::fs::remove_file(env.root.join("SLOW")).unwrap();
    let (code, err) = toryo(
        &env,
        &["resume", "RG-14", "--yes", "--fake-agents", &s],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let t = task(&env, "RG-14");
    assert_eq!(t.state, toryo::engine::PipelineState::Completed, "{err}");
    assert_eq!(t.fix_cycle, 0);
    assert!(attempts(&env, "RG-14").iter().any(
        |a| a.stage == "verify" && a.status == toryo::engine::store::AttemptStatus::Interrupted
    ));
}

/// Edge case 16: a red-green gate before an environment gate keeps its order (the environment
/// gate runs after the proof); one after an environment gate is refused, as for every check gate.
#[test]
fn m17_ec16_a_red_green_gate_keeps_its_place_before_an_environment_gate() {
    let org = org_rg(RG_SRC, "").replace(
        "gates:\n",
        "gates:\n  deploy: { kind: environment, timeout: 5m }\n",
    );
    let env = setup_rg(
        "",
        "",
        &[
            (".toryo/toryo-orgchart.yaml", &org),
            (
                ".toryo/roles/implementer/toryo-role.yaml",
                "toryo: 1\npurpose: Change the code.\nbefore_handoff: [divide-present, deploy]\n",
            ),
        ],
    );
    std::fs::write(
        &env.machine,
        format!(
            "{MACHINE_PROFILES}{MACHINE_FLOOR}environment_gates:\n  deploy: {{ program: git, args: [--version], lock: m17-env }}\n"
        ),
    )
    .unwrap();
    create(&env, "RG-15");
    let s = rg_script(&env, "rg15.json", full_proof());
    let (code, err) = toryo(&env, &["run", "RG-15", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    let ev = events(&env, "RG-15");
    let at = |f: &dyn Fn(&Value) -> bool| ev.iter().position(f).unwrap();
    let proof = at(&|e| e["type"] == "red_green_finished");
    let deploy = at(&|e| e["type"] == "verify_command" && e["kind"] == "environment");
    assert!(proof < deploy, "the proof first, then the environment gate");
    // after an environment gate: refused at load
    let env2 = setup_rg(
        "",
        "",
        &[
            (".toryo/toryo-orgchart.yaml", &org),
            (
                ".toryo/roles/implementer/toryo-role.yaml",
                "toryo: 1\npurpose: Change the code.\nbefore_handoff: [deploy, divide-present]\n",
            ),
        ],
    );
    let (code, _, err) = run_out(&env2, &["config", "show"]);
    assert_ne!(code, 0);
    assert!(
        err.contains("check gate 'divide-present' comes after environment gate 'deploy'"),
        "{err}"
    );
}

/// Edge case 19: a per-test run whose output passes the 1 MB + 8 MB bound — its gate log is
/// bounded (one line says how much was dropped) and a profile secret in it is redacted, as for
/// every gate.
#[test]
fn m17_ec19_a_huge_per_test_output_is_bounded_and_redacted() {
    const SECRET: &str = "m17-s3cr3t-value-x";
    let rg = r#"red_green: { tests: ["tests/**"], args: [cat-file, -p, "{test}"] }"#;
    let mut big = format!("{SECRET}\n");
    let line = format!("{}\n", "x".repeat(99));
    while big.len() < 11 * 1024 * 1024 {
        big.push_str(&line);
    }
    big.push_str(&format!("{SECRET}\n"));
    let env = setup_rg(rg, "", &[("big.txt", &big)]);
    std::fs::write(
        &env.machine,
        MACHINE_PROFILES.replace(
            "exe: codex-not-installed }",
            "exe: codex-not-installed, secret_env: { TORYO_M17_TOKEN: \"env:TORYO_M17_SECRET\" } }",
        ) + MACHINE_FLOOR,
    )
    .unwrap();
    create(&env, "RG-16");
    let proof = json!([
        {"ac": "AC-1", "tests": ["HEAD:big.txt"]},
        {"ac": "AC-2", "tests": ["HEAD:big.txt"]},
        {"ac": "AC-3", "tests": ["HEAD:big.txt"]},
    ]);
    let s = rg_script(&env, "rg16.json", proof);
    let _ = toryo(
        &env,
        &["run", "RG-16", "--yes", "--fake-agents", &s],
        &[("TORYO_M17_SECRET", SECRET)],
    );
    let logs = task(&env, "RG-16").run_store.join("gate-logs");
    let green = std::fs::read_dir(&logs)
        .unwrap()
        .flatten()
        .find(|e| {
            e.file_name()
                .to_string_lossy()
                .ends_with("-divide-present_HEAD_big.txt.log")
        })
        .expect("the per-test run's log")
        .path();
    let text = std::fs::read_to_string(&green).unwrap();
    assert!(
        text.len() < 9 * 1024 * 1024 + 4096,
        "bounded: {}",
        text.len()
    );
    assert!(text.len() > 8 * 1024 * 1024, "{}", text.len());
    assert!(
        text.contains("bytes of output omitted: Toryo keeps the first 1 MB and the last 8 MB"),
        "the marker"
    );
    assert!(!text.contains(SECRET), "redacted");
    assert_eq!(text.matches("[REDACTED:TORYO_M17_TOKEN]").count(), 2);
}

/// Edge case 15, AC-6: `{test}` inside an element (`-e{test}`, as in
/// `--filter=FullyQualifiedName~{test}`) is substituted there, as argv — the gate proves every
/// criterion; as a whole element it is the fixture's `-e "{test}"` (every other test here).
#[test]
fn m17_ec15_test_inside_an_element_is_substituted_there() {
    let env = setup_rg(
        r#"red_green: { tests: ["tests/**"], args: [grep, -q, -F, "-e{test}", --, src] }"#,
        "",
        &[],
    );
    create(&env, "RG-15");
    let s = rg_script(&env, "rg15.json", full_proof());
    let (code, err) = toryo(&env, &["run", "RG-15", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        task(&env, "RG-15").state,
        toryo::engine::PipelineState::Completed,
        "{err}"
    );
    let rg = &verify_result(&env, "RG-15")["red_green"];
    assert_eq!(
        (rg["proven"].clone(), rg["pass"].clone()),
        (json!(3), json!(true)),
        "{rg}"
    );
    let t = &rg["acs"][1]["tests"][0];
    assert_eq!(
        (t["green"]["exit"].clone(), t["red"]["exit"].clone()),
        (json!(0), json!(1)),
        "{rg}"
    );
}
