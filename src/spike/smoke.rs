//! `toryo spike smoke` — the smallest end-to-end orchestration of both CLIs:
//! Claude plans (read-only) → Codex implements (sandboxed) → toryo checkpoints with
//! `GitCmd` and builds the canonical diff → toryo runs the fixture's verify scripts →
//! Claude reviews. It is runbook §5 automated, with no human gate (smoke test only).
//!
//! For the Claude steps it tries the plan's launch mode first and then the two §6.2
//! fallbacks, recording which one worked — the first answer to M0-VERIFY 1.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::agents::{claude_turn, codex_turn, ClaudeArgs, CodexArgs};
use super::fixture;
use super::render;
use super::results::{append, new_run_dir, Record};
use super::turn::{print_summary, TurnOpts, TurnRun};
use crate::agents::claude::LaunchMode;
use crate::config::{expand_vars, Effective};
use crate::git::Loc;
use crate::process::env::build_env;
use crate::process::redact::Redactions;
use crate::process::{spawn, Boundary, Chunk, SpawnSpec, WaitOptions};

fn fwd(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

pub struct SmokeArgs {
    pub claude_profile: String,
    pub codex_profile: String,
    pub claude_model: Option<String>,
    pub codex_model: Option<String>,
    /// Launch modes to try, in order.
    pub modes: Vec<LaunchMode>,
}

struct Step {
    name: &'static str,
    ok: bool,
    detail: String,
}

fn step(steps: &mut Vec<Step>, name: &'static str, ok: bool, detail: impl Into<String>) {
    let detail = detail.into();
    eprintln!("[{}] {name}: {detail}", if ok { "PASS" } else { "FAIL" });
    steps.push(Step { name, ok, detail });
}

/// (mode that worked, every attempt with its note, the successful turn)
type ClaudeStep = (
    Option<LaunchMode>,
    Vec<(LaunchMode, String)>,
    Option<TurnRun>,
);

/// Run a Claude step with the first launch mode that produces a structured answer.
#[allow(clippy::too_many_arguments)]
fn claude_step(
    cfg: &Effective,
    a: &SmokeArgs,
    wt: &Path,
    rs: &Path,
    root: &Path,
    label: &str,
    prompt: &str,
    context: &Path,
    schema: &str,
) -> Result<ClaudeStep> {
    let mut tried = Vec::new();
    for mode in &a.modes {
        eprintln!("\n== {label}: Claude, launch mode {mode:?} ==");
        let r = claude_turn(
            cfg,
            ClaudeArgs {
                profile: a.claude_profile.clone(),
                worktree: wt.to_path_buf(),
                run_store: rs.to_path_buf(),
                prompt: prompt.into(),
                schema: Some(schema.into()),
                context_file: Some(context.to_path_buf()),
                model: a.claude_model.clone(),
                mode: *mode,
                resume: None,
                drop_flags: vec![],
                extra_args: vec![],
                label: format!("{label}-{mode:?}").to_lowercase(),
                opts: TurnOpts {
                    max: Duration::from_secs(20 * 60),
                    ..Default::default()
                },
            },
            root,
        );
        match r {
            Ok(r) => {
                print_summary(&r);
                if r.outcome.structured.is_some() {
                    let note = if r.outcome.success {
                        "ok".to_string()
                    } else {
                        format!(
                            "structured output, but {:?}: {}",
                            r.outcome.error_class,
                            r.outcome.error_detail.clone().unwrap_or_default()
                        )
                    };
                    tried.push((*mode, note));
                    return Ok((Some(*mode), tried, Some(r)));
                }
                let stderr = std::fs::read_to_string(r.run_dir.join(format!(
                    "{}.stderr.txt",
                    format!("{label}-{mode:?}").to_lowercase()
                )))
                .unwrap_or_default();
                tried.push((
                    *mode,
                    format!(
                        "{:?}: {} {}",
                        r.outcome.error_class,
                        r.outcome.error_detail.clone().unwrap_or_default(),
                        stderr.lines().take(3).collect::<Vec<_>>().join(" | ")
                    ),
                ));
            }
            Err(e) => tried.push((*mode, format!("{e:#}"))),
        }
    }
    Ok((None, tried, None))
}

pub(crate) fn run_verify(script: &Path, cwd: &Path) -> Result<(Option<i32>, String)> {
    let pwsh = which("pwsh").context("pwsh not found in PATH")?;
    let spec = SpawnSpec {
        program: pwsh,
        args: vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-File".into(),
            script.display().to_string(),
        ],
        cwd: cwd.to_path_buf(),
        env: {
            let mut e = build_env(std::env::vars(), &Default::default(), &Default::default());
            e.insert("NO_COLOR".into(), "1".into());
            e
        },
        stdin: None,
        boundary: Boundary::None, // smoke: trusted fixture code; the boundary is lowil-canary's job
    };
    let r = spawn(&spec, &Redactions::empty())?;
    let mut out = Vec::new();
    let info = r.wait(
        WaitOptions {
            max: Some(Duration::from_secs(600)),
            ..Default::default()
        },
        |c| {
            let (Chunk::Stdout(b) | Chunk::Stderr(b)) = c;
            out.extend(b)
        },
    )?;
    Ok((
        info.code,
        crate::process::strip_ansi(&String::from_utf8_lossy(&out)),
    ))
}

