//! The turns of the read-only roles, Planner and Reviewer (R23-27, moved as they were): reruns by
//! error class (plan §11, F7), admission, the checks after the turn.

use super::*;

impl<'a> Engine<'a> {
    /// One turn of a read-only role in attempt `a`. `Err(flow)`: the turn ended the attempt
    /// — the live budget interrupt, a stop, a quota pause, or a policy escalation.
    pub(super) fn read_only_turn(
        &self,
        t: &TaskRow,
        a: &AttemptRow,
        role: Role,
        ctx: &Path,
        stop: &StopWatch,
        ask: Ask,
    ) -> Result<Result<StageOutcome, Flow>> {
        let (profile, schema) = match role {
            Role::Planner => (&t.planner, "plan.schema.json"),
            _ => (&t.reviewer, "review.schema.json"),
        };
        let (hooks, budget_hit) = self.hooks(a, stop, profile)?;
        // EC-31 (F5): the worktree as the turn finds it — only what changes during the turn
        // is the read-only role's write, never what a human left there before
        let before = TreeState::read(&Worktree::new(&self.g, &t.worktree))?;
        // M13-06: kept in the run store (hashed like every artifact) — when the engine dies
        // after the turn, recovery still knows what was there before it, and escalates and
        // puts back only what the role changed
        crate::engine::artifacts::put(
            self.store,
            t,
            Some(&a.id),
            &tree_before_name(a),
            "json",
            "tree_before",
            &serde_json::to_vec(&before)?,
            None,
        )?;
        let r = self.hub.turn_with(
            OpenSession {
                profile: profile.clone(),
                role,
                cwd: t.worktree.clone(),
                run_store: Some(t.artifacts()),
                capture_dir: t.captures(),
                schema: Some(contracts::schema(schema)?.into()),
                context_file: Some(ctx.to_path_buf()),
                model: self.role_model(role.as_str()),
                resume: ask.resume,
            },
            TurnInput {
                prompt: ask.prompt.to_string(),
                label: ask.label,
                echo: true,
                // (no TEMP/TMP of its own: R46 redirects the implementer's; R23-23 removed an empty
                // `capture_env` that suggested otherwise)
                env: BTreeMap::new(),
            },
            self.limits(),
            hooks,
        );
        crash_point("read_only:after_turn");
        // M13-06: an error after the agent ran (an I/O error, the session's close) must not
        // skip the checks either — what the role wrote is undone before the error goes up
        let r = match r {
            Ok(r) => {
                self.agent_ended(a, profile, &r, budget_hit.load(Ordering::SeqCst));
                r
            }
            Err(e) => {
                // M13-R7: the same findings as after a turn that ended normally — the protected
                // check first, so that an untracked protected file the role edited is seen
                // before the undo removes it — and a failure of the checks themselves must not
                // mask the error of the turn: it is logged, the turn's own error goes up
                match self.read_only_findings(t, &a.id, &before) {
                    Ok(Some((reason, class, _))) => {
                        // R23-08: the same closer as every other attempt — `stage_finished`,
                        // the `escalated` event and the class on the machine
                        let detail = format!("{reason} (the turn itself ended in an error: {e:#})");
                        let none = StageOutcome::default();
                        let end = AttemptEnd {
                            error_class: Some(class.to_string()),
                            error_detail: Some(detail),
                            ..Default::default()
                        };
                        self.close(
                            a,
                            Close::of_turn(AttemptStatus::Failed, &none, end)
                                .then(Some(Ev::Escalate { reason }), None)
                                .class(Some(class)),
                        )?;
                    }
                    Ok(None) => {}
                    Err(x) => crate::errln!(
                        "!! the checks after the read-only turn failed too ({x:#}); the turn's own error follows"
                    ),
                }
                return Err(e);
            }
        };
        let model = self.role_model(role.as_str());
        if let Err(e) = self.usage(a, profile, &r, model.as_deref()) {
            // the same for an error while the usage is recorded: checks first
            if let Some(flow) = self.read_only_checks(t, a, &r, &before)? {
                crate::errln!("!! the turn's usage could not be recorded: {e:#}");
                return Ok(Err(flow));
            }
            return Err(e);
        }
        // M12-03 (H6): the protected-state and "a read-only role wrote" checks come first, on
        // every way the turn ended — a stop, the live budget and a quota pause must not hide a
        // write (it would stay in the tree and go on under the implementer's name)
        if let Some(flow) = self.read_only_checks(t, a, &r, &before)? {
            return Ok(Err(flow));
        }
        if let Some(flow) = self.cut_short(a, &r, budget_hit.load(Ordering::SeqCst))? {
            return Ok(Err(flow));
        }
        if let Some(flow) = self.read_only_quota(t, a, &r, role.as_str())? {
            return Ok(Err(flow));
        }
        Ok(Ok(r))
    }

