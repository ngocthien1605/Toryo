//! The engine (plan §4, §5.2): drives one task through the state machine, one stage at a
//! time, with every stage recorded as a `stage_attempts` row and every state change
//! persisted with its event in one transaction (M3). Runs under the task lock; checks for a
//! stop request at every stage boundary and while a stage runs (R38).
//!
//! The stage bodies port the `spike pipeline` behaviour (R52): Planner (read-only, gate),
//! Implementer (fallback chain, HEAD guard, path policy), checkpoint (trailers, no-change
//! R40), Verify (stop at first failure; never to Review), Reviewer (exact AC set, correction
//! turns, consistency downgrade), final gate. Every agent attempt gets its context from the
//! ProjectContextLoader (§7, M4). The protected-state snapshot (R35, M5) is compared before
//! every running stage and after every agent turn and verify run. Quota exhaustion and
//! `--no-fallback` pause the task (M5). Artifacts are immutable versions (R34) and Verify runs
//! in a detached checkout of the checkpoint under `verify_root`, inside the Low-IL boundary
//! (R29/R36/R45, M6). Every turn is admitted against the token budget, and a live adapter's
//! turn is interrupted when it reaches the budget (R41, M7).
//!
//! The Implementer and Reviewer stages run in steps: prepare the turn, run it, interpret the
//! result into an outcome (`ImplementOutcome`, `ReviewTurns`, `ReviewVerdict`), then record
//! that outcome. The decisions are pure functions near the end of this file, with unit tests.

use crate::git::short;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::checks::{
    parse_allow_protected, resolve_program, review_violations, verify_commands, verify_files,
    Protected,
};
use super::context;
use super::contracts;
use super::lock::{StopWatch, TaskLock};
use super::protect;
use super::state::{GateKind, PipelineEvent as Ev, PipelineState as St};
use super::store::{self, AttemptEnd, AttemptRow, AttemptStatus, NewAttempt, Store, TaskRow};
use super::task::{frozen_config, sha256_hex, task_git};
use super::verify;
use super::worktree::{rel, Worktree, T_AGENT, T_ATTEMPT, T_NO_CHANGE, T_STAGE, T_TASK};
use crate::agents::adapter::TurnHooks;
use crate::agents::hub::AgentHub;
use crate::config::{expand_vars, Billing, CommandSpec, Effective, ProfileKind};
use crate::domain::{ErrorClass, OpenSession, Role, StageOutcome, Tri, TurnInput, TurnLimits};
use crate::git::GitCmd;
use crate::process::Boundary;
use crate::runner::{CommandResult, CommandRunner, LocalRunner, RunSpec};
use crate::usage::{Accounting, Weights};

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Approve both gates automatically (demos / CI only).
    pub yes: bool,
    /// R49: do not switch implementers; pause when the implementer is out of quota.
    pub no_fallback: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEnd {
    /// A human decision is needed (gate, `Escalated`, `Paused`).
    Waiting(St),
    /// `Completed`, `Cancelled` or `Failed`.
    Done(St),
    /// A `toryo stop` was handled; the task stays in its state (R38).
    Stopped,
}

enum Flow {
    Next,
    Stopped,
}

/// R49: rate-limit retries of one implementer before the switch to the next one.
const RATE_LIMIT_RETRIES: u32 = 2;

/// R22: correction turns for an invalid review before `InvalidOutput`.
const MAX_CORRECTIONS: u32 = 2;

/// Test hook: `TORYO_TEST_CRASH_AT=<point>[,<point>…]` exits the process abruptly at that
/// point, like a kill — no cleanup, the OS releases the lock (§10 recovery tests).
pub fn crash_point(name: &str) {
    if let Ok(v) = std::env::var("TORYO_TEST_CRASH_AT") {
        if v.split(',').any(|p| p.trim() == name) {
            eprintln!("!! TORYO_TEST_CRASH_AT: crashing at `{name}`");
            std::process::exit(86);
        }
    }
}

/// Git (with the admitted filter plan), protected paths and verify commands of a task.
pub fn task_policy(t: &TaskRow, cfg: &Effective) -> Result<(GitCmd, Protected, Vec<CommandSpec>)> {
    let g = task_git(cfg, &t.repo)?;
    let cmds = verify_commands(cfg, &t.worktree)?;
    let mut exact = verify_files(&cmds, &t.worktree);
    if let Ok(r) = rel(&t.repo, &t.task_file) {
        exact.push(r); // the task is the contract: agents may not edit it
    }
    // the allow list comes from the TASK.md snapshot (hashed at create, R28), not the repo
    let task_md = std::fs::read(t.artifacts().join("TASK.md")).unwrap_or_default();
    if sha256_hex(&task_md) != t.task_sha256 {
        bail!(
            "artifact_tampered: TASK.md of {} does not match tasks.task_sha256 (R27/R28)",
            t.id
        );
    }
    let task_md = String::from_utf8_lossy(&task_md).to_string();
    let p = Protected::new(exact, &cfg.repo.policy.protected_paths).with_policy(
        &parse_allow_protected(&task_md),
        &cfg.repo.policy.deny_paths,
    );
    Ok((g, p, cmds))
}

/// The writing stage label for a fix cycle: `implement`, `fix-1`, `fix-2`, …
pub fn writing_stage(fix_cycle: u32) -> String {
    if fix_cycle == 0 {
        "implement".into()
    } else {
        format!("fix-{fix_cycle}")
    }
}

pub struct Engine<'a> {
    store: &'a Store,
    hub: &'a AgentHub,
    lock: &'a TaskLock,
    pub cfg: Effective,
    g: GitCmd,
    id: String,
    opts: RunOptions,
    protect: Protected,
    cmds: Vec<CommandSpec>,
    retries: u32,
}

impl<'a> Engine<'a> {
    pub fn new(
        store: &'a Store,
        hub: &'a AgentHub,
        lock: &'a TaskLock,
        opts: RunOptions,
    ) -> Result<Self> {
        let id = lock.task.clone();
        let t = {
            let i = id.clone();
            store.call(move |tx| store::get_task(tx, &i))?
        };
        let cfg = frozen_config(&t)?;
        let (g, protect, cmds) = task_policy(&t, &cfg)?;
        Ok(Self {
            store,
            hub,
            lock,
            cfg,
            g,
            id,
            opts,
            protect,
            cmds,
            retries: 0,
        })
    }

    pub fn protected(&self) -> &Protected {
        &self.protect
    }

    pub fn git(&self) -> &GitCmd {
        &self.g
    }

    fn task(&self) -> Result<TaskRow> {
        let i = self.id.clone();
        self.store.call(move |tx| store::get_task(tx, &i))
    }

    fn apply(&self, e: Ev) -> Result<St> {
        let i = self.id.clone();
        let from = self.task()?.state;
        let name = e.name();
        let m = self.store.call(move |tx| store::transition(tx, &i, e))?;
        eprintln!("   [state] {from:?} --{name}--> {:?}", m.state());
        Ok(m.state())
    }

    fn limits(&self) -> TurnLimits {
        let t = &self.cfg.repo.timeouts;
        TurnLimits {
            max: Duration::from_secs(60 * t.agent_stage_max_min.unwrap_or(30)),
            stall: Duration::from_secs(60 * t.agent_stall_min.unwrap_or(10)),
        }
    }

