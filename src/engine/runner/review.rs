//! The Reviewing stage (R23-27, moved as it was): the review turn and its correction turns (R22),
//! the decision of a valid review, the final gate's notes (R53) and the same-vendor check.

use super::*;

impl<'a> Engine<'a> {
    pub(super) fn review(&mut self, t: &TaskRow) -> Result<Flow> {
        let head = t
            .machine
            .checkpoint()
            .context("no checkpoint to review")?
            .to_string();
        // EC-10 (R29): the review starts only on a current passing verify result of this
        // checkpoint — a `task config --reload` that changed the verify configuration while
        // the task waited (Paused, Escalated) invalidated it: verify again first
        if self.verify_stale(t)? {
            crate::errln!("!! checkpoint {} has no current passing verify result (e.g. the verify configuration was reloaded) — it is verified again before the review (R29)", short(&head));
            self.apply(Ev::Reverify)?;
            return Ok(Flow::Next);
        }
        // M11-C3: `reviewer-differs-from-builder: error` — no review turn by the vendor that
        // built the checkpoint (the profile that actually ran, after any fallback)
        if reviewer_check(&self.cfg) == Level::Error {
            let imps = implementer_profiles(self.store, t)?;
            let same = same_vendor(&self.cfg, &imps, &t.reviewer);
            if !same.is_empty() {
                let kind = both_kind(&self.cfg, &t.reviewer);
                let reason = same_vendor_reason(&t.id, &same, &t.reviewer, &kind);
                self.escalate(SAME_VENDOR, reason)?;
                return Ok(Flow::Next);
            }
        }
        // R48: a review of the same checkpoint that was interrupted (crash, stop) resumes the
        // Reviewer's session
        let prev = {
            let i = self.id.clone();
            self.store
                .call(move |tx| store::last_attempt(tx, &i, "review"))?
        };
        let resume = review_resume(prev.as_ref(), &head, &t.reviewer);
        if !self.admitted()? || self.turn_blocked(t, "review", &t.reviewer)? {
            return Ok(Flow::Next);
        }
        let a = self.begin(NewAttempt {
            stage: "review",
            kind: "agent",
            profile: Some(&t.reviewer),
            fallback_from: None,
            head_before: Some(&head),
            subject_commit: Some(&head),
        })?;
        self.stage_started(
            &a,
            resume
                .is_some()
                .then(|| "resuming the interrupted session".to_string()),
        );
        crash_point("review:after_begin");
        let Some(ctx) = self.context(t, &a, &t.reviewer)? else {
            return Ok(Flow::Next);
        };
        let stop = self.lock.watch_stop();
        let prompt = review_prompt(&head, &t.acceptance_ids, resume.is_some());
        let fresh = review_prompt(&head, &t.acceptance_ids, false);
        match self.review_turns(t, &a, &ctx, &stop, prompt, resume, &fresh)? {
            ReviewTurns::Valid(valid) => self.decide_review(t, &a, &head, valid),
            ReviewTurns::Rejected { last, violations } => {
                // R23-22: what was still wrong, in the reason and the attempt (was: a count only)
                let reason = format!(
                    "InvalidOutput: the review was rejected {} times (R22); still wrong: {}",
                    MAX_CORRECTIONS + 1,
                    rejected_words(&violations)
                );
                self.end_and(
                    &a,
                    AttemptStatus::Failed,
                    &last,
                    AttemptEnd {
                        error_class: Some("InvalidOutput".into()),
                        error_detail: Some(violations.join("\n")),
                        correction_turns: Some(MAX_CORRECTIONS),
                        ..Default::default()
                    },
                    Some(Ev::Escalate { reason }),
                    None,
                )?;
                Ok(Flow::Next)
            }
            ReviewTurns::Failed {
                last,
                class,
                retries,
                corrections,
            } => self.read_only_failed(&a, &last, "reviewer", class, retries, corrections),
            ReviewTurns::Ended(flow) => Ok(flow),
        }
    }