    /// R41: a further turn of a running attempt (a correction or a retry) is admitted like
    /// the first. A refusal has already paused the task (`admitted`), so the attempt only
    /// ends here, as `budget` — no second `BudgetExceeded` (EC-13).
    pub(super) fn admit_next_turn(
        &self,
        a: &AttemptRow,
        last: &StageOutcome,
        corrections: u32,
    ) -> Result<bool> {
        if self.admitted()? {
            return Ok(true);
        }
        self.end_and(
            a,
            AttemptStatus::Failed,
            last,
            AttemptEnd {
                error_class: Some("budget".into()),
                error_detail: Some("the admission budget refused the next turn (R41)".into()),
                correction_turns: Some(corrections),
                ..Default::default()
            },
            None,
            None,
        )?;
        Ok(false)
    }

    /// A read-only turn failed for good (EC-01, EC-14): the attempt fails with the turn's
    /// real class and the task escalates.
    pub(super) fn read_only_failed(
        &self,
        a: &AttemptRow,
        last: &StageOutcome,
        role: &str,
        class: ErrorClass,
        retries: u32,
        corrections: u32,
    ) -> Result<Flow> {
        last.print_summary();
        let reason = read_only_failure(role, class, last.error_detail.as_deref(), retries);
        self.end_and(
            a,
            AttemptStatus::Failed,
            last,
            AttemptEnd {
                error_class: Some(format!("{class:?}")),
                correction_turns: Some(corrections),
                ..Default::default()
            },
            Some(Ev::Escalate { reason }),
            None,
        )?;
        Ok(Flow::Next)
    }

    /// A Planner/Reviewer turn that ended by quota or rate limit → `Paused` (no Planner/Reviewer
    /// fallback in v0.1, R49). Runs after [`Self::read_only_checks`].
    pub(super) fn read_only_quota(
        &self,
        t: &TaskRow,
        a: &AttemptRow,
        r: &StageOutcome,
        role: &str,
    ) -> Result<Option<Flow>> {
        if let Some(
            c @ (ErrorClass::QuotaExceeded | ErrorClass::RateLimited | ErrorClass::Overloaded),
        ) = r.error_class
        {
            let reason = format!("{role}_quota: {c:?} {} — `toryo resume {}` when the quota is back (no {role} fallback in v0.1)", r.error_detail.clone().unwrap_or_default(), t.id);
            self.end_and(
                a,
                AttemptStatus::Failed,
                r,
                AttemptEnd::default(),
                Some(Ev::Pause { reason }),
                None,
            )?;
            return Ok(Some(Flow::Next));
        }
        Ok(None)
    }

    /// After a Planner/Reviewer turn, however it ended (M12-03): a changed protected state →
    /// `Escalated`; a worktree that changed during the turn (from `before`) or a moved HEAD →
    /// `Escalated` as a policy violation (EC-31: read-only roles write nothing, and what one
    /// wrote must never reach the next checkpoint under the implementer's name).
    pub(super) fn read_only_checks(
        &self,
        t: &TaskRow,
        a: &AttemptRow,
        r: &StageOutcome,
        before: &TreeState,
    ) -> Result<Option<Flow>> {
        if let Some((reason, class, undone)) = self.read_only_findings(t, &a.id, before)? {
            self.end_and(
                a,
                AttemptStatus::Failed,
                r,
                AttemptEnd {
                    error_class: Some(class.into()),
                    // (a protected change alone has always ended the attempt without a detail)
                    error_detail: (undone || class != protect::REASON).then(|| reason.clone()),
                    ..Default::default()
                },
                Some(Ev::Escalate { reason }),
                None,
            )?;
            return Ok(Some(Flow::Next));
        }
        Ok(None)
    }

