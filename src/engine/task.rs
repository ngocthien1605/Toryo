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
use crate::config::{data_dir, expand_vars, BuildEntry, Effective};
use crate::git::{default_gitcmd, GitCmd, Loc};

pub struct CreateArgs {
    pub id: Option<String>,
    pub repo: PathBuf,
    pub file: PathBuf,
    pub base: Option<String>,
    pub max_fix_cycles: Option<u32>,
}

/// M12-12 (c): a task id is a file name (the lock file, the run store): `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`
/// and never `..` — checked before any file is touched, on every command that takes an id.
/// M13-14: and a file name on Windows too — not a device name (`con`, `nul`, `com1`, … also
/// before a dot, in any case) and not ending in a dot.
pub fn valid_task_id(id: &str) -> Result<()> {
    let mut chars = id.chars();
    let ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !id.contains("..")
        && !id.ends_with('.')
        && !super::lock::is_device_name(id);
    if !ok {
        bail!(
            "`{id}` is not a task id — letters, digits, `.`, `_` and `-`, starting with a letter or digit, at most 64 characters, never `..`, not ending in `.`, and not a Windows device name (con, prn, aux, nul, com1–com9, lpt1–lpt9)"
        );
    }
    Ok(())
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

/// M18 admission: every repository program and script of a verify command (a check gate,
/// [`super::checks::verify_entry_points`]) is in the commit `base` — one the task created could
/// be written by the implementer before verify runs it. At `task create` and at `task config
/// --reload` (group E review).
/// Returns the entry scripts admitted as build output (M11-R14, X9), to freeze with the task.
pub fn check_entry_points(
    cfg: &Effective,
    g: &GitCmd,
    repo: &Path,
    base: &str,
) -> Result<Vec<BuildEntry>> {
    let a = admission(cfg, g, repo, base)?;
    if let Some(refusal) = a.refused.into_iter().next() {
        bail!("{refusal}");
    }
    for n in a.notes {
        crate::errln!("note: {n}");
    }
    Ok(a.build)
}

/// [`check_entry_points`]'s notes for the other arguments the base commit lacks (the net:
/// never a refusal); a refusal is the error. Used by `doctor`.
pub fn entry_point_notes(
    cfg: &Effective,
    g: &GitCmd,
    repo: &Path,
    base: &str,
) -> Result<Vec<String>> {
    let a = admission(cfg, g, repo, base)?;
    if let Some(refusal) = a.refused.into_iter().next() {
        bail!("{refusal}");
    }
    Ok(a.notes)
}

/// M11-R14 (X9): the entry scripts of `cfg`'s verify commands that are build output at `base`
/// (missing there, ignored by the repository) — what `task create` freezes with the task and
/// `task config --reload` recomputes against the same base commit. Never a refusal.
pub fn build_entries(
    cfg: &Effective,
    g: &GitCmd,
    repo: &Path,
    base: &str,
) -> Result<Vec<BuildEntry>> {
    Ok(admission(cfg, g, repo, base)?.build)
}

/// The files of commit `base` in `repo` (M11-R14): repository-relative, `/`, lower-case.
/// What protects arguments: never a folder.
pub fn base_files(
    g: &GitCmd,
    repo: &Path,
    base: &str,
) -> Result<std::collections::HashSet<String>> {
    ls_base(g, repo, base, false)
}

/// The files **and folders** of commit `base` (M11-R18, X14): what admission asks "is it in the
/// base commit" of — an entry that is a folder (`node --test tests/unit`) is there.
pub fn base_paths(
    g: &GitCmd,
    repo: &Path,
    base: &str,
) -> Result<std::collections::HashSet<String>> {
    ls_base(g, repo, base, true)
}

fn ls_base(
    g: &GitCmd,
    repo: &Path,
    base: &str,
    folders: bool,
) -> Result<std::collections::HashSet<String>> {
    let mut args = vec!["ls-tree", "-r", "-z", "--name-only"];
    if folders {
        args.push("-t");
    }
    args.push(base);
    let listing = g
        .run(&Loc::Discover(repo.to_path_buf()), true, &args)
        .with_context(|| {
            format!(
                "listing the files of the base commit {}",
                crate::git::short(base)
            )
        })?;
    Ok(listing
        .split(|b| *b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).to_lowercase())
        .collect())
}

