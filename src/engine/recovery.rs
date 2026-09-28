//! Crash recovery (plan §10; R31, R38, R39, R40, R48). Runs only under the task lock
//! (step 0, lock-first): holding it proves the previous owner is gone, so its stage
//! processes died with its Job (`KILL_ON_JOB_CLOSE`, R23).
//!
//! Not yet here: the protected-state comparison (step 3, R35 → M5) and artifact
//! reconciliation (step 4, R34 → M6).

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde_json::json;

use super::checks::Protected;
use super::runner::writing_stage;
use super::snapshot;
use super::state::{PipelineEvent as Ev, PipelineState as St};
use super::store::{self, AttemptEnd, AttemptRow, AttemptStatus, Store, TaskRow};
use super::worktree::{Worktree, T_ATTEMPT, T_IMPLEMENTER, T_PARTIAL, T_STAGE, T_TASK};
use crate::config::{Billing, Effective};
use crate::git::GitCmd;

/// `recovery_policy_violation` prefix of the escalation reason (§11).
pub const POLICY_VIOLATION: &str = "recovery_policy_violation";

pub struct Recovery<'a> {
    pub store: &'a Store,
    pub g: &'a GitCmd,
    pub protect: &'a Protected,
    /// The task's frozen configuration (billing kind of a killed turn's usage row).
    pub cfg: &'a Effective,
}

fn short(s: &str) -> &str {
    &s[..12.min(s.len())]
}

impl<'a> Recovery<'a> {
    fn task(&self, id: &str) -> Result<TaskRow> {
        let i = id.to_string();
        self.store.call(move |tx| store::get_task(tx, &i))
    }

    fn finish(&self, a: &AttemptRow, status: AttemptStatus, end: AttemptEnd, e: Option<Ev>, handoff: Option<String>) -> Result<()> {
        let (aid, tid) = (a.id.clone(), a.task_id.clone());
        // R41: an agent turn killed with its owner reported no usage: record it as `unknown`
        // with a reservation — never as zero
        let killed = (a.status == AttemptStatus::Running && a.kind == "agent").then(|| {
            let p = a.profile.clone().unwrap_or_default();
            let billing = match self.cfg.profile(&p).map(|x| x.billing) {
                Ok(Billing::Foundry) => "foundry",
                Ok(Billing::Api) => "api",
                _ => "subscription",
            };
            (p, billing)
        });
        self.store.call(move |tx| {
            if let Some((p, b)) = killed {
                if !store::has_usage(tx, &aid)? {
                    store::record_usage(tx, &tid, &aid, &p, b, None)?;
                }
            }
            store::finish_attempt(tx, &aid, status, &end)?;
            if let Some(h) = handoff {
                store::set_handoff(tx, &tid, Some(&h))?;
            }
            if let Some(e) = e {
                store::transition(tx, &tid, e)?;
            }
            Ok(())
        })
    }

    /// Snapshot directory of one attempt.
    pub fn snapshot_root(t: &TaskRow, a: &AttemptRow) -> PathBuf {
        t.run_store.join("artifacts").join("partial").join(format!("{}-{}", a.stage, a.attempt))
    }

