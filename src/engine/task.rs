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

/// The state machine of a new task: the org chart's split limits (M11-C4: a missing one of
/// the two is 2, today's default), else the single bound — `--max-fix-cycles`, then
/// `pipeline.max_fix_cycles` (or a lone `limits.max_fix_cycles`), then 2.
pub fn new_machine(cfg: &Effective, cli_max: Option<u32>) -> Result<Machine> {
    match cfg.team.as_ref().map(|t| &t.limits).filter(|l| l.split()) {
        Some(l) => {
            if cli_max.is_some() {
                bail!("--max-fix-cycles sets the single fix-cycle counter, but the org chart splits it (limits.max_gate_retries / limits.max_review_rounds) — change those in .toryo/toryo-orgchart.yaml");
            }
            Ok(Machine::with_limits(
                l.max_gate_retries.unwrap_or(2),
                l.max_review_rounds.unwrap_or(2),
            ))
        }
        None => Ok(Machine::new(
            cli_max.or(cfg.repo.pipeline.max_fix_cycles).unwrap_or(2),
        )),
    }
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

/// Git for `task cleanup` when the frozen configuration cannot be read (EC-12): no trusted
/// filter, so every filter driver the repository defines or references is neutralised (R37).
pub fn cleanup_git(repo: &Path) -> Result<GitCmd> {
    let mut g = default_gitcmd()?;
    g.filters = g.plan_filters(repo, &Default::default(), true)?;
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
    // review H2 (M11-A2): the repository chooses its verify sandbox only above the floor
    if let Some(reason) = super::verify::below_floor(cfg) {
        bail!("{reason}");
    }
    let machine = new_machine(cfg, a.max_fix_cycles)?;

    let g = task_git(cfg, &repo)?;
    // R36/R45 admission: the verify boundary must be available before anything is created
    super::verify::probe(cfg)?;
    // R59 admission: a `tool: xpp` command needs MSBuild and the five CI/CD packages here
    super::xpp::for_config(cfg)?;
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
    // M18 admission: every repository program and `-File` script of a verify command (a
    // check gate) is in the base commit — one the task created could be written by the
    // implementer before verify runs it
    let cmds = super::checks::verify_commands(cfg, &repo).unwrap_or_default();
    let entry = super::checks::verify_entry_points(&cmds, &repo);
    if !entry.is_empty() {
        let listing = g.run(&repol, true, &["ls-tree", "-r", "-z", "--name-only", &base])?;
        let tree: std::collections::HashSet<String> = listing
            .split(|b| *b == 0)
            .filter(|f| !f.is_empty())
            .map(|f| String::from_utf8_lossy(f).to_lowercase())
            .collect();
        let what = if cfg.team.is_some() {
            "gate"
        } else {
            "verify command"
        };
        for (name, path) in entry {
            if !tree.contains(&path.to_lowercase()) {
                bail!(
                    "verify_script_missing: {what} `{name}` runs `{path}`, which is not in the base commit {} of {} — commit it before creating the task (M18: a verify script the task itself creates could be written by the implementer)",
                    crate::git::short(&base),
                    repo.display()
                );
            }
        }
    }
    // R59 admission: its metadata folder and (protected) build projects are in the base commit
    if super::xpp::compile_only(cfg) {
        let listing = g.run(&repol, true, &["ls-tree", "-r", "-z", "--name-only", &base])?;
        let mut tree = std::collections::HashMap::new();
        for f in listing.split(|b| *b == 0).filter(|f| !f.is_empty()) {
            let f = super::xpp::tree_key(&String::from_utf8_lossy(f));
            let mut dir = f.as_str();
            while let Some((parent, _)) = dir.rsplit_once('/') {
                tree.insert(parent.to_string(), true);
                dir = parent;
            }
            tree.insert(f, false);
        }
        let place = format!("commit {} of {}", crate::git::short(&base), repo.display());
        let kind_of = |p: &str| tree.get(&super::xpp::tree_key(p)).copied();
        super::xpp::check_repo(cfg, &place, &kind_of)?;
    }
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
        machine,
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
    let (path, text) = frozen_config_text(t)?;
    Effective::from_frozen(&text, &path)
}

/// The exact frozen bytes of a task's configuration and the file they were read from: the
/// immutable `config` version whose SHA-256 is `tasks.config_sha256` (R34/R35, EC-12). Neither
/// the view `runstore/config.json` nor the artifact head is trusted to be the one in force, so a
/// reload that stopped before it switched the hash leaves the old configuration in force. Run
/// stores from before the immutable versions (M6) only have the view; it is used when its
/// SHA-256 matches.
pub fn frozen_config_text(t: &TaskRow) -> Result<(PathBuf, String)> {
    let found = match super::artifacts::find_by_sha(t, "config", "json", &t.config_sha256)? {
        Some(v) => Some(v),
        None => {
            let view = t.artifacts().join("config.json");
            std::fs::read(&view)
                .ok()
                .filter(|b| sha256_hex(b) == t.config_sha256)
                .map(|b| (view, b))
        }
    };
    let Some((path, bytes)) = found else {
        bail!(
            "artifact_tampered: no version of config.json of {} matches tasks.config_sha256 {} (R27/R35)",
            t.id,
            crate::git::short(&t.config_sha256)
        );
    };
    let text = String::from_utf8(bytes)
        .with_context(|| format!("the frozen config of {} is not UTF-8", t.id))?;
    Ok((path, text))
}
