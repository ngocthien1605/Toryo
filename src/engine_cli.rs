//! The engine commands (plan §15): `task`, `run`, `approve`, `reject`, `resume`, `stop`,
//! `status`, `partial export`. Every command that mutates a task takes its lock first
//! (R38) and exits with code 3 (`task_active`) if another process holds it.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Subcommand, ValueEnum};

use toryo::agents::claude::LaunchMode;
use toryo::agents::hub::AgentHub;
use toryo::engine::lock::{self, StopOutcome, TaskLock};
use toryo::engine::recovery::Recovery;
use toryo::engine::runner::{approve_now, next_action, reject_now, task_policy, write_summary, Engine, RunEnd, RunOptions};
use toryo::engine::snapshot;
use toryo::engine::store::{self, Store, TaskRow};
use toryo::engine::task::{self, frozen_config, CreateArgs};
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
    List,
    /// One task: state, stage attempts, gate occurrences.
    Show { id: String },
    /// Remove the worktree and the run store (the branch and the task record stay).
    Cleanup {
        id: String,
        /// Cancel a task that is not finished first
        #[arg(long)]
        force: bool,
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
    let i = id.to_string();
    store.call(move |tx| store::get_task(tx, &i))
}

fn hub_for(t: &TaskRow, d: &DriveArgs) -> Result<AgentHub> {
    let cfg = frozen_config(t)?;
    let fake = d.fake_agents.as_deref();
    if fake.is_none() {
        // R51: isolation evidence must match the CLI versions installed now
        let mut ps: Vec<&str> = vec![&t.planner, &t.reviewer];
        ps.extend(t.implementers.iter().skip(t.implementer_index).map(String::as_str));
        let bad = toryo::evidence::unverified_profiles(&cfg, &ps)?;
        if !bad.is_empty() {
            let list = bad.iter().map(|(p, why)| format!("  - {p}: {why}")).collect::<Vec<_>>().join("\n");
            if d.accept_unverified {
                eprintln!("WARNING: running with unverified profiles (--accept-unverified):\n{list}");
            } else {
                bail!("unverified profiles (R51):\n{list}\nrun `toryo doctor --canaries`, or pass --accept-unverified");
            }
        }
    }
    AgentHub::new(&cfg, LaunchMode::Restricted, fake)
}

/// Recover (if needed) and run the locked task until it waits, finishes or is stopped.
fn drive(store: &Store, lock: &TaskLock, d: &DriveArgs) -> Result<i32> {
    // recovery launches no agent: it needs neither the hub nor the R51 check
    let t = get(store, &lock.task)?;
    let (g, protect, _) = task_policy(&t, &frozen_config(&t)?)?;
    let rec = Recovery { store, g: &g, protect: &protect };
    for n in rec.recover(&lock.task)? {
        eprintln!("↺ recovery: {n}");
    }
    let t = get(store, &lock.task)?;
    let waiting = t.state.is_terminal()
        || matches!(t.state, PipelineState::Escalated | PipelineState::Paused)
        || (!d.yes && matches!(t.state, PipelineState::WaitingPlanApproval | PipelineState::WaitingFinalApproval));
    let end = if waiting {
        if t.state.is_terminal() { RunEnd::Done(t.state) } else { RunEnd::Waiting(t.state) }
    } else {
        let hub = hub_for(&t, d)?;
        let mut eng = Engine::new(store, &hub, lock, RunOptions { yes: d.yes, no_fallback: d.no_fallback })?;
        let end = eng.run();
        if hub.is_fake() {
            let p = t.captures().join(format!("fake-calls-{}.json", lock.generation));
            let _ = std::fs::create_dir_all(t.captures());
            std::fs::write(&p, serde_json::to_vec_pretty(&hub.fake_calls())?)?;
        }
        end?
    };
    let t = get(store, &lock.task)?;
    let summary = write_summary(store, &t)?;
    eprintln!("\n══════════ {}: {:?} ══════════", t.id, t.state);
    eprintln!("next: {}", next_action(&t));
    eprintln!("summary: {}", summary.display());
    Ok(match end {
        RunEnd::Done(PipelineState::Completed) | RunEnd::Stopped => 0,
        RunEnd::Waiting(PipelineState::WaitingPlanApproval | PipelineState::WaitingFinalApproval) => 0,
        _ => 1,
    })
}