    /// The review turn and up to two correction turns in the same session (R22): a review
    /// that fails its schema (EC-25) or the checks (R28) is sent back with the violations.
    /// Every turn is admitted (R41) and checked like any read-only turn. A failed turn never
    /// yields a review (EC-01): a crash, a network error, an unknown failure, a timeout or a
    /// context overflow runs again as plan §11 says for its class — resumed with the same
    /// prompt, or fresh with the review prompt `fresh` (F7) — bounded; only invalid output
    /// gets a correction turn (EC-14).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn review_turns(
        &self,
        t: &TaskRow,
        a: &AttemptRow,
        ctx: &Path,
        stop: &StopWatch,
        mut prompt: String,
        mut resume: Option<String>,
        fresh: &str,
    ) -> Result<ReviewTurns> {
        let mut last = StageOutcome::default();
        let (mut turn, mut corrections) = (0, 0);
        let mut retries = TurnRetries::default();
        loop {
            // R41: every turn is admitted, correction turns and retries too
            if turn > 0 && !self.admit_next_turn(a, &last, corrections)? {
                return Ok(ReviewTurns::Ended(Flow::Next));
            }
            let ask = Ask {
                prompt: &prompt,
                resume: resume.clone(),
                label: format!("review-{}-{turn}", a.attempt),
            };
            turn += 1;
            let r = match self.read_only_turn(t, a, Role::Reviewer, ctx, stop, ask)? {
                Ok(r) => r,
                Err(flow) => return Ok(ReviewTurns::Ended(flow)),
            };
            let viol = match ReadOnlyTurn::of(&r) {
                ReadOnlyTurn::Output(review) => {
                    let viol = review_problems(&review, &t.acceptance_ids);
                    if viol.is_empty() {
                        return Ok(ReviewTurns::Valid(ValidReview {
                            review,
                            last: r,
                            corrections,
                        }));
                    }
                    viol
                }
                ReadOnlyTurn::Invalid(why) => vec![why],
                ReadOnlyTurn::Retry(class) => match retries.next(class) {
                    Some(how) => {
                        crate::errln!(
                            "{}",
                            retry_note("reviewer", class, &r, how, retries.of(class))
                        );
                        // not a correction: resumed with the same prompt, or fresh (§11)
                        (prompt, resume) = how.ask(&r, &prompt, resume.as_deref(), fresh);
                        last = r;
                        continue;
                    }
                    None => {
                        return Ok(ReviewTurns::Failed {
                            last: r,
                            class,
                            retries: retries.total(),
                            corrections,
                        })
                    }
                },
                ReadOnlyTurn::Failed(class) => {
                    return Ok(ReviewTurns::Failed {
                        last: r,
                        class,
                        retries: retries.total(),
                        corrections,
                    });
                }
            };
            last = r;
            crate::errln!(
                "!! invalid review (correction {}/{MAX_CORRECTIONS}): {viol:?}",
                corrections + 1
            );
            if corrections == MAX_CORRECTIONS {
                return Ok(ReviewTurns::Rejected {
                    last,
                    violations: viol.clone(),
                });
            }
            corrections += 1;
            prompt = correction_prompt(&viol);
            resume = last.session_id.clone();
        }
    }

    /// Publish a valid review (review.json, REVIEW.md) and apply its decision: an
    /// inconsistent approval is downgraded to `changes_requested`, and an approval's
    /// non-blocking issues become follow-ups (R53).
    pub(super) fn decide_review(
        &self,
        t: &TaskRow,
        a: &AttemptRow,
        head: &str,
        valid: ValidReview,
    ) -> Result<Flow> {
        let ValidReview {
            review,
            last,
            corrections,
        } = valid;
        let md = crate::engine::render::render("review", &review)?;
        self.put(
            t,
            a,
            "review",
            "json",
            &serde_json::to_vec_pretty(&review)?,
            "review.json",
        )?;
        self.put(t, a, "review-md", "md", md.as_bytes(), "REVIEW.md")?;
        // the agent wrote it: inert, and never painted like a line of Toryo's own (M15)
        crate::term::quoted(crate::term::Stream::Err, &format!("\n{md}"));
        let verdict = ReviewVerdict::of(&review);
        if let Some(unmet) = &verdict.downgraded {
            crate::errln!("!! review_inconsistent: `approved` with unmet {unmet:?} / blocking issues → changes_requested");
        }
        let (i, h) = (self.id.clone(), head.to_string());
        self.store
            .call(move |tx| store::set_last_reviewed(tx, &i, &h))?;
        let feedback = (verdict.decision == ReviewDecision::ChangesRequested)
            .then(|| untrusted::wrap("review", None, &review_feedback(&review)));
        crash_point("review:before_decision");
        self.end_and(
            a,
            AttemptStatus::Succeeded,
            &last,
            AttemptEnd {
                correction_turns: Some(corrections),
                ..Default::default()
            },
            Some(verdict.decision.event(head)),
            feedback.map(Some),
        )?;
        // R53: non-blocking issues survive an approval as follow-ups. R23-10: after the
        // decision is recorded — DECISIONS.md is append-only, and a crash before the decision
        // must not leave "approved with these open issues" for a review the resumed reviewer
        // may answer differently (the final gate lists them from the review either way)
        if verdict.decision == ReviewDecision::Approved {
            let nb = non_blocking(&review);
            if !nb.is_empty() {
                crate::engine::decisions::append(
                    self.store,
                    t,
                    "follow-ups: non-blocking review issues (R53)",
                    &format!(
                        "Review of checkpoint {} approved with these open issues:\n{}",
                        short(head),
                        nb.join("\n")
                    ),
                )?;
            }
        }
        Ok(Flow::Next)
    }

    /// EC-10: is the verify result in force not a current pass for the checkpoint? A
    /// tampered or missing verify result is not decided here: the task pack's hash check
    /// escalates it when the stage starts (R34).
    pub(super) fn verify_stale(&self, t: &TaskRow) -> Result<bool> {
        match verify_current(self.store, t) {
            Ok(current) => Ok(!current),
            Err(e) if format!("{e:#}").starts_with("artifact_") => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// A review that passed the checks (R28).
pub(super) struct ValidReview {
    pub(super) review: Value,
    /// The turn that returned it.
    pub(super) last: StageOutcome,
    /// Correction turns it took (R22).
    pub(super) corrections: u32,
}

/// R23-22: the violations of a rejected review for its escalation reason — at most five, then
/// how many there are (the attempt's `error_detail` has them all).
pub(super) fn rejected_words(violations: &[String]) -> String {
    const SHOWN: usize = 5;
    let mut s = violations
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    if violations.len() > SHOWN {
        s.push_str(&format!("; … ({} in all)", violations.len()));
    }
    s
}

/// How the turns of a review attempt ended.
pub(super) enum ReviewTurns {
    Valid(ValidReview),
    /// Still invalid after the correction turns (R22); `last` is the final turn, `violations`
    /// what was still wrong with it (R23-22).
    Rejected {
        last: StageOutcome,
        violations: Vec<String>,
    },
    /// The last turn failed for good (EC-01, EC-14): at once, or after the retries.
    Failed {
        last: StageOutcome,
        class: ErrorClass,
        retries: u32,
        corrections: u32,
    },
    /// The attempt has already ended (budget, stop, quota, protected state or a policy
    /// violation).
    Ended(Flow),
}

/// Why a review returned by a successful turn cannot be used: the checks in code (R28), then
/// its schema (EC-25). Empty: valid.
pub(super) fn review_problems(review: &Value, acceptance_ids: &[String]) -> Vec<String> {
    let mut out = review_violations(review, acceptance_ids);
    if let Err(e) = contracts::validate("review.schema.json", review) {
        out.extend(e);
    }
    out
}

/// The decision of a valid review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReviewDecision {
    Approved,
    ChangesRequested,
    Blocked,
}

impl ReviewDecision {
    /// The review's transition for the checkpoint `head`.
    pub(super) fn event(self, head: &str) -> Ev {
        let head = head.to_string();
        match self {
            Self::Approved => Ev::ReviewApproved { head },
            Self::ChangesRequested => Ev::ChangesRequested { head },
            Self::Blocked => Ev::ReviewBlocked { head },
        }
    }
}

/// What a valid review decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReviewVerdict {
    pub(super) decision: ReviewDecision,
    /// `approved` with an unmet criterion or a blocking issue is inconsistent and becomes
    /// `changes_requested`; then these are the unmet criteria (possibly none).
    pub(super) downgraded: Option<Vec<String>>,
}

