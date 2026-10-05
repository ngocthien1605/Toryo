//! The engine commands (plan §15): `task`, `run`, `approve`, `reject`, `resume`, `stop`,
//! `status`, `partial export`. Every command that mutates a task takes its lock first
//! (R38) and exits with code 3 (`task_active`) if another process holds it.

use std::path::PathBuf;
use std::time::Duration;
use toryo::git::short;

use anyhow::{bail, Context, Result};
use clap::{Subcommand, ValueEnum};

use toryo::agents::claude::LaunchMode;
use toryo::agents::hub::AgentHub;
use toryo::engine::flight::{EngineEvent, Recorder};
use toryo::engine::lock::{self, StopOutcome, TaskLock};
use toryo::engine::recovery::Recovery;
use toryo::engine::runner::{
    approve_now, next_action, reject_now, task_policy, write_summary, Engine, RunEnd, RunOptions,
};
use toryo::engine::snapshot;
use toryo::engine::store::{self, Store, TaskRow};
use toryo::engine::task::{self, frozen_config, CreateArgs};
use toryo::engine::views::{self, TaskFacts};
use toryo::engine::worktree::Worktree;
use toryo::engine::{GateKind, PipelineEvent, PipelineState};

use crate::{load_config, Cli};

#[derive(Clone, Copy, ValueEnum)]
pub enum Gate {
    Plan,
    Final,
}

impl From<Gate> for GateKind {
    fn from(g: Gate) -> Self {
        match g {
            Gate::Plan => GateKind::Plan,
            Gate::Final => GateKind::Final,
        }
    }
}

#[derive(clap::Args, Clone, Default)]
pub struct DriveArgs {
    /// Approve both gates automatically (demos / CI only)
    #[arg(long)]
    yes: bool,
    /// R49: escalate instead of switching implementer when it is out of quota
    #[arg(long)]
    no_fallback: bool,
    /// R51: run even if a profile's canary did not pass for its installed CLI version
    #[arg(long)]
    accept_unverified: bool,
    /// Scripted agents instead of the real CLIs (tests / demos; plan §6.4)
    #[arg(long, hide = true)]
    fake_agents: Option<PathBuf>,
    /// M10: a Windows toast when the task needs you (a gate, an escalation, a pause, the end),
    /// whatever `notify.toast` in the machine config says
    #[arg(long, conflicts_with = "no_notify")]
    notify: bool,
    /// M10: no toast, whatever `notify.toast` in the machine config says
    #[arg(long)]
    no_notify: bool,
    /// The machine config in use (`--machine-config`, else the default): where `notify:` is read.
    #[arg(skip)]
    pub machine_config: Option<PathBuf>,
}

