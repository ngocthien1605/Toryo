//! The Planning stage (R23-27, moved out of `runner.rs` as it was): the planner's turn, the
//! plan-time checks (protected paths, the write fence, the skills on the implementer's menu and in
//! its budget) and the plan's prompts.

use super::*;

impl<'a> Engine<'a> {
    pub(super) fn plan(&mut self, t: &TaskRow) -> Result<Flow> {
        let art = t.artifacts();
        let prev = {
            let i = self.id.clone();
            self.store
                .call(move |tx| store::last_attempt(tx, &i, "plan"))?
        };
        // R48: the Planner resumes its own session for a revision or after an interruption —
        // never the session of another profile (EC-30)
        let resume = plan_resume(prev.as_ref(), &t.planner);
        let prompt = plan_prompt(
            t.feedback.as_deref(),
            prev.as_ref().map(|p| p.status),
            &art.join("TASK.md"),
        );
        if !self.admitted()? || self.turn_blocked(t, "plan", &t.planner)? {
            return Ok(Flow::Next);
        }
        let head = Worktree::new(&self.g, &t.worktree).head()?;
        let a = self.begin(NewAttempt {
            stage: "plan",
            kind: "agent",
            profile: Some(&t.planner),
            fallback_from: None,
            head_before: Some(&head),
            subject_commit: None,
        })?;
        self.stage_started(&a, None);
        crash_point("plan:after_begin");
        let Some(ctx) = self.context(t, &a, &t.planner)? else {
            return Ok(Flow::Next);
        };
        let stop = self.lock.watch_stop();
        // EC-01/EC-14: a failed turn never yields a plan; a crash, a network error, an unknown
        // failure, a timeout or a context overflow runs again as plan §11 says for its class —
        // resumed or fresh, bounded (F7) — anything else escalates
        let fresh = plan_fresh_prompt(
            t.feedback.as_deref(),
            Some(art.join("PLAN.md")).filter(|p| p.is_file()).as_deref(),
            &art.join("TASK.md"),
        );
        let (mut prompt, mut resume) = (prompt, resume);
        let mut last = StageOutcome::default();
        let mut retries = TurnRetries::default();
        let (r, output) = loop {
            if retries.total() > 0 && !self.admit_next_turn(&a, &last, 0)? {
                return Ok(Flow::Next);
            }
            let ask = Ask {
                prompt: &prompt,
                resume: resume.clone(),
                label: match retries.total() {
                    0 => format!("plan-{}", a.attempt),
                    n => format!("plan-{}-{n}", a.attempt),
                },
            };
            let r = match self.read_only_turn(t, &a, Role::Planner, &ctx, &stop, ask)? {
                Ok(r) => r,
                Err(flow) => return Ok(flow),
            };
            match ReadOnlyTurn::of(&r) {
                ReadOnlyTurn::Retry(class) => match retries.next(class) {
                    Some(how) => {
                        crate::errln!(
                            "{}",
                            retry_note("planner", class, &r, how, retries.of(class))
                        );
                        (prompt, resume) = how.ask(&r, &prompt, resume.as_deref(), &fresh);
                        last = r;
                    }
                    None => {
                        return self.read_only_failed(&a, &r, "planner", class, retries.total(), 0)
                    }
                },
                ReadOnlyTurn::Failed(class) => {
                    return self.read_only_failed(&a, &r, "planner", class, retries.total(), 0);
                }
                output @ (ReadOnlyTurn::Output(_) | ReadOnlyTurn::Invalid(_)) => break (r, output),
            }
        };
        // EC-25: a plan that does not match plan.schema.json is no plan (R22, R56: escalate)
        let plan = match plan_output(output) {
            Ok(plan) => plan,
            Err(reason) => {
                self.end_and(
                    &a,
                    AttemptStatus::Failed,
                    &r,
                    AttemptEnd {
                        error_class: Some("InvalidOutput".into()),
                        ..Default::default()
                    },
                    Some(Ev::Escalate {
                        reason,
                        class: None,
                    }),
                    None,
                )?;
                return Ok(Flow::Next);
            }
        };
        // M11-D2 (group D review): `skills` means something only with an org chart — a
        // legacy plan is stored and shown as in M10
        let mut plan = plan;
        if self.cfg.team.is_none() {
            if let Some(o) = plan.as_object_mut() {
                o.remove("skills");
            }
        }
        let bytes = serde_json::to_vec_pretty(&plan)?;
        let md = crate::engine::render::render("plan", &plan)?;
        self.put(t, &a, "plan", "json", &bytes, "plan.json")?;
        self.put(t, &a, "plan-md", "md", md.as_bytes(), "PLAN.md")?;
        // the agent wrote it: inert, and never painted like a line of Toryo's own (M15)
        crate::term::quoted(crate::term::Stream::Err, &format!("\n{md}"));
        let bad: Vec<String> = plan_files(&plan)
            .into_iter()
            .filter(|f| self.protect.violates(f))
            .collect();
        if !bad.is_empty() {
            // M11-R8: with a fence violation, the reason (and so the escalation after the
            // re-plan bound) names the rule, not "protected paths"
            let fenced = bad.iter().any(|p| self.protect.fence_rule(p).is_some());
            let reason =
                fenced.then(|| format!("the plan lists {}", self.protect.violation_words(&bad)));
            let fb = match plan_fence_feedback(&bad, &self.protect) {
                // M11-C2: the feedback names the fence rule each path breaks
                Some(fb) => {
                    crate::errln!("!! the plan lists paths the implementer may not change {bad:?} — rejected automatically (plan-time policy, write fence)");
                    fb
                }
                None => {
                    crate::errln!("!! the plan lists protected or denied paths {bad:?} — rejected automatically (plan-time policy)");
                    format!("Your plan lists protected or denied paths {bad:?} (toryo.yaml policy), which may not change. Create the plan again without touching them.")
                }
            };
            self.end_and(
                &a,
                AttemptStatus::Succeeded,
                &r,
                AttemptEnd::default(),
                Some(Ev::PlanPolicyViolation { paths: bad, reason }),
                Some(Some(fb)),
            )?;
            return Ok(Flow::Next);
        }
        // M11-D2: the implementer's skills — on its menu, within its context budget
        if let Some((reason, fb)) = self.plan_skills_problem(t, &plan)? {
            crate::errln!("!! {reason} — rejected automatically (plan-time skills check)");
            self.end_and(
                &a,
                AttemptStatus::Succeeded,
                &r,
                AttemptEnd::default(),
                Some(Ev::PlanPolicyViolation {
                    paths: vec![],
                    reason: Some(reason),
                }),
                Some(Some(fb)),
            )?;
            return Ok(Flow::Next);
        }
        let subject = sha256_hex(&bytes);
        self.end_and(
            &a,
            AttemptStatus::Succeeded,
            &r,
            AttemptEnd::default(),
            Some(Ev::PlanReady { subject }),
            Some(None),
        )?;
        Ok(Flow::Next)
    }