/// What M18 admission finds for a configuration at a base commit.
struct Admission {
    /// `verify_script_missing: …` for each refusable entry script the base commit lacks and
    /// the repository does not ignore.
    refused: Vec<String>,
    /// Entry scripts the base commit lacks and the repository ignores: build output (X9).
    build: Vec<BuildEntry>,
    /// The net: other arguments the base commit lacks (never a refusal).
    notes: Vec<String>,
}

/// M18 admission (M11-R10, X7; R14, X9): a refusable entry script missing at `base` is refused
/// only when the repository does not ignore it — an ignored one is build output a gate creates
/// in the clean verify checkout, never the implementer's: it is admitted and recorded.
fn admission(cfg: &Effective, g: &GitCmd, repo: &Path, base: &str) -> Result<Admission> {
    let mut found = Admission {
        refused: Vec::new(),
        build: Vec::new(),
        notes: Vec::new(),
    };
    let cmds = super::checks::verify_commands(cfg, repo).unwrap_or_default();
    let entries = super::checks::verify_entries(&cmds, repo);
    let net: Vec<(String, String)> = super::checks::script_like_args(&cmds, repo)
        .into_iter()
        .filter(|e| !entries.iter().any(|x| x.0 == e.0 && x.1 == e.1))
        .collect();
    if entries.is_empty() && net.is_empty() {
        return Ok(found);
    }
    let repol = Loc::Discover(repo.to_path_buf());
    let tree = base_paths(g, repo, base)?;
    let what = if cfg.team.is_some() {
        "gate"
    } else {
        "verify command"
    };
    let ignored = |path: &str| {
        g.output(
            &repol,
            true,
            &["check-ignore", "--no-index", "-q", "--", path],
        )
        .map(|o| o.status.success())
        .unwrap_or(false)
    };
    for (name, path, refusable) in entries {
        if tree.contains(&path.to_lowercase()) {
            continue;
        }
        if ignored(&path) {
            let e = BuildEntry { gate: name, path };
            if !found.build.contains(&e) {
                found.build.push(e);
            }
        } else if refusable {
            found.refused.push(format!(
                "verify_script_missing: {what} `{name}` runs `{path}`, which is not in the base commit {} of {} — commit it before creating the task (M18: a verify script the task itself creates could be written by the implementer)",
                crate::git::short(base),
                repo.display()
            ));
        }
    }
    // (R21 review) a data or configuration file (`ci/settings.json`) that the repository ignores
    // is build output: no note. A script-like path keeps its note, ignored or not (M11-R10).
    found.notes = net
        .into_iter()
        .filter(|(_, path)| {
            !tree.contains(&path.to_lowercase())
                && (super::checks::has_script_ext(path) || !ignored(path))
        })
        .map(|(name, path)| {
            format!("{what} `{name}` passes `{path}`, which is not in the base commit — fine for build output; if the {what} runs it as its script, commit it first")
        })
        .collect();
    Ok(found)
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

/// M13-11 (X28): what `task create` has made so far. Dropped without [`Made::keep`] — any error
/// after the first of them — it removes them again in reverse order, best effort, one line
/// each: the same `TASK.md` can then be created again. The error that ended the creation is
/// the one reported.
struct Made<'a> {
    g: &'a GitCmd,
    store: &'a Store,
    repo: PathBuf,
    id: String,
    run_store: Option<PathBuf>,
    worktree: Option<PathBuf>,
    branch: Option<String>,
    /// The commit the branch is made at (M13-R7): only a branch still there is deleted.
    base: String,
    row: bool,
    kept: bool,
}

impl Made<'_> {
    fn keep(mut self) {
        self.kept = true;
    }
}

