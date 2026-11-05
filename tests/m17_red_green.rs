//! M17 Part A — the red-green proof gate (`docs/tasks/M17/TASK.md`): a check gate proves, per
//! acceptance criterion, that the named tests fail on the base commit with the task's test files
//! laid over it (red) and pass on the checkpoint (green). End to end through the `toryo` binary
//! with fake agents; the test runner is `git grep` (see `common::m17`), so no script runs.
#![cfg(feature = "m0-kit")]

mod common;

use common::m17::*;
use common::*;
use serde_json::{json, Value};

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