    /// M11-D2: what is wrong with the plan's `skills` — `(reason, feedback for the planner)`:
    /// a name that is not on the implementer's menu (the feedback lists the menu, with a "did
    /// you mean"), or chosen skills that, with the implementer's never-trimmed parts, exceed its
    /// context budget ("choose fewer"). `None` without an org chart or without skills.
    pub(super) fn plan_skills_problem(
        &self,
        t: &TaskRow,
        plan: &Value,
    ) -> Result<Option<(String, String)>> {
        let Some(team) = &self.cfg.team else {
            return Ok(None);
        };
        let chosen = context::plan_skills(plan);
        if chosen.is_empty() {
            return Ok(None);
        }
        // M11-R5: the menu is what resolves now — a skill that cannot be delivered is off it
        let machine = crate::team::machine_skills_dir(&team.skill_roots);
        let available = context::available_skills(team, &t.repo, machine.as_deref());
        let menu: Vec<&str> = available.iter().map(|k| k.name.as_str()).collect();
        let off: Vec<String> = chosen
            .iter()
            .filter(|s| !menu.contains(&s.as_str()))
            .map(|s| {
                let mean = crate::team::did_you_mean(s, &menu)
                    .map(|m| format!(" (did you mean `{m}`?)"))
                    .unwrap_or_default();
                // a name the planner wrote: escaped, it reaches the terminal and the reason
                format!("`{}`{mean}", s.escape_debug())
            })
            .collect();
        if !off.is_empty() {
            let reason = format!(
                "plan_skills: the plan lists skills that are not on the implementer's menu: {}",
                off.join(", ")
            );
            let listed =
                context::skill_menu(team, &t.repo, machine.as_deref()).unwrap_or_else(|| {
                    "(no skill of the implementer resolves on this machine now)\n".into()
                });
            let fb = format!(
                "Your plan lists skills that are not on the implementer's menu: {}. {listed}Create the plan again with `skills` naming only skills from this menu (or none).",
                off.join(", ")
            );
            return Ok(Some((reason, fb)));
        }
        // the chosen skills are never trimmed: with the implementer's other never-trimmed
        // parts they must fit its budget — the implementer's context as it would be assembled
        // (D3: from this plan, the head of `plan`), for every profile of the chain (M11-R5: a
        // fallback without the run store also has TASK.md and PLAN.md inline, never trimmed)
        let mut chain: Vec<String> = Vec::new();
        for p in &t.implementers {
            if !chain.contains(p) {
                chain.push(p.clone());
            }
        }
        if chain.is_empty() {
            chain.push(context::stage_profile(t, "implement")?);
        }
        for profile in &chain {
            let mut req = context::request(self.store, t, &self.cfg, "implement", profile)?;
            let budget = req.budget;
            // (R4–R8 review) the test of the attempt (`context::over_budget`: the rendered
            // context after trimming), on a budget that leaves room for what a fix stage adds
            // (its pack sentence, the verify result and review as data)
            req.budget = budget.saturating_sub(PLAN_BUDGET_MARGIN);
            let c = context::assemble(&req)?;
            req.budget = budget;
            let skills: usize = c
                .sources
                .iter()
                .filter(|s| s.label.starts_with("skill "))
                .map(|s| s.bytes)
                .sum();
            let fixed = c.fixed_bytes.saturating_sub(skills);
            if c.fixed_bytes > req.budget || context::over_budget(&c).is_some() {
                let inline = if req.no_run_store {
                    ", with TASK.md and PLAN.md inline"
                } else {
                    ""
                };
                let reason = format!(
                    "plan_skills: the chosen skills ({skills} bytes) and the fixed instructions of the implementer `{profile}` ({fixed} bytes{inline}) exceed its context budget ({} KB)",
                    req.budget / 1024
                );
                let fb = format!(
                    "Choose fewer skills: the skills you chose ({}; {skills} bytes) and the fixed instructions of the implementer `{profile}` ({fixed} bytes{inline}) exceed its context budget of {} KB (context.budget_kb.implementer). Create the plan again with fewer names in `skills`.",
                    chosen.join(", "),
                    req.budget / 1024
                );
                return Ok(Some((reason, fb)));
            }
        }
        Ok(None)
    }

