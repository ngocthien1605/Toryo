//! The gate decisions of a task (R23-27, moved out of `runner.rs` as they were): approving
//! and rejecting the open gate (`toryo approve` / `reject` and `--yes`), the §11
//! `approval_mismatch` of the final gate, the verify result in force, the next action of every
//! state, and SUMMARY.md.

use super::*;

/// Approve the open gate after re-checking its subject (R32): the plan on disk must still
/// hash to the approved digest; the final gate's checkpoint must still be the task branch
/// (§11 `approval_mismatch` → `Escalated`, EC-11) and still have a current passing verify
/// result (else the gate is withdrawn and the checkpoint verified again, EC-10). Uncommitted
/// changes in the worktree (an IDE's `.vs/`, `.idea/`) are not part of the checkpoint: a
/// one-line note, not a refusal (owner decision, F6). A refused approval is an error:
/// nothing was approved.
pub fn approve_now(
    store: &Store,
    rec: &Recorder,
    t: &TaskRow,
    kind: GateKind,
    expect: Option<&str>,
) -> Result<()> {
    let gate = t
        .machine
        .gate()
        .filter(|g| g.kind == kind)
        .with_context(|| format!("{}: no open {kind:?} gate (state {:?})", t.id, t.state))?
        .clone();
    if let Some(e) = expect {
        let digest = gate_digest(store, &t.id, kind, gate.occurrence)?;
        if !digest.starts_with(e) {
            bail!("subject_changed: the gate subject digest is {} — it does not start with --expect {e} (R32)", short(&digest));
        }
    }
    let e = match kind {
        GateKind::Plan => {
            let (_, bytes) = crate::engine::artifacts::read_head(store, t, "plan")?
                .context("no registered plan")?;
            if sha256_hex(&bytes) != gate.subject {
                bail!("subject_changed: plan.json no longer matches the plan under approval (R32)");
            }
            Ev::PlanApproved {
                occurrence: gate.occurrence,
                subject: gate.subject.clone(),
            }
        }
        GateKind::Final => {
            let g = task_git(&frozen_config(t)?, &t.repo)?;
            if let Some(reason) = approval_mismatch(&g, t, &gate.subject)? {
                withdraw_final(
                    store,
                    rec,
                    t,
                    Ev::Escalate {
                        reason: reason.clone(),
                        class: None,
                    },
                )?;
                bail!("{reason}");
            }
            if !verify_current(store, t)? {
                withdraw_final(store, rec, t, Ev::Reverify)?;
                bail!(
                    "{}: checkpoint {} has no current passing verify result (e.g. the verify configuration was reloaded) — nothing was approved; the final gate was withdrawn: `toryo run {}` verifies the checkpoint again (R29)",
                    t.id,
                    short(&gate.subject),
                    t.id
                );
            }
            if let Some(note) = uncommitted_note(&worktree_changes(&g, t), &gate.subject) {
                crate::errln!("{note}");
            }
            Ev::FinalApproved {
                occurrence: gate.occurrence,
                subject: gate.subject.clone(),
            }
        }
    };
    let (i, name) = (t.id.clone(), e.name());
    let m = store.call(move |tx| store::transition(tx, &i, e))?;
    rec.transition(t.state, name, &m, true, false);
    let digest = gate_digest(store, &t.id, kind, gate.occurrence).unwrap_or_default();
    crate::engine::decisions::append(
        store,
        t,
        &format!("{} gate approved", gate_name(kind)),
        &format!(
            "occurrence {} · subject {} · digest {} · by {}",
            gate.occurrence,
            &gate.subject[..12.min(gate.subject.len())],
            &digest[..12.min(digest.len())],
            store::whoami()
        ),
    )?;
    Ok(())
}

