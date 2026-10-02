//! M10 acceptance (plan §12): `toryo usage` with estimated cost from `prices.toml`, the model
//! recorded on every usage row, and the fallback for rows written before M10 — through the
//! `toryo` binary with fake agents. Every command passes `--prices`: the owner's
//! `%USERPROFILE%\.toryo\prices.toml` is never read.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::{json, Value};
use toryo::engine::PipelineState as St;

const PRICES: &str = r#"
version = "2026-09-29"
safety_factor = 1.2
[models."gpt-6-astra"]
uncached_input = 1.25
cache_read = 0.125
cache_write = 0.0
output = 10.0
"#;

fn usage_step(input: u64, cache_read: u64, output: u64) -> Value {
    json!({"kind": "usage", "input": input, "cache_read": cache_read, "output": output})
}

fn out(env: &Env, args: &[&str]) -> (i32, String, String) {
    let o = command(env, args, &[]).output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

/// A completed run: planner `claude-max` (subscription, role model `opus`), implementer
/// `codex-foundry` (billed foundry, profile model `gpt-6-astra`), reviewer `claude-max` with no
/// model anywhere.
fn foundry_run(env: &Env, id: &str) {
    std::fs::write(
        &env.machine,
        format!("{MACHINE_PROFILES}  codex-foundry: {{ kind: codex, billing: foundry, exe: codex-not-installed, model: gpt-6-astra }}\n{MACHINE_FLOOR}"),
    )
    .unwrap();
    let cfg = std::fs::read_to_string(&env.config).unwrap();
    std::fs::write(
        &env.config,
        cfg.replace(
            "planner: { profile: claude-max }",
            "planner: { profile: claude-max, model: opus }\n  reviewer: { profile: claude-max }",
        ),
    )
    .unwrap();
    create(env, id);
    let s = script(
        env,
        "u.json",
        json!({
            "planner": [turn(vec![usage_step(1000, 500, 100), plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), usage_step(1_000_000, 2_000_000, 100_000), implementation()])],
            "reviewer": [turn(vec![usage_step(10, 0, 5), approved()])],
        }),
    );
    let (code, err) = toryo(env, &["run", id, "--yes", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(env, id).state, St::Completed, "{err}");
}

fn models(env: &Env, id: &str) -> Vec<(String, Option<String>)> {
    let i = id.to_string();
    db(env)
        .call(move |tx| {
            let mut q =
                tx.prepare("SELECT profile, model FROM usage WHERE task_id = ?1 ORDER BY id")?;
            let v = q
                .query_map([i], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(v)
        })
        .unwrap()
}

#[test]
fn usage_estimates_the_cost_of_the_foundry_rows() {
    let env = setup("codex-foundry");
    foundry_run(&env, "U-1");
    let prices = env.root.join("prices.toml");
    std::fs::write(&prices, PRICES).unwrap();
    let p = prices.to_str().unwrap();

    // AC-4: the new rows carry the model the turn ran with
    assert_eq!(
        models(&env, "U-1"),
        vec![
            ("claude-max".into(), Some("opus".into())),
            ("codex-foundry".into(), Some("gpt-6-astra".into())),
            ("claude-max".into(), None),
        ]
    );

    // AC-2: text
    let (code, text, err) = out(&env, &["usage", "U-1", "--prices", p]);
    assert_eq!(code, 0, "{err}");
    let line = |stage: &str| {
        text.lines()
            .find(|l| l.starts_with(stage))
            .unwrap_or_else(|| panic!("{stage}: {text}"))
            .to_string()
    };
    let imp = line("implement");
    for want in [
        "codex-foundry",
        "gpt-6-astra",
        "1000000",
        "2000000",
        "100000",
        "3100000",
        "est. $3.0000",
    ] {
        assert!(imp.contains(want), "{want:?} in {imp:?}");
    }
    assert!(!imp.contains("~est."), "{imp}");
    let plan = line("plan");
    assert!(
        plan.contains("opus") && plan.contains("1600") && plan.trim_end().ends_with('—'),
        "{plan}"
    );
    let review = line("review");
    assert!(
        review.contains("unknown model") && review.trim_end().ends_with('—'),
        "{review}"
    );
    let total = line("total");
    assert!(
        total.contains("3101615") && total.contains("est. $3.0000"),
        "{total}"
    );
    assert!(
        text.contains("prices: version 2026-09-29, safety factor 1.2"),
        "{text}"
    );

    // AC-2: --json
    let (code, raw, err) = out(&env, &["usage", "U-1", "--json", "--prices", p]);
    assert_eq!(code, 0, "{err}");
    let j: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(j["task"], "U-1");
    assert_eq!(j["prices"]["version"], "2026-09-29");
    let rows = j["rows"].as_array().unwrap();
    let r = rows.iter().find(|r| r["stage"] == "implement").unwrap();
    assert_eq!(r["model"], "gpt-6-astra");
    assert_eq!(r["billing_kind"], "foundry");
    assert_eq!(
        r["buckets"],
        json!({"uncached_input": 1_000_000, "cache_read": 2_000_000, "cache_write": 0, "output": 100_000})
    );
    assert_eq!(r["exact_tokens"], 3_100_000);
    assert_eq!(r["reserved_tokens"], 0);
    assert!(
        (r["est_cost_usd"].as_f64().unwrap() - 3.0).abs() < 1e-9,
        "{r}"
    );
    let plan = rows.iter().find(|r| r["stage"] == "plan").unwrap();
    assert!(plan["est_cost_usd"].is_null());
    assert_eq!(plan["cost"], "—");
    assert_eq!(j["totals"]["exact_tokens"], 3_101_615);
    assert_eq!(j["totals"]["provisional_tokens"], 0);
    assert!((j["totals"]["est_cost_usd"].as_f64().unwrap() - 3.0).abs() < 1e-9);
    assert_eq!(j["no_price"], json!([]));

    // a model missing from the table: `no price`, listed at the end
    std::fs::write(&prices, PRICES.replace("gpt-6-astra", "another-model")).unwrap();
    let (_, text, _) = out(&env, &["usage", "U-1", "--prices", p]);
    assert!(line_of(&text, "implement").contains("no price"), "{text}");
    assert!(
        text.contains("no price: gpt-6-astra (codex-foundry)"),
        "{text}"
    );

    // AC-4: rows written before M10 (model NULL) take it from the frozen configuration
    db(&env)
        .call(|tx| Ok(tx.execute("UPDATE usage SET model = NULL", [])?))
        .unwrap();
    std::fs::write(&prices, PRICES).unwrap();
    let (_, raw, _) = out(&env, &["usage", "U-1", "--json", "--prices", p]);
    let j: Value = serde_json::from_str(&raw).unwrap();
    let by_stage = |s: &str| {
        j["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["stage"] == s)
            .unwrap()["model"]
            .clone()
    };
    assert_eq!(by_stage("plan"), "opus", "role model");
    assert_eq!(by_stage("implement"), "gpt-6-astra", "profile model");
    assert_eq!(by_stage("review"), "unknown model");
}

fn line_of(text: &str, stage: &str) -> String {
    text.lines()
        .find(|l| l.starts_with(stage))
        .unwrap_or_default()
        .to_string()
}

#[test]
fn a_bad_price_table_names_the_key_and_none_means_no_costs() {
    let env = setup("codex-foundry");
    foundry_run(&env, "U-2");
    let bad = env.root.join("bad.toml");
    std::fs::write(
        &bad,
        PRICES.replace("output = 10.0", "output = 10.0\nreasoning = 1.0"),
    )
    .unwrap();
    let (code, _, err) = out(&env, &["usage", "U-2", "--prices", bad.to_str().unwrap()]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("models.\"gpt-6-astra\".reasoning"), "{err}");
    std::fs::write(&bad, PRICES.replace("output = 10.0", "output = -10.0")).unwrap();
    let (code, _, err) = out(&env, &["usage", "U-2", "--prices", bad.to_str().unwrap()]);
    assert_eq!(code, 2);
    assert!(
        err.contains("models.\"gpt-6-astra\".output") && err.contains("negative"),
        "{err}"
    );

    let none = env.root.join("no-such-prices.toml");
    let (code, text, err) = out(&env, &["usage", "U-2", "--prices", none.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    assert!(text.contains("no price table"), "{text}");
    assert!(!text.contains("est. $"), "{text}");
    assert!(line_of(&text, "implement").contains("3100000"), "{text}");
}

// ── C2: scripted turns in a mixed run report zero usage ──────────────────────────────

/// AC-1: when the fake script lists `only_profiles`, a scripted turn that ends without a
/// `usage` step is an `exact` zero row — no reservation — and `status` shows none. Both profiles
/// are listed here, so nothing real runs. The same turns without `only_profiles` keep R41's
/// reservation (AC-2 is the R41 tests, unchanged).
#[test]
fn scripted_turns_of_a_mixed_run_report_exact_zero() {
    let env = setup("codex-openai");
    let turns = json!({
        "planner": [turn(vec![plan()])],
        "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
        "reviewer": [turn(vec![fail("Unknown", "a scripted failure without usage")])],
    });
    let mixed = env.root.join("mixed.json");
    std::fs::write(
        &mixed,
        serde_json::to_vec(
            &json!({"turns": turns, "only_profiles": ["claude-max", "codex-openai"]}),
        )
        .unwrap(),
    )
    .unwrap();
    create(&env, "U-3");
    let (_, err) = toryo(
        &env,
        &[
            "run",
            "U-3",
            "--yes",
            "--fake-agents",
            mixed.to_str().unwrap(),
        ],
        &[],
    );
    let t = task(&env, "U-3");
    assert_eq!(t.state, St::Escalated, "the failed review escalates: {err}");
    assert_eq!((t.spent_tokens, t.reserved_tokens), (0, 0), "{err}");
    let i = "U-3".to_string();
    let rows: Vec<(String, Option<i64>, Option<i64>)> = db(&env)
        .call(move |tx| {
            let mut q = tx.prepare(
                "SELECT accounting, budget_tokens, reservation_tokens FROM usage WHERE task_id = ?1 ORDER BY id",
            )?;
            let v = q
                .query_map([i], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(v)
        })
        .unwrap();
    assert!(rows.len() >= 3, "{rows:?}");
    for r in &rows {
        assert_eq!(r, &("exact".to_string(), Some(0), None), "{rows:?}");
    }
    assert!(!status_out(&env, "U-3").contains("reserved"));

    // the same script without `only_profiles`: turns with no usage are reserved (R41)
    let pure = script(&env, "pure.json", turns);
    create(&env, "U-4");
    let _ = toryo(&env, &["run", "U-4", "--yes", "--fake-agents", &pure], &[]);
    assert!(task(&env, "U-4").reserved_tokens > 0);
    assert!(status_out(&env, "U-4").contains("reserved"));
}