impl Drop for Made<'_> {
    fn drop(&mut self) {
        if self.kept {
            return;
        }
        let repo = Loc::Discover(self.repo.clone());
        // R23-06: the worktree before the row — a worktree that stays needs the row, so that
        // `toryo task cleanup <id> --force` can still remove it
        let mut worktree_left = false;
        if let Some(wt) = &self.worktree {
            let existed = wt.exists();
            let w = wt.display().to_string().replace('\\', "/");
            if existed
                && self
                    .g
                    .run(&repo, false, &["worktree", "remove", "--force", &w])
                    .is_err()
            {
                let _ = std::fs::remove_dir_all(wt);
                // M13-R7: only here — `prune` forgets every stale entry of the repository (a
                // worktree on a drive that is not mounted now), not just ours
                let _ = self.g.run(&repo, false, &["worktree", "prune"]);
            }
            match (existed, wt.exists()) {
                (true, false) => crate::errln!("removed: the worktree {}", wt.display()),
                (_, true) => worktree_left = true,
                (false, false) => {}
            }
        }
        if self.row && worktree_left {
            crate::errln!(
                "WARNING: could not remove the worktree {} — the task {} is kept: `toryo task cleanup {} --force` removes it once nothing holds its files",
                self.worktree.as_deref().unwrap_or(Path::new("?")).display(),
                self.id,
                self.id
            );
            return;
        }
        if worktree_left {
            crate::errln!(
                "WARNING: could not remove the worktree {} — delete it by hand",
                self.worktree.as_deref().unwrap_or(Path::new("?")).display()
            );
        }
        if self.row {
            let id = self.id.clone();
            match self
                .store
                .call(move |tx| store::remove_created_task(tx, &id))
            {
                Ok(()) => crate::errln!("removed: the database row of {}", self.id),
                Err(e) => crate::errln!(
                    "WARNING: could not remove the database row of {}: {e:#}",
                    self.id
                ),
            }
        }
        if let Some(b) = &self.branch {
            // it did not exist before this `task create` (checked there) — but another process
            // can have made it since: it is ours only while it is still at the base (M13-R7)
            let at = self
                .g
                .output(
                    &repo,
                    true,
                    &[
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("refs/heads/{b}"),
                    ],
                )
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
            match at {
                Some(sha) if sha == self.base => {
                    match self.g.run(&repo, false, &["branch", "-D", b]) {
                        Ok(_) => crate::errln!("removed: the branch {b}"),
                        Err(e) => crate::errln!("WARNING: could not remove the branch {b}: {e:#}"),
                    }
                }
                Some(sha) => crate::errln!(
                    "WARNING: the branch {b} is at {} now, not at the base it was made at ({}) — it is not ours any more and is left",
                    crate::git::short(&sha),
                    crate::git::short(&self.base)
                ),
                None => {}
            }
        }
        if let Some(rs) = &self.run_store {
            if rs.exists() {
                match std::fs::remove_dir_all(rs) {
                    Ok(()) => crate::errln!("removed: the run store {}", rs.display()),
                    Err(e) => crate::errln!(
                        "WARNING: could not remove the run store {}: {e} — delete it by hand",
                        rs.display()
                    ),
                }
            }
        }
    }
}

