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
//! (R29/R36/R45, M6). Not yet here: budget admission (M7).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::checks::{parse_allow_protected, resolve_program, review_violations, verify_commands, verify_files, Protected};
use super::context;
use super::contracts;
use super::lock::TaskLock;
use super::verify;
use super::protect;
use super::state::{GateKind, PipelineEvent as Ev, PipelineState as St};
use super::store::{self, AttemptEnd, AttemptRow, AttemptStatus, NewAttempt, Store, TaskRow};
use super::task::{frozen_config, sha256_hex, task_git};
use super::worktree::{rel, Worktree, T_AGENT, T_ATTEMPT, T_NO_CHANGE, T_STAGE, T_TASK};
use crate::agents::adapter::TurnHooks;
use crate::agents::hub::AgentHub;
use crate::config::{expand_vars, Billing, CommandSpec, Effective, ProfileKind};
use crate::domain::{ErrorClass, OpenSession, Role, StageOutcome, TurnInput, TurnLimits};
use crate::git::GitCmd;
use crate::runner::{CommandRunner, LocalRunner, RunSpec};

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
        bail!("artifact_tampered: TASK.md of {} does not match tasks.task_sha256 (R27/R28)", t.id);
    }
    let task_md = String::from_utf8_lossy(&task_md).to_string();
    let p = Protected::new(exact, &cfg.repo.policy.protected_paths).with_policy(&parse_allow_protected(&task_md), &cfg.repo.policy.deny_paths);
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
    pub fn new(store: &'a Store, hub: &'a AgentHub, lock: &'a TaskLock, opts: RunOptions) -> Result<Self> {
        let id = lock.task.clone();
        let t = { let i = id.clone(); store.call(move |tx| store::get_task(tx, &i))? };
        let cfg = frozen_config(&t)?;
        let (g, protect, cmds) = task_policy(&t, &cfg)?;
        Ok(Self { store, hub, lock, cfg, g, id, opts, protect, cmds, retries: 0 })
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

    fn hooks(&self, attempt: &str, stop: &super::lock::StopWatch) -> TurnHooks {
        let (st, a) = (self.store.clone(), attempt.to_string());
        TurnHooks {
            cancel: Some(stop.flag.clone()),
            on_session: Some(Arc::new(move |sid: &str| {
                let (a, s) = (a.clone(), sid.to_string());
                let _ = st.call(move |tx| store::set_attempt_session(tx, &a, &s));
            })),
        }
    }

    fn stopped(&self) -> Result<Flow> {
        self.lock.handle_stop("stopped")?;
        eprintln!("■ stop request handled: the stage attempt is `interrupted`; `toryo resume {}` continues", self.id);
        Ok(Flow::Stopped)
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
                    let kind = if t.state == St::WaitingPlanApproval { GateKind::Plan } else { GateKind::Final };
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
        let prev = { let i = self.id.clone(); self.store.call(move |tx| store::last_attempt(tx, &i, "plan"))? };
        // R48: the Planner (Claude) resumes its session for a revision or after an interruption
        let resume = prev.as_ref().and_then(|p| p.external_session_id.clone());
        let prompt = match (&t.feedback, prev.as_ref().map(|p| p.status)) {
            (Some(f), _) => f.clone(),
            (None, Some(AttemptStatus::Interrupted)) => format!(
                "Your previous turn was interrupted before it finished. Create the plan for the task in {} again and return it.",
                art.join("TASK.md").display()
            ),
            _ => format!("Create the plan for the task in {}.", art.join("TASK.md").display()),
        };
        let head = Worktree::new(&self.g, &t.worktree).head()?;
        let a = self.begin(NewAttempt { stage: "plan", kind: "agent", profile: Some(&t.planner), fallback_from: None, head_before: Some(&head), subject_commit: None })?;
        eprintln!("\n── PLAN (attempt {}, {}) ──", a.attempt, t.planner);
        crash_point("plan:after_begin");
        let Some(ctx) = self.context(t, &a, &t.planner)? else { return Ok(Flow::Next) };
        let stop = self.lock.watch_stop();
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
            TurnInput { prompt, label: format!("plan-{}", a.attempt), echo: true, env: self.capture_env() },
            self.limits(),
            self.hooks(&a.id, &stop),
        )?;
        self.usage(&a, &t.planner, &r)?;
        if r.cancelled {
            self.end(&a, AttemptStatus::Interrupted, &r, AttemptEnd::default())?;
            return self.stopped();
        }
        if let Some(flow) = self.after_read_only_turn(t, &a, &r, "planner")? {
            return Ok(flow);
        }
        let Some(plan) = r.structured.clone() else {
            let reason = format!("planner returned no structured plan: {:?} {}", r.error_class, r.error_detail.clone().unwrap_or_default());
            self.end_and(&a, AttemptStatus::Failed, &r, AttemptEnd::default(), Some(Ev::Escalate { reason }), None)?;
            return Ok(Flow::Next);
        };
        let bytes = serde_json::to_vec_pretty(&plan)?;
        let md = super::render::render("plan", &plan)?;
        self.put(t, &a, "plan", "json", &bytes, "plan.json")?;
        self.put(t, &a, "plan-md", "md", md.as_bytes(), "PLAN.md")?;
        eprintln!("\n{md}");
        let files: Vec<String> = plan["files_likely_involved"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
        let bad: Vec<String> = files.iter().filter(|f| self.protect.violates(f)).cloned().collect();
        if !bad.is_empty() {
            eprintln!("!! the plan lists protected or denied paths {bad:?} — rejected automatically (plan-time policy)");
            let fb = format!("Your plan lists protected or denied paths {bad:?} (toryo.yaml policy), which may not change. Create the plan again without touching them.");
            self.end_and(&a, AttemptStatus::Succeeded, &r, AttemptEnd::default(), Some(Ev::PlanPolicyViolation { paths: bad }), Some(Some(fb)))?;
            return Ok(Flow::Next);
        }
        let subject = sha256_hex(&bytes);
        self.end_and(&a, AttemptStatus::Succeeded, &r, AttemptEnd::default(), Some(Ev::PlanReady { subject }), Some(None))?;
        Ok(Flow::Next)
    }

    fn role_model(&self, role: &str) -> Option<String> {
        self.cfg.repo.roles.get(role).and_then(|r| r.model.clone())
    }

    // ── Implementing / Fixing ────────────────────────────────────────────────────────

    fn implement(&mut self, t: &TaskRow) -> Result<Flow> {
        let stage = writing_stage(t.fix_cycle);
        let wt = Worktree::new(&self.g, &t.worktree);
        let limit = if self.opts.no_fallback { 1 } else { t.implementers.len() };
        let Some(profile) = t.implementers.get(t.implementer_index).filter(|_| t.implementer_index < limit).cloned() else {
            let reason = format!(
                "implementer_quota: no implementer left ({}{}); the worktree keeps any partial changes — `toryo resume` when the quota is back",
                t.implementers.join(" → "),
                if self.opts.no_fallback { ", --no-fallback" } else { "" }
            );
            self.apply(Ev::Pause { reason })?;
            return Ok(Flow::Next);
        };
        let kind = self.cfg.profile(&profile)?.kind;
        let prev = { let (i, s) = (self.id.clone(), stage.clone()); self.store.call(move |tx| store::last_attempt(tx, &i, &s))? };
        let mut prompt = "Implement the approved plan (PLAN.md) for the task (TASK.md) in this worktree, following your role and the project rules in the context.\n".to_string();
        if t.fix_cycle > 0 {
            if let Some(f) = &t.feedback {
                prompt.push_str(&format!("\n## Fix cycle {}\nThe previous checkpoint was not accepted. Address the following, then report the issue ids you fixed in `resolved`:\n\n{f}\n", t.fix_cycle));
            }
        }
        if let Some(h) = &t.handoff {
            prompt.push_str(h);
        }
        // R48 recovery policy: Claude resumes the interrupted session, Codex starts fresh
        // (the hand-over note written by recovery names the partial commit)
        let resume = match (&prev, kind) {
            (Some(p), ProfileKind::Claude) if p.status == AttemptStatus::Interrupted && p.profile.as_deref() == Some(profile.as_str()) => p.external_session_id.clone(),
            _ => None,
        };
        let fallback_from = if t.implementer_index > 0 && prev.as_ref().is_none_or(|p| p.profile.as_deref() != Some(profile.as_str())) {
            t.implementers.get(t.implementer_index - 1).cloned()
        } else {
            None
        };
        let (prompt, model) = match kind {
            ProfileKind::Codex => (prompt, self.role_model("implementer").or_else(|| self.cfg.profile(&profile).ok().and_then(|p| p.model.clone()))),
            ProfileKind::Claude => (claude_implementer_prompt(&prompt), None),
        };
        let head_before = wt.head()?;
        let tmp = t.worktree.join(".toryo-tmp");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        let env: BTreeMap<String, String> = [("TEMP", &tmp), ("TMP", &tmp)].into_iter().map(|(k, v)| (k.to_string(), v.display().to_string())).collect();
        let a = self.begin(NewAttempt { stage: &stage, kind: "agent", profile: Some(&profile), fallback_from: fallback_from.as_deref(), head_before: Some(&head_before), subject_commit: None })?;
        eprintln!("\n── {} (attempt {}, {profile}) ──", stage.to_uppercase(), a.attempt);
        crash_point("implement:after_begin");
        let Some(ctx) = self.context(t, &a, &profile)? else { return Ok(Flow::Next) };
        let stop = self.lock.watch_stop();
        let r = self.hub.turn_with(
            OpenSession {
                profile: profile.clone(),
                role: Role::Implementer,
                cwd: t.worktree.clone(),
                run_store: None,
                capture_dir: t.captures(),
                schema: Some(contracts::schema("implementation.schema.json")?.into()),
                context_file: Some(ctx),
                model,
                resume,
            },
            TurnInput { prompt, label: format!("{stage}-{}", a.attempt), env, echo: true },
            self.limits(),
            self.hooks(&a.id, &stop),
        )?;
        crash_point("implement:after_turn");
        self.usage(&a, &profile, &r)?;
        if r.cancelled {
            self.end(&a, AttemptStatus::Interrupted, &r, AttemptEnd::default())?;
            return self.stopped();
        }
        if let Some(v) = &r.structured {
            self.put(t, &a, "implementation", "json", &serde_json::to_vec_pretty(v)?, &format!("implementation-{}.json", t.fix_cycle))?;
        }
        // quota / rate limits: retry, then the next implementer (sticky, R49)
        let switch = match r.error_class {
            Some(ErrorClass::QuotaExceeded) => Some(format!("quota exhausted: {}", r.error_detail.clone().unwrap_or_default())),
            Some(ErrorClass::RateLimited | ErrorClass::Overloaded) if self.retries < 2 => {
                self.retries += 1;
                let base: u64 = std::env::var("TORYO_RETRY_BASE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
                let wait = base * u64::from(self.retries);
                eprintln!("!! {profile}: {:?} — retry {}/2 in {wait}s", r.error_class, self.retries);
                self.end(&a, AttemptStatus::Failed, &r, AttemptEnd::default())?;
                std::thread::sleep(Duration::from_secs(wait));
                return Ok(Flow::Next);
            }
            Some(c @ (ErrorClass::RateLimited | ErrorClass::Overloaded)) => Some(format!("{c:?} after 2 retries")),
            _ => None,
        };
        if let Some(reason) = switch {
            self.retries = 0;
            let changed: Vec<String> = wt.changed()?.into_iter().map(|(s, p)| format!("{s} {p}")).collect();
            let next = t.implementers.get(t.implementer_index + 1).filter(|_| t.implementer_index + 1 < limit).cloned();
            eprintln!("!! implementer {profile}: {reason} → {}", next.as_deref().map(|n| format!("switching to {n}")).unwrap_or_else(|| "no fallback left".into()));
            let note = if changed.is_empty() {
                format!("\n## Hand-over\nThe previous implementer (`{profile}`) stopped before producing any change ({reason}). Start the task from the plan.\n")
            } else {
                format!(
                    "\n## Hand-over\nThe previous implementer (`{profile}`) stopped before finishing ({reason}). The worktree contains its UNCOMMITTED partial changes:\n{}\nInspect them, keep what is correct, fix or complete the rest, and finish the task.\n",
                    changed.iter().map(|x| format!("- `{x}`")).collect::<Vec<_>>().join("\n")
                )
            };
            let (i, aid, idx, st2) = (self.id.clone(), a.id.clone(), t.implementer_index + 1, r.session_id.clone());
            let pause = next.is_none();
            let payload = json!({"from": profile, "to": next, "reason": reason, "partial_changes": changed});
            self.store.call(move |tx| {
                store::finish_attempt(tx, &aid, AttemptStatus::Failed, &AttemptEnd { error_class: Some("QuotaExceeded".into()), external_session_id: st2, ..Default::default() })?;
                store::set_implementer_index(tx, &i, idx)?;
                store::set_handoff(tx, &i, Some(&note))?;
                store::event(tx, Some(&i), Some(&aid), "implementer_switch", &payload)?;
                if pause {
                    store::transition(tx, &i, Ev::Pause { reason: format!("implementer_quota: no implementer left ({reason}); the worktree keeps any partial changes — `toryo resume` when the quota is back") })?;
                }
                Ok(())
            })?;
            return Ok(Flow::Next);
        }
        self.retries = 0;
        // HEAD guard (§8): agents must not commit
        if wt.head()? != head_before {
            eprintln!("!! policy_violation: the agent moved HEAD — soft reset to {}", &head_before[..12]);
            wt.soft_reset(&head_before)?;
            let (i, aid) = (self.id.clone(), a.id.clone());
            self.store.call(move |tx| store::event(tx, Some(&i), Some(&aid), "policy_violation", &json!({"kind": "agent commit", "action": "soft reset"})))?;
        }
        // R35: untracked/ignored protected files or Git metadata → Escalated, nothing else runs
        if let Some(reason) = self.protected_violation(t, true)? {
            eprintln!("!! {reason}");
            self.end_and(&a, AttemptStatus::Failed, &r, AttemptEnd { error_class: Some(protect::REASON.into()), ..Default::default() }, Some(Ev::Escalate { reason }), None)?;
            return Ok(Flow::Next);
        }
        let changed = wt.changed()?;
        let prot: Vec<String> = changed.iter().map(|c| c.1.clone()).filter(|p| self.protect.violates(p)).collect();
        if !prot.is_empty() {
            eprintln!("!! protected or denied paths changed: {prot:?} — restored, nothing committed; sent back to the implementer");
            wt.restore(&prot)?;
            let fb = format!("- POLICY: you changed protected or denied paths {prot:?}. They were restored. Do not modify them; achieve the task another way.");
            self.end_and(&a, AttemptStatus::Failed, &r, AttemptEnd { error_class: Some("policy_violation".into()), error_detail: Some(format!("{prot:?}")), ..Default::default() }, Some(Ev::PathPolicyViolation { paths: prot }), Some(Some(fb)))?;
            return Ok(Flow::Next);
        }
        if !r.success && changed.is_empty() {
            r.print_summary();
            let reason = format!("implementer failed: {:?} {}", r.error_class, r.error_detail.clone().unwrap_or_default());
            self.end_and(&a, AttemptStatus::Failed, &r, AttemptEnd::default(), Some(Ev::Escalate { reason }), None)?;
            return Ok(Flow::Next);
        }
        let i = self.id.clone();
        self.end_and(&a, AttemptStatus::Succeeded, &r, AttemptEnd::default(), Some(Ev::ImplementationDone), None)?;
        self.store.call(move |tx| store::set_handoff(tx, &i, None))?;
        Ok(Flow::Next)
    }

    // ── Checkpointing (idempotent via the trailer search, R40) ───────────────────────

    fn checkpoint(&mut self, t: &TaskRow) -> Result<Flow> {
        let stage = writing_stage(t.fix_cycle);
        let wt = Worktree::new(&self.g, &t.worktree);
        let a = { let (i, s) = (self.id.clone(), stage.clone()); self.store.call(move |tx| store::last_attempt(tx, &i, &s))? }
            .with_context(|| format!("no `{stage}` attempt to checkpoint"))?;
        let implementer = a.profile.clone().unwrap_or_default();
        let (head, kind) = match wt.find_checkpoint(&t.base_commit, &t.id, &stage, a.attempt)? {
            Some(c) if !c.partial => {
                eprintln!("✔ checkpoint {} already committed (trailer match)", &c.sha[..12]);
                (c.sha, if c.no_change { "no_change" } else { "normal" })
            }
            _ => {
                // §8: deny paths are checked again right before the commit
                let denied: Vec<String> = wt.changed()?.into_iter().map(|c| c.1).filter(|p| self.protect.is_denied(p)).collect();
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
        let full = self.g.canonical_diff(&wt.loc, &t.base_commit, Some(&head))?;
        self.put(t, &a, "diff", "patch", &full, "DIFF.patch")?;
        let inc = match &t.last_reviewed_head {
            Some(h) => self.g.canonical_diff(&wt.loc, h, Some(&head))?,
            None => Vec::new(),
        };
        self.put(t, &a, "diff-incremental", "patch", &inc, "DIFF-incremental.patch")?;
        eprintln!("✔ checkpoint {} ({kind})", &head[..12]);
        // R35: a committed tracked protected change allowed by TASK.md becomes the new baseline
        let c = protect::check(self.store, t, &self.g, &self.protect)?;
        let allowed: Vec<String> = c.changes.iter().filter(|x| !x.is_hard() && self.protect.is_allowed(&x.path)).map(|x| x.path.clone()).collect();
        if !allowed.is_empty() && c.hard().is_empty() {
            protect::take_baseline(self.store, t, &self.g, &self.protect, &format!("checkpoint {}: allowed protected change {allowed:?} (TASK.md)", &head[..12]))?;
            eprintln!("   protected baseline renewed: {allowed:?} (## Allow protected)");
        }
        let (i, aid, h, k) = (self.id.clone(), a.id.clone(), head.clone(), kind.to_string());
        self.store.call(move |tx| {
            store::finish_attempt(tx, &aid, AttemptStatus::Succeeded, &AttemptEnd { head_after: Some(h.clone()), checkpoint_kind: Some(k), ..Default::default() })?;
            store::transition(tx, &i, Ev::Checkpointed { head: h })?;
            Ok(())
        })?;
        Ok(Flow::Next)
    }

    // ── Verifying ────────────────────────────────────────────────────────────────────

    fn verify(&mut self, t: &TaskRow) -> Result<Flow> {
        let head = t.machine.checkpoint().context("no checkpoint to verify")?.to_string();
        let a = self.begin(NewAttempt { stage: "verify", kind: "command", profile: None, fallback_from: None, head_before: Some(&head), subject_commit: Some(&head) })?;
        crash_point("verify:after_begin");
        // R36/R45: the boundary is established or the stage stops — never a silent downgrade
        let prepared = verify::boundary(&self.cfg).and_then(|b| verify::prepare(&self.g, t, &self.cfg, &head, b).map(|c| (b, c)));
        let (b, co) = match prepared {
            Ok(x) => x,
            Err(e) if format!("{e:#}").starts_with(verify::UNAVAILABLE) || format!("{e:#}").starts_with(verify::MUTATED) => {
                let reason = format!("{e:#}");
                eprintln!("!! {reason}");
                let class = reason.split(':').next().unwrap_or(verify::UNAVAILABLE).to_string();
                let (i, aid) = (self.id.clone(), a.id.clone());
                self.store.call(move |tx| {
                    store::finish_attempt(tx, &aid, AttemptStatus::Failed, &AttemptEnd { error_class: Some(class), ..Default::default() })?;
                    store::transition(tx, &i, Ev::Escalate { reason })?;
                    Ok(())
                })?;
                return Ok(Flow::Next);
            }
            Err(e) => return Err(e),
        };
        eprintln!("\n── VERIFY (attempt {}, {} in {}) ──", a.attempt, verify::describe(b), co.dir.display());
        let stop = self.lock.watch_stop();
        let env = verify::env(&co);
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
                let (aid,) = (a.id.clone(),);
                self.store.call(move |tx| store::finish_attempt(tx, &aid, AttemptStatus::Interrupted, &AttemptEnd::default()))?;
                return self.stopped();
            }
            let ok = res.ok();
            pass &= ok;
            eprintln!("  {} {} (exit {:?}, {:.1}s)", if ok { "✔" } else { "✘" }, c.name, res.exit, res.duration_ms as f64 / 1000.0);
            // R50: LocalRunner strips ANSI (CSI/OSC) from everything it captures
            results.push(json!({"name": c.name, "argv": std::iter::once(c.program.clone()).chain(c.args.clone()).collect::<Vec<_>>(), "exit": res.exit, "ms": res.duration_ms, "output_tail": res.tail(40)}));
            if !ok {
                break; // stop at the first failure (§5.2)
            }
        }
        // R29/R47: the checkout must be exactly the checkpoint after the run
        let mutated = verify::mutated(&self.g, &co, &head)?;
        let tree = self.g.run_str(&Worktree::new(&self.g, &t.worktree).loc, true, &["rev-parse", &format!("{head}^{{tree}}")])?;
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
        self.put(t, &a, "verify-result", "json", &serde_json::to_vec_pretty(&vr)?, "VERIFY_RESULT.json")?;
        if let Some(reason) = mutated {
            eprintln!("!! {reason}");
            let (i, aid) = (self.id.clone(), a.id.clone());
            self.store.call(move |tx| {
                store::finish_attempt(tx, &aid, AttemptStatus::Failed, &AttemptEnd { error_class: Some(verify::MUTATED.into()), ..Default::default() })?;
                store::transition(tx, &i, Ev::Escalate { reason })?;
                Ok(())
            })?;
            return Ok(Flow::Next);
        }
        // R35: the verify run must not have touched protected state
        if let Some(reason) = self.protected_violation(t, false)? {
            eprintln!("!! {reason}");
            let aid = a.id.clone();
            let i = self.id.clone();
            self.store.call(move |tx| {
                store::finish_attempt(tx, &aid, AttemptStatus::Failed, &AttemptEnd { error_class: Some(protect::REASON.into()), ..Default::default() })?;
                store::transition(tx, &i, Ev::Escalate { reason })?;
                Ok(())
            })?;
            return Ok(Flow::Next);
        }
        let (i, aid, h) = (self.id.clone(), a.id.clone(), head.clone());
        let fb = if pass {
            None
        } else {
            let f = results.last().cloned().unwrap_or(Value::Null);
            Some(format!(
                "- VERIFY FAILED: `{}` exited {:?}. Last output:\n```\n{}\n```",
                f["name"].as_str().unwrap_or(""),
                f["exit"],
                f["output_tail"].as_str().unwrap_or("")
            ))
        };
        self.store.call(move |tx| {
            store::finish_attempt(tx, &aid, if pass { AttemptStatus::Succeeded } else { AttemptStatus::Failed }, &AttemptEnd::default())?;
            if pass {
                store::transition(tx, &i, Ev::VerifyPassed { head: h })?;
            } else {
                store::set_feedback(tx, &i, fb.as_deref())?;
                store::transition(tx, &i, Ev::VerifyFailed { head: h })?;
            }
            Ok(())
        })?;
        Ok(Flow::Next)
    }

    // ── Reviewing ────────────────────────────────────────────────────────────────────

    fn review(&mut self, t: &TaskRow) -> Result<Flow> {
        let head = t.machine.checkpoint().context("no checkpoint to review")?.to_string();
        let art = t.artifacts();
        // R48: a review of the same checkpoint that was interrupted (crash, stop) resumes the
        // Reviewer's session
        let prev = { let i = self.id.clone(); self.store.call(move |tx| store::last_attempt(tx, &i, "review"))? };
        let mut resume: Option<String> = prev
            .filter(|p| p.status == AttemptStatus::Interrupted && p.subject_commit.as_deref() == Some(head.as_str()) && p.profile.as_deref() == Some(t.reviewer.as_str()))
            .and_then(|p| p.external_session_id);
        let a = self.begin(NewAttempt { stage: "review", kind: "agent", profile: Some(&t.reviewer), fallback_from: None, head_before: Some(&head), subject_commit: Some(&head) })?;
        eprintln!("\n── REVIEW (attempt {}, {}{}) ──", a.attempt, t.reviewer, if resume.is_some() { ", resuming the interrupted session" } else { "" });
        crash_point("review:after_begin");
        let Some(ctx) = self.context(t, &a, &t.reviewer)? else { return Ok(Flow::Next) };
        let stop = self.lock.watch_stop();
        let mut prompt = format!(
            "Review the checkpoint {} in this worktree against TASK.md. DIFF.patch is base → checkpoint; DIFF-incremental.patch is the change since your last review (empty on the first). Acceptance criteria ids: {:?}.",
            &head[..12],
            t.acceptance_ids
        );
        if resume.is_some() {
            prompt = format!("Your previous review turn was interrupted before it finished. {prompt} Return the complete review.");
        }
        let mut review = Value::Null;
        let mut valid = false;
        let mut last = StageOutcome::default();
        let mut corrections = 0u32;
        for corr in 0..=2u32 {
            let r = self.hub.turn_with(
                OpenSession {
                    profile: t.reviewer.clone(),
                    role: Role::Reviewer,
                    cwd: t.worktree.clone(),
                    run_store: Some(art.clone()),
                    capture_dir: t.captures(),
                    schema: Some(contracts::schema("review.schema.json")?.into()),
                    context_file: Some(ctx.clone()),
                    model: self.role_model("reviewer"),
                    resume: resume.clone(),
                },
                TurnInput { prompt: prompt.clone(), label: format!("review-{}-{corr}", a.attempt), echo: true, env: self.capture_env() },
                self.limits(),
                self.hooks(&a.id, &stop),
            )?;
            self.usage(&a, &t.reviewer, &r)?;
            if r.cancelled {
                self.end(&a, AttemptStatus::Interrupted, &r, AttemptEnd::default())?;
                return self.stopped();
            }
            if let Some(flow) = self.after_read_only_turn(t, &a, &r, "reviewer")? {
                return Ok(flow);
            }
            review = r.structured.clone().unwrap_or(Value::Null);
            let viol = review_violations(&review, &t.acceptance_ids);
            last = r;
            if viol.is_empty() {
                valid = true;
                break;
            }
            eprintln!("!! invalid review (correction {}/2): {viol:?}", corr + 1);
            corrections = corr + 1;
            prompt = format!("Your previous final response was rejected and was not used. Validation error: {}. Return only a corrected JSON object matching the schema.", viol.join("; "));
            resume = last.session_id.clone();
        }
        if !valid {
            let reason = "InvalidOutput: the review was rejected 3 times (R22)".to_string();
            self.end_and(&a, AttemptStatus::Failed, &last, AttemptEnd { error_class: Some("InvalidOutput".into()), correction_turns: Some(2), ..Default::default() }, Some(Ev::Escalate { reason }), None)?;
            return Ok(Flow::Next);
        }
        let md = super::render::render("review", &review)?;
        self.put(t, &a, "review", "json", &serde_json::to_vec_pretty(&review)?, "review.json")?;
        self.put(t, &a, "review-md", "md", md.as_bytes(), "REVIEW.md")?;
        eprintln!("\n{md}");
        let mut decision = review["decision"].as_str().unwrap_or("").to_string();
        let unmet: Vec<String> = review["acceptance_criteria"].as_array().into_iter().flatten().filter(|x| x["met"].as_bool() != Some(true)).filter_map(|x| x["id"].as_str().map(str::to_string)).collect();
        let blocking = review["issues"].as_array().into_iter().flatten().any(|i| i["blocking"].as_bool() == Some(true));
        if decision == "approved" && (!unmet.is_empty() || blocking) {
            eprintln!("!! review_inconsistent: `approved` with unmet {unmet:?} / blocking issues → changes_requested");
            decision = "changes_requested".into();
        }
        let (event, feedback) = match decision.as_str() {
            "approved" => (Ev::ReviewApproved { head: head.clone() }, None),
            "blocked" => (Ev::ReviewBlocked { head: head.clone() }, None),
            _ => (Ev::ChangesRequested { head: head.clone() }, Some(review_feedback(&review))),
        };
        let (i, h) = (self.id.clone(), head.clone());
        self.store.call(move |tx| store::set_last_reviewed(tx, &i, &h))?;
        self.end_and(&a, AttemptStatus::Succeeded, &last, AttemptEnd { correction_turns: Some(corrections), ..Default::default() }, Some(event), feedback.map(Some))?;
        Ok(Flow::Next)
    }

    // ── protected state (R35) and pauses (R49) ───────────────────────────────────────

    /// Compare the protected state with the baseline in force. `only_hard`: right after an
    /// Implementer turn, where tracked protected changes go to the path policy instead.
    /// Changes the task may make (`## Allow protected`, tracked) never count. Returns the
    /// escalation reason.
    fn protected_violation(&self, t: &TaskRow, only_hard: bool) -> Result<Option<String>> {
        let c = protect::check(self.store, t, &self.g, &self.protect)?;
        let bad: Vec<&protect::Change> = c.changes.iter().filter(|x| x.is_hard() || (!only_hard && !self.protect.is_allowed(&x.path))).collect();
        if bad.is_empty() {
            return Ok(None);
        }
        let (i, payload) = (self.id.clone(), json!({"changes": bad, "baseline": c.baseline_id, "diff": c.diff_path}));
        self.store.call(move |tx| store::event(tx, Some(&i), None, "protected_changed", &payload))?;
        Ok(Some(protect::escalation_reason(&t.id, &bad, c.diff_path.as_deref())))
    }

    /// After a Planner/Reviewer turn: quota → `Paused` (no Planner/Reviewer fallback in
    /// v0.1, R49); a changed protected state → `Escalated`.
    fn after_read_only_turn(&self, t: &TaskRow, a: &AttemptRow, r: &StageOutcome, role: &str) -> Result<Option<Flow>> {
        if let Some(c @ (ErrorClass::QuotaExceeded | ErrorClass::RateLimited | ErrorClass::Overloaded)) = r.error_class {
            let reason = format!("{role}_quota: {c:?} {} — `toryo resume {}` when the quota is back (no {role} fallback in v0.1)", r.error_detail.clone().unwrap_or_default(), t.id);
            eprintln!("!! {reason}");
            self.end_and(a, AttemptStatus::Failed, r, AttemptEnd::default(), Some(Ev::Pause { reason }), None)?;
            return Ok(Some(Flow::Next));
        }
        if let Some(reason) = self.protected_violation(t, false)? {
            eprintln!("!! {reason}");
            self.end_and(a, AttemptStatus::Failed, r, AttemptEnd { error_class: Some(protect::REASON.into()), ..Default::default() }, Some(Ev::Escalate { reason }), None)?;
            return Ok(Some(Flow::Next));
        }
        Ok(None)
    }

    // ── attempt helpers ──────────────────────────────────────────────────────────────

    /// Publish an immutable artifact version for this attempt (R34) + its view.
    fn put(&self, t: &TaskRow, a: &AttemptRow, logical: &str, ext: &str, bytes: &[u8], view: &str) -> Result<super::artifacts::Version> {
        super::artifacts::put(self.store, t, Some(&a.id), logical, ext, logical, bytes, Some(view))
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
                let class = reason.split(':').next().unwrap_or("artifact_tampered").to_string();
                let (i, aid) = (self.id.clone(), a.id.clone());
                self.store.call(move |tx| {
                    store::finish_attempt(tx, &aid, AttemptStatus::Failed, &AttemptEnd { error_class: Some(class), ..Default::default() })?;
                    store::transition(tx, &i, Ev::Escalate { reason })?;
                    Ok(())
                })?;
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let c = context::assemble(&req)?;
        for w in &c.warnings {
            eprintln!("!! {w}");
        }
        eprintln!("   context: {} bytes, {} sources, sha256 {}", c.text.len(), c.sources.len(), &c.sha256[..12]);
        let logical = format!("context/{}-{}", a.stage, a.attempt);
        let v = super::artifacts::put(self.store, t, Some(&a.id), &logical, "md", "context", c.text.as_bytes(), Some(&format!("context/{}-{}.md", a.stage, a.attempt)))?;
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

    fn usage(&self, a: &AttemptRow, profile: &str, r: &StageOutcome) -> Result<()> {
        let (i, aid, p, b, u) = (self.id.clone(), a.id.clone(), profile.to_string(), self.billing(profile), r.usage);
        self.store.call(move |tx| store::record_usage(tx, &i, &aid, &p, b, u))
    }

    fn end(&self, a: &AttemptRow, status: AttemptStatus, r: &StageOutcome, end: AttemptEnd) -> Result<()> {
        self.end_and(a, status, r, end, None, None)
    }

    /// Finish the attempt, optionally set the feedback and apply a transition — one transaction.
    fn end_and(&self, a: &AttemptRow, status: AttemptStatus, r: &StageOutcome, mut end: AttemptEnd, e: Option<Ev>, feedback: Option<Option<String>>) -> Result<()> {
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
}

fn review_feedback(review: &Value) -> String {
    let mut f = String::new();
    for ac in review["acceptance_criteria"].as_array().into_iter().flatten().filter(|x| x["met"].as_bool() != Some(true)) {
        f.push_str(&format!("- {} NOT MET: {}\n", ac["id"].as_str().unwrap_or(""), ac["evidence"].as_str().unwrap_or("")));
    }
    for i in review["issues"].as_array().into_iter().flatten() {
        f.push_str(&format!(
            "- {} [{}{}] {}{}\n",
            i["id"].as_str().unwrap_or(""),
            i["severity"].as_str().unwrap_or(""),
            if i["blocking"].as_bool() == Some(true) { ", blocking" } else { "" },
            i["file"].as_str().map(|x| format!("{x}: ")).unwrap_or_default(),
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
    let gate = t.machine.gate().filter(|g| g.kind == kind).with_context(|| format!("{}: no open {kind:?} gate (state {:?})", t.id, t.state))?.clone();
    if let Some(e) = expect {
        let digest = gate_digest(store, &t.id, kind, gate.occurrence)?;
        if !digest.starts_with(e) {
            bail!("subject_changed: the gate subject digest is {} — it does not start with --expect {e} (R32)", &digest[..12]);
        }
    }
    let e = match kind {
        GateKind::Plan => {
            let (_, bytes) = super::artifacts::read_head(store, t, "plan")?.context("no registered plan")?;
            if sha256_hex(&bytes) != gate.subject {
                bail!("subject_changed: plan.json no longer matches the plan under approval (R32)");
            }
            Ev::PlanApproved { occurrence: gate.occurrence, subject: gate.subject }
        }
        GateKind::Final => Ev::FinalApproved { occurrence: gate.occurrence, subject: gate.subject },
    };
    let i = t.id.clone();
    let m = store.call(move |tx| store::transition(tx, &i, e))?;
    eprintln!("   [state] {:?} --{}_approved--> {:?}", t.state, if kind == GateKind::Plan { "plan" } else { "final" }, m.state());
    Ok(())
}

/// Reject the open gate with a reason; the revision opens a new occurrence (R44a).
pub fn reject_now(store: &Store, t: &TaskRow, kind: GateKind, reason: &str) -> Result<()> {
    let gate = t.machine.gate().filter(|g| g.kind == kind).with_context(|| format!("{}: no open {kind:?} gate (state {:?})", t.id, t.state))?.clone();
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
    Ok(())
}

pub fn gate_digest(store: &Store, task: &str, kind: GateKind, occurrence: u32) -> Result<String> {
    let t = task.to_string();
    let rows = store.call(move |tx| store::approvals(tx, &t))?;
    let g = if kind == GateKind::Plan { "plan" } else { "final" };
    rows.into_iter()
        .find(|r| r.gate == g && r.occurrence == occurrence)
        .map(|r| r.subject_digest)
        .context("no approval row for the open gate")
}

/// Is there a passing, hash-checked verify result for the current checkpoint (R29)?
pub fn verify_current(store: &Store, t: &TaskRow) -> Result<bool> {
    let Some((_, b)) = super::artifacts::read_head(store, t, "verify-result")? else { return Ok(false) };
    let v: Value = serde_json::from_slice(&b)?;
    Ok(v["pass"].as_bool() == Some(true) && v["invalidated"].as_bool() != Some(true) && v["head"].as_str().is_some() && v["head"].as_str() == t.machine.checkpoint())
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
        "# {} — {:?}\n\n{}\n\n- branch: `{}` (base `{}`) in `{}`\n- fix cycles: {} / {}\n- tokens: {} spent{}\n- next: {}\n\n## Stage attempts\n\n| attempt | status | profile | error | checkpoint |\n|---|---|---|---|---|\n",
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
        next_action(t)
    );
    for a in atts {
        s.push_str(&format!(
            "| {} | {:?} | {} | {} | {} |\n",
            a.id,
            a.status,
            a.profile.unwrap_or_default(),
            a.error_class.unwrap_or_default(),
            a.git_head_after.or(a.partial_commit.map(|p| format!("{p} (partial)"))).map(|h| h.chars().take(20).collect::<String>()).unwrap_or_default()
        ));
    }
    let review: Value = std::fs::read(t.artifacts().join("review.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
    let open: Vec<String> = review["issues"].as_array().into_iter().flatten().map(|i| format!("- {} [{}] {}", i["id"].as_str().unwrap_or(""), i["severity"].as_str().unwrap_or(""), i["description"].as_str().unwrap_or(""))).collect();
    if !open.is_empty() {
        s.push_str(&format!("\n## Open review issues (R53)\n{}\n", open.join("\n")));
    }
    let p = t.artifacts().join("SUMMARY.md");
    std::fs::write(&p, s)?;
    Ok(p)
}
