//! The library half of the engine commands (R23-34, review 2026-10-23 A5b; moved out of the
//! bin's `engine_cli.rs` as it was): what `status`, `resume`, `task cleanup` and a drive decide
//! from the store and the task row alone — no clap, no console of their own — so it can be
//! unit-tested, plus [`with_locked`], the preamble of every command that changes a task.

use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::engine::flight::{EngineEvent, Recorder};
use crate::engine::lock::TaskLock;
use crate::engine::recovery::Recovery;
use crate::engine::runner::{next_action, task_policy, RunEnd};
use crate::engine::store::{self, Store, TaskRow};
use crate::engine::task::{self, frozen_config};
use crate::engine::views::{self, TaskFacts};
use crate::engine::{PipelineEvent, PipelineState};
use crate::git::short;

/// R23-34: the preamble of a command that changes task `id` — open the store, take the task's
/// lock first (R38, lock-first), read the row under it — then `f`; the lock is released when `f`
/// returns, also on an error (its `Drop`, CODE-09).
pub fn with_locked<T>(
    id: &str,
    f: impl FnOnce(&Store, &TaskLock, TaskRow) -> Result<T>,
) -> Result<T> {
    let st = store()?;
    let lock = TaskLock::acquire(&st, id)?;
    let t = get(&st, id)?;
    let out = f(&st, &lock, t);
    lock.release();
    out
}

pub fn store() -> Result<Store> {
    Store::open_default()
}

pub fn get(store: &Store, id: &str) -> Result<TaskRow> {
    task::valid_task_id(id)?;
    let i = id.to_string();
    store.call(move |tx| store::get_task(tx, &i))
}

/// [`get`] in a read-only transaction (R6): for the commands that only look (`status`, `task
/// show`, `usage`, `watch`).
pub fn read_task(store: &Store, id: &str) -> Result<TaskRow> {
    task::valid_task_id(id)?;
    let i = id.to_string();
    store.read(move |tx| store::get_task(tx, &i))
}

pub fn st_attempts(store: &Store, id: &str) -> Result<Vec<store::AttemptRow>> {
    let i = id.to_string();
    store.call(move |tx| store::attempts(tx, &i))
}

/// R6: the attempts §10 recovery ended (they were `running` before it) get their
/// `stage_finished`, and a killed turn's reservation its `turn_finished` — quiet events, like the
/// engine's own.
pub fn recovered_events(
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
                duration_ms: crate::engine::flight::ms_since(&a.started_at),
            });
        }
        rec.emit_quiet(EngineEvent::StageFinished {
            stage: a.stage.clone(),
            attempt: a.attempt,
            status: a.status,
            error_class: a.error_class.clone(),
            duration_ms: crate::engine::flight::ms_since(&a.started_at),
        });
    }
    Ok(())
}

/// X39: `run_ended.outcome` of how the engine ended the drive.
pub fn end_outcome(end: &RunEnd) -> &'static str {
    match end {
        RunEnd::Done(_) => "done",
        RunEnd::Waiting(_) => "waiting",
        RunEnd::Stopped => "stopped",
    }
}

/// Emit the one `run_ended` of a drive: state and next action re-read from the store, the last
/// known state if it cannot be read.
pub fn run_ended(
    store: &Store,
    rec: &Recorder,
    task: &str,
    from: Option<PipelineState>,
    error: Option<String>,
    outcome: &str,
    exit_code: i32,
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
        outcome: Some(outcome.into()),
        exit_code: Some(exit_code),
    });
}

/// R23-24: the run store of a task row is `<data dir>\runs\<id>`, and its worktree a folder
/// named `<id>` (under the frozen `project.worktrees_root` when the configuration reads):
/// what `task create` made. A row that names anything else — a changed database (N-11: nothing
/// guards it from a process with the owner's token) — makes `task cleanup` refuse, before it
/// deletes a folder it never made.
pub fn cleanup_confined(t: &TaskRow, cfg: Option<&crate::config::Effective>) -> Result<()> {
    let id = &t.id;
    let named = |p: &std::path::Path| p.file_name().is_some_and(|n| n == id.as_str());
    let under = |p: &std::path::Path, root: &std::path::Path| {
        let canon = |q: &std::path::Path| std::fs::canonicalize(q).unwrap_or(q.to_path_buf());
        canon(p.parent().unwrap_or(p)) == canon(root)
    };
    if !(named(&t.run_store) && under(&t.run_store, &crate::config::data_dir().join("runs"))) {
        bail!(
            "{id}: the run store of the task row is {}, not {} — `task cleanup` deletes only what `task create` made; nothing was deleted",
            t.run_store.display(),
            task::run_store_dir(id).display()
        );
    }
    let root = cfg
        .and_then(|c| crate::config::expand_vars(&c.repo.project.worktrees_root).ok())
        .map(PathBuf::from)
        // (a relative root was read against the folder `task create` ran in: only the name counts)
        .filter(|r| r.is_absolute());
    let ok = named(&t.worktree) && root.is_none_or(|r| under(&t.worktree, &r));
    if !ok {
        bail!(
            "{id}: the worktree of the task row is {}, not a folder `{id}` in project.worktrees_root — `task cleanup` deletes only what `task create` made; nothing was deleted",
            t.worktree.display()
        );
    }
    Ok(())
}

