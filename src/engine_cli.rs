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
use toryo::engine::lock::{self, StopOutcome, TaskLock};
use toryo::engine::recovery::Recovery;
use toryo::engine::runner::{
    approve_now, next_action, reject_now, task_policy, write_summary, Engine, RunEnd, RunOptions,
};
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
    let i = id.to_string();
    store.call(move |tx| store::get_task(tx, &i))
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

/// Recover (if needed) and run the locked task until it waits, finishes or is stopped.
fn drive(store: &Store, lock: &TaskLock, d: &DriveArgs) -> Result<i32> {
    // recovery launches no agent: it needs neither the hub nor the R51 check
    let t = get(store, &lock.task)?;
    let cfg = frozen_config(&t)?;
    let (g, protect, _) = task_policy(&t, &cfg)?;
    let rec = Recovery {
        store,
        g: &g,
        protect: &protect,
        cfg: &cfg,
    };
    for n in rec.recover(&lock.task)? {
        eprintln!("↺ recovery: {n}");
    }
    let t = get(store, &lock.task)?;
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
            let repo = match repo {
                Some(r) => r.clone(),
                None => PathBuf::from(toryo::config::expand_vars(&cfg.repo.project.root)?),
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
        TaskCmd::List => status(None),
        TaskCmd::Config { id, reload, expect } => task_config(cli, id, *reload, expect.as_deref()),
        TaskCmd::Show { id } => show(id),
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
            if !t.state.is_terminal() {
                let i = id.clone();
                st.call(move |tx| store::transition(tx, &i, PipelineEvent::Cancel).map(|_| ()))?;
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
            toryo::engine::verify::cleanup(&g, &t, cfg.as_ref())?;
            if t.worktree.exists() {
                let w = t.worktree.display().to_string().replace('\\', "/");
                g.run(
                    &toryo::git::Loc::Discover(t.repo.clone()),
                    false,
                    &["worktree", "remove", "--force", &w],
                )?;
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
    let new = load_config(cli)?;
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
        || serde_json::to_value(&old.repo.verify)? != serde_json::to_value(&new.repo.verify)?;
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
    let code = drive(&st, &lock, d);
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
        let t = get(&st, id)?;
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
    approve_now(&st, &t, kind, expect)?;
    let code = if cont {
        drive(&st, &lock, d)?
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

fn resume_one(st: &Store, lock: &TaskLock, r: &ResumeArgs, d: &DriveArgs) -> Result<i32> {
    let t = get(st, &lock.task)?;
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
    if protected && r.note.is_none() {
        if !resume_protected(st, &t, &r.accept_protected)? {
            return Ok(1);
        }
    } else if r.discard_partial || r.keep_partial {
        let cfg = frozen_config(&t)?;
        let (g, protect, _) = task_policy(&t, &cfg)?;
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
            &t,
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
            &t,
            "resumed from Escalated with a note (R5)",
            &format!(
                "Escalation: {}\n\nNote: {note}",
                t.machine.reason().unwrap_or("")
            ),
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
        let active = if lock::probe_free(&t.id)? {
            String::new()
        } else {
            format!(
                " · ACTIVE ({})",
                t.run_lock_owner.clone().unwrap_or_default()
            )
        };
        println!(
            "{:<14} {:<22} fix {}/{} · tokens {}{}{}{}",
            t.id,
            format!("{:?}", t.state),
            t.fix_cycle,
            t.machine.max_fix_cycles(),
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
        let next = if t.state.is_terminal() && !t.run_store.exists() && !t.worktree.exists() {
            toryo::engine::runner::cleaned_up_action(&t)
        } else {
            next_action(&t)
        };
        println!("               next: {next}");
    }
    Ok(0)
}

fn show(id: &str) -> Result<i32> {
    status(Some(id))?;
    let st = store()?;
    let t = get(&st, id)?;
    println!(
        "               worktree: {} · branch {} · run store {}",
        t.worktree.display(),
        t.branch,
        t.run_store.display()
    );
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
            a.git_head_after
                .map(|h| format!(" → {}", short(&h)))
                .unwrap_or_default(),
            a.partial_commit
                .map(|h| format!(" partial {}", short(&h)))
                .unwrap_or_default()
        );
    }
    let i = id.to_string();
    let rows = st.call(move |tx| store::usage_rows(tx, &i))?;
    if !rows.is_empty() {
        println!("\nusage (R41; ~ = reservation for a turn with no telemetry):");
        for (uid, att, prof, acc, budget, res, rec, inc) in rows {
            let amount = match (acc.as_str(), budget, res) {
                ("unknown", _, Some(r)) => format!("~{r}"),
                (_, Some(b), _) => b.to_string(),
                _ => "?".into(),
            };
            println!(
                "  #{uid:<4} {att:<22} {prof:<14} {acc:<11} {amount:>9}{}{}",
                if inc {
                    "  includes an earlier killed turn"
                } else {
                    ""
                },
                rec.map(|r| format!("  reconciled by #{r}"))
                    .unwrap_or_default()
            );
        }
    }
    let i = id.to_string();
    let ap = st.call(move |tx| store::approvals(tx, &i))?;
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