/// Reject the open gate with a reason; the revision opens a new occurrence (R44a).
pub fn reject_now(
    store: &Store,
    rec: &Recorder,
    t: &TaskRow,
    kind: GateKind,
    reason: &str,
) -> Result<()> {
    let gate = t
        .machine
        .gate()
        .filter(|g| g.kind == kind)
        .with_context(|| format!("{}: no open {kind:?} gate (state {:?})", t.id, t.state))?
        .clone();
    let (i, r) = (t.id.clone(), reason.to_string());
    let m = store.call(move |tx| {
        let (e, fb) = match kind {
            GateKind::Plan => (
                Ev::PlanRejected { occurrence: gate.occurrence, reason: r.clone() },
                format!("The human reviewer rejected the plan. Reason: {r}\nCreate the plan again, revised to address this."),
            ),
            GateKind::Final => (Ev::FinalRejected { occurrence: gate.occurrence, reason: r.clone() }, format!("- HUMAN (final gate): {r}\n")),
        };
        store::set_feedback(tx, &i, Some(&fb))?;
        Ok((e.name(), store::transition(tx, &i, e)?))
    })?;
    let (name, m) = m;
    // R7: a rejection at the fix-cycle bound escalates: its reason is printed like every other
    rec.transition(t.state, name, &m, true, m.state() == St::Escalated);
    crate::engine::decisions::append(
        store,
        t,
        &format!("{} gate rejected", gate_name(kind)),
        &format!(
            "occurrence {} · subject {} · by {}\n\nReason: {reason}",
            gate.occurrence,
            &gate.subject[..12.min(gate.subject.len())],
            store::whoami()
        ),
    )?;
    Ok(())
}

/// The class of the escalation that withdraws the final gate (§11, R32).
pub const APPROVAL_MISMATCH: &str =
    crate::engine::state::EscalationClass::ApprovalMismatch.as_str();

/// R23-03: before `approve --gate final --force` opens the final gate — the task branch must
/// point at the checkpoint, as for an approval; otherwise the reason (and nothing is opened).
pub fn force_final_mismatch(t: &TaskRow) -> Result<Option<String>> {
    let Some(checkpoint) = t.machine.checkpoint() else {
        return Ok(None);
    };
    let g = task_git(&frozen_config(t)?, &t.repo)?;
    approval_mismatch(&g, t, checkpoint)
}

/// §11 `approval_mismatch` (R32, EC-11) of the final gate: the task branch must still point
/// at the approved checkpoint — what the human merges is what was verified and reviewed.
/// Returns the escalation reason.
pub(super) fn approval_mismatch(
    g: &GitCmd,
    t: &TaskRow,
    checkpoint: &str,
) -> Result<Option<String>> {
    let branch = g.output(
        &crate::git::Loc::Discover(t.repo.clone()),
        true,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{}^{{commit}}", t.branch),
        ],
    )?;
    let at = String::from_utf8_lossy(&branch.stdout).trim().to_string();
    Ok(mismatch_reason(&t.branch, &at, checkpoint))
}

/// The pure half of [`approval_mismatch`]: the branch is at `at` (empty: gone), the approved
/// checkpoint is `checkpoint`.
pub(super) fn mismatch_reason(branch: &str, at: &str, checkpoint: &str) -> Option<String> {
    (at != checkpoint).then(|| {
        let now = if at.is_empty() {
            "gone".to_string()
        } else {
            format!("at {}", short(at))
        };
        format!("approval_mismatch: the branch {branch} is {now}, not at the approved checkpoint {} — nothing was approved (R32)", short(checkpoint))
    })
}

/// The uncommitted paths in the task worktree at the final approval (best effort: a
/// worktree that cannot be read has none to note).
pub(super) fn worktree_changes(g: &GitCmd, t: &TaskRow) -> Vec<String> {
    Worktree::new(g, &t.worktree)
        .changed()
        .map(|c| c.into_iter().map(|(_, p)| p).collect())
        .unwrap_or_default()
}

/// F6 (EC-11, owner decision): uncommitted changes at the final approval — an IDE folder
/// such as `.vs/` or `.idea/` in the worktree — are no `approval_mismatch`: they are not part
/// of the approved checkpoint, which is what the branch holds. One line, naming the first
/// few paths.
pub(super) fn uncommitted_note(changed: &[String], checkpoint: &str) -> Option<String> {
    if changed.is_empty() {
        return None;
    }
    const SHOWN: usize = 3;
    let mut paths = changed[..changed.len().min(SHOWN)].join(", ");
    if changed.len() > SHOWN {
        paths.push_str(&format!(", … ({} in all)", changed.len()));
    }
    Some(format!(
        "note: the worktree has uncommitted changes ({paths}) — they are not part of the approved checkpoint {} and stay uncommitted",
        short(checkpoint)
    ))
}

/// Withdraw the open final gate with `e` (`Escalate`, `Reverify`): the approval is refused.
pub(super) fn withdraw_final(store: &Store, rec: &Recorder, t: &TaskRow, e: Ev) -> Result<()> {
    let (i, name) = (t.id.clone(), e.name());
    // the only escalation that withdraws the final gate is §11 `approval_mismatch` (R32) — stored
    // on the machine (R23-03), so `next_action` names the way out instead of `--force`, which
    // would open the same gate again
    let class = matches!(e, Ev::Escalate { .. }).then_some(APPROVAL_MISMATCH);
    let m = store.call(move |tx| store::transition_classed(tx, &i, e, class))?;
    // the refusal is the command's error: the escalation reason is not printed twice
    rec.transition_classed(t.state, name, &m, true, false, class);
    Ok(())
}

