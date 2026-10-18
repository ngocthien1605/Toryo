//! `toryo goal split` (M16-03): split a goal into several `TASK.md` files.
//!
//! One read-only turn of the planner seat (or `--profile`) proposes the split; the files are
//! made by [`toryo::engine::goal`], which decides the ids, keeps the planner's text inert and
//! checks every file with the parser of `task create`. The turn runs in a **throwaway detached
//! worktree** of the base commit under `<data_dir>\goals\<stamp>\worktree`, never in the owner's
//! checkout: a Codex seat runs `workspace-write` even when its role only reads (inside a task the
//! engine undoes what a read-only role wrote; here the whole checkout is removed, on every exit).
//! The profile's canary evidence is checked like before a task's turn (R51). Nothing is created
//! in the store: the owner reads the files and creates the tasks (X38).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Subcommand;

use toryo::agents::adapter::TurnHooks;
use toryo::agents::claude::LaunchMode;
use toryo::agents::hub::AgentHub;
use toryo::config::{data_dir, expand_vars, Effective};
use toryo::domain::{OpenSession, Role, TurnInput, TurnLimits};
use toryo::engine::context::{self, Request, TeamContext};
use toryo::engine::goal::{self, Planned};
use toryo::engine::store::{self, Store};
use toryo::engine::task;
use toryo::git::{GitCmd, Loc};

use crate::engine_cli::{interrupted, watch_interrupt};
use crate::{load_config, Cli};

#[derive(Subcommand)]
pub enum GoalCmd {
    /// Split a goal into several task files (`<prefix>-1.md`, …) and `GOAL-SPLIT.json`: one
    /// read-only turn of the planner seat in a throwaway checkout of the base. Creates no task —
    /// read the files, then `toryo task create --file …` each.
    Split {
        /// The goal: Markdown, the owner's text (UTF-8)
        #[arg(long)]
        file: PathBuf,
        /// A new or empty folder for the task files
        #[arg(long)]
        out: PathBuf,
        /// The task ids are `<prefix>-1`, `<prefix>-2`, … (permanent: choose them well)
        #[arg(long)]
        prefix: String,
        /// At most this many tasks (1 to 20)
        #[arg(long, default_value_t = goal::DEFAULT_MAX_TASKS as u64, value_parser = clap::value_parser!(u64).range(1..=goal::MAX_TASKS as u64))]
        max_tasks: u64,
        /// The profile that splits (default: the planner seat's)
        #[arg(long)]
        profile: Option<String>,
        /// Base branch/commit the planner reads (default: `project.base_branch`, else the
        /// checked-out branch — as `task create`)
        #[arg(long)]
        base: Option<String>,
        /// R51: split even if the profile's canary did not pass for its installed CLI version
        #[arg(long)]
        accept_unverified: bool,
        /// Scripted agents instead of the real CLIs (tests / demos; the `planner` turns)
        #[arg(long, hide = true)]
        fake_agents: Option<PathBuf>,
    },
}

pub fn run(cli: &Cli, cmd: &GoalCmd) -> Result<i32> {
    match cmd {
        GoalCmd::Split {
            file,
            out,
            prefix,
            max_tasks,
            profile,
            base,
            accept_unverified,
            fake_agents,
        } => split(
            cli,
            &Split {
                file,
                out,
                prefix,
                max_tasks: *max_tasks as usize,
                profile: profile.as_deref(),
                base: base.as_deref(),
                accept_unverified: *accept_unverified,
                fake_agents: fake_agents.as_deref(),
            },
        ),
    }
}

struct Split<'a> {
    file: &'a Path,
    out: &'a Path,
    prefix: &'a str,
    max_tasks: usize,
    profile: Option<&'a str>,
    base: Option<&'a str>,
    accept_unverified: bool,
    fake_agents: Option<&'a Path>,
}

/// The throwaway checkout of one split: removed (Git first, then the folder) when dropped —
/// also after an error or a panic.
struct Scratch<'a> {
    g: &'a GitCmd,
    repo: PathBuf,
    dir: PathBuf,
}

impl Drop for Scratch<'_> {
    fn drop(&mut self) {
        let repo = Loc::Discover(self.repo.clone());
        let fwd = self.dir.display().to_string().replace('\\', "/");
        let _ = self
            .g
            .output(&repo, false, &["worktree", "remove", "--force", &fwd]);
        if self.dir.exists() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
        let _ = self.g.output(&repo, false, &["worktree", "prune"]);
    }
}

/// The folder for the files: new, or an empty directory.
fn usable_out(out: &Path) -> Result<()> {
    if out.exists() {
        let empty = out.is_dir() && std::fs::read_dir(out)?.next().is_none();
        if !empty {
            bail!(
                "{} exists and is not an empty folder — choose a new one (nothing is overwritten)",
                out.display()
            );
        }
    }
    Ok(())
}

