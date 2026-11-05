//! The anchor (X24, X31; R23-27, moved as it was): where a writing attempt and a fresh checkpoint
//! start, and the HEAD guard after a turn.

use super::*;

impl<'a> Engine<'a> {
    /// M13-01 (X24): a writing attempt and a fresh checkpoint start at the anchor. HEAD at another
    /// commit is never trusted — and who made that commit is never asked. When HEAD descends
    /// from the anchor on the task branch it is soft-reset to the anchor: the changes of the
    /// commits stay in the tree and go through the checks every turn's changes get. Otherwise
    /// (another branch, a detached HEAD, an unfinished operation, a history that does not
    /// descend from the anchor) nothing is reset and the task escalates. `false`: escalated.
    pub(super) fn start_at_anchor(
        &self,
        t: &TaskRow,
        wt: &Worktree,
        fresh_checkpoint: bool,
    ) -> Result<bool> {
        let i = self.id.clone();
        let atts = self.store.call(move |tx| store::attempts(tx, &i))?;
        // M13-R4 (X32): what a read-only role wrote while HEAD was off the task branch could not
        // be put back then; the branch is back now, so it is first — before the anchor test
        if let Some(reason) = finish_pending_undo(self.store, t, &atts, wt)? {
            crate::errln!("note: the read-only role's earlier write is undone now: {reason}");
        }
        let anchor = anchor_of(t, &atts);
        let head = wt.head().ok();
        let on_branch = wt.head_ref()? == Some(format!("refs/heads/{}", t.branch));
        let unfinished = wt.unfinished_operation()?;
        // M13-R5 (X31): for a fresh checkpoint HEAD at the anchor is not enough — it must be on
        // the task branch with no Git operation unfinished (a detached HEAD checkpoints off the
        // branch; `git commit` during a merge makes a merge commit whose second parent is
        // whatever MERGE_HEAD names). At the start of a writing attempt the HEAD guard after the
        // turn judges that state, as before (the ec18 tests: a state broken before the turn is
        // escalated after it): there it only costs a turn, here it would cost the history.
        let at_anchor = head.as_deref() == Some(anchor.as_str());
        if at_anchor && (!fresh_checkpoint || (on_branch && unfinished.is_none())) {
            return Ok(true);
        }
        let at = head.as_deref().map(short).unwrap_or("?").to_string();
        let descends = head.is_some() && wt.contains(&anchor)?;
        if on_branch && descends && unfinished.is_none() {
            wt.soft_reset(&anchor)?;
            crate::errln!(
                "note: commits {}..{at} on top of the anchor were undone; their changes stay in the tree and are checked like any turn's",
                short(&anchor)
            );
            let (i, payload) = (
                self.id.clone(),
                json!({"kind": "commits on the anchor", "action": "soft reset", "head": head, "reset_to": anchor}),
            );
            self.store
                .call(move |tx| store::event(tx, Some(&i), None, "policy_violation", &payload))?;
            return Ok(true);
        }
        let why = match (&unfinished, on_branch) {
            (Some(op), _) => format!("an unfinished Git operation ({op}) is in the task worktree"),
            (None, false) => format!("HEAD is not on the task branch {}", t.branch),
            (None, true) => "it does not descend from that commit".to_string(),
        };
        let w = t.worktree.display();
        let first = if unfinished.is_some() {
            "finish or abort that operation, then "
        } else {
            ""
        };
        let place = if at_anchor {
            format!("HEAD is at {at}, the last commit Toryo recorded for this task, but {why}")
        } else {
            format!(
                "HEAD is at {at}, not at the last commit Toryo recorded for this task ({}), and {why}",
                short(&anchor)
            )
        };
        let reason = format!(
            "policy_violation: {place}; nothing was reset or committed — put the task branch back by hand ({first}`git -C \"{w}\" checkout {}`, then `git -C \"{w}\" reset --soft {}`: what the branch held stays in the tree and is checked), then `toryo resume {} --note \"…\"`",
            t.branch,
            short(&anchor),
            t.id
        );
        self.escalate("policy_violation", reason)?;
        Ok(false)
    }

