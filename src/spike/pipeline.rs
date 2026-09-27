//! `thien spike pipeline` — the plan's workflow (§5.2) on a real repository, as a
//! single-process prototype of M2–M8:
//!
//! ```text
//! TASK.md → Claude plans (read-only) → HUMAN plan gate (approve / reject+reason)
//!   → Codex implements (sandboxed worktree) → thien: HEAD guard + protected paths
//!   → checkpoint commit → Verify (commands.verify) → Claude reviews (exact AC set)
//!   → approved: done · changes_requested / verify fail: Codex fix → … (max N cycles)
//! ```
//!
//! What it deliberately does NOT have yet (M3+): SQLite state, crash recovery/resume,
//! immutable run-store versions, the Low-IL verify boundary, budget pause. Every turn's
//! stream, prompt and outcome is still captured under the run directory.

use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::agents::{claude_turn, codex_turn, ClaudeArgs, CodexArgs};
use super::fixture;
use super::render;
use super::results::{append, stamp, Record};
use super::turn::{print_summary, TurnOpts, TurnRun};
use crate::agents::claude::LaunchMode;
use crate::config::{data_dir, expand_vars, CommandSpec, Effective};
use crate::git::{parse_status_z, GitCmd, Loc};
use crate::process::env::build_env;
use crate::process::redact::Redactions;
use crate::process::{spawn, Boundary, Chunk, SpawnSpec, WaitOptions};
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
    pub codex_profile: String,
    pub claude_model: Option<String>,
    pub codex_model: Option<String>,
    pub mode: LaunchMode,
    pub max_fix_cycles: u32,
    /// Skip the human plan gate (demo / CI only).
    pub yes: bool,
}

/// `AC-<n>` ids under `## Acceptance criteria` (R28). At least one, unique.
pub fn parse_acceptance_ids(task: &str) -> Result<Vec<String>> {
    let re = regex::Regex::new(r"(?m)^\s*[-*]?\s*\**(AC-[1-9][0-9]*)\**\s*[:.)]").expect("regex");
    let mut in_section = false;
    let mut ids = Vec::new();
    for line in task.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            in_section = t
                .trim_start_matches('#')
                .trim()
                .to_ascii_lowercase()
                .starts_with("acceptance criteria");
            continue;
        }
        if in_section {
            if let Some(c) = re.captures(line) {
                ids.push(c[1].to_string());
            }
        }
    }
    if ids.is_empty() {
        bail!("TASK.md has no `AC-<n>:` items under `## Acceptance criteria` (R28)");
    }
    let uniq: BTreeSet<&String> = ids.iter().collect();
    if uniq.len() != ids.len() {
        bail!("TASK.md has duplicate acceptance-criteria ids: {ids:?}");
    }
    Ok(ids)
}

/// Task id: `--id`, else the `# <ID> —` heading, else the file stem.
fn task_id(task_text: &str, task_path: &Path, explicit: Option<&str>) -> String {
    if let Some(i) = explicit {
        return i.to_string();
    }
    let re = regex::Regex::new(r"(?m)^#\s+([A-Za-z][A-Za-z0-9_-]*-[0-9]+)\b").expect("regex");
    if let Some(c) = re.captures(task_text) {
        return c[1].to_string();
    }
    task_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "TASK".into())
}