    /// Reconcile a task after its owner died (or was stopped). Returns what was done.
    pub fn recover(&self, id: &str) -> Result<Vec<String>> {
        let mut notes = Vec::new();
        let t = self.task(id)?;
        let i = id.to_string();
        let atts = self.store.call(move |tx| store::attempts(tx, &i))?;
        let wt = Worktree::new(self.g, &t.worktree);
        match t.state {
            St::Implementing | St::Fixing => {
                let stage = writing_stage(t.fix_cycle);
                if let Some(a) = atts.iter().rev().find(|a| a.stage == stage) {
                    let needs = a.status == AttemptStatus::Running
                        || (a.status == AttemptStatus::Interrupted && a.partial_commit.is_none() && a.partial_snapshot_id.is_none() && !wt.is_clean()?);
                    if needs {
                        self.recover_writing(&t, a, &wt, &mut notes)?;
                    }
                }
                // any other leftover running attempt
                for a in atts.iter().filter(|a| a.status == AttemptStatus::Running && a.stage != stage) {
                    self.finish(a, AttemptStatus::Interrupted, AttemptEnd { error_detail: Some("owner process ended".into()), ..Default::default() }, None, None)?;
                    notes.push(format!("{}: interrupted", a.id));
                }
            }
            St::Planning | St::Reviewing => {
                let mut had = false;
                for a in atts.iter().filter(|a| a.status == AttemptStatus::Running) {
                    had = true;
                    self.finish(a, AttemptStatus::Interrupted, AttemptEnd { error_detail: Some("owner process ended".into()), ..Default::default() }, None, None)?;
                    notes.push(format!("{}: interrupted (read-only stage, re-run; Claude resumes its session, R48)", a.id));
                }
                // read-only roles cannot write: a dirty tree means something else wrote into it
                let changed = wt.changed()?;
                if had && !changed.is_empty() {
                    let paths: Vec<String> = changed.into_iter().map(|c| c.1).collect();
                    let reason = format!("policy_violation: the worktree changed during a read-only stage: {paths:?}");
                    let i = id.to_string();
                    self.store.call(move |tx| store::transition(tx, &i, Ev::Escalate { reason }).map(|_| ()))?;
                    notes.push("escalated: dirty worktree after a read-only stage".into());
                }
            }
            _ => {
                // Verifying: the verify run is disposable and re-runs; Checkpointing: the
                // trailer search makes the redo idempotent; waiting states: nothing ran.
                for a in atts.iter().filter(|a| a.status == AttemptStatus::Running) {
                    self.finish(a, AttemptStatus::Interrupted, AttemptEnd { error_detail: Some("owner process ended".into()), ..Default::default() }, None, None)?;
                    notes.push(format!("{}: interrupted (re-runs)", a.id));
                }
            }
        }
        if !notes.is_empty() {
            let (i, n) = (id.to_string(), notes.clone());
            self.store.call(move |tx| store::event(tx, Some(&i), None, "recovered", &json!({"notes": n})))?;
        }
        Ok(notes)
    }

    /// §10 step 5, writing stage.
    fn recover_writing(&self, t: &TaskRow, a: &AttemptRow, wt: &Worktree, notes: &mut Vec<String>) -> Result<()> {
        // i. trailer search: the crash may have come after the commit, before the DB update
        if let Some(c) = wt.find_checkpoint(&t.base_commit, &t.id, &a.stage, a.attempt)? {
            if c.partial {
                self.finish(a, AttemptStatus::Interrupted, AttemptEnd { partial_commit: Some(c.sha.clone()), ..Default::default() }, None, Some(recovery_note(a, Some(&c.sha))))?;
                notes.push(format!("{}: partial checkpoint {} already committed", a.id, short(&c.sha)));
            } else {
                self.finish(
                    a,
                    AttemptStatus::Succeeded,
                    AttemptEnd { head_after: Some(c.sha.clone()), checkpoint_kind: Some(if c.no_change { "no_change" } else { "normal" }.into()), ..Default::default() },
                    Some(Ev::ImplementationDone),
                    None,
                )?;
                notes.push(format!("{}: checkpoint {} found by trailer — succeeded", a.id, short(&c.sha)));
            }
            return Ok(());
        }
        // HEAD guard: an agent commit is undone (its changes stay in the tree)
        if let Some(before) = &a.git_head_before {
            if wt.head()? != *before && wt.contains(before)? {
                wt.soft_reset(before)?;
                notes.push(format!("{}: the agent had moved HEAD — soft reset to {}", a.id, short(before)));
            }
        }
        // ii. clean tree
        if wt.is_clean()? {
            self.finish(a, AttemptStatus::Interrupted, AttemptEnd { error_detail: Some("owner process ended".into()), ..Default::default() }, None, Some(recovery_note(a, None)))?;
            notes.push(format!("{}: interrupted, tree clean", a.id));
            return Ok(());
        }
        // iii. dirty: snapshot first (R39), then the unfinished-operation check and the path policy
        let (snap_id, dir) = self.snapshot(t, a, wt)?;
        let changed: Vec<String> = wt.changed()?.into_iter().map(|c| c.1).collect();
        let prot: Vec<String> = changed.iter().filter(|p| self.protect.is_protected(p)).cloned().collect();
        let unfinished = wt.unfinished_operation()?;
        if !prot.is_empty() || unfinished.is_some() {
            let what = match (&unfinished, prot.is_empty()) {
                (Some(op), true) => format!("an unfinished Git operation ({op})"),
                (Some(op), false) => format!("an unfinished Git operation ({op}) and protected paths {prot:?}"),
                (None, _) => format!("protected paths {prot:?}"),
            };
            let reason = format!("{POLICY_VIOLATION}: the interrupted attempt {} left {what}; snapshot {}", a.id, dir.display());
            self.finish(
                a,
                AttemptStatus::Interrupted,
                AttemptEnd { partial_snapshot_id: Some(snap_id), error_class: Some(POLICY_VIOLATION.into()), ..Default::default() },
                Some(Ev::Escalate { reason }),
                None,
            )?;
            notes.push(format!("{}: {POLICY_VIOLATION} — escalated, nothing committed", a.id));
            return Ok(());
        }
        let sha = self.partial_commit(t, a, wt)?;
        self.finish(
            a,
            AttemptStatus::Interrupted,
            AttemptEnd { partial_commit: Some(sha.clone()), partial_snapshot_id: Some(snap_id), ..Default::default() },
            None,
            Some(recovery_note(a, Some(&sha))),
        )?;
        notes.push(format!("{}: recovery checkpoint {} (Toryo-Partial)", a.id, short(&sha)));
        Ok(())
    }