    /// HEAD guard (§8; R12, R25) after every Implementer turn, whatever its end (EC-18):
    /// agents must not commit. An agent commit on the task branch is soft-reset to where the
    /// turn began — its changes stay, uncommitted, so the path policy and the deny check
    /// before the checkpoint see them. An unfinished Git operation, another branch, a
    /// detached or unresolvable HEAD, or history rewritten below the base cannot be repaired
    /// that way (a reset would move another branch): nothing is reset and the escalation
    /// reason is returned. Both are recorded as a `policy_violation` event.
    pub(super) fn head_guard(
        &self,
        t: &TaskRow,
        wt: &Worktree,
        run: &ImplementRun,
    ) -> Result<Option<String>> {
        let (payload, escalation) = match HeadGuard::check(
            wt,
            &t.branch,
            &t.base_commit,
            &run.head_before,
        )? {
            HeadGuard::Unchanged => return Ok(None),
            HeadGuard::Reset { head } => {
                crate::errln!(
                    "!! policy_violation: the agent moved HEAD — soft reset to {}",
                    short(&run.head_before)
                );
                wt.soft_reset(&run.head_before)?;
                let payload = json!({"kind": "agent commit", "action": "soft reset", "head": head, "reset_to": run.head_before});
                (payload, None)
            }
            HeadGuard::Violation { kind, reason } => (
                json!({"kind": kind, "action": "escalate", "reason": reason}),
                Some(reason),
            ),
        };
        let (i, aid) = (self.id.clone(), run.attempt.id.clone());
        self.store
            .call(move |tx| store::event(tx, Some(&i), Some(&aid), "policy_violation", &payload))?;
        Ok(escalation)
    }
}

/// R25: an unfinished Git operation in the task worktree — a marker of
/// `Worktree::unfinished_operation`, the `sequencer` of a multi-commit cherry-pick or revert,
/// or unmerged paths (a conflicting `git stash pop` leaves no marker).
pub(super) fn unfinished_git_operation(wt: &Worktree) -> Result<Option<String>> {
    if let Some(op) = wt.unfinished_operation()? {
        return Ok(Some(op));
    }
    if wt.git_dir()?.join("sequencer").exists() {
        return Ok(Some("sequencer".into()));
    }
    let unmerged = wt.unmerged_paths()?;
    Ok((!unmerged.is_empty()).then(|| format!("unmerged paths {unmerged:?}")))
}

/// What the HEAD guard reads after an Implementer turn (§8; R12, R25).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GitAfterTurn {
    /// An unfinished merge, rebase, cherry-pick, revert or bisect, or unmerged paths.
    pub(super) unfinished: Option<String>,
    /// What HEAD points at (`refs/heads/…`); `None` when it is detached.
    pub(super) head_ref: Option<String>,
    /// The commit HEAD resolves to; `None` when it does not resolve.
    pub(super) head: Option<String>,
    /// The task's base commit is an ancestor of HEAD (only read when HEAD moved).
    pub(super) descends_from_base: bool,
}

impl GitAfterTurn {
    /// Read the task worktree, against `head_before`, where the turn began.
    pub(super) fn read(wt: &Worktree, base: &str, head_before: &str) -> Result<Self> {
        let unfinished = unfinished_git_operation(wt)?;
        let head_ref = wt.head_ref()?;
        let head = wt.head().ok();
        let moved = head.as_deref().is_some_and(|h| h != head_before);
        Ok(Self {
            unfinished,
            head_ref,
            head,
            descends_from_base: !moved || wt.contains(base)?,
        })
    }
}

/// M13-01 (X24): the anchor of a task — the newest attempt's checkpoint (`git_head_after`) or
/// recovery checkpoint (`partial_commit`), else the task's base commit. M17: a best-of-N
/// candidate's attempts are in its own worktree and never the task's anchor; the promotion's
/// attempt (`implement`, at the winner's checkpoint) is.
pub(in crate::engine) fn anchor_of(t: &TaskRow, attempts: &[AttemptRow]) -> String {
    attempts
        .iter()
        .rev()
        .filter(|a| !crate::engine::candidates::is_candidate_stage(&a.stage))
        .find_map(|a| {
            a.git_head_after
                .clone()
                .or_else(|| a.partial_commit.clone())
        })
        .unwrap_or_else(|| t.base_commit.clone())
}