/// Review checks in code (plan §9 R28). Returns violations (empty = valid).
pub fn review_violations(v: &Value, ac_ids: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let decision = v["decision"].as_str().unwrap_or("");
    if !matches!(decision, "approved" | "changes_requested" | "blocked") {
        out.push(format!(
            "decision must be approved|changes_requested|blocked, got `{decision}`"
        ));
    }
    let acs = v["acceptance_criteria"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let got: Vec<String> = acs
        .iter()
        .filter_map(|a| a["id"].as_str().map(str::to_string))
        .collect();
    let want: BTreeSet<&String> = ac_ids.iter().collect();
    let have: BTreeSet<&String> = got.iter().collect();
    let missing: Vec<&&String> = want.difference(&have).collect();
    let extra: Vec<&&String> = have.difference(&want).collect();
    if !missing.is_empty() {
        out.push(format!("missing acceptance criteria: {missing:?}"));
    }
    if !extra.is_empty() {
        out.push(format!("unknown acceptance criteria: {extra:?}"));
    }
    if have.len() != got.len() {
        out.push("duplicate acceptance-criteria ids".into());
    }
    for a in &acs {
        let ev = a["evidence"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if ev.is_empty()
            || (a["met"].as_bool() == Some(true)
                && matches!(ev.as_str(), "n/a" | "-" | "ok" | "yes"))
        {
            out.push(format!(
                "{}: evidence is empty or a placeholder",
                a["id"].as_str().unwrap_or("?")
            ));
        }
    }
    let issues = v["issues"].as_array().cloned().unwrap_or_default();
    if decision == "changes_requested"
        && issues.is_empty()
        && acs.iter().all(|a| a["met"].as_bool() == Some(true))
    {
        out.push("changes_requested needs at least one unmet criterion or one issue".into());
    }
    out
}

/// Protected paths (plan §8/§14, simplified): verify scripts, the task file, instruction /
/// config files, and `policy.protected_paths` globs from thien.yaml.
pub struct Protected {
    exact: Vec<String>,
    globs: Vec<regex::Regex>,
}

fn glob_to_regex(g: &str) -> regex::Regex {
    let g = g.replace('\\', "/");
    let mut re = String::from("^");
    let mut chars = g.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    chars.next();
                    re.push_str("(?:.*/)?");
                } else {
                    re.push_str(".*");
                }
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    regex::Regex::new(&re).unwrap_or_else(|_| regex::Regex::new("^$").expect("regex"))
}

impl Protected {
    pub fn new(exact: Vec<String>, globs: &[String]) -> Self {
        Self {
            exact: exact.into_iter().map(|e| e.replace('\\', "/")).collect(),
            globs: globs.iter().map(|g| glob_to_regex(g)).collect(),
        }
    }

    pub fn is_protected(&self, path: &str) -> bool {
        let p = path.replace('\\', "/");
        let name = p.rsplit('/').next().unwrap_or(&p);
        self.exact.contains(&p)
            || self.globs.iter().any(|g| g.is_match(&p))
            || matches!(
                name,
                "AGENTS.md"
                    | "AGENTS.override.md"
                    | "CLAUDE.md"
                    | "CLAUDE.local.md"
                    | ".mcp.json"
                    | ".gitattributes"
                    | "thien.yaml"
                    | "thien.local.yaml"
                    | ".gitmodules"
            )
            || [".claude/", ".codex/", ".thien/", ".githooks/", ".git/"]
                .iter()
                .any(|d| p.starts_with(d) || p.contains(&format!("/{d}")))
    }
}

fn resolve_program(p: &str) -> PathBuf {
    let pb = PathBuf::from(p);
    if pb.is_absolute() || p.contains('/') || p.contains('\\') {
        return pb;
    }
    super::smoke::which(p).unwrap_or(pb)
}

fn run_command(spec: &CommandSpec, cwd: &Path) -> Result<(Option<i32>, String, u128)> {
    let program = resolve_program(&expand_vars(&spec.program)?);
    let s = SpawnSpec {
        program,
        args: spec.args.clone(),
        cwd: cwd.to_path_buf(),
        env: {
            let mut e = build_env(std::env::vars(), &Default::default(), &Default::default());
            e.insert("NO_COLOR".into(), "1".into());
            e
        },
        stdin: None,
        // Prototype: the verify boundary (R36) is not applied yet — see HUONG-DAN.md.
        boundary: Boundary::None,
    };
    let r = spawn(&s, &Redactions::empty())
        .with_context(|| format!("starting verify command `{}`", spec.name))?;
    let mut out = Vec::new();
    let info = r.wait(
        WaitOptions {
            max: Some(Duration::from_secs(60 * spec.timeout_min.unwrap_or(30))),
            ..Default::default()
        },
        |c| {
            let (Chunk::Stdout(b) | Chunk::Stderr(b)) = c;
            out.extend(b)
        },
    )?;
    Ok((
        if info.timed_out { None } else { info.code },
        String::from_utf8_lossy(&out).into_owned(),
        info.duration_ms,
    ))
}

/// `commands.verify` from the config, else `scripts/build.ps1` + `scripts/test.ps1` if they exist.
fn verify_commands(cfg: &Effective, repo: &Path) -> Result<Vec<CommandSpec>> {
    if !cfg.repo.commands.verify.is_empty() {
        return Ok(cfg.repo.commands.verify.clone());
    }
    let mut v = Vec::new();
    for (name, rel) in [("build", "scripts/build.ps1"), ("test", "scripts/test.ps1")] {
        if repo.join(rel).exists() {
            v.push(CommandSpec {
                name: name.into(),
                program: "pwsh".into(),
                args: vec![
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-File".into(),
                    rel.into(),
                ],
                timeout_min: Some(30),
            });
        }
    }
    if v.is_empty() {
        bail!("no verify commands: add `commands.verify` to thien.yaml (or scripts/build.ps1 / scripts/test.ps1 to the repo)");
    }
    Ok(v)
}

fn verify_files(cmds: &[CommandSpec], repo: &Path) -> Vec<String> {
    cmds.iter()
        .flat_map(|c| c.args.iter())
        .filter(|a| repo.join(a).is_file())
        .map(|a| a.replace('\\', "/"))
        .collect()
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

struct Totals {
    tokens: Buckets,
}

impl Totals {
    fn add(&mut self, r: &TurnRun, label: &str) {
        if let Some(u) = r.outcome.usage {
            self.tokens.add(&u);
        }
        eprintln!(
            "   ↳ {label}: {:?} usage {} tokens, {:.0}s",
            r.outcome.accounting,
            r.outcome.usage.map(|u| u.total()).unwrap_or(0),
            r.exit.duration_ms as f64 / 1000.0
        );
    }
}

pub fn pipeline(cfg: &Effective, a: PipelineArgs) -> Result<bool> {
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
    let branch = format!("thien/{id}");
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
    eprintln!(
        "\n■ {id}: repo {} · base {base_ref} ({}) · worktree {} · branch {branch}",
        repo.display(),
        &base[..12],
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

    let fin = Fin {
        run: run.clone(),
        id: id.clone(),
        branch: branch.clone(),
        repo: repo.clone(),
        base_ref: base_ref.clone(),
        wt: wt.clone(),
    };
    let mut totals = Totals {
        tokens: Buckets::default(),
    };
    let mut log: Vec<Value> = Vec::new();
    let res = (|| -> Result<(bool, String)> {
        let turn_opts = |mins: u64| TurnOpts {
            max: Duration::from_secs(mins * 60),
            ..Default::default()
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
        let mut approved = false;
        for attempt in 1..=3 {
            eprintln!("\n── PLAN (Claude, attempt {attempt}) ──");
            let r = claude_turn(
                cfg,
                ClaudeArgs {
                    profile: a.claude_profile.clone(),
                    worktree: wt.clone(),
                    run_store: rs.clone(),
                    prompt: plan_prompt.clone(),
                    schema: Some(fixture::schema("plan.schema.json")?.into()),
                    context_file: Some(planner_ctx.clone()),
                    model: a.claude_model.clone(),
                    mode: a.mode,
                    resume: resume.clone(),
                    drop_flags: vec![],
                    extra_args: vec![],
                    label: format!("plan-{attempt}"),
                    opts: turn_opts(20),
                },
                &run,
            )?;
            totals.add(&r, "planner");
            let Some(p) = r.outcome.structured.clone() else {
                print_summary(&r);
                bail!(
                    "planner returned no structured plan: {:?} {}",
                    r.outcome.error_class,
                    r.outcome.error_detail.unwrap_or_default()
                );
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
                plan_prompt = format!("Your plan lists protected paths {bad:?}, which may not change. Create the plan again without touching them.");
                resume = r.outcome.session_id.clone();
                continue;
            }
            if a.yes {
                eprintln!("(plan gate skipped: --yes)");
                approved = true;
                break;
            }
            let ans = ask(&format!(
                "\n▶ Approve this plan for {id}? [y]es / [n]o (you will give a reason) / [q]uit: "
            ))?;
            match ans.to_ascii_lowercase().as_str() {
                "y" | "yes" => {
                    log.push(json!({"step": "plan-gate", "decision": "approved"}));
                    approved = true;
                    break;
                }
                "q" | "quit" => {
                    log.push(json!({"step": "plan-gate", "decision": "cancelled"}));
                    return Ok((false, "cancelled at the plan gate".to_string()));
                }
                _ => {
                    let reason = ask("   Reason / what to change: ")?;
                    log.push(
                        json!({"step": "plan-gate", "decision": "rejected", "reason": reason}),
                    );
                    plan_prompt = format!("The human reviewer rejected the plan. Reason: {reason}\nCreate the plan again, revised to address this.");
                    resume = r.outcome.session_id.clone();
                }
            }
        }
        if !approved {
            return Ok((false, "plan not approved after 3 attempts".to_string()));
        }

        // ── 2. IMPLEMENT → CHECKPOINT → VERIFY → REVIEW, with fix cycles ───────────────────
        let impl_schema = contracts.join("schemas").join("implementation.schema.json");
        let mut feedback: Option<String> = None;
        let mut last_reviewed: Option<String> = None;
        for cycle in 0..=a.max_fix_cycles {
            let stage = if cycle == 0 {
                "implement".to_string()
            } else {
                format!("fix-{cycle}")
            };
            eprintln!("\n── {} (Codex) ──", stage.to_uppercase());
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
            let imp = codex_turn(
                cfg,
                CodexArgs {
                    profile: a.codex_profile.clone(),
                    worktree: wt.clone(),
                    prompt,
                    schema_path: Some(impl_schema.clone()),
                    model: a.codex_model.clone(),
                    resume: None,
                    extra_args: vec![],
                    label: stage.clone(),
                    opts: turn_opts(30),
                },
                &run,
            )?;
            totals.add(&imp, "implementer");
            if let Some(v) = &imp.outcome.structured {
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
                    &head_before[..12]
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
                continue;
            }
            if !imp.outcome.success && changed.is_empty() {
                print_summary(&imp);
                return Ok((
                    false,
                    format!(
                        "implementer failed: {:?} {}",
                        imp.outcome.error_class,
                        imp.outcome.error_detail.clone().unwrap_or_default()
                    ),
                ));
            }
            // checkpoint (thien owns commits; no-change → empty checkpoint, R40)
            g.run(&wtl, false, &["add", "-A"])?;
            let no_change = g
                .output(&wtl, true, &["diff", "--cached", "--quiet"])?
                .status
                .success();
            let msg = format!("thien({id}): {stage}");
            let mut c: Vec<&str> = vec!["commit", "-q", "--no-verify", "-m", &msg];
            let t1 = format!("Thien-Task: {id}");
            let t2 = format!("Thien-Stage: {stage}");
            c.extend(["--trailer", &t1, "--trailer", &t2]);
            if no_change {
                c.extend(["--allow-empty", "--trailer", "Thien-No-Change: true"]);
            }
            g.run(&wtl, false, &c)?;
            let checkpoint = g.run_str(&wtl, true, &["rev-parse", "HEAD"])?;
            let diff = g.canonical_diff(&wtl, &base, Some("HEAD"))?;
            std::fs::write(rs.join("DIFF.patch"), &diff)?;
            let inc = match &last_reviewed {
                Some(h) => g.canonical_diff(&wtl, h, Some("HEAD"))?,
                None => Vec::new(),
            };
            std::fs::write(rs.join("DIFF-incremental.patch"), &inc)?;
            eprintln!(
                "✔ checkpoint {} ({} file(s), {} bytes diff{})",
                &checkpoint[..12],
                changed.len(),
                diff.len(),
                if no_change { ", no change" } else { "" }
            );

            // verify
            eprintln!("\n── VERIFY ──");
            let mut results = Vec::new();
            let mut pass = true;
            for cmd in &cmds {
                let (code, out, ms) = run_command(cmd, &wt)?;
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
                results.push(json!({"name": cmd.name, "argv": std::iter::once(cmd.program.clone()).chain(cmd.args.clone()).collect::<Vec<_>>(), "exit": code, "ms": ms, "output_tail": tail}));
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
                continue; // → Fixing (never to Review, §5.2)
            }

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
            let mut rprompt = format!("Review the checkpoint {} in this worktree against TASK.md. Acceptance criteria ids: {ac_ids:?}.", &checkpoint[..12]);
            let mut rresume = None;
            let mut review = Value::Null;
            for corr in 0..=2 {
                let r = claude_turn(
                    cfg,
                    ClaudeArgs {
                        profile: a.claude_profile.clone(),
                        worktree: wt.clone(),
                        run_store: rs.clone(),
                        prompt: rprompt.clone(),
                        schema: Some(fixture::schema("review.schema.json")?.into()),
                        context_file: Some(reviewer_ctx.clone()),
                        model: a.claude_model.clone(),
                        mode: a.mode,
                        resume: rresume.clone(),
                        drop_flags: vec![],
                        extra_args: vec![],
                        label: format!("review-{cycle}-{corr}"),
                        opts: turn_opts(20),
                    },
                    &run,
                )?;
                totals.add(&r, "reviewer");
                review = r.outcome.structured.clone().unwrap_or(Value::Null);
                let viol = review_violations(&review, &ac_ids);
                if viol.is_empty() {
                    break;
                }
                eprintln!("!! invalid review (correction {}/2): {viol:?}", corr + 1);
                if corr == 2 {
                    return Ok((
                        false,
                        format!("InvalidOutput: review rejected 3 times: {viol:?}"),
                    ));
                }
                rprompt = format!("Your previous final response was rejected and was not used. Validation error: {}. Return only a corrected JSON object matching the schema.", viol.join("; "));
                rresume = r.outcome.session_id.clone();
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
            log.push(
                json!({"step": format!("review-{cycle}"), "decision": decision, "unmet": unmet}),
            );
            match decision.as_str() {
                "approved" => {
                    return Ok((
                        true,
                        format!("approved at checkpoint {}", &checkpoint[..12]),
                    ));
                }
                "blocked" => {
                    return Ok((
                        false,
                        format!(
                            "reviewer blocked: {}",
                            review["summary"].as_str().unwrap_or("")
                        ),
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
                }
            }
        }
        Ok((
            false,
            format!(
                "escalated: not approved after {} fix cycle(s)",
                a.max_fix_cycles
            ),
        ))
    })();
    match res {
        Ok((ok, why)) => finish(&fin, ok, &why, &totals, log),
        Err(e) => {
            eprintln!("error: {e:#}");
            finish(&fin, false, &format!("error: {e:#}"), &totals, log)?;
            Ok(false)
        }
    }
}

struct Fin {
    run: PathBuf,
    id: String,
    branch: String,
    repo: PathBuf,
    base_ref: String,
    wt: PathBuf,
}

fn finish(f: &Fin, ok: bool, why: &str, totals: &Totals, log: Vec<Value>) -> Result<bool> {
    let (run, id, branch, repo, base) = (&f.run, &f.id, &f.branch, &f.repo, &f.base_ref);
    let t = &totals.tokens;
    let summary = format!(
        "# {id} — {}\n\n{why}\n\n- branch: `{branch}` in `{}` (base `{base}`)\n- tokens: {} total (uncached in {}, cache read {}, cache write {}, out {})\n- artifacts: `{}`\n",
        if ok { "APPROVED" } else { "STOPPED" },
        repo.display(),
        t.total(),
        t.uncached_input,
        t.cache_read,
        t.cache_write,
        t.output,
        run.join("runstore").display()
    );
    std::fs::write(run.join("runstore").join("SUMMARY.md"), &summary)?;
    let mut rec = Record::new("pipeline", None, &["pipeline prototype"]);
    rec.pass = Some(ok);
    rec.summary = format!("{id}: {why}");
    rec.details =
        json!({"id": id, "branch": branch, "base": base, "repo": repo, "steps": log, "tokens": t});
    rec.run_dir = Some(run.display().to_string());
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
        eprintln!("\nNext (you decide — thien never merges or pushes):");
        eprintln!("  git -C \"{r}\" log --oneline {base}..{branch}      # the checkpoints");
        eprintln!("  git -C \"{r}\" diff {base}...{branch}              # the full change");
        eprintln!("  git -C \"{r}\" merge --squash {branch}; git -C \"{r}\" commit -m \"{id}\"");
    } else {
        eprintln!("\nThe worktree and branch `{branch}` are kept for inspection. To discard them:");
    }
    eprintln!("  git -C \"{r}\" worktree remove --force \"{}\"; git -C \"{r}\" branch -D {branch}   # clean up", f.wt.display());
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac_parser() {
        let t = "# BR-001 — x\n\n## Acceptance criteria\n- AC-1: a\n- **AC-2**: b\n* AC-3. c\n\n## Scope\n- AC-9: not an AC here\n";
        assert_eq!(
            parse_acceptance_ids(t).unwrap(),
            vec!["AC-1", "AC-2", "AC-3"]
        );
        assert!(parse_acceptance_ids("# X\n## Acceptance criteria\nnone\n").is_err());
        assert!(parse_acceptance_ids("## Acceptance criteria\n- AC-1: a\n- AC-1: b\n").is_err());
        assert_eq!(task_id(t, Path::new("x.md"), None), "BR-001");
        assert_eq!(
            task_id("no heading", Path::new("tasks/foo.md"), None),
            "foo"
        );
    }

    #[test]
    fn review_checks() {
        let ids = vec!["AC-1".to_string(), "AC-2".to_string()];
        let ok = json!({"decision":"approved","acceptance_criteria":[{"id":"AC-1","met":true,"evidence":"file.ps1:3"},{"id":"AC-2","met":true,"evidence":"test x"}],"issues":[]});
        assert!(review_violations(&ok, &ids).is_empty());
        let missing = json!({"decision":"approved","acceptance_criteria":[{"id":"AC-1","met":true,"evidence":"e"}],"issues":[]});
        assert!(review_violations(&missing, &ids)[0].contains("missing"));
        let extra = json!({"decision":"approved","acceptance_criteria":[{"id":"AC-1","met":true,"evidence":"e"},{"id":"AC-2","met":true,"evidence":"e"},{"id":"AC-7","met":true,"evidence":"e"}],"issues":[]});
        assert!(review_violations(&extra, &ids)
            .iter()
            .any(|v| v.contains("unknown")));
        let placeholder = json!({"decision":"approved","acceptance_criteria":[{"id":"AC-1","met":true,"evidence":"ok"},{"id":"AC-2","met":true,"evidence":"e"}],"issues":[]});
        assert!(!review_violations(&placeholder, &ids).is_empty());
        let empty_cr = json!({"decision":"changes_requested","acceptance_criteria":[{"id":"AC-1","met":true,"evidence":"e"},{"id":"AC-2","met":true,"evidence":"e"}],"issues":[]});
        assert!(!review_violations(&empty_cr, &ids).is_empty());
        assert!(!review_violations(&json!({}), &ids).is_empty());
    }

    #[test]
    fn protected_paths() {
        let p = Protected::new(
            vec!["scripts/test.ps1".into(), "tasks/BR-001.md".into()],
            &["docs/**".into(), "*.lock".into()],
        );
        assert!(p.is_protected("scripts/test.ps1"));
        assert!(p.is_protected("tasks/BR-001.md"));
        assert!(p.is_protected("AGENTS.md"));
        assert!(p.is_protected("sub/.claude/settings.local.json"));
        assert!(p.is_protected("docs/a/b.md"));
        assert!(p.is_protected("x.lock"));
        assert!(!p.is_protected("a/x.lock"));
        assert!(!p.is_protected("src/BatchRename.psm1"));
        assert!(!p.is_protected("scripts/other.ps1"));
        assert!(!p.is_protected("tasks/BR-002.md"));
    }
}