/// The gate's name in approval rows, `[state]` lines and DECISIONS.md.
pub(super) fn gate_name(kind: GateKind) -> &'static str {
    kind.name()
}

pub fn gate_digest(store: &Store, task: &str, kind: GateKind, occurrence: u32) -> Result<String> {
    let t = task.to_string();
    let rows = store.call(move |tx| store::approvals(tx, &t))?;
    let g = gate_name(kind);
    rows.into_iter()
        .find(|r| r.gate == g && r.occurrence == occurrence)
        .map(|r| r.subject_digest)
        .context("no approval row for the open gate")
}

/// Is there a passing, hash-checked verify result for the current checkpoint (R29)?
pub fn verify_current(store: &Store, t: &TaskRow) -> Result<bool> {
    let Some((_, b)) = crate::engine::artifacts::read_head(store, t, "verify-result")? else {
        return Ok(false);
    };
    let v: Value = serde_json::from_slice(&b)?;
    Ok(v["pass"].as_bool() == Some(true)
        && v["invalidated"].as_bool() != Some(true)
        && v["head"].as_str().is_some()
        && v["head"].as_str() == t.machine.checkpoint())
}

/// R62: the launch was refused before any process started. R23-13: read from the class, or
/// from the start of the reason Toryo wrote (`PolicyRefusal: the … turn failed`, `implementer
/// failed: PolicyRefusal — …`) — never from anywhere in it: the detail after it is the CLI's
/// or the vendor's text.
pub(super) fn policy_refusal(class: Option<&str>, why: &str) -> bool {
    class == Some("policy_refusal")
        || why.starts_with("PolicyRefusal:")
        || why.starts_with("implementer failed: PolicyRefusal")
}

/// What the human does next, for `status` and the end of `run`.
pub fn next_action(t: &TaskRow) -> String {
    let id = &t.id;
    match t.state {
        St::WaitingPlanApproval => format!("review {}, then `toryo approve {id} --gate plan --continue` or `toryo reject {id} --gate plan --reason \"…\"`", t.artifacts().join("PLAN.md").display()),
        St::WaitingFinalApproval => format!("review the branch {} (REVIEW.md in the run store), then `toryo approve {id} --gate final` or `toryo reject {id} --gate final --reason \"…\"`", t.branch),
        St::Escalated => {
            let why = t.machine.reason().unwrap_or("");
            // R23-26: the stored class, typed; the reason's text only for a machine stored
            // before it had one
            use crate::engine::state::EscalationClass as C;
            let class = t.machine.reason_class().and_then(|c| c.parse::<C>().ok());
            if class == Some(C::EnvironmentGateConfirm) {
                // M13-09: the question before a deploy — nothing to fix; what to look at, how
                // to say yes, and how to say no
                format!(
                    "{why}\n  → look at the diff (`git -C \"{}\" show --stat {}`); yes: `toryo resume {id}`; no: `toryo resume {id} --note \"…\"` (one fix cycle) or `toryo task cleanup {id}`",
                    t.worktree.display(),
                    t.machine.checkpoint().map(short).unwrap_or("HEAD")
                )
            } else if class == Some(C::ApprovalMismatch) {
                // R23-03: `--force` would open the same gate, and the next approval would be
                // refused again — the branch has to be at the checkpoint first
                format!(
                    "{why}\n  → put the branch back at the checkpoint (`git -C \"{}\" reset --hard {}` — what came after it was never verified), then `toryo approve {id} --gate final --force`; or `toryo resume {id} --note \"…\"` (another fix cycle), or give up: `toryo task cleanup {id}`",
                    t.worktree.display(),
                    t.machine.checkpoint().map(short).unwrap_or("<checkpoint>")
                )
            } else if class.is_some_and(C::is_machine_caused) {
                // M11-R11 (X8): the machine caused it — plain resume goes back into the stage.
                // M11-R16: a fix in a config file is not used until the task is reloaded
                format!("{why}\n  → fix the cause, then `toryo resume {id}` (no fix cycle; the stage is checked again). A fix in a config file (the machine config, toryo.yaml, a profile) is used only after `toryo task config {id} --reload` — the task runs on its frozen configuration")
            } else if class == Some(C::ProtectedConfigChanged) || why.starts_with(protect::REASON) {
                format!("{why}\n  → restore the file(s) and `toryo resume {id}`, or `toryo resume {id} --accept-protected-change <path>` (recorded in DECISIONS.md)")
            } else if class == Some(C::RecoveryPolicyViolation)
                || why.starts_with(crate::engine::recovery::POLICY_VIOLATION)
            {
                format!("{why}\n  → `toryo resume {id} --discard-partial` (after a snapshot check) or fix the worktree by hand and `toryo resume {id} --keep-partial`")
            } else if policy_refusal(t.machine.reason_class(), why) {
                // R62: the launch was refused before any process started — a configuration
                // problem, which another turn with the frozen configuration cannot fix
                format!("{why}\n  → fix the profile in the configuration, `toryo task config {id} --reload`, then `toryo resume {id} --note \"…\"`; or give up: `toryo task cleanup {id}`")
            } else {
                format!("{why}\n  → `toryo resume {id} --note \"…\"` (another fix cycle), `toryo approve {id} --gate final --force`, or give up: `toryo task cleanup {id}`")
            }
        }
        St::Paused if t.machine.reason() == Some("budget") => format!(
            "paused (budget): {} of {} tokens used — `toryo budget set {id} --tokens N`, then `toryo resume {id}`",
            t.spent_tokens + t.reserved_tokens,
            t.budget_tokens.unwrap_or(0)
        ),
        St::Paused => format!("paused — {}\n  → `toryo resume {id}`", t.machine.reason().unwrap_or("")),
        St::Completed => format!("done — merge `{}` yourself (toryo never merges); `toryo task cleanup {id}` afterwards", t.branch),
        St::Cancelled => format!("cancelled — `toryo task cleanup {id}`"),
        St::Failed => format!("failed: {} — `toryo task cleanup {id}`", t.machine.reason().unwrap_or("")),
        s if s.is_running() => format!("`toryo run {id}` (or `toryo resume {id}` after a crash)"),
        _ => format!("`toryo run {id}`"),
    }
}