/// M12-12 (a): what `task cleanup` would delete that exists nowhere else — uncommitted changes
/// in the worktree and the partial snapshots of the run store (the branch is kept). Empty: a
/// clean worktree and no snapshot. `Err`: Git could not say what the worktree holds (M13-07).
pub fn cleanup_would_lose(g: &crate::git::GitCmd, t: &TaskRow) -> Result<Vec<String>> {
    let mut lost = Vec::new();
    if t.worktree.exists() {
        let n = crate::engine::worktree::Worktree::new(g, &t.worktree)
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
pub fn resume_protected(st: &Store, t: &TaskRow, accept: &[String]) -> Result<bool> {
    let cfg = frozen_config(t)?;
    let (g, protect, _) = task_policy(t, &cfg)?;
    let c = crate::engine::protect::check(st, t, &g, &protect)?;
    let bad: Vec<&crate::engine::protect::Change> = c
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
        crate::errln!(
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
        crate::errln!(
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
    let m = crate::engine::protect::take_baseline(
        st,
        t,
        &g,
        &protect,
        &format!("accepted by the human: {}", list.join(", ")),
    )?;
    crate::engine::decisions::append(
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
    crate::errln!(
        "{}: accepted {} — protected baseline v{}",
        t.id,
        list.join(", "),
        m.version
    );
    Ok(true)
}

/// M11-R11 (X8): the task is `Escalated` with a machine-caused class, from a running state — a
/// plain `toryo resume` goes back into the stage (`ResumeStage`).
pub fn machine_caused_stop(t: &TaskRow) -> bool {
    t.state == PipelineState::Escalated
        && t.machine
            .reason_class()
            .is_some_and(crate::engine::state::is_machine_caused)
        && t.machine.escalated_from().is_some_and(|s| s.is_running())
}

/// M12-07: the task is stopped with `environment_gate_confirm` — it waits for the owner's "yes".
pub fn waits_for_a_deploy_yes(t: &TaskRow) -> bool {
    machine_caused_stop(t) && t.machine.reason_class() == Some(crate::engine::gates::ENV_CONFIRM)
}

/// The message of a task that waits for the owner's "yes" before a deploy, which a `resume` of
/// every task never gives (M12-07 review).
pub fn deploy_yes_note(id: &str) -> String {
    format!("{id}: waits for your yes before it deploys — look at it, then `toryo resume {id}`")
}

/// What `resume` changes before the task is driven again: the protected-state check, a
/// partial-work decision, a note, or the end of a pause. False: the protected change is still
/// there and not accepted — the task is not driven.
pub fn resume_before_drive(
    st: &Store,
    t: &TaskRow,
    r: &ResumeArgs,
    protected: bool,
) -> Result<bool> {
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
        crate::errln!("↺ {msg}");
        crate::engine::decisions::append(
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
        crate::engine::decisions::append(
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
        let confirms = class == crate::engine::gates::ENV_CONFIRM;
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
            crate::errln!(
                "{}: deploy confirmed ({})",
                t.id,
                said.iter()
                    .map(|(gate, commit)| format!("{gate} @ {}", short(commit)))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        crate::engine::decisions::append(
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

/// What `status` shows of a task besides its row: the lock owner while another process holds
/// the lock, compile-only verify (R59), and the next action (the cleaned-up variant once the
/// worktree and the run store are gone).
pub fn facts(st: &Store, t: &TaskRow) -> Result<TaskFacts> {
    facts_with(st, t, compile_only(t))
}

/// R59 for [`facts`]: `null` when the frozen configuration cannot be read (after `task cleanup`).
pub fn compile_only(t: &TaskRow) -> Option<bool> {
    task::frozen_config(t)
        .ok()
        .map(|c| crate::engine::xpp::compile_only(&c))
}

/// [`facts`] with `compile_only` known — `dashboard` keeps it per task and frozen configuration
/// instead of reading and parsing that configuration on every frame (M16-R2).
pub fn facts_with(st: &Store, t: &TaskRow, compile_only: Option<bool>) -> Result<TaskFacts> {
    // R6: from the recorded owner, never a lock probe; `None`, never an empty label
    let active_owner = driven_owner(st, &t.id)?;
    let next_action = if t.state.is_terminal() && !t.run_store.exists() && !t.worktree.exists() {
        crate::engine::runner::cleaned_up_action(t)
    } else {
        next_action(t)
    };
    Ok(TaskFacts {
        active_owner,
        compile_only,
        next_action,
    })
}

/// M11-E3: what each environment lock's environment holds — every lock, or only the locks
/// `task` deployed to — with its status in words and whether that task's final gate is
/// approved.
pub fn environments(
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

/// The usage rows of a task as `task show` prints them.
pub fn usage_lines(st: &Store, id: &str) -> Result<Vec<views::UsageLine>> {
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

/// Does a live process drive the task? From the recorded lock owner (its PID and creation
/// time), never by taking the lock: a probe could refuse a `toryo run` starting at that moment.
pub fn driven(st: &Store, id: &str) -> Result<bool> {
    Ok(driven_owner(st, id)?.is_some())
}

/// The recorded lock owner, while its process (PID and creation time) is alive — never by
/// taking the lock (R6: a probe could refuse a `toryo run` starting at that moment).
pub fn driven_owner(st: &Store, id: &str) -> Result<Option<String>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::state::{Machine, PipelineEvent as Ev};
    use std::path::PathBuf;

    /// A task row whose machine took `events`.
    fn task_in(events: &[Ev], class: Option<&str>) -> TaskRow {
        let mut machine = Machine::new(3);
        for e in events {
            machine.apply(e.clone()).expect("a valid transition");
        }
        if let Some(c) = class {
            machine.set_reason_class(c);
        }
        TaskRow {
            id: "T-1".into(),
            title: "t".into(),
            state: machine.state(),
            task_sha256: String::new(),
            acceptance_ids: vec!["AC-1".into()],
            run_store: PathBuf::from("runs").join("T-1"),
            config_sha256: String::new(),
            lock_generation: 1,
            fix_cycle: machine.fix_cycle(),
            implementer_index: 0,
            worktree: PathBuf::from("wts").join("T-1"),
            branch: "toryo/T-1".into(),
            base_commit: "b".repeat(40),
            last_reviewed_head: None,
            spent_tokens: 0,
            reserved_tokens: 0,
            run_lock_owner: None,
            run_lock_at: None,
            created_at: String::new(),
            updated_at: String::new(),
            repo: PathBuf::from("repo"),
            task_file: PathBuf::from("repo").join("TASK.md"),
            machine,
            implementers: vec!["p".into()],
            planner: "p".into(),
            reviewer: "p".into(),
            feedback: None,
            handoff: None,
            budget_tokens: None,
        }
    }

    /// R23-34: what `resume` decides from the row alone is testable without a CLI — here the
    /// deploy question of X17/X40: only a machine-caused stop from a running stage with the
    /// class `environment_gate_confirm` waits for the owner's yes.
    #[test]
    fn only_a_confirm_stop_from_a_running_stage_waits_for_a_deploy_yes() {
        let h = "a".repeat(40);
        let to_verify = vec![
            Ev::Start,
            Ev::PlanReady {
                subject: "s".into(),
            },
            Ev::PlanApproved {
                occurrence: 1,
                subject: "s".into(),
            },
            Ev::ImplementationDone,
            Ev::Checkpointed { head: h },
        ];
        let escalated = |class: Option<&str>| {
            let mut ev = to_verify.clone();
            ev.push(Ev::Escalate { reason: "r".into() });
            task_in(&ev, class)
        };
        let confirm = escalated(Some(crate::engine::gates::ENV_CONFIRM));
        assert!(machine_caused_stop(&confirm) && waits_for_a_deploy_yes(&confirm));
        let other = escalated(Some(crate::engine::gates::GATE_UNAVAILABLE));
        assert!(machine_caused_stop(&other) && !waits_for_a_deploy_yes(&other));
        let none = escalated(None);
        assert!(!machine_caused_stop(&none) && !waits_for_a_deploy_yes(&none));
        assert!(!waits_for_a_deploy_yes(&task_in(&to_verify, None)));
        assert_eq!(
            deploy_yes_note("T-1"),
            "T-1: waits for your yes before it deploys — look at it, then `toryo resume T-1`"
        );
    }
}