#[derive(Subcommand)]
pub enum TaskCmd {
    /// Snapshot TASK.md, freeze the config, create the worktree + branch `toryo/<id>`.
    Create {
        /// The task file (`# ID — title`, `## Acceptance criteria` with `- AC-1: …`)
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        id: Option<String>,
        /// The Git repository (default: `project.root` of toryo.yaml)
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Base branch/commit (default: the repo's current branch)
        #[arg(long)]
        base: Option<String>,
        #[arg(long)]
        max_fix_cycles: Option<u32>,
    },
    /// All tasks with state and the next human action.
    List {
        /// One JSON document on stdout (the `status --json` schema)
        #[arg(long)]
        json: bool,
    },
    /// One task: state, stage attempts, gate occurrences.
    Show {
        id: String,
        /// One JSON document on stdout: the task, its attempts, usage rows, gates, events file
        #[arg(long)]
        json: bool,
    },
    /// The frozen configuration (R35). `--reload` re-reads toryo.yaml + the machine config,
    /// shows the diff and applies it — waiting states only (a gate, Escalated, Paused).
    Config {
        id: String,
        #[arg(long)]
        reload: bool,
        /// Apply only if the new configuration's digest starts with this
        #[arg(long, requires = "reload")]
        expect: Option<String>,
    },
    /// Remove the worktree and the run store (the branch and the task record stay).
    Cleanup {
        id: String,
        /// Cancel a task that is not finished first
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum BudgetCmd {
    /// Set (raise) the admission cap; a task paused on its budget continues with `toryo resume`.
    Set {
        id: String,
        /// Tokens (weighted sum of the four buckets, R41)
        #[arg(long, conflicts_with = "none")]
        tokens: Option<u64>,
        /// Remove the cap
        #[arg(long)]
        none: bool,
    },
}

#[derive(Subcommand)]
pub enum ContextCmd {
    /// Print exactly what the agent of a stage receives: the source list on stderr, the
    /// context itself on stdout. Without --attempt: the next attempt (assembled now, nothing
    /// written); with --attempt: the stored context of that attempt (hash-checked).
    Show {
        id: String,
        /// plan, implement, fix-N or review (default for implement in a fix cycle: fix-N)
        #[arg(long)]
        stage: String,
        #[arg(long)]
        attempt: Option<u32>,
        /// Preview for another profile (default: the stage's current profile)
        #[arg(long)]
        profile: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum PartialCmd {
    /// Re-create an interrupted attempt's files from its recovery snapshot (R39).
    Export {
        id: String,
        #[arg(long)]
        attempt: u32,
        /// Stage (`implement`, `fix-1`, …); default: the latest with a snapshot
        #[arg(long)]
        stage: Option<String>,
        /// An empty or new directory (outside the task)
        #[arg(long)]
        into: PathBuf,
    },
}

fn store() -> Result<Store> {
    Store::open_default()
}

fn get(store: &Store, id: &str) -> Result<TaskRow> {
    task::valid_task_id(id)?;
    let i = id.to_string();
    store.call(move |tx| store::get_task(tx, &i))
}

/// [`get`] in a read-only transaction (R6): for the commands that only look (`status`, `task
/// show`, `usage`, `watch`).
fn read_task(store: &Store, id: &str) -> Result<TaskRow> {
    task::valid_task_id(id)?;
    let i = id.to_string();
    store.read(move |tx| store::get_task(tx, &i))
}

fn hub_for(t: &TaskRow, d: &DriveArgs) -> Result<AgentHub> {
    let cfg = frozen_config(t)?;
    let fake = d.fake_agents.as_deref();
    // the profiles that run a real CLI: all without a fake script; with one, those outside its
    // `only_profiles` (a script without the list fakes every profile)
    let script = fake
        .map(toryo::agents::fake::FakeScript::load)
        .transpose()?;
    let real = |p: &str| script.as_ref().is_none_or(|s| !s.fakes(p));
    let mut ps: Vec<&str> = vec![&t.planner, &t.reviewer];
    ps.extend(
        t.implementers
            .iter()
            .skip(t.implementer_index)
            .map(String::as_str),
    );
    ps.retain(|p| real(p));
    if !ps.is_empty() {
        // R51: isolation evidence must match the CLI versions installed now
        let mut bad = toryo::evidence::unverified_profiles(&cfg, &ps)?;
        let imps: Vec<&str> = t
            .implementers
            .iter()
            .skip(t.implementer_index)
            .map(String::as_str)
            .filter(|p| real(p))
            .collect();
        bad.extend(toryo::evidence::unverified_implementers(&cfg, &imps)?);
        if !bad.is_empty() {
            let list = bad
                .iter()
                .map(|(p, why)| format!("  - {p}: {why}"))
                .collect::<Vec<_>>()
                .join("\n");
            if d.accept_unverified {
                eprintln!(
                    "WARNING: running with unverified profiles (--accept-unverified):\n{list}"
                );
            } else {
                bail!("unverified profiles (R51):\n{list}\nrun `toryo doctor --canaries` (and `toryo spike claude-implementer-isolation --profile <p>` for a Claude implementer), or pass --accept-unverified");
            }
        }
    }
    AgentHub::new(&cfg, LaunchMode::Restricted, fake)
}

/// The flight recorder of a command holding the task lock (M10).
fn recorder(t: &TaskRow, lock: &TaskLock) -> Recorder {
    Recorder::for_task(t, lock.generation)
}

/// The recorder of a drive command: [`recorder`] plus the toast notifier when toasts are on
/// (`--notify` / `--no-notify`, else `notify.toast` of the machine config, M10-D1).
fn drive_recorder(t: &TaskRow, lock: &TaskLock, d: &DriveArgs) -> Recorder {
    let rec = recorder(t, lock);
    if toryo::notify::enabled(d.notify, d.no_notify, machine_notify(d)) {
        let n = toryo::notify::Notifier::system();
        // R4 (Q2): with --yes the gates are approved automatically — nobody has to act on them
        rec.with(Box::new(if d.yes { n.without_gates() } else { n }))
    } else {
        rec
    }
}

/// `notify:` of the machine config in use; a machine config that cannot be read turns toasts
/// off with a warning (the run itself uses the frozen configuration).
fn machine_notify(d: &DriveArgs) -> toryo::config::Notify {
    match d.machine_config.as_deref().filter(|p| p.exists()) {
        Some(p) => match toryo::config::MachineConfig::load(p) {
            Ok(m) => m.notify,
            Err(e) => {
                eprintln!("WARNING: {e:#} — no toasts");
                Default::default()
            }
        },
        None => Default::default(),
    }
}

/// Record a state change a command made outside the engine (`event`: the pipeline event's
/// name), if the state changed. The CLI prints no `[state]` line for these.
fn recorded(rec: &Recorder, from: PipelineState, event: &str, now: &TaskRow) {
    if now.state != from {
        rec.transition(from, event, &now.machine, false, false);
    }
}

fn st_attempts(store: &Store, id: &str) -> Result<Vec<store::AttemptRow>> {
    let i = id.to_string();
    store.call(move |tx| store::attempts(tx, &i))
}

/// R6: the attempts §10 recovery ended (they were `running` before it) get their
/// `stage_finished`, and a killed turn's reservation its `turn_finished` — quiet events, like the
/// engine's own.
fn recovered_events(
    store: &Store,
    rec: &Recorder,
    id: &str,
    running: &[store::AttemptRow],
    usage_before: &[i64],
) -> Result<()> {
    if running.is_empty() {
        return Ok(());
    }
    let now = st_attempts(store, id)?;
    let i = id.to_string();
    let usage = store.call(move |tx| store::usage_rows(tx, &i))?;
    for before in running {
        let Some(a) = now
            .iter()
            .find(|a| a.id == before.id && a.status != store::AttemptStatus::Running)
        else {
            continue;
        };
        let reserved = usage
            .iter()
            // R7: only a row recovery inserted — the crashed process emitted its own rows
            .find(|u| u.1 == a.id && u.3 == "unknown" && !usage_before.contains(&u.0))
            .and_then(|u| u.5);
        if let (Some(n), Some(p)) = (reserved, &a.profile) {
            rec.emit_quiet(EngineEvent::TurnFinished {
                stage: a.stage.clone(),
                attempt: a.attempt,
                profile: p.clone(),
                success: false,
                usage: None,
                reserved_tokens: Some(n),
                duration_ms: toryo::engine::flight::ms_since(&a.started_at),
            });
        }
        rec.emit_quiet(EngineEvent::StageFinished {
            stage: a.stage.clone(),
            attempt: a.attempt,
            status: a.status,
            error_class: a.error_class.clone(),
            duration_ms: toryo::engine::flight::ms_since(&a.started_at),
        });
    }
    Ok(())
}

/// Recover (if needed) and run the locked task until it waits, finishes or is stopped. Every
/// exit — also an error before the first stage (an unverified profile, a bad fake script, a
/// Git or store error) — ends with one `run_ended` (R1): `watch` stops and the owner is told.
fn drive(store: &Store, lock: &TaskLock, rec: &Recorder, d: &DriveArgs) -> Result<i32> {
    rec.emit_quiet(EngineEvent::RunStarted {
        owner: lock::owner_label(),
    });
    let from = get(store, &lock.task).ok().map(|t| t.state);
    // R7: a panic unwinding through the drive still ends the record (exit code 101 stays)
    let mut guard = RunEndGuard {
        store,
        rec,
        task: &lock.task,
        from,
        armed: true,
    };
    let res = drive_inner(store, lock, rec, d);
    guard.armed = false;
    // the full text: the recorder redacts it, then shortens it (R7)
    run_ended(
        store,
        rec,
        &lock.task,
        from,
        res.as_ref().err().map(|e| format!("{e:#}")),
    );
    res
}

/// Emit the one `run_ended` of a drive: state and next action re-read from the store, the last
/// known state if it cannot be read.
fn run_ended(
    store: &Store,
    rec: &Recorder,
    task: &str,
    from: Option<PipelineState>,
    error: Option<String>,
) {
    let (state, next) = match get(store, task) {
        Ok(t) => (t.state, next_action(&t)),
        Err(_) => (
            from.unwrap_or(PipelineState::New),
            format!("`toryo status {task}`"),
        ),
    };
    rec.emit_quiet(EngineEvent::RunEnded {
        state,
        next_action: next,
        from,
        error,
    });
}

/// Emits `run_ended { error: "panicked" }` when a panic unwinds through the drive (R7).
struct RunEndGuard<'a> {
    store: &'a Store,
    rec: &'a Recorder,
    task: &'a str,
    from: Option<PipelineState>,
    armed: bool,
}

impl Drop for RunEndGuard<'_> {
    fn drop(&mut self) {
        if self.armed && std::thread::panicking() {
            run_ended(
                self.store,
                self.rec,
                self.task,
                self.from,
                Some("panicked".into()),
            );
        }
    }
}

fn drive_inner(store: &Store, lock: &TaskLock, rec: &Recorder, d: &DriveArgs) -> Result<i32> {
    // recovery launches no agent: it needs neither the hub nor the R51 check
    let t = get(store, &lock.task)?;
    let cfg = frozen_config(&t)?;
    let (g, protect, _) = task_policy(&t, &cfg)?;
    let recovery = Recovery {
        store,
        g: &g,
        protect: &protect,
        cfg: &cfg,
    };
    let running: Vec<store::AttemptRow> = {
        let i = lock.task.clone();
        st_attempts(store, &i)?
            .into_iter()
            .filter(|a| a.status == store::AttemptStatus::Running)
            .collect()
    };
    let usage_before: Vec<i64> = {
        let i = lock.task.clone();
        store
            .call(move |tx| store::usage_rows(tx, &i))?
            .into_iter()
            .map(|u| u.0)
            .collect()
    };
    for n in recovery.recover(&lock.task)? {
        eprintln!("↺ recovery: {n}");
    }
    recovered_events(store, rec, &lock.task, &running, &usage_before)?;
    let before = t.state;
    let t = get(store, &lock.task)?;
    recorded(rec, before, "recovery", &t);
    let waiting = t.state.is_terminal()
        || matches!(t.state, PipelineState::Escalated | PipelineState::Paused)
        || (!d.yes
            && matches!(
                t.state,
                PipelineState::WaitingPlanApproval | PipelineState::WaitingFinalApproval
            ));
    let end = if waiting {
        if t.state.is_terminal() {
            RunEnd::Done(t.state)
        } else {
            RunEnd::Waiting(t.state)
        }
    } else {
        let hub = hub_for(&t, d)?;
        let mut eng = Engine::new(
            store,
            &hub,
            lock,
            rec,
            RunOptions {
                yes: d.yes,
                no_fallback: d.no_fallback,
            },
        )?;
        let end = eng.run();
        if hub.is_fake() {
            let p = t
                .captures()
                .join(format!("fake-calls-{}.json", lock.generation));
            let _ = std::fs::create_dir_all(t.captures());
            std::fs::write(&p, serde_json::to_vec_pretty(&hub.fake_calls())?)?;
        }
        end?
    };
    let t = get(store, &lock.task)?;
    let summary = write_summary(store, &t)?;
    eprintln!("\n══════════ {}: {:?} ══════════", t.id, t.state);
    if t.state == PipelineState::WaitingFinalApproval {
        for n in toryo::engine::runner::final_gate_notes(store, &t)? {
            eprintln!("final gate — {n}");
        }
    }
    eprintln!("next: {}", next_action(&t));
    eprintln!("summary: {}", summary.display());
    Ok(match end {
        RunEnd::Done(PipelineState::Completed) | RunEnd::Stopped => 0,
        RunEnd::Waiting(
            PipelineState::WaitingPlanApproval | PipelineState::WaitingFinalApproval,
        ) => 0,
        _ => 1,
    })
}

pub fn task(cli: &Cli, cmd: &TaskCmd) -> Result<i32> {
    match cmd {
        TaskCmd::Create {
            file,
            id,
            repo,
            base,
            max_fix_cycles,
        } => {
            let cfg = load_config(cli)?;
            let root = PathBuf::from(toryo::config::expand_vars(&cfg.repo.project.root)?);
            let repo = match repo {
                // M11-R8: the org chart was validated from project.root, but instructions,
                // culture and skills are read from the task's repository — they must be one
                Some(r)
                    if cfg.team.is_some()
                        && (std::fs::canonicalize(r).ok() != std::fs::canonicalize(&root).ok()
                            || !r.is_dir()) =>
                {
                    bail!(
                        "task create --repo {}: in org chart mode the task's repository must be project.root ({}) — the org chart, the role files and the skills are read from it; change project.root in {} or leave --repo out",
                        r.display(),
                        root.display(),
                        cfg.repo_path.display()
                    );
                }
                Some(r) => r.clone(),
                None => root,
            };
            let st = store()?;
            let t = task::create(
                &cfg,
                &st,
                CreateArgs {
                    id: id.clone(),
                    repo,
                    file: file.clone(),
                    base: base.clone(),
                    max_fix_cycles: *max_fix_cycles,
                },
            )?;
            println!("{}", t.id);
            eprintln!(
                "created {}: {} acceptance criteria {:?}\n  worktree {} · branch {} · base {}\n  planner {} · implementer {} · reviewer {}\n  run store {}\nnext: `toryo run {}`",
                t.id,
                t.acceptance_ids.len(),
                t.acceptance_ids,
                t.worktree.display(),
                t.branch,
                short(&t.base_commit),
                t.planner,
                t.implementers.join(" → (fallback) "),
                t.reviewer,
                t.run_store.display(),
                t.id
            );
            Ok(0)
        }
        TaskCmd::List { json } => status(None, *json),
        TaskCmd::Config { id, reload, expect } => task_config(cli, id, *reload, expect.as_deref()),
        TaskCmd::Show { id, json } => show(id, *json),
        TaskCmd::Cleanup { id, force } => {
            let st = store()?;
            let lock = TaskLock::acquire(&st, id)?;
            let t = get(&st, id)?;
            if !t.state.is_terminal() && t.state != PipelineState::Escalated && !force {
                bail!(
                    "{id} is {:?}: pass --force to cancel it and clean up",
                    t.state
                );
            }
            // EC-12: a task whose frozen configuration cannot be read can still be removed
            let cfg = match frozen_config(&t) {
                Ok(c) => Some(c),
                Err(e) => {
                    eprintln!("WARNING: {e:#} — cleaning up without it (every repository filter driver neutralised)");
                    None
                }
            };
            let g = match &cfg {
                Some(c) => task::task_git(c, &t.repo)?,
                None => task::cleanup_git(&t.repo)?,
            };
            // M12-12 (a): what cleanup deletes cannot be had back — say what, and ask for --force
            if !*force {
                // M13-07: a cleanup that cannot look does not delete
                let lost = match cleanup_would_lose(&g, &t) {
                    Ok(lost) => lost,
                    Err(e) => bail!(
                        "{id}: cannot tell what the worktree holds ({e:#}) — pass `--force` to delete it anyway (`toryo task cleanup {id} --force`)"
                    ),
                };
                if !lost.is_empty() {
                    bail!(
                        "{id}: `task cleanup` would delete {} — look at it first (`git -C {} status`); `toryo task cleanup {id} --force` deletes it anyway",
                        lost.join(" and "),
                        t.worktree.display()
                    );
                }
            }
            if !t.state.is_terminal() {
                let i = id.clone();
                st.call(move |tx| store::transition(tx, &i, PipelineEvent::Cancel).map(|_| ()))?;
            }
            toryo::engine::verify::cleanup(&g, &t, cfg.as_ref())?;
            if t.worktree.exists() {
                let w = t.worktree.display().to_string().replace('\\', "/");
                let repo = toryo::git::Loc::Discover(t.repo.clone());
                if let Err(e) = g.run(&repo, false, &["worktree", "remove", "--force", &w]) {
                    // M13-07: Git does not remove a worktree it cannot look into (a broken
                    // `.git` file) — `--force` deletes the folder itself and forgets it
                    if !*force {
                        return Err(e);
                    }
                    eprintln!("note: {e:#} — removing the folder itself");
                    std::fs::remove_dir_all(&t.worktree)
                        .with_context(|| format!("removing {}", t.worktree.display()))?;
                    let _ = g.run(&repo, false, &["worktree", "prune"]);
                }
            }
            if t.run_store.exists() {
                std::fs::remove_dir_all(&t.run_store)
                    .with_context(|| format!("removing {}", t.run_store.display()))?;
            }
            let i = id.clone();
            st.call(move |tx| {
                store::event(tx, Some(&i), None, "cleaned_up", &serde_json::json!({}))
            })?;
            lock.release();
            eprintln!("removed the worktree and the run store of {id}; the branch {} is kept (`git branch -D {}` when you no longer need it)", t.branch, t.branch);
            Ok(0)
        }
    }
}

/// M12-12 (a): what `task cleanup` would delete that exists nowhere else — uncommitted changes
/// in the worktree and the partial snapshots of the run store (the branch is kept). Empty: a
/// clean worktree and no snapshot. `Err`: Git could not say what the worktree holds (M13-07).
fn cleanup_would_lose(g: &toryo::git::GitCmd, t: &TaskRow) -> Result<Vec<String>> {
    let mut lost = Vec::new();
    if t.worktree.exists() {
        let n = toryo::engine::worktree::Worktree::new(g, &t.worktree)
            .changed()?
            .len();
        if n > 0 {
            lost.push(format!(
                "{n} uncommitted change(s) in the worktree {}",
                t.worktree.display()
            ));
        }
    }
    let partial = t.run_store.join("artifacts").join("partial");
    let n = std::fs::read_dir(&partial).map_or(0, |d| d.flatten().count());
    if n > 0 {
        lost.push(format!(
            "the partial snapshot(s) of {n} attempt(s) in {}",
            partial.display()
        ));
    }
    Ok(lost)
}

fn task_config(cli: &Cli, id: &str, reload: bool, expect: Option<&str>) -> Result<i32> {
    use toryo::engine::artifacts;
    use toryo::engine::task::sha256_hex;
    let st = store()?;
    if !reload {
        let t = get(&st, id)?;
        let (_, text) = task::frozen_config_text(&t)?;
        eprintln!(
            "{id}: frozen configuration, digest {}",
            short(&t.config_sha256)
        );
        let v: serde_json::Value = serde_json::from_str(&text)?;
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(0);
    }
    let lock = TaskLock::acquire(&st, id)?;
    let t = get(&st, id)?;
    if !t.state.is_waiting() {
        bail!("{id} is {:?}: `task config --reload` only in a waiting state (a gate, Escalated, Paused)", t.state);
    }
    let (old_path, old_text) = task::frozen_config_text(&t)?;
    let old = toryo::config::Effective::from_frozen(&old_text, &old_path)?;
    let mut new = load_config(cli)?;
    // review H2 (M11-A2): a reload cannot go below the machine floor either
    if let Some(reason) = toryo::engine::verify::below_floor(&new) {
        bail!("{reason}");
    }
    // M12-05: an environment gate this machine cannot start is refused here too
    if let Some(reason) = toryo::engine::gates::env_gates_refusal(&new) {
        bail!("{reason}");
    }
    // M11-R21: a changed `verify_root` is probed as `task create` and `doctor` do — a root
    // Low-integrity code can write would hand the export and the gate's folder to it
    if toryo::engine::verify::verify_root(&new) != toryo::engine::verify::verify_root(&old) {
        toryo::engine::verify::probe(&new)?;
    }
    // M11-R14 (X9): the entry scripts that are build output are part of the frozen
    // configuration — recomputed against the task's own base commit (the refusals follow below)
    new.build_entries = task::build_entries(
        &new,
        &toryo::git::default_gitcmd()?,
        &t.repo,
        &t.base_commit,
    )?;
    let new_text = new.canonical_json();
    let new_sha = sha256_hex(new_text.as_bytes());
    if new_sha == t.config_sha256 {
        eprintln!(
            "{id}: the effective configuration is unchanged ({})",
            short(&new_sha)
        );
        lock.release();
        return Ok(0);
    }
    // diff of the pretty-printed configurations
    let dir = t.artifacts().join("config-history");
    std::fs::create_dir_all(&dir)?;
    let n = std::fs::read_dir(&dir)?.count() / 2 + 1;
    let pretty = |s: &str| -> Result<String> {
        Ok(serde_json::to_string_pretty(&serde_json::from_str::<serde_json::Value>(s)?)? + "\n")
    };
    let (a, b) = (
        dir.join(format!("{n}-old-{}.json", short(&t.config_sha256))),
        dir.join(format!("{n}-new-{}.json", short(&new_sha))),
    );
    std::fs::write(&a, pretty(&old_text)?)?;
    std::fs::write(&b, pretty(&new_text)?)?;
    let g = task::task_git(&new, &t.repo)?; // R37 admission under the new machine config
                                            // M18 (group E review): a reloaded gate script must be in the base commit too
    task::check_entry_points(&new, &g, &t.repo, &t.base_commit)?;
    let out = g.output(
        &toryo::git::Loc::Discover(dir.clone()),
        true,
        &[
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--",
            &a.display().to_string(),
            &b.display().to_string(),
        ],
    )?;
    eprintln!("{}", String::from_utf8_lossy(&out.stdout));
    eprintln!(
        "{id}: new configuration digest {} (was {})",
        short(&new_sha),
        short(&t.config_sha256)
    );
    if let Some(e) = expect {
        if !new_sha.starts_with(e) {
            bail!("subject_changed: the new configuration digest is {} — it does not start with --expect {e}; nothing was changed", short(&new_sha));
        }
    }
    let (planner, implementers, reviewer) = task::roles(&new)?;
    let idx = t
        .current_implementer()
        .and_then(|c| implementers.iter().position(|x| x == c))
        .unwrap_or(0);
    let verify_changed = serde_json::to_value(&old.repo.commands.verify)?
        != serde_json::to_value(&new.repo.commands.verify)?
        || serde_json::to_value(&old.repo.verify)? != serde_json::to_value(&new.repo.verify)?
        || old.machine_verify != new.machine_verify // M11-A2
        // M11 (group E review): the gates, their machine commands, the role files
        || serde_json::to_value(&old.team)? != serde_json::to_value(&new.team)?
        || toryo::engine::xpp::build_toolchain(&old) != toryo::engine::xpp::build_toolchain(&new); // R59
    let reverify = verify_changed && artifacts::head(&st, &t, "verify-result")?.is_some();
    let invalidated = serde_json::to_vec_pretty(
        &serde_json::json!({"invalidated": true, "reason": "task config --reload changed the verify configuration", "config_sha256": new_sha}),
    )?;
    // EC-12: publish the new versions first — a crash up to the commit below leaves orphans
    // and the old configuration in force, and the reload can simply be repeated …
    let config_v = artifacts::publish(
        &st,
        &t,
        None,
        "config",
        "json",
        "config",
        new_text.as_bytes(),
    )?;
    let verify_v = reverify
        .then(|| {
            let (l, k) = ("verify-result", "verify_result");
            artifacts::publish(&st, &t, None, l, "json", k, &invalidated)
        })
        .transpose()?;
    // … then one transaction registers them and switches tasks.config_sha256 (R34/R35)
    let (i, sha, im, pl, rv) = (
        id.to_string(),
        new_sha.clone(),
        implementers.clone(),
        planner.clone(),
        reviewer.clone(),
    );
    let to_verify = reverify && t.state == PipelineState::WaitingFinalApproval;
    let payload = serde_json::json!({"from": t.config_sha256, "to": new_sha, "verify_changed": verify_changed});
    let (cv, vv) = (config_v.clone(), verify_v.clone());
    st.call(move |tx| {
        artifacts::register(tx, &cv)?;
        if let Some(v) = &vv {
            artifacts::register(tx, v)?;
        }
        store::set_config(tx, &i, &sha, &pl, &im, &rv, idx)?;
        store::event(tx, Some(&i), None, "config_reloaded", &payload)?;
        if to_verify {
            store::transition(tx, &i, PipelineEvent::Reverify)?;
        }
        Ok(())
    })?;
    config_v.committed();
    if to_verify {
        recorded(&recorder(&t, &lock), t.state, "reverify", &get(&st, id)?);
    }
    artifacts::write_view(&t, "config.json", new_text.as_bytes())?;
    let mut notes = vec![format!(
        "configuration {} → {} (diff: {})",
        short(&t.config_sha256),
        short(&new_sha),
        b.display()
    )];
    if let Some(v) = &verify_v {
        v.committed();
        artifacts::write_view(&t, "VERIFY_RESULT.json", &invalidated)?;
        notes.push("the verify configuration changed: VERIFY_RESULT.json invalidated".into());
    }
    if to_verify {
        notes.push("the final gate was withdrawn: the checkpoint is verified again".into());
    }
    toryo::engine::decisions::append(
        &st,
        &t,
        "configuration reloaded (R35)",
        &notes
            .iter()
            .map(|x| format!("- {x}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )?;
    for x in &notes {
        eprintln!("{id}: {x}");
    }
    let t = get(&st, id)?;
    eprintln!("next: {}", next_action(&t));
    lock.release();
    Ok(0)
}

pub fn run(id: &str, d: &DriveArgs) -> Result<i32> {
    let st = store()?;
    let lock = TaskLock::acquire(&st, id)?;
    let rec = drive_recorder(&get(&st, id)?, &lock, d);
    let code = drive(&st, &lock, &rec, d);
    lock.release();
    code
}

pub fn approve(
    id: &str,
    gate: Gate,
    expect: Option<&str>,
    force: bool,
    cont: bool,
    d: &DriveArgs,
) -> Result<i32> {
    let st = store()?;
    let lock = TaskLock::acquire(&st, id)?;
    let t = get(&st, id)?;
    // only a drive (`--continue`) raises toasts: the human is at the keyboard otherwise
    let rec = if cont {
        drive_recorder(&t, &lock, d)
    } else {
        recorder(&t, &lock)
    };
    let kind: GateKind = gate.into();
    if force {
        if kind != GateKind::Final || t.state != PipelineState::Escalated {
            bail!(
                "--force only takes an Escalated task to the final gate (state {:?})",
                t.state
            );
        }
        if !toryo::engine::runner::verify_current(&st, &t)? {
            bail!("{id}: no passing VERIFY_RESULT.json for the current checkpoint (e.g. the verify configuration was reloaded) — `toryo resume {id} --note \"…\"` to verify again");
        }
        let i = id.to_string();
        st.call(move |tx| store::transition(tx, &i, PipelineEvent::ForceFinal).map(|_| ()))?;
        toryo::engine::decisions::append(
            &st,
            &t,
            "final gate forced (reviewer skipped)",
            &format!("From Escalated: {}", t.machine.reason().unwrap_or("")),
        )?;
        let before = t.state;
        let t = get(&st, id)?;
        // the owner just opened this gate: recorded, never toasted (R1)
        recorded(&recorder(&t, &lock), before, "force_final", &t);
        eprintln!(
            "{id}: reviewer skipped — the final gate is open for checkpoint {}",
            t.machine.checkpoint().unwrap_or("")
        );
        eprintln!("next: {}", next_action(&t));
        lock.release();
        return Ok(0);
    }
    if let Some(g) = t.machine.gate() {
        let digest =
            toryo::engine::runner::gate_digest(&st, id, g.kind, g.occurrence).unwrap_or_default();
        eprintln!(
            "{id}: {:?} gate, occurrence {}, subject {} (digest {})",
            g.kind,
            g.occurrence,
            &g.subject[..12.min(g.subject.len())],
            &digest[..12.min(digest.len())]
        );
    }
    approve_now(&st, &rec, &t, kind, expect)?;
    let code = if cont {
        drive(&st, &lock, &rec, d)?
    } else {
        let t = get(&st, id)?;
        write_summary(&st, &t)?;
        eprintln!("next: {}", next_action(&t));
        0
    };
    lock.release();
    Ok(code)
}

pub fn reject(id: &str, gate: Gate, reason: &str) -> Result<i32> {
    if reason.trim().is_empty() {
        bail!("--reason must say what to change");
    }
    let st = store()?;
    let lock = TaskLock::acquire(&st, id)?;
    let t = get(&st, id)?;
    reject_now(&st, &recorder(&t, &lock), &t, gate.into(), reason)?;
    let t = get(&st, id)?;
    eprintln!("next: {}", next_action(&t));
    lock.release();
    Ok(0)
}

#[derive(Default)]
pub struct ResumeArgs {
    pub note: Option<String>,
    pub discard_partial: bool,
    pub keep_partial: bool,
    pub accept_protected: Vec<String>,
}

/// `resume` of a task escalated with `protected_config_changed` (R35): re-compare; restored →
/// back into the stage; accepted paths → new baseline + DECISIONS.md. Returns false when the
/// change is still there and not accepted.
fn resume_protected(st: &Store, t: &TaskRow, accept: &[String]) -> Result<bool> {
    let cfg = frozen_config(t)?;
    let (g, protect, _) = task_policy(t, &cfg)?;
    let c = toryo::engine::protect::check(st, t, &g, &protect)?;
    let bad: Vec<&toryo::engine::protect::Change> = c
        .changes
        .iter()
        .filter(|x| x.is_hard() || !protect.is_allowed(&x.path))
        .collect();
    let i = t.id.clone();
    if bad.is_empty() {
        st.call(move |tx| {
            store::event(
                tx,
                Some(&i),
                None,
                "protected_restored",
                &serde_json::json!({}),
            )?;
            store::transition(tx, &i, PipelineEvent::ResumeStage).map(|_| ())
        })?;
        eprintln!(
            "{}: protected state matches the baseline again — the stage continues",
            t.id
        );
        return Ok(true);
    }
    let norm = |p: &str| p.replace('\\', "/").trim_start_matches("./").to_string();
    let accepted: Vec<String> = accept.iter().map(|p| norm(p)).collect();
    let missing: Vec<String> = bad
        .iter()
        .filter(|x| !accepted.contains(&x.path))
        .map(|x| format!("{} {}", x.kind, x.path))
        .collect();
    if accept.is_empty() || !missing.is_empty() {
        eprintln!(
            "{}: protected state still differs from the baseline: {}
  diff: {}
  → restore, or `toryo resume {} --accept-protected-change <path>` for each",
            t.id,
            if accept.is_empty() {
                bad.iter()
                    .map(|x| format!("{} {}", x.kind, x.path))
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                format!("not accepted: {}", missing.join(", "))
            },
            c.diff_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "`git diff` in the worktree".into()),
            t.id
        );
        return Ok(false);
    }
    let list: Vec<String> = bad
        .iter()
        .map(|x| format!("{} {}", x.kind, x.path))
        .collect();
    let m = toryo::engine::protect::take_baseline(
        st,
        t,
        &g,
        &protect,
        &format!("accepted by the human: {}", list.join(", ")),
    )?;
    toryo::engine::decisions::append(
        st,
        t,
        "protected change accepted (R35)",
        &format!(
            "Accepted out-of-band changes to protected state: {}.\nNew protected baseline v{}. Diff: {}",
            list.join(", "),
            m.version,
            c.diff_path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "(tracked: git diff)".into())
        ),
    )?;
    st.call(move |tx| store::transition(tx, &i, PipelineEvent::ResumeStage).map(|_| ()))?;
    eprintln!(
        "{}: accepted {} — protected baseline v{}",
        t.id,
        list.join(", "),
        m.version
    );
    Ok(true)
}

/// M11-R11 (X8): the task is `Escalated` with a machine-caused class, from a running state — a
/// plain `toryo resume` goes back into the stage (`ResumeStage`).
fn machine_caused_stop(t: &TaskRow) -> bool {
    t.state == PipelineState::Escalated
        && t.machine
            .reason_class()
            .is_some_and(toryo::engine::state::is_machine_caused)
        && t.machine.escalated_from().is_some_and(|s| s.is_running())
}

/// M12-07: the task is stopped with `environment_gate_confirm` — it waits for the owner's "yes".
fn waits_for_a_deploy_yes(t: &TaskRow) -> bool {
    machine_caused_stop(t) && t.machine.reason_class() == Some(toryo::engine::gates::ENV_CONFIRM)
}

fn resume_one(st: &Store, lock: &TaskLock, r: &ResumeArgs, d: &DriveArgs) -> Result<i32> {
    let t = get(st, &lock.task)?;
    // M12-07 (X17): `--yes` never confirms a deploy — it is for the plan and the final gate
    if d.yes
        && waits_for_a_deploy_yes(&t)
        && r.note.is_none()
        && !r.discard_partial
        && !r.keep_partial
    {
        bail!(
            "{}: --yes never confirms a deploy. Look at the checkpoint, then `toryo resume {}` without --yes (the final gate is then approved with `toryo approve {} --gate final`)",
            t.id,
            t.id,
            t.id
        );
    }
    let protected = t.state == PipelineState::Escalated
        && t.machine
            .reason()
            .is_some_and(|x| x.starts_with(toryo::engine::protect::REASON));
    if !r.accept_protected.is_empty() && !protected {
        bail!(
            "--accept-protected-change: {} is not waiting on protected_config_changed (state {:?})",
            t.id,
            t.state
        );
    }
    let rec = drive_recorder(&t, lock, d);
    let event = if protected && r.note.is_none() {
        "resume_stage"
    } else if r.discard_partial || r.keep_partial {
        "recovery"
    } else if r.note.is_some() {
        "resume_with_note"
    } else if machine_caused_stop(&t) {
        // M11-R16: a plain resume after a machine-caused escalation is `ResumeStage`
        "resume_stage"
    } else {
        "unpause"
    };
    let go = resume_before_drive(st, &t, r, protected)?;
    recorded(&rec, t.state, event, &get(st, &lock.task)?);
    if !go {
        return Ok(1); // the protected change is still there and not accepted
    }
    drive(st, lock, &rec, d)
}

/// What `resume` changes before the task is driven again: the protected-state check, a
/// partial-work decision, a note, or the end of a pause. False: the protected change is still
/// there and not accepted — the task is not driven.
fn resume_before_drive(st: &Store, t: &TaskRow, r: &ResumeArgs, protected: bool) -> Result<bool> {
    if protected && r.note.is_none() {
        return resume_protected(st, t, &r.accept_protected);
    } else if r.discard_partial || r.keep_partial {
        let cfg = frozen_config(t)?;
        let (g, protect, _) = task_policy(t, &cfg)?;
        let rec = Recovery {
            store: st,
            g: &g,
            protect: &protect,
            cfg: &cfg,
        };
        let msg = if r.discard_partial {
            rec.discard_partial(&t.id)?
        } else {
            rec.keep_partial(&t.id)?
        };
        eprintln!("↺ {msg}");
        toryo::engine::decisions::append(
            st,
            t,
            if r.discard_partial {
                "partial work discarded (R31)"
            } else {
                "partial work kept (R31)"
            },
            &msg,
        )?;
    } else if let Some(note) = &r.note {
        if t.state != PipelineState::Escalated {
            bail!("--note leaves Escalated; {} is {:?}", t.id, t.state);
        }
        let (i, n) = (t.id.clone(), note.clone());
        st.call(move |tx| {
            store::set_feedback(tx, &i, Some(&format!("- HUMAN (resume note): {n}\n")))?;
            store::transition(tx, &i, PipelineEvent::ResumeWithNote { note: n })?;
            Ok(())
        })?;
        toryo::engine::decisions::append(
            st,
            t,
            "resumed from Escalated with a note (R5)",
            &format!(
                "Escalation: {}\n\nNote: {note}",
                t.machine.reason().unwrap_or("")
            ),
        )?;
    } else if machine_caused_stop(t) {
        // M11-R11 (X8): the machine caused this (a missing skill, a context over budget, a gate
        // the machine cannot run) — a plain `toryo resume` goes back into the stage, which
        // checks its precondition again. No fix cycle, no implementer turn for nothing.
        let class = t.machine.reason_class().unwrap_or_default().to_string();
        // M12-07 (X17): this resume is the owner's "yes" to the deploy the stop asked about.
        // M13-05 (X26): the yes is written in the same transaction as the transition
        let confirms = class == toryo::engine::gates::ENV_CONFIRM;
        let i = t.id.clone();
        let said = st.call(move |tx| {
            let said = if confirms {
                store::confirm_deploys(tx, &i)?
            } else {
                Vec::new()
            };
            store::transition(tx, &i, PipelineEvent::ResumeStage)?;
            Ok(said)
        })?;
        if confirms {
            eprintln!(
                "{}: deploy confirmed ({})",
                t.id,
                said.iter()
                    .map(|(gate, commit)| format!("{gate} @ {}", short(commit)))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        toryo::engine::decisions::append(
            st,
            t,
            &format!("resumed after {class} (M11-R11)"),
            t.machine.reason().unwrap_or(""),
        )?;
    } else if t.state == PipelineState::Paused {
        // R49: after an implementer quota pause the chain starts again from the primary
        let quota = t
            .machine
            .reason()
            .is_some_and(|x| x.starts_with("implementer_quota"));
        let i = t.id.clone();
        st.call(move |tx| {
            if quota {
                store::set_implementer_index(tx, &i, 0)?;
                store::event(
                    tx,
                    Some(&i),
                    None,
                    "implementer_chain_reset",
                    &serde_json::json!({"index": 0}),
                )?;
            }
            store::transition(tx, &i, PipelineEvent::Unpause).map(|_| ())
        })?;
    }
    Ok(true)
}

pub fn resume(id: Option<&str>, r: &ResumeArgs, d: &DriveArgs) -> Result<i32> {
    // M12-12 (b): these flags belong to one task — `resume --note "x"` used to be accepted
    // and then ignored
    if id.is_none() {
        let one = [
            ("--note", r.note.is_some()),
            ("--discard-partial", r.discard_partial),
            ("--keep-partial", r.keep_partial),
            ("--accept-protected-change", !r.accept_protected.is_empty()),
        ];
        if let Some((flag, _)) = one.iter().find(|(_, set)| *set) {
            bail!("{flag} belongs to one task — `toryo resume <id> {flag} …`; without an id `toryo resume` takes every waiting task as it is");
        }
    }
    let st = store()?;
    match id {
        Some(id) => {
            let lock = TaskLock::acquire(&st, id)?;
            let code = resume_one(&st, &lock, r, d);
            lock.release();
            code
        }
        None => {
            // lock-first (R38): held tasks are reported and skipped, never recovered
            let tasks = st.call(store::list_tasks)?;
            let mut code = 0;
            for t in tasks.into_iter().filter(|t| !t.state.is_terminal()) {
                // M12-12 (b): a task that was never started is `run`, not `resume`
                if t.state == PipelineState::New {
                    eprintln!("\n{}: not started — `toryo run {}`", t.id, t.id);
                    continue;
                }
                // M12-07 (review): a stop that asks before a deploy is answered by `resume <id>`
                // — never by a resume of everything
                if waits_for_a_deploy_yes(&t) {
                    eprintln!(
                        "\n{}: waits for your yes before it deploys — look at it, then `toryo resume {}`",
                        t.id, t.id
                    );
                    code = code.max(1);
                    continue;
                }
                match TaskLock::acquire(&st, &t.id) {
                    Ok(lock) => {
                        eprintln!("\n▶ resume {}", t.id);
                        // M12-12 (b): one failing task does not stop the others; the exit
                        // code says that one failed
                        match resume_one(&st, &lock, &ResumeArgs::default(), d) {
                            Ok(c) => code = code.max(c),
                            Err(e) => {
                                eprintln!("{}: error: {e:#}", t.id);
                                code = code.max(2);
                            }
                        }
                        lock.release();
                    }
                    Err(e) => match e.downcast_ref::<lock::TaskActive>() {
                        Some(a) => eprintln!("{} — skipped", a),
                        None => return Err(e),
                    },
                }
            }
            Ok(code)
        }
    }
}

pub fn stop(id: &str, wait: u64) -> Result<i32> {
    let st = store()?;
    get(&st, id)?;
    eprintln!("stop requested for {id}; waiting up to {wait}s …");
    Ok(match lock::stop(&st, id, Duration::from_secs(wait))? {
        StopOutcome::Stopped(o) => {
            eprintln!("{id}: {o}");
            0
        }
        StopOutcome::NoOwner => {
            eprintln!("{id}: no_owner — nothing was running");
            0
        }
        StopOutcome::Pending => {
            eprintln!("{id}: the owner has not answered yet (the request stays for it)");
            1
        }
    })
}

/// What `status` shows of a task besides its row: the lock owner while another process holds
/// the lock, compile-only verify (R59), and the next action (the cleaned-up variant once the
/// worktree and the run store are gone).
fn facts(st: &Store, t: &TaskRow) -> Result<TaskFacts> {
    // R6: from the recorded owner, never a lock probe; `None`, never an empty label
    let active_owner = driven_owner(st, &t.id)?;
    let next_action = if t.state.is_terminal() && !t.run_store.exists() && !t.worktree.exists() {
        toryo::engine::runner::cleaned_up_action(t)
    } else {
        next_action(t)
    };
    Ok(TaskFacts {
        active_owner,
        // R6: unknown (`null`) when the frozen configuration cannot be read (after `task cleanup`)
        compile_only: task::frozen_config(t)
            .ok()
            .map(|c| toryo::engine::xpp::compile_only(&c)),
        next_action,
    })
}

/// Print one JSON document on stdout (`--json`, DES-10).
fn print_json(v: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// M11-E3: what each environment lock's environment holds — every lock, or only the locks
/// `task` deployed to — with its status in words and whether that task's final gate is
/// approved.
fn environments(
    st: &Store,
    task: Option<&str>,
) -> Result<Vec<(store::Deployment, &'static str, bool)>> {
    let rows = st.read(store::deployments)?;
    let mut out = Vec::new();
    for d in rows
        .into_iter()
        .filter(|d| task.is_none_or(|i| d.task == i))
    {
        let i = d.task.clone();
        let holder = st.read(move |tx| {
            Ok(store::get_task(tx, &i)
                .ok()
                .map(|t| (t.state, t.machine.checkpoint().map(str::to_string))))
        })?;
        let state = holder.as_ref().map(|h| h.0);
        // M11-R8: the head the holder's final gate approved — only a completed task has one
        let approved = holder
            .as_ref()
            .filter(|h| h.0 == PipelineState::Completed)
            .and_then(|h| h.1.clone());
        let active = driven_owner(st, &d.task).ok().flatten().is_some();
        let status = views::deployment_status(&d, state, approved.as_deref(), active);
        out.push((d, status, status == "approved"));
    }
    Ok(out)
}

pub fn status(id: Option<&str>, json: bool) -> Result<i32> {
    let st = store()?;
    let tasks = match id {
        Some(i) => vec![read_task(&st, i)?],
        None => st.read(store::list_tasks)?,
    };
    let envs = environments(&st, id)?;
    if json {
        let list = tasks
            .iter()
            .map(|t| Ok(views::task_json(t, &facts(&st, t)?)))
            .collect::<Result<Vec<_>>>()?;
        let mut doc = serde_json::json!({ "tasks": list });
        // M11-E3: only when an environment gate has deployed (the M10 document otherwise)
        if !envs.is_empty() {
            doc["environments"] = envs
                .iter()
                .map(|(d, s, a)| views::deployment_json(d, s, *a))
                .collect();
        }
        print_json(&doc)?;
        return Ok(0);
    }
    if tasks.is_empty() {
        println!("no tasks (`toryo task create --file TASK.md`)");
    }
    for t in tasks {
        let f = facts(&st, &t)?;
        let active = f
            .active_owner
            .as_ref()
            .map(|o| format!(" · ACTIVE ({o})"))
            .unwrap_or_default();
        println!(
            "{:<14} {:<22} fix {} · tokens {}{}{}{}",
            t.id,
            format!("{:?}", t.state),
            views::fix_cycles(&t, "/"),
            t.spent_tokens,
            if t.reserved_tokens > 0 {
                format!(" (+~{} reserved)", t.reserved_tokens)
            } else {
                String::new()
            },
            t.budget_tokens
                .map(|b| format!(" / {b} admission cap (may overshoot by one turn)"))
                .unwrap_or_default(),
            active
        );
        // R59: an X++ verify proves compile level only — say so wherever the state is shown
        if f.compile_only == Some(true) {
            println!("               {}", toryo::engine::xpp::COMPILE_ONLY);
        }
        // M11-E3: an environment gate waiting for its machine-wide lock (only while its run is
        // alive: a crash while waiting leaves the row behind — group E review)
        if t.state == PipelineState::Verifying && f.active_owner.is_some() {
            let i = t.id.clone();
            if let Some(lock) = st.read(move |tx| store::env_wait(tx, &i))? {
                let holder = st
                    .read(store::deployments)?
                    .into_iter()
                    .find(|d| d.lock == lock && d.finished_at.is_none() && d.task != t.id)
                    .map(|d| format!(" (held by {})", d.task))
                    .unwrap_or_default();
                println!("               waiting for lock {lock}{holder}");
            }
        }
        println!("               next: {}", f.next_action);
    }
    // M11-E3: one line per environment lock — what code that environment runs
    for (d, s, _) in &envs {
        println!("environment    {}", views::deployment_line(d, s));
    }
    Ok(0)
}

/// The usage rows of a task as `task show` prints them.
fn usage_lines(st: &Store, id: &str) -> Result<Vec<views::UsageLine>> {
    let i = id.to_string();
    let rows = st.read(move |tx| store::usage_rows(tx, &i))?;
    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                attempt,
                profile,
                accounting,
                budget_tokens,
                reservation_tokens,
                reconciled_by,
                includes_unreported,
            )| {
                views::UsageLine {
                    id,
                    attempt,
                    profile,
                    accounting,
                    budget_tokens,
                    reservation_tokens,
                    reconciled_by,
                    includes_unreported,
                }
            },
        )
        .collect())
}

fn show(id: &str, json: bool) -> Result<i32> {
    let st = store()?;
    let t = read_task(&st, id)?;
    let i = id.to_string();
    let atts = st.read(move |tx| store::attempts(tx, &i))?;
    let usage = usage_lines(&st, id)?;
    let i = id.to_string();
    let ap = st.read(move |tx| store::approvals(tx, &i))?;
    if json {
        let task = views::task_json(&t, &facts(&st, &t)?);
        let events = toryo::engine::flight::events_path(&t);
        let mut doc = views::show_json(task, &t, &atts, &usage, &ap, &events);
        // M11-E3: the environments this task deployed to (only then)
        let envs = environments(&st, Some(id))?;
        if !envs.is_empty() {
            doc["environments"] = envs
                .iter()
                .map(|(d, s, a)| views::deployment_json(d, s, *a))
                .collect();
        }
        print_json(&doc)?;
        return Ok(0);
    }
    status(Some(id), false)?;
    println!(
        "               worktree: {} · branch {} · run store {}",
        t.worktree.display(),
        t.branch,
        t.run_store.display()
    );
    println!("\nstage attempts:");
    for a in atts {
        println!(
            "  {:<22} {:<11} {:<14} {}{}{}",
            a.id,
            format!("{:?}", a.status),
            a.profile.unwrap_or_default(),
            a.error_class.unwrap_or_default(),
            a.git_head_after
                .map(|h| format!(" → {}", short(&h)))
                .unwrap_or_default(),
            a.partial_commit
                .map(|h| format!(" partial {}", short(&h)))
                .unwrap_or_default()
        );
    }
    if !usage.is_empty() {
        println!("\nusage (R41; ~ = reservation for a turn with no telemetry):");
        for u in usage {
            let amount = match (u.accounting.as_str(), u.budget_tokens, u.reservation_tokens) {
                ("unknown", _, Some(r)) => format!("~{r}"),
                (_, Some(b), _) => b.to_string(),
                _ => "?".into(),
            };
            println!(
                "  #{:<4} {:<22} {:<14} {:<11} {amount:>9}{}{}",
                u.id,
                u.attempt,
                u.profile,
                u.accounting,
                if u.includes_unreported {
                    "  includes an earlier killed turn"
                } else {
                    ""
                },
                u.reconciled_by
                    .map(|r| format!("  reconciled by #{r}"))
                    .unwrap_or_default()
            );
        }
    }
    if !ap.is_empty() {
        println!("\ngates:");
        for a in ap {
            println!(
                "  {} #{} {:<10} digest {} {}",
                a.gate,
                a.occurrence,
                a.status,
                short(&a.subject_digest),
                a.reason.unwrap_or_default()
            );
        }
    }
    Ok(0)
}

pub fn budget(cmd: &BudgetCmd) -> Result<i32> {
    let BudgetCmd::Set { id, tokens, none } = cmd;
    if tokens.is_none() && !none {
        bail!("give --tokens N or --none");
    }
    let st = store()?;
    let lock = TaskLock::acquire(&st, id)?;
    let t = get(&st, id)?;
    if t.state.is_terminal() {
        bail!("{id} is {:?}", t.state);
    }
    let new = tokens.map(|n| n as i64);
    let (i, n) = (id.clone(), new);
    st.call(move |tx| {
        store::set_budget(tx, &i, n)?;
        store::event(
            tx,
            Some(&i),
            None,
            "budget_set",
            &serde_json::json!({"tokens": n}),
        )
    })?;
    let used = t.spent_tokens + t.reserved_tokens;
    toryo::engine::decisions::append(
        &st,
        &t,
        "budget set (R41)",
        &format!(
            "Admission cap {} → {} ({} tokens used: {} spent + ~{} reserved).",
            t.budget_tokens
                .map(|b| b.to_string())
                .unwrap_or_else(|| "none".into()),
            new.map(|b| b.to_string()).unwrap_or_else(|| "none".into()),
            used,
            t.spent_tokens,
            t.reserved_tokens
        ),
    )?;
    eprintln!(
        "{id}: admission cap {} (used {used}; may overshoot by one turn){}",
        new.map(|b| b.to_string()).unwrap_or_else(|| "none".into()),
        if t.state == PipelineState::Paused {
            format!(" — `toryo resume {id}` to continue")
        } else {
            String::new()
        }
    );
    lock.release();
    Ok(0)
}

pub fn context(cmd: &ContextCmd) -> Result<i32> {
    use toryo::engine::context as cx;
    let ContextCmd::Show {
        id,
        stage,
        attempt,
        profile,
    } = cmd;
    let st = store()?;
    let t = get(&st, id)?;
    let stage = if stage == "implement" {
        toryo::engine::runner::writing_stage(t.fix_cycle)
    } else {
        stage.clone()
    };
    cx::stage_role(&stage)?;
    match attempt {
        Some(n) => {
            let (i, s2) = (id.clone(), stage.clone());
            let atts = st.call(move |tx| store::attempts(tx, &i))?;
            let a = atts
                .into_iter()
                .find(|a| a.stage == s2 && a.attempt == *n)
                .with_context(|| format!("{id}: no attempt {stage}/{n}"))?;
            let v = toryo::engine::artifacts::head(&st, &t, &cx::logical(&stage, *n))?
                .with_context(|| format!("{id}/{stage}/{n}: no stored context"))?;
            let bytes = std::fs::read(&v.path)
                .with_context(|| format!("artifact_missing: {} ({})", v.id, v.path.display()))?;
            let text = String::from_utf8_lossy(&bytes).to_string();
            let sha = toryo::engine::task::sha256_hex(&bytes);
            let ok = a.context_sha256.as_deref() == Some(sha.as_str()) && v.sha256 == sha;
            eprintln!(
                "{}: context of {} ({}), sha256 {} — {}",
                id,
                a.id,
                a.profile.unwrap_or_default(),
                short(&sha),
                if ok {
                    "matches stage_attempts.context_sha256"
                } else {
                    "DOES NOT MATCH stage_attempts.context_sha256 (artifact_tampered)"
                }
            );
            print!("{text}");
            eprintln!(
                "(the user prompt sent with it — for the Claude implementer with the <untrusted_data> blocks, R61 — is captured in {})",
                t.captures().display()
            );
            Ok(if ok { 0 } else { 1 })
        }
        None => {
            let cfg = frozen_config(&t)?;
            let profile = match profile {
                Some(p) => p.clone(),
                None => cx::stage_profile(&t, &stage)?,
            };
            let c = cx::assemble(&cx::request(&st, &t, &cfg, &stage, &profile)?)?;
            eprint!(
                "{id}: next `{stage}` attempt with `{profile}` — {}",
                cx::describe(&c)
            );
            print!("{}", c.text);
            // R61: what goes into the user prompt instead of the context (Claude implementer)
            if let Some(d) = c.data_prompt() {
                print!(
                    "\n<!-- user prompt part: untrusted task files as data blocks (R61) -->\n{d}"
                );
            }
            Ok(0)
        }
    }
}

pub fn partial(cmd: &PartialCmd) -> Result<i32> {
    match cmd {
        PartialCmd::Export {
            id,
            attempt,
            stage,
            into,
        } => {
            // writes only into an empty directory outside the task: no lock (R38)
            let st = store()?;
            let t = get(&st, id)?;
            let i = id.clone();
            let atts = st.call(move |tx| store::attempts(tx, &i))?;
            let a = atts
                .into_iter()
                .rev()
                .find(|a| {
                    a.attempt == *attempt
                        && a.partial_snapshot_id.is_some()
                        && stage.as_deref().is_none_or(|s| s == a.stage)
                })
                .with_context(|| format!("{id}: no attempt {attempt} with a recovery snapshot"))?;
            let sid = a.partial_snapshot_id.clone().unwrap_or_default();
            let (_, rel, _) = st.call(move |tx| store::artifact(tx, &sid))?;
            let dir = t.run_store.join(rel);
            let cfg = frozen_config(&t)?;
            let g = task::task_git(&cfg, &t.repo)?;
            let m = snapshot::export(&Worktree::new(&g, &t.worktree), &dir, into)?;
            eprintln!(
                "{}: {} entries of {} restored into {} (a detached worktree at {}; remove it with `git worktree remove`)",
                id,
                m.entries.len(),
                a.id,
                into.display(),
                short(&m.head)
            );
            Ok(0)
        }
    }
}

/// Ctrl+C during `watch`: set, not killed, so `watch` exits 0.
static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Route Ctrl+C to [`INTERRUPTED`] (no new crate: the console handler on Windows, SIGINT on
/// Unix).
fn catch_ctrl_c() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
        unsafe extern "system" fn handler(kind: u32) -> windows_sys::Win32::Foundation::BOOL {
            // CTRL_C_EVENT, CTRL_BREAK_EVENT; anything else (close, logoff) as usual
            if kind <= 1 {
                INTERRUPTED.store(true, std::sync::atomic::Ordering::SeqCst);
                return 1;
            }
            0
        }
        // SAFETY: registers a handler that only stores to an atomic
        unsafe {
            SetConsoleCtrlHandler(Some(handler), 1);
        }
    }
    #[cfg(unix)]
    {
        extern "C" fn handler(_: libc::c_int) {
            INTERRUPTED.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        // SAFETY: the handler only stores to an atomic (async-signal-safe)
        unsafe {
            libc::signal(libc::SIGINT, handler as *const () as libc::sighandler_t);
        }
    }
}

/// Sleep `d` in short steps; true when Ctrl+C was pressed.
fn interrupted_during(d: Duration) -> bool {
    let end = std::time::Instant::now() + d;
    while std::time::Instant::now() < end {
        if INTERRUPTED.load(std::sync::atomic::Ordering::SeqCst) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    INTERRUPTED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Does a live process drive the task? From the recorded lock owner (its PID and creation
/// time), never by taking the lock: a probe could refuse a `toryo run` starting at that moment.
fn driven(st: &Store, id: &str) -> Result<bool> {
    Ok(driven_owner(st, id)?.is_some())
}

/// The recorded lock owner, while its process (PID and creation time) is alive — never by
/// taking the lock (R6: a probe could refuse a `toryo run` starting at that moment).
fn driven_owner(st: &Store, id: &str) -> Result<Option<String>> {
    let i = id.to_string();
    let owner = st.read(move |tx| store::lock_owner(tx, &i))?;
    Ok(match owner {
        Some((label, _, Some(pid), Some(created))) if store::identity_alive(pid, &created) => {
            Some(if label.trim().is_empty() {
                format!("pid {pid}")
            } else {
                label
            })
        }
        _ => None,
    })
}

/// `toryo watch` (B2): the task's flight recorder like `tail -f`, without the task lock. The
/// history first (the last 20 events, or all with `from_start`), then new events every ~500 ms,
/// rendered or (`json`) as the file's lines, until the task stops moving on its own
/// (`flight::watch_done`) or Ctrl+C.
pub fn watch(id: &str, json: bool, from_start: bool, no_follow: bool) -> Result<i32> {
    use toryo::engine::flight::{self, Tail};
    let st = store()?;
    let t = read_task(&st, id)?;
    let path = flight::events_path(&t);
    if !path.is_file() && (no_follow || !driven(&st, id)?) {
        bail!(
            "{id}: no flight recorder at {} — the task has not run since M10, or `task cleanup` removed its run store",
            path.display()
        );
    }
    catch_ctrl_c();
    let mut tail = Tail::default();
    let history = tail.poll(&path)?;
    // a reader that went away (`watch … | head -n 5`) ends `watch` with exit 0 (R1)
    if !show_lines(flight::history(&history, from_start), json)? || no_follow {
        return Ok(0);
    }
    let mut last = std::time::Instant::now();
    // the engine commits a transition before it writes its events: "done" must hold on two
    // polls in a row, and the file is read once more before returning (R1)
    let mut done = flight::DonePolls::default();
    loop {
        if interrupted_during(Duration::from_millis(500)) {
            return Ok(0);
        }
        let new = tail.poll(&path)?;
        if !new.is_empty() {
            last = std::time::Instant::now();
            if !show_lines(&new, json)? {
                return Ok(0);
            }
        }
        let state = read_task(&st, id)?.state;
        let now_done = flight::watch_done(state, driven(&st, id)?, last.elapsed());
        if done.observe(now_done, !new.is_empty()) {
            show_lines(&tail.poll(&path)?, json)?;
            return Ok(0);
        }
    }
}

/// Print the `watch` text of `lines` on stdout. `false`: stdout is closed (a broken pipe).
fn show_lines(lines: &[String], json: bool) -> Result<bool> {
    use std::io::Write;
    let out = std::io::stdout();
    let mut out = out.lock();
    for line in lines {
        if let Some(text) = toryo::engine::flight::watch_line(line, json) {
            match writeln!(out, "{text}").and_then(|_| out.flush()) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(false),
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(true)
}

/// `toryo usage` (C1, plan §12): the task's tokens per stage × profile × model and the estimated
/// cost from the machine's price table (`--prices`, else `%USERPROFILE%\.toryo\prices.toml`).
/// Read-only: no lock. Rows written before M10 have no model: the task's frozen configuration
/// names it (role → profile), else `unknown model`.
pub fn usage(id: &str, json: bool, prices: Option<&std::path::Path>) -> Result<i32> {
    use toryo::engine::usage_report as ur;
    let st = store()?;
    let t = read_task(&st, id)?;
    let cfg = frozen_config(&t).ok();
    let i = id.to_string();
    let mut rows = st.read(move |tx| store::usage_detail(tx, &i))?;
    for r in &mut rows {
        if r.model.is_none() {
            r.model = cfg
                .as_ref()
                .and_then(|c| ur::configured_model(c, &r.stage, &r.profile));
        }
    }
    let path = prices
        .map(std::path::Path::to_path_buf)
        .or_else(ur::default_prices_path);
    let table = match &path {
        Some(p) => ur::load_prices(p)?,
        None => None,
    };
    let r = ur::report(&rows, table.as_ref());
    if json {
        print_json(&ur::to_json(&r, id, path.as_deref()))?;
    } else {
        print!("{}", ur::render_text(&r, id, path.as_deref()));
    }
    Ok(0)
}