impl ReviewVerdict {
    pub(super) fn of(review: &Value) -> Self {
        let unmet: Vec<String> = review["acceptance_criteria"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|x| x["met"].as_bool() != Some(true))
            .filter_map(|x| x["id"].as_str().map(str::to_string))
            .collect();
        let blocking = review["issues"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|i| i["blocking"].as_bool() == Some(true));
        let (decision, downgraded) = match review["decision"].as_str().unwrap_or("") {
            "approved" if !unmet.is_empty() || blocking => {
                (ReviewDecision::ChangesRequested, Some(unmet))
            }
            "approved" => (ReviewDecision::Approved, None),
            "blocked" => (ReviewDecision::Blocked, None),
            _ => (ReviewDecision::ChangesRequested, None),
        };
        Self {
            decision,
            downgraded,
        }
    }
}

/// R48: a review of the same checkpoint by the same reviewer that was interrupted resumes
/// its session.
pub(super) fn review_resume(
    prev: Option<&AttemptRow>,
    head: &str,
    reviewer: &str,
) -> Option<String> {
    prev.filter(|p| {
        p.status == AttemptStatus::Interrupted
            && p.subject_commit.as_deref() == Some(head)
            && p.profile.as_deref() == Some(reviewer)
    })
    .and_then(|p| p.external_session_id.clone())
}