pub fn task(cli: &Cli, cmd: &TaskCmd) -> Result<i32> {
    match cmd {
        TaskCmd::Create { file, id, repo, base, max_fix_cycles } => {
            let cfg = load_config(cli)?;
            let repo = match repo {
                Some(r) => r.clone(),
                None => PathBuf::from(toryo::config::expand_vars(&cfg.repo.project.root)?),
            };
            let st = store()?;
            let t = task::create(&cfg, &st, CreateArgs { id: id.clone(), repo, file: file.clone(), base: base.clone(), max_fix_cycles: *max_fix_cycles })?;
            println!("{}", t.id);
            eprintln!(
                "created {}: {} acceptance criteria {:?}\n  worktree {} · branch {} · base {}\n  planner {} · implementer {} · reviewer {}\n  run store {}\nnext: `toryo run {}`",
                t.id,
                t.acceptance_ids.len(),
                t.acceptance_ids,
                t.worktree.display(),
                t.branch,
                &t.base_commit[..12],
                t.planner,
                t.implementers.join(" → (fallback) "),
                t.reviewer,
                t.run_store.display(),
                t.id
            );
            Ok(0)
        }
        TaskCmd::List => status(None),
        TaskCmd::Show { id } => show(id),
        TaskCmd::Cleanup { id, force } => {
            let st = store()?;
            let lock = TaskLock::acquire(&st, id)?;
            let t = get(&st, id)?;
            if !t.state.is_terminal() && t.state != PipelineState::Escalated && !force {
                bail!("{id} is {:?}: pass --force to cancel it and clean up", t.state);
            }
            if !t.state.is_terminal() {
                let i = id.clone();
                st.call(move |tx| store::transition(tx, &i, PipelineEvent::Cancel).map(|_| ()))?;
            }
            let cfg = frozen_config(&t)?;
            let g = task::task_git(&cfg, &t.repo)?;
            if t.worktree.exists() {
                let w = t.worktree.display().to_string().replace('\\', "/");
                g.run(&toryo::git::Loc::Discover(t.repo.clone()), false, &["worktree", "remove", "--force", &w])?;
            }
            if t.run_store.exists() {
                std::fs::remove_dir_all(&t.run_store).with_context(|| format!("removing {}", t.run_store.display()))?;
            }
            let i = id.clone();
            st.call(move |tx| store::event(tx, Some(&i), None, "cleaned_up", &serde_json::json!({})))?;
            lock.release();
            eprintln!("removed the worktree and the run store of {id}; the branch {} is kept (`git branch -D {}` when you no longer need it)", t.branch, t.branch);
            Ok(0)
        }
    }
}

pub fn run(id: &str, d: &DriveArgs) -> Result<i32> {
    let st = store()?;
    let lock = TaskLock::acquire(&st, id)?;
    let code = drive(&st, &lock, d);
    lock.release();
    code
}

