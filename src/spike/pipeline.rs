//! `toryo spike pipeline` — the plan's workflow (§5.2) on a real repository, as a
//! single-process prototype of M2–M8:
//!
//! ```text
//! TASK.md → Claude plans (read-only) → HUMAN plan gate (approve / reject+reason)
//!   → Codex implements (sandboxed worktree) → toryo: HEAD guard + protected paths
//!   → checkpoint commit → Verify (commands.verify) → Claude reviews (exact AC set)
//!   → approved: done · changes_requested / verify fail: Codex fix → … (max N cycles)
//! ```
//!
//! What it deliberately does NOT have yet (M3+): SQLite state, crash recovery/resume,
//! immutable run-store versions, the Low-IL verify boundary, budget pause. Every turn's
//! stream, prompt and outcome is still captured under the run directory.

use crate::git::short;
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::fixture;
use super::render;
use crate::agents::hub::AgentHub;
use crate::config::{data_dir, expand_vars, CommandKind, CommandSpec, Effective, ProfileKind};
use crate::domain::{ErrorClass, OpenSession, Role, StageOutcome, TurnInput, TurnLimits};
use crate::engine::checks::{
    parse_acceptance_ids, resolve_program, review_violations, task_id, verify_commands,
    verify_files, Protected,
};
use crate::engine::{Machine, PipelineEvent as Ev, PipelineState as St};
use crate::evidence::{append, stamp, Record};
use crate::git::{parse_status_z, GitCmd, Loc};
use crate::process::env::build_env;
use crate::process::Boundary;
use crate::runner::{CommandResult, CommandRunner, LocalRunner, RunSpec};
use crate::usage::Buckets;

fn fwd(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

pub struct PipelineArgs {
    pub repo: PathBuf,
    pub task: PathBuf,
    pub id: Option<String>,
    pub base: Option<String>,
    pub claude_profile: String,
    /// Implementer profiles in order: the primary, then the fallbacks used when one runs
    /// out of quota (e.g. `codex-openai`, `claude-max`).
    pub implementers: Vec<String>,
    /// Test hook: treat the primary implementer as out of quota without calling it.
    pub simulate_quota: bool,
    pub claude_model: Option<String>,
    pub codex_model: Option<String>,
    /// Reviewer profile (`roles.reviewer`): an implementer fallback never changes it
    /// (owner decision 15).
    pub reviewer_profile: String,
    /// R51: CLI versions in use (bound into the pipeline record).
    pub cli_versions: BTreeMap<String, String>,
    pub max_fix_cycles: u32,
    /// Skip the human plan gate (demo / CI only).
    pub yes: bool,
}

fn run_command(hub: &AgentHub, spec: &CommandSpec, cwd: &Path) -> Result<CommandResult> {
    let CommandKind::Program { program, args } = spec.kind() else {
        bail!("{}: `spike pipeline` runs only `program` verify commands — `tool: xpp` runs in `toryo run` (R59)", spec.name);
    };
    let program = resolve_program(&expand_vars(program)?);
    let mut env = build_env(std::env::vars(), &Default::default(), &Default::default());
    env.insert("NO_COLOR".into(), "1".into());
    hub.block_on(LocalRunner.run(
        RunSpec {
            name: spec.name.clone(),
            program,
            args: args.to_vec(),
            cwd: cwd.to_path_buf(),
            env,
            // Prototype: the verify boundary (R36) is wired in M6 — see HUONG-DAN.md.
            boundary: Boundary::None,
            timeout: Duration::from_secs(60 * spec.timeout_min.unwrap_or(30)),
            cancel: None,
            redact: crate::process::redact::Redactions::empty(),
        },
        None,
    ))
}

fn ask(question: &str) -> Result<String> {
    eprint!("{question}");
    std::io::stderr().flush()?;
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line)? == 0 {
        eprintln!("(no input — stdin closed; treating as quit)");
        return Ok("q".into());
    }
    Ok(line.trim().to_string())
}

/// M2: every step of the prototype goes through the state machine (plan §5.2). A step the
/// machine refuses is a bug in the prototype, not a user error.
fn step(sm: &mut Machine, trail: &mut Vec<Value>, e: Ev) -> Result<St> {
    let from = sm.state();
    let name = e.name();
    let to = sm
        .apply(e)
        .map_err(|err| anyhow::anyhow!("state machine refused `{name}` in {from:?}: {err}"))?;
    eprintln!("   [state] {from:?} --{name}--> {to:?}");
    trail.push(json!({"from": from, "event": name, "to": to, "fix_cycle": sm.fix_cycle()}));
    Ok(to)
}

fn escalated(sm: &Machine) -> String {
    format!("escalated: {}", sm.reason().unwrap_or(""))
}

fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

struct Totals {
    tokens: Buckets,
}

impl Totals {
    fn add(&mut self, r: &StageOutcome, label: &str) {
        if let Some(b) = r.usage.and_then(|u| u.buckets) {
            self.tokens.add(&b);
        }
        eprintln!(
            "   ↳ {label}: {:?} usage {} tokens, {:.0}s",
            r.usage.map(|u| u.accounting),
            r.usage.map(|u| u.total()).unwrap_or(0),
            r.duration_ms as f64 / 1000.0
        );
    }
}