    fn snapshot(&self, t: &TaskRow, a: &AttemptRow, wt: &Worktree) -> Result<(String, PathBuf)> {
        let root = Self::snapshot_root(t, a);
        let (dir, m) = snapshot::take(wt, self.protect, &root, &t.id, &a.id)?;
        let manifest = std::fs::read(dir.join("manifest.json"))?;
        let rel = dir.strip_prefix(&t.run_store).map(|p| p.display().to_string()).unwrap_or_else(|_| dir.display().to_string());
        let (tid, aid, logical, sha, size) = (t.id.clone(), a.id.clone(), format!("partial/{}-{}", a.stage, a.attempt), super::task::sha256_hex(&manifest), manifest.len() as u64);
        let id = self.store.call(move |tx| store::register_artifact(tx, &tid, Some(&aid), &logical, "partial_snapshot", &rel, &sha, size))?;
        eprintln!("   snapshot v{} of {}: {} entries → {}", m.version, a.id, m.entries.len(), dir.display());
        Ok((id, dir))
    }

    fn partial_commit(&self, t: &TaskRow, a: &AttemptRow, wt: &Worktree) -> Result<String> {
        wt.commit(
            &format!("toryo({}): {} (recovery checkpoint of attempt {})", t.id, a.stage, a.attempt),
            &[
                (T_TASK, t.id.clone()),
                (T_STAGE, a.stage.clone()),
                (T_ATTEMPT, a.attempt.to_string()),
                (T_IMPLEMENTER, a.profile.clone().unwrap_or_default()),
                (T_PARTIAL, "true".into()),
            ],
            false,
        )
    }

    fn escalated_writing_attempt(&self, t: &TaskRow) -> Result<AttemptRow> {
        if t.state != St::Escalated || !t.machine.reason().unwrap_or("").starts_with(POLICY_VIOLATION) {
            bail!("{}: not waiting on a {POLICY_VIOLATION} (state {:?})", t.id, t.state);
        }
        let stage = writing_stage(t.fix_cycle);
        let (i, s) = (t.id.clone(), stage.clone());
        self.store
            .call(move |tx| store::last_attempt(tx, &i, &s))?
            .with_context(|| format!("no `{stage}` attempt"))
    }

    fn snapshot_dir(&self, t: &TaskRow, a: &AttemptRow) -> Result<PathBuf> {
        let sid = a.partial_snapshot_id.clone().context("the attempt has no recovery snapshot")?;
        let (_, rel, _) = self.store.call(move |tx| store::artifact(tx, &sid))?;
        Ok(t.run_store.join(rel))
    }