pub fn approve(id: &str, gate: Gate, expect: Option<&str>, force: bool, cont: bool, d: &DriveArgs) -> Result<i32> {
    let st = store()?;
    let lock = TaskLock::acquire(&st, id)?;
    let t = get(&st, id)?;
    let kind: GateKind = gate.into();
    if force {
        if kind != GateKind::Final || t.state != PipelineState::Escalated {
            bail!("--force only takes an Escalated task to the final gate (state {:?})", t.state);
        }
        let i = id.to_string();
        st.call(move |tx| store::transition(tx, &i, PipelineEvent::ForceFinal).map(|_| ()))?;
        let t = get(&st, id)?;
        eprintln!("{id}: reviewer skipped — the final gate is open for checkpoint {}", t.machine.checkpoint().unwrap_or(""));
        eprintln!("next: {}", next_action(&t));
        lock.release();
        return Ok(0);
    }
    if let Some(g) = t.machine.gate() {
        let digest = toryo::engine::runner::gate_digest(&st, id, g.kind, g.occurrence).unwrap_or_default();
        eprintln!("{id}: {:?} gate, occurrence {}, subject {} (digest {})", g.kind, g.occurrence, &g.subject[..12.min(g.subject.len())], &digest[..12.min(digest.len())]);
    }
    approve_now(&st, &t, kind, expect)?;
    let code = if cont { drive(&st, &lock, d)? } else {
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
    reject_now(&st, &t, gate.into(), reason)?;
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
}

fn resume_one(st: &Store, lock: &TaskLock, r: &ResumeArgs, d: &DriveArgs) -> Result<i32> {
    let t = get(st, &lock.task)?;
    if r.discard_partial || r.keep_partial {
        let (g, protect, _) = task_policy(&t, &frozen_config(&t)?)?;
        let rec = Recovery { store: st, g: &g, protect: &protect };
        let msg = if r.discard_partial { rec.discard_partial(&t.id)? } else { rec.keep_partial(&t.id)? };
        eprintln!("↺ {msg}");
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
    } else if t.state == PipelineState::Paused {
        let i = t.id.clone();
        st.call(move |tx| store::transition(tx, &i, PipelineEvent::BudgetResumed).map(|_| ()))?;
    }
    drive(st, lock, d)
}

pub fn resume(id: Option<&str>, r: &ResumeArgs, d: &DriveArgs) -> Result<i32> {
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
                match TaskLock::acquire(&st, &t.id) {
                    Ok(lock) => {
                        eprintln!("\n▶ resume {}", t.id);
                        let c = resume_one(&st, &lock, &ResumeArgs::default(), d)?;
                        code = code.max(c);
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

pub fn status(id: Option<&str>) -> Result<i32> {
    let st = store()?;
    let tasks = match id {
        Some(i) => vec![get(&st, i)?],
        None => st.call(store::list_tasks)?,
    };
    if tasks.is_empty() {
        println!("no tasks (`toryo task create --file TASK.md`)");
    }
    for t in tasks {
        let active = if lock::probe_free(&t.id)? { String::new() } else { format!(" · ACTIVE ({})", t.run_lock_owner.clone().unwrap_or_default()) };
        println!(
            "{:<14} {:<22} fix {}/{} · tokens {}{}{}",
            t.id,
            format!("{:?}", t.state),
            t.fix_cycle,
            t.machine.max_fix_cycles(),
            t.spent_tokens,
            if t.reserved_tokens > 0 { format!(" (+~{} reserved)", t.reserved_tokens) } else { String::new() },
            active
        );
        println!("               next: {}", next_action(&t));
    }
    Ok(0)
}

fn show(id: &str) -> Result<i32> {
    status(Some(id))?;
    let st = store()?;
    let i = id.to_string();
    let atts = st.call(move |tx| store::attempts(tx, &i))?;
    println!("\nstage attempts:");
    for a in atts {
        println!(
            "  {:<22} {:<11} {:<14} {}{}{}",
            a.id,
            format!("{:?}", a.status),
            a.profile.unwrap_or_default(),
            a.error_class.unwrap_or_default(),
            a.git_head_after.map(|h| format!(" → {}", &h[..12])).unwrap_or_default(),
            a.partial_commit.map(|h| format!(" partial {}", &h[..12])).unwrap_or_default()
        );
    }
    let i = id.to_string();
    let ap = st.call(move |tx| store::approvals(tx, &i))?;
    if !ap.is_empty() {
        println!("\ngates:");
        for a in ap {
            println!("  {} #{} {:<10} digest {} {}", a.gate, a.occurrence, a.status, &a.subject_digest[..12], a.reason.unwrap_or_default());
        }
    }
    Ok(0)
}

pub fn partial(cmd: &PartialCmd) -> Result<i32> {
    match cmd {
        PartialCmd::Export { id, attempt, stage, into } => {
            // writes only into an empty directory outside the task: no lock (R38)
            let st = store()?;
            let t = get(&st, id)?;
            let i = id.clone();
            let atts = st.call(move |tx| store::attempts(tx, &i))?;
            let a = atts
                .into_iter()
                .rev()
                .find(|a| a.attempt == *attempt && a.partial_snapshot_id.is_some() && stage.as_deref().is_none_or(|s| s == a.stage))
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
                &m.head[..12]
            );
            Ok(0)
        }
    }
}