    /// What the checks after a read-only turn found — `(reason, class, the undo found a write)`.
    /// M13-R4 (X32): the protected check runs first (it records what it found), and then the
    /// undo runs all the same — a protected change is no reason to leave the role's other
    /// writes in the tree, where the next fix cycle would commit them. M13-R7: the same code
    /// after a turn that ended in an error.
    pub(super) fn read_only_findings(
        &self,
        t: &TaskRow,
        attempt: &str,
        before: &TreeState,
    ) -> Result<Option<(String, &'static str, bool)>> {
        let protected = self.protected_violation(t, false)?;
        let wt = Worktree::new(&self.g, &t.worktree);
        let undone = undo_read_only_write(&wt, t, attempt, before)?;
        Ok(match (protected, undone) {
            (Some(p), Some(u)) => Some((format!("{p}; {u}"), protect::REASON, true)),
            (Some(p), None) => Some((p, protect::REASON, false)),
            (None, Some(u)) => Some((u, "policy_violation", true)),
            (None, None) => None,
        })
    }
}

/// Retries of the turns of one Planner/Reviewer attempt, whatever their classes, before the
/// task escalates with the last turn's class (EC-14): the largest allowance of a class in
/// plan §11 ([`class_retries`]), so mixed failures stay bounded too.
pub(super) const MAX_TURN_RETRIES: u32 = 2;

/// Plan §11: the retries a failed Planner/Reviewer turn of `class` gets — a crash or a
/// network error two (resume, then a fresh start), an unknown failure, a timeout or a context
/// overflow one; anything else none.
pub(super) fn class_retries(class: ErrorClass) -> u32 {
    match class {
        ErrorClass::ProcessCrashed | ErrorClass::Network => 2,
        ErrorClass::Unknown | ErrorClass::Timeout | ErrorClass::ContextWindowExceeded => 1,
        _ => 0,
    }
}

/// How a failed Planner/Reviewer turn runs again (plan §11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Rerun {
    /// "Resume within the stage": the failed turn's session, resumed, asked the same again.
    Resume,
    /// "A new attempt" / "a new session for the same stage": a fresh session, asked the
    /// stage's first prompt.
    Fresh,
}

impl Rerun {
    /// The prompt and the session of the turn that runs again after the failed turn `r`,
    /// which was asked `prompt` in `resume`; `fresh` is the stage's first prompt for a fresh
    /// session. A turn that never reported a session resumes the one it was asked in.
    pub(super) fn ask(
        self,
        r: &StageOutcome,
        prompt: &str,
        resume: Option<&str>,
        fresh: &str,
    ) -> (String, Option<String>) {
        match self {
            Self::Resume => (
                prompt.to_string(),
                r.session_id.clone().or(resume.map(str::to_string)),
            ),
            Self::Fresh => (fresh.to_string(), None),
        }
    }
}

/// Plan §11 for a failed Planner/Reviewer turn of `class`, after `earlier` retries of the
/// same class and `total` retries of the attempt: a crash or a network error resumes the
/// stage's session once, then starts it fresh once; an unknown failure is treated like a
/// crash, once; a timeout and a context overflow start fresh once. `None`: escalate.
pub(super) fn rerun(class: ErrorClass, earlier: u32, total: u32) -> Option<Rerun> {
    if earlier >= class_retries(class) || total >= MAX_TURN_RETRIES {
        return None;
    }
    Some(match class {
        ErrorClass::ProcessCrashed | ErrorClass::Network if earlier == 0 => Rerun::Resume,
        ErrorClass::Unknown => Rerun::Resume,
        _ => Rerun::Fresh,
    })
}

/// The retries of one Planner/Reviewer attempt so far: the class of each.
#[derive(Debug, Clone, Default)]
pub(super) struct TurnRetries(pub(super) Vec<ErrorClass>);