pub fn pipeline(cfg: &Effective, hub: &AgentHub, a: PipelineArgs) -> Result<bool> {
    let repo = std::path::absolute(&a.repo)?;
    if !repo.join(".git").exists() {
        bail!(
            "{} is not a Git repository (git init + first commit first)",
            repo.display()
        );
    }
    // --task may be relative to the current directory or to the repo
    let task_path = if a.task.exists() {
        a.task.clone()
    } else {
        repo.join(&a.task)
    };
    let task_text = std::fs::read_to_string(&task_path)
        .with_context(|| format!("reading {}", a.task.display()))?;
    let ac_ids = parse_acceptance_ids(&task_text)?;
    let id = task_id(&task_text, &task_path, a.id.as_deref());
    let g = fixture::gitcmd()?;
    let repol = Loc::Discover(repo.clone());
    GitCmd::check_submodules(&repo, cfg.repo.policy.allow_submodules)?;
    let base_ref = match &a.base {
        Some(b) => b.clone(),
        None => g.run_str(&repol, true, &["rev-parse", "--abbrev-ref", "HEAD"])?,
    };
    let base = g.run_str(&repol, true, &["rev-parse", &base_ref])?;
    // R37 admission: filters referenced by the repo must be trusted (or neutralised)
    let mut g = g;
    g.filters = g.plan_filters(
        &repo,
        &cfg.trusted_filters,
        cfg.repo.policy.neutralize_untrusted_filters,
    )?;
    let cmds = verify_commands(cfg, &repo)?;
    if let Some(c) = cmds.iter().find(|c| c.is_xpp()) {
        bail!("{}: `spike pipeline` runs only `program` verify commands — `tool: xpp` runs in `toryo run` (R59)", c.name);
    }
    let mut exact = verify_files(&cmds, &repo);
    if let Ok(rel) = std::path::absolute(&task_path)?.strip_prefix(&repo) {
        exact.push(fwd(rel)); // the task is the contract: agents may not edit it
    }
    let protect = Protected::new(exact, &cfg.repo.policy.protected_paths);

    let wt_root = PathBuf::from(expand_vars(&cfg.repo.project.worktrees_root)?);
    std::fs::create_dir_all(&wt_root)?;
    let wt = wt_root.join(&id);
    if wt.exists() {
        bail!("worktree {} already exists — use another --id, or remove it: git -C \"{}\" worktree remove --force \"{}\"", wt.display(), repo.display(), wt.display());
    }
    let branch = format!("toryo/{id}");
    if g.output(
        &repol,
        true,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )?
    .status
    .success()
    {
        bail!(
            "branch {branch} already exists in {} — use another --id or delete the branch",
            repo.display()
        );
    }
    let run = data_dir().join("runs").join(format!("{id}-{}", stamp()));
    let rs = run.join("runstore");
    std::fs::create_dir_all(&rs)?;
    std::fs::write(rs.join("TASK.md"), &task_text)?;
    let contracts = run.join("contracts");
    fixture::write_contracts(&contracts)?;
    g.run(
        &repol,
        false,
        &["worktree", "add", "-q", "-b", &branch, &fwd(&wt), &base],
    )?;
    let wtl = Loc::Discover(wt.clone());
    // Per-task TEMP inside the worktree, invisible to Git (info/exclude of the common dir).
    let task_tmp = wt.join(".toryo-tmp");
    {
        let common = g.run_str(
            &wtl,
            true,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        let excl = PathBuf::from(common.trim()).join("info").join("exclude");
        let cur = std::fs::read_to_string(&excl).unwrap_or_default();
        if !cur.lines().any(|l| l.trim() == "/.toryo-tmp/") {
            std::fs::create_dir_all(excl.parent().expect("parent"))?;
            std::fs::write(
                &excl,
                format!(
                    "{cur}{}/.toryo-tmp/\n",
                    if cur.is_empty() || cur.ends_with('\n') {
                        ""
                    } else {
                        "\n"
                    }
                ),
            )?;
        }
    }
    eprintln!(
        "\n■ {id}: repo {} · base {base_ref} ({}) · worktree {} · branch {branch}",
        repo.display(),
        short(&base),
        wt.display()
    );
    eprintln!(
        "  acceptance criteria: {ac_ids:?} · verify: {}",
        cmds.iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>()
            .join(" → ")
    );
    eprintln!("  run directory: {}", run.display());
    eprintln!("  implementer: {}", a.implementers.join(" → (fallback) "));

    let fin = Fin {
        run: run.clone(),
        id: id.clone(),
        branch: branch.clone(),
        repo: repo.clone(),
        base_ref: base_ref.clone(),
        wt: wt.clone(),
        cli_versions: a.cli_versions.clone(),
    };
    let mut totals = Totals {
        tokens: Buckets::default(),
    };
    let mut log: Vec<Value> = Vec::new();
    let mut sm = Machine::new(a.max_fix_cycles);
    let mut trail: Vec<Value> = Vec::new();
    let res = (|| -> Result<String> {
        let limits = |mins: u64| TurnLimits {
            max: Duration::from_secs(mins * 60),
            stall: Duration::from_secs(10 * 60),
        };

        // ── 1. PLAN (+ human gate) ───────────────────────────────────────────────────────
        let planner_ctx = rs.join("context-plan.md");
        std::fs::write(
            &planner_ctx,
            format!(
                "{}\n\n## Task pack\n- TASK.md: {}\n",
                fixture::ROLES[0].1,
                rs.join("TASK.md").display()
            ),
        )?;
        let mut plan_prompt = format!(
            "Create the plan for the task in {}.",
            rs.join("TASK.md").display()
        );
        let mut resume: Option<String> = None;
        step(&mut sm, &mut trail, Ev::Start)?;
        loop {
            // human rejections are unlimited; automatic ones are bounded by the machine
            let attempt = sm.plan_attempt();
            eprintln!("\n── PLAN (Claude, attempt {attempt}) ──");
            let r = hub.turn(
                OpenSession {
                    profile: a.claude_profile.clone(),
                    role: Role::Planner,
                    cwd: wt.clone(),
                    run_store: Some(rs.clone()),
                    capture_dir: run.clone(),
                    schema: Some(fixture::schema("plan.schema.json")?.into()),
                    context_file: Some(planner_ctx.clone()),
                    model: a.claude_model.clone(),
                    resume: resume.clone(),
                },
                TurnInput {
                    prompt: plan_prompt.clone(),
                    label: format!("plan-{attempt}"),
                    echo: true,
                    ..Default::default()
                },
                limits(20),
            )?;
            totals.add(&r, "planner");
            let Some(p) = r.structured.clone() else {
                r.print_summary();
                let reason = format!(
                    "planner returned no structured plan: {:?} {}",
                    r.error_class,
                    r.error_detail.clone().unwrap_or_default()
                );
                step(&mut sm, &mut trail, Ev::Escalate { reason })?;
                return Ok(escalated(&sm));
            };
            let plan_json = p;
            std::fs::write(rs.join("plan.json"), serde_json::to_vec_pretty(&plan_json)?)?;
            let plan_md = render::render("plan", &plan_json)?;
            std::fs::write(rs.join("PLAN.md"), &plan_md)?;
            let files: Vec<String> = plan_json["files_likely_involved"]
                .as_array()
                .map(|x| {
                    x.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let bad: Vec<&String> = files.iter().filter(|f| protect.is_protected(f)).collect();
            log.push(json!({"step": "plan", "attempt": attempt, "files": files, "protected": bad}));
            eprintln!("\n{plan_md}");
            if !bad.is_empty() {
                eprintln!("!! plan wants to touch protected paths {bad:?} — rejected automatically (plan-time policy)");
                let paths = bad.iter().map(|x| x.to_string()).collect();
                if step(&mut sm, &mut trail, Ev::PlanPolicyViolation { paths })? == St::Escalated {
                    return Ok(escalated(&sm));
                }
                plan_prompt = format!("Your plan lists protected paths {bad:?}, which may not change. Create the plan again without touching them.");
                resume = r.session_id.clone();
                continue;
            }
            let subject = digest(&std::fs::read(rs.join("plan.json"))?);
            step(
                &mut sm,
                &mut trail,
                Ev::PlanReady {
                    subject: subject.clone(),
                },
            )?;
            let occurrence = sm.gate().map(|g| g.occurrence).unwrap_or_default();
            if a.yes {
                eprintln!("(plan gate skipped: --yes)");
                step(
                    &mut sm,
                    &mut trail,
                    Ev::PlanApproved {
                        occurrence,
                        subject,
                    },
                )?;
                break;
            }
            let ans = ask(&format!(
                "\n▶ Approve this plan for {id}? [y]es / [n]o (you will give a reason) / [q]uit: "
            ))?;
            match ans.to_ascii_lowercase().as_str() {
                "y" | "yes" => {
                    log.push(json!({"step": "plan-gate", "decision": "approved"}));
                    step(
                        &mut sm,
                        &mut trail,
                        Ev::PlanApproved {
                            occurrence,
                            subject,
                        },
                    )?;
                    break;
                }
                "q" | "quit" => {
                    log.push(json!({"step": "plan-gate", "decision": "cancelled"}));
                    step(&mut sm, &mut trail, Ev::Cancel)?;
                    return Ok("cancelled at the plan gate".to_string());
                }
                _ => {
                    let reason = ask("   Reason / what to change: ")?;
                    log.push(
                        json!({"step": "plan-gate", "decision": "rejected", "reason": reason}),
                    );
                    step(
                        &mut sm,
                        &mut trail,
                        Ev::PlanRejected {
                            occurrence,
                            reason: reason.clone(),
                        },
                    )?;
                    plan_prompt = format!("The human reviewer rejected the plan. Reason: {reason}\nCreate the plan again, revised to address this.");
                    resume = r.session_id.clone();
                }
            }
        }

        // ── 2. IMPLEMENT → CHECKPOINT → VERIFY → REVIEW, with fix cycles ───────────────────
        let mut feedback: Option<String> = None;
        let mut last_reviewed: Option<String> = None;
        let mut same_vendor_noted = false;
        let mut imps = Implementers {
            chain: a.implementers.clone(),
            cur: 0,
            retries: 0,
            handoff: None,
        };
        loop {
            // the machine owns the cycle count and its bound (Implementing → 0, Fixing → n)
            if !sm.state().is_writing() {
                bail!("internal: implementer turn in state {:?}", sm.state());
            }
            let cycle = sm.fix_cycle();
            let stage = if cycle == 0 {
                "implement".to_string()
            } else {
                format!("fix-{cycle}")
            };
            let mut prompt = format!(
                "{}\n\n## Task pack (read-only)\n- TASK.md: {}\n- PLAN.md: {}\n",
                fixture::ROLES[1].1,
                rs.join("TASK.md").display(),
                rs.join("PLAN.md").display()
            );
            if let Some(f) = &feedback {
                prompt.push_str(&format!("\n## Fix cycle {cycle}\nThe previous checkpoint was not accepted. Address the following, then report the issue ids you fixed in `resolved`:\n\n{f}\n"));
            }
            let head_before = g.run_str(&wtl, true, &["rev-parse", "HEAD"])?;
            // fresh per-task temp dir for every implementer turn
            let _ = std::fs::remove_dir_all(&task_tmp);
            std::fs::create_dir_all(&task_tmp)?;
            let ctx = ImplCtx {
                task_env: [("TEMP", &task_tmp), ("TMP", &task_tmp)]
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.display().to_string()))
                    .collect(),
                g: &g,
                wtl: &wtl,
                wt: &wt,
                rs: &rs,
                run: &run,
                schema: fixture::schema("implementation.schema.json")?,
            };
            let Some((imp, implementer)) = implement_turn(
                cfg,
                hub,
                &a,
                &mut imps,
                &ctx,
                &prompt,
                &stage,
                &mut totals,
                &mut log,
            )?
            else {
                let reason = format!(
                    "all implementers are out of quota ({}); the worktree keeps any partial changes — rerun later with another --id, or add a fallback profile",
                    imps.chain.join(" → ")
                );
                step(&mut sm, &mut trail, Ev::Escalate { reason })?;
                return Ok(escalated(&sm));
            };
            if cfg.profile(&implementer)?.kind == cfg.profile(&a.reviewer_profile)?.kind
                && !same_vendor_noted
            {
                same_vendor_noted = true;
                // Owner decision 15: the configured reviewer stays; this is information only.
                eprintln!("note: implementer {implementer} and reviewer {} are the same vendor (information only; the configured reviewer is kept, in a fresh session).", a.reviewer_profile);
                log.push(json!({"step": stage, "note": "implementer and reviewer: same vendor"}));
            }
            log.push(json!({"step": stage, "implemented_by": implementer}));
            if let Some(v) = &imp.structured {
                std::fs::write(
                    rs.join(format!("implementation-{cycle}.json")),
                    serde_json::to_vec_pretty(v)?,
                )?;
            }
            // HEAD guard (§8): agents must not commit
            let head_after = g.run_str(&wtl, true, &["rev-parse", "HEAD"])?;
            if head_after != head_before {
                eprintln!(
                    "!! policy_violation: the agent moved HEAD — soft reset to {}",
                    short(&head_before)
                );
                g.run(&wtl, false, &["reset", "-q", "--soft", &head_before])?;
                log.push(json!({"step": stage, "policy_violation": "agent commit, soft-reset"}));
            }
            let changed = parse_status_z(&g.run(
                &wtl,
                true,
                &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
            )?);
            let prot: Vec<String> = changed
                .iter()
                .map(|c| c.1.clone())
                .filter(|p| protect.is_protected(p))
                .collect();
            if !prot.is_empty() {
                // Path policy (§8): nothing is committed; restore and send back to the implementer.
                eprintln!(
                    "!! protected paths changed: {prot:?} — restored, sent back to the implementer"
                );
                for p in &prot {
                    let _ = g.output(&wtl, false, &["checkout", "-q", "HEAD", "--", p]);
                    let f = wt.join(p);
                    if g.output(&wtl, true, &["ls-files", "--error-unmatch", p])?
                        .status
                        .success()
                    {
                        continue;
                    }
                    let _ = std::fs::remove_file(f);
                }
                feedback = Some(format!("- POLICY: you changed protected paths {prot:?}. They were restored. Do not modify them; achieve the task another way."));
                log.push(json!({"step": stage, "protected_changed": prot}));
                // owner decision (M2): a path-policy violation is a fix cycle
                if step(&mut sm, &mut trail, Ev::PathPolicyViolation { paths: prot })?
                    == St::Escalated
                {
                    return Ok(escalated(&sm));
                }
                continue;
            }
            if !imp.success && changed.is_empty() {
                imp.print_summary();
                let reason = format!(
                    "implementer failed: {:?} {}",
                    imp.error_class,
                    imp.error_detail.clone().unwrap_or_default()
                );
                step(&mut sm, &mut trail, Ev::Escalate { reason })?;
                return Ok(escalated(&sm));
            }
            // checkpoint (toryo owns commits; no-change → empty checkpoint, R40)
            step(&mut sm, &mut trail, Ev::ImplementationDone)?;
            g.run(&wtl, false, &["add", "-A"])?;
            let no_change = g
                .output(&wtl, true, &["diff", "--cached", "--quiet"])?
                .status
                .success();
            let msg = format!("toryo({id}): {stage}");
            let mut c: Vec<&str> = vec!["commit", "-q", "--no-verify", "-m", &msg];
            let t1 = format!("Toryo-Task: {id}");
            let t2 = format!("Toryo-Stage: {stage}");
            let t3 = format!("Toryo-Implementer: {implementer}");
            c.extend(["--trailer", &t1, "--trailer", &t2, "--trailer", &t3]);
            if no_change {
                c.extend(["--allow-empty", "--trailer", "Toryo-No-Change: true"]);
            }
            g.run(&wtl, false, &c)?;
            let checkpoint = g.run_str(&wtl, true, &["rev-parse", "HEAD"])?;
            step(
                &mut sm,
                &mut trail,
                Ev::Checkpointed {
                    head: checkpoint.clone(),
                },
            )?;
            let diff = g.canonical_diff(&wtl, &base, Some("HEAD"))?;
            std::fs::write(rs.join("DIFF.patch"), &diff)?;
            let inc = match &last_reviewed {
                Some(h) => g.canonical_diff(&wtl, h, Some("HEAD"))?,
                None => Vec::new(),
            };
            std::fs::write(rs.join("DIFF-incremental.patch"), &inc)?;
            eprintln!(
                "✔ checkpoint {} ({} file(s), {} bytes diff{})",
                short(&checkpoint),
                changed.len(),
                diff.len(),
                if no_change { ", no change" } else { "" }
            );

            // verify
            eprintln!("\n── VERIFY ──");
            let mut results = Vec::new();
            let mut pass = true;
            for cmd in &cmds {
                let res = run_command(hub, cmd, &wt)?;
                let (code, out, ms) = (res.exit, res.output.clone(), res.duration_ms);
                let ok = code == Some(0);
                pass &= ok;
                eprintln!(
                    "  {} {} (exit {:?}, {:.1}s)",
                    if ok { "✔" } else { "✘" },
                    cmd.name,
                    code,
                    ms as f64 / 1000.0
                );
                let tail: String = out
                    .lines()
                    .rev()
                    .take(40)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n");
                results.push(json!({"name": cmd.name, "argv": std::iter::once(cmd.program.clone().unwrap_or_default()).chain(cmd.args.clone()).collect::<Vec<_>>(), "exit": code, "ms": ms, "output_tail": tail}));
                if !ok {
                    break; // stop at first failure (§5.2)
                }
            }
            let vr = json!({"head": checkpoint, "pass": pass, "boundary": "none (prototype)", "commands": results});
            std::fs::write(
                rs.join("VERIFY_RESULT.json"),
                serde_json::to_vec_pretty(&vr)?,
            )?;
            log.push(json!({"step": stage, "checkpoint": checkpoint, "files": changed.iter().map(|c| &c.1).collect::<Vec<_>>(), "verify": pass}));
            if !pass {
                let failed = results.last().cloned().unwrap_or(Value::Null);
                feedback = Some(format!(
                    "- VERIFY FAILED: `{}` exited {:?}. Last output:\n```\n{}\n```",
                    failed["name"].as_str().unwrap_or(""),
                    failed["exit"],
                    failed["output_tail"].as_str().unwrap_or("")
                ));
                // → Fixing (never to Review, §5.2), or Escalated when no cycle is left
                if step(
                    &mut sm,
                    &mut trail,
                    Ev::VerifyFailed {
                        head: checkpoint.clone(),
                    },
                )? == St::Escalated
                {
                    return Ok(escalated(&sm));
                }
                continue;
            }
            step(
                &mut sm,
                &mut trail,
                Ev::VerifyPassed {
                    head: checkpoint.clone(),
                },
            )?;

            // review
            eprintln!("\n── REVIEW (Claude) ──");
            let reviewer_ctx = rs.join("context-review.md");
            std::fs::write(
            &reviewer_ctx,
            format!(
                "{}\n\n## Task pack\n- TASK.md: {}\n- PLAN.md: {}\n- DIFF.patch (base → checkpoint): {}\n- DIFF-incremental.patch (since your last review): {}\n- VERIFY_RESULT.json: {}\n",
                fixture::ROLES[2].1,
                rs.join("TASK.md").display(),
                rs.join("PLAN.md").display(),
                rs.join("DIFF.patch").display(),
                rs.join("DIFF-incremental.patch").display(),
                rs.join("VERIFY_RESULT.json").display()
            ),
        )?;
            let mut rprompt = format!("Review the checkpoint {} in this worktree against TASK.md. Acceptance criteria ids: {ac_ids:?}.", short(&checkpoint));
            let mut rresume = None;
            let mut review = Value::Null;
            for corr in 0..=2 {
                let r = hub.turn(
                    OpenSession {
                        profile: a.reviewer_profile.clone(),
                        role: Role::Reviewer,
                        cwd: wt.clone(),
                        run_store: Some(rs.clone()),
                        capture_dir: run.clone(),
                        schema: Some(fixture::schema("review.schema.json")?.into()),
                        context_file: Some(reviewer_ctx.clone()),
                        model: a.claude_model.clone(),
                        resume: rresume.clone(),
                    },
                    TurnInput {
                        prompt: rprompt.clone(),
                        label: format!("review-{cycle}-{corr}"),
                        echo: true,
                        ..Default::default()
                    },
                    limits(20),
                )?;
                totals.add(&r, "reviewer");
                review = r.structured.clone().unwrap_or(Value::Null);
                let viol = review_violations(&review, &ac_ids);
                if viol.is_empty() {
                    break;
                }
                eprintln!("!! invalid review (correction {}/2): {viol:?}", corr + 1);
                if corr == 2 {
                    let reason = format!("InvalidOutput: review rejected 3 times: {viol:?}");
                    step(&mut sm, &mut trail, Ev::Escalate { reason })?;
                    return Ok(escalated(&sm));
                }
                rprompt = format!("Your previous final response was rejected and was not used. Validation error: {}. Return only a corrected JSON object matching the schema.", viol.join("; "));
                rresume = r.session_id.clone();
            }
            std::fs::write(rs.join("review.json"), serde_json::to_vec_pretty(&review)?)?;
            let review_md = render::render("review", &review)?;
            std::fs::write(rs.join("REVIEW.md"), &review_md)?;
            eprintln!("\n{review_md}");
            last_reviewed = Some(checkpoint.clone());
            let mut decision = review["decision"].as_str().unwrap_or("").to_string();
            let unmet: Vec<String> = review["acceptance_criteria"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|x| x["met"].as_bool() != Some(true))
                .filter_map(|x| x["id"].as_str().map(str::to_string))
                .collect();
            let blocking = review["issues"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|i| i["blocking"].as_bool() == Some(true));
            if decision == "approved" && (!unmet.is_empty() || blocking) {
                eprintln!("!! review_inconsistent: `approved` with unmet {unmet:?} / blocking issues → changes_requested");
                decision = "changes_requested".into();
            }
            let issues: Vec<String> = review["issues"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|i| {
                    format!(
                        "{} [{}{}] {}{}",
                        i["id"].as_str().unwrap_or(""),
                        i["severity"].as_str().unwrap_or(""),
                        if i["blocking"].as_bool() == Some(true) {
                            ", blocking"
                        } else {
                            ""
                        },
                        i["file"]
                            .as_str()
                            .map(|x| format!("`{x}`: "))
                            .unwrap_or_default(),
                        i["description"].as_str().unwrap_or("")
                    )
                })
                .collect();
            log.push(json!({"step": format!("review-{cycle}"), "decision": decision, "unmet": unmet, "issues": issues}));
            match decision.as_str() {
                "approved" => {
                    step(
                        &mut sm,
                        &mut trail,
                        Ev::ReviewApproved {
                            head: checkpoint.clone(),
                        },
                    )?;
                    // ── final gate (§5.2; R53: open review issues were printed above) ──
                    let occurrence = sm.gate().map(|g| g.occurrence).unwrap_or_default();
                    let subject = checkpoint.clone();
                    let ans = if a.yes {
                        eprintln!("(final gate skipped: --yes)");
                        "y".to_string()
                    } else {
                        ask(&format!(
                            "\n▶ Accept the result of {id} at checkpoint {}? [y]es / [n]o (you will give a reason) / [q]uit: ",
                            short(&checkpoint)
                        ))?
                    };
                    match ans.to_ascii_lowercase().as_str() {
                        "y" | "yes" => {
                            log.push(json!({"step": "final-gate", "decision": "approved"}));
                            step(
                                &mut sm,
                                &mut trail,
                                Ev::FinalApproved {
                                    occurrence,
                                    subject,
                                },
                            )?;
                            return Ok(format!("approved at checkpoint {}", short(&checkpoint)));
                        }
                        "q" | "quit" => {
                            log.push(json!({"step": "final-gate", "decision": "cancelled"}));
                            step(&mut sm, &mut trail, Ev::Cancel)?;
                            return Ok("cancelled at the final gate".to_string());
                        }
                        _ => {
                            let reason = ask("   Reason / what to change: ")?;
                            log.push(json!({"step": "final-gate", "decision": "rejected", "reason": reason}));
                            // a final rejection is a fix cycle (§5.2)
                            if step(
                                &mut sm,
                                &mut trail,
                                Ev::FinalRejected {
                                    occurrence,
                                    reason: reason.clone(),
                                },
                            )? == St::Escalated
                            {
                                return Ok(escalated(&sm));
                            }
                            feedback = Some(format!("- HUMAN (final gate): {reason}\n"));
                        }
                    }
                }
                "blocked" => {
                    step(
                        &mut sm,
                        &mut trail,
                        Ev::ReviewBlocked {
                            head: checkpoint.clone(),
                        },
                    )?;
                    return Ok(format!(
                        "escalated: reviewer blocked: {}",
                        review["summary"].as_str().unwrap_or("")
                    ));
                }
                _ => {
                    let mut f = String::new();
                    for ac in review["acceptance_criteria"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|x| x["met"].as_bool() != Some(true))
                    {
                        f.push_str(&format!(
                            "- {} NOT MET: {}\n",
                            ac["id"].as_str().unwrap_or(""),
                            ac["evidence"].as_str().unwrap_or("")
                        ));
                    }
                    for i in review["issues"].as_array().into_iter().flatten() {
                        f.push_str(&format!(
                            "- {} [{}{}] {}{}\n",
                            i["id"].as_str().unwrap_or(""),
                            i["severity"].as_str().unwrap_or(""),
                            if i["blocking"].as_bool() == Some(true) {
                                ", blocking"
                            } else {
                                ""
                            },
                            i["file"]
                                .as_str()
                                .map(|x| format!("{x}: "))
                                .unwrap_or_default(),
                            i["description"].as_str().unwrap_or("")
                        ));
                    }
                    feedback = Some(f);
                    if step(
                        &mut sm,
                        &mut trail,
                        Ev::ChangesRequested {
                            head: checkpoint.clone(),
                        },
                    )? == St::Escalated
                    {
                        return Ok(escalated(&sm));
                    }
                }
            }
        }
    })();
    if hub.is_fake() {
        // scripted agents: what each role was asked, for the FakeAgent e2e tests
        std::fs::write(
            run.join("fake-calls.json"),
            serde_json::to_vec_pretty(&hub.fake_calls())?,
        )?;
    }
    let why = match res {
        Ok(why) => why,
        Err(e) => {
            eprintln!("error: {e:#}");
            let why = format!("error: {e:#}");
            if !sm.state().is_terminal() {
                let _ = step(&mut sm, &mut trail, Ev::Fail { error: why.clone() });
            }
            why
        }
    };
    finish(&fin, &sm, &why, &totals, log, trail)
}

/// The implementer chain (primary + fallbacks). The position is sticky for the whole run:
/// once an agent is out of quota, later fix cycles go straight to the next one.
struct Implementers {
    chain: Vec<String>,
    cur: usize,
    retries: u32,
    /// Hand-over note for the first turn of the next agent after a switch.
    handoff: Option<String>,
}

struct ImplCtx<'a> {
    /// Per-task TEMP/TMP inside the worktree (M0 finding: the Codex sandbox cannot write
    /// %TEMP%, so tests that create temp folders failed until the agent redirected TEMP).
    task_env: BTreeMap<String, String>,
    g: &'a GitCmd,
    wtl: &'a Loc,
    wt: &'a Path,
    rs: &'a Path,
    run: &'a Path,
    schema: &'static str,
}

