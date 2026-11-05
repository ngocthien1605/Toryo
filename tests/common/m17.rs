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
        {"ac": "AC-2", "tests": ["Division"]},
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

/// What the fake test runner of [`rg_runner`] does besides passing and failing.
#[derive(Default, Clone, Copy)]
pub struct Runner {
    /// On the red side, wait (about a minute) while `<root>/SLOW` exists.
    pub slow: bool,
    /// On the red side, append to the tracked `README.md` of the checkout.
    pub mutate: bool,
    /// On a green run, rename the runner away once while `<root>/MOVE_ONCE` exists: the next
    /// start finds no program.
    pub move_once: bool,
}

/// A fake test runner outside the repository — a `.cmd` on Windows, an executable `sh` script
/// elsewhere: `runner suite` exits 0; `runner <id>` exits 0 when the checkout's `src` holds the
/// text `<id>` (green), and otherwise — the red side — does what `r` says and exits 1. Returns its
/// path with forward slashes (for the org chart).
pub fn rg_runner(root: &Path, r: Runner) -> String {
    let marker = |n: &str| root.join(n).display().to_string();
    #[cfg(windows)]
    let p = {
        let p = root.join("rg-runner.cmd");
        let mut s = String::from("@echo off\r\nif \"%~1\"==\"suite\" exit /b 0\r\ngit grep -q -F -e \"%~1\" -- src\r\nif errorlevel 1 goto red\r\n");
        if r.move_once {
            // the runner's own path spelled with `\`: cmd's `move` and `del` can read a `/` as a
            // switch, and `%~f0` keeps the spelling the runner was started with. `exit 0`, not
            // `exit /b 0`: cmd ends at once — returning from a batch file that is gone makes it
            // say "The batch file cannot be found." and exit 1
            s.push_str(&format!(
                "if exist \"{m}\" ( del \"{m}\" & move \"{me}\" \"{me}.gone\" >nul & exit 0 )\r\n",
                m = marker("MOVE_ONCE"),
                me = p.display()
            ));
        }
        s.push_str("exit /b 0\r\n:red\r\n");
        if r.slow {
            s.push_str(&format!(
                "if exist \"{}\" ping -n 61 127.0.0.1 >nul\r\n",
                marker("SLOW")
            ));
        }
        if r.mutate {
            s.push_str("echo mutated>>README.md\r\n");
        }
        s.push_str("exit /b 1\r\n");
        std::fs::write(&p, s).unwrap();
        p
    };
    #[cfg(not(windows))]
    let p = {
        use std::os::unix::fs::PermissionsExt;
        let p = root.join("rg-runner.sh");
        let mut s = String::from(
            "#!/bin/sh\n[ \"$1\" = suite ] && exit 0\nif git grep -q -F -e \"$1\" -- src; then\n",
        );
        if r.move_once {
            s.push_str(&format!(
                "  if [ -f '{m}' ]; then rm -f '{m}'; mv \"$0\" \"$0.gone\"; fi\n",
                m = marker("MOVE_ONCE")
            ));
        }
        s.push_str("  exit 0\nfi\n");
        if r.slow {
            s.push_str(&format!(
                "i=0; while [ -f '{}' ] && [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done\n",
                marker("SLOW")
            ));
        }
        if r.mutate {
            s.push_str("echo mutated >> README.md\n");
        }
        s.push_str("exit 1\n");
        std::fs::write(&p, s).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    };
    p.display().to_string().replace('\\', "/")
}

/// The org chart of the M17 fixture whose gate runs the fake test runner `program` (its own
/// args: `suite`) within `timeout`, with `rg_extra` inside its `red_green` block.
pub fn org_runner(program: &str, timeout: &str, rg_extra: &str) -> String {
    format!(
        "toryo: 1\nteam: calc-team\ngates:\n  divide-present: {{ kind: check, program: \"{program}\", args: [suite], timeout: {timeout}, red_green: {{ tests: [\"tests/**\"], args: [\"{{test}}\"]{rg_extra} }} }}\nseats:\n  planner: {{ profile: claude-max }}\n  implementer: {{ profile: codex-openai }}\n  reviewer: {{ profile: claude-max }}\n"
    )
}

// ── Part B: best-of-N ──────────────────────────────────────────────────────────────────────

/// The M17 fixture with implementer candidates: `candidates` is the YAML list of the seat (e.g.
/// `[codex-openai, codex-openai]`), `rg` the red-green block of the gate (empty: none).
pub fn setup_bon(candidates: &str, rg: &str, files: &[(&str, &str)]) -> Env {
    setup_rg(rg, &format!(", candidates: {candidates}"), files)
}

/// The fan-out of a task.
pub fn fanout(env: &Env, id: &str) -> Option<store::FanoutRow> {
    let i = id.to_string();
    db(env).call(move |tx| store::fanout(tx, &i)).unwrap()
}

/// The candidates of a task.
pub fn candidates(env: &Env, id: &str) -> Vec<store::CandidateRow> {
    let i = id.to_string();
    db(env).call(move |tx| store::candidates(tx, &i)).unwrap()
}

/// The answer of a compare turn.
pub fn select(winner: &str, ranking: &[&str]) -> Value {
    json!({"kind":"complete","structured":{"winner": winner, "reason": format!("{winner} is the clearest"), "ranking": ranking}})
}

/// A candidate turn that writes the division (with `extra` appended to it) and reports.
pub fn candidate_turn(extra: &str) -> Value {
    turn(vec![
        write("src/Calculator.psm1", &format!("{DIVIDE}{extra}")),
        implementation(),
    ])
}

/// The worktree folder of candidate `k`.
pub fn cwt(env: &Env, id: &str, k: u32) -> std::path::PathBuf {
    env.root.join("wts").join(format!("{id}.c{k}"))
}

/// A gate program outside the repository — a `.cmd` on Windows, an executable `sh` script
/// elsewhere — with the body `cmd` or `sh`. Returns its path with forward slashes.
pub fn gate_program(root: &Path, name: &str, cmd: &str, sh: &str) -> String {
    #[cfg(windows)]
    let p = {
        let p = root.join(format!("{name}.cmd"));
        std::fs::write(&p, format!("@echo off\r\n{}", cmd.replace('\n', "\r\n"))).unwrap();
        p
    };
    #[cfg(not(windows))]
    let p = {
        use std::os::unix::fs::PermissionsExt;
        let _ = cmd;
        let p = root.join(format!("{name}.sh"));
        std::fs::write(&p, format!("#!/bin/sh\n{sh}")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    };
    p.display().to_string().replace('\\', "/")
}

/// The org chart of the M17 fixture whose check gate runs `program` (no args), with `seat_extra`
/// inside the implementer seat.
pub fn org_gate(program: &str, seat_extra: &str) -> String {
    format!(
        "toryo: 1\nteam: calc-team\ngates:\n  divide-present: {{ kind: check, program: \"{program}\", timeout: 5m }}\nseats:\n  planner: {{ profile: claude-max }}\n  implementer: {{ profile: codex-openai{seat_extra} }}\n  reviewer: {{ profile: claude-max }}\n"
    )
}