/// The Reviewer's prompt; a resumed session is told that its previous turn was interrupted.
pub(super) fn review_prompt(head: &str, acceptance_ids: &[String], resumed: bool) -> String {
    let prompt = format!(
        "Review the checkpoint {} in this worktree against TASK.md. DIFF.patch is base → checkpoint; DIFF-incremental.patch is the change since your last review (empty on the first). Acceptance criteria ids: {:?}.",
        short(head),
        acceptance_ids
    );
    if resumed {
        format!("Your previous review turn was interrupted before it finished. {prompt} Return the complete review.")
    } else {
        prompt
    }
}

/// R22: the prompt of a correction turn, with the violations of the rejected review.
pub(super) fn correction_prompt(violations: &[String]) -> String {
    format!("Your previous final response was rejected and was not used. Validation error: {}. Return only a corrected JSON object matching the schema.", violations.join("; "))
}

/// The non-blocking issues of a review, one line each (R53).
pub fn non_blocking(review: &Value) -> Vec<String> {
    review["issues"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i["blocking"].as_bool() != Some(true))
        .map(|i| {
            format!(
                "- {} [{}] {}{}",
                i["id"].as_str().unwrap_or(""),
                i["severity"].as_str().unwrap_or(""),
                i["file"]
                    .as_str()
                    .map(|x| format!("{x}: "))
                    .unwrap_or_default(),
                i["description"].as_str().unwrap_or("")
            )
        })
        .collect()
}

/// What the human should see at the final gate (R53, R49, R59): that verify proved compile
/// level only (X++), the approved review's non-blocking issues, and a note when the
/// implementer and the reviewer share a vendor.
pub fn final_gate_notes(store: &Store, t: &TaskRow) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let cfg = frozen_config(t)?;
    // M11-E3: what the environment gates of the verify result in force did
    let (mut passed, mut skipped, mut machine) = (Vec::new(), Vec::new(), String::new());
    if cfg.team.is_some() {
        if let Some((_, b)) = crate::engine::artifacts::read_head(store, t, "verify-result")? {
            let vr: Value = serde_json::from_slice(&b).unwrap_or_default();
            for c in vr["commands"].as_array().into_iter().flatten() {
                if c["kind"] != "environment" {
                    continue;
                }
                let name = c["name"].as_str().unwrap_or_default().to_string();
                if c["skipped"] == true {
                    skipped.push(name);
                } else if c["exit"] == 0 {
                    machine = c["machine"].as_str().unwrap_or_default().to_string();
                    passed.push(name);
                }
            }
        }
    }
    out.extend(verify_scope_notes(
        xpp::compile_only(&cfg),
        &passed,
        &skipped,
        &machine,
    ));
    // M11-E3: the environments this task's checkpoints were deployed to
    let i = t.id.clone();
    let deployed = store.call(move |tx| {
        Ok(store::deployments(tx)?
            .into_iter()
            .filter(|d| d.task == i)
            .collect::<Vec<_>>())
    })?;
    for d in deployed {
        let status = crate::engine::views::deployment_status(&d, Some(t.state), None, false);
        out.push(format!(
            "environment: {}",
            crate::engine::views::deployment_line(&d, status)
        ));
    }
    if let Some((_, b)) = crate::engine::artifacts::read_head(store, t, "review")? {
        let v: Value = serde_json::from_slice(&b)?;
        let nb = non_blocking(&v);
        if !nb.is_empty() {
            out.push(format!(
                "non-blocking issues (follow-ups, also in DECISIONS.md):\n{}",
                nb.join("\n")
            ));
        }
    }
    // M11-C3: `off` drops the note; `warn` (the default, and legacy mode) keeps it
    if reviewer_check(&cfg) != Level::Off {
        let imps = implementer_profiles(store, t)?;
        let same = same_vendor(&cfg, &imps, &t.reviewer);
        if !same.is_empty() {
            out.push(format!(
                "same vendor: {} implemented and {} reviewed ({}) — the review is less independent (R49); read the diff yourself",
                same.join(", "),
                t.reviewer,
                both_kind(&cfg, &t.reviewer)
            ));
        }
    }
    Ok(out)
}

