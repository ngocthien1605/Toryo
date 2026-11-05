//! M17 Part B — best-of-N implementer candidates (X55–X62): the fan-out of a task's first
//! Implementing stage, one phase per call of [`Engine::fan_out`] (each re-entrant, so a stop, a
//! crash or an escalation resumes where it was, and the run loop checks a stop between phases):
//!
//! - `creating`: candidate `k` gets `<worktrees_root>/<ID>.c<k>` on a new branch
//!   `toryo-candidate/<ID>/c<k>` at the base commit — recorded first; a failure or a stop takes
//!   back what was made (X28 style); an unrecorded folder or branch is a machine stop
//!   (`candidate_unavailable`, refused by name, never deleted);
//! - `turns`: the candidates' implementer turns run at the same time (threads of this process,
//!   under the one task lock, X61), each in its worktree with its own session, TEMP and context;
//!   each one's result goes through the single turn's post-turn path there (HEAD guard, protected
//!   state, path policy, Toryo's checkpoint commit with the candidate's trailers);
//! - `verify`: each checkpointed candidate's check gates (red-green included, no environment
//!   gate), one after another under the machine verify lock, in `<verify_root>/<task>/c<k>`;
//! - `select`: one passing candidate wins (`only`); two or more go to one read-only compare turn
//!   of the reviewer seat, then the score (`score`); none passing: the best by score enters Fixing;
//!   none checkpointed: the single path, once;
//! - `selected`: promotion — `toryo/<ID>` is moved to exactly the winner's checkpoint, and the
//!   normal flow goes on from there (`promoted`).
//!
//! The pure parts (names, score, the compare answer, admission) are `engine/candidates.rs`.

use super::*;
use crate::engine::candidates as cand;
use crate::engine::store::{CandidateRow, FanoutRow};
use std::sync::Mutex;

/// The phases of a fan-out (`fanouts.phase`).
pub mod phase {
    pub const CREATING: &str = "creating";
    pub const TURNS: &str = "turns";
    pub const VERIFY: &str = "verify";
    pub const SELECT: &str = "select";
    pub const SELECTED: &str = "selected";
    pub const PROMOTED: &str = "promoted";
    pub const SINGLE: &str = "single";
}

/// M17-B4 (X58): a candidate's folder or branch exists and the store did not record it.
pub const CANDIDATE_UNAVAILABLE: &str =
    crate::engine::state::EscalationClass::CandidateUnavailable.as_str();

/// The trailer naming the candidate of a checkpoint (`Toryo-Candidate: c2`).
pub const T_CANDIDATE: &str = "Toryo-Candidate";

/// M17-B1 (AC-12): the candidates' profiles that may still run a turn — while the task's first
/// Implementing stage has not fanned out yet or its candidates' turns are not over. For the R51
/// evidence check of a drive.
pub fn pending_candidate_profiles(
    store: &Store,
    t: &TaskRow,
    cfg: &Effective,
) -> Result<Vec<String>> {
    let Some(profiles) = cand::configured(cfg) else {
        return Ok(Vec::new());
    };
    let early = matches!(
        t.state,
        St::New
            | St::Planning
            | St::WaitingPlanApproval
            | St::Implementing
            | St::Escalated
            | St::Paused
    );
    if t.fix_cycle != 0 || !early {
        return Ok(Vec::new());
    }
    let i = t.id.clone();
    let fan = store.read(move |tx| store::fanout(tx, &i))?;
    Ok(match fan {
        None => profiles,
        Some(f) if f.phase == phase::CREATING || f.phase == phase::TURNS => profiles,
        Some(_) => Vec::new(),
    })
}

/// How a candidate's turn ended, for the fan-out.
enum TurnEnd {
    /// The candidate is finished (`outcome` recorded).
    Finished,
    /// Cut short (a stop, the budget, a sibling's escalation): the turn runs again.
    Pending,
    /// The task escalated.
    Escalated,
}

/// A candidate turn ready to start.
struct Prepared {
    c: CandidateRow,
    profile: String,
    model: Option<String>,
    attempt: AttemptRow,
    open: OpenSession,
    input: TurnInput,
    cancel: Arc<AtomicBool>,
    pid: Arc<Mutex<Option<u32>>>,
    hooks: TurnHooks,
}

/// R41 for concurrent turns (X57): the provisional usage of every running candidate turn counts
/// against what is left of the budget; at the limit every candidate turn is cut.
struct SharedBudget {
    left: Option<u64>,
    used: Mutex<BTreeMap<u32, u64>>,
    hit: AtomicBool,
    cancels: Mutex<Vec<Arc<AtomicBool>>>,
}

impl<'a> Engine<'a> {
    /// M17-B4: the fan-out of the first Implementing stage, one phase per call. `None`: no
    /// fan-out — no candidates configured, a fix cycle, or the fan-out ended on the single path —
    /// and the implement stage runs as before (AC-13).
    pub(super) fn fan_out(&mut self, t: &TaskRow) -> Result<Option<Flow>> {
        if t.state != St::Implementing || t.fix_cycle != 0 {
            return Ok(None);
        }
        let Some(profiles) = cand::configured(&self.cfg) else {
            return Ok(None);
        };
        let i = self.id.clone();
        let Some(fan) = self.store.call(move |tx| store::fanout(tx, &i))? else {
            return self.start_fanout(t, &profiles);
        };
        match fan.phase.as_str() {
            phase::SINGLE | phase::PROMOTED => Ok(None),
            phase::CREATING => self.create_candidates(t, &fan).map(Some),
            phase::TURNS => self.candidate_turns(t).map(Some),
            phase::VERIFY => self.candidates_verify(t).map(Some),
            phase::SELECT => self.select_winner(t).map(Some),
            phase::SELECTED => self.promote(t, &fan).map(Some),
            other => bail!("{}: the fan-out is in an unknown phase `{other}`", t.id),
        }
    }

    fn candidate_rows(&self) -> Result<Vec<CandidateRow>> {
        let i = self.id.clone();
        self.store.call(move |tx| store::candidates(tx, &i))
    }

    fn worktrees_root(&self) -> Result<PathBuf> {
        Ok(PathBuf::from(expand_vars(
            &self.cfg.repo.project.worktrees_root,
        )?))
    }

