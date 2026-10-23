//! The Checkpointing stage (R23-27, moved as it was): idempotent through the trailer search (R40),
//! the same path checks for a found and a fresh commit (X25).

use super::*;

impl<'a> Engine<'a> {
    pub(super) fn checkpoint(&mut self, t: &TaskRow) -> Result<Flow> {
        let stage = writing_stage(t.fix_cycle);
        let wt = Worktree::new(&self.g, &t.worktree);
        let a = {
            let (i, s) = (self.id.clone(), stage.clone());
            self.store.call(move |tx| store::last_attempt(tx, &i, &s))?
        }
        .with_context(|| format!("no `{stage}` attempt to checkpoint"))?;
        let implementer = a.profile.clone().unwrap_or_default();
        let (head, kind, found) = match wt.find_checkpoint(
            &t.base_commit,
            &t.id,
            &stage,
            a.attempt,
        )? {
            Some(c) if !c.partial => {
                // M12-01 (H1): a commit found by its trailers is taken only when it is HEAD,
                // its first parent is a commit Toryo recorded for this task, and its diff
                // passes the same path policy a fresh commit gets — who made it is never asked
                let why = crate::engine::recovery::foreign_commit(
                    &wt,
                    &self.protect,
                    a.git_head_before.as_deref(),
                    &c.sha,
                )?;
                if let Some(why) = why {
                    // undone like an agent commit: the changes stay in the tree, HEAD is where
                    // the attempt began (the next turn's policy checks see them). M13-02: also
                    // when the commit lies under further commits ("it is not HEAD") — the
                    // whole stack is undone, or every resume would find and refuse it again.
                    // Where a soft reset cannot repair it, the reason says what to do by hand.
                    let head = wt.head()?;
                    let on_branch = wt.head_ref()? == Some(format!("refs/heads/{}", t.branch));
                    let undone = match &a.git_head_before {
                        Some(before)
                            if on_branch
                                && wt.contains(before)?
                                && wt.unfinished_operation()?.is_none() =>
                        {
                            wt.soft_reset(before)?;
                            format!(
                                "; {} undone (soft reset to {}): the changes stay in the tree and the next turn's checks see them — `toryo resume {} --note \"…\"`",
                                if head == c.sha {
                                    "the commit was".to_string()
                                } else {
                                    format!("it and the commits on top of it (HEAD was {}) were", short(&head))
                                },
                                short(before),
                                t.id
                            )
                        }
                        before => format!(
                            "; nothing was reset (HEAD is {}{}) — put the task branch back by hand (after any unfinished Git operation is finished or aborted: `git -C \"{}\" checkout {}`, then `git -C \"{}\" reset --soft {}`), then `toryo resume {} --note \"…\"`",
                            short(&head),
                            if on_branch { "" } else { ", not on the task branch" },
                            t.worktree.display(),
                            t.branch,
                            t.worktree.display(),
                            before.as_deref().map(short).unwrap_or("<the commit the attempt began at>"),
                            t.id
                        ),
                    };
                    let reason = format!("policy_violation: commit {} carries the trailers of this checkpoint but is not Toryo's ({why}); nothing was accepted{undone}", short(&c.sha));
                    self.escalate("policy_violation", reason)?;
                    return Ok(Flow::Next);
                }
                (
                    c.sha,
                    if c.no_change { "no_change" } else { "normal" },
                    true,
                )
            }
            _ => {
                // M13-01 (X24): a fresh checkpoint is made on the anchor, nowhere else
                if !self.start_at_anchor(t, &wt, true)? {
                    return Ok(Flow::Next);
                }
                // §8, M13-02 (X25): the path policy is asked again right before the commit —
                // the same question a found commit gets (`violates`: protected, denied, the
                // write fence), not `deny_paths` alone. What the turn's own checks saw can have
                // changed since: the stage is entered again after a crash or an escalation.
                let mut bad: Vec<String> = wt
                    .changed()?
                    .into_iter()
                    .map(|c| c.1)
                    .filter(|p| self.protect.violates(p))
                    .collect();
                bad.sort();
                bad.dedup();
                if !bad.is_empty() {
                    let reason = format!(
                        "policy_violation: {} would be committed — the path policy forbids it; nothing was committed. `toryo resume {} --note \"…\"` gives the implementer a turn whose checks put them back (or restore them by hand first)",
                        self.protect.violation_words(&bad),
                        t.id
                    );
                    self.escalate("policy_violation", reason)?;
                    return Ok(Flow::Next);
                }
                let no_change = wt.nothing_to_commit()?;
                let mut tr = vec![
                    (T_TASK, t.id.clone()),
                    (T_STAGE, stage.clone()),
                    (T_ATTEMPT, a.attempt.to_string()),
                    (T_AGENT, implementer.clone()),
                ];
                if no_change {
                    tr.push((T_NO_CHANGE, "true".into()));
                }
                let sha = wt.commit(&format!("toryo({}): {stage}", t.id), &tr, no_change)?;
                crash_point("checkpoint:after_commit");
                (sha, if no_change { "no_change" } else { "normal" }, false)
            }
        };
        let full = self
            .g
            .canonical_diff(&wt.loc, &t.base_commit, Some(&head))?;
        self.put(t, &a, "diff", "patch", &full, "DIFF.patch")?;
        let inc = match &t.last_reviewed_head {
            Some(h) => self.g.canonical_diff(&wt.loc, h, Some(&head))?,
            None => Vec::new(),
        };
        self.put(
            t,
            &a,
            "diff-incremental",
            "patch",
            &inc,
            "DIFF-incremental.patch",
        )?;
        self.rec.emit(EngineEvent::CheckpointCreated {
            head: head.clone(),
            kind: kind.to_string(),
            found,
        });
        // R35: a committed tracked protected change allowed by TASK.md becomes the new baseline
        let c = protect::check(self.store, t, &self.g, &self.protect)?;
        let allowed: Vec<String> = c
            .changes
            .iter()
            .filter(|x| !x.is_hard() && self.protect.is_allowed(&x.path))
            .map(|x| x.path.clone())
            .collect();
        if !allowed.is_empty() && c.hard().is_empty() {
            protect::take_baseline(
                self.store,
                t,
                &self.g,
                &self.protect,
                &format!(
                    "checkpoint {}: allowed protected change {allowed:?} (TASK.md)",
                    short(&head)
                ),
            )?;
            crate::errln!("   protected baseline renewed: {allowed:?} (## Allow protected)");
        }
        self.finish_and(
            &a,
            AttemptStatus::Succeeded,
            AttemptEnd {
                head_after: Some(head.clone()),
                checkpoint_kind: Some(kind.to_string()),
                ..Default::default()
            },
            None,
            Some(Ev::Checkpointed { head }),
        )?;
        Ok(Flow::Next)
    }
}