    pub(super) fn role_model(&self, role: &str) -> Option<String> {
        self.cfg.repo.roles.get(role).and_then(|r| r.model.clone())
    }
}

/// R48: the Planner resumes the session of its previous attempt — only when that attempt ran
/// with the same profile: after `task config --reload` switched `roles.planner`, the new
/// planner never gets the old CLI's session id (EC-30).
pub(super) fn plan_resume(prev: Option<&AttemptRow>, planner: &str) -> Option<String> {
    prev.filter(|p| p.profile.as_deref() == Some(planner))
        .and_then(|p| p.external_session_id.clone())
}

/// The Planner's prompt: the feedback on a revision, else the task (again, after an
/// interrupted turn).
pub(super) fn plan_prompt(
    feedback: Option<&str>,
    prev: Option<AttemptStatus>,
    task_md: &Path,
) -> String {
    match (feedback, prev) {
        (Some(f), _) => f.to_string(),
        (None, Some(AttemptStatus::Interrupted)) => format!(
            "Your previous turn was interrupted before it finished. Create the plan for the task in {} again and return it.",
            task_md.display()
        ),
        _ => format!("Create the plan for the task in {}.", task_md.display()),
    }
}

/// The prompt of a planner turn retried in a new session (plan §11): a revision cannot rely
/// on the session that saw the plan, so it names where the plan being revised is.
pub(super) fn plan_fresh_prompt(
    feedback: Option<&str>,
    plan_md: Option<&Path>,
    task_md: &Path,
) -> String {
    match (feedback, plan_md) {
        (Some(f), Some(plan)) => format!(
            "{f}\nThis is a new session: the plan being revised is in {}, the task in {}.",
            plan.display(),
            task_md.display()
        ),
        _ => plan_prompt(feedback, None, task_md),
    }
}