pub(crate) fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for ext in ["", ".exe", ".cmd"] {
            let p = dir.join(format!("{name}{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

pub fn smoke(cfg: &Effective, a: SmokeArgs) -> Result<bool> {
    let root = new_run_dir("smoke", "calc")?;
    let mut steps: Vec<Step> = Vec::new();
    eprintln!("smoke run directory: {}", root.display());

    // 0. fixture repo + task worktree + run store
    let repo = root.join("calc");
    let base = fixture::init_repo(&repo)?;
    let g = fixture::gitcmd()?;
    let wt = root.join("wt");
    g.run(
        &Loc::Discover(repo.clone()),
        false,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "toryo/CALC-001",
            &fwd(&wt),
            "main",
        ],
    )?;
    let wtl = Loc::Discover(wt.clone());
    let rs = root.join("runstore");
    std::fs::create_dir_all(&rs)?;
    let contracts = root.join("contracts");
    fixture::write_contracts(&contracts)?;
    std::fs::copy(repo.join("TASK.md"), rs.join("TASK.md"))?;
    step(
        &mut steps,
        "setup",
        true,
        format!("fixture main={}, worktree toryo/CALC-001", &base[..12]),
    );

    // 1. plan (Claude, read-only)
    let planner_ctx = rs.join("context-plan.md");
    std::fs::write(
        &planner_ctx,
        format!(
            "{}\n\n## Task pack\n- TASK.md: {}\n",
            fixture::ROLES[0].1,
            rs.join("TASK.md").display()
        ),
    )?;
    let (mode, tried, plan) = claude_step(
        cfg,
        &a,
        &wt,
        &rs,
        &root,
        "planner",
        &format!(
            "Create the plan for the task in {}.",
            rs.join("TASK.md").display()
        ),
        &planner_ctx,
        fixture::schema("plan.schema.json")?,
    )?;
    let modes_note = tried
        .iter()
        .map(|(m, n)| format!("{m:?}: {n}"))
        .collect::<Vec<_>>()
        .join(" || ");
    let Some(plan) = plan else {
        step(
            &mut steps,
            "plan (Claude)",
            false,
            format!("no launch mode produced a plan — {modes_note}"),
        );
        return finish(&root, steps, json!({"claude_modes": modes_note}));
    };
    let plan_json = plan.outcome.structured.clone().unwrap_or(Value::Null);
    std::fs::write(rs.join("plan.json"), serde_json::to_vec_pretty(&plan_json)?)?;
    std::fs::write(rs.join("PLAN.md"), render::render("plan", &plan_json)?)?;
    step(
        &mut steps,
        "plan (Claude)",
        plan.outcome.success,
        format!(
            "mode {:?}; files {}",
            mode.unwrap(),
            plan_json["files_likely_involved"]
        ),
    );
    let mode = mode.unwrap();

    // 2. implement (Codex, sandboxed)
    let impl_prompt = format!(
        "{}\n\n## Task pack (read-only)\n- TASK.md: {}\n- PLAN.md: {}\n",
        fixture::ROLES[1].1,
        rs.join("TASK.md").display(),
        rs.join("PLAN.md").display()
    );
    let schema_path = contracts.join("schemas").join("implementation.schema.json");
    eprintln!("\n== implementer: Codex ==");
    let head_before = g.run_str(&wtl, true, &["rev-parse", "HEAD"])?;
    let imp = codex_turn(
        cfg,
        CodexArgs {
            profile: a.codex_profile.clone(),
            worktree: wt.clone(),
            prompt: impl_prompt,
            schema_path: Some(schema_path),
            model: a.codex_model.clone(),
            resume: None,
            extra_args: vec![],
            label: "implementer".into(),
            opts: TurnOpts {
                max: Duration::from_secs(30 * 60),
                ..Default::default()
            },
        },
        &root,
    )?;
    print_summary(&imp);
    if let Some(v) = &imp.outcome.structured {
        std::fs::write(
            rs.join("implementation.json"),
            serde_json::to_vec_pretty(v)?,
        )?;
        std::fs::write(
            rs.join("IMPLEMENTATION.md"),
            render::render("implementation", v)?,
        )?;
    }
    let head_after = g.run_str(&wtl, true, &["rev-parse", "HEAD"])?;
    let status = g.run(
        &wtl,
        true,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let changed = crate::git::parse_status_z(&status);
    step(
        &mut steps,
        "implement (Codex)",
        imp.outcome.success,
        format!(
            "{} file(s) changed: {:?}; {:?}",
            changed.len(),
            changed.iter().map(|c| &c.1).collect::<Vec<_>>(),
            imp.outcome.error_detail
        ),
    );
    step(
        &mut steps,
        "agent did not commit (HEAD guard)",
        head_before == head_after,
        format!("HEAD {}", &head_after[..12]),
    );
    let protected: Vec<&String> = changed
        .iter()
        .map(|c| &c.1)
        .filter(|p| p.starts_with("scripts/") || *p == "AGENTS.md" || *p == "TASK.md")
        .collect();
    step(
        &mut steps,
        "no protected path touched",
        protected.is_empty(),
        format!("{protected:?}"),
    );
    if !imp.outcome.success && changed.is_empty() {
        return finish(
            &root,
            steps,
            json!({"claude_mode": format!("{mode:?}"), "claude_modes": modes_note}),
        );
    }

    // 3. checkpoint (toryo owns commits) + canonical diff
    g.run(&wtl, false, &["add", "-A"])?;
    let no_change = g
        .output(&wtl, true, &["diff", "--cached", "--quiet"])?
        .status
        .success();
    let mut commit = vec![
        "commit",
        "-q",
        "--no-verify",
        "-m",
        "toryo(CALC-001): implement attempt 1",
        "--trailer",
        "Toryo-Task: CALC-001",
        "--trailer",
        "Toryo-Stage: implement",
        "--trailer",
        "Toryo-Attempt: 1",
    ];
    if no_change {
        commit.push("--allow-empty");
        commit.extend(["--trailer", "Toryo-No-Change: true"]);
    }
    g.run(&wtl, false, &commit)?;
    let checkpoint = g.run_str(&wtl, true, &["rev-parse", "HEAD"])?;
    let diff = g.canonical_diff(&wtl, "main", Some("HEAD"))?;
    std::fs::write(rs.join("DIFF.patch"), &diff)?;
    step(
        &mut steps,
        "checkpoint + canonical DIFF.patch",
        true,
        format!(
            "{} ({} bytes diff{})",
            &checkpoint[..12],
            diff.len(),
            if no_change { ", no-change" } else { "" }
        ),
    );

    // 4. verify (fixture scripts, in the worktree for the smoke test)
    let mut verify = Vec::new();
    let mut verify_ok = true;
    for s in ["scripts/build.ps1", "scripts/test.ps1"] {
        match run_verify(&wt.join(s), &wt) {
            Ok((code, out)) => {
                verify_ok &= code == Some(0);
                verify.push(json!({"script": s, "exit": code, "output": out}));
            }
            Err(e) => {
                verify_ok = false;
                verify.push(json!({"script": s, "error": format!("{e:#}")}));
            }
        }
    }
    let vr = json!({"head": checkpoint, "pass": verify_ok, "boundary": "none (smoke test)", "commands": verify});
    std::fs::write(
        rs.join("VERIFY_RESULT.json"),
        serde_json::to_vec_pretty(&vr)?,
    )?;
    step(
        &mut steps,
        "verify (build + test)",
        verify_ok,
        vr["commands"]
            .as_array()
            .map(|c| {
                c.iter()
                    .map(|x| format!("{}={}", x["script"], x["exit"]))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default(),
    );

    // 5. review (Claude, read-only), with the launch mode that worked for planning
    let reviewer_ctx = rs.join("context-review.md");
    std::fs::write(
        &reviewer_ctx,
        format!(
            "{}\n\n## Task pack\n- TASK.md: {}\n- PLAN.md: {}\n- DIFF.patch: {}\n- VERIFY_RESULT.json: {}\n",
            fixture::ROLES[2].1,
            rs.join("TASK.md").display(),
            rs.join("PLAN.md").display(),
            rs.join("DIFF.patch").display(),
            rs.join("VERIFY_RESULT.json").display()
        ),
    )?;
    let review_args = SmokeArgs {
        modes: vec![mode],
        claude_profile: a.claude_profile.clone(),
        codex_profile: a.codex_profile.clone(),
        claude_model: a.claude_model.clone(),
        codex_model: a.codex_model.clone(),
    };
    let (_, _, review) = claude_step(
        cfg,
        &review_args,
        &wt,
        &rs,
        &root,
        "reviewer",
        "Review the checkpoint in this worktree against TASK.md.",
        &reviewer_ctx,
        fixture::schema("review.schema.json")?,
    )?;
    match review {
        None => step(&mut steps, "review (Claude)", false, "no structured review"),
        Some(r) => {
            let v = r.outcome.structured.clone().unwrap_or(Value::Null);
            std::fs::write(rs.join("review.json"), serde_json::to_vec_pretty(&v)?)?;
            std::fs::write(rs.join("REVIEW.md"), render::render("review", &v)?)?;
            let ids: Vec<&str> = v["acceptance_criteria"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x["id"].as_str()).collect())
                .unwrap_or_default();
            let exact = {
                let mut s = ids.clone();
                s.sort();
                s == ["AC-1", "AC-2", "AC-3"]
            };
            step(
                &mut steps,
                "review (Claude)",
                r.outcome.success,
                format!("decision={} ACs={ids:?}", v["decision"]),
            );
            step(
                &mut steps,
                "review returns exactly AC-1..AC-3 (R28)",
                exact,
                format!("{ids:?}"),
            );
        }
    }
    finish(
        &root,
        steps,
        json!({"claude_mode": format!("{mode:?}"), "claude_modes": modes_note, "checkpoint": checkpoint, "runstore": rs}),
    )
}

fn finish(root: &Path, steps: Vec<Step>, extra: Value) -> Result<bool> {
    let pass = !steps.is_empty() && steps.iter().all(|s| s.ok);
    let mut rec = Record::new("smoke", None, &["runbook §5 (automated)", "M0-VERIFY 1"]);
    rec.pass = Some(pass);
    rec.summary = steps
        .iter()
        .map(|s| format!("{}={}", s.name, if s.ok { "ok" } else { "FAIL" }))
        .collect::<Vec<_>>()
        .join(", ");
    rec.details = json!({
        "steps": steps.iter().map(|s| json!({"step": s.name, "ok": s.ok, "detail": s.detail})).collect::<Vec<_>>(),
        "extra": extra,
    });
    rec.run_dir = Some(root.display().to_string());
    append(&rec)?;
    eprintln!(
        "\nsmoke: {}  (artifacts in {})",
        if pass { "PASS" } else { "FAIL" },
        root.join("runstore").display()
    );
    Ok(pass)
}

/// Ensure the configured exes exist before spending tokens.
pub fn preflight(cfg: &Effective, profiles: &[&str]) -> Result<()> {
    for p in profiles {
        let prof = cfg.profile(p)?;
        let exe = PathBuf::from(expand_vars(&prof.exe)?);
        if !exe.is_file() {
            bail!("profile `{p}`: exe not found: {}", exe.display());
        }
    }
    Ok(())
}