impl TurnRetries {
    pub(super) fn total(&self) -> u32 {
        self.0.len() as u32
    }

    pub(super) fn of(&self, class: ErrorClass) -> u32 {
        self.0.iter().filter(|c| **c == class).count() as u32
    }

    /// A failed turn of `class`: how it runs again (and count it), or `None` (escalate).
    pub(super) fn next(&mut self, class: ErrorClass) -> Option<Rerun> {
        let how = rerun(class, self.of(class), self.total())?;
        self.0.push(class);
        Some(how)
    }
}

/// One turn of a read-only role: what is asked, in which session, under which capture label.
pub(super) struct Ask<'p> {
    pub(super) prompt: &'p str,
    pub(super) resume: Option<String>,
    pub(super) label: String,
}

/// What a finished Planner/Reviewer turn yields (EC-01, EC-14), after the live budget
/// interrupt, a stop, quota and rate limits (pause) and the policy checks (escalate). A failed
/// turn never yields a plan or a review — whatever payload it carries (plan §6.2).
#[derive(Debug, Clone, PartialEq)]
pub(super) enum ReadOnlyTurn {
    /// A successful turn's structured output (`Null` when it returned none), to be checked.
    Output(Value),
    /// The turn failed because its output could not be used (no final message, not JSON, too
    /// long: class `InvalidOutput`): invalid output, like a review that fails its checks (R22).
    Invalid(String),
    /// A crash, a network error, an unknown failure, a timeout or a context overflow: the turn
    /// runs again as §11 says for the class ([`rerun`]), else the task escalates.
    Retry(ErrorClass),
    /// Anything else — a permission denial, a policy refusal, auth, a bad request: no blind
    /// retry (§11), the task escalates with the class.
    Failed(ErrorClass),
}

impl ReadOnlyTurn {
    pub(super) fn of(r: &StageOutcome) -> Self {
        // any denial fails the stage (plan §6.2), even if an adapter reported success
        let class = match (r.success, r.permission_denials.is_empty()) {
            (true, true) => return Self::Output(r.structured.clone().unwrap_or(Value::Null)),
            (true, false) => Some(ErrorClass::PermissionDenied),
            (false, _) => r.error_class,
        };
        match class {
            Some(ErrorClass::InvalidOutput) => Self::Invalid(format!(
                "the final response could not be used: {}",
                r.error_detail.as_deref().unwrap_or("InvalidOutput")
            )),
            Some(
                c @ (ErrorClass::ProcessCrashed
                | ErrorClass::Timeout
                | ErrorClass::Network
                | ErrorClass::Unknown
                | ErrorClass::ContextWindowExceeded),
            ) => Self::Retry(c),
            None => Self::Retry(ErrorClass::Unknown),
            Some(c) => Self::Failed(c),
        }
    }
}

/// The line printed before a failed read-only turn runs again: how, and which retry of its
/// class it is (`n` of [`class_retries`]).
pub(super) fn retry_note(
    role: &str,
    class: ErrorClass,
    r: &StageOutcome,
    how: Rerun,
    n: u32,
) -> String {
    let again = match how {
        Rerun::Resume => "the same turn again, resuming its session",
        Rerun::Fresh => "the stage again in a fresh session",
    };
    format!(
        "!! {role} turn failed: {class:?} ({}) — {again} ({n}/{}, plan §11)",
        r.error_detail.as_deref().unwrap_or("no detail"),
        class_retries(class)
    )
}

/// Why the task escalates after a read-only turn failed for good: the turn's real class
/// first, as in `InvalidOutput: …` (EC-14).
pub(super) fn read_only_failure(
    role: &str,
    class: ErrorClass,
    detail: Option<&str>,
    retries: u32,
) -> String {
    let detail = detail.filter(|d| !d.is_empty()).unwrap_or("no detail");
    match retries {
        0 => format!("{class:?}: the {role} turn failed ({detail}); a failed turn is never used and not retried blindly (plan §6.2, §11)"),
        1 => format!("{class:?}: the {role} turn failed ({detail}), also after 1 retry (plan §11)"),
        n => format!("{class:?}: the {role} turn failed ({detail}), also after {n} retries (plan §11)"),
    }
}