/// The plan's `files_likely_involved` (checked against the path policy at plan time).
pub(super) fn plan_files(plan: &Value) -> Vec<String> {
    plan["files_likely_involved"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

/// M11-C2: the plan-time feedback when a planned path breaks the implementer's write fence —
/// each such path with its rule, then any protected or denied ones; `None` when no path breaks
/// the fence (the M10 message is used).
pub(super) fn plan_fence_feedback(bad: &[String], protect: &Protected) -> Option<String> {
    let fenced: Vec<String> = bad
        .iter()
        .filter_map(|f| {
            protect
                .fence_rule(f)
                .map(|r| format!("`{f}` — {}", r.plan_reason()))
        })
        .collect();
    if fenced.is_empty() {
        return None;
    }
    let others: Vec<&String> = bad
        .iter()
        .filter(|f| protect.fence_rule(f).is_none())
        .collect();
    let mut fb = format!(
        "Your plan lists paths the implementer may not change: {}.",
        fenced.join("; ")
    );
    if !others.is_empty() {
        fb.push_str(&format!(
            " It also lists protected or denied paths {others:?} (toryo.yaml policy)."
        ));
    }
    fb.push_str(" Create the plan again without them.");
    Some(fb)
}

/// The plan of a Planner turn that was not retried or escalated: its structured output when
/// it matches plan.schema.json (EC-25), else the escalation reason (R56: no correction turns
/// for the planner).
pub(super) fn plan_output(turn: ReadOnlyTurn) -> std::result::Result<Value, String> {
    match turn {
        ReadOnlyTurn::Output(Value::Null) => {
            Err("InvalidOutput: the planner returned no structured plan".into())
        }
        ReadOnlyTurn::Output(plan) => match contracts::validate("plan.schema.json", &plan) {
            Ok(()) => Ok(plan),
            Err(e) => Err(format!(
                "InvalidOutput: the planner returned no valid plan — it does not match plan.schema.json: {}",
                e.join("; ")
            )),
        },
        ReadOnlyTurn::Invalid(why) => Err(format!(
            "InvalidOutput: the planner returned no structured plan: {why}"
        )),
        ReadOnlyTurn::Retry(c) | ReadOnlyTurn::Failed(c) => Err(format!(
            "{c:?}: the planner turn failed; a failed turn never yields a plan"
        )),
    }
}

/// M11-R5 (R4–R8 review): the plan-time skills budget check leaves this much room for what a
/// fix stage adds to the implementer's context (its pack sentence, the untrusted-data note).
pub(super) const PLAN_BUDGET_MARGIN: usize = 512;