    /// `toryo resume --discard-partial` (R31/R39): only after the snapshot completion check,
    /// reset to the attempt's `git_head_before` and start attempt n+1 from a clean tree.
    pub fn discard_partial(&self, id: &str) -> Result<String> {
        let t = self.task(id)?;
        let a = self.escalated_writing_attempt(&t)?;
        let wt = Worktree::new(self.g, &t.worktree);
        let dir = self.snapshot_dir(&t, &a)?;
        snapshot::completion_check(&wt, &dir)?; // snapshot_incomplete → no reset
        let before = a.git_head_before.clone().context("the attempt has no git_head_before")?;
        wt.hard_reset_and_clean(&before)?;
        let note = format!(
            "\n## Recovery\nThe interrupted attempt {} was discarded after a policy violation (its files are kept in snapshot {}; `toryo partial export` restores them). Start from the plan.\n",
            a.id,
            dir.display()
        );
        let (i, aid, d) = (id.to_string(), a.id.clone(), dir.display().to_string());
        self.store.call(move |tx| {
            store::event(tx, Some(&i), Some(&aid), "partial_discarded", &json!({"snapshot": d}))?;
            store::set_handoff(tx, &i, Some(&note))?;
            store::transition(tx, &i, Ev::ResumeStage)?;
            Ok(())
        })?;
        Ok(format!("discarded the partial work of {} (reset to {}); snapshot kept in {}", a.id, short(&before), dir.display()))
    }

    /// `toryo resume --keep-partial` (R31): after a hand fix — snapshot again (new version),
    /// re-run the policy; pass → recovery checkpoint.
    pub fn keep_partial(&self, id: &str) -> Result<String> {
        let t = self.task(id)?;
        let a = self.escalated_writing_attempt(&t)?;
        let wt = Worktree::new(self.g, &t.worktree);
        let prot: Vec<String> = wt.changed()?.into_iter().map(|c| c.1).filter(|p| self.protect.is_protected(p)).collect();
        if let Some(op) = wt.unfinished_operation()? {
            bail!("an unfinished Git operation ({op}) is still in the worktree — finish or abort it first");
        }
        if !prot.is_empty() {
            bail!("protected paths still changed: {prot:?} — restore them first (or `--discard-partial`)");
        }
        let (snap, _) = self.snapshot(&t, &a, &wt)?;
        let sha = if wt.is_clean()? { None } else { Some(self.partial_commit(&t, &a, &wt)?) };
        let note = recovery_note(&a, sha.as_deref());
        let (i, aid, s2) = (id.to_string(), a.id.clone(), sha.clone());
        self.store.call(move |tx| {
            store::finish_attempt(tx, &aid, AttemptStatus::Interrupted, &AttemptEnd { partial_commit: s2, partial_snapshot_id: Some(snap), ..Default::default() })?;
            store::set_handoff(tx, &i, Some(&note))?;
            store::transition(tx, &i, Ev::ResumeStage)?;
            Ok(())
        })?;
        Ok(match sha {
            Some(s) => format!("kept the partial work of {} as recovery checkpoint {}", a.id, short(&s)),
            None => format!("{}: nothing left to keep; the stage runs again", a.id),
        })
    }
}

/// §10 step iv: the note for attempt n+1 (R48: Claude resumes its session with it, Codex
/// starts a fresh session with it).
fn recovery_note(a: &AttemptRow, partial: Option<&str>) -> String {
    match partial {
        Some(sha) => format!(
            "\n## Recovery\nThe previous attempt ({} by `{}`) was interrupted. Its uncommitted work was saved as the recovery checkpoint {} (trailer `Toryo-Partial: true`) — the worktree starts from it. Check that work, keep what is correct, fix or complete the rest, and finish the task.\n",
            a.id,
            a.profile.as_deref().unwrap_or("?"),
            short(sha)
        ),
        None => format!(
            "\n## Recovery\nThe previous attempt ({} by `{}`) was interrupted before it changed anything. Do the task from the plan.\n",
            a.id,
            a.profile.as_deref().unwrap_or("?")
        ),
    }
}