/// Extra instructions when Claude is the Implementer: it has file tools but no shell, and
/// no access to the run store, so the task pack is inlined.
fn claude_implementer_prompt(base: &str, rs: &Path) -> Result<String> {
    let task = std::fs::read_to_string(rs.join("TASK.md"))?;
    let plan = std::fs::read_to_string(rs.join("PLAN.md"))?;
    Ok(format!(
        "{base}\n## Session notes (Claude implementer)\n\
         - Your working directory is the task worktree. You can Read, Grep, Glob, Edit and Write files there only.\n\
         - You have NO shell in this session: do not try to run commands. The orchestrator runs build/test after you \
         and sends failures back. Put what you checked by reading code in `validation_performed`.\n\
         - Read `AGENTS.md` in the worktree first if it exists and follow it.\n\
         - The paths above are not accessible to you; their content follows.\n\n\
         ## TASK.md\n\n{task}\n\n## PLAN.md\n\n{plan}\n"
    ))
}

/// Run one implementer turn, switching down the chain when an agent is out of quota.
/// `None` = every implementer is exhausted.
#[allow(clippy::too_many_arguments)]
fn implement_turn(
    cfg: &Effective,
    hub: &AgentHub,
    a: &PipelineArgs,
    imps: &mut Implementers,
    c: &ImplCtx,
    base_prompt: &str,
    stage: &str,
    totals: &mut Totals,
    log: &mut Vec<Value>,
) -> Result<Option<(StageOutcome, String)>> {
    let retry_base: u64 = std::env::var("TORYO_RETRY_BASE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let mut attempt = 0;
    loop {
        let Some(profile) = imps.chain.get(imps.cur).cloned() else {
            return Ok(None);
        };
        let kind = cfg.profile(&profile)?.kind;
        // label by turns actually run (a simulated skip does not use a number)
        let label = if attempt == 0 {
            stage.to_string()
        } else {
            format!("{stage}-{}", attempt + 1)
        };
        let who = match kind {
            ProfileKind::Codex => "Codex",
            ProfileKind::Claude => "Claude",
        };
        eprintln!("\n── {} ({who}: {profile}) ──", stage.to_uppercase());
        let reason = if a.simulate_quota && imps.cur == 0 {
            eprintln!("(simulated: {profile} is out of quota)");
            Some("quota exhausted (simulated)".to_string())
        } else {
            let mut prompt = base_prompt.to_string();
            if let Some(h) = &imps.handoff {
                prompt.push_str(h);
            }
            let (prompt, model) = match kind {
                ProfileKind::Codex => (prompt, a.codex_model.clone()),
                // R49: no shell, no run store — the task pack is inlined
                ProfileKind::Claude => (
                    claude_implementer_prompt(&prompt, c.rs)?,
                    a.claude_model.clone(),
                ),
            };
            let r = hub.turn(
                OpenSession {
                    profile: profile.clone(),
                    role: Role::Implementer,
                    cwd: c.wt.to_path_buf(),
                    run_store: None,
                    capture_dir: c.run.to_path_buf(),
                    schema: Some(c.schema.to_string()),
                    context_file: None,
                    model,
                    resume: None,
                },
                TurnInput {
                    prompt,
                    label: label.clone(),
                    env: c.task_env.clone(),
                    echo: true,
                },
                TurnLimits {
                    max: Duration::from_secs(30 * 60),
                    stall: Duration::from_secs(10 * 60),
                },
            )?;
            attempt += 1;
            totals.add(&r, "implementer");
            match r.error_class {
                Some(ErrorClass::QuotaExceeded) => Some(format!(
                    "quota exhausted: {}",
                    r.error_detail.clone().unwrap_or_default()
                )),
                Some(ErrorClass::RateLimited | ErrorClass::Overloaded) if imps.retries < 2 => {
                    imps.retries += 1;
                    let wait = retry_base * u64::from(imps.retries);
                    eprintln!(
                        "!! {profile}: {:?} — retry {}/2 in {wait}s",
                        r.error_class, imps.retries
                    );
                    log.push(json!({"step": stage, "implementer": profile, "retry": imps.retries, "error": r.error_class}));
                    std::thread::sleep(Duration::from_secs(wait));
                    continue;
                }
                Some(ErrorClass::RateLimited | ErrorClass::Overloaded) => {
                    Some(format!("{:?} after 2 retries", r.error_class.unwrap()))
                }
                _ => {
                    imps.retries = 0;
                    imps.handoff = None;
                    return Ok(Some((r, profile)));
                }
            }
        };
        // switch to the next implementer, keeping the partial changes in the worktree
        let reason = reason.unwrap_or_default();
        imps.cur += 1;
        imps.retries = 0;
        let next = imps.chain.get(imps.cur).cloned();
        let changed: Vec<String> = parse_status_z(&c.g.run(
            c.wtl,
            true,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )?)
        .into_iter()
        .map(|(st, p)| format!("{st} {p}"))
        .collect();
        eprintln!(
            "!! implementer {profile}: {reason} → {}",
            next.as_deref()
                .map(|n| format!("switching to {n}"))
                .unwrap_or_else(|| "no fallback left".into())
        );
        log.push(json!({"step": stage, "implementer_switch": {"from": profile, "to": next, "reason": reason, "partial_changes": changed}}));
        imps.handoff = Some(if changed.is_empty() {
            format!("\n## Hand-over\nThe previous implementer (`{profile}`) stopped before producing any change ({reason}). Start the task from the plan.\n")
        } else {
            format!(
                "\n## Hand-over\nThe previous implementer (`{profile}`) stopped before finishing ({reason}). The worktree contains its UNCOMMITTED partial changes:\n{}\nInspect them, keep what is correct, fix or complete the rest, and finish the task.\n",
                changed.iter().map(|x| format!("- `{x}`")).collect::<Vec<_>>().join("\n")
            )
        });
    }
}

struct Fin {
    run: PathBuf,
    id: String,
    branch: String,
    repo: PathBuf,
    base_ref: String,
    wt: PathBuf,
    cli_versions: BTreeMap<String, String>,
}

fn finish(
    f: &Fin,
    sm: &Machine,
    why: &str,
    totals: &Totals,
    log: Vec<Value>,
    trail: Vec<Value>,
) -> Result<bool> {
    let state = sm.state();
    let ok = state == St::Completed;
    let (run, id, branch, repo, base) = (&f.run, &f.id, &f.branch, &f.repo, &f.base_ref);
    let t = &totals.tokens;
    let summary = format!(
        "# {id} — {} ({state:?})\n\n{why}\n\n- branch: `{branch}` in `{}` (base `{base}`)\n- tokens: {} total (uncached in {}, cache read {}, cache write {}, out {})\n- artifacts: `{}`\n",
        if ok { "APPROVED" } else { "STOPPED" },
        repo.display(),
        t.total(),
        t.uncached_input,
        t.cache_read,
        t.cache_write,
        t.output,
        run.join("runstore").display()
    );
    let mut summary = summary;
    let switches: Vec<String> = log
        .iter()
        .filter_map(|v| {
            v.get("implementer_switch")
                .map(|s| (v["step"].clone(), s.clone()))
        })
        .map(|(st, s)| {
            format!(
                "- {}: `{}` → `{}` ({})",
                st.as_str().unwrap_or(""),
                s["from"].as_str().unwrap_or(""),
                s["to"].as_str().unwrap_or("none"),
                s["reason"].as_str().unwrap_or("")
            )
        })
        .collect();
    // Issues the last review left open (non-blocking when approved) — follow-up candidates.
    if let Some(last) = log.iter().rev().find(|v| v.get("issues").is_some()) {
        let issues: Vec<&str> = last["issues"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if !issues.is_empty() {
            summary.push_str(&format!(
                "\n## Open review issues ({})\n{}\n",
                last["step"].as_str().unwrap_or(""),
                issues
                    .iter()
                    .map(|i| format!("- {i}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
    }
    if !switches.is_empty() {
        summary.push_str(&format!(
            "\n## Implementer fallback\n{}\n",
            switches.join("\n")
        ));
    }
    std::fs::write(run.join("runstore").join("SUMMARY.md"), &summary)?;
    let mut rec = Record::new("pipeline", None, &["pipeline prototype"]);
    rec.pass = Some(ok);
    rec.summary = format!("{id}: {why}");
    rec.details = json!({
        "id": id, "branch": branch, "base": base, "repo": repo, "steps": log, "tokens": t,
        "state": state, "fix_cycle": sm.fix_cycle(), "states": trail,
    });
    rec.run_dir = Some(run.display().to_string());
    rec.cli_versions = f.cli_versions.clone();
    append(&rec)?;
    eprintln!(
        "\n══════════ {id}: {} ══════════\n{why}",
        if ok { "APPROVED ✔" } else { "STOPPED" }
    );
    eprintln!(
        "tokens this run: {} · artifacts: {}",
        t.total(),
        run.join("runstore").display()
    );
    let r = repo.display();
    if ok {
        eprintln!("\nNext (you decide — toryo never merges or pushes):");
        eprintln!("  git -C \"{r}\" log --oneline {base}..{branch}      # the checkpoints");
        eprintln!("  git -C \"{r}\" diff {base}...{branch}              # the full change");
        eprintln!("  git -C \"{r}\" merge --squash {branch}; git -C \"{r}\" commit -m \"{id}\"");
    } else {
        eprintln!("\nThe worktree and branch `{branch}` are kept for inspection. To discard them:");
    }
    eprintln!("  git -C \"{r}\" worktree remove --force \"{}\"; git -C \"{r}\" branch -D {branch}   # clean up", f.wt.display());
    Ok(ok)
}
