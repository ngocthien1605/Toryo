//! M17 fixtures: the calculator in org chart mode with a red-green gate (Part A) and with
//! implementer candidates (Part B). The test runner of the red-green gate is `git grep`, so the
//! same fixture runs on Windows and Linux without a script: test id `X` passes when the
//! checkout's `src` holds the text `X` (`git grep -q -F -e X -- src`).

use std::path::Path;

use serde_json::{json, Value};

use super::*;

/// The red-green block of the fixture gate: one `git grep` per test id over `src`.
pub const RG_SRC: &str =
    r#"red_green: { tests: ["tests/**"], args: [grep, -q, -F, -e, "{test}", --, src] }"#;

/// The org chart of the M17 fixture: the gate `divide-present` (as in [`ORGCHART`]) with `rg`
/// inside its flow mapping (empty: no red-green), and `seat_extra` inside the implementer seat
/// (e.g. `, candidates: [codex-openai, claude-max]`).
pub fn org_rg(rg: &str, seat_extra: &str) -> String {
    let rg = if rg.is_empty() {
        String::new()
    } else {
        format!(", {rg}")
    };
    format!(
        "toryo: 1\nteam: calc-team\ngates:\n  divide-present: {{ kind: check, program: git, args: [grep, -q, Divide-Numbers, --, src], timeout: 5m{rg} }}\nseats:\n  planner: {{ profile: claude-max }}\n  implementer: {{ profile: codex-openai{seat_extra} }}\n  reviewer: {{ profile: claude-max }}\n"
    )
}

/// The M17 fixture in org chart mode: [`org_rg`] and `files` (which may replace any file).
pub fn setup_rg(rg: &str, seat_extra: &str, files: &[(&str, &str)]) -> Env {
    let org = org_rg(rg, seat_extra);
    let mut all: Vec<(&str, &str)> = vec![(".toryo/toryo-orgchart.yaml", &org)];
    all.extend_from_slice(files);
    setup_orgchart("codex-openai", "", &all)
}

/// `toryo …` → (exit code, stdout, stderr).
pub fn run_out(env: &Env, args: &[&str]) -> (i32, String, String) {
    let o = command(env, args, &[]).output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

/// An implementation report with a `proof` map.
pub fn implementation_with(proof: Value) -> Value {
    json!({"kind":"complete","structured":{"summary":"s","deviations":[],"validation_performed":["read"],"open_issues":[],"resolved":[],"proof": proof}})
}

/// The proof map that proves AC-1..AC-3 of the fixture with tests that pass once `src` holds
/// the division (green) and fail on the base commit (red).
pub fn full_proof() -> Value {
    json!([
        {"ac": "AC-1", "tests": ["Divide-Numbers"]},
        {"ac": "AC-2", "tests": ["Division by zero"]},
        {"ac": "AC-3", "tests": ["Divide-Numbers"]},
    ])
}

/// The newest `VERIFY_RESULT.json` view of a task.
pub fn verify_result(env: &Env, id: &str) -> Value {
    let p = task(env, id).artifacts().join("VERIFY_RESULT.json");
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

/// Every version of a task's artifact `logical`, oldest first.
pub fn versions(env: &Env, id: &str, logical: &str) -> Vec<Value> {
    let dir = toryo::engine::artifacts::root(&task(env, id)).join(logical);
    let mut files: Vec<(u32, std::path::PathBuf)> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().to_string();
                    let v = n.split('-').next()?.parse().ok()?;
                    Some((v, e.path()))
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
        .into_iter()
        .map(|(_, p)| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap())
        .collect()
}

/// The recorded events (`events.jsonl`) of a task.
pub fn events(env: &Env, id: &str) -> Vec<Value> {
    let p = task(env, id).run_store.join("events.jsonl");
    std::fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Every ref of a repository (`git for-each-ref`).
pub fn refs(repo: &Path) -> Vec<String> {
    git(repo, &["for-each-ref", "--format=%(refname)"])
        .lines()
        .map(str::to_string)
        .collect()
}

/// The fake-agent turns of a red-green run: the plan, one implementer turn that writes the
/// division and `tests/Divide.Tests.ps1` with `proof`, an approving review.
pub fn rg_script(env: &Env, name: &str, proof: Value) -> String {
    script(
        env,
        name,
        json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![
                write("src/Calculator.psm1", DIVIDE),
                write("tests/Divide.Tests.ps1", "Describe 'Divide' { It 'divides' { Divide-Numbers -A 6 -B 3 | Should -Be 2 } }\n"),
                implementation_with(proof),
            ])],
            "reviewer": [turn(vec![approved()])],
        }),
    )
}