/// What verify proved, for the final gate (R59, M11-E3): `COMPILE_ONLY` for an X++ compile
/// without environment gates; `verify: compile + dbsync on <machine>` when environment gates
/// passed after it; `verify: dbsync on <machine> (environment gate)` without X++; one note per
/// skipped environment gate.
pub(super) fn verify_scope_notes(
    compile_only: bool,
    passed: &[String],
    skipped: &[String],
    machine: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    match (compile_only, passed.is_empty()) {
        (true, true) => out.push(xpp::COMPILE_ONLY.to_string()),
        (true, false) => out.push(format!(
            "verify: compile + {} on {machine}",
            passed.join(" + ")
        )),
        (false, false) => out.push(format!(
            "verify: {} on {machine} (environment gate)",
            passed.join(" + ")
        )),
        (false, true) => {}
    }
    for s in skipped {
        out.push(format!(
            "{s} did not run on this machine (environment gate not defined here, if_missing: skip)"
        ));
    }
    out
}

/// The profiles of the implement and fix attempts whose turn ran — after a fallback, every one
/// that wrote into the checkpoint (R49 keeps the partial work of a failed or interrupted turn)
/// — in order, consecutive repeats once (M11-C3: the final-gate note and the pre-review check
/// share it). M11-R7 (X4): any end counts (`Succeeded`, `Failed`, `Interrupted`); only an
/// attempt that ended before its agent started — no context recorded (the context is stored
/// right before the agent starts) — is left out. A succeeded one always counts.
pub fn implementer_profiles(store: &Store, t: &TaskRow) -> Result<Vec<String>> {
    let i = t.id.clone();
    let atts = store.call(move |tx| store::attempts(tx, &i))?;
    let mut imps: Vec<String> = atts
        .iter()
        .filter(|a| {
            a.kind == "agent"
                && (a.stage == "implement" || a.stage.starts_with("fix-"))
                && (a.status == AttemptStatus::Succeeded || a.context_sha256.is_some())
        })
        .filter_map(|a| a.profile.clone())
        .collect();
    imps.dedup();
    Ok(imps)
}

/// The implementer profiles of `imps` whose `kind` (claude | codex) is the reviewer's.
pub(super) fn same_vendor<'a>(cfg: &Effective, imps: &'a [String], reviewer: &str) -> Vec<&'a str> {
    let kind = |p: &str| cfg.profile(p).map(|x| x.kind).ok();
    imps.iter()
        .map(String::as_str)
        .filter(|p| kind(p).is_some() && kind(p) == kind(reviewer))
        .collect()
}

/// `Claude both` / `Codex both`, for the messages.
pub(super) fn both_kind(cfg: &Effective, reviewer: &str) -> String {
    cfg.profile(reviewer)
        .map(|p| format!("{:?} both", p.kind))
        .unwrap_or_default()
}

/// `checks.reviewer-differs-from-builder` of the org chart (M11-C3); `warn` in legacy mode.
pub(super) fn reviewer_check(cfg: &Effective) -> Level {
    cfg.team
        .as_ref()
        .map(|t| t.checks.reviewer_differs_from_builder)
        .unwrap_or(Level::Warn)
}

/// The escalation reason of `reviewer-differs-from-builder: error` (M11-C3).
pub(super) fn same_vendor_reason(id: &str, same: &[&str], reviewer: &str, kind: &str) -> String {
    format!(
        "{SAME_VENDOR}: {} built this checkpoint and {reviewer} would review it ({kind}) — `checks.reviewer-differs-from-builder: error` in the org chart (R49). Review it yourself (`toryo approve {id} --gate final --force` opens the final gate without the AI review), or give the reviewer seat a profile of another kind (machine `seats:`), `toryo task config {id} --reload`, then `toryo resume {id} --note …`",
        same.join(", ")
    )
}

/// The escalation class of a same-vendor review refused before it starts (M11-C3).
pub const SAME_VENDOR: &str = crate::engine::state::EscalationClass::SameVendorReview.as_str();

pub(super) fn review_feedback(review: &Value) -> String {
    let mut f = String::new();
    for ac in review["acceptance_criteria"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|x| x["met"].as_bool() != Some(true))
    {
        f.push_str(&format!(
            "- {} NOT MET: {}\n",
            ac["id"].as_str().unwrap_or(""),
            ac["evidence"].as_str().unwrap_or("")
        ));
    }
    for i in review["issues"].as_array().into_iter().flatten() {
        f.push_str(&format!(
            "- {} [{}{}] {}{}\n",
            i["id"].as_str().unwrap_or(""),
            i["severity"].as_str().unwrap_or(""),
            if i["blocking"].as_bool() == Some(true) {
                ", blocking"
            } else {
                ""
            },
            i["file"]
                .as_str()
                .map(|x| format!("{x}: "))
                .unwrap_or_default(),
            i["description"].as_str().unwrap_or("")
        ));
    }
    f
}