    fn branch_exists(&self, t: &TaskRow, branch: &str) -> Result<bool> {
        Ok(self
            .g
            .output(
                &crate::git::Loc::Discover(t.repo.clone()),
                true,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}"),
                ],
            )?
            .status
            .success())
    }

    /// M17-B11 (AC-24, X57): the reservation estimate of one candidate turn — the configured
    /// `unknown_turn_reservation_tokens`, else the largest exact implement turn of the task,
    /// else the default reservation.
    fn candidate_estimate(&self, t: &TaskRow) -> Result<i64> {
        if let Some(n) = self
            .cfg
            .repo
            .pipeline
            .unknown_turn_reservation_tokens
            .as_ref()
            .and_then(|v| v.as_u64())
        {
            return Ok(n as i64);
        }
        let i = t.id.clone();
        let max: Option<i64> = self.store.call(move |tx| {
            Ok(tx.query_row(
                "SELECT MAX(u.budget_tokens) FROM usage u JOIN stage_attempts a ON a.id = u.stage_attempt_id WHERE u.task_id = ?1 AND a.stage LIKE 'implement%' AND u.accounting = 'exact'",
                [&i],
                |r| r.get::<_, Option<i64>>(0),
            )?)
        })?;
        Ok(max
            .filter(|m| *m > 0)
            .unwrap_or(store::DEFAULT_UNKNOWN_RESERVATION))
    }

    /// The fan-out begins: the budget's admission (fewer than two: the single path, with a
    /// note), the check for leftovers, then every candidate is recorded — before its worktree
    /// and branch are made (AC-14).
    fn start_fanout(&self, t: &TaskRow, profiles: &[String]) -> Result<Option<Flow>> {
        let estimate = self.candidate_estimate(t)?;
        let admitted = cand::admitted(
            profiles.len(),
            t.budget_tokens,
            t.spent_tokens + t.reserved_tokens,
            estimate,
        );
        let i = self.id.clone();
        if admitted < crate::config::MIN_CANDIDATES {
            let note = format!(
                "best-of-N: the budget admits {admitted} of {} candidate turns (~{estimate} tokens each) — the single implementer path runs",
                profiles.len()
            );
            crate::errln!("note: {note}");
            self.store
                .call(move |tx| store::insert_fanout(tx, &i, 0, phase::SINGLE, Some(&note)))?;
            return Ok(None);
        }
        let n = admitted.min(profiles.len());
        let note = (n < profiles.len()).then(|| {
            format!(
                "best-of-N: the budget admits {n} of {} candidate turns (~{estimate} tokens each) — c1 … c{n} run",
                profiles.len()
            )
        });
        if let Some(n) = &note {
            crate::errln!("note: {n}");
        }
        let root = self.worktrees_root()?;
        let mut found = Vec::new();
        for k in 1..=n as u32 {
            let path = cand::worktree_path(&root, &t.id, k);
            if std::fs::symlink_metadata(&path).is_ok() {
                found.push(format!("the folder {}", path.display()));
            }
            let branch = cand::branch(&t.id, k);
            if self.branch_exists(t, &branch)? {
                found.push(format!("the branch {branch}"));
            }
        }
        if !found.is_empty() {
            let reason = format!(
                "{CANDIDATE_UNAVAILABLE}: {} already exist{} and this task did not make {} — Toryo never deletes what it did not make: remove {} by hand (`git worktree remove …`, `git branch -D …` in {}), then `toryo resume {}`",
                found.join(" and "),
                if found.len() == 1 { "s" } else { "" },
                if found.len() == 1 { "it" } else { "them" },
                if found.len() == 1 { "it" } else { "them" },
                t.repo.display(),
                t.id
            );
            self.escalate(CANDIDATE_UNAVAILABLE, reason)?;
            return Ok(Some(Flow::Next));
        }
        let rows: Vec<(u32, String, PathBuf, String)> = profiles[..n]
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let k = i as u32 + 1;
                (
                    k,
                    p.clone(),
                    cand::worktree_path(&root, &t.id, k),
                    cand::branch(&t.id, k),
                )
            })
            .collect();
        self.store.call(move |tx| {
            store::insert_fanout(tx, &i, n as u32, phase::CREATING, note.as_deref())?;
            for (k, p, w, b) in &rows {
                store::insert_candidate(tx, &i, *k, p, w, b)?;
            }
            Ok(())
        })?;
        Ok(Some(Flow::Next))
    }

    /// M17-B4 (AC-14): make each recorded candidate's worktree and branch at the base commit. A
    /// failure, or a stop request (Ctrl+C), takes back every one made and the record, so the next
    /// `resume` starts the fan-out again; a crash leaves the record, and what is at a recorded place
    /// that was not marked made is this task's and is taken back before it is made again.
    fn create_candidates(&self, t: &TaskRow, fan: &FanoutRow) -> Result<Flow> {
        let rows = self.candidate_rows()?;
        let made = (|| -> Result<bool> {
            for c in rows.iter().filter(|c| !c.created) {
                self.take_back_candidate(t, c)?;
                if self.lock.stop_requested()? {
                    return Ok(false);
                }
                if let Some(d) = c.worktree.parent() {
                    std::fs::create_dir_all(d).with_context(|| {
                        format!("creating project.worktrees_root {}", d.display())
                    })?;
                }
                let fwd = c.worktree.display().to_string().replace('\\', "/");
                self.g.run(
                    &crate::git::Loc::Discover(t.repo.clone()),
                    false,
                    &[
                        "worktree",
                        "add",
                        "-q",
                        "-b",
                        &c.branch,
                        &fwd,
                        &t.base_commit,
                    ],
                )?;
                let (i, k) = (self.id.clone(), c.k);
                self.store
                    .call(move |tx| store::set_candidate_created(tx, &i, k, true))?;
                crash_point("fanout:after_worktree");
                crate::engine::lock::interrupt_point("fanout:after_worktree");
                if std::env::var("TORYO_TEST_FAIL_AT").is_ok_and(|v| v == "fanout:after_worktree") {
                    bail!("TORYO_TEST_FAIL_AT=fanout:after_worktree: a worktree step failed");
                }
                if self.lock.stop_requested()? {
                    return Ok(false);
                }
            }
            Ok(true)
        })();
        match made {
            Ok(true) => {}
            Ok(false) => {
                self.take_back_fanout(t)?;
                self.lock.handle_stop("stopped")?;
                crate::errln!("■ stop request handled during the creation of the candidates' worktrees: what was made is taken back; `toryo resume {}` starts the fan-out again", t.id);
                return Ok(Flow::Stopped);
            }
            Err(e) => {
                if let Err(x) = self.take_back_fanout(t) {
                    crate::errln!("WARNING: taking the candidates back failed too: {x:#}");
                }
                return Err(e);
            }
        }
        // each candidate's own protected baseline, before any agent runs in it (R35)
        let rows = self.candidate_rows()?;
        for c in &rows {
            let scan = protect::scan(&self.g, &c.worktree, &self.protect)?;
            let m = protect::manifest_of(scan, &format!("candidate {} made", cand::id(c.k)));
            self.put_task(
                t,
                &format!("protected#{}", cand::id(c.k)),
                "json",
                &serde_json::to_vec_pretty(&m)?,
            )?;
        }
        let i = self.id.clone();
        self.store
            .call(move |tx| store::set_fanout_phase(tx, &i, phase::TURNS))?;
        self.rec.emit(EngineEvent::CandidatesStarted {
            n: fan.n,
            profiles: rows.iter().map(|c| c.profile.clone()).collect(),
        });
        Ok(Flow::Next)
    }

    /// An artifact of the task with no attempt (a candidate's protected baseline).
    fn put_task(&self, t: &TaskRow, logical: &str, ext: &str, bytes: &[u8]) -> Result<()> {
        crate::engine::artifacts::put(self.store, t, None, logical, ext, logical, bytes, None)?;
        Ok(())
    }

    /// What is at the recorded place of candidate `c` that was not marked made: this task's
    /// (it was recorded before it was made) — removed, the branch only while it is at the base.
    fn take_back_candidate(&self, t: &TaskRow, c: &CandidateRow) -> Result<()> {
        let repo = crate::git::Loc::Discover(t.repo.clone());
        if std::fs::symlink_metadata(&c.worktree).is_ok() {
            let fwd = c.worktree.display().to_string().replace('\\', "/");
            if self
                .g
                .run(&repo, false, &["worktree", "remove", "--force", &fwd])
                .is_err()
            {
                std::fs::remove_dir_all(&c.worktree)
                    .with_context(|| format!("removing {}", c.worktree.display()))?;
                let _ = self.g.run(&repo, false, &["worktree", "prune"]);
            }
            crate::errln!("removed: the worktree {}", c.worktree.display());
        }
        if self.branch_exists(t, &c.branch)? {
            let at = self.g.run_str(
                &repo,
                true,
                &["rev-parse", &format!("refs/heads/{}", c.branch)],
            )?;
            if at == t.base_commit {
                self.g.run(&repo, false, &["branch", "-D", &c.branch])?;
                crate::errln!("removed: the branch {}", c.branch);
            } else {
                bail!(
                    "the branch {} is at {}, not at the base it was made at — it is not taken back",
                    c.branch,
                    short(&at)
                );
            }
        }
        Ok(())
    }

    /// X28 for a fan-out that did not get past its creation: every candidate's worktree and
    /// branch and the record are taken back.
    fn take_back_fanout(&self, t: &TaskRow) -> Result<()> {
        for c in self.candidate_rows()? {
            self.take_back_candidate(t, &c)?;
        }
        let i = self.id.clone();
        self.store.call(move |tx| store::delete_fanout(tx, &i))
    }

    // ── turns (B5, B6) ─────────────────────────────────────────────────────────────────────

    /// M17-B5 (AC-15): the turns of every candidate that has not finished, at the same time, then
    /// each through the post-turn path as it ends (AC-16, AC-17). A protected change, a tampered
    /// artifact or a recovery policy violation in one candidate escalates the task and ends the
    /// other turns; a stop request or Ctrl+C ends every turn; the live budget interrupt counts the
    /// turns together and pauses the task.
    fn candidate_turns(&self, t: &TaskRow) -> Result<Flow> {
        let rows = self.candidate_rows()?;
        let pending: Vec<CandidateRow> = rows
            .iter()
            .filter(|c| c.state != "finished")
            .cloned()
            .collect();
        // a turn that passed its checks before a crash: its checkpoint (R40, found by trailers)
        for c in pending.iter().filter(|c| c.state == "turn_done") {
            let i = self.id.clone();
            let stage = cand::stage(c.k);
            let a = self
                .store
                .call(move |tx| store::last_attempt(tx, &i, &stage))?
                .context("a candidate whose turn is done has an attempt")?;
            self.candidate_checkpoint(t, c, &a)?;
        }
        let pending: Vec<CandidateRow> = pending
            .into_iter()
            .filter(|c| c.state == "pending")
            .collect();
        if pending.is_empty() {
            let i = self.id.clone();
            self.store
                .call(move |tx| store::set_fanout_phase(tx, &i, phase::VERIFY))?;
            return Ok(Flow::Next);
        }
        if !self.admitted()? {
            return Ok(Flow::Next); // paused (R41): `budget set` + `resume` runs these again
        }
        for c in &pending {
            if self.turn_blocked(t, &cand::stage(c.k), &c.profile)? {
                return Ok(Flow::Next); // a machine stop (skills, the context): `resume` comes back
            }
        }
        let stop = self.lock.watch_stop();
        let budget = Arc::new(self.shared_budget(t)?);
        let mut prepared = Vec::new();
        for c in &pending {
            match self.prepare_candidate(t, c, &budget)? {
                Ok(Some(p)) => prepared.push(p),
                Ok(None) => {} // finished before its turn (disqualified at its start)
                Err(flow) => {
                    // the task escalated while a turn was prepared: the ones begun end now
                    for p in &prepared {
                        self.finish_and(
                            &p.attempt,
                            AttemptStatus::Interrupted,
                            AttemptEnd {
                                error_detail: Some("not started: the task escalated".into()),
                                ..Default::default()
                            },
                            None,
                            None,
                        )?;
                    }
                    return Ok(flow);
                }
            }
        }
        if prepared.is_empty() {
            return Ok(Flow::Next);
        }
        budget
            .cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(prepared.iter().map(|p| p.cancel.clone()));
        let limits = self.limits();
        let hub = self.hub;
        let (tx, rx) = std::sync::mpsc::channel::<(usize, Result<StageOutcome>)>();
        let mut escalated = false;
        let mut stopped = false;
        let outcome: Result<()> = std::thread::scope(|s| {
            for (n, p) in prepared.iter_mut().enumerate() {
                let tx = tx.clone();
                let (open, input, hooks) = (
                    p.open.clone(),
                    p.input.clone(),
                    std::mem::take(&mut p.hooks),
                );
                s.spawn(move || {
                    let r = hub.turn_with(open, input, limits, hooks);
                    let _ = tx.send((n, r));
                });
            }
            drop(tx);
            // M17-B12 test hook: a Ctrl+C while the candidates' turns run
            crate::engine::lock::interrupt_point("fanout:turns");
            let cancel_all = |flags: &[&Arc<AtomicBool>]| {
                for f in flags {
                    f.store(true, Ordering::SeqCst);
                }
            };
            let mut done = 0;
            while done < prepared.len() {
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok((n, r)) => {
                        done += 1;
                        crash_point("fanout:after_turn");
                        let p = &prepared[n];
                        let r = match r {
                            Ok(r) => r,
                            Err(e) => {
                                // the turn could not run at all: the candidate's turn failed
                                crate::errln!(
                                    "!! candidate {}: the turn ended in an error: {e:#}",
                                    cand::id(p.c.k)
                                );
                                StageOutcome {
                                    error_class: Some(ErrorClass::Unknown),
                                    error_detail: Some(format!("{e:#}")),
                                    ..Default::default()
                                }
                            }
                        };
                        let budget_hit = budget.hit.load(Ordering::SeqCst);
                        match self.candidate_post_turn(t, p, r, budget_hit, escalated)? {
                            TurnEnd::Escalated => {
                                escalated = true;
                                cancel_all(&prepared.iter().map(|p| &p.cancel).collect::<Vec<_>>());
                            }
                            TurnEnd::Finished | TurnEnd::Pending => {}
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if stop.stopped() && !stopped {
                            stopped = true;
                            cancel_all(&prepared.iter().map(|p| &p.cancel).collect::<Vec<_>>());
                            // M17-B12 test hook: a second Ctrl+C while the turns are ended
                            crate::engine::lock::interrupt_point("fanout:stopping");
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            Ok(())
        });
        outcome?;
        if escalated {
            return Ok(Flow::Next);
        }
        if budget.hit.load(Ordering::SeqCst) {
            crate::errln!(
                "!! budget reached during the candidates' turns — interrupted (R41 live interrupt), the task pauses; `toryo budget set {}` and `toryo resume {}` run the unfinished candidates",
                t.id, t.id
            );
            let from = self.task()?.state;
            let i = self.id.clone();
            let m = self
                .store
                .call(move |tx| store::transition(tx, &i, Ev::BudgetExceeded))?;
            self.rec
                .transition(from, "budget_exceeded", &m, true, false);
            return Ok(Flow::Next);
        }
        if stopped || stop.stopped() {
            return self.stopped();
        }
        if self.candidate_rows()?.iter().all(|c| c.state == "finished") {
            let i = self.id.clone();
            self.store
                .call(move |tx| store::set_fanout_phase(tx, &i, phase::VERIFY))?;
        }
        Ok(Flow::Next)
    }

    /// The live budget of the candidates' turns (X57).
    fn shared_budget(&self, t: &TaskRow) -> Result<SharedBudget> {
        let live = self.cfg.repo.pipeline.budget_live_interrupt.unwrap_or(true);
        let left = match (live, t.budget_tokens) {
            (true, Some(b)) => Some((b - t.spent_tokens - t.reserved_tokens).max(0) as u64),
            _ => None,
        };
        Ok(SharedBudget {
            left,
            used: Mutex::new(BTreeMap::new()),
            hit: AtomicBool::new(false),
            cancels: Mutex::new(Vec::new()),
        })
    }

    /// One candidate's turn, before it starts (as `prepare_implement` + `run_implement_turn` do
    /// for the single turn): the candidate's anchor is the base commit on its own branch, an empty
    /// TEMP, the attempt, the context, the hooks. `Ok(None)`: disqualified at its start (its
    /// branch or HEAD is not where Toryo left it); `Err(flow)`: the task escalated.
    fn prepare_candidate(
        &self,
        t: &TaskRow,
        c: &CandidateRow,
        budget: &Arc<SharedBudget>,
    ) -> Result<std::result::Result<Option<Prepared>, Flow>> {
        let wt = Worktree::new(&self.g, &c.worktree);
        let stage = cand::stage(c.k);
        // X31 for a candidate: an unfinished Git operation is recovery's (§10) — the task stops
        if let Some(op) = anchor::unfinished_git_operation(&wt)? {
            let reason = format!(
                "{}: candidate {} ({}) has an unfinished Git operation ({op}) in {} — finish or abort it, then `toryo resume {} --note \"…\"`",
                crate::engine::recovery::POLICY_VIOLATION,
                cand::id(c.k),
                c.profile,
                c.worktree.display(),
                t.id
            );
            self.escalate(crate::engine::recovery::POLICY_VIOLATION, reason)?;
            return Ok(Err(Flow::Next));
        }
        let head = wt.head().ok();
        let on_branch = wt.head_ref()? == Some(format!("refs/heads/{}", c.branch));
        if head.as_deref() != Some(t.base_commit.as_str()) {
            if on_branch && head.is_some() && wt.contains(&t.base_commit)? {
                // an earlier, interrupted turn committed: undone into the tree (X24)
                wt.soft_reset(&t.base_commit)?;
                crate::errln!(
                    "note: candidate {}: commits on its branch were undone; their changes stay in the tree and are checked like any turn's",
                    cand::id(c.k)
                );
            } else {
                let why = format!(
                    "its HEAD ({}) is not the base commit on {}",
                    head.as_deref().map(short).unwrap_or("?"),
                    c.branch
                );
                self.disqualify(c, "policy_violation", &why)?;
                return Ok(Ok(None));
            }
        }
        let kind = self.cfg.profile(&c.profile)?.kind;
        let model = match kind {
            ProfileKind::Codex => self.role_model("implementer").or_else(|| {
                self.cfg
                    .profile(&c.profile)
                    .ok()
                    .and_then(|p| p.model.clone())
            }),
            ProfileKind::Claude => None,
        };
        let prev = {
            let (i, s) = (self.id.clone(), stage.clone());
            self.store.call(move |tx| store::last_attempt(tx, &i, &s))?
        };
        let mut prompt = implementer_prompt(0, None, None);
        if let Some(rule) = self.proof_rule(t)? {
            prompt.push_str(&rule);
        }
        if prev
            .as_ref()
            .is_some_and(|p| p.status == AttemptStatus::Interrupted)
            && !wt.is_clean()?
        {
            prompt.push_str("\n## Recovery\nYour previous turn on this candidate was interrupted before it finished. The worktree keeps what it changed — check that work, keep what is correct, fix or complete the rest, and finish the task.\n");
        }
        if kind == ProfileKind::Claude {
            prompt = claude_implementer_prompt(&prompt);
        }
        let resume = implementer_resume(prev.as_ref(), kind, &c.profile);
        let tmp = c.worktree.join(".toryo-tmp");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        let env: BTreeMap<String, String> = [("TEMP", &tmp), ("TMP", &tmp)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.display().to_string()))
            .collect();
        let a = self.begin(NewAttempt {
            stage: &stage,
            kind: "agent",
            profile: Some(&c.profile),
            fallback_from: None,
            head_before: Some(&t.base_commit),
            subject_commit: None,
        })?;
        self.stage_started(
            &a,
            Some(format!(
                "candidate {} in {}",
                cand::id(c.k),
                c.worktree.display()
            )),
        );
        crash_point("fanout:after_begin");
        let Some((ctx, data)) = self.context_and_data(t, &a, &c.profile)? else {
            return Ok(Err(Flow::Next));
        };
        let prompt = match data {
            Some(d) => format!("{prompt}{d}"),
            None => prompt,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(Mutex::new(None));
        let hooks = self.candidate_hooks(&a, c, &cancel, &pid, budget)?;
        let open = OpenSession {
            profile: c.profile.clone(),
            role: Role::Implementer,
            cwd: c.worktree.clone(),
            run_store: None,
            capture_dir: t.captures(),
            schema: Some(contracts::schema("implementation.schema.json")?.into()),
            context_file: Some(ctx),
            model: model.clone(),
            resume,
        };
        let input = TurnInput {
            prompt,
            label: format!("{stage}-{}", a.attempt),
            env,
            echo: true,
        };
        Ok(Ok(Some(Prepared {
            c: c.clone(),
            profile: c.profile.clone(),
            model,
            attempt: a,
            open,
            input,
            cancel,
            pid,
            hooks,
        })))
    }

    /// The hooks of a candidate's turn: its own cancel flag (the stop request and a sibling's
    /// escalation set it), its session, its pid (X39), and the shared live budget (X57).
    fn candidate_hooks(
        &self,
        a: &AttemptRow,
        c: &CandidateRow,
        cancel: &Arc<AtomicBool>,
        pid: &Arc<Mutex<Option<u32>>>,
        budget: &Arc<SharedBudget>,
    ) -> Result<TurnHooks> {
        let (st, aid) = (self.store.clone(), a.id.clone());
        let (rec, cell) = (self.rec.clone(), pid.clone());
        let (stage, n, who) = (a.stage.clone(), a.attempt, c.profile.clone());
        let mut h = TurnHooks {
            cancel: Some(cancel.clone()),
            on_session: Some(Arc::new(move |sid: &str| {
                let (a, s) = (aid.clone(), sid.to_string());
                let _ = st.call(move |tx| store::set_attempt_session(tx, &a, &s));
            })),
            on_usage: None,
            on_spawn: Some(Arc::new(move |p: Option<u32>| {
                *cell.lock().unwrap_or_else(|e| e.into_inner()) = p;
                rec.emit_quiet(EngineEvent::AgentSpawned {
                    stage: stage.clone(),
                    attempt: n,
                    profile: who.clone(),
                    pid: p,
                });
            })),
        };
        if self.hub.capabilities(&c.profile)?.live && budget.left.is_some() {
            let (b, w, k) = (budget.clone(), self.weights(), c.k);
            h.on_usage = Some(Arc::new(move |u: &crate::domain::TokenUsage| {
                let Some(now) = u.buckets.map(|x| x.budget_tokens(&w)) else {
                    return;
                };
                let total: u64 = {
                    let mut used = b.used.lock().unwrap_or_else(|e| e.into_inner());
                    used.insert(k, now);
                    used.values().sum()
                };
                if b.left.is_some_and(|l| total >= l) && !b.hit.swap(true, Ordering::SeqCst) {
                    for f in b.cancels.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                        f.store(true, Ordering::SeqCst);
                    }
                }
            }));
        }
        Ok(h)
    }

    /// M17-B6 (AC-16, AC-17): one candidate's turn is over — the single turn's post-turn path in
    /// its worktree: the HEAD guard, a turn cut short, the report, the protected state (escalates
    /// the task), a failed turn (`failed_turn`, no fallback chain), the path policy (disqualifies
    /// the candidate), then Toryo's checkpoint commit. `escalated`: the task is already escalated
    /// (by a sibling) — this turn only ends.
    fn candidate_post_turn(
        &self,
        t: &TaskRow,
        p: &Prepared,
        r: StageOutcome,
        budget_hit: bool,
        escalated: bool,
    ) -> Result<TurnEnd> {
        let (a, c) = (&p.attempt, &p.c);
        if r.cancelled || r.timed_out {
            let pid = *p.pid.lock().unwrap_or_else(|e| e.into_inner());
            self.rec.emit_quiet(EngineEvent::AgentKilled {
                stage: a.stage.clone(),
                attempt: a.attempt,
                profile: p.profile.clone(),
                pid,
                reason: match (budget_hit, r.cancelled) {
                    (true, _) => "budget",
                    (false, true) => "stop",
                    (false, false) => "limit",
                }
                .into(),
            });
        }
        self.usage(a, &p.profile, &r, p.model.as_deref())?;
        let wt = Worktree::new(&self.g, &c.worktree);
        // the HEAD guard (§8, X24) on the candidate's own branch
        match HeadGuard::check(&wt, &c.branch, &t.base_commit, &t.base_commit)? {
            HeadGuard::Unchanged => {}
            HeadGuard::Reset { head } => {
                crate::errln!(
                    "!! policy_violation: candidate {} moved HEAD — soft reset to the base",
                    cand::id(c.k)
                );
                wt.soft_reset(&t.base_commit)?;
                let (i, aid) = (self.id.clone(), a.id.clone());
                let payload = json!({"kind": "agent commit", "action": "soft reset", "head": head, "reset_to": t.base_commit, "candidate": cand::id(c.k)});
                self.store.call(move |tx| {
                    store::event(tx, Some(&i), Some(&aid), "policy_violation", &payload)
                })?;
            }
            HeadGuard::Violation { reason, .. } => {
                self.end_candidate_attempt(
                    a,
                    &r,
                    AttemptStatus::Failed,
                    Some("policy_violation"),
                    Some(&reason),
                )?;
                self.disqualify(c, "policy_violation", &reason)?;
                return Ok(TurnEnd::Finished);
            }
        }
        if escalated || r.cancelled || budget_hit {
            let class = budget_hit.then_some("budget");
            self.end_candidate_attempt(a, &r, AttemptStatus::Interrupted, class, None)?;
            return Ok(TurnEnd::Pending);
        }
        if let Some(v) = &r.structured {
            self.put(
                t,
                a,
                &format!("implementation#{}", cand::id(c.k)),
                "json",
                &serde_json::to_vec_pretty(v)?,
                &format!("candidates/{}/implementation.json", cand::id(c.k)),
            )?;
            let red_green = crate::engine::redgreen::gate_of(&self.cmds).is_some();
            if let Err(errors) = contracts::validate(
                "implementation.schema.json",
                &contracts::implementation_as_checked(v, red_green),
            ) {
                let (i, aid) = (self.id.clone(), a.id.clone());
                let payload = json!({"schema": "implementation.schema.json", "errors": errors});
                self.store.call(move |tx| {
                    store::event(tx, Some(&i), Some(&aid), "schema_violation", &payload)
                })?;
            }
        }
        // R35 in the candidate's worktree, against its own baseline: escalates the task
        if let Some(reason) = self.candidate_protected(t, c)? {
            self.end_candidate_attempt(a, &r, AttemptStatus::Failed, Some(protect::REASON), None)?;
            self.escalate(protect::REASON, reason)?;
            return Ok(TurnEnd::Escalated);
        }
        // a failed turn: no fallback chain for a candidate (AC-16)
        if !r.success {
            let why = implementer_failed(r.error_class, r.error_detail.as_deref());
            r.print_summary();
            self.end_candidate_attempt(a, &r, AttemptStatus::Failed, None, None)?;
            let (i, k, w) = (self.id.clone(), c.k, why.clone());
            self.store.call(move |tx| {
                store::set_candidate_outcome(tx, &i, k, "failed_turn", None, Some(&w))
            })?;
            self.candidate_finished(c.k, "failed_turn", None, None);
            return Ok(TurnEnd::Finished);
        }
        // the path policy (protected tracked paths, deny, the fence): this candidate only
        let bad: Vec<String> = wt
            .changed()?
            .into_iter()
            .map(|x| x.1)
            .filter(|p| self.protect.violates(p))
            .collect();
        if !bad.is_empty() {
            let why = self.protect.violation_words(&bad);
            self.end_candidate_attempt(
                a,
                &r,
                AttemptStatus::Failed,
                Some("policy_violation"),
                Some(&why),
            )?;
            self.disqualify(c, "policy_violation", &why)?;
            return Ok(TurnEnd::Finished);
        }
        // the turn passed its checks: recorded with the attempt's end, then the checkpoint
        let (i, k, aid) = (self.id.clone(), c.k, a.id.clone());
        let end = AttemptEnd {
            external_session_id: r.session_id.clone(),
            ..Default::default()
        };
        self.store.call(move |tx| {
            store::finish_attempt(tx, &aid, AttemptStatus::Succeeded, &end)?;
            store::set_candidate_state(tx, &i, k, "turn_done")
        })?;
        self.stage_finished(a, AttemptStatus::Succeeded, None);
        self.candidate_checkpoint(t, c, a)?;
        Ok(TurnEnd::Finished)
    }

    /// End a candidate's attempt (no state transition: the task stays in its stage).
    fn end_candidate_attempt(
        &self,
        a: &AttemptRow,
        r: &StageOutcome,
        status: AttemptStatus,
        class: Option<&str>,
        detail: Option<&str>,
    ) -> Result<()> {
        self.close(
            a,
            Close::of_turn(
                status,
                r,
                AttemptEnd {
                    error_class: class.map(str::to_string),
                    error_detail: detail.map(str::to_string),
                    ..Default::default()
                },
            ),
        )
    }

    /// `disqualified:<class>` for candidate `c` (AC-17: a fence or deny violation, a moved HEAD, a
    /// mutated verify checkout — this candidate only).
    fn disqualify(&self, c: &CandidateRow, class: &str, why: &str) -> Result<()> {
        crate::errln!(
            "!! candidate {} ({}) is disqualified: {class} — {why}",
            cand::id(c.k),
            c.profile
        );
        let outcome = format!("disqualified:{class}");
        let (i, k, o, w) = (self.id.clone(), c.k, outcome.clone(), why.to_string());
        self.store
            .call(move |tx| store::set_candidate_outcome(tx, &i, k, &o, None, Some(&w)))?;
        self.candidate_finished(c.k, &outcome, c.head.clone(), None);
        Ok(())
    }

    /// The printed `candidate_finished` event (AC-27).
    fn candidate_finished(&self, k: u32, outcome: &str, head: Option<String>, gates: Option<bool>) {
        self.rec.emit(EngineEvent::CandidateFinished {
            k,
            outcome: outcome.to_string(),
            head,
            gates_passed: gates,
        });
    }

    /// R35 for candidate `c`: its worktree (and the Git metadata it reaches) against the baseline
    /// taken when Toryo made it — the reason when it changed protected state the hard way
    /// (untracked or ignored protected files, Git metadata).
    fn candidate_protected(&self, t: &TaskRow, c: &CandidateRow) -> Result<Option<String>> {
        let logical = format!("protected#{}", cand::id(c.k));
        let Some((_, bytes)) = crate::engine::artifacts::read_head(self.store, t, &logical)? else {
            return Ok(Some(format!(
                "artifact_tampered: the protected baseline of candidate {} is missing",
                cand::id(c.k)
            )));
        };
        let base: protect::PManifest = serde_json::from_slice(&bytes)?;
        let now = match protect::scan(&self.g, &c.worktree, &self.protect) {
            Ok(n) => n,
            Err(e) => {
                return Ok(Some(format!(
                    "{}: candidate {}: {e:#}",
                    protect::REASON,
                    cand::id(c.k)
                )))
            }
        };
        let changes =
            protect::compare_since_base(&self.g, &c.worktree, &t.base_commit, &base, &now);
        let hard: Vec<&protect::Change> = changes.iter().filter(|x| x.is_hard()).collect();
        if hard.is_empty() {
            return Ok(None);
        }
        let (i, payload) = (
            self.id.clone(),
            json!({"changes": hard, "candidate": cand::id(c.k), "worktree": c.worktree}),
        );
        self.store
            .call(move |tx| store::event(tx, Some(&i), None, "protected_changed", &payload))?;
        Ok(Some(format!(
            "{} (in candidate {}'s worktree {})",
            protect::escalation_reason(&t.id, &hard, None),
            cand::id(c.k),
            c.worktree.display()
        )))
    }

    /// M17-B6: Toryo's checkpoint of candidate `c` (attempt `a` succeeded) in its worktree —
    /// found by its trailers after a crash (taken only when it is Toryo's: HEAD, on the base, a
    /// diff the path policy allows), else committed now with the trailers naming the task and the
    /// candidate. No change: `no_change`, no commit.
    fn candidate_checkpoint(&self, t: &TaskRow, c: &CandidateRow, a: &AttemptRow) -> Result<()> {
        let wt = Worktree::new(&self.g, &c.worktree);
        let stage = cand::stage(c.k);
        if let Some(found) = wt
            .find_checkpoint(&t.base_commit, &t.id, &stage, a.attempt)?
            .filter(|f| !f.partial)
        {
            let why = crate::engine::recovery::foreign_commit(
                &wt,
                &self.protect,
                Some(&t.base_commit),
                &found.sha,
            )?;
            match why {
                None => {
                    self.candidate_checkpointed(t, c, &found.sha, true)?;
                    return Ok(());
                }
                Some(why) => {
                    // never accepted (recovery never takes a commit Toryo did not make): undone
                    // into the tree where that can be done, then checked like any change
                    let on_branch = wt.head_ref()? == Some(format!("refs/heads/{}", c.branch));
                    if on_branch
                        && wt.contains(&t.base_commit)?
                        && wt.unfinished_operation()?.is_none()
                    {
                        wt.soft_reset(&t.base_commit)?;
                    }
                    crate::errln!(
                        "!! candidate {}: commit {} carries its checkpoint's trailers but is not Toryo's ({why}) — undone",
                        cand::id(c.k),
                        short(&found.sha)
                    );
                }
            }
        }
        if wt.head()? != t.base_commit || wt.head_ref()? != Some(format!("refs/heads/{}", c.branch))
        {
            return self.disqualify(
                c,
                "policy_violation",
                "its HEAD is not the base commit on its branch at the checkpoint",
            );
        }
        let bad: Vec<String> = wt
            .changed()?
            .into_iter()
            .map(|x| x.1)
            .filter(|p| self.protect.violates(p))
            .collect();
        if !bad.is_empty() {
            let why = self.protect.violation_words(&bad);
            return self.disqualify(c, "policy_violation", &why);
        }
        if wt.nothing_to_commit()? {
            let (i, k) = (self.id.clone(), c.k);
            self.store.call(move |tx| {
                store::set_candidate_outcome(
                    tx,
                    &i,
                    k,
                    "no_change",
                    None,
                    Some("the turn changed nothing"),
                )
            })?;
            self.candidate_finished(c.k, "no_change", None, None);
            return Ok(());
        }
        let trailers = vec![
            (T_TASK, t.id.clone()),
            (T_STAGE, stage.clone()),
            (T_ATTEMPT, a.attempt.to_string()),
            (T_AGENT, c.profile.clone()),
            (T_CANDIDATE, cand::id(c.k)),
        ];
        let sha = wt.commit(
            &format!("toryo({}): {stage} (candidate {})", t.id, cand::id(c.k)),
            &trailers,
            false,
        )?;
        crash_point("fanout:after_checkpoint");
        self.candidate_checkpointed(t, c, &sha, false)
    }

    fn candidate_checkpointed(
        &self,
        _t: &TaskRow,
        c: &CandidateRow,
        sha: &str,
        found: bool,
    ) -> Result<()> {
        let (i, k, s) = (self.id.clone(), c.k, sha.to_string());
        self.store.call(move |tx| {
            store::set_candidate_outcome(tx, &i, k, "checkpointed", Some(&s), None)
        })?;
        crate::errln!(
            "✔ candidate {} ({}): checkpoint {}{}",
            cand::id(c.k),
            c.profile,
            short(sha),
            if found {
                " (already committed: trailer match)"
            } else {
                ""
            }
        );
        Ok(())
    }

    // ── verify (B7) ────────────────────────────────────────────────────────────────────────

    /// M17-B7 (AC-18): the check gates of every checkpointed candidate not verified yet, one after
    /// another (each under the machine verify lock), then the selection.
    fn candidates_verify(&self, t: &TaskRow) -> Result<Flow> {
        crash_point("fanout:before_verify");
        for c in self.candidate_rows()? {
            if c.outcome.as_deref() != Some("checkpointed") || c.verify_pass.is_some() {
                continue;
            }
            if let Some(flow) = self.candidate_verify(t, &c)? {
                return Ok(flow);
            }
        }
        let i = self.id.clone();
        self.store
            .call(move |tx| store::set_fanout_phase(tx, &i, phase::SELECT))?;
        Ok(Flow::Next)
    }

    /// One candidate's check gates — red-green included, no environment gate — in
    /// `<verify_root>/<task>/c<k>`, with its own verify-result artifact. A mutated checkout
    /// disqualifies the candidate; a machine stop escalates the task (a plain `resume` comes back
    /// here); a stop interrupts the attempt. `Some(flow)`: the stage ended.
    fn candidate_verify(&self, t: &TaskRow, c: &CandidateRow) -> Result<Option<Flow>> {
        let head = c
            .head
            .clone()
            .context("a checkpointed candidate has a head")?;
        let stage = cand::verify_stage(c.k);
        let a = self.begin(NewAttempt {
            stage: &stage,
            kind: "command",
            profile: None,
            fallback_from: None,
            head_before: Some(&head),
            subject_commit: Some(&head),
        })?;
        let xpp = match xpp::for_config(&self.cfg) {
            Ok(x) => x,
            Err(e) => {
                let reason = format!(
                    "{e:#} — {}",
                    machine_fix_hint(
                        &t.id,
                        "a NuGet folder or MSBuild put back",
                        "`toolchains.xpp` in the machine config"
                    )
                );
                self.escalate_attempt(&a, xpp::UNAVAILABLE.into(), reason)?;
                return Ok(Some(Flow::Next));
            }
        };
        if let Some(reason) = self.check_gates_preflight(t) {
            self.escalate_attempt(&a, GATE_UNAVAILABLE.into(), reason)?;
            return Ok(Some(Flow::Next));
        }
        let Some(lock) = self.verify_lock(t, &self.lock.watch_stop())? else {
            self.finish_and(
                &a,
                AttemptStatus::Interrupted,
                AttemptEnd::default(),
                None,
                None,
            )?;
            return self.stopped().map(Some);
        };
        let lock = Held::new(self.rec, crate::engine::lock::VERIFY_LOCK, lock);
        if let Some(l) = &lock.lock {
            l.announce(&t.id);
        }
        let slot = verify::Slot::candidate(t, &self.cfg, c.k);
        let prepared = verify::boundary(&self.cfg).and_then(|b| {
            verify::prepare_in(&self.g, t, &self.cfg, &head, b, &slot).map(|co| (b, co))
        });
        let (b, co) = match prepared {
            Ok(x) => x,
            Err(e) => match e.downcast_ref::<verify::Stop>().map(|s| s.class) {
                Some(class) => {
                    self.escalate_attempt(&a, class.to_string(), format!("{e:#}"))?;
                    return Ok(Some(Flow::Next));
                }
                None => return Err(e),
            },
        };
        self.stage_started(
            &a,
            Some(format!(
                "candidate {}, {} in {}",
                cand::id(c.k),
                verify::describe(b),
                co.dir.display()
            )),
        );
        let stop = self.lock.watch_stop();
        let proof = ProofInput {
            proof: self.candidate_proof(t, c)?,
            exempt: self.proof_exempt(t)?,
            slot,
        };
        let ran = match self.run_verify_commands(b, &co, &head, &stop, xpp.as_ref(), &proof) {
            Ok(r) => r,
            Err(e) => return self.gate_start_failed(t, &a, e).map(Some),
        };
        let Some(run) = ran else {
            self.finish_and(
                &a,
                AttemptStatus::Interrupted,
                AttemptEnd::default(),
                None,
                None,
            )?;
            return self.stopped().map(Some);
        };
        crash_point("fanout:after_gates");
        let scripts = verify::script_hashes(
            &verify_files(&crate::engine::checks::with_red_green(&self.cmds), &co.dir),
            &co,
        );
        let mutated = match run.red_mutated.clone() {
            Some(m) => Some(m),
            None => verify::mutated(&self.g, &co, &head)?,
        };
        drop(lock);
        let pass = run.failure.is_none() && mutated.is_none();
        let mut vr = json!({
            "candidate": cand::id(c.k),
            "head": head,
            "pass": pass,
            "boundary": verify::describe(b),
            "checkout": self.cfg.repo.verify.checkout,
            "config_sha256": t.config_sha256,
            "scripts": scripts,
            "mutated": mutated,
            "commands": run.results,
        });
        if let Some(r) = &run.red_green {
            vr["red_green"] = json!(r);
        }
        self.put(
            t,
            &a,
            &format!("verify-result#{}", cand::id(c.k)),
            "json",
            &serde_json::to_vec_pretty(&vr)?,
            &format!("candidates/{}/VERIFY_RESULT.json", cand::id(c.k)),
        )?;
        if let Some(reason) = mutated {
            self.finish_and(
                &a,
                AttemptStatus::Failed,
                AttemptEnd {
                    error_class: Some(verify::MUTATED.into()),
                    error_detail: Some(reason.clone()),
                    ..Default::default()
                },
                None,
                None,
            )?;
            self.disqualify(c, verify::MUTATED, &reason)?;
            return Ok(None);
        }
        if let Some(reason) = self.protected_violation(t, false)? {
            self.escalate_attempt(&a, protect::REASON.into(), reason)?;
            return Ok(Some(Flow::Next));
        }
        let numstat = self.g.run_str(
            &crate::git::Loc::Discover(t.repo.clone()),
            true,
            &["diff", "--numstat", "--no-renames", &t.base_commit, &head],
        )?;
        let changed = cand::changed_lines(&numstat);
        let proven = run.red_green.as_ref().map(|r| r.proven as u32);
        let (i, k, fb) = (self.id.clone(), c.k, run.failure.clone());
        self.store.call(move |tx| {
            store::set_candidate_verify(tx, &i, k, pass, proven, changed, fb.as_deref())
        })?;
        self.finish_and(
            &a,
            if pass {
                AttemptStatus::Succeeded
            } else {
                AttemptStatus::Failed
            },
            AttemptEnd::default(),
            None,
            None,
        )?;
        self.candidate_finished(c.k, "checkpointed", Some(head), Some(pass));
        Ok(None)
    }

    /// The proof map of candidate `c` (X51): its own implementation report.
    fn candidate_proof(&self, t: &TaskRow, c: &CandidateRow) -> Result<Option<Value>> {
        let i = self.id.clone();
        let stage = cand::stage(c.k);
        let Some(a) = self.store.call(move |tx| {
            Ok(store::attempts(tx, &i)?
                .into_iter()
                .rev()
                .find(|a| a.stage == stage && a.status == AttemptStatus::Succeeded))
        })?
        else {
            return Ok(None);
        };
        self.proof_of_attempt(t, &a.id, &format!("implementation#{}", cand::id(c.k)))
    }

    // ── selection (B8) ─────────────────────────────────────────────────────────────────────

    /// M17-B8 (AC-19 … AC-21): the winner — the only passing candidate, else the compare turn of
    /// the reviewer seat among the passing ones, else the score; with none passing the best by
    /// score (it enters Fixing); with none checkpointed the single path, once. The losers' worktree
    /// folders are removed; their branches stay.
    fn select_winner(&self, t: &TaskRow) -> Result<Flow> {
        let rows = self.candidate_rows()?;
        let eligible: Vec<&CandidateRow> = rows
            .iter()
            .filter(|c| c.outcome.as_deref() == Some("checkpointed"))
            .collect();
        if eligible.is_empty() {
            let outcomes: Vec<String> = rows
                .iter()
                .map(|c| {
                    format!(
                        "{} {}",
                        cand::id(c.k),
                        c.outcome.as_deref().unwrap_or("unfinished")
                    )
                })
                .collect();
            let note = format!(
                "best-of-N: no candidate made a checkpoint ({}) — the single implementer path runs (the primary and its fallback chain), once",
                outcomes.join(", ")
            );
            crate::errln!("note: {note}");
            for c in &rows {
                self.remove_candidate_worktree(t, c)?;
            }
            let i = self.id.clone();
            self.store.call(move |tx| {
                store::set_fanout_note(tx, &i, &note)?;
                store::set_fanout_phase(tx, &i, phase::SINGLE)
            })?;
            return Ok(Flow::Next);
        }
        let mut scored = Vec::new();
        for c in &eligible {
            let (i, k) = (self.id.clone(), c.k);
            let tokens = self
                .store
                .call(move |tx| store::candidate_tokens(tx, &i, k))?;
            scored.push(cand::Scored {
                k: c.k,
                pass: c.verify_pass == Some(true),
                proven: c.proven.unwrap_or(0),
                changed: c.changed_lines.unwrap_or(0),
                tokens,
            });
        }
        let ranked = cand::rank(scored);
        let passing: Vec<u32> = ranked.iter().filter(|s| s.pass).map(|s| s.k).collect();
        let (winner, by, reason) = match passing.len() {
            1 => (
                passing[0],
                "only",
                "the only candidate that passed every check gate".to_string(),
            ),
            0 => (ranked[0].k, "score", cand::score_reason(&ranked)),
            _ => match self.compare_turn(t, &passing)? {
                Compared::Winner(k, why) => (k, "compare", why),
                Compared::Score(why) => {
                    let among: Vec<cand::Scored> =
                        ranked.iter().filter(|s| s.pass).cloned().collect();
                    (
                        among[0].k,
                        "score",
                        format!("{why}; {}", cand::score_reason(&among)),
                    )
                }
                Compared::Ended(flow) => return Ok(flow),
            },
        };
        let (i, r) = (self.id.clone(), reason.clone());
        self.store
            .call(move |tx| store::set_fanout_winner(tx, &i, winner, by, Some(&r)))?;
        self.rec.emit(EngineEvent::CandidateSelected {
            k: winner,
            by: by.to_string(),
        });
        crash_point("fanout:after_select");
        let losers: Vec<&CandidateRow> = rows.iter().filter(|c| c.k != winner).collect();
        for c in &losers {
            self.remove_candidate_worktree(t, c)?;
        }
        let w = rows.iter().find(|c| c.k == winner).expect("the winner");
        crate::errln!(
            "   winner: candidate {} ({}) by {by} — {reason}",
            cand::id(winner),
            w.profile
        );
        if !losers.is_empty() {
            crate::errln!(
                "   the other candidates' branches are kept: {}",
                losers
                    .iter()
                    .map(|c| c.branch.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        Ok(Flow::Next)
    }

    /// M17-B10 (AC-23): remove candidate `c`'s worktree folder — only the one the store recorded at
    /// its place (X44); its branch stays.
    pub(in crate::engine) fn remove_candidate_worktree(
        &self,
        t: &TaskRow,
        c: &CandidateRow,
    ) -> Result<()> {
        if !c.created || c.removed {
            return Ok(());
        }
        let root = self.worktrees_root()?;
        if c.worktree != cand::worktree_path(&root, &t.id, c.k) {
            bail!(
                "candidate {}'s recorded worktree {} is not {} — it is not removed",
                cand::id(c.k),
                c.worktree.display(),
                cand::worktree_path(&root, &t.id, c.k).display()
            );
        }
        if std::fs::symlink_metadata(&c.worktree).is_ok() {
            let repo = crate::git::Loc::Discover(t.repo.clone());
            let fwd = c.worktree.display().to_string().replace('\\', "/");
            if self
                .g
                .run(&repo, false, &["worktree", "remove", "--force", &fwd])
                .is_err()
            {
                std::fs::remove_dir_all(&c.worktree)
                    .with_context(|| format!("removing {}", c.worktree.display()))?;
                let _ = self.g.run(&repo, false, &["worktree", "prune"]);
            }
        }
        let (i, k) = (self.id.clone(), c.k);
        self.store
            .call(move |tx| store::set_candidate_removed(tx, &i, k))?;
        Ok(())
    }

    /// M17-B8 (AC-19): one read-only compare turn of the reviewer seat among the passing
    /// candidates — their diffs, implementation reports and verify summaries as data — answering
    /// `select.schema.json`; one correction turn for an answer that is not valid; a failed turn, a
    /// second invalid answer or a turn that wrote (put back) falls to the score.
    fn compare_turn(&self, t: &TaskRow, passing: &[u32]) -> Result<Compared> {
        if !self.admitted()? {
            return Ok(Compared::Ended(Flow::Next));
        }
        if self.turn_blocked(t, cand::SELECT_STAGE, &t.reviewer)? {
            return Ok(Compared::Ended(Flow::Next));
        }
        let a = self.begin(NewAttempt {
            stage: cand::SELECT_STAGE,
            kind: "agent",
            profile: Some(&t.reviewer),
            fallback_from: None,
            head_before: Some(&t.base_commit),
            subject_commit: None,
        })?;
        self.stage_started(
            &a,
            Some(format!(
                "compare {}",
                passing
                    .iter()
                    .map(|k| cand::id(*k))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        );
        let Some(ctx) = self.context(t, &a, &t.reviewer)? else {
            return Ok(Compared::Ended(Flow::Next));
        };
        let ids: Vec<String> = passing.iter().map(|k| cand::id(*k)).collect();
        let mut prompt = cand::compare_prompt(&ids);
        prompt.push_str(&format!("\n{}\n", untrusted::NOTE));
        for k in passing {
            prompt.push_str(&self.compare_data(t, *k)?);
        }
        let stop = self.lock.watch_stop();
        let wt = Worktree::new(&self.g, &t.worktree);
        let (mut ask, mut resume, mut corrections) = (prompt, None::<String>, 0u32);
        loop {
            let before = TreeState::read(&wt)?;
            crate::engine::artifacts::put(
                self.store,
                t,
                Some(&a.id),
                &tree_before_name(&a),
                "json",
                "tree_before",
                &serde_json::to_vec(&before)?,
                None,
            )?;
            let (hooks, budget_hit) = self.hooks(&a, &stop, &t.reviewer)?;
            let r = self.hub.turn_with(
                OpenSession {
                    profile: t.reviewer.clone(),
                    role: Role::Reviewer,
                    cwd: t.worktree.clone(),
                    run_store: Some(t.artifacts()),
                    capture_dir: t.captures(),
                    schema: Some(contracts::schema("select.schema.json")?.into()),
                    context_file: Some(ctx.clone()),
                    model: self.role_model("reviewer"),
                    resume: resume.clone(),
                },
                TurnInput {
                    prompt: ask.clone(),
                    label: format!("select-{}-{corrections}", a.attempt),
                    echo: true,
                    env: BTreeMap::new(),
                },
                self.limits(),
                hooks,
            );
            crash_point("fanout:after_compare_turn");
            // the checks of every read-only turn, however it ended: the protected state first
            // (it escalates the task), then what it wrote is put back (X32)
            if let Some(reason) = self.protected_violation(t, false)? {
                let _ = undo_read_only_write(&wt, t, &a.id, &before);
                self.escalate_attempt(&a, protect::REASON.into(), reason)?;
                return Ok(Compared::Ended(Flow::Next));
            }
            if let Some(u) = undo_read_only_write(&wt, t, &a.id, &before)? {
                if u.pending {
                    // what it wrote could not be put back: the task stops (the promotion needs a clean
                    // worktree at the base)
                    let end = AttemptEnd {
                        error_class: Some("policy_violation".into()),
                        error_detail: Some(u.reason.clone()),
                        undo_pending: true,
                        ..Default::default()
                    };
                    self.close(
                        &a,
                        Close::of(AttemptStatus::Failed, end).then(
                            Some(Ev::Escalate {
                                reason: u.reason,
                                class: None,
                            }),
                            None,
                        ),
                    )?;
                    return Ok(Compared::Ended(Flow::Next));
                }
                crate::errln!(
                    "!! the compare turn changed the worktree — put back; its answer is not used: {}",
                    u.reason
                );
                self.finish_and(
                    &a,
                    AttemptStatus::Failed,
                    AttemptEnd {
                        error_class: Some("policy_violation".into()),
                        error_detail: Some(u.reason.clone()),
                        ..Default::default()
                    },
                    None,
                    None,
                )?;
                return Ok(Compared::Score(
                    "the compare turn wrote into the worktree (put back, its answer not used)"
                        .into(),
                ));
            }
            let r = match r {
                Ok(r) => r,
                Err(e) => {
                    self.finish_and(
                        &a,
                        AttemptStatus::Failed,
                        AttemptEnd {
                            error_class: Some("Unknown".into()),
                            error_detail: Some(format!("{e:#}")),
                            ..Default::default()
                        },
                        None,
                        None,
                    )?;
                    crate::errln!("!! the compare turn ended in an error: {e:#}");
                    // (the reason is shown by `status`: Toryo's words only, X35)
                    return Ok(Compared::Score("the compare turn ended in an error".into()));
                }
            };
            self.agent_ended(&a, &t.reviewer, &r, budget_hit.load(Ordering::SeqCst));
            self.usage(&a, &t.reviewer, &r, self.role_model("reviewer").as_deref())?;
            if let Some(flow) = self.cut_short(&a, &r, budget_hit.load(Ordering::SeqCst))? {
                return Ok(Compared::Ended(flow));
            }
            let problems = match ReadOnlyTurn::of(&r) {
                ReadOnlyTurn::Output(v) => match cand::check_answer(&v, passing) {
                    Ok(ans) => {
                        self.put(
                            t,
                            &a,
                            "select",
                            "json",
                            &serde_json::to_vec_pretty(&v)?,
                            "select.json",
                        )?;
                        self.end_and(
                            &a,
                            AttemptStatus::Succeeded,
                            &r,
                            AttemptEnd {
                                correction_turns: Some(corrections),
                                ..Default::default()
                            },
                            None,
                            None,
                        )?;
                        return Ok(Compared::Winner(ans.winner, ans.reason));
                    }
                    Err(p) => p,
                },
                ReadOnlyTurn::Invalid(why) => vec![why],
                ReadOnlyTurn::Retry(class) | ReadOnlyTurn::Failed(class) => {
                    r.print_summary();
                    self.end_and(
                        &a,
                        AttemptStatus::Failed,
                        &r,
                        AttemptEnd {
                            error_class: Some(format!("{class:?}")),
                            correction_turns: Some(corrections),
                            ..Default::default()
                        },
                        None,
                        None,
                    )?;
                    return Ok(Compared::Score(format!(
                        "the compare turn failed ({class:?})"
                    )));
                }
            };
            crate::errln!(
                "!! invalid compare answer (correction {}/1): {problems:?}",
                corrections + 1
            );
            if corrections == 1 {
                self.end_and(
                    &a,
                    AttemptStatus::Failed,
                    &r,
                    AttemptEnd {
                        error_class: Some("InvalidOutput".into()),
                        error_detail: Some(problems.join("\n")),
                        correction_turns: Some(corrections),
                        ..Default::default()
                    },
                    None,
                    None,
                )?;
                // (the problems quote the answer: they stay in the attempt, not in the reason)
                return Ok(Compared::Score(
                    "the compare turn's answer was not valid after one correction".into(),
                ));
            }
            if !self.admit_next_turn(&a, &r, corrections)? {
                return Ok(Compared::Ended(Flow::Next));
            }
            corrections += 1;
            ask = cand::correction_prompt(&problems);
            resume = r.session_id.clone();
        }
    }

    /// The data of candidate `k` for the compare turn: its DIFF.patch (base → checkpoint), its
    /// implementation report and its verify summary — each a block of untrusted data (R61).
    fn compare_data(&self, t: &TaskRow, k: u32) -> Result<String> {
        let rows = self.candidate_rows()?;
        let c = rows.iter().find(|c| c.k == k).context("the candidate")?;
        let head = c.head.clone().unwrap_or_default();
        let diff = self.g.canonical_diff(
            &crate::git::Loc::Discover(t.repo.clone()),
            &t.base_commit,
            Some(&head),
        )?;
        let report = crate::engine::artifacts::read_head(
            self.store,
            t,
            &format!("implementation#{}", cand::id(k)),
        )?
        .map(|(_, b)| String::from_utf8_lossy(&b).to_string())
        .unwrap_or_else(|| "(no implementation report)".into());
        let vr: Value = crate::engine::artifacts::read_head(
            self.store,
            t,
            &format!("verify-result#{}", cand::id(k)),
        )?
        .and_then(|(_, b)| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
        let mut summary = format!(
            "candidate {} ({}), checkpoint {}: every check gate passed; {} changed lines\n",
            cand::id(k),
            c.profile,
            short(&head),
            c.changed_lines.unwrap_or(0)
        );
        for g in vr["commands"].as_array().into_iter().flatten() {
            summary.push_str(&format!(
                "- gate {}: exit {}\n",
                g["name"].as_str().unwrap_or("?"),
                g["exit"]
            ));
        }
        if let Some(r) = vr.get("red_green").and_then(|x| {
            serde_json::from_value::<crate::engine::redgreen::RedGreenResult>(x.clone()).ok()
        }) {
            summary.push_str(&format!(
                "red-green: {} of {} acceptance criteria proven\n{}",
                r.proven,
                r.total,
                crate::engine::redgreen::evidence_lines(&r)
            ));
        }
        let id = cand::id(k);
        Ok(format!(
            "\n{}\n\n{}\n\n{}\n",
            untrusted::wrap(
                &format!("{id} DIFF.patch"),
                None,
                &String::from_utf8_lossy(&diff)
            ),
            untrusted::wrap(&format!("{id} IMPLEMENTATION.json"), None, &report),
            untrusted::wrap(&format!("{id} verify summary"), None, &summary)
        ))
    }

    // ── promotion (B9) ─────────────────────────────────────────────────────────────────────

    /// M17-B9 (AC-22): the task worktree must be at the base (or already at the winner, after a
    /// crash), clean, on `toryo/<ID>`, with no unfinished Git operation — else the task escalates
    /// and no branch moves. Then `toryo/<ID>` is moved to exactly the winner's checkpoint (no new
    /// commit), and one transaction records the promotion: the winner's implement attempt (the
    /// anchor, X24), the implementer chain with the winner first (fix cycles), the fan-out's end and
    /// the transitions — on to Verifying, or, for a winner that passed no gate, into Fixing with its
    /// verify feedback (one gate retry).
    fn promote(&self, t: &TaskRow, fan: &FanoutRow) -> Result<Flow> {
        let rows = self.candidate_rows()?;
        let k = fan.winner.context("a selected fan-out has a winner")?;
        let w = rows.iter().find(|c| c.k == k).context("the winner")?;
        let head = w.head.clone().context("the winner has a checkpoint")?;
        let wt = Worktree::new(&self.g, &t.worktree);
        let at = wt.head().ok();
        let on_branch = wt.head_ref()? == Some(format!("refs/heads/{}", t.branch));
        let unfinished = anchor::unfinished_git_operation(&wt)?;
        let clean = wt.is_clean()?;
        let moved = at.as_deref() == Some(head.as_str());
        let at_base = at.as_deref() == Some(t.base_commit.as_str());
        if !on_branch || unfinished.is_some() || !clean || !(at_base || moved) {
            let mut why = Vec::new();
            if !on_branch {
                why.push(format!("HEAD is not on {}", t.branch));
            }
            if let Some(op) = &unfinished {
                why.push(format!("an unfinished Git operation ({op})"));
            }
            if !clean {
                let paths: Vec<String> = wt.changed()?.into_iter().map(|x| x.1).take(10).collect();
                why.push(format!("the worktree is not clean ({paths:?})"));
            }
            if !(at_base || moved) {
                why.push(format!(
                    "HEAD is at {}, not at the base {}",
                    at.as_deref().map(short).unwrap_or("?"),
                    short(&t.base_commit)
                ));
            }
            // X60: a machine stop — a plain `resume` tries the promotion again once the owner put
            // the task worktree back (a `--note` would start a fix cycle on the base commit)
            let reason = format!(
                "{CANDIDATE_UNAVAILABLE}: the winner {} ({}) cannot be promoted — in the task worktree {}: {}; nothing was moved. Put the task worktree back by hand (clean, at the base {}, on {}: `git -C \"{}\" checkout {}`), then `toryo resume {}`",
                cand::id(k),
                w.profile,
                t.worktree.display(),
                why.join("; "),
                short(&t.base_commit),
                t.branch,
                t.worktree.display(),
                t.branch,
                t.id
            );
            self.escalate(CANDIDATE_UNAVAILABLE, reason)?;
            return Ok(Flow::Next);
        }
        if !moved {
            // exactly the winner's commit: the branch moves, no new commit is made
            self.g
                .run(&wt.loc, false, &["reset", "-q", "--hard", &head])?;
        }
        crash_point("fanout:after_promote_ref");
        // Q2 (X56): no candidate folder is needed any more — the losers' (again: a crash after
        // the selection left them) and the winner's; every branch stays
        for c in &rows {
            self.remove_candidate_worktree(t, c)?;
        }
        // what the review reads, as after a checkpoint: base → checkpoint (and no increment yet)
        let full = self
            .g
            .canonical_diff(&wt.loc, &t.base_commit, Some(&head))?;
        crate::engine::artifacts::put(
            self.store,
            t,
            None,
            "diff",
            "patch",
            "diff",
            &full,
            Some("DIFF.patch"),
        )?;
        crate::engine::artifacts::put(
            self.store,
            t,
            None,
            "diff-incremental",
            "patch",
            "diff-incremental",
            &[],
            Some("DIFF-incremental.patch"),
        )?;
        // the winner failed a gate: its verify result is the task's, for the fix turn
        if w.verify_pass != Some(true) {
            if let Some((_, b)) = crate::engine::artifacts::read_head(
                self.store,
                t,
                &format!("verify-result#{}", cand::id(k)),
            )? {
                crate::engine::artifacts::put(
                    self.store,
                    t,
                    None,
                    "verify-result",
                    "json",
                    "verify_result",
                    &b,
                    Some("VERIFY_RESULT.json"),
                )?;
            }
        }
        // R35: an allowed protected change in the winner's commit becomes the baseline (as a
        // checkpoint does)
        let mut ck = protect::check(self.store, t, &self.g, &self.protect)?;
        let allowed: Vec<String> = ck
            .changes
            .iter()
            .filter(|x| !x.is_hard() && self.protect.is_allowed(&x.path))
            .map(|x| x.path.clone())
            .collect();
        if !allowed.is_empty() && ck.hard().is_empty() {
            ck.take_baseline(
                self.store,
                t,
                &self.g,
                &self.protect,
                &format!("promotion of candidate {} ({}): allowed protected change {allowed:?} (TASK.md)", cand::id(k), short(&head)),
            )?;
        }
        let context = {
            let (i, stage) = (self.id.clone(), cand::stage(k));
            self.store.call(move |tx| {
                Ok(store::attempts(tx, &i)?
                    .into_iter()
                    .rev()
                    .find(|a| a.stage == stage && a.status == AttemptStatus::Succeeded)
                    .and_then(|a| a.context_sha256))
            })?
        };
        let mut chain = vec![w.profile.clone()];
        chain.extend(t.implementers.iter().filter(|p| **p != w.profile).cloned());
        let (i, profile, base, h) = (
            self.id.clone(),
            w.profile.clone(),
            t.base_commit.clone(),
            head.clone(),
        );
        let failed = w.verify_pass != Some(true);
        let feedback = w.feedback.clone();
        let from = self.task()?.state;
        let steps = self.store.call(move |tx| {
            let a = store::begin_attempt(
                tx,
                &i,
                &NewAttempt {
                    stage: "implement",
                    kind: "agent",
                    profile: Some(&profile),
                    fallback_from: None,
                    head_before: Some(&base),
                    subject_commit: None,
                },
            )?;
            if let Some(sha) = &context {
                store::set_attempt_context(tx, &a.id, sha)?;
            }
            store::finish_attempt(
                tx,
                &a.id,
                AttemptStatus::Succeeded,
                &AttemptEnd {
                    head_after: Some(h.clone()),
                    checkpoint_kind: Some("normal".into()),
                    ..Default::default()
                },
            )?;
            store::set_implementers(tx, &i, &chain, 0)?;
            store::set_fanout_phase(tx, &i, phase::PROMOTED)?;
            let mut steps = Vec::new();
            let mut st = from;
            for e in [Ev::ImplementationDone, Ev::Checkpointed { head: h.clone() }] {
                let name = e.name();
                let m = store::transition(tx, &i, e)?;
                steps.push((st, name, m.clone()));
                st = m.state();
            }
            if failed {
                let v = store::begin_attempt(
                    tx,
                    &i,
                    &NewAttempt {
                        stage: "verify",
                        kind: "command",
                        profile: None,
                        fallback_from: None,
                        head_before: Some(&h),
                        subject_commit: Some(&h),
                    },
                )?;
                store::finish_attempt(
                    tx,
                    &v.id,
                    AttemptStatus::Failed,
                    &AttemptEnd {
                        error_detail: Some(format!("the check gates of candidate c{k}")),
                        ..Default::default()
                    },
                )?;
                store::set_feedback(tx, &i, feedback.as_deref())?;
                let e = Ev::VerifyFailed { head: h.clone() };
                let name = e.name();
                let m = store::transition(tx, &i, e)?;
                steps.push((st, name, m));
            }
            Ok(steps)
        })?;
        crash_point("fanout:after_promote");
        self.rec.emit(EngineEvent::CheckpointCreated {
            head: head.clone(),
            kind: format!("candidate {}", cand::id(k)),
            found: moved,
        });
        for (from, name, m) in steps {
            self.rec.transition(from, name, &m, true, true);
        }
        Ok(Flow::Next)
    }
}

/// How the compare turn ended.
enum Compared {
    Winner(u32, String),
    /// Fall to the score (`why`).
    Score(String),
    /// The stage ended (a stop, the budget, an escalation).
    Ended(Flow),
}
