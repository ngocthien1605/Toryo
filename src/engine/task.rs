//! `toryo task create` (plan §15; R27, R28, R35, R37, R46): snapshot TASK.md into the run
//! store, parse the acceptance ids, freeze the effective configuration, admit the repo's
//! Git filters, create the worktree + branch, and register the task (state `New`).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use super::checks::{parse_acceptance_ids, task_id};
use super::state::Machine;
use super::store::{self, NewTask, Store, TaskRow};
use super::worktree::Worktree;
use crate::config::{data_dir, expand_vars, Effective};
use crate::git::{default_gitcmd, GitCmd, Loc};

pub struct CreateArgs {
    pub id: Option<String>,
    pub repo: PathBuf,
    pub file: PathBuf,
    pub base: Option<String>,
    pub max_fix_cycles: Option<u32>,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn run_store_dir(id: &str) -> PathBuf {
    data_dir().join("runs").join(id)
}

/// Frozen roles: planner, the implementer chain (primary + fallbacks, R49), reviewer
/// (owner decision 15: never follows an implementer fallback).
pub fn roles(cfg: &Effective) -> Result<(String, Vec<String>, String)> {
    let role = |r: &str| cfg.repo.roles.get(r);
    let planner = role("planner")
        .map(|r| r.profile.clone())
        .context("toryo.yaml has no `roles.planner`")?;
    let imp = role("implementer").context("toryo.yaml has no `roles.implementer`")?;
    let mut chain = vec![imp.profile.clone()];
    // an absent `fallback` defaults to the planner's profile; `fallback: []` is none (EC-23)
    let fb = match &imp.fallback {
        None => vec![planner.clone()],
        Some(list) => list.clone(),
    };
    for f in fb {
        if !chain.contains(&f) {
            chain.push(f);
        }
    }
    let reviewer = role("reviewer")
        .map(|r| r.profile.clone())
        .unwrap_or_else(|| planner.clone());
    for p in chain.iter().chain([&planner, &reviewer]) {
        cfg.profile(p)?;
    }
    Ok((planner, chain, reviewer))
}

/// Git for this task's repository, with the R37 filter plan admitted.
pub fn task_git(cfg: &Effective, repo: &Path) -> Result<GitCmd> {
    let mut g = default_gitcmd()?;
    GitCmd::check_submodules(repo, cfg.repo.policy.allow_submodules)?;
    g.filters = g.plan_filters(
        repo,
        &cfg.trusted_filters,
        cfg.repo.policy.neutralize_untrusted_filters,
    )?;
    if !g.filters.untrusted_referenced.is_empty() && !cfg.repo.policy.neutralize_untrusted_filters {
        bail!(
            "untrusted_git_filter: the repository uses filter drivers {:?} that are not in the trusted table (R37)",
            g.filters.untrusted_referenced
        );
    }
    Ok(g)
}

pub fn create(cfg: &Effective, store: &Store, a: CreateArgs) -> Result<TaskRow> {
    let repo = std::path::absolute(&a.repo)?;
    let file = if a.file.is_absolute() || a.file.exists() {
        a.file.clone()
    } else {
        repo.join(&a.file)
    };
    let file = std::path::absolute(&file)?;
    let text =
        std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    let ac = parse_acceptance_ids(&text)?;
    let id = task_id(&text, &file, a.id.as_deref());
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("task id `{id}` may only contain letters, digits, `-` and `_`");
    }
    let title = text
        .lines()
        .find(|l| l.starts_with("# "))
        .map(|l| l.trim_start_matches("# ").trim().to_string())
        .unwrap_or_else(|| id.clone());
    let (planner, implementers, reviewer) = roles(cfg)?;

