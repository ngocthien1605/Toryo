//! Crash recovery (plan §10; R31, R38, R39, R40, R48). Runs only under the task lock
//! (step 0, lock-first): holding it proves the previous owner is gone, so its stage
//! processes died with its Job (`KILL_ON_JOB_CLOSE`, R23).
//!
//! Step 3 (R35, M5): the protected state is compared first; a change to untracked/ignored
//! protected files or Git metadata escalates before anything else. Step 4 (R34, M6):
//! artifact reconciliation — staging leftovers removed, orphans moved to `orphans/`, every
//! registered version re-hashed (`artifact_missing` / `artifact_tampered` → Escalated).
//! Step 5, writing stage: the Implementer turn's HEAD guard (§8) before any reset, snapshot
//! or recovery checkpoint (F4); a failed turn whose work was being kept ends as decided (F8).

use crate::git::short;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde_json::json;

use super::checks::Protected;
use super::protect;
use super::runner::{partial_trailers, writing_stage, FailedTurn, HeadGuard};
use super::snapshot;
use super::state::{PipelineEvent as Ev, PipelineState as St};
use super::store::{self, AttemptEnd, AttemptRow, AttemptStatus, Store, TaskRow};
use super::worktree::Worktree;
use crate::config::{Billing, Effective};
use crate::git::GitCmd;

/// `recovery_policy_violation` prefix of the escalation reason (§11).
pub const POLICY_VIOLATION: &str = "recovery_policy_violation";

/// M12-01 (H1): why `commit`, which carries Toryo's trailers, is **not** a commit Toryo made for
/// this task — `None` when it may be taken. No check here asks who made it; three things must
/// hold: it is `HEAD` (a commit under further commits is never taken), its first parent is the
/// commit the attempt began at (`head_before`: what Toryo recorded — every genuine checkpoint
/// and partial commit of an attempt sits right on it), and its diff against that parent passes
/// the path policy (protected, denied, fence) — the same test a fresh commit gets. The diff is
/// the defence; the other two only keep strangers out early.
pub(super) fn foreign_commit(
    wt: &Worktree,
    protect: &Protected,
    head_before: Option<&str>,
    commit: &str,
) -> Result<Option<String>> {
    if wt.head()? != commit {
        return Ok(Some("it is not HEAD".into()));
    }
    // M13-R5 (X31): a merge commit is never a checkpoint — the history of its second parent
    // would enter the task branch unseen by every review of the first-parent diff
    if wt.has_second_parent(commit)? {
        return Ok(Some("it is a merge commit".into()));
    }
    let Some(parent) = wt.first_parent(commit)? else {
        return Ok(Some("it has no parent".into()));
    };
    if head_before != Some(parent.as_str()) {
        return Ok(Some(format!(
            "its parent {} is not the commit the attempt began at ({})",
            short(&parent),
            head_before.map(short).unwrap_or("not recorded")
        )));
    }
    let bad: Vec<String> = wt
        .changed_between(&parent, commit)?
        .into_iter()
        .map(|c| c.1)
        .filter(|p| protect.violates(p))
        .collect();
    if bad.is_empty() {
        Ok(None)
    } else {
        Ok(Some(format!(
            "it changes {} — which the path policy forbids",
            protect.violation_words(&bad)
        )))
    }
}

/// How a read-only attempt that was `Running` when its owner died ends (M13-R4, X32): when the
/// undo of its write could not be done yet ([`super::runner::UNDO_PENDING`] in `undone`) it
/// ends `Failed` with that reason, so that the next writing attempt finds it and finishes the
/// undo; otherwise as always, `Interrupted`.
fn read_only_end(
    undone: &Option<String>,
    class: Option<String>,
    detail: &str,
) -> (AttemptStatus, AttemptEnd) {
    match undone {
        Some(u) if u.contains(super::runner::UNDO_PENDING) => (
            AttemptStatus::Failed,
            AttemptEnd {
                error_class: class.or_else(|| Some("policy_violation".into())),
                error_detail: Some(u.clone()),
                ..Default::default()
            },
        ),
        _ => (
            AttemptStatus::Interrupted,
            AttemptEnd {
                error_class: class,
                error_detail: Some(detail.into()),
                ..Default::default()
            },
        ),
    }
}

pub struct Recovery<'a> {
    pub store: &'a Store,
    pub g: &'a GitCmd,
    pub protect: &'a Protected,
    /// The task's frozen configuration (billing kind of a killed turn's usage row).
    pub cfg: &'a Effective,
}