pub fn create(cfg: &Effective, store: &Store, a: CreateArgs) -> Result<TaskRow> {
    let repo = std::path::absolute(&a.repo)?;
    let file = if a.file.is_absolute() || a.file.exists() {
        a.file.clone()
    } else {
        repo.join(&a.file)
    };
    let file = std::path::absolute(&file)?;
    let text = crate::textfile::read_text(&file)?;
    let ac = parse_acceptance_ids(&text)?;
    let id = task_id(&text, &file, a.id.as_deref());
    valid_task_id(&id)?;
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
    // M12-05: an environment gate this machine cannot start is refused, like one it does not define
    if let Some(reason) = super::gates::env_gates_refusal(cfg) {
        bail!("{reason}");
    }
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
    // M12-08 (X19): `--base`, else `project.base_branch` when the file sets it (a branch that
    // does not exist is an error naming the key), else the checked-out branch, as before
    let checked_out = g.run_str(&repol, true, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let base_ref = match (&a.base, &cfg.repo.project.base_branch) {
        (Some(b), _) => b.clone(),
        (None, Some(branch)) => {
            let found = g.output(
                &repol,
                true,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}^{{commit}}"),
                ],
            )?;
            if !found.status.success() {
                bail!(
                    "project.base_branch: the branch `{branch}` does not exist in {} — create it, or fix the key in {} (or remove it to start from the checked-out branch)",
                    repo.display(),
                    cfg.repo_path.display()
                );
            }
            branch.clone()
        }
        (None, None) => checked_out.clone(),
    };
    let base = g.run_str(
        &repol,
        true,
        &["rev-parse", &format!("{base_ref}^{{commit}}")],
    )?;
    crate::errln!("base: {base_ref} @ {}", crate::git::short(&base));
    if base_ref != checked_out {
        crate::errln!(
            "note: {checked_out} is checked out in {}; the task starts from {base_ref} ({})",
            repo.display(),
            if a.base.is_some() {
                "--base"
            } else {
                "project.base_branch"
            }
        );
    }
    for key in cfg.keys_without_effect() {
        crate::errln!("note: {}", Effective::no_effect_line(key));
    }
    // M11-R14 (X9): the entry scripts admitted as build output are frozen with the task
    let admitted = Effective {
        build_entries: check_entry_points(cfg, &g, &repo, &base)?,
        ..cfg.clone()
    };
    let cfg = &admitted;
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
    // R23-17: Windows file names ignore case — `a-1` would share the lock file, the run store
    // and the worktree of a task `A-1`
    let t = id.clone();
    if let Some(other) = store.read(move |tx| store::task_id_in_other_case(tx, &t))? {
        bail!(
            "task {other} already exists and `{id}` differs from it only in case — they would share a lock file, a run store and a worktree on Windows; use another --id"
        );
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

    // M13-11 (X28): every refusal is above this line — nothing has been made yet. From here
    // on, whatever fails, what was made is removed again (`Made`).
    let mut made = Made {
        g: &g,
        store,
        repo: repo.clone(),
        id: id.clone(),
        run_store: Some(rs.clone()),
        worktree: None,
        branch: None,
        base: base.clone(),
        row: false,
        kept: false,
    };
    // run store: TASK.md snapshot (R27) + frozen config (R35)
    let art = rs.join("runstore");
    std::fs::create_dir_all(&art)?;
    std::fs::create_dir_all(rs.join("captures"))?;
    std::fs::write(art.join("TASK.md"), &text)?;
    let frozen = cfg.frozen_json()?;
    std::fs::write(art.join("config.json"), &frozen)?;

    std::fs::create_dir_all(&wt_root)
        .with_context(|| format!("creating project.worktrees_root {}", wt_root.display()))?;
    let fwd = |p: &Path| p.display().to_string().replace('\\', "/");
    (made.worktree, made.branch) = (Some(wt.clone()), Some(branch.clone()));
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
    // M13-R2 test hook (STATUS O-39): the step after `git worktree add` fails the same way on
    // every Git version (a folder at `info/exclude` is refused before anything is created by
    // Git 2.43 and only warned about by 2.55)
    if std::env::var("TORYO_TEST_FAIL_AT").is_ok_and(|v| v == "create:after_worktree") {
        bail!(
            "TORYO_TEST_FAIL_AT=create:after_worktree: cannot write {}",
            excl.display()
        );
    }
    let cur = std::fs::read_to_string(&excl).unwrap_or_default();
    if !cur.lines().any(|l| l.trim() == "/.toryo-tmp/") {
        std::fs::create_dir_all(excl.parent().expect("parent"))?;
        let sep = if cur.is_empty() || cur.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        std::fs::write(&excl, format!("{cur}{sep}/.toryo-tmp/\n"))
            .with_context(|| format!("writing {}", excl.display()))?;
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
    let t = store.call(move |tx| {
        store::insert_task(tx, &nt)?;
        store::get_task(tx, &nt.id)
    })?;
    made.row = true;
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
    made.keep();
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

#[cfg(test)]
mod made_tests {
    use super::*;

    /// A repository with two commits; returns it, the first commit and the second.
    fn repo(d: &Path) -> (GitCmd, PathBuf, String, String) {
        let g = GitCmd::new("git", d.join("hooks")).unwrap();
        let r = d.join("repo");
        std::fs::create_dir_all(&r).unwrap();
        let loc = Loc::Discover(r.clone());
        g.run(&loc, false, &["init", "-q", "-b", "main"]).unwrap();
        let mut shas = Vec::new();
        for n in ["a", "b"] {
            std::fs::write(r.join(format!("{n}.txt")), "x\n").unwrap();
            g.run(&loc, false, &["add", "-A"]).unwrap();
            g.run(&loc, false, &["commit", "-q", "-m", n]).unwrap();
            shas.push(g.run_str(&loc, true, &["rev-parse", "HEAD"]).unwrap());
        }
        (g, r, shas[0].trim().into(), shas[1].trim().into())
    }

    /// M13-R7: `Made` deletes the branch of a failed `task create` only while it is still at the
    /// commit it was made at. Another process that made `toryo/<id>` in between (the check at
    /// the top of `create` had passed) owns it.
    #[test]
    fn a_branch_made_by_someone_else_in_between_is_not_deleted() {
        let d = tempfile::tempdir().unwrap();
        let (g, r, first, second) = repo(d.path());
        let store = Store::open(&d.path().join("t.db")).unwrap();
        let loc = Loc::Discover(r.clone());
        let made = |branch: &str, base: &str| {
            drop(Made {
                g: &g,
                store: &store,
                repo: r.clone(),
                id: "x".into(),
                run_store: None,
                worktree: None,
                branch: Some(branch.into()),
                base: base.into(),
                row: false,
                kept: false,
            })
        };
        let there = |b: &str| {
            g.output(
                &loc,
                true,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{b}"),
                ],
            )
            .unwrap()
            .status
            .success()
        };
        // someone else's branch, at another commit than the base we made ours at
        g.run(&loc, false, &["branch", "toryo/theirs", &second])
            .unwrap();
        made("toryo/theirs", &first);
        assert!(there("toryo/theirs"), "not ours: it must stay");
        // ours: at the base
        g.run(&loc, false, &["branch", "toryo/ours", &first])
            .unwrap();
        made("toryo/ours", &first);
        assert!(!there("toryo/ours"), "ours: removed");
    }

    /// R23-06 (review 2026-10-23 C6): a `task create` that fails after its row exists, with a
    /// worktree that cannot be removed (a file in it is held open). Was: the row went first,
    /// then the worktree stayed — `task cleanup <id> --force`, the tool for it, said "no task"
    /// and the same TASK.md could not be created again. Now the row stays with the worktree.
    #[cfg(windows)]
    #[test]
    fn a_worktree_that_cannot_be_removed_keeps_its_task() {
        use std::os::windows::fs::OpenOptionsExt;
        let d = tempfile::tempdir().unwrap();
        let (g, r, first, _) = repo(d.path());
        let store = Store::open(&d.path().join("t.db")).unwrap();
        let nt = NewTask {
            id: "W-1".into(),
            title: "t".into(),
            task_sha256: "0".into(),
            acceptance_ids: vec!["AC-1".into()],
            run_store: d.path().join("rs"),
            config_sha256: "0".into(),
            worktree: d.path().join("wt"),
            branch: "toryo/W-1".into(),
            base_commit: first.clone(),
            repo: r.clone(),
            task_file: r.join("TASK.md"),
            machine: Machine::new(1),
            implementers: vec!["p".into()],
            planner: "p".into(),
            reviewer: "p".into(),
            budget_tokens: None,
        };
        store.call(move |tx| store::insert_task(tx, &nt)).unwrap();
        let wt = d.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        let held = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .share_mode(0)
            .open(wt.join("busy.txt"))
            .unwrap();
        let row = |store: &Store| {
            store
                .call(|tx| Ok(store::get_task(tx, "W-1").is_ok()))
                .unwrap()
        };
        let made = |row: bool| {
            drop(Made {
                g: &g,
                store: &store,
                repo: r.clone(),
                id: "W-1".into(),
                run_store: None,
                worktree: Some(wt.clone()),
                branch: None,
                base: first.clone(),
                row,
                kept: false,
            })
        };
        made(true);
        assert!(wt.exists(), "the file is held: the worktree stays");
        assert!(row(&store), "the row stays with the worktree");
        // once nothing holds it, the same rollback removes both
        drop(held);
        made(true);
        assert!(!wt.exists() && !row(&store));
    }
}