    let g = task_git(cfg, &repo)?;
    // R36/R45 admission: the verify boundary must be available before anything is created
    super::verify::probe(cfg)?;
    let repol = Loc::Discover(repo.clone());
    if !g
        .output(&repol, true, &["rev-parse", "--git-dir"])?
        .status
        .success()
    {
        bail!("{} is not a Git repository", repo.display());
    }
    let base_ref = match &a.base {
        Some(b) => b.clone(),
        None => g.run_str(&repol, true, &["rev-parse", "--abbrev-ref", "HEAD"])?,
    };
    let base = g.run_str(
        &repol,
        true,
        &["rev-parse", &format!("{base_ref}^{{commit}}")],
    )?;
    let t = id.clone();
    if store.call(move |tx| Ok(store::get_task(tx, &t).is_ok()))? {
        bail!("task {id} already exists (`toryo task show {id}`)");
    }
    let rs = run_store_dir(&id);
    if rs.exists() {
        bail!(
            "run store {} already exists — remove it or use another --id",
            rs.display()
        );
    }
    let wt_root = PathBuf::from(expand_vars(&cfg.repo.project.worktrees_root)?);
    let wt = wt_root.join(&id);
    if wt.exists() {
        bail!(
            "worktree {} already exists — use another --id",
            wt.display()
        );
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
        bail!("branch {branch} already exists in {}", repo.display());
    }

    // run store: TASK.md snapshot (R27) + frozen config (R35)
    let art = rs.join("runstore");
    std::fs::create_dir_all(&art)?;
    std::fs::create_dir_all(rs.join("captures"))?;
    std::fs::write(art.join("TASK.md"), &text)?;
    let frozen = cfg.canonical_json();
    std::fs::write(art.join("config.json"), &frozen)?;

    std::fs::create_dir_all(&wt_root)?;
    let fwd = |p: &Path| p.display().to_string().replace('\\', "/");
    g.run(
        &repol,
        false,
        &["worktree", "add", "-q", "-b", &branch, &fwd(&wt), &base],
    )?;
    // R46: per-task TEMP inside the worktree, invisible to Git (info/exclude of the common dir)
    let w = Worktree::new(&g, &wt);
    let common = g.run_str(
        &w.loc,
        true,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let excl = PathBuf::from(common.trim()).join("info").join("exclude");
    let cur = std::fs::read_to_string(&excl).unwrap_or_default();
    if !cur.lines().any(|l| l.trim() == "/.toryo-tmp/") {
        std::fs::create_dir_all(excl.parent().expect("parent"))?;
        let sep = if cur.is_empty() || cur.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        std::fs::write(&excl, format!("{cur}{sep}/.toryo-tmp/\n"))?;
    }

    let max = a
        .max_fix_cycles
        .or(cfg.repo.pipeline.max_fix_cycles)
        .unwrap_or(2);
    let nt = NewTask {
        id: id.clone(),
        title,
        task_sha256: sha256_hex(text.as_bytes()),
        acceptance_ids: ac,
        run_store: rs.clone(),
        config_sha256: sha256_hex(frozen.as_bytes()),
        worktree: wt.clone(),
        branch,
        base_commit: base,
        repo,
        task_file: file,
        machine: Machine::new(max),
        implementers,
        planner,
        reviewer,
        budget_tokens: cfg.repo.pipeline.budget_tokens.map(|b| b as i64),
    };
    let res = store.call(move |tx| {
        store::insert_task(tx, &nt)?;
        store::get_task(tx, &nt.id)
    });
    if res.is_err() {
        let _ = g.output(
            &Loc::Discover(wt.clone()),
            false,
            &["worktree", "remove", "--force", &fwd(&wt)],
        );
    }
    let t = res?;
    // R34: the task snapshot and the frozen configuration are the first immutable versions
    super::artifacts::put(
        store,
        &t,
        None,
        "task",
        "md",
        "task",
        text.as_bytes(),
        Some("TASK.md"),
    )?;
    super::artifacts::put(
        store,
        &t,
        None,
        "config",
        "json",
        "config",
        frozen.as_bytes(),
        Some("config.json"),
    )?;
    // R35: protected baseline v1 — after the info/exclude line (R46), before any agent runs
    let (_, protect, _) = super::runner::task_policy(&t, cfg)?;
    super::protect::take_baseline(store, &t, &g, &protect, "v1 (task created)")?;
    Ok(t)
}

/// The frozen configuration of a task (R35).
pub fn frozen_config(t: &TaskRow) -> Result<Effective> {
    let text = std::fs::read_to_string(t.artifacts().join("config.json"))
        .with_context(|| format!("reading the frozen config of {}", t.id))?;
    if sha256_hex(text.as_bytes()) != t.config_sha256 {
        bail!(
            "artifact_tampered: config.json of {} does not match tasks.config_sha256 (R27/R35)",
            t.id
        );
    }
    Effective::from_frozen(&text, &t.artifacts().join("config.json"))
}