impl<'a> Recovery<'a> {
    fn task(&self, id: &str) -> Result<TaskRow> {
        let i = id.to_string();
        self.store.call(move |tx| store::get_task(tx, &i))
    }

    fn finish(
        &self,
        a: &AttemptRow,
        status: AttemptStatus,
        end: AttemptEnd,
        e: Option<Ev>,
        handoff: Option<String>,
    ) -> Result<()> {
        let (aid, tid) = (a.id.clone(), a.task_id.clone());
        // R41: an agent turn killed with its owner reported no usage: record it as `unknown`
        // with a reservation — never as zero
        let reservation = self
            .cfg
            .repo
            .pipeline
            .unknown_turn_reservation_tokens
            .as_ref()
            .and_then(|v| v.as_u64());
        let (stage, sess) = (a.stage.clone(), a.external_session_id.clone());
        let killed = (a.status == AttemptStatus::Running && a.kind == "agent").then(|| {
            let p = a.profile.clone().unwrap_or_default();
            let billing = match self.cfg.profile(&p).map(|x| x.billing) {
                Ok(Billing::Foundry) => "foundry",
                Ok(Billing::Api) => "api",
                _ => "subscription",
            };
            let model = super::usage_report::configured_model(self.cfg, &a.stage, &p);
            (p, billing, model)
        });
        self.store.call(move |tx| {
            if let Some((p, b, model)) = killed {
                if !store::has_usage(tx, &aid)? {
                    store::record_usage_in(
                        tx,
                        &store::UsageIn {
                            task: &tid,
                            attempt: &aid,
                            stage: &stage,
                            profile: &p,
                            model: model.as_deref(),
                            billing: b,
                            session: sess.as_deref(),
                            cumulative: false,
                            killed_included: false,
                            usage: None,
                            weights: Default::default(),
                            reservation,
                        },
                    )?;
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
        t.run_store
            .join("artifacts")
            .join("partial")
            .join(format!("{}-{}", a.stage, a.attempt))
    }

    /// M13-R4 (X32): recovery stops for another reason (a protected change, a missing or
    /// tampered artifact) in a read-only stage. The role's writes are put back first when the
    /// record of the tree before its turn is readable (the same undo as after a turn that ended
    /// normally); when that record is the artifact that is gone, nothing can be put back safely
    /// and the reason names every dirty path with "not put back". `None`: not a read-only stage,
    /// an attempt from before M13, or nothing changed.
    fn undo_before_early_return(
        &self,
        t: &TaskRow,
        atts: &[AttemptRow],
        wt: &Worktree,
    ) -> Result<Option<String>> {
        if !matches!(t.state, St::Planning | St::Reviewing) {
            return Ok(None);
        }
        let Some(a) = atts
            .iter()
            .rev()
            .find(|a| a.status == AttemptStatus::Running)
        else {
            return Ok(None);
        };
        match super::runner::tree_before(self.store, t, a) {
            Ok(Some(before)) => super::runner::undo_read_only_write(wt, t, &a.id, &before),
            Ok(None) => Ok(None),
            Err(e) => {
                let paths: Vec<String> = wt.changed()?.into_iter().map(|c| c.1).collect();
                Ok((!paths.is_empty()).then(|| {
                    format!(
                        "the record of the tree before the read-only turn cannot be read ({e:#}), so nothing could be told apart: {paths:?} — not put back"
                    )
                }))
            }
        }
    }

    /// Reconcile a task after its owner died (or was stopped). Returns what was done.
    pub fn recover(&self, id: &str) -> Result<Vec<String>> {
        let mut notes = Vec::new();
        let t = self.task(id)?;
        let i = id.to_string();
        let atts = self.store.call(move |tx| store::attempts(tx, &i))?;
        let wt = Worktree::new(self.g, &t.worktree);
        // §10 step 3 (R35): protected state first
        if t.state.is_running() {
            let c = protect::check(self.store, &t, self.g, self.protect)?;
            let hard = c.hard();
            if !hard.is_empty() {
                let reason = protect::escalation_reason(&t.id, &hard, c.diff_path.as_deref());
                // M13-R4 (X32): the check has run and recorded what it found; the read-only
                // role's other writes are put back all the same
                let undone = self.undo_before_early_return(&t, &atts, &wt)?;
                let reason = match &undone {
                    Some(u) => format!("{reason}; {u}"),
                    None => reason,
                };
                for a in atts.iter().filter(|a| a.status == AttemptStatus::Running) {
                    let (status, end) =
                        read_only_end(&undone, Some(protect::REASON.into()), "owner process ended");
                    self.finish(a, status, end, None, None)?;
                }
                let (i, r) = (id.to_string(), reason.clone());
                self.store.call(move |tx| {
                    store::transition(tx, &i, Ev::Escalate { reason: r }).map(|_| ())
                })?;
                notes.push(format!("escalated before anything else: {reason}"));
                let (i, n) = (id.to_string(), notes.clone());
                self.store.call(move |tx| {
                    store::event(tx, Some(&i), None, "recovered", &json!({"notes": n}))
                })?;
                return Ok(notes);
            }
        }
        // §10 step 4 (R34)
        if !t.state.is_terminal() {
            let r = super::artifacts::reconcile(self.store, &t)?;
            notes.extend(r.notes);
            if let Some(problem) = r.problem {
                // M13-R4 (X32): the undo also runs here — with the record of the tree when it is
                // readable, and without it the reason names what could not be put back
                let undone = self.undo_before_early_return(&t, &atts, &wt)?;
                let problem = match &undone {
                    Some(u) => format!("{problem}; {u}"),
                    None => problem,
                };
                for a in atts.iter().filter(|a| a.status == AttemptStatus::Running) {
                    let (status, end) = read_only_end(&undone, None, "owner process ended");
                    self.finish(a, status, end, None, None)?;
                }
                if t.state != St::Escalated {
                    let (i, p) = (id.to_string(), problem.clone());
                    self.store.call(move |tx| {
                        store::transition(tx, &i, Ev::Escalate { reason: p }).map(|_| ())
                    })?;
                }
                notes.push(format!("escalated: {problem}"));
                let (i, n) = (id.to_string(), notes.clone());
                self.store.call(move |tx| {
                    store::event(tx, Some(&i), None, "recovered", &json!({"notes": n}))
                })?;
                return Ok(notes);
            }
        }
        match t.state {
            St::Implementing | St::Fixing => {
                let stage = writing_stage(t.fix_cycle);
                if let Some(a) = atts.iter().rev().find(|a| a.stage == stage) {
                    let needs = a.status == AttemptStatus::Running
                        || (a.status == AttemptStatus::Interrupted
                            && a.partial_commit.is_none()
                            && a.partial_snapshot_id.is_none()
                            && !wt.is_clean()?);
                    if needs {
                        self.recover_writing(&t, a, &wt, &mut notes)?;
                    }
                }
                // any other leftover running attempt
                for a in atts
                    .iter()
                    .filter(|a| a.status == AttemptStatus::Running && a.stage != stage)
                {
                    self.finish(
                        a,
                        AttemptStatus::Interrupted,
                        AttemptEnd {
                            error_detail: Some("owner process ended".into()),
                            ..Default::default()
                        },
                        None,
                        None,
                    )?;
                    notes.push(format!("{}: interrupted", a.id));
                }
            }
            St::Planning | St::Reviewing => {
                // M13-06: the turn's `before` state is in the run store — escalate only on what
                // the role changed since then, and put that back (the same code as after a
                // turn that ended normally); what the owner had in the tree is left alone.
                // Before the attempt is closed: an error here leaves it `Running`, and the
                // next `resume` comes back to this place.
                let before = match atts
                    .iter()
                    .rev()
                    .find(|a| a.status == AttemptStatus::Running)
                {
                    Some(a) => super::runner::tree_before(self.store, &t, a)?.map(|b| (a, b)),
                    None => None,
                };
                let saved = before.is_some();
                let undone = match &before {
                    Some((a, before)) => {
                        super::runner::undo_read_only_write(&wt, &t, &a.id, before)?
                    }
                    None => None,
                };
                let mut had = false;
                for a in atts.iter().filter(|a| a.status == AttemptStatus::Running) {
                    had = true;
                    let (status, end) = read_only_end(&undone, None, "owner process ended");
                    self.finish(a, status, end, None, None)?;
                    notes.push(format!("{}: interrupted (read-only stage, re-run; Claude resumes its session, R48)", a.id));
                }
                if saved {
                    if let Some(reason) = undone {
                        let i = id.to_string();
                        self.store.call(move |tx| {
                            store::transition(tx, &i, Ev::Escalate { reason }).map(|_| ())
                        })?;
                        notes.push(
                            "escalated: the read-only role changed the worktree (put back)".into(),
                        );
                    }
                } else {
                    // an attempt from before M13, or one that died before its first turn: a
                    // dirty tree means something wrote into it (read-only roles cannot write)
                    let changed = wt.changed()?;
                    if had && !changed.is_empty() {
                        let paths: Vec<String> = changed.into_iter().map(|c| c.1).collect();
                        let reason = format!("policy_violation: the worktree changed during a read-only stage: {paths:?}");
                        let i = id.to_string();
                        self.store.call(move |tx| {
                            store::transition(tx, &i, Ev::Escalate { reason }).map(|_| ())
                        })?;
                        notes.push("escalated: dirty worktree after a read-only stage".into());
                    }
                }
            }
            _ => {
                // Verifying: the verify run is disposable and re-runs; Checkpointing: the
                // trailer search makes the redo idempotent; waiting states: nothing ran.
                for a in atts.iter().filter(|a| a.status == AttemptStatus::Running) {
                    self.finish(
                        a,
                        AttemptStatus::Interrupted,
                        AttemptEnd {
                            error_detail: Some("owner process ended".into()),
                            ..Default::default()
                        },
                        None,
                        None,
                    )?;
                    notes.push(format!("{}: interrupted (re-runs)", a.id));
                }
            }
        }
        if !notes.is_empty() {
            let (i, n) = (id.to_string(), notes.clone());
            self.store.call(move |tx| {
                store::event(tx, Some(&i), None, "recovered", &json!({"notes": n}))
            })?;
        }
        Ok(notes)
    }

    /// §10 step 5, writing stage. The HEAD guard of the Implementer turn (§8; R12, R25) comes
    /// first (F4): a Git state a soft reset cannot repair — another branch, a detached or
    /// unresolvable HEAD, history rewritten below the base — escalates before anything is
    /// reset, snapshotted or committed (a reset would move another branch, a recovery
    /// checkpoint would land on it). An unfinished operation keeps recovery's own path: no
    /// reset during it, a snapshot, `recovery_policy_violation` (so `resume --discard-partial`
    /// can clear it). A turn that failed and left changes ends `failed`, as its owner had
    /// decided before it died (F8, EC-15).
    fn recover_writing(
        &self,
        t: &TaskRow,
        a: &AttemptRow,
        wt: &Worktree,
        notes: &mut Vec<String>,
    ) -> Result<()> {
        // without the attempt's HEAD, nothing counts as moved (the violations still count)
        let before = match &a.git_head_before {
            Some(h) => h.clone(),
            None => wt.head().unwrap_or_default(),
        };
        let guard = HeadGuard::check_refs(wt, &t.branch, &t.base_commit, &before)?;
        if let HeadGuard::Violation { kind, reason } = &guard {
            return self.git_violation(a, kind, reason, notes);
        }
        let failed = FailedTurn::recorded(self.store, &t.id, &a.id)?;
        // i. trailer search: the crash may have come after the commit, before the DB update.
        // M12-01 (H1): no check may depend on who made a commit. Toryo makes a normal
        // checkpoint only in `Checkpointing`, after this attempt has ended — while it is still
        // running a normal trailer match is an agent's and is undone below like any agent
        // commit; a partial one is Toryo's only when its first parent is a commit Toryo recorded
        // and its diff passes the path policy.
        let found = match wt.find_checkpoint(&t.base_commit, &t.id, &a.stage, a.attempt)? {
            Some(c) if c.partial => {
                match foreign_commit(wt, self.protect, a.git_head_before.as_deref(), &c.sha)? {
                    None => Some(c),
                    Some(why) => {
                        notes.push(format!(
                            "{}: commit {} with Toryo-Partial trailers is not Toryo's ({why}) — undone",
                            a.id,
                            short(&c.sha)
                        ));
                        None
                    }
                }
            }
            _ => None,
        };
        if let Some(c) = found {
            if let (true, Some(f)) = (c.partial, &failed) {
                // the failed turn's work was committed, the attempt not yet ended (F8)
                return self.failed_turn(a, f, &c.sha, None, notes);
            }
            self.finish(
                a,
                AttemptStatus::Interrupted,
                AttemptEnd {
                    partial_commit: Some(c.sha.clone()),
                    ..Default::default()
                },
                None,
                Some(recovery_note(a, Some(&c.sha))),
            )?;
            notes.push(format!(
                "{}: partial checkpoint {} already committed",
                a.id,
                short(&c.sha)
            ));
            return Ok(());
        }
        // HEAD guard: an agent commit on the task branch is undone (its changes stay in the
        // tree) — never during an unfinished operation, which the snapshot path below handles
        if let (HeadGuard::Reset { .. }, None) = (&guard, wt.unfinished_operation()?) {
            wt.soft_reset(&before)?;
            notes.push(format!(
                "{}: the agent had moved HEAD — soft reset to {}",
                a.id,
                short(&before)
            ));
        }
        // ii. clean tree
        if wt.is_clean()? {
            self.finish(
                a,
                AttemptStatus::Interrupted,
                AttemptEnd {
                    error_detail: Some("owner process ended".into()),
                    ..Default::default()
                },
                None,
                Some(recovery_note(a, None)),
            )?;
            notes.push(format!("{}: interrupted, tree clean", a.id));
            return Ok(());
        }
        // iii. dirty: snapshot first (R39), then the unfinished-operation check and the path policy
        let (snap_id, dir) = self.snapshot(t, a, wt)?;
        let changed: Vec<String> = wt.changed()?.into_iter().map(|c| c.1).collect();
        let prot: Vec<String> = changed
            .iter()
            .filter(|p| self.protect.violates(p))
            .cloned()
            .collect();
        let unfinished = wt.unfinished_operation()?;
        if !prot.is_empty() || unfinished.is_some() {
            // M11-R8: a fence violation names its rule
            let paths = self.protect.violation_words(&prot);
            let what = match (&unfinished, prot.is_empty()) {
                (Some(op), true) => format!("an unfinished Git operation ({op})"),
                (Some(op), false) => format!("an unfinished Git operation ({op}) and {paths}"),
                (None, _) => paths,
            };
            let reason = format!(
                "{POLICY_VIOLATION}: the interrupted attempt {} left {what}; snapshot {}",
                a.id,
                dir.display()
            );
            self.finish(
                a,
                AttemptStatus::Interrupted,
                AttemptEnd {
                    partial_snapshot_id: Some(snap_id),
                    error_class: Some(POLICY_VIOLATION.into()),
                    ..Default::default()
                },
                Some(Ev::Escalate { reason }),
                None,
            )?;
            notes.push(format!(
                "{}: {POLICY_VIOLATION} — escalated, nothing committed",
                a.id
            ));
            return Ok(());
        }
        let sha = self.partial_commit(t, a, wt, failed.is_some())?;
        if let Some(f) = &failed {
            // the owner died between recording the failed turn and committing its work
            return self.failed_turn(a, f, &sha, Some(snap_id), notes);
        }
        self.finish(
            a,
            AttemptStatus::Interrupted,
            AttemptEnd {
                partial_commit: Some(sha.clone()),
                partial_snapshot_id: Some(snap_id),
                ..Default::default()
            },
            None,
            Some(recovery_note(a, Some(&sha))),
        )?;
        notes.push(format!(
            "{}: recovery checkpoint {} (Toryo-Partial)",
            a.id,
            short(&sha)
        ));
        Ok(())
    }

    /// F4: the HEAD guard found a Git state recovery cannot repair. The attempt ends
    /// `interrupted` with `policy_violation` and the task escalates with the guard's reason —
    /// nothing was reset, snapshotted or committed, so the human finds the worktree as the
    /// agent left it.
    fn git_violation(
        &self,
        a: &AttemptRow,
        kind: &str,
        reason: &str,
        notes: &mut Vec<String>,
    ) -> Result<()> {
        let (tid, aid) = (a.task_id.clone(), a.id.clone());
        let payload =
            json!({"kind": kind, "action": "escalate", "reason": reason, "during": "recovery"});
        self.store.call(move |tx| {
            store::event(tx, Some(&tid), Some(&aid), "policy_violation", &payload)
        })?;
        self.finish(
            a,
            AttemptStatus::Interrupted,
            AttemptEnd {
                error_class: Some("policy_violation".into()),
                error_detail: Some(reason.to_string()),
                ..Default::default()
            },
            Some(Ev::Escalate {
                reason: reason.to_string(),
            }),
            None,
        )?;
        notes.push(format!("{}: {reason} — escalated", a.id));
        Ok(())
    }

    /// F8 (EC-15): the attempt's turn had failed and left changes, now in the recovery
    /// checkpoint `sha`: the attempt ends as its owner had decided before it died — `failed`
    /// with the turn's class, the hand-over note, and the escalation unless the stage runs
    /// again.
    fn failed_turn(
        &self,
        a: &AttemptRow,
        f: &FailedTurn,
        sha: &str,
        snapshot: Option<String>,
        notes: &mut Vec<String>,
    ) -> Result<()> {
        let escalation = f.escalation(sha);
        self.finish(
            a,
            AttemptStatus::Failed,
            AttemptEnd {
                partial_snapshot_id: snapshot,
                ..f.end(sha)
            },
            escalation.clone().map(|reason| Ev::Escalate { reason }),
            Some(f.note(a, sha)),
        )?;
        notes.push(match escalation {
            Some(why) => format!("{}: {why} — failed, escalated (EC-15)", a.id),
            None => format!(
                "{}: {}; {} — failed, the stage runs again from it (EC-15)",
                a.id,
                f.reason,
                FailedTurn::kept(sha)
            ),
        });
        Ok(())
    }

    fn snapshot(&self, t: &TaskRow, a: &AttemptRow, wt: &Worktree) -> Result<(String, PathBuf)> {
        let root = Self::snapshot_root(t, a);
        let (dir, m) = snapshot::take(wt, self.protect, &root, &t.id, &a.id)?;
        let manifest = std::fs::read(dir.join("manifest.json"))?;
        let rel = dir
            .strip_prefix(&t.run_store)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| dir.display().to_string());
        let (tid, aid, logical, sha, size) = (
            t.id.clone(),
            a.id.clone(),
            format!("partial/{}-{}", a.stage, a.attempt),
            super::task::sha256_hex(&manifest),
            manifest.len() as u64,
        );
        let id = self.store.call(move |tx| {
            store::register_artifact(
                tx,
                &tid,
                Some(&aid),
                &logical,
                "partial_snapshot",
                &rel,
                &sha,
                size,
            )
        })?;
        eprintln!(
            "   snapshot v{} of {}: {} entries → {}",
            m.version,
            a.id,
            m.entries.len(),
            dir.display()
        );
        Ok((id, dir))
    }

    /// A recovery checkpoint of attempt `a` (§8, R31); of a `failed` turn (EC-15), it reads
    /// like the one its owner would have committed.
    fn partial_commit(
        &self,
        t: &TaskRow,
        a: &AttemptRow,
        wt: &Worktree,
        failed: bool,
    ) -> Result<String> {
        let message = if failed {
            FailedTurn::message(t, a)
        } else {
            format!(
                "toryo({}): {} (recovery checkpoint of attempt {})",
                t.id, a.stage, a.attempt
            )
        };
        wt.commit(&message, &partial_trailers(t, a), false)
    }

    fn escalated_writing_attempt(&self, t: &TaskRow) -> Result<AttemptRow> {
        if t.state != St::Escalated
            || !t
                .machine
                .reason()
                .unwrap_or("")
                .starts_with(POLICY_VIOLATION)
        {
            bail!(
                "{}: not waiting on a {POLICY_VIOLATION} (state {:?})",
                t.id,
                t.state
            );
        }
        let stage = writing_stage(t.fix_cycle);
        let (i, s) = (t.id.clone(), stage.clone());
        self.store
            .call(move |tx| store::last_attempt(tx, &i, &s))?
            .with_context(|| format!("no `{stage}` attempt"))
    }

    fn snapshot_dir(&self, t: &TaskRow, a: &AttemptRow) -> Result<PathBuf> {
        let sid = a
            .partial_snapshot_id
            .clone()
            .context("the attempt has no recovery snapshot")?;
        let (_, rel, _) = self.store.call(move |tx| store::artifact(tx, &sid))?;
        Ok(t.run_store.join(rel))
    }

    /// `toryo resume --discard-partial` (R31/R39): only after the snapshot completion check,
    /// and after a **new** snapshot of the worktree as it is now (EC-19: edits made since the
    /// recovery snapshot are kept too), reset to the attempt's `git_head_before` and start
    /// attempt n+1 from a clean tree. The new snapshot becomes the attempt's, so `toryo partial
    /// export` restores what was discarded.
    pub fn discard_partial(&self, id: &str) -> Result<String> {
        let t = self.task(id)?;
        let a = self.escalated_writing_attempt(&t)?;
        let wt = Worktree::new(self.g, &t.worktree);
        let first = self.snapshot_dir(&t, &a)?;
        snapshot::completion_check(&wt, &first)?; // snapshot_incomplete → no reset
        let before = a
            .git_head_before
            .clone()
            .context("the attempt has no git_head_before")?;
        let (snap, dir) = self.snapshot(&t, &a, &wt)?;
        snapshot::completion_check(&wt, &dir)?;
        let (aid, s) = (a.id.clone(), snap);
        self.store.call(move |tx| {
            store::finish_attempt(
                tx,
                &aid,
                AttemptStatus::Interrupted,
                &AttemptEnd {
                    partial_snapshot_id: Some(s),
                    ..Default::default()
                },
            )
        })?;
        wt.hard_reset_and_clean(&before)?;
        let note = format!(
            "\n## Recovery\nThe interrupted attempt {} was discarded after a policy violation (its files are kept in snapshot {}; `toryo partial export` restores them). Start from the plan.\n",
            a.id,
            dir.display()
        );
        let (i, aid, d) = (id.to_string(), a.id.clone(), dir.display().to_string());
        self.store.call(move |tx| {
            store::event(
                tx,
                Some(&i),
                Some(&aid),
                "partial_discarded",
                &json!({"snapshot": d}),
            )?;
            store::set_handoff(tx, &i, Some(&note))?;
            store::transition(tx, &i, Ev::ResumeStage)?;
            Ok(())
        })?;
        Ok(format!(
            "discarded the partial work of {} (reset to {}); snapshot kept in {}",
            a.id,
            short(&before),
            dir.display()
        ))
    }

    /// `toryo resume --keep-partial` (R31): after a hand fix — snapshot again (new version),
    /// re-run the policy; pass → recovery checkpoint.
    pub fn keep_partial(&self, id: &str) -> Result<String> {
        let t = self.task(id)?;
        let a = self.escalated_writing_attempt(&t)?;
        let wt = Worktree::new(self.g, &t.worktree);
        if let Some(op) = wt.unfinished_operation()? {
            bail!("an unfinished Git operation ({op}) is still in the worktree — finish or abort it first");
        }
        // M13-01 (X24), review: the recovery checkpoint goes on the commit the attempt began
        // at. A commit left on top of it (recovery's HEAD guard does not reset during an
        // unfinished operation) is undone into the tree first, so that its changes meet the
        // policy below — who made it is not asked. A HEAD anywhere else: nothing is committed.
        let before = a
            .git_head_before
            .clone()
            .context("the attempt has no git_head_before")?;
        let head = wt.head()?;
        if head != before {
            let on_branch = wt.head_ref()? == Some(format!("refs/heads/{}", t.branch));
            if !(on_branch && wt.contains(&before)?) {
                bail!(
                    "HEAD is at {}, which is not the task branch {} on top of the commit the attempt began at ({}) — put the branch back by hand first, or `--discard-partial`",
                    short(&head),
                    t.branch,
                    short(&before)
                );
            }
            wt.soft_reset(&before)?;
            eprintln!(
                "note: commits {}..{} made during the attempt were undone; their changes stay in the tree and are checked",
                short(&before),
                short(&head)
            );
        }
        let prot: Vec<String> = wt
            .changed()?
            .into_iter()
            .map(|c| c.1)
            .filter(|p| self.protect.violates(p))
            .collect();
        if !prot.is_empty() {
            bail!("protected paths still changed: {prot:?} — restore them first (or `--discard-partial`)");
        }
        let (snap, _) = self.snapshot(&t, &a, &wt)?;
        let sha = if wt.is_clean()? {
            None
        } else {
            Some(self.partial_commit(&t, &a, &wt, false)?)
        };
        let note = recovery_note(&a, sha.as_deref());
        let (i, aid, s2) = (id.to_string(), a.id.clone(), sha.clone());
        self.store.call(move |tx| {
            store::finish_attempt(
                tx,
                &aid,
                AttemptStatus::Interrupted,
                &AttemptEnd {
                    partial_commit: s2,
                    partial_snapshot_id: Some(snap),
                    ..Default::default()
                },
            )?;
            store::set_handoff(tx, &i, Some(&note))?;
            store::transition(tx, &i, Ev::ResumeStage)?;
            Ok(())
        })?;
        Ok(match sha {
            Some(s) => format!(
                "kept the partial work of {} as recovery checkpoint {}",
                a.id,
                short(&s)
            ),
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