    fn billing(&self, profile: &str) -> &'static str {
        match self.cfg.profile(profile).map(|p| p.billing) {
            Ok(Billing::Foundry) => "foundry",
            Ok(Billing::Api) => "api",
            _ => "subscription",
        }
    }

    /// Stop request + session id hooks; for a live adapter (Claude) with a budget, also the
    /// R41 live interrupt: the turn is cancelled when its provisional usage reaches what is
    /// left of the budget (the returned flag says it was the budget, not a stop).
    fn hooks(
        &self,
        attempt: &str,
        stop: &StopWatch,
        profile: &str,
    ) -> Result<(TurnHooks, Arc<AtomicBool>)> {
        let (st, a) = (self.store.clone(), attempt.to_string());
        let hit = Arc::new(AtomicBool::new(false));
        let mut h = TurnHooks {
            cancel: Some(stop.flag.clone()),
            on_session: Some(Arc::new(move |sid: &str| {
                let (a, s) = (a.clone(), sid.to_string());
                let _ = st.call(move |tx| store::set_attempt_session(tx, &a, &s));
            })),
            on_usage: None,
        };
        let t = self.task()?;
        let live = self.hub.capabilities(profile)?.live
            && self.cfg.repo.pipeline.budget_live_interrupt.unwrap_or(true);
        if let (true, Some(b)) = (live, t.budget_tokens) {
            let left = (b - t.spent_tokens - t.reserved_tokens).max(0) as u64;
            let (w, flag, h2) = (self.weights(), stop.flag.clone(), hit.clone());
            h.on_usage = Some(Arc::new(move |u: &crate::domain::TokenUsage| {
                if u.buckets.is_some_and(|b| b.budget_tokens(&w) >= left)
                    && !h2.swap(true, Ordering::SeqCst)
                {
                    flag.store(true, Ordering::SeqCst);
                }
            }));
        }
        Ok((h, hit))
    }

    fn weights(&self) -> Weights {
        let m = self
            .cfg
            .repo
            .pipeline
            .budget_weights
            .clone()
            .unwrap_or_default();
        let g = |k: &str| m.get(k).copied().unwrap_or(1.0);
        Weights {
            uncached_input: g("uncached_input"),
            cache_read: g("cache_read"),
            cache_write: g("cache_write"),
            output: g("output"),
        }
    }

    /// R41 admission: a turn starts only while `spent + reserved < budget`; otherwise the
    /// task pauses (`Paused(Budget)`) without starting it. Warns at 80 %.
    fn admitted(&self) -> Result<bool> {
        let t = self.task()?;
        let Some(b) = t.budget_tokens else {
            return Ok(true);
        };
        let used = t.spent_tokens + t.reserved_tokens;
        if used >= b {
            eprintln!(
                "!! admission cap reached: {used} tokens used ({} spent + ~{} reserved) of {b} — paused before the turn (`toryo budget set {} --tokens N`, then `toryo resume {}`)",
                t.spent_tokens, t.reserved_tokens, t.id, t.id
            );
            self.apply(Ev::BudgetExceeded)?;
            return Ok(false);
        }
        if used * 10 >= b * 8 {
            eprintln!("!! budget: {used} of {b} tokens used (≥ 80 %; admission cap, may overshoot by one turn)");
        }
        Ok(true)
    }

    /// The live interrupt fired: the turn's provisional usage is recorded, the attempt is
    /// interrupted and the task pauses.
    fn budget_interrupted(&self, a: &AttemptRow, r: &StageOutcome) -> Result<Flow> {
        eprintln!(
            "!! budget reached during the turn — interrupted (R41 live interrupt), the task pauses"
        );
        self.end_and(
            a,
            AttemptStatus::Interrupted,
            r,
            AttemptEnd {
                error_class: Some("budget".into()),
                error_detail: Some("budget reached during the turn (R41 live interrupt)".into()),
                ..Default::default()
            },
            Some(Ev::BudgetExceeded),
            None,
        )?;
        Ok(Flow::Next)
    }

    fn stopped(&self) -> Result<Flow> {
        self.lock.handle_stop("stopped")?;
        eprintln!("■ stop request handled: the stage attempt is `interrupted`; `toryo resume {}` continues", self.id);
        Ok(Flow::Stopped)
    }

    /// A stop request cancelled the turn (R38): the attempt is `interrupted`.
    fn stopped_turn(&self, a: &AttemptRow, r: &StageOutcome) -> Result<Flow> {
        self.end(a, AttemptStatus::Interrupted, r, AttemptEnd::default())?;
        self.stopped()
    }

    /// A Planner/Reviewer turn cut short by the live budget interrupt (R41) or a stop
    /// request (R38) ends its attempt here.
    fn cut_short(
        &self,
        a: &AttemptRow,
        r: &StageOutcome,
        budget_hit: bool,
    ) -> Result<Option<Flow>> {
        if budget_hit {
            return self.budget_interrupted(a, r).map(Some);
        }
        if r.cancelled {
            return self.stopped_turn(a, r).map(Some);
        }
        Ok(None)
    }

    /// Run until a gate, an escalation, a pause, a stop or a terminal state.
    pub fn run(&mut self) -> Result<RunEnd> {
        loop {
            if self.lock.stop_requested()? {
                self.lock.handle_stop("stopped_at_boundary")?;
                eprintln!("■ stopped at a stage boundary (nothing was interrupted)");
                return Ok(RunEnd::Stopped);
            }
            let t = self.task()?;
            // R35: nothing runs while protected state differs from the baseline in force
            if t.state.is_running() {
                if let Some(reason) = self.protected_violation(&t, false)? {
                    eprintln!("!! {reason}");
                    self.apply(Ev::Escalate { reason })?;
                    continue;
                }
            }
            let flow = match t.state {
                St::New => {
                    self.apply(Ev::Start)?;
                    Flow::Next
                }
                St::Planning => self.plan(&t)?,
                St::WaitingPlanApproval | St::WaitingFinalApproval => {
                    if !self.opts.yes {
                        return Ok(RunEnd::Waiting(t.state));
                    }
                    let kind = if t.state == St::WaitingPlanApproval {
                        GateKind::Plan
                    } else {
                        GateKind::Final
                    };
                    eprintln!("({kind:?} gate approved automatically: --yes)");
                    approve_now(self.store, &t, kind, None)?;
                    Flow::Next
                }
                St::Implementing | St::Fixing => self.implement(&t)?,
                St::Checkpointing => self.checkpoint(&t)?,
                St::Verifying => self.verify(&t)?,
                St::Reviewing => self.review(&t)?,
                St::Escalated | St::Paused => return Ok(RunEnd::Waiting(t.state)),
                St::Completed | St::Cancelled | St::Failed => return Ok(RunEnd::Done(t.state)),
            };
            if let Flow::Stopped = flow {
                return Ok(RunEnd::Stopped);
            }
        }
    }

    fn capture_env(&self) -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    // ── Planning ─────────────────────────────────────────────────────────────────────

    fn plan(&mut self, t: &TaskRow) -> Result<Flow> {
        let art = t.artifacts();
        let prev = {
            let i = self.id.clone();
            self.store
                .call(move |tx| store::last_attempt(tx, &i, "plan"))?
        };
        // R48: the Planner (Claude) resumes its session for a revision or after an interruption
        let resume = plan_resume(prev.as_ref());
        let prompt = plan_prompt(
            t.feedback.as_deref(),
            prev.as_ref().map(|p| p.status),
            &art.join("TASK.md"),
        );
        if !self.admitted()? {
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
        eprintln!("\n── PLAN (attempt {}, {}) ──", a.attempt, t.planner);
        crash_point("plan:after_begin");
        let Some(ctx) = self.context(t, &a, &t.planner)? else {
            return Ok(Flow::Next);
        };
        let stop = self.lock.watch_stop();
        let (hooks, budget_hit) = self.hooks(&a.id, &stop, &t.planner)?;
        let r = self.hub.turn_with(
            OpenSession {
                profile: t.planner.clone(),
                role: Role::Planner,
                cwd: t.worktree.clone(),
                run_store: Some(art.clone()),
                capture_dir: t.captures(),
                schema: Some(contracts::schema("plan.schema.json")?.into()),
                context_file: Some(ctx),
                model: self.role_model("planner"),
                resume,
            },
            TurnInput {
                prompt,
                label: format!("plan-{}", a.attempt),
                echo: true,
                env: self.capture_env(),
            },
            self.limits(),
            hooks,
        )?;
        self.usage(&a, &t.planner, &r)?;
        if let Some(flow) = self.cut_short(&a, &r, budget_hit.load(Ordering::SeqCst))? {
            return Ok(flow);
        }
        if let Some(flow) = self.after_read_only_turn(t, &a, &r, "planner")? {
            return Ok(flow);
        }
        let Some(plan) = r.structured.clone() else {
            let reason = format!(
                "planner returned no structured plan: {:?} {}",
                r.error_class,
                r.error_detail.clone().unwrap_or_default()
            );
            self.end_and(
                &a,
                AttemptStatus::Failed,
                &r,
                AttemptEnd::default(),
                Some(Ev::Escalate { reason }),
                None,
            )?;
            return Ok(Flow::Next);
        };
        let bytes = serde_json::to_vec_pretty(&plan)?;
        let md = super::render::render("plan", &plan)?;
        self.put(t, &a, "plan", "json", &bytes, "plan.json")?;
        self.put(t, &a, "plan-md", "md", md.as_bytes(), "PLAN.md")?;
        eprintln!("\n{md}");
        let bad: Vec<String> = plan_files(&plan)
            .into_iter()
            .filter(|f| self.protect.violates(f))
            .collect();
        if !bad.is_empty() {
            eprintln!("!! the plan lists protected or denied paths {bad:?} — rejected automatically (plan-time policy)");
            let fb = format!("Your plan lists protected or denied paths {bad:?} (toryo.yaml policy), which may not change. Create the plan again without touching them.");
            self.end_and(
                &a,
                AttemptStatus::Succeeded,
                &r,
                AttemptEnd::default(),
                Some(Ev::PlanPolicyViolation { paths: bad }),
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

    fn role_model(&self, role: &str) -> Option<String> {
        self.cfg.repo.roles.get(role).and_then(|r| r.model.clone())
    }

    // ── Implementing / Fixing ────────────────────────────────────────────────────────
    //
    // The implementer of the chain (R49) → prepare the turn → admission (R41) → run it →
    // interpret the result (`ImplementOutcome`) → record the outcome.

    fn implement(&mut self, t: &TaskRow) -> Result<Flow> {
        let wt = Worktree::new(&self.g, &t.worktree);
        // `--no-fallback` never takes the current implementer away (EC-16): no implementer
        // here means the chain was used up
        let Some(profile) = implementer_at(t, t.implementer_index, self.opts.no_fallback) else {
            let reason = implementer_quota_reason(&t.implementers.join(" → "));
            self.apply(Ev::Pause { reason })?;
            return Ok(Flow::Next);
        };
        let turn = self.prepare_implement(t, profile)?;
        if !self.admitted()? {
            return Ok(Flow::Next);
        }
        let Some(run) = self.run_implement_turn(t, &turn, &wt)? else {
            return Ok(Flow::Next); // the task pack failed its hash check: escalated (R34)
        };
        let outcome = self.interpret_implement(t, &turn, &run, &wt)?;
        self.retries = outcome.retries_after(self.retries);
        self.decide_implement(t, &turn, &run, &wt, outcome)
    }

    /// The turn's stage, prompt, model, resume and fallback (R48, R49), and whether the stage
    /// already ran again after a failed turn (EC-15), before admission.
    fn prepare_implement(&self, t: &TaskRow, profile: String) -> Result<ImplementTurn> {
        let stage = writing_stage(t.fix_cycle);
        let kind = self.cfg.profile(&profile)?.kind;
        let (prev, failed_before) = {
            let (i, s) = (self.id.clone(), stage.clone());
            self.store.call(move |tx| {
                let failed = failed_with_changes_before(&store::attempts(tx, &i)?, &s);
                Ok((store::last_attempt(tx, &i, &s)?, failed))
            })?
        };
        let prompt = implementer_prompt(t.fix_cycle, t.feedback.as_deref(), t.handoff.as_deref());
        let resume = implementer_resume(prev.as_ref(), kind, &profile);
        let fallback_from = fallback_from(t, prev.as_ref(), &profile);
        let (prompt, model) = match kind {
            ProfileKind::Codex => (
                prompt,
                self.role_model("implementer").or_else(|| {
                    self.cfg
                        .profile(&profile)
                        .ok()
                        .and_then(|p| p.model.clone())
                }),
            ),
            ProfileKind::Claude => (claude_implementer_prompt(&prompt), None),
        };
        Ok(ImplementTurn {
            stage,
            profile,
            prompt,
            model,
            resume,
            fallback_from,
            failed_before,
        })
    }

    /// Begin the attempt at the current HEAD with an empty TEMP (R46), assemble its context
    /// (§7), run the turn and record its usage (R41). `None`: the context could not be
    /// assembled and the task escalated.
    fn run_implement_turn(
        &self,
        t: &TaskRow,
        turn: &ImplementTurn,
        wt: &Worktree,
    ) -> Result<Option<ImplementRun>> {
        let head_before = wt.head()?;
        let tmp = t.worktree.join(".toryo-tmp");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        let env: BTreeMap<String, String> = [("TEMP", &tmp), ("TMP", &tmp)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.display().to_string()))
            .collect();
        let a = self.begin(NewAttempt {
            stage: &turn.stage,
            kind: "agent",
            profile: Some(&turn.profile),
            fallback_from: turn.fallback_from.as_deref(),
            head_before: Some(&head_before),
            subject_commit: None,
        })?;
        eprintln!(
            "\n── {} (attempt {}, {}) ──",
            turn.stage.to_uppercase(),
            a.attempt,
            turn.profile
        );
        crash_point("implement:after_begin");
        let Some(ctx) = self.context(t, &a, &turn.profile)? else {
            return Ok(None);
        };
        let stop = self.lock.watch_stop();
        let (hooks, budget_hit) = self.hooks(&a.id, &stop, &turn.profile)?;
        let r = self.hub.turn_with(
            OpenSession {
                profile: turn.profile.clone(),
                role: Role::Implementer,
                cwd: t.worktree.clone(),
                run_store: None,
                capture_dir: t.captures(),
                schema: Some(contracts::schema("implementation.schema.json")?.into()),
                context_file: Some(ctx),
                model: turn.model.clone(),
                resume: turn.resume.clone(),
            },
            TurnInput {
                prompt: turn.prompt.clone(),
                label: format!("{}-{}", turn.stage, a.attempt),
                env,
                echo: true,
            },
            self.limits(),
            hooks,
        )?;
        crash_point("implement:after_turn");
        self.usage(&a, &turn.profile, &r)?;
        Ok(Some(ImplementRun {
            budget_hit: budget_hit.load(Ordering::SeqCst),
            attempt: a,
            result: r,
            head_before,
            stop,
        }))
    }

    /// What the finished turn leads to, in this order: the HEAD guard, whatever the turn's
    /// end (EC-18: an agent commit is undone before a stop, a budget interrupt, a retry or a
    /// switch hands the tree on); the live budget interrupt or a stop; then the
    /// implementation report is stored; quota and rate limits (retry, then switch); then
    /// protected state (R35) and the path policy (§8), which also decides what a failed turn
    /// leads to (EC-15).
    fn interpret_implement(
        &self,
        t: &TaskRow,
        turn: &ImplementTurn,
        run: &ImplementRun,
        wt: &Worktree,
    ) -> Result<ImplementOutcome> {
        let r = &run.result;
        if let Some(reason) = self.head_guard(t, wt, run)? {
            return Ok(ImplementOutcome::GitViolation { reason });
        }
        if let Some(o) = ImplementOutcome::interrupted(r, run.budget_hit) {
            return Ok(o);
        }
        if let Some(v) = &r.structured {
            self.put(
                t,
                &run.attempt,
                "implementation",
                "json",
                &serde_json::to_vec_pretty(v)?,
                &format!("implementation-{}.json", t.fix_cycle),
            )?;
        }
        // quota / rate limits: retry, then the next implementer (sticky, R49)
        if let Some(o) = ImplementOutcome::quota(r, self.retries, retry_base_secs()) {
            return Ok(o);
        }
        // R35: untracked/ignored protected files or Git metadata → Escalated, nothing else runs
        if let Some(reason) = self.protected_violation(t, true)? {
            return Ok(ImplementOutcome::ProtectedChanged { reason });
        }
        Ok(ImplementOutcome::after_checks(
            r,
            &wt.changed()?,
            &self.protect,
            turn.failed_before,
        ))
    }

    /// Record the outcome: end the attempt, apply the event, set the feedback — with the
    /// same transactions and output as before the split into steps.
    fn decide_implement(
        &self,
        t: &TaskRow,
        turn: &ImplementTurn,
        run: &ImplementRun,
        wt: &Worktree,
        outcome: ImplementOutcome,
    ) -> Result<Flow> {
        let (a, r) = (&run.attempt, &run.result);
        match outcome {
            ImplementOutcome::BudgetHit => self.budget_interrupted(a, r),
            ImplementOutcome::Stopped => self.stopped_turn(a, r),
            ImplementOutcome::Retry { retry, wait } => {
                eprintln!(
                    "!! {}: {:?} — retry {retry}/{RATE_LIMIT_RETRIES} in {}s",
                    turn.profile,
                    r.error_class,
                    wait.as_secs()
                );
                self.end(a, AttemptStatus::Failed, r, AttemptEnd::default())?;
                // a stop request cuts the backoff short (EC-17, R38); the run loop then
                // stops at this stage boundary, before the retry starts
                if backoff(&run.stop.flag, wait) {
                    eprintln!("■ stop requested during the backoff: the retry does not start");
                }
                Ok(Flow::Next)
            }
            ImplementOutcome::Switch { reason } => {
                self.switch_implementer(t, &turn.profile, run, wt, reason)
            }
            ImplementOutcome::GitViolation { reason } => {
                eprintln!("!! {reason}");
                self.end_and(
                    a,
                    AttemptStatus::Failed,
                    r,
                    AttemptEnd {
                        error_class: Some("policy_violation".into()),
                        error_detail: Some(reason.clone()),
                        ..Default::default()
                    },
                    Some(Ev::Escalate { reason }),
                    None,
                )?;
                Ok(Flow::Next)
            }
            ImplementOutcome::ProtectedChanged { reason } => {
                eprintln!("!! {reason}");
                self.end_and(
                    a,
                    AttemptStatus::Failed,
                    r,
                    AttemptEnd {
                        error_class: Some(protect::REASON.into()),
                        ..Default::default()
                    },
                    Some(Ev::Escalate { reason }),
                    None,
                )?;
                Ok(Flow::Next)
            }
            ImplementOutcome::PathPolicy { paths } => {
                eprintln!("!! protected or denied paths changed: {paths:?} — restored, nothing committed; sent back to the implementer");
                wt.restore(&paths)?;
                let fb = format!("- POLICY: you changed protected or denied paths {paths:?}. They were restored. Do not modify them; achieve the task another way.");
                self.end_and(
                    a,
                    AttemptStatus::Failed,
                    r,
                    AttemptEnd {
                        error_class: Some("policy_violation".into()),
                        error_detail: Some(format!("{paths:?}")),
                        ..Default::default()
                    },
                    Some(Ev::PathPolicyViolation { paths }),
                    Some(Some(fb)),
                )?;
                Ok(Flow::Next)
            }
            ImplementOutcome::Failed { reason } => {
                r.print_summary();
                self.end_and(
                    a,
                    AttemptStatus::Failed,
                    r,
                    AttemptEnd::default(),
                    Some(Ev::Escalate { reason }),
                    None,
                )?;
                Ok(Flow::Next)
            }
            ImplementOutcome::FailedWithChanges { reason, retry } => {
                r.print_summary();
                self.keep_failed_work(t, run, wt, reason, retry)
            }
            ImplementOutcome::Done => {
                let i = self.id.clone();
                self.end_and(
                    a,
                    AttemptStatus::Succeeded,
                    r,
                    AttemptEnd::default(),
                    Some(Ev::ImplementationDone),
                    None,
                )?;
                self.store
                    .call(move |tx| store::set_handoff(tx, &i, None))?;
                Ok(Flow::Next)
            }
        }
    }

    /// HEAD guard (§8; R12, R25) after every Implementer turn, whatever its end (EC-18):
    /// agents must not commit. An agent commit on the task branch is soft-reset to where the
    /// turn began — its changes stay, uncommitted, so the path policy and the deny check
    /// before the checkpoint see them. An unfinished Git operation, another branch, a
    /// detached or unresolvable HEAD, or history rewritten below the base cannot be repaired
    /// that way (a reset would move another branch): nothing is reset and the escalation
    /// reason is returned. Both are recorded as a `policy_violation` event.
    fn head_guard(&self, t: &TaskRow, wt: &Worktree, run: &ImplementRun) -> Result<Option<String>> {
        let unfinished = unfinished_git_operation(wt)?;
        let head_ref = wt.head_ref()?;
        let head = wt.head().ok();
        let moved = head.as_deref().is_some_and(|h| h != run.head_before);
        let git = GitAfterTurn {
            unfinished,
            head_ref,
            head,
            descends_from_base: !moved || wt.contains(&t.base_commit)?,
        };
        let (payload, escalation) = match HeadGuard::of(
            &git,
            &t.branch,
            &t.base_commit,
            &run.head_before,
        ) {
            HeadGuard::Unchanged => return Ok(None),
            HeadGuard::Reset { head } => {
                eprintln!(
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

    /// EC-15 (R31, §11): the changes of a failed turn never become a normal checkpoint. They
    /// are kept as a recovery checkpoint (`Toryo-Partial: true`, §8: never verified or
    /// reviewed on its own), the attempt is `failed` with its error class, and a hand-over
    /// note names the partial commit. The stage then runs again from it (`retry`), else the
    /// task escalates. The commit comes first: after a crash in between, recovery finds it
    /// by its trailers (§10).
    fn keep_failed_work(
        &self,
        t: &TaskRow,
        run: &ImplementRun,
        wt: &Worktree,
        reason: String,
        retry: bool,
    ) -> Result<Flow> {
        let (a, r) = (&run.attempt, &run.result);
        let sha = wt.commit(
            &format!(
                "toryo({}): {} (recovery checkpoint of failed attempt {})",
                t.id, a.stage, a.attempt
            ),
            &[
                (T_TASK, t.id.clone()),
                (T_STAGE, a.stage.clone()),
                (T_ATTEMPT, a.attempt.to_string()),
                (T_AGENT, a.profile.clone().unwrap_or_default()),
                (super::worktree::T_PARTIAL, "true".into()),
            ],
            false,
        )?;
        let kept = format!(
            "its partial work is kept in the recovery checkpoint {} (Toryo-Partial)",
            short(&sha)
        );
        let escalation = (!retry).then(|| format!("{reason}; {kept}"));
        match &escalation {
            Some(why) => eprintln!("!! {why}"),
            None => eprintln!("!! {reason}; {kept} — the stage runs again from it (once, §11)"),
        }
        let note = failed_turn_note(a, r, &sha);
        let end = AttemptEnd {
            error_class: r.error_class.map(|c| format!("{c:?}")),
            error_detail: r.error_detail.clone(),
            external_session_id: r.session_id.clone(),
            partial_commit: Some(sha),
            ..Default::default()
        };
        let (i, aid) = (self.id.clone(), a.id.clone());
        let from = self.task()?.state;
        let to = self.store.call(move |tx| {
            store::finish_attempt(tx, &aid, AttemptStatus::Failed, &end)?;
            store::set_handoff(tx, &i, Some(&note))?;
            match escalation {
                Some(reason) => Ok(Some(
                    store::transition(tx, &i, Ev::Escalate { reason })?.state(),
                )),
                None => Ok(None),
            }
        })?;
        if let Some(to) = to {
            eprintln!("   [state] {from:?} --escalate--> {to:?}");
        }
        Ok(Flow::Next)
    }

    /// R49: the next implementer of the chain takes over (sticky), with a hand-over note that
    /// lists the uncommitted partial changes; with none left (or `--no-fallback`), the task
    /// pauses. The attempt keeps the turn's real error class and detail (CODE-03: a switch
    /// after rate-limit retries is not a quota). One transaction.
    fn switch_implementer(
        &self,
        t: &TaskRow,
        profile: &str,
        run: &ImplementRun,
        wt: &Worktree,
        reason: String,
    ) -> Result<Flow> {
        let changed: Vec<String> = wt
            .changed()?
            .into_iter()
            .map(|(s, p)| format!("{s} {p}"))
            .collect();
        let next = implementer_at(t, t.implementer_index + 1, self.opts.no_fallback);
        eprintln!(
            "!! implementer {profile}: {reason} → {}",
            next.as_deref()
                .map(|n| format!("switching to {n}"))
                .unwrap_or_else(|| "no fallback left".into())
        );
        let note = handover_note(profile, &reason, &changed);
        let (i, aid, idx) = (
            self.id.clone(),
            run.attempt.id.clone(),
            t.implementer_index + 1,
        );
        let end = AttemptEnd {
            error_class: run.result.error_class.map(|c| format!("{c:?}")),
            error_detail: run.result.error_detail.clone(),
            external_session_id: run.result.session_id.clone(),
            ..Default::default()
        };
        let pause = next.is_none();
        let payload =
            json!({"from": profile, "to": next, "reason": reason, "partial_changes": changed});
        self.store.call(move |tx| {
            store::finish_attempt(tx, &aid, AttemptStatus::Failed, &end)?;
            store::set_implementer_index(tx, &i, idx)?;
            store::set_handoff(tx, &i, Some(&note))?;
            store::event(tx, Some(&i), Some(&aid), "implementer_switch", &payload)?;
            if pause {
                store::transition(
                    tx,
                    &i,
                    Ev::Pause {
                        reason: implementer_quota_reason(&reason),
                    },
                )?;
            }
            Ok(())
        })?;
        Ok(Flow::Next)
    }

    // ── Checkpointing (idempotent via the trailer search, R40) ───────────────────────

    fn checkpoint(&mut self, t: &TaskRow) -> Result<Flow> {
        let stage = writing_stage(t.fix_cycle);
        let wt = Worktree::new(&self.g, &t.worktree);
        let a = {
            let (i, s) = (self.id.clone(), stage.clone());
            self.store.call(move |tx| store::last_attempt(tx, &i, &s))?
        }
        .with_context(|| format!("no `{stage}` attempt to checkpoint"))?;
        let implementer = a.profile.clone().unwrap_or_default();
        let (head, kind) = match wt.find_checkpoint(&t.base_commit, &t.id, &stage, a.attempt)? {
            Some(c) if !c.partial => {
                eprintln!(
                    "✔ checkpoint {} already committed (trailer match)",
                    short(&c.sha)
                );
                (c.sha, if c.no_change { "no_change" } else { "normal" })
            }
            _ => {
                // §8: deny paths are checked again right before the commit
                let denied: Vec<String> = wt
                    .changed()?
                    .into_iter()
                    .map(|c| c.1)
                    .filter(|p| self.protect.is_denied(p))
                    .collect();
                if !denied.is_empty() {
                    let reason = format!("policy_violation: denied paths {denied:?} would be committed (policy.deny_paths); nothing was committed");
                    eprintln!("!! {reason}");
                    self.apply(Ev::Escalate { reason })?;
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
                (sha, if no_change { "no_change" } else { "normal" })
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
        eprintln!("✔ checkpoint {} ({kind})", short(&head));
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
            eprintln!("   protected baseline renewed: {allowed:?} (## Allow protected)");
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

    // ── Verifying ────────────────────────────────────────────────────────────────────

    fn verify(&mut self, t: &TaskRow) -> Result<Flow> {
        let head = t
            .machine
            .checkpoint()
            .context("no checkpoint to verify")?
            .to_string();
        let a = self.begin(NewAttempt {
            stage: "verify",
            kind: "command",
            profile: None,
            fallback_from: None,
            head_before: Some(&head),
            subject_commit: Some(&head),
        })?;
        crash_point("verify:after_begin");
        let Some((b, co)) = self.verify_checkout(t, &a, &head)? else {
            return Ok(Flow::Next);
        };
        eprintln!(
            "\n── VERIFY (attempt {}, {} in {}) ──",
            a.attempt,
            verify::describe(b),
            co.dir.display()
        );
        let stop = self.lock.watch_stop();
        let Some((results, pass)) = self.run_verify_commands(b, &co, &stop)? else {
            self.finish_and(
                &a,
                AttemptStatus::Interrupted,
                AttemptEnd::default(),
                None,
                None,
            )?;
            return self.stopped();
        };
        // R29/R47: the checkout must be exactly the checkpoint after the run
        let mutated = verify::mutated(&self.g, &co, &head)?;
        let tree = self.g.run_str(
            &Worktree::new(&self.g, &t.worktree).loc,
            true,
            &["rev-parse", &format!("{head}^{{tree}}")],
        )?;
        let vr = json!({
            "head": head,
            "tree": tree.trim(),
            "pass": pass && mutated.is_none(),
            "boundary": verify::describe(b),
            "checkout": self.cfg.repo.verify.checkout,
            "verify_checkout": co.dir,
            "config_sha256": t.config_sha256,
            "scripts": verify::script_hashes(&verify_files(&self.cmds, &co.dir), &co),
            "mutated": mutated,
            "commands": results,
        });
        self.put(
            t,
            &a,
            "verify-result",
            "json",
            &serde_json::to_vec_pretty(&vr)?,
            "VERIFY_RESULT.json",
        )?;
        if let Some(reason) = mutated {
            eprintln!("!! {reason}");
            self.escalate_attempt(&a, verify::MUTATED.into(), reason)?;
            return Ok(Flow::Next);
        }
        // R35: the verify run must not have touched protected state
        if let Some(reason) = self.protected_violation(t, false)? {
            eprintln!("!! {reason}");
            self.escalate_attempt(&a, protect::REASON.into(), reason)?;
            return Ok(Flow::Next);
        }
        let (status, feedback, event) = if pass {
            (AttemptStatus::Succeeded, None, Ev::VerifyPassed { head })
        } else {
            (
                AttemptStatus::Failed,
                Some(Some(verify_feedback(&results))),
                Ev::VerifyFailed { head },
            )
        };
        self.finish_and(&a, status, AttemptEnd::default(), feedback, Some(event))?;
        Ok(Flow::Next)
    }

    /// R36/R45: the execution boundary and the detached checkout of `head` are established,
    /// or the stage stops — never a silent downgrade. `None`: the boundary is unavailable or
    /// the checkout was mutated, and the task escalated.
    fn verify_checkout(
        &self,
        t: &TaskRow,
        a: &AttemptRow,
        head: &str,
    ) -> Result<Option<(Boundary, verify::Checkout)>> {
        let prepared = verify::boundary(&self.cfg)
            .and_then(|b| verify::prepare(&self.g, t, &self.cfg, head, b).map(|c| (b, c)));
        match prepared {
            Ok(x) => Ok(Some(x)),
            Err(e)
                if format!("{e:#}").starts_with(verify::UNAVAILABLE)
                    || format!("{e:#}").starts_with(verify::MUTATED) =>
            {
                let reason = format!("{e:#}");
                eprintln!("!! {reason}");
                self.escalate_attempt(a, reason_class(&reason, verify::UNAVAILABLE), reason)?;
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Run the verify commands in the checkout, inside the boundary, and stop at the first
    /// failure (§5.2). Returns each command's result and whether all passed; `None` when a
    /// stop request ended a command (R38).
    fn run_verify_commands(
        &self,
        b: Boundary,
        co: &verify::Checkout,
        stop: &StopWatch,
    ) -> Result<Option<(Vec<Value>, bool)>> {
        let env = verify::env(co);
        let mut results = Vec::new();
        let mut pass = true;
        for c in &self.cmds {
            let program = resolve_program(&expand_vars(&c.program)?);
            let res = self.hub.block_on(LocalRunner.run(
                RunSpec {
                    name: c.name.clone(),
                    program,
                    args: c.args.clone(),
                    cwd: co.dir.clone(),
                    env: env.clone(),
                    boundary: b,
                    timeout: Duration::from_secs(60 * c.timeout_min.unwrap_or(30)),
                    cancel: Some(stop.flag.clone()),
                },
                None,
            ))?;
            if res.cancelled || stop.stopped() {
                return Ok(None);
            }
            let ok = res.ok();
            pass &= ok;
            eprintln!(
                "  {} {} (exit {:?}, {:.1}s)",
                if ok { "✔" } else { "✘" },
                c.name,
                res.exit,
                res.duration_ms as f64 / 1000.0
            );
            // R50: LocalRunner strips ANSI (CSI/OSC) from everything it captures
            results.push(command_result(c, &res));
            if !ok {
                break; // stop at the first failure (§5.2)
            }
        }
        Ok(Some((results, pass)))
    }

    // ── Reviewing ────────────────────────────────────────────────────────────────────
    //
    // The review turn and its correction turns (`ReviewTurns`) → the decision of a valid
    // review (`ReviewVerdict`) → record it.

    fn review(&mut self, t: &TaskRow) -> Result<Flow> {
        let head = t
            .machine
            .checkpoint()
            .context("no checkpoint to review")?
            .to_string();
        // R48: a review of the same checkpoint that was interrupted (crash, stop) resumes the
        // Reviewer's session
        let prev = {
            let i = self.id.clone();
            self.store
                .call(move |tx| store::last_attempt(tx, &i, "review"))?
        };
        let resume = review_resume(prev.as_ref(), &head, &t.reviewer);
        if !self.admitted()? {
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
        eprintln!(
            "\n── REVIEW (attempt {}, {}{}) ──",
            a.attempt,
            t.reviewer,
            if resume.is_some() {
                ", resuming the interrupted session"
            } else {
                ""
            }
        );
        crash_point("review:after_begin");
        let Some(ctx) = self.context(t, &a, &t.reviewer)? else {
            return Ok(Flow::Next);
        };
        let stop = self.lock.watch_stop();
        let prompt = review_prompt(&head, &t.acceptance_ids, resume.is_some());
        match self.review_turns(t, &a, &ctx, &stop, prompt, resume)? {
            ReviewTurns::Valid(valid) => self.decide_review(t, &a, &head, valid),
            ReviewTurns::Rejected { last } => {
                let reason = format!(
                    "InvalidOutput: the review was rejected {} times (R22)",
                    MAX_CORRECTIONS + 1
                );
                self.end_and(
                    &a,
                    AttemptStatus::Failed,
                    &last,
                    AttemptEnd {
                        error_class: Some("InvalidOutput".into()),
                        correction_turns: Some(MAX_CORRECTIONS),
                        ..Default::default()
                    },
                    Some(Ev::Escalate { reason }),
                    None,
                )?;
                Ok(Flow::Next)
            }
            ReviewTurns::Ended(flow) => Ok(flow),
        }
    }

    /// The review turn and up to two correction turns in the same session (R22): a review
    /// that fails the checks (R28) is sent back with the violations. Every turn is admitted
    /// (R41) and checked like any read-only turn. A turn that failed is checked like any
    /// other (EC-01), and a crash or timeout also gets a correction turn (EC-14).
    fn review_turns(
        &self,
        t: &TaskRow,
        a: &AttemptRow,
        ctx: &Path,
        stop: &StopWatch,
        mut prompt: String,
        mut resume: Option<String>,
    ) -> Result<ReviewTurns> {
        let mut last = StageOutcome::default();
        for corr in 0..=MAX_CORRECTIONS {
            // R41: every turn is admitted, correction turns too (EC-13: a refusal has already
            // applied `BudgetExceeded`, so the transition below is refused)
            if corr > 0 && !self.admitted()? {
                self.end_and(
                    a,
                    AttemptStatus::Failed,
                    &last,
                    AttemptEnd {
                        error_class: Some("budget".into()),
                        ..Default::default()
                    },
                    Some(Ev::BudgetExceeded),
                    None,
                )?;
                return Ok(ReviewTurns::Ended(Flow::Next));
            }
            let (hooks, budget_hit) = self.hooks(&a.id, stop, &t.reviewer)?;
            let r = self.hub.turn_with(
                OpenSession {
                    profile: t.reviewer.clone(),
                    role: Role::Reviewer,
                    cwd: t.worktree.clone(),
                    run_store: Some(t.artifacts()),
                    capture_dir: t.captures(),
                    schema: Some(contracts::schema("review.schema.json")?.into()),
                    context_file: Some(ctx.to_path_buf()),
                    model: self.role_model("reviewer"),
                    resume: resume.clone(),
                },
                TurnInput {
                    prompt: prompt.clone(),
                    label: format!("review-{}-{corr}", a.attempt),
                    echo: true,
                    env: self.capture_env(),
                },
                self.limits(),
                hooks,
            )?;
            self.usage(a, &t.reviewer, &r)?;
            if let Some(flow) = self.cut_short(a, &r, budget_hit.load(Ordering::SeqCst))? {
                return Ok(ReviewTurns::Ended(flow));
            }
            if let Some(flow) = self.after_read_only_turn(t, a, &r, "reviewer")? {
                return Ok(ReviewTurns::Ended(flow));
            }
            let review = r.structured.clone().unwrap_or(Value::Null);
            let viol = review_violations(&review, &t.acceptance_ids);
            last = r;
            if viol.is_empty() {
                return Ok(ReviewTurns::Valid(ValidReview {
                    review,
                    last,
                    corrections: corr,
                }));
            }
            eprintln!(
                "!! invalid review (correction {}/{MAX_CORRECTIONS}): {viol:?}",
                corr + 1
            );
            prompt = correction_prompt(&viol);
            resume = last.session_id.clone();
        }
        Ok(ReviewTurns::Rejected { last })
    }

    /// Publish a valid review (review.json, REVIEW.md) and apply its decision: an
    /// inconsistent approval is downgraded to `changes_requested`, and an approval's
    /// non-blocking issues become follow-ups (R53).
    fn decide_review(
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
        let md = super::render::render("review", &review)?;
        self.put(
            t,
            a,
            "review",
            "json",
            &serde_json::to_vec_pretty(&review)?,
            "review.json",
        )?;
        self.put(t, a, "review-md", "md", md.as_bytes(), "REVIEW.md")?;
        eprintln!("\n{md}");
        let verdict = ReviewVerdict::of(&review);
        if let Some(unmet) = &verdict.downgraded {
            eprintln!("!! review_inconsistent: `approved` with unmet {unmet:?} / blocking issues → changes_requested");
        }
        let (i, h) = (self.id.clone(), head.to_string());
        self.store
            .call(move |tx| store::set_last_reviewed(tx, &i, &h))?;
        if verdict.decision == ReviewDecision::Approved {
            // R53: non-blocking issues survive an approval as follow-ups
            let nb = non_blocking(&review);
            if !nb.is_empty() {
                super::decisions::append(
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
        let feedback = (verdict.decision == ReviewDecision::ChangesRequested)
            .then(|| review_feedback(&review));
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
        Ok(Flow::Next)
    }

    // ── protected state (R35) and pauses (R49) ───────────────────────────────────────

    /// Compare the protected state with the baseline in force. `only_hard`: right after an
    /// Implementer turn, where tracked protected changes go to the path policy instead.
    /// Changes the task may make (`## Allow protected`, tracked) never count. Returns the
    /// escalation reason.
    fn protected_violation(&self, t: &TaskRow, only_hard: bool) -> Result<Option<String>> {
        let c = protect::check(self.store, t, &self.g, &self.protect)?;
        let bad: Vec<&protect::Change> = c
            .changes
            .iter()
            .filter(|x| x.is_hard() || (!only_hard && !self.protect.is_allowed(&x.path)))
            .collect();
        if bad.is_empty() {
            return Ok(None);
        }
        let (i, payload) = (
            self.id.clone(),
            json!({"changes": bad, "baseline": c.baseline_id, "diff": c.diff_path}),
        );
        self.store
            .call(move |tx| store::event(tx, Some(&i), None, "protected_changed", &payload))?;
        Ok(Some(protect::escalation_reason(
            &t.id,
            &bad,
            c.diff_path.as_deref(),
        )))
    }

    /// After a Planner/Reviewer turn: quota → `Paused` (no Planner/Reviewer fallback in
    /// v0.1, R49); a changed protected state → `Escalated`.
    fn after_read_only_turn(
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
            eprintln!("!! {reason}");
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
        if let Some(reason) = self.protected_violation(t, false)? {
            eprintln!("!! {reason}");
            self.end_and(
                a,
                AttemptStatus::Failed,
                r,
                AttemptEnd {
                    error_class: Some(protect::REASON.into()),
                    ..Default::default()
                },
                Some(Ev::Escalate { reason }),
                None,
            )?;
            return Ok(Some(Flow::Next));
        }
        Ok(None)
    }

    // ── attempt helpers ──────────────────────────────────────────────────────────────

    /// Publish an immutable artifact version for this attempt (R34) + its view.
    fn put(
        &self,
        t: &TaskRow,
        a: &AttemptRow,
        logical: &str,
        ext: &str,
        bytes: &[u8],
        view: &str,
    ) -> Result<super::artifacts::Version> {
        super::artifacts::put(
            self.store,
            t,
            Some(&a.id),
            logical,
            ext,
            logical,
            bytes,
            Some(view),
        )
    }

    /// Assemble the attempt's context (§7), store it as `context/<stage>-<attempt>` and
    /// record its hash on the attempt.
    fn context(&self, t: &TaskRow, a: &AttemptRow, profile: &str) -> Result<Option<PathBuf>> {
        // R34: the task pack is hash-checked before it is handed over
        let req = match context::request(self.store, t, &self.cfg, &a.stage, profile) {
            Ok(r) => r,
            Err(e) if format!("{e:#}").starts_with("artifact_") => {
                let reason = format!("{e:#}");
                eprintln!("!! {reason}");
                self.escalate_attempt(a, reason_class(&reason, "artifact_tampered"), reason)?;
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let c = context::assemble(&req)?;
        for w in &c.warnings {
            eprintln!("!! {w}");
        }
        eprintln!(
            "   context: {} bytes, {} sources, sha256 {}",
            c.text.len(),
            c.sources.len(),
            short(&c.sha256)
        );
        let logical = format!("context/{}-{}", a.stage, a.attempt);
        let v = super::artifacts::put(
            self.store,
            t,
            Some(&a.id),
            &logical,
            "md",
            "context",
            c.text.as_bytes(),
            Some(&format!("context/{}-{}.md", a.stage, a.attempt)),
        )?;
        let (tid, aid, sha) = (t.id.clone(), a.id.clone(), c.sha256.clone());
        let payload = json!({"sha256": c.sha256, "artifact": v.id, "sources": c.sources, "skipped": c.skipped, "warnings": c.warnings, "budget": c.budget});
        self.store.call(move |tx| {
            store::set_attempt_context(tx, &aid, &sha)?;
            store::event(tx, Some(&tid), Some(&aid), "context_assembled", &payload)?;
            Ok(())
        })?;
        // the agent reads the immutable version, never the view (R34)
        Ok(Some(v.path))
    }

    fn begin(&self, a: NewAttempt) -> Result<AttemptRow> {
        let i = self.id.clone();
        let (stage, kind, profile, fb, hb, sc) = (
            a.stage.to_string(),
            a.kind.to_string(),
            a.profile.map(str::to_string),
            a.fallback_from.map(str::to_string),
            a.head_before.map(str::to_string),
            a.subject_commit.map(str::to_string),
        );
        self.store.call(move |tx| {
            let a = store::begin_attempt(
                tx,
                &i,
                &NewAttempt {
                    stage: &stage,
                    kind: &kind,
                    profile: profile.as_deref(),
                    fallback_from: fb.as_deref(),
                    head_before: hb.as_deref(),
                    subject_commit: sc.as_deref(),
                },
            )?;
            // R35: the baseline version in force when the attempt started
            tx.execute(
                "UPDATE stage_attempts SET protected_manifest_id = (SELECT artifact_id FROM artifact_heads WHERE task_id = ?2 AND logical_name = 'protected') WHERE id = ?1",
                rusqlite::params![a.id, i],
            )?;
            Ok(a)
        })
    }

    /// Record the turn's usage (R41): cumulative adapters store the delta against the
    /// session baseline; a turn that did not complete counts its streamed usage as provisional.
    fn usage(&self, a: &AttemptRow, profile: &str, r: &StageOutcome) -> Result<()> {
        let caps = self.hub.capabilities(profile)?;
        let mut u = r.usage;
        if let Some(x) = &mut u {
            if !r.success && (r.cancelled || r.timed_out) && x.accounting == Accounting::Exact {
                x.accounting = Accounting::Provisional;
            }
        }
        let reservation = self
            .cfg
            .repo
            .pipeline
            .unknown_turn_reservation_tokens
            .as_ref()
            .and_then(|v| v.as_u64());
        let (i, aid, stage, p, b, sess, w) = (
            self.id.clone(),
            a.id.clone(),
            a.stage.clone(),
            profile.to_string(),
            self.billing(profile),
            r.session_id.clone(),
            self.weights(),
        );
        self.store.call(move |tx| {
            store::record_usage_in(
                tx,
                &store::UsageIn {
                    task: &i,
                    attempt: &aid,
                    stage: &stage,
                    profile: &p,
                    billing: b,
                    session: sess.as_deref(),
                    cumulative: caps.cumulative_on_resume == Tri::Yes,
                    killed_included: caps.killed_turn_in_resume == Tri::Yes,
                    usage: u,
                    weights: w,
                    reservation,
                },
            )
            .map(|_| ())
        })
    }

    fn end(
        &self,
        a: &AttemptRow,
        status: AttemptStatus,
        r: &StageOutcome,
        end: AttemptEnd,
    ) -> Result<()> {
        self.end_and(a, status, r, end, None, None)
    }

    /// Finish the attempt, optionally set the feedback and apply a transition — one transaction.
    fn end_and(
        &self,
        a: &AttemptRow,
        status: AttemptStatus,
        r: &StageOutcome,
        mut end: AttemptEnd,
        e: Option<Ev>,
        feedback: Option<Option<String>>,
    ) -> Result<()> {
        if end.error_class.is_none() {
            end.error_class = r.error_class.map(|c| format!("{c:?}"));
        }
        if end.error_detail.is_none() {
            end.error_detail = r.error_detail.clone();
        }
        end.external_session_id = r.session_id.clone();
        let (i, aid) = (self.id.clone(), a.id.clone());
        let from = self.task()?.state;
        let name = e.as_ref().map(|e| e.name());
        let to = self.store.call(move |tx| {
            store::finish_attempt(tx, &aid, status, &end)?;
            if let Some(f) = feedback {
                store::set_feedback(tx, &i, f.as_deref())?;
            }
            match e {
                Some(e) => Ok(Some(store::transition(tx, &i, e)?.state())),
                None => Ok(None),
            }
        })?;
        if let (Some(to), Some(name)) = (to, name) {
            eprintln!("   [state] {from:?} --{name}--> {to:?}");
        }
        Ok(())
    }

    /// Finish the attempt with `end` as given, optionally set the feedback and apply a
    /// transition — one transaction. Unlike `end_and` there is no turn outcome to take the
    /// error or the session from, and no `[state]` line is printed.
    fn finish_and(
        &self,
        a: &AttemptRow,
        status: AttemptStatus,
        end: AttemptEnd,
        feedback: Option<Option<String>>,
        e: Option<Ev>,
    ) -> Result<()> {
        let (i, aid) = (self.id.clone(), a.id.clone());
        self.store.call(move |tx| {
            store::finish_attempt(tx, &aid, status, &end)?;
            if let Some(f) = feedback {
                store::set_feedback(tx, &i, f.as_deref())?;
            }
            if let Some(e) = e {
                store::transition(tx, &i, e)?;
            }
            Ok(())
        })
    }

    /// Fail the attempt with `class` and escalate the task with `reason` (one transaction;
    /// the caller prints the reason).
    fn escalate_attempt(&self, a: &AttemptRow, class: String, reason: String) -> Result<()> {
        self.finish_and(
            a,
            AttemptStatus::Failed,
            AttemptEnd {
                error_class: Some(class),
                ..Default::default()
            },
            None,
            Some(Ev::Escalate { reason }),
        )
    }
}

// ── stage decisions: pure, unit-tested ───────────────────────────────────────────────

/// R48: the Planner resumes the session of its previous attempt, whatever its profile
/// (EC-30).
fn plan_resume(prev: Option<&AttemptRow>) -> Option<String> {
    prev.and_then(|p| p.external_session_id.clone())
}

/// The Planner's prompt: the feedback on a revision, else the task (again, after an
/// interrupted turn).
fn plan_prompt(feedback: Option<&str>, prev: Option<AttemptStatus>, task_md: &Path) -> String {
    match (feedback, prev) {
        (Some(f), _) => f.to_string(),
        (None, Some(AttemptStatus::Interrupted)) => format!(
            "Your previous turn was interrupted before it finished. Create the plan for the task in {} again and return it.",
            task_md.display()
        ),
        _ => format!("Create the plan for the task in {}.", task_md.display()),
    }
}

/// The plan's `files_likely_involved` (checked against the path policy at plan time).
fn plan_files(plan: &Value) -> Vec<String> {
    plan["files_likely_involved"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

/// An Implementer turn as prepared before admission (R48, R49).
struct ImplementTurn {
    /// `implement` or `fix-N`.
    stage: String,
    profile: String,
    prompt: String,
    model: Option<String>,
    /// R48: the interrupted Claude session to resume.
    resume: Option<String>,
    /// R49: the implementer this attempt replaces.
    fallback_from: Option<String>,
    /// An earlier attempt of the stage failed and left changes (EC-15): the stage already
    /// ran again once, so another such failure escalates (§11).
    failed_before: bool,
}

/// A finished Implementer turn and its attempt.
struct ImplementRun {
    attempt: AttemptRow,
    result: StageOutcome,
    /// HEAD when the attempt began: the HEAD guard's reference.
    head_before: String,
    /// The R41 live interrupt fired during the turn.
    budget_hit: bool,
    /// The stage's stop poller (R38), kept until the stage ends: a rate-limit backoff
    /// waits on it (EC-17).
    stop: StopWatch,
}

/// What an Implementer turn leads to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ImplementOutcome {
    /// The live budget interrupt fired (R41): the attempt is interrupted, the task pauses.
    BudgetHit,
    /// A stop request cancelled the turn (R38).
    Stopped,
    /// Rate limited or overloaded: the same implementer again after `wait` (R49).
    Retry { retry: u32, wait: Duration },
    /// Out of quota, or still rate limited after the retries: the next implementer (R49).
    Switch { reason: String },
    /// The HEAD guard found a Git state a soft reset cannot repair (§8; R12, R25):
    /// escalate, nothing reset (EC-18).
    GitViolation { reason: String },
    /// Untracked/ignored protected files or Git metadata changed (R35): escalate.
    ProtectedChanged { reason: String },
    /// Protected or denied paths changed (§8): restore them, back to the implementer.
    PathPolicy { paths: Vec<String> },
    /// The turn failed and changed nothing: escalate.
    Failed { reason: String },
    /// The turn failed but left changes that pass the path policy (EC-15): they become a
    /// recovery checkpoint (R31), then the stage runs again from it (`retry`) or the task
    /// escalates.
    FailedWithChanges { reason: String, retry: bool },
    /// On to the checkpoint.
    Done,
}

impl ImplementOutcome {
    /// The turn was cut short: by the live budget interrupt (which also cancels the turn),
    /// else by a stop request.
    fn interrupted(r: &StageOutcome, budget_hit: bool) -> Option<Self> {
        if budget_hit {
            Some(Self::BudgetHit)
        } else if r.cancelled {
            Some(Self::Stopped)
        } else {
            None
        }
    }

    /// Quota and rate limits (R49): out of quota → the next implementer; rate limited or
    /// overloaded → retry after `base × n` seconds, and after the retries → the next
    /// implementer. `retries` is the number of retries so far.
    fn quota(r: &StageOutcome, retries: u32, retry_base_secs: u64) -> Option<Self> {
        match r.error_class {
            Some(ErrorClass::QuotaExceeded) => Some(Self::Switch {
                reason: format!(
                    "quota exhausted: {}",
                    r.error_detail.clone().unwrap_or_default()
                ),
            }),
            Some(ErrorClass::RateLimited | ErrorClass::Overloaded)
                if retries < RATE_LIMIT_RETRIES =>
            {
                let retry = retries + 1;
                Some(Self::Retry {
                    retry,
                    wait: Duration::from_secs(retry_base_secs * u64::from(retry)),
                })
            }
            Some(c @ (ErrorClass::RateLimited | ErrorClass::Overloaded)) => Some(Self::Switch {
                reason: format!("{c:?} after {RATE_LIMIT_RETRIES} retries"),
            }),
            _ => None,
        }
    }

    /// After the HEAD guard and the protected-state check: changed protected or denied
    /// paths → the path policy; a failed turn that changed nothing → escalate; a failed turn
    /// that left changes → a recovery checkpoint of them (EC-15), then the stage runs again
    /// from it — once (`failed_before`: it already did), and never for a class §11 does not
    /// retry; else on to the checkpoint.
    fn after_checks(
        r: &StageOutcome,
        changed: &[(String, String)],
        protect: &Protected,
        failed_before: bool,
    ) -> Self {
        let paths: Vec<String> = changed
            .iter()
            .map(|c| c.1.clone())
            .filter(|p| protect.violates(p))
            .collect();
        if !paths.is_empty() {
            return Self::PathPolicy { paths };
        }
        if r.success {
            return Self::Done;
        }
        let reason = format!(
            "implementer failed: {:?} {}",
            r.error_class,
            r.error_detail.clone().unwrap_or_default()
        );
        if changed.is_empty() {
            Self::Failed { reason }
        } else {
            Self::FailedWithChanges {
                reason,
                retry: !failed_before && retried_after_failure(r.error_class),
            }
        }
    }

    /// Consecutive rate-limit retries after this outcome: a retry counts, a turn cut short
    /// leaves the count, anything else resets it.
    fn retries_after(&self, retries: u32) -> u32 {
        match self {
            Self::Retry { retry, .. } => *retry,
            Self::BudgetHit | Self::Stopped => retries,
            _ => 0,
        }
    }
}

/// The implementer at `index` of the task's chain (R49). `--no-fallback` forbids a new
/// switch, not the current implementer: it leaves nothing after `t.implementer_index`, which
/// may already be a fallback after a sticky switch (EC-16).
fn implementer_at(t: &TaskRow, index: usize, no_fallback: bool) -> Option<String> {
    let limit = if no_fallback {
        t.implementer_index + 1
    } else {
        t.implementers.len()
    };
    t.implementers.get(index).filter(|_| index < limit).cloned()
}

/// Why the task pauses when no implementer is left (R49).
fn implementer_quota_reason(detail: &str) -> String {
    format!("implementer_quota: no implementer left ({detail}); the worktree keeps any partial changes — `toryo resume` when the quota is back")
}

/// The Implementer's prompt: the plan, the feedback of a fix cycle, the hand-over note.
fn implementer_prompt(fix_cycle: u32, feedback: Option<&str>, handoff: Option<&str>) -> String {
    let mut prompt = "Implement the approved plan (PLAN.md) for the task (TASK.md) in this worktree, following your role and the project rules in the context.\n".to_string();
    if fix_cycle > 0 {
        if let Some(f) = feedback {
            prompt.push_str(&format!("\n## Fix cycle {fix_cycle}\nThe previous checkpoint was not accepted. Address the following, then report the issue ids you fixed in `resolved`:\n\n{f}\n"));
        }
    }
    if let Some(h) = handoff {
        prompt.push_str(h);
    }
    prompt
}

/// R48 recovery policy: Claude resumes its interrupted session; Codex starts fresh (the
/// hand-over note written by recovery names the partial commit).
fn implementer_resume(
    prev: Option<&AttemptRow>,
    kind: ProfileKind,
    profile: &str,
) -> Option<String> {
    match (prev, kind) {
        (Some(p), ProfileKind::Claude)
            if p.status == AttemptStatus::Interrupted && p.profile.as_deref() == Some(profile) =>
        {
            p.external_session_id.clone()
        }
        _ => None,
    }
}

/// R49: the implementer this attempt replaces — on the first attempt of a fallback profile.
fn fallback_from(t: &TaskRow, prev: Option<&AttemptRow>, profile: &str) -> Option<String> {
    if t.implementer_index > 0 && prev.is_none_or(|p| p.profile.as_deref() != Some(profile)) {
        t.implementers.get(t.implementer_index - 1).cloned()
    } else {
        None
    }
}

/// R49: the hand-over note for the next implementer, with the uncommitted partial changes
/// (`<status> <path>`).
fn handover_note(profile: &str, reason: &str, changed: &[String]) -> String {
    if changed.is_empty() {
        format!("\n## Hand-over\nThe previous implementer (`{profile}`) stopped before producing any change ({reason}). Start the task from the plan.\n")
    } else {
        format!(
            "\n## Hand-over\nThe previous implementer (`{profile}`) stopped before finishing ({reason}). The worktree contains its UNCOMMITTED partial changes:\n{}\nInspect them, keep what is correct, fix or complete the rest, and finish the task.\n",
            changed.iter().map(|x| format!("- `{x}`")).collect::<Vec<_>>().join("\n")
        )
    }
}

/// EC-15, §11: a failed Implementer turn that left changes runs its stage again from them
/// — except for a permission denial ("no blind retry"), an auth failure, a bad request or a
/// policy refusal ("no retry").
fn retried_after_failure(class: Option<ErrorClass>) -> bool {
    !matches!(
        class,
        Some(
            ErrorClass::PermissionDenied
                | ErrorClass::Auth
                | ErrorClass::BadRequest
                | ErrorClass::PolicyRefusal
        )
    )
}

/// EC-15, §11: has an attempt of `stage` already failed and left changes (kept in a
/// recovery checkpoint)? Then the stage already ran again once.
fn failed_with_changes_before(attempts: &[AttemptRow], stage: &str) -> bool {
    attempts.iter().any(|a| {
        a.stage == stage && a.status == AttemptStatus::Failed && a.partial_commit.is_some()
    })
}

/// EC-15: the hand-over note after a failed turn whose changes became the recovery
/// checkpoint `partial` (the §10 recovery note, for a turn that failed): the class and the
/// first line of the detail.
fn failed_turn_note(a: &AttemptRow, r: &StageOutcome, partial: &str) -> String {
    let detail: String = r
        .error_detail
        .as_deref()
        .and_then(|d| d.lines().next())
        .unwrap_or("")
        .chars()
        .take(300)
        .collect();
    let why = match (r.error_class, detail.trim()) {
        (Some(c), "") => format!("{c:?}"),
        (Some(c), d) => format!("{c:?}: {d}"),
        (None, "") => "no error class".to_string(),
        (None, d) => d.to_string(),
    };
    format!(
        "\n## Recovery\nThe previous attempt ({} by `{}`) failed before it finished ({why}). Its uncommitted work was saved as the recovery checkpoint {} (trailer `Toryo-Partial: true`) — the worktree starts from it. Check that work, keep what is correct, fix or complete the rest, and finish the task.\n",
        a.id,
        a.profile.as_deref().unwrap_or("?"),
        short(partial)
    )
}

/// The rate-limit backoff (R49): sleeps `wait` unless `stop` — the stage's stop flag, set by
/// its watcher within a second of `toryo stop` (R38) — is set first. Returns whether it was
/// (EC-17).
fn backoff(stop: &AtomicBool, wait: Duration) -> bool {
    // a wait beyond what `Instant` can represent lasts until the stop
    let end = std::time::Instant::now().checked_add(wait);
    loop {
        if stop.load(Ordering::SeqCst) {
            return true;
        }
        let left = end.map_or(Duration::MAX, |e| {
            e.saturating_duration_since(std::time::Instant::now())
        });
        if left.is_zero() {
            return false;
        }
        std::thread::sleep(left.min(Duration::from_millis(50)));
    }
}

/// R25: an unfinished Git operation in the task worktree — a marker of
/// `Worktree::unfinished_operation`, the `sequencer` of a multi-commit cherry-pick or revert,
/// or unmerged paths (a conflicting `git stash pop` leaves no marker).
fn unfinished_git_operation(wt: &Worktree) -> Result<Option<String>> {
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
struct GitAfterTurn {
    /// An unfinished merge, rebase, cherry-pick, revert or bisect, or unmerged paths.
    unfinished: Option<String>,
    /// What HEAD points at (`refs/heads/…`); `None` when it is detached.
    head_ref: Option<String>,
    /// The commit HEAD resolves to; `None` when it does not resolve.
    head: Option<String>,
    /// The task's base commit is an ancestor of HEAD (only read when HEAD moved).
    descends_from_base: bool,
}

/// What the HEAD guard does about it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HeadGuard {
    /// HEAD is where the turn began, on the task branch.
    Unchanged,
    /// The agent committed on the task branch: soft reset to where the turn began.
    Reset { head: String },
    /// Not repairable by a soft reset: the stage fails, the task escalates (`reason`).
    Violation { kind: &'static str, reason: String },
}

impl HeadGuard {
    /// §8, in this order: an unfinished operation (R25); HEAD on another branch or detached
    /// (R12: a reset would move that branch, not the task's); a HEAD that does not resolve;
    /// history rewritten below the base (R25); then a moved HEAD is reset.
    fn of(g: &GitAfterTurn, branch: &str, base: &str, head_before: &str) -> Self {
        let violation = |kind, what: String, fix: &str| {
            Self::Violation {
            kind,
            reason: format!(
                "policy_violation: after the Implementer turn {what}; nothing was reset or committed{fix}"
            ),
        }
        };
        let checkout = format!(" — check out {branch} again first");
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
                    &checkout,
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

/// The first rate-limit backoff in seconds: `TORYO_RETRY_BASE_SECS`, default 30.
fn retry_base_secs() -> u64 {
    std::env::var("TORYO_RETRY_BASE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30)
}

/// A review that passed the checks (R28).
struct ValidReview {
    review: Value,
    /// The turn that returned it.
    last: StageOutcome,
    /// Correction turns it took (R22).
    corrections: u32,
}

/// How the turns of a review attempt ended.
enum ReviewTurns {
    Valid(ValidReview),
    /// Still invalid after the correction turns (R22); `last` is the final turn.
    Rejected {
        last: StageOutcome,
    },
    /// The attempt has already ended (budget, stop, quota or protected state).
    Ended(Flow),
}

/// The decision of a valid review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewDecision {
    Approved,
    ChangesRequested,
    Blocked,
}

impl ReviewDecision {
    /// The review's transition for the checkpoint `head`.
    fn event(self, head: &str) -> Ev {
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
struct ReviewVerdict {
    decision: ReviewDecision,
    /// `approved` with an unmet criterion or a blocking issue is inconsistent and becomes
    /// `changes_requested`; then these are the unmet criteria (possibly none).
    downgraded: Option<Vec<String>>,
}

impl ReviewVerdict {
    fn of(review: &Value) -> Self {
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
fn review_resume(prev: Option<&AttemptRow>, head: &str, reviewer: &str) -> Option<String> {
    prev.filter(|p| {
        p.status == AttemptStatus::Interrupted
            && p.subject_commit.as_deref() == Some(head)
            && p.profile.as_deref() == Some(reviewer)
    })
    .and_then(|p| p.external_session_id.clone())
}

/// The Reviewer's prompt; a resumed session is told that its previous turn was interrupted.
fn review_prompt(head: &str, acceptance_ids: &[String], resumed: bool) -> String {
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
fn correction_prompt(violations: &[String]) -> String {
    format!("Your previous final response was rejected and was not used. Validation error: {}. Return only a corrected JSON object matching the schema.", violations.join("; "))
}

/// One verify command's entry in VERIFY_RESULT.json.
fn command_result(c: &CommandSpec, res: &CommandResult) -> Value {
    json!({"name": c.name, "argv": std::iter::once(c.program.clone()).chain(c.args.clone()).collect::<Vec<_>>(), "exit": res.exit, "ms": res.duration_ms, "output_tail": res.tail(40)})
}

/// The feedback of a failed verify: the last command run (the failing one) and its output.
fn verify_feedback(results: &[Value]) -> String {
    let f = results.last().cloned().unwrap_or(Value::Null);
    format!(
        "- VERIFY FAILED: `{}` exited {:?}. Last output:\n```\n{}\n```",
        f["name"].as_str().unwrap_or(""),
        f["exit"],
        f["output_tail"].as_str().unwrap_or("")
    )
}

/// The error class of a prefixed reason: `artifact_tampered: …` → `artifact_tampered`.
fn reason_class(reason: &str, default: &str) -> String {
    reason.split(':').next().unwrap_or(default).to_string()
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

/// What the human should see at the final gate (R53, R49): the approved review's
/// non-blocking issues, and a note when the implementer and the reviewer share a vendor.
pub fn final_gate_notes(store: &Store, t: &TaskRow) -> Result<Vec<String>> {
    let mut out = Vec::new();
    if let Some((_, b)) = super::artifacts::read_head(store, t, "review")? {
        let v: Value = serde_json::from_slice(&b)?;
        let nb = non_blocking(&v);
        if !nb.is_empty() {
            out.push(format!(
                "non-blocking issues (follow-ups, also in DECISIONS.md):\n{}",
                nb.join("\n")
            ));
        }
    }
    let cfg = frozen_config(t)?;
    let i = t.id.clone();
    let atts = store.call(move |tx| store::attempts(tx, &i))?;
    let mut imps: Vec<String> = atts
        .iter()
        .filter(|a| {
            a.status == AttemptStatus::Succeeded
                && a.kind == "agent"
                && (a.stage == "implement" || a.stage.starts_with("fix-"))
        })
        .filter_map(|a| a.profile.clone())
        .collect();
    imps.dedup();
    let kind = |p: &str| cfg.profile(p).map(|x| x.kind).ok();
    let same: Vec<&String> = imps
        .iter()
        .filter(|p| kind(p).is_some() && kind(p) == kind(&t.reviewer))
        .collect();
    if !same.is_empty() {
        out.push(format!(
            "same vendor: {} implemented and {} reviewed ({:?} both) — the review is less independent (R49); read the diff yourself",
            same.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
            t.reviewer,
            kind(&t.reviewer).map(|k| format!("{k:?}")).unwrap_or_default()
        ));
    }
    Ok(out)
}

fn review_feedback(review: &Value) -> String {
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

/// Extra instructions when Claude is the Implementer (R49): file tools only, no shell, no
/// run store — the context inlines the task pack.
fn claude_implementer_prompt(base: &str) -> String {
    format!(
        "{base}\n## Session notes (Claude implementer)\n\
         - Your working directory is the task worktree. You can Read, Grep, Glob, Edit and Write files there only.\n\
         - You have NO shell in this session: do not try to run commands. The orchestrator runs build/test after you \
         and sends failures back. Put what you checked by reading code in `validation_performed`.\n\
         - TASK.md and PLAN.md are inlined in your system prompt (the run store is not accessible to you).\n"
    )
}

// ── gate decisions (used by `toryo approve` / `reject` and `--yes`) ──────────────────

/// Approve the open gate after re-checking its subject (R32): the plan on disk must still
/// hash to the approved digest; the final gate's checkpoint must still be verified.
pub fn approve_now(store: &Store, t: &TaskRow, kind: GateKind, expect: Option<&str>) -> Result<()> {
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
            let (_, bytes) =
                super::artifacts::read_head(store, t, "plan")?.context("no registered plan")?;
            if sha256_hex(&bytes) != gate.subject {
                bail!("subject_changed: plan.json no longer matches the plan under approval (R32)");
            }
            Ev::PlanApproved {
                occurrence: gate.occurrence,
                subject: gate.subject.clone(),
            }
        }
        GateKind::Final => Ev::FinalApproved {
            occurrence: gate.occurrence,
            subject: gate.subject.clone(),
        },
    };
    let i = t.id.clone();
    let m = store.call(move |tx| store::transition(tx, &i, e))?;
    eprintln!(
        "   [state] {:?} --{}_approved--> {:?}",
        t.state,
        gate_name(kind),
        m.state()
    );
    let digest = gate_digest(store, &t.id, kind, gate.occurrence).unwrap_or_default();
    super::decisions::append(
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
pub fn reject_now(store: &Store, t: &TaskRow, kind: GateKind, reason: &str) -> Result<()> {
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
        store::transition(tx, &i, e)
    })?;
    eprintln!("   [state] {:?} --rejected--> {:?}", t.state, m.state());
    super::decisions::append(
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

/// The gate's name in approval rows, `[state]` lines and DECISIONS.md.
fn gate_name(kind: GateKind) -> &'static str {
    match kind {
        GateKind::Plan => "plan",
        GateKind::Final => "final",
    }
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
    let Some((_, b)) = super::artifacts::read_head(store, t, "verify-result")? else {
        return Ok(false);
    };
    let v: Value = serde_json::from_slice(&b)?;
    Ok(v["pass"].as_bool() == Some(true)
        && v["invalidated"].as_bool() != Some(true)
        && v["head"].as_str().is_some()
        && v["head"].as_str() == t.machine.checkpoint())
}

/// What the human does next, for `status` and the end of `run`.
pub fn next_action(t: &TaskRow) -> String {
    let id = &t.id;
    match t.state {
        St::WaitingPlanApproval => format!("review {}, then `toryo approve {id} --gate plan --continue` or `toryo reject {id} --gate plan --reason \"…\"`", t.artifacts().join("PLAN.md").display()),
        St::WaitingFinalApproval => format!("review the branch {} (REVIEW.md in the run store), then `toryo approve {id} --gate final` or `toryo reject {id} --gate final --reason \"…\"`", t.branch),
        St::Escalated => {
            let why = t.machine.reason().unwrap_or("");
            if why.starts_with(protect::REASON) {
                format!("{why}\n  → restore the file(s) and `toryo resume {id}`, or `toryo resume {id} --accept-protected-change <path>` (recorded in DECISIONS.md)")
            } else if why.starts_with("recovery_policy_violation") {
                format!("{why}\n  → `toryo resume {id} --discard-partial` (after a snapshot check) or fix the worktree by hand and `toryo resume {id} --keep-partial`")
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

/// SUMMARY.md in the run store: state, reason, tokens, next action.
pub fn write_summary(store: &Store, t: &TaskRow) -> Result<PathBuf> {
    let i = t.id.clone();
    let atts = store.call(move |tx| store::attempts(tx, &i))?;
    let mut s = format!(
        "# {} — {:?}\n\n{}\n\n- branch: `{}` (base `{}`) in `{}`\n- fix cycles: {} / {}\n- tokens: {} spent{}{}\n- next: {}\n\n## Stage attempts\n\n| attempt | status | profile | error | checkpoint |\n|---|---|---|---|---|\n",
        t.id,
        t.state,
        t.title,
        t.branch,
        &t.base_commit[..12.min(t.base_commit.len())],
        t.repo.display(),
        t.fix_cycle,
        t.machine.max_fix_cycles(),
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

#[cfg(test)]
#[path = "runner_implement_tests.rs"]
mod implement_tests;
#[cfg(test)]
mod tests {
    //! The stage decisions without the engine: no store, no git, no agents. The tests that
    //! pin a reviewed defect (EC-xx) say so; fixing the defect means inverting them.

    use super::*;
    use crate::engine::state::Machine;
    use ErrorClass as C;
    use ImplementOutcome as O;

    const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

    fn outcome(success: bool, class: Option<ErrorClass>, detail: Option<&str>) -> StageOutcome {
        StageOutcome {
            success,
            error_class: class,
            error_detail: detail.map(str::to_string),
            ..Default::default()
        }
    }

    fn attempt(
        status: AttemptStatus,
        profile: &str,
        session: Option<&str>,
        subject: Option<&str>,
    ) -> AttemptRow {
        AttemptRow {
            id: "T-1/review/1".into(),
            task_id: "T-1".into(),
            stage: "review".into(),
            attempt: 1,
            kind: "agent".into(),
            fallback_from: None,
            profile: Some(profile.into()),
            external_session_id: session.map(str::to_string),
            pid: None,
            pid_created_at: None,
            status,
            error_class: None,
            error_detail: None,
            git_head_before: None,
            git_head_after: None,
            subject_commit: subject.map(str::to_string),
            partial_commit: None,
            checkpoint_kind: None,
            partial_snapshot_id: None,
            correction_turns: 0,
            started_at: String::new(),
            ended_at: None,
            context_sha256: None,
        }
    }

    /// A task whose machine went through `events` (implementers: codex-openai → claude-max).
    fn task_in(events: &[Ev]) -> TaskRow {
        let mut machine = Machine::new(3);
        for e in events {
            machine.apply(e.clone()).expect("a valid transition");
        }
        TaskRow {
            id: "T-1".into(),
            title: "t".into(),
            state: machine.state(),
            task_sha256: String::new(),
            acceptance_ids: vec!["AC-1".into(), "AC-2".into()],
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
            implementers: vec!["codex-openai".into(), "claude-max".into()],
            planner: "claude-max".into(),
            reviewer: "claude-max".into(),
            feedback: None,
            handoff: None,
            budget_tokens: None,
        }
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // ── plan ────────────────────────────────────────────────────────────────────────

    #[test]
    fn writing_stage_names_the_fix_cycle() {
        assert_eq!(writing_stage(0), "implement");
        assert_eq!(writing_stage(1), "fix-1");
        assert_eq!(writing_stage(12), "fix-12");
    }

    #[test]
    fn the_plan_prompt_prefers_feedback_then_notes_an_interrupted_turn() {
        let task = PathBuf::from("runstore").join("TASK.md");
        let shown = task.display().to_string();
        let again = format!("Your previous turn was interrupted before it finished. Create the plan for the task in {shown} again and return it.");
        let first = format!("Create the plan for the task in {shown}.");
        let cases = [
            (Some("Reason: x"), None, "Reason: x".to_string()),
            (
                Some("Reason: x"),
                Some(AttemptStatus::Interrupted),
                "Reason: x".to_string(),
            ),
            (None, Some(AttemptStatus::Interrupted), again),
            (None, None, first.clone()),
            (None, Some(AttemptStatus::Succeeded), first.clone()),
            (None, Some(AttemptStatus::Failed), first.clone()),
            (None, Some(AttemptStatus::Running), first),
        ];
        for (feedback, prev, want) in cases {
            assert_eq!(
                plan_prompt(feedback, prev, &task),
                want,
                "{feedback:?} {prev:?}"
            );
        }
    }

    #[test]
    fn the_planner_resumes_the_previous_session_of_any_profile() {
        assert_eq!(plan_resume(None), None);
        let none = attempt(AttemptStatus::Succeeded, "claude-max", None, None);
        assert_eq!(plan_resume(Some(&none)), None);
        let claude = attempt(AttemptStatus::Succeeded, "claude-max", Some("s-1"), None);
        assert_eq!(plan_resume(Some(&claude)), Some("s-1".into()));
        // EC-30 (not fixed): the profile is not compared, so a changed planner profile gets
        // the previous profile's session
        let codex = attempt(
            AttemptStatus::Interrupted,
            "codex-openai",
            Some("t-1"),
            None,
        );
        assert_eq!(plan_resume(Some(&codex)), Some("t-1".into()));
    }

    #[test]
    fn plan_files_are_the_string_entries_of_files_likely_involved() {
        let cases = [
            (
                json!({"files_likely_involved": ["a.ps1", 3, null, "b/c.ps1"]}),
                strings(&["a.ps1", "b/c.ps1"]),
            ),
            (json!({"files_likely_involved": []}), vec![]),
            (json!({"files_likely_involved": "a.ps1"}), vec![]),
            (json!({}), vec![]),
            (Value::Null, vec![]),
        ];
        for (plan, want) in cases {
            assert_eq!(plan_files(&plan), want, "{plan}");
        }
    }

    // ── implement: prepare ──────────────────────────────────────────────────────────

    #[test]
    fn implementer_at_follows_the_chain_and_no_fallback_keeps_the_current() {
        let mut t = task_in(&[]);
        // (current implementer_index, asked index, --no-fallback, implementer)
        let cases = [
            (0, 0, false, Some("codex-openai")),
            (0, 1, false, Some("claude-max")),
            (0, 2, false, None),
            (0, 0, true, Some("codex-openai")),
            // --no-fallback forbids the switch to the next implementer …
            (0, 1, true, None),
            (0, 2, true, None),
            // … but not the current one after a sticky switch (EC-16, was None)
            (1, 1, true, Some("claude-max")),
            (1, 2, true, None),
            (1, 1, false, Some("claude-max")),
        ];
        for (current, index, no_fallback, want) in cases {
            t.implementer_index = current;
            assert_eq!(
                implementer_at(&t, index, no_fallback).as_deref(),
                want,
                "current {current}, index {index}, no_fallback {no_fallback}"
            );
        }
        let mut one = task_in(&[]);
        one.implementers = strings(&["codex-openai"]);
        assert_eq!(implementer_at(&one, 1, false), None);
    }

    #[test]
    fn the_quota_pause_reason_names_what_is_left() {
        assert_eq!(
            implementer_quota_reason("codex-openai → claude-max, --no-fallback"),
            "implementer_quota: no implementer left (codex-openai → claude-max, --no-fallback); the worktree keeps any partial changes — `toryo resume` when the quota is back"
        );
    }

    #[test]
    fn the_implementer_prompt_adds_fix_cycle_feedback_and_the_hand_over() {
        let base = "Implement the approved plan (PLAN.md) for the task (TASK.md) in this worktree, following your role and the project rules in the context.\n";
        let fb = "- AC-2 NOT MET: no zero test\n";
        let note = "\n## Hand-over\nnote\n";
        let fix = format!("\n## Fix cycle 2\nThe previous checkpoint was not accepted. Address the following, then report the issue ids you fixed in `resolved`:\n\n{fb}\n");
        let cases = [
            (0, None, None, base.to_string()),
            // feedback counts only in a fix cycle (a re-plan's feedback is the planner's)
            (0, Some(fb), None, base.to_string()),
            (2, None, None, base.to_string()),
            (2, Some(fb), None, format!("{base}{fix}")),
            (0, None, Some(note), format!("{base}{note}")),
            (2, Some(fb), Some(note), format!("{base}{fix}{note}")),
        ];
        for (cycle, feedback, handoff, want) in cases {
            assert_eq!(
                implementer_prompt(cycle, feedback, handoff),
                want,
                "cycle {cycle}, {feedback:?}, {handoff:?}"
            );
        }
    }

    #[test]
    fn the_claude_implementer_prompt_adds_the_session_notes() {
        let p = claude_implementer_prompt("BASE\n");
        assert!(
            p.starts_with("BASE\n\n## Session notes (Claude implementer)\n"),
            "{p}"
        );
        assert!(p.contains("You have NO shell in this session"), "{p}");
        assert!(
            p.ends_with("(the run store is not accessible to you).\n"),
            "{p}"
        );
    }

    #[test]
    fn only_an_interrupted_claude_implementer_resumes_its_session() {
        use AttemptStatus::{Failed, Interrupted, Succeeded};
        use ProfileKind::{Claude, Codex};
        let cases = [
            (None, Claude, "claude-max", None),
            (
                Some(attempt(Interrupted, "claude-max", Some("s-1"), None)),
                Claude,
                "claude-max",
                Some("s-1"),
            ),
            // R48: Codex starts fresh, the recovery hand-over names the partial commit
            (
                Some(attempt(Interrupted, "codex-openai", Some("t-1"), None)),
                Codex,
                "codex-openai",
                None,
            ),
            (
                Some(attempt(Failed, "claude-max", Some("s-1"), None)),
                Claude,
                "claude-max",
                None,
            ),
            (
                Some(attempt(Succeeded, "claude-max", Some("s-1"), None)),
                Claude,
                "claude-max",
                None,
            ),
            // another profile's session is never resumed
            (
                Some(attempt(Interrupted, "claude-other", Some("s-1"), None)),
                Claude,
                "claude-max",
                None,
            ),
            (
                Some(attempt(Interrupted, "claude-max", None, None)),
                Claude,
                "claude-max",
                None,
            ),
        ];
        for (prev, kind, profile, want) in cases {
            assert_eq!(
                implementer_resume(prev.as_ref(), kind, profile).as_deref(),
                want,
                "{kind:?} {profile} after {:?}",
                prev.as_ref().map(|p| (p.status, p.profile.clone()))
            );
        }
    }

    #[test]
    fn fallback_from_names_the_replaced_implementer_on_its_first_attempt() {
        let mut t = task_in(&[]);
        assert_eq!(fallback_from(&t, None, "codex-openai"), None);
        t.implementer_index = 1;
        let codex = attempt(AttemptStatus::Failed, "codex-openai", None, None);
        let claude = attempt(AttemptStatus::Interrupted, "claude-max", None, None);
        assert_eq!(
            fallback_from(&t, None, "claude-max").as_deref(),
            Some("codex-openai")
        );
        assert_eq!(
            fallback_from(&t, Some(&codex), "claude-max").as_deref(),
            Some("codex-openai")
        );
        // a later attempt of the same implementer is no longer a fallback
        assert_eq!(fallback_from(&t, Some(&claude), "claude-max"), None);
    }

    #[test]
    fn the_hand_over_note_lists_the_partial_changes() {
        assert_eq!(
            handover_note("codex-openai", "quota exhausted: x", &[]),
            "\n## Hand-over\nThe previous implementer (`codex-openai`) stopped before producing any change (quota exhausted: x). Start the task from the plan.\n"
        );
        assert_eq!(
            handover_note(
                "codex-openai",
                "RateLimited after 2 retries",
                &strings(&["?? src/a.ps1", " M src/b.ps1"])
            ),
            "\n## Hand-over\nThe previous implementer (`codex-openai`) stopped before finishing (RateLimited after 2 retries). The worktree contains its UNCOMMITTED partial changes:\n- `?? src/a.ps1`\n- ` M src/b.ps1`\nInspect them, keep what is correct, fix or complete the rest, and finish the task.\n"
        );
    }

    // ── implement: interpret ────────────────────────────────────────────────────────

    #[test]
    fn a_turn_is_cut_short_by_the_budget_first_then_by_a_stop() {
        let cancelled = StageOutcome {
            cancelled: true,
            ..Default::default()
        };
        let timed_out = StageOutcome {
            timed_out: true,
            error_class: Some(C::Timeout),
            ..Default::default()
        };
        let done = outcome(true, None, None);
        let cases = [
            (&done, true, Some(O::BudgetHit)),
            // the live interrupt cancels the turn too: it is the budget, not a stop
            (&cancelled, true, Some(O::BudgetHit)),
            (&cancelled, false, Some(O::Stopped)),
            (&done, false, None),
            (&timed_out, false, None),
        ];
        for (r, budget_hit, want) in cases {
            assert_eq!(ImplementOutcome::interrupted(r, budget_hit), want, "{r:?}");
        }
    }

    #[test]
    fn quota_switches_and_rate_limits_retry_twice_then_switch() {
        let secs = Duration::from_secs;
        let cases = [
            (None, None, 0, None),
            (Some(C::Unknown), Some("x"), 0, None),
            (Some(C::Timeout), None, 1, None),
            (Some(C::PermissionDenied), None, 0, None),
            (Some(C::ContextWindowExceeded), None, 0, None),
            (Some(C::Auth), None, 2, None),
            (
                Some(C::QuotaExceeded),
                Some("weekly limit"),
                0,
                Some(O::Switch {
                    reason: "quota exhausted: weekly limit".into(),
                }),
            ),
            // a quota switch does not wait for the rate-limit retries
            (
                Some(C::QuotaExceeded),
                None,
                1,
                Some(O::Switch {
                    reason: "quota exhausted: ".into(),
                }),
            ),
            (
                Some(C::RateLimited),
                Some("429"),
                0,
                Some(O::Retry {
                    retry: 1,
                    wait: secs(30),
                }),
            ),
            (
                Some(C::Overloaded),
                None,
                1,
                Some(O::Retry {
                    retry: 2,
                    wait: secs(60),
                }),
            ),
            (
                Some(C::RateLimited),
                None,
                2,
                Some(O::Switch {
                    reason: "RateLimited after 2 retries".into(),
                }),
            ),
            (
                Some(C::Overloaded),
                None,
                7,
                Some(O::Switch {
                    reason: "Overloaded after 2 retries".into(),
                }),
            ),
        ];
        for (class, detail, retries, want) in cases {
            let r = outcome(false, class, detail);
            assert_eq!(
                ImplementOutcome::quota(&r, retries, 30),
                want,
                "{class:?} after {retries} retries"
            );
        }
        // TORYO_RETRY_BASE_SECS=0 (the tests): no wait
        let r = outcome(false, Some(C::RateLimited), None);
        assert_eq!(
            ImplementOutcome::quota(&r, 1, 0),
            Some(O::Retry {
                retry: 2,
                wait: Duration::ZERO
            })
        );
    }

    #[test]
    fn after_the_checks_the_path_policy_comes_before_a_failed_turn() {
        let protect = Protected::new(strings(&["scripts/test.ps1"]), &[])
            .with_policy(&[], &strings(&["**/ZZZ/**"]));
        let ok = outcome(true, None, None);
        let failed = outcome(false, Some(C::Unknown), Some("exit 1"));
        let bare = outcome(false, None, None);
        let changed = |paths: &[&str]| -> Vec<(String, String)> {
            paths.iter().map(|p| ("??".into(), p.to_string())).collect()
        };
        let cases = [
            (&ok, changed(&[]), O::Done),
            (&ok, changed(&["src/a.ps1"]), O::Done),
            (
                &failed,
                changed(&[]),
                O::Failed {
                    reason: "implementer failed: Some(Unknown) exit 1".into(),
                },
            ),
            (
                &bare,
                changed(&[]),
                O::Failed {
                    reason: "implementer failed: None ".into(),
                },
            ),
            // EC-15: a failed turn that left changes never goes on to the checkpoint (was
            // `Done`): its work becomes a recovery checkpoint and the stage runs again
            (
                &failed,
                changed(&["src/a.ps1"]),
                O::FailedWithChanges {
                    reason: "implementer failed: Some(Unknown) exit 1".into(),
                    retry: true,
                },
            ),
            (
                &ok,
                changed(&[
                    "src/a.ps1",
                    "AGENTS.md",
                    "x/zzz/secret.txt",
                    "scripts/test.ps1",
                ]),
                O::PathPolicy {
                    paths: strings(&["AGENTS.md", "x/zzz/secret.txt", "scripts/test.ps1"]),
                },
            ),
            (
                &failed,
                changed(&[".claude/settings.json"]),
                O::PathPolicy {
                    paths: strings(&[".claude/settings.json"]),
                },
            ),
        ];
        for (r, changed, want) in cases {
            assert_eq!(
                ImplementOutcome::after_checks(r, &changed, &protect, false),
                want,
                "{changed:?}"
            );
        }
    }

    #[test]
    fn rate_limit_retries_count_up_and_start_over_after_anything_else() {
        let cases = [
            (
                O::Retry {
                    retry: 2,
                    wait: Duration::ZERO,
                },
                1,
                2,
            ),
            (O::BudgetHit, 1, 1),
            (O::Stopped, 2, 2),
            (O::Switch { reason: "q".into() }, 2, 0),
            (O::ProtectedChanged { reason: "p".into() }, 1, 0),
            (O::PathPolicy { paths: vec![] }, 1, 0),
            (O::Failed { reason: "f".into() }, 1, 0),
            (O::Done, 1, 0),
        ];
        for (o, before, after) in cases {
            assert_eq!(o.retries_after(before), after, "{o:?}");
        }
    }

    // ── review ──────────────────────────────────────────────────────────────────────

    #[test]
    fn an_interrupted_review_of_the_same_checkpoint_resumes_its_session() {
        use AttemptStatus::{Failed, Interrupted};
        let cases = [
            (None, None),
            (
                Some(attempt(Interrupted, "claude-max", Some("s-1"), Some(HEAD))),
                Some("s-1"),
            ),
            // another checkpoint, reviewer or status: a fresh session
            (
                Some(attempt(
                    Interrupted,
                    "claude-max",
                    Some("s-1"),
                    Some("f00d"),
                )),
                None,
            ),
            (
                Some(attempt(Interrupted, "claude-max", Some("s-1"), None)),
                None,
            ),
            (
                Some(attempt(
                    Interrupted,
                    "codex-openai",
                    Some("s-1"),
                    Some(HEAD),
                )),
                None,
            ),
            (
                Some(attempt(Failed, "claude-max", Some("s-1"), Some(HEAD))),
                None,
            ),
            (
                Some(attempt(Interrupted, "claude-max", None, Some(HEAD))),
                None,
            ),
        ];
        for (prev, want) in cases {
            assert_eq!(
                review_resume(prev.as_ref(), HEAD, "claude-max").as_deref(),
                want,
                "{:?}",
                prev.as_ref()
                    .map(|p| (p.status, p.profile.clone(), p.subject_commit.clone()))
            );
        }
    }

    #[test]
    fn the_review_prompt_names_the_checkpoint_and_the_criteria() {
        let ids = strings(&["AC-1", "AC-2"]);
        let fresh = "Review the checkpoint 0123456789ab in this worktree against TASK.md. DIFF.patch is base → checkpoint; DIFF-incremental.patch is the change since your last review (empty on the first). Acceptance criteria ids: [\"AC-1\", \"AC-2\"].";
        assert_eq!(review_prompt(HEAD, &ids, false), fresh);
        assert_eq!(
            review_prompt(HEAD, &ids, true),
            format!("Your previous review turn was interrupted before it finished. {fresh} Return the complete review.")
        );
        // a short head is shown as it is
        assert!(review_prompt("abc", &[], false).contains("checkpoint abc in"));
    }

    #[test]
    fn review_violations_seen_by_the_correction_loop() {
        let ids = strings(&["AC-1", "AC-2"]);
        let ac = |id: &str, met: bool, ev: &str| json!({"id": id, "met": met, "evidence": ev});
        let cases = [
            // no structured output at all: the loop checks `Value::Null`
            (
                Value::Null,
                vec![
                    "decision must be approved|changes_requested|blocked, got ``",
                    "missing acceptance criteria: [\"AC-1\", \"AC-2\"]",
                ],
            ),
            (
                json!({"decision": "approved", "acceptance_criteria": [ac("AC-1", true, "a.ps1:3"), ac("AC-2", true, "t")], "issues": []}),
                vec![],
            ),
            // a placeholder counts only for a met criterion; trimmed, any case
            (
                json!({"decision": "approved", "acceptance_criteria": [ac("AC-1", true, " N/A "), ac("AC-2", false, "n/a")]}),
                vec!["AC-1: evidence is empty or a placeholder"],
            ),
            (
                json!({"decision": "blocked", "acceptance_criteria": [ac("AC-1", false, ""), {"id": "AC-2", "met": true}]}),
                vec![
                    "AC-1: evidence is empty or a placeholder",
                    "AC-2: evidence is empty or a placeholder",
                ],
            ),
            (
                json!({"decision": "approved", "acceptance_criteria": [ac("AC-1", true, "e"), ac("AC-1", true, "e"), ac("AC-2", true, "e")]}),
                vec!["duplicate acceptance-criteria ids"],
            ),
            (
                json!({"decision": "approved", "acceptance_criteria": [ac("AC-1", true, "e"), ac("AC-2", true, "e"), ac("AC-9", true, "e")]}),
                vec!["unknown acceptance criteria: [\"AC-9\"]"],
            ),
            (
                json!({"decision": "approved", "acceptance_criteria": [{"met": true, "evidence": ""}]}),
                vec![
                    "missing acceptance criteria: [\"AC-1\", \"AC-2\"]",
                    "?: evidence is empty or a placeholder",
                ],
            ),
            (
                json!({"decision": "changes_requested", "acceptance_criteria": [ac("AC-1", true, "e"), ac("AC-2", true, "e")], "issues": []}),
                vec!["changes_requested needs at least one unmet criterion or one issue"],
            ),
            (
                json!({"decision": "changes_requested", "acceptance_criteria": [ac("AC-1", true, "e"), ac("AC-2", false, "no test")]}),
                vec![],
            ),
        ];
        for (review, want) in cases {
            assert_eq!(review_violations(&review, &ids), want, "{review}");
        }
    }

    #[test]
    fn a_correction_prompt_quotes_the_violations() {
        let viol = review_violations(&Value::Null, &strings(&["AC-1"]));
        assert_eq!(
            correction_prompt(&viol),
            "Your previous final response was rejected and was not used. Validation error: decision must be approved|changes_requested|blocked, got ``; missing acceptance criteria: [\"AC-1\"]. Return only a corrected JSON object matching the schema."
        );
    }

    #[test]
    fn an_inconsistent_approval_becomes_changes_requested() {
        use ReviewDecision::{Approved, Blocked, ChangesRequested};
        let ac = |id: &str, met: Value| json!({"id": id, "met": met, "evidence": "e"});
        let blocking =
            json!({"id": "I-1", "severity": "high", "blocking": true, "description": "d"});
        let minor = json!({"id": "I-2", "severity": "low", "blocking": false, "description": "d"});
        let cases = [
            (
                json!({"decision": "approved", "acceptance_criteria": [ac("AC-1", json!(true))], "issues": [minor]}),
                Approved,
                None,
            ),
            (
                json!({"decision": "approved", "acceptance_criteria": [ac("AC-1", json!(true)), ac("AC-2", json!(false))]}),
                ChangesRequested,
                Some(vec!["AC-2"]),
            ),
            // `met` missing or not a boolean is unmet
            (
                json!({"decision": "approved", "acceptance_criteria": [{"id": "AC-3", "evidence": "e"}, ac("AC-4", json!("yes"))]}),
                ChangesRequested,
                Some(vec!["AC-3", "AC-4"]),
            ),
            (
                json!({"decision": "approved", "acceptance_criteria": [ac("AC-1", json!(true))], "issues": [minor, blocking]}),
                ChangesRequested,
                Some(vec![]),
            ),
            // only `blocking: true` blocks
            (
                json!({"decision": "approved", "issues": [{"id": "I-3", "blocking": "true"}]}),
                Approved,
                None,
            ),
            // an unmet criterion without an id does not count (R28 checks reject it first)
            (
                json!({"decision": "approved", "acceptance_criteria": [{"met": false}]}),
                Approved,
                None,
            ),
            (
                json!({"decision": "blocked", "acceptance_criteria": [ac("AC-1", json!(false))], "issues": [blocking]}),
                Blocked,
                None,
            ),
            (
                json!({"decision": "changes_requested", "acceptance_criteria": [ac("AC-1", json!(false))]}),
                ChangesRequested,
                None,
            ),
            (json!({"decision": "maybe"}), ChangesRequested, None),
            (json!({}), ChangesRequested, None),
        ];
        for (review, decision, downgraded) in cases {
            assert_eq!(
                ReviewVerdict::of(&review),
                ReviewVerdict {
                    decision,
                    downgraded: downgraded.map(|d| strings(&d)),
                },
                "{review}"
            );
        }
    }

    #[test]
    fn a_review_decision_is_the_transition_for_its_checkpoint() {
        let head = || HEAD.to_string();
        assert_eq!(
            ReviewDecision::Approved.event(HEAD),
            Ev::ReviewApproved { head: head() }
        );
        assert_eq!(
            ReviewDecision::ChangesRequested.event(HEAD),
            Ev::ChangesRequested { head: head() }
        );
        assert_eq!(
            ReviewDecision::Blocked.event(HEAD),
            Ev::ReviewBlocked { head: head() }
        );
    }

    #[test]
    fn review_feedback_lists_the_unmet_criteria_then_every_issue() {
        let cases = [
            (json!({}), ""),
            (json!({"acceptance_criteria": "x", "issues": null}), ""),
            (
                json!({"acceptance_criteria": [{"id": "AC-1", "met": true, "evidence": "ok"}], "issues": []}),
                "",
            ),
            (
                json!({"acceptance_criteria": [
                    {"id": "AC-1", "met": false, "evidence": "no test"},
                    {"id": "AC-2", "evidence": "met missing"},
                    {"id": "AC-3", "met": "yes", "evidence": "not a boolean"},
                    {"id": "AC-4", "met": true, "evidence": "e"},
                    {"met": false}]}),
                "- AC-1 NOT MET: no test\n- AC-2 NOT MET: met missing\n- AC-3 NOT MET: not a boolean\n-  NOT MET: \n",
            ),
            (
                json!({"issues": [
                    {"id": "I-1", "severity": "high", "blocking": true, "file": "src/a.ps1", "description": "d1"},
                    {"id": "I-2", "severity": "low", "blocking": false, "description": "d2"},
                    {}]}),
                "- I-1 [high, blocking] src/a.ps1: d1\n- I-2 [low] d2\n-  [] \n",
            ),
            (
                json!({"acceptance_criteria": [{"id": "AC-2", "met": false, "evidence": "e"}],
                       "issues": [{"id": "I-1", "severity": "medium", "description": "d"}]}),
                "- AC-2 NOT MET: e\n- I-1 [medium] d\n",
            ),
        ];
        for (review, want) in cases {
            assert_eq!(review_feedback(&review), want, "{review}");
        }
    }

    #[test]
    fn non_blocking_keeps_every_issue_not_marked_blocking() {
        let cases = [
            (
                json!({"issues": [
                    {"id": "I-1", "severity": "high", "blocking": true, "description": "b"},
                    {"id": "I-2", "severity": "low", "blocking": false, "file": "src/x.ps1", "description": "nb"},
                    {"id": "I-3", "severity": "medium", "description": "no flag"},
                    {"id": "I-4", "severity": "low", "blocking": "true", "description": "a string"}]}),
                vec![
                    "- I-2 [low] src/x.ps1: nb",
                    "- I-3 [medium] no flag",
                    "- I-4 [low] a string",
                ],
            ),
            (json!({"issues": [{}]}), vec!["-  [] "]),
            (json!({"issues": []}), vec![]),
            (json!({"issues": {}}), vec![]),
            (json!({}), vec![]),
        ];
        for (review, want) in cases {
            assert_eq!(non_blocking(&review), want, "{review}");
        }
    }

    // ── verify ──────────────────────────────────────────────────────────────────────

    #[test]
    fn a_command_result_records_argv_exit_time_and_the_output_tail() {
        let c = CommandSpec {
            name: "test".into(),
            program: "pwsh".into(),
            args: strings(&["-File", "scripts/test.ps1"]),
            timeout_min: None,
        };
        let output: String = (1..=45).map(|i| format!("line {i}\n")).collect();
        let res = CommandResult {
            name: "test".into(),
            exit: Some(1),
            timed_out: false,
            cancelled: false,
            duration_ms: 1500,
            output,
        };
        let tail: Vec<String> = (6..=45).map(|i| format!("line {i}")).collect();
        assert_eq!(
            command_result(&c, &res),
            json!({"name": "test", "argv": ["pwsh", "-File", "scripts/test.ps1"], "exit": 1, "ms": 1500, "output_tail": tail.join("\n")})
        );
        let stopped = CommandResult {
            exit: None,
            output: String::new(),
            ..res
        };
        assert_eq!(command_result(&c, &stopped)["exit"], Value::Null);
        assert_eq!(command_result(&c, &stopped)["output_tail"], json!(""));
    }

    #[test]
    fn verify_feedback_quotes_the_last_command_and_its_output() {
        let failed = [
            json!({"name": "build", "exit": 0, "output_tail": "ok"}),
            json!({"name": "test", "exit": 1, "output_tail": "1 failed\nat line 3"}),
        ];
        let stopped = [json!({"name": "slow", "exit": null, "output_tail": ""})];
        // the exit code is shown with the Debug format of a JSON value ("Number(1)")
        let cases: [(&[Value], &str); 3] = [
            (
                &failed,
                "- VERIFY FAILED: `test` exited Number(1). Last output:\n```\n1 failed\nat line 3\n```",
            ),
            (
                &stopped,
                "- VERIFY FAILED: `slow` exited Null. Last output:\n```\n\n```",
            ),
            (&[], "- VERIFY FAILED: `` exited Null. Last output:\n```\n\n```"),
        ];
        for (results, want) in cases {
            assert_eq!(verify_feedback(results), want, "{results:?}");
        }
    }

    #[test]
    fn the_error_class_is_the_reason_up_to_the_first_colon() {
        let cases = [
            ("artifact_tampered: TASK.md of T-1 …", "artifact_tampered"),
            ("verify_mutated_tree: a: b", "verify_mutated_tree"),
            ("no colon at all", "no colon at all"),
            (": leading", ""),
            ("", ""),
        ];
        for (reason, want) in cases {
            assert_eq!(reason_class(reason, "default"), want, "{reason:?}");
        }
    }

    // ── gates and the next action ───────────────────────────────────────────────────

    #[test]
    fn gate_names_match_the_approval_rows() {
        assert_eq!(gate_name(GateKind::Plan), "plan");
        assert_eq!(gate_name(GateKind::Final), "final");
    }

    #[test]
    fn next_action_names_the_command_for_every_state() {
        let head = || HEAD.to_string();
        let implementing = vec![
            Ev::Start,
            Ev::PlanReady {
                subject: "p".into(),
            },
            Ev::PlanApproved {
                occurrence: 1,
                subject: "p".into(),
            },
        ];
        let with = |base: &[Ev], more: &[Ev]| -> Vec<Ev> { [base, more].concat() };
        let checkpointing = with(&implementing, &[Ev::ImplementationDone]);
        let verifying = with(&checkpointing, &[Ev::Checkpointed { head: head() }]);
        let reviewing = with(&verifying, &[Ev::VerifyPassed { head: head() }]);
        let final_gate = with(&reviewing, &[Ev::ReviewApproved { head: head() }]);
        let run = "`toryo run T-1` (or `toryo resume T-1` after a crash)".to_string();
        let plan_md = task_in(&[])
            .artifacts()
            .join("PLAN.md")
            .display()
            .to_string();
        let escalated = |reason: &str| {
            vec![
                Ev::Start,
                Ev::Escalate {
                    reason: reason.into(),
                },
            ]
        };
        let protected = format!("{}: AGENTS.md changed", protect::REASON);
        let cases = [
            (vec![], "`toryo run T-1`".to_string()),
            (vec![Ev::Start], run.clone()),
            (implementing.clone(), run.clone()),
            (checkpointing, run.clone()),
            (verifying, run.clone()),
            (reviewing.clone(), run.clone()),
            (with(&reviewing, &[Ev::ChangesRequested { head: head() }]), run),
            (
                with(&implementing[..2], &[]),
                format!("review {plan_md}, then `toryo approve T-1 --gate plan --continue` or `toryo reject T-1 --gate plan --reason \"…\"`"),
            ),
            (
                final_gate.clone(),
                "review the branch toryo/T-1 (REVIEW.md in the run store), then `toryo approve T-1 --gate final` or `toryo reject T-1 --gate final --reason \"…\"`".to_string(),
            ),
            (
                with(&final_gate, &[Ev::FinalApproved { occurrence: 2, subject: head() }]),
                "done — merge `toryo/T-1` yourself (toryo never merges); `toryo task cleanup T-1` afterwards".to_string(),
            ),
            (
                escalated(&protected),
                format!("{protected}\n  → restore the file(s) and `toryo resume T-1`, or `toryo resume T-1 --accept-protected-change <path>` (recorded in DECISIONS.md)"),
            ),
            (
                escalated("recovery_policy_violation: partial"),
                "recovery_policy_violation: partial\n  → `toryo resume T-1 --discard-partial` (after a snapshot check) or fix the worktree by hand and `toryo resume T-1 --keep-partial`".to_string(),
            ),
            (
                escalated("the reviewer blocked the task"),
                "the reviewer blocked the task\n  → `toryo resume T-1 --note \"…\"` (another fix cycle), `toryo approve T-1 --gate final --force`, or give up: `toryo task cleanup T-1`".to_string(),
            ),
            (
                vec![Ev::Start, Ev::Pause { reason: "planner_quota: x".into() }],
                "paused — planner_quota: x\n  → `toryo resume T-1`".to_string(),
            ),
            (vec![Ev::Cancel], "cancelled — `toryo task cleanup T-1`".to_string()),
            (
                vec![Ev::Fail { error: "boom".into() }],
                "failed: boom — `toryo task cleanup T-1`".to_string(),
            ),
        ];
        for (events, want) in cases {
            let t = task_in(&events);
            assert_eq!(next_action(&t), want, "{:?}", t.state);
        }
        // a budget pause shows what is used (spent + reserved) of the cap
        let mut t = task_in(&[Ev::Start, Ev::BudgetExceeded]);
        (t.spent_tokens, t.reserved_tokens, t.budget_tokens) = (900, 200, Some(1000));
        assert_eq!(
            next_action(&t),
            "paused (budget): 1100 of 1000 tokens used — `toryo budget set T-1 --tokens N`, then `toryo resume T-1`"
        );
    }
}