/// What the human does next with a finished task whose worktree and run store `task cleanup`
/// removed: only its branch is left.
pub fn cleaned_up_action(t: &TaskRow) -> String {
    let b = &t.branch;
    match t.state {
        St::Completed => format!("done, cleaned up — merge `{b}` yourself if you have not (toryo never merges); `git branch -D {b}` when you no longer need it"),
        St::Failed => format!("failed, cleaned up — `git branch -D {b}` when you no longer need it"),
        _ => format!("cancelled, cleaned up — `git branch -D {b}` when you no longer need it"),
    }
}

/// SUMMARY.md in the run store: state, reason, tokens, next action.
pub fn write_summary(store: &Store, t: &TaskRow) -> Result<PathBuf> {
    let i = t.id.clone();
    let atts = store.call(move |tx| store::attempts(tx, &i))?;
    let mut s = format!(
        "# {} — {:?}\n\n{}\n\n- branch: `{}` (base `{}`) in `{}`\n- fix cycles: {}\n- tokens: {} spent{}{}\n- next: {}\n\n## Stage attempts\n\n| attempt | status | profile | error | checkpoint |\n|---|---|---|---|---|\n",
        t.id,
        t.state,
        t.title,
        t.branch,
        &t.base_commit[..12.min(t.base_commit.len())],
        t.repo.display(),
        crate::engine::views::fix_cycles(t, " / "),
        t.spent_tokens,
        if t.reserved_tokens > 0 { format!(" + ~{} reserved (unknown usage, R41)", t.reserved_tokens) } else { String::new() },
        t.budget_tokens.map(|b| format!(" of {b} (admission cap, may overshoot by one turn)")).unwrap_or_default(),
        next_action(t)
    );
    for a in atts {
        s.push_str(&format!(
            "| {} | {:?} | {} | {} | {} |\n",
            a.id,
            a.status,
            a.profile.unwrap_or_default(),
            a.error_class.unwrap_or_default(),
            a.git_head_after
                .or(a.partial_commit.map(|p| format!("{p} (partial)")))
                .map(|h| h.chars().take(20).collect::<String>())
                .unwrap_or_default()
        ));
    }
    if t.state == St::WaitingFinalApproval {
        let notes = final_gate_notes(store, t).unwrap_or_default();
        if !notes.is_empty() {
            s.push_str(&format!("\n## Final gate\n\n{}\n", notes.join("\n\n")));
        }
    }
    let p = t.artifacts().join("SUMMARY.md");
    std::fs::write(&p, s)?;
    Ok(p)
}