/// What the HEAD guard does about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::engine) enum HeadGuard {
    /// HEAD is where the turn began, on the task branch.
    Unchanged,
    /// The agent committed on the task branch: soft reset to where the turn began.
    Reset { head: String },
    /// Not repairable by a soft reset: the stage fails, the task escalates (`reason`).
    Violation { kind: &'static str, reason: String },
}

impl HeadGuard {
    /// The guard on the task worktree `wt` of the task branch `branch` (base `base`) for a
    /// turn that began at `head_before` — after an Implementer turn, and in recovery before
    /// anything is reset or committed (F4).
    pub(in crate::engine) fn check(
        wt: &Worktree,
        branch: &str,
        base: &str,
        head_before: &str,
    ) -> Result<Self> {
        let git = GitAfterTurn::read(wt, base, head_before)?;
        Ok(Self::of(&git, branch, base, head_before))
    }

    /// The guard for recovery (F4): the same, except that an unfinished operation is left to
    /// recovery's own path — snapshot first, `recovery_policy_violation`, so that
    /// `resume --discard-partial` can clear it — while another branch, a detached or
    /// unresolvable HEAD and rewritten history still escalate before anything is touched.
    pub(in crate::engine) fn check_refs(
        wt: &Worktree,
        branch: &str,
        base: &str,
        head_before: &str,
    ) -> Result<Self> {
        let mut git = GitAfterTurn::read(wt, base, head_before)?;
        git.unfinished = None;
        Ok(Self::of(&git, branch, base, head_before))
    }

    /// §8, in this order: an unfinished operation (R25); HEAD on another branch or detached
    /// (R12: a reset would move that branch, not the task's); a HEAD that does not resolve;
    /// history rewritten below the base (R25); then a moved HEAD is reset.
    pub(super) fn of(g: &GitAfterTurn, branch: &str, base: &str, head_before: &str) -> Self {
        let violation = |kind, what: String, fix: &str| {
            Self::Violation {
            kind,
            reason: format!(
                "policy_violation: after the Implementer turn {what}; nothing was reset or committed{fix}"
            ),
        }
        };
        // M13-01 (X24): the way out must not walk the owner past the check — the commits an
        // agent left on the branch are undone into the tree at the next writing attempt
        let checkout = format!(
            " — check out {branch} again; `toryo resume {} --note \"…\"` then undoes the commits made since {} into the tree and checks them",
            branch.strip_prefix("toryo/").unwrap_or("<id>"),
            short(head_before)
        );
        // (HEAD on another branch: the task branch was not touched — when the owner checks it
        // out again HEAD is at the anchor and nothing is undone; M13-R8)
        let other = format!(
            " — check out {branch} again; `toryo resume {} --note \"…\"` then starts at {} (what was committed on the other branch stays there)",
            branch.strip_prefix("toryo/").unwrap_or("<id>"),
            short(head_before)
        );
        if let Some(op) = &g.unfinished {
            return violation(
                "unfinished git operation",
                format!("an unfinished Git operation ({op}) is in the task worktree (R25)"),
                " — finish or abort it first",
            );
        }
        match &g.head_ref {
            Some(r) if *r == format!("refs/heads/{branch}") => {}
            Some(r) => {
                return violation(
                    "branch changed",
                    format!("HEAD is on {r}, not on the task branch {branch} (R12)"),
                    &other,
                )
            }
            None => {
                let at = g.head.as_deref().map(short).unwrap_or("?");
                return violation(
                    "detached HEAD",
                    format!("HEAD is detached at {at}, not on the task branch {branch} (R12)"),
                    &checkout,
                );
            }
        }
        let Some(head) = &g.head else {
            return violation(
                "unresolved HEAD",
                format!("the task branch {branch} does not resolve to a commit"),
                "",
            );
        };
        if head == head_before {
            return Self::Unchanged;
        }
        if !g.descends_from_base {
            return violation(
                "history rewritten",
                format!(
                    "HEAD {} no longer descends from the task base {} (history rewritten, R25)",
                    short(head),
                    short(base)
                ),
                "",
            );
        }
        Self::Reset { head: head.clone() }
    }
}