/// Write every file of the split into `out`, or none: `out` is checked again (the turn may have
/// taken half an hour), each file is created new — never one that appeared meanwhile — and
/// after a failure the files already written are removed again.
fn write_all_or_none(out: &Path, writes: &[(PathBuf, Vec<u8>)]) -> Result<()> {
    use std::io::Write;
    usable_out(out)?;
    std::fs::create_dir_all(out)?;
    let mut made: Vec<&Path> = Vec::new();
    for (path, bytes) in writes {
        let r = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .and_then(|mut f| f.write_all(bytes).and_then(|_| f.flush()));
        if let Err(e) = r {
            // a file that exists already was not made here: leave it
            if e.kind() != std::io::ErrorKind::AlreadyExists {
                made.push(path);
            }
            for p in &made {
                let _ = std::fs::remove_file(p);
            }
            return Err(e).with_context(|| {
                format!(
                    "writing {} — none of the split's files was kept",
                    path.display()
                )
            });
        }
        made.push(path);
    }
    Ok(())
}

/// The base commit, chosen like `task create` (X19): `--base`, else `project.base_branch`, else
/// the checked-out branch.
fn base_commit(
    g: &GitCmd,
    cfg: &Effective,
    repo: &Path,
    base: Option<&str>,
) -> Result<(String, String)> {
    let loc = Loc::Discover(repo.to_path_buf());
    let base_ref = match (base, &cfg.repo.project.base_branch) {
        (Some(b), _) => b.to_string(),
        // M16-R2: as `task create` does — the branch must exist, and the error names the key
        (None, Some(b)) => {
            let found = g.output(
                &loc,
                true,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{b}^{{commit}}"),
                ],
            )?;
            if !found.status.success() {
                bail!(
                    "project.base_branch: the branch `{b}` does not exist in {} — create it, or fix the key in {} (or remove it to start from the checked-out branch)",
                    repo.display(),
                    cfg.repo_path.display()
                );
            }
            b.clone()
        }
        (None, None) => g
            .run_str(&loc, true, &["rev-parse", "--abbrev-ref", "HEAD"])?
            .trim()
            .to_string(),
    };
    let sha = g
        .run_str(
            &loc,
            true,
            &["rev-parse", &format!("{base_ref}^{{commit}}")],
        )
        .with_context(|| {
            format!(
                "the base `{base_ref}` is not a commit of {}",
                repo.display()
            )
        })?;
    Ok((base_ref, sha.trim().to_string()))
}

/// The context of the split turn: the project's own sources (policy, `AGENTS.md`, the culture of
/// an org chart, the global context) read from the throwaway checkout, and the embedded
/// goal-splitter role text instead of the planner's (which plans one task).
fn split_context(cfg: &Effective, profile: &str, checkout: &Path) -> Result<context::Assembled> {
    let kind = cfg.profile(profile)?.kind;
    let kb = cfg
        .repo
        .context
        .budget_kb
        .get("planner")
        .copied()
        .unwrap_or(context::DEFAULT_BUDGET_KB);
    let team = cfg.team.as_ref().map(|team| TeamContext {
        culture: team
            .culture
            .as_ref()
            .map(|rel| (rel.clone(), checkout.join(rel))),
        ..Default::default()
    });
    let a = context::assemble(&Request {
        role: Role::Planner,
        kind,
        repo: checkout.to_path_buf(),
        global: context::global_context_path(),
        pack: Vec::new(),
        no_run_store: false,
        budget: (kb as usize) * 1024,
        team,
        template: Some(goal::TEMPLATE),
    })?;
    if !a.unreadable.is_empty() {
        bail!(
            "{}: {}",
            context::CONTEXT_UNREADABLE,
            a.unreadable.join("; ")
        );
    }
    if let Some(why) = context::over_budget(&a) {
        bail!("{why}");
    }
    Ok(a)
}

fn split(cli: &Cli, s: &Split) -> Result<i32> {
    goal::valid_prefix(s.prefix)?;
    usable_out(s.out)?;
    let cfg = load_config(cli)?;
    let repo = PathBuf::from(expand_vars(&cfg.repo.project.root)?);
    let text = toryo::textfile::read_text(s.file)?;
    if text.trim().is_empty() {
        bail!("{} is empty: write the goal first", s.file.display());
    }
    // the ids are permanent: none of them may exist yet — checked before a token is spent
    let st = Store::open_default()?;
    let taken: Vec<String> = (1..=s.max_tasks)
        .map(|n| format!("{}-{n}", s.prefix))
        .filter(|id| {
            let i = id.clone();
            st.read(move |tx| Ok(store::get_task(tx, &i).is_ok()))
                .unwrap_or(false)
        })
        .collect();
    if !taken.is_empty() {
        bail!(
            "the task{} {} already exist{} — choose another --prefix",
            if taken.len() == 1 { "" } else { "s" },
            taken.join(", "),
            if taken.len() == 1 { "s" } else { "" }
        );
    }
    let profile = match s.profile {
        Some(p) => p.to_string(),
        None => task::roles(&cfg)?.0,
    };
    cfg.profile(&profile)?;
    let hub = AgentHub::new(&cfg, LaunchMode::Restricted, s.fake_agents)?;
    if !hub.fakes(&profile) {
        // R51: the profile's isolation evidence matches the CLI installed now
        let bad = toryo::evidence::unverified_profiles(&cfg, &[&profile])?;
        if !bad.is_empty() {
            let list = bad
                .iter()
                .map(|(p, why)| format!("  - {p}: {why}"))
                .collect::<Vec<_>>()
                .join("\n");
            if !s.accept_unverified {
                bail!("unverified profile (R51):\n{list}\nrun `toryo doctor --canaries`, or pass --accept-unverified");
            }
            toryo::errln!(
                "WARNING: splitting with an unverified profile (--accept-unverified):\n{list}"
            );
        }
    }
    let g = task::task_git(&cfg, &repo)?;
    let (base_ref, base) = base_commit(&g, &cfg, &repo, s.base)?;
    let stamp: String = store::now().chars().filter(char::is_ascii_digit).collect();
    let home = data_dir()
        .join("goals")
        .join(format!("{}-{stamp}", s.prefix));
    std::fs::create_dir_all(&home)?;
    // from here Ctrl+C ends the split through this process (the turn is cancelled, the
    // checkout removed), never by ending it with the checkout left behind
    watch_interrupt();
    let dir = home.join("worktree");
    let fwd = dir.display().to_string().replace('\\', "/");
    let scratch = Scratch {
        g: &g,
        repo: repo.clone(),
        dir: dir.clone(),
    };
    g.run(
        &Loc::Discover(repo.clone()),
        false,
        &["worktree", "add", "-q", "--detach", &fwd, &base],
    )?;
    let ctx = split_context(&cfg, &profile, &dir)?;
    let ctx_file = home.join("context.md");
    std::fs::write(&ctx_file, &ctx.text)?;
    toryo::errln!(
        "── GOAL SPLIT ({profile}, base {base_ref} @ {}) ──\n   context: {} source(s), {} bytes",
        toryo::git::short(&base),
        ctx.sources.len(),
        ctx.text.len()
    );
    if interrupted() {
        bail!("stopped (Ctrl+C) — nothing was written");
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    {
        let (cancel, done) = (cancel.clone(), done.clone());
        std::thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                if interrupted() {
                    cancel.store(true, Ordering::SeqCst);
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
    }
    let t = &cfg.repo.timeouts;
    let outcome = hub.turn_with(
        OpenSession {
            profile: profile.clone(),
            role: Role::Planner,
            cwd: dir.clone(),
            run_store: None,
            capture_dir: home.join("captures"),
            schema: Some(toryo::engine::contracts::schema(goal::SCHEMA)?.into()),
            context_file: Some(ctx_file),
            // M16-R2: the seat's model only for the seat's own profile — another `--profile`
            // (perhaps another CLI) runs with its profile's `model`
            model: cfg
                .repo
                .roles
                .get("planner")
                .filter(|r| r.profile == profile)
                .and_then(|r| r.model.clone()),
            resume: None,
        },
        TurnInput {
            prompt: goal::prompt(&text, s.max_tasks),
            label: "split".into(),
            env: Default::default(),
            echo: true,
        },
        TurnLimits {
            max: Duration::from_secs(60 * t.agent_stage_max_min.unwrap_or(30)),
            stall: Duration::from_secs(60 * t.agent_stall_min.unwrap_or(10)),
        },
        TurnHooks {
            cancel: Some(cancel),
            ..Default::default()
        },
    );
    done.store(true, Ordering::SeqCst);
    let outcome = outcome?;
    if hub.is_fake() {
        let _ = std::fs::write(
            home.join("fake-calls.json"),
            serde_json::to_vec_pretty(&hub.fake_calls())?,
        );
    }
    // what the planner wrote in its checkout is gone with it — said, never kept
    let changed = g
        .run_str(
            &Loc::Discover(dir.clone()),
            true,
            &["status", "--porcelain"],
        )
        .map(|o| o.lines().count())
        .unwrap_or(0);
    drop(scratch);
    if changed > 0 {
        toryo::errln!(
            "!! the planner changed {changed} file(s) in its throwaway checkout — discarded"
        );
    }
    if outcome.cancelled {
        bail!("stopped (Ctrl+C) — nothing was written");
    }
    let Some(answer) = outcome.structured.clone().filter(|_| outcome.success) else {
        bail!(
            "the split turn failed ({}): {} — nothing was written; the captures are in {}",
            outcome
                .error_class
                .map(|c| format!("{c:?}"))
                .unwrap_or_else(|| "no structured answer".into()),
            outcome.error_detail.as_deref().unwrap_or("-"),
            home.join("captures").display()
        );
    };
    std::fs::write(home.join("split.json"), serde_json::to_vec_pretty(&answer)?)?;
    let tasks = goal::plan(&answer, s.prefix, s.max_tasks)?;
    let goal_name = s
        .file
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| s.file.display().to_string());
    // every file is rendered and checked before the first is written
    let mut files: Vec<(&Planned, PathBuf, String)> = Vec::new();
    for p in &tasks {
        let path = s.out.join(format!("{}.md", p.id));
        let body = goal::render(p, &goal_name, &tasks);
        goal::check_rendered(p, &body, &path)?;
        files.push((p, path, body));
    }
    let questions: Vec<String> = answer["open_questions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|q| q.as_str().map(str::to_string))
        .collect();
    let manifest = goal::manifest(
        &tasks,
        &std::path::absolute(s.file)?,
        &task::sha256_hex(text.as_bytes()),
        &base,
        &profile,
        answer["summary"].as_str().unwrap_or(""),
        &questions,
    );
    let mut writes: Vec<(PathBuf, Vec<u8>)> = files
        .iter()
        .map(|(_, path, body)| (path.clone(), body.clone().into_bytes()))
        .collect();
    writes.push((
        s.out.join(goal::MANIFEST),
        serde_json::to_vec_pretty(&manifest)?,
    ));
    write_all_or_none(s.out, &writes)?;
    let width = tasks.iter().map(|p| p.id.len()).max().unwrap_or(0);
    toryo::errln!(
        "✔ {} split into {} task(s) in {}:",
        goal_name,
        tasks.len(),
        s.out.display()
    );
    for p in &tasks {
        let after = if p.depends_on.is_empty() {
            String::new()
        } else {
            format!("  (after {})", p.depends_on.join(", "))
        };
        toryo::errln!("  {:<width$}  {}{after}", p.id, p.title);
    }
    if let Some(u) = &outcome.usage {
        toryo::errln!("   turn: {} tokens ({:?})", u.total(), u.accounting);
    }
    if !questions.is_empty() {
        toryo::errln!(
            "open questions of the planner (also in {}):",
            goal::MANIFEST
        );
        for q in &questions {
            // the planner's words: inert, never painted (X33, X34)
            toryo::term::quoted(toryo::term::Stream::Err, &format!("  - {q}"));
        }
    }
    toryo::errln!("next: read each file (they are the tasks' TASK.md, frozen at `task create`), then wave by wave:");
    for (n, wave) in goal::waves(&tasks).iter().enumerate() {
        let creates: Vec<String> = wave
            .iter()
            .map(|id| {
                format!(
                    "`toryo task create --file {}`",
                    s.out.join(format!("{id}.md")).display()
                )
            })
            .collect();
        let after = if n == 0 {
            String::new()
        } else {
            " — after the tasks it depends on are merged".to_string()
        };
        toryo::errln!(
            "  {}. {}, then `toryo run {}`{after}",
            n + 1,
            creates.join(" "),
            wave.join(" ")
        );
    }
    for (_, path, _) in &files {
        toryo::outln!("{}", path.display());
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_files_of_a_split_are_written_all_or_none() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        let w = |names: &[&str]| -> Vec<(PathBuf, Vec<u8>)> {
            names
                .iter()
                .map(|n| (out.join(n), n.as_bytes().to_vec()))
                .collect()
        };
        write_all_or_none(&out, &w(&["A-1.md", "A-2.md"])).unwrap();
        assert_eq!(std::fs::read(out.join("A-2.md")).unwrap(), b"A-2.md");
        // the folder is no longer empty: nothing is overwritten or added
        let e = write_all_or_none(&out, &w(&["A-3.md"])).unwrap_err();
        assert!(format!("{e:#}").contains("is not an empty folder"), "{e:#}");
        assert!(!out.join("A-3.md").exists());
        // a file that cannot be made undoes the ones made before it
        let fresh = d.path().join("fresh");
        let mut ws: Vec<(PathBuf, Vec<u8>)> = vec![(fresh.join("B-1.md"), b"x".to_vec())];
        ws.push((fresh.join("no-such-folder").join("B-2.md"), b"y".to_vec()));
        let e = write_all_or_none(&fresh, &ws).unwrap_err();
        assert!(
            format!("{e:#}").contains("none of the split's files was kept"),
            "{e:#}"
        );
        assert!(!fresh.join("B-1.md").exists());
    }
}
