//! The Verifying stage (R23-27, moved as it was): the preflights, the check gates in the verify
//! checkout and their results and feedback.

use super::*;

impl<'a> Engine<'a> {
    pub(super) fn verify(&mut self, t: &TaskRow) -> Result<Flow> {
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
        // R59: the machine's X++ toolchain must still be there (a NuGet folder moved or
        // replaced since `task create`) — not something another fix cycle can repair
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
                return Ok(Flow::Next);
            }
        };
        // M11-R16 (X12): what the environment gates of this run need from the machine is
        // checked before any check gate runs — a build of tens of minutes is not spent on a
        // gate that cannot start. (Checked again when each gate starts: things can change.)
        if let Some(reason) = self.environment_gates_preflight(t) {
            self.escalate_attempt(&a, ENV_UNAVAILABLE.into(), reason)?;
            return Ok(Flow::Next);
        }
        // M12-05: the same for the check gates — a program that is not on this machine is a
        // machine stop before any gate runs, not a failing gate and not a plain error
        if let Some(reason) = self.check_gates_preflight(t) {
            self.escalate_attempt(&a, GATE_UNAVAILABLE.into(), reason)?;
            return Ok(Flow::Next);
        }
        // M12-07 (X17): a gate that asks before it deploys — before any gate runs, so nothing
        // was built or deployed. A plain `resume` is the "yes".
        if let Some(reason) = self.confirm_preflight(t, &head)? {
            self.escalate_attempt(&a, crate::engine::gates::ENV_CONFIRM.into(), reason)?;
            return Ok(Flow::Next);
        }
        // M16-02 (X36, O-23): one task at a time runs its check gates on this machine — from
        // before the checkout is reset until its checks after the check gates are done; released
        // before the environment gates (they read a fresh export no Low code can write, X6)
        let Some(verify_lock) = self.verify_lock(t, &self.lock.watch_stop())? else {
            self.finish_and(
                &a,
                AttemptStatus::Interrupted,
                AttemptEnd::default(),
                None,
                None,
            )?;
            return self.stopped();
        };
        let verify_lock = Held::new(self.rec, crate::engine::lock::VERIFY_LOCK, verify_lock);
        // M16-R4: named as the holder only now that its `lock_acquired` is recorded
        if let Some(l) = &verify_lock.lock {
            l.announce(&t.id);
        }
        let Some((b, co)) = self.verify_checkout(t, &a, &head)? else {
            return Ok(Flow::Next);
        };
        self.stage_started(
            &a,
            Some(format!("{} in {}", verify::describe(b), co.dir.display())),
        );
        let stop = self.lock.watch_stop();
        let interrupted = |e: &Self| -> Result<Flow> {
            e.finish_and(
                &a,
                AttemptStatus::Interrupted,
                AttemptEnd::default(),
                None,
                None,
            )?;
            e.stopped()
        };
        // M17-A4: what the red-green gate needs — the proof map of the attempt that made the
        // checkpoint, the exempt criteria, and where its red checkout goes
        let proof = ProofInput {
            proof: self.proof_of(t, &head)?,
            exempt: self.proof_exempt(t)?,
            slot: verify::Slot::main(t, &self.cfg),
        };
        let ran = match self.run_verify_commands(b, &co, &head, &stop, xpp.as_ref(), &proof) {
            Ok(r) => r,
            // M12-05: the program disappeared between the preflight and the start (or is no
            // executable): the same class, with the OS error
            Err(e) => return self.gate_start_failed(t, &a, e),
        };
        let Some(VerifyRun {
            mut results,
            mut failure,
            red_green,
            red_mutated,
        }) = ran
        else {
            return interrupted(self);
        };
        // M11-R9 (X6, replaces X2): the environment gates read a fresh export of the commit,
        // never the Low checkout. Before the first one, a check gate that changed a tracked file
        // is still wrong (R29: escalate, no environment gate); what it left besides is not
        // deployed — one warning and `checkout_extra` in VERIFY_RESULT.json
        // M17-A6: a red run that changed the red checkout is `verify_mutated_tree`, before
        // anything else, and no environment gate runs
        let mut before_deploy: Option<String> = red_mutated;
        let mut extra: Option<Vec<String>> = None;
        if failure.is_none() && before_deploy.is_none() && self.deploys() {
            before_deploy = verify::mutated(&self.g, &co, &head)?;
            let found = verify::checkout_extra(&self.g, &co)?;
            if !found.is_empty() {
                crate::errln!(
                    "!! a check gate left files in the verify checkout — they are not deployed: {:?}",
                    found.iter().take(10).collect::<Vec<_>>()
                );
                extra = Some(found.into_iter().take(10).collect());
            }
        }
        // M16-02 (X36): what the check gates ran, hashed while no other task's Low code can
        // change the checkout; then the next task may run its check gates while the
        // environment gates of this one wait for their own lock (M11-E3)
        let scripts = verify::script_hashes(
            &verify_files(&crate::engine::checks::with_red_green(&self.cmds), &co.dir),
            &co,
        );
        // M16-R1: when no environment gate follows (none is defined, or a check gate failed),
        // this is the one check of the checkout after the run — made under the lock, so that no
        // other task's Low code can hide a mutation (or add one) before it is looked at
        let locked_mutated = if failure.is_none() && self.deploys() {
            None
        } else {
            Some(verify::mutated(&self.g, &co, &head)?)
        };
        drop(verify_lock);
        // M11-E3: the environment gates, only after every check gate passed
        let mut export_mutated: Option<String> = None;
        if failure.is_none() && before_deploy.is_none() {
            match self.run_environment_gates(t, &co, &head, &stop, &mut results)? {
                EnvGates::Done(f) => failure = f,
                EnvGates::Stopped => return interrupted(self),
                EnvGates::Unavailable(reason) => {
                    self.escalate_attempt(&a, ENV_UNAVAILABLE.into(), reason)?;
                    return Ok(Flow::Next);
                }
                EnvGates::Mutated(reason) => export_mutated = Some(reason),
                // M12-06 (X18): the owner's problem, not the implementer's
                EnvGates::TimedOut(reason) => {
                    self.escalate_attempt(&a, ENV_TIMEOUT.into(), reason)?;
                    return Ok(Flow::Next);
                }
            }
        }
        let pass = failure.is_none() && before_deploy.is_none();
        // R29/R47: the checkout must be exactly the checkpoint after the run; M11-R9: the
        // export too, after each environment gate (E3 AC-8)
        let mutated = match (before_deploy.or(export_mutated), locked_mutated) {
            (Some(reason), _) => Some(reason),
            (None, Some(m)) => m,
            // after the environment gates: a re-check that can only add a mutation (the
            // check before them was made under the lock)
            (None, None) => verify::mutated(&self.g, &co, &head)?,
        };
        let tree = self.g.run_str(
            &Worktree::new(&self.g, &t.worktree).loc,
            true,
            &["rev-parse", &format!("{head}^{{tree}}")],
        )?;
        let mut vr = json!({
            "head": head,
            "tree": tree.trim(),
            "pass": pass && mutated.is_none(),
            "boundary": verify::describe(b),
            "checkout": self.cfg.repo.verify.checkout,
            "verify_checkout": co.dir,
            "config_sha256": t.config_sha256,
            "scripts": scripts,
            "mutated": mutated,
            "commands": results,
        });
        // R59: an X++ build proves compile level only — no DB sync, no unit tests
        if results.iter().any(|r| r["kind"] == "xpp") {
            vr["scope"] = json!("compile");
        }
        if let Some(x) = extra {
            vr["checkout_extra"] = json!(x);
        }
        // M17-A7 (AC-11): only with a red-green gate — every other result keeps its bytes
        if let Some(r) = &red_green {
            vr["red_green"] = json!(r);
        }
        self.put(
            t,
            &a,
            "verify-result",
            "json",
            &serde_json::to_vec_pretty(&vr)?,
            "VERIFY_RESULT.json",
        )?;
        if let Some(reason) = mutated {
            self.escalate_attempt(&a, verify::MUTATED.into(), reason)?;
            return Ok(Flow::Next);
        }
        // R35: the verify run must not have touched protected state
        if let Some(reason) = self.protected_violation(t, false)? {
            self.escalate_attempt(&a, protect::REASON.into(), reason)?;
            return Ok(Flow::Next);
        }
        let (status, feedback, event) = match failure {
            None => (AttemptStatus::Succeeded, None, Ev::VerifyPassed { head }),
            Some(f) => (
                AttemptStatus::Failed,
                Some(Some(f)),
                Ev::VerifyFailed { head },
            ),
        };
        self.finish_and(&a, status, AttemptEnd::default(), feedback, Some(event))?;
        Ok(Flow::Next)
    }

    /// R36/R45: the execution boundary and the detached checkout of `head` are established,
    /// or the stage stops — never a silent downgrade. `None`: the boundary is unavailable or
    /// the checkout was mutated, and the task escalated.
    pub(super) fn verify_checkout(
        &self,
        t: &TaskRow,
        a: &AttemptRow,
        head: &str,
    ) -> Result<Option<(Boundary, verify::Checkout)>> {
        let prepared = verify::boundary(&self.cfg)
            .and_then(|b| verify::prepare(&self.g, t, &self.cfg, head, b).map(|c| (b, c)));
        match prepared {
            Ok(x) => Ok(Some(x)),
            // M11-R16 (X8): the class comes typed with the error, never from its text
            Err(e) => match e.downcast_ref::<verify::Stop>().map(|s| s.class) {
                Some(class) => {
                    let mut reason = format!("{e:#}");
                    if class == verify::UNAVAILABLE {
                        reason = format!(
                            "{reason} — {}",
                            machine_fix_hint(
                                &t.id,
                                "the rights on the verify root",
                                "`verify.verify_root` or `verify.boundary`"
                            )
                        );
                    }
                    self.escalate_attempt(a, class.to_string(), reason)?;
                    Ok(None)
                }
                None => Err(e),
            },
        }
    }

    /// M11-R16 (X12): the reason the first environment gate of this run that the machine
    /// defines cannot start here ([`Self::gate_ready`]), or `None`. Nothing is run, locked or
    /// recorded; a resolved secret is dropped at once.
    pub(super) fn environment_gates_preflight(&self, t: &TaskRow) -> Option<String> {
        let team = self.cfg.team.as_ref()?;
        team.handoff_gates()
            .into_iter()
            .filter(|(_, g)| g.decl.kind == crate::team::GateType::Environment)
            .find_map(|(name, g)| {
                g.machine
                    .as_ref()
                    .and_then(|m| self.gate_ready(t, name, m).err())
            })
    }

    /// M12-05: the reason the first check gate of this run cannot start here — its program is
    /// not on the PATH the gate gets, or an absolute path does not exist
    /// ([`crate::engine::gates::check_gate_problem`]) — or `None`. Nothing is run.
    pub(super) fn check_gates_preflight(&self, t: &TaskRow) -> Option<String> {
        let path = std::env::var_os("PATH");
        self.cmds.iter().find_map(|c| {
            crate::engine::gates::check_gate_problem(c, path.as_deref()).map(|why| {
                format!(
                    "{GATE_UNAVAILABLE}: check gate `{}` cannot start — {why} — {}",
                    c.name,
                    machine_fix_hint(
                        &t.id,
                        "the program installed or put on the PATH; or the working directory and the rights it needs",
                        "`commands.verify` in toryo.yaml (or the gate in the org chart)"
                    )
                )
            })
        })
    }

    /// M12-07 (X17): the reason this verify run must wait for the owner — an environment gate of
    /// the run whose `confirm` is `first` (once per task) or `always` (once per checkpoint
    /// commit) and that was not confirmed yet — or `None`. Writes what a plain `resume` will
    /// confirm. Nothing is run.
    pub(super) fn confirm_preflight(&self, t: &TaskRow, head: &str) -> Result<Option<String>> {
        use crate::config::Confirm;
        let Some(team) = self.cfg.team.as_ref() else {
            return Ok(None);
        };
        // M13-05 (X26): the yes is a database row bound to the gate, the commit and the task's
        // frozen configuration digest — `first`: any commit under this digest; `always`: this
        // commit. A reload that changes the digest asks again.
        let gates: Vec<(&str, &crate::config::EnvGate)> = team
            .handoff_gates()
            .into_iter()
            .filter(|(_, g)| g.decl.kind == crate::team::GateType::Environment)
            .filter_map(|(name, g)| g.machine.as_ref().map(|m| (name, m)))
            .filter(|(_, m)| m.confirm != Confirm::Never)
            .collect();
        if gates.is_empty() {
            return Ok(None);
        }
        let ask: Vec<(String, bool)> = gates
            .iter()
            .map(|(name, m)| (name.to_string(), m.confirm == Confirm::Always))
            .collect();
        let (i, digest, h) = (self.id.clone(), t.config_sha256.clone(), head.to_string());
        // (confirmed, a yes exists under another configuration) per gate
        let state: Vec<(bool, bool)> = self.store.call(move |tx| {
            ask.iter()
                .map(|(gate, always)| {
                    Ok((
                        store::deploy_confirmed(
                            tx,
                            &i,
                            gate,
                            &digest,
                            always.then_some(h.as_str()),
                        )?,
                        store::deploy_confirmed_under_another_config(tx, &i, gate, &digest)?,
                    ))
                })
                .collect()
        })?;
        let asked: Vec<(&str, &crate::config::EnvGate, bool)> = gates
            .into_iter()
            .zip(state)
            .filter(|(_, (confirmed, _))| !confirmed)
            .map(|((name, m), (_, changed))| (name, m, changed))
            .collect();
        if asked.is_empty() {
            return Ok(None);
        }
        let asks: Vec<store::DeployAsk> = asked
            .iter()
            .map(|(name, m, _)| store::DeployAsk {
                gate: name.to_string(),
                commit: head.to_string(),
                mode: m.confirm.as_str().to_string(),
            })
            .collect();
        let (i, digest) = (self.id.clone(), t.config_sha256.clone());
        self.store
            .call(move |tx| store::ask_deploys(tx, &i, &digest, &asks))?;
        let wt = Worktree::new(&self.g, &t.worktree);
        let stat = self
            .g
            .run_str(
                &wt.loc,
                true,
                &["diff", "--stat", &format!("{}..{head}", t.base_commit)],
            )
            .unwrap_or_default();
        let stat: Vec<&str> = stat.lines().take(20).collect();
        let mut s = format!(
            "{}: the checkpoint {} would be deployed to the environment before the review and before any human has read it.\n",
            crate::engine::gates::ENV_CONFIRM,
            short(head)
        );
        for (name, m, changed) in &asked {
            let argv: Vec<&str> = std::iter::once(m.program.as_str())
                .chain(m.args.iter().map(String::as_str))
                .collect();
            s.push_str(&format!(
                "  gate `{name}` (lock {}, confirm: {}) runs: {}{}\n",
                crate::engine::lock::lock_key(&m.lock),
                m.confirm.as_str(),
                argv.join(" "),
                if *changed {
                    " — asked again: the task's configuration changed since your last yes (`task config --reload`)"
                } else {
                    ""
                }
            ));
        }
        s.push_str(&format!(
            "  changes since the base commit (git diff --stat, first 20 lines):\n{}\n",
            stat.iter()
                .map(|l| format!("    {l}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
        s.push_str(&format!(
            "  look at it (`git -C {} show --stat {}`), then `toryo resume {}` allows the deploy",
            t.worktree.display(),
            short(head),
            t.id
        ));
        Ok(Some(s))
    }

    /// M12-05: a verify run ended in an error — when that is a command that could not be
    /// started ([`crate::runner::StartFailed`]), the attempt ends `gate_unavailable` (machine-
    /// caused: plain `resume` goes back into the stage); any other error stays an error.
    pub(super) fn gate_start_failed(
        &self,
        t: &TaskRow,
        a: &AttemptRow,
        e: anyhow::Error,
    ) -> Result<Flow> {
        // M17-A5: the red checkout could not be made at the boundary, or is not clean — the
        // class of the verify checkout (X8), typed with the error
        if let Some(stop) = e.downcast_ref::<verify::Stop>() {
            let class = stop.class.to_string();
            self.escalate_attempt(a, class, format!("{e:#}"))?;
            return Ok(Flow::Next);
        }
        let Some(s) = e.downcast_ref::<crate::runner::StartFailed>() else {
            return Err(e);
        };
        let reason = format!(
            "{GATE_UNAVAILABLE}: check gate `{}` could not be started ({}): {} — {}",
            s.name,
            s.program,
            s.error,
            machine_fix_hint(
                &t.id,
                "the program installed or put on the PATH; or the working directory and the rights it needs",
                "`commands.verify` in toryo.yaml (or the gate in the org chart)"
            )
        );
        self.escalate_attempt(a, GATE_UNAVAILABLE.into(), reason)?;
        Ok(Flow::Next)
    }

    /// M11-R16: what the environment gate `name` needs from this machine before it can start —
    /// no `%NAME%` left in its command (a task frozen before M11-R3), a valid lock name, every
    /// `secret_env` reference resolved, the program found and outside the roots agent code can
    /// write. `Ok`: the program to start and the resolved secrets; `Err`: the
    /// `environment_gate_unavailable` reason, with the way out.
    pub(super) fn gate_ready(
        &self,
        t: &TaskRow,
        name: &str,
        m: &crate::config::EnvGate,
    ) -> std::result::Result<(PathBuf, BTreeMap<String, String>), String> {
        // M11-R12 (item 3): a frozen config has no machine path, so the hint names no file
        let unavailable = |what: String| {
            format!(
                "{ENV_UNAVAILABLE}: environment gate `{name}`: {what} — {}",
                machine_fix_hint(
                    &t.id,
                    "a value added to Credential Manager, a program installed",
                    &format!("`environment_gates.{name}` in the machine config")
                )
            )
        };
        // M11-R12 (item 2): a task frozen before R3 may still hold `%VAR%` in program/args
        // (they are expanded at load now, not at run time) — do not run it half-expanded
        if let Some(var) = gate_residual_var(&m.program, &m.args) {
            return Err(format!(
                "{ENV_UNAVAILABLE}: environment gate `{name}`: the command still contains `%{var}%` (a task created before M11 expanded gate variables at load) — run `toryo task config {} --reload`, then `toryo resume {}`",
                t.id, t.id
            ));
        }
        // (only a change of the config helps here: no "fixed on the machine" half)
        if let Some(problem) =
            crate::engine::lock::lock_name_problem(&crate::engine::lock::lock_key(&m.lock))
        {
            return Err(format!(
                "{ENV_UNAVAILABLE}: environment gate `{name}`: lock `{}`: {problem} — change `environment_gates.{name}.lock` in the machine config, then `toryo task config {} --reload` and `toryo resume {}`",
                m.lock, t.id, t.id
            ));
        }
        let mut secrets = BTreeMap::new();
        for (k, r) in &m.secret_env {
            match crate::config::resolve_secret(r) {
                Ok(v) => {
                    secrets.insert(k.clone(), v);
                }
                Err(e) => return Err(unavailable(format!("secret_env.{k}: {e:#}"))),
            }
        }
        let program = env_gate_program(&self.cfg, &m.program, std::env::var_os("PATH").as_deref())
            .map_err(unavailable)?;
        // M12-05: the script it is handed (`-File`, an absolute script path) is there — a typo
        // in the machine config is not a failing gate
        if let Some(why) = crate::engine::gates::script_problems(&m.args)
            .into_iter()
            .next()
        {
            return Err(unavailable(why));
        }
        Ok((program, secrets))
    }

    /// Run the verify commands in the checkout, inside the boundary, and stop at the first
    /// failure (§5.2). A `tool: xpp` command runs MSBuild once per project, in order, within
    /// the command's `timeout_min` (R59). Returns each run's result and, if one failed, the
    /// feedback for the implementer; `None` when a stop request ended a command (R38).
    pub(super) fn run_verify_commands(
        &self,
        b: Boundary,
        co: &verify::Checkout,
        head: &str,
        stop: &StopWatch,
        xpp: Option<&xpp::Toolchain>,
        proof: &ProofInput,
    ) -> Result<Option<VerifyRun>> {
        let mut env = verify::env(co);
        // M11-E1: TORYO_TASK, TORYO_SHA, TORYO_VERIFY_CHECKOUT for every command
        verify::set_gate_vars(&mut env, &self.id, head, co, &co.dir);
        // M12-15 (X23): English messages from every check gate, not only MSBuild
        xpp::set_ui_language(&mut env, &BTreeMap::new());
        let t = self.task()?;
        let at = VerifyAt {
            co,
            env,
            boundary: b,
            stop,
            redact: crate::engine::flight::task_redactions(&self.cfg, &t),
        };
        let mut results = Vec::new();
        let mut red_green = None;
        for c in &self.cmds {
            // M17-A4 (AC-4): a red-green gate checks the proof map first — no process starts,
            // not even the gate's own args, for a map that is not valid
            let plan = match &c.red_green {
                None => None,
                Some(rg) => match crate::engine::redgreen::plan(
                    proof.proof.as_ref(),
                    &t.acceptance_ids,
                    &proof.exempt,
                    rg.max_tests(),
                ) {
                    Ok(p) => Some((rg, p)),
                    Err(problems) => {
                        let f = map_feedback(&c.name, &problems);
                        let r = crate::engine::redgreen::RedGreenResult::refused(
                            &c.name,
                            &t.base_commit,
                            &t.acceptance_ids,
                            &proof.exempt,
                            problems,
                        );
                        self.red_green_finished(&r);
                        return Ok(Some(VerifyRun {
                            results,
                            failure: Some(untrusted::wrap("verify", None, &f)),
                            red_green: Some(r),
                            red_mutated: None,
                        }));
                    }
                },
            };
            let ran = match (c.kind(), xpp) {
                (CommandKind::Program { program, args }, _) => {
                    let program = verify_program(&expand_vars(program)?, &co.dir);
                    let limit = xpp::command_timeout(c);
                    let run = at.spec(&c.name, program, args.to_vec(), limit);
                    self.run_verify_command(run, stop)?.map(|res| {
                        // `diagnostics: msbuild`: the first real errors ahead of the tail
                        let diags = c
                            .reads_msbuild()
                            .then(|| xpp::diagnostics(&res.output, &co.dir));
                        results.push(command_result_with(c, &res, diags.as_ref()));
                        (!res.ok()).then(|| {
                            let errors = diags
                                .as_ref()
                                .map(|d| xpp::feedback_errors_as(d, "Build"))
                                .unwrap_or_default();
                            verify_feedback_in(&res, &errors, Some(limit))
                        })
                    })
                }
                (CommandKind::Xpp { .. }, Some(tc)) => {
                    self.run_xpp_command(c, tc, &at, &mut results)?
                }
                (CommandKind::Xpp { .. }, None) => {
                    bail!("{}: `tool: xpp` without a resolved toolchain", c.name)
                }
            };
            match ran {
                None => return Ok(None),
                // R61: verify output comes from the code under test — data, not instructions
                Some(Some(f)) => {
                    return Ok(Some(VerifyRun {
                        results,
                        failure: Some(untrusted::wrap("verify", None, &f)),
                        red_green: None,
                        red_mutated: None,
                    }))
                }
                Some(None) => {}
            }
            // M17-A6: the gate's own args passed — its tests, one by one, green then red
            if let Some((rg, plan)) = plan {
                let Some(out) = self.red_green_run(c, rg, plan, &at, b, head, proof)? else {
                    return Ok(None);
                };
                red_green = Some(out.result);
                if out.mutated.is_some() || out.failure.is_some() {
                    return Ok(Some(VerifyRun {
                        results,
                        failure: out.failure.map(|f| untrusted::wrap("verify", None, &f)),
                        red_green,
                        red_mutated: out.mutated,
                    }));
                }
            }
        }
        Ok(Some(VerifyRun {
            results,
            failure: None,
            red_green,
            red_mutated: None,
        }))
    }

    /// R59: MSBuild on each project of a `tool: xpp` command, in order, stopping at the first
    /// failure; each run gets the time the command has left. One `VERIFY_RESULT.json` entry per
    /// project. `None`: stopped; `Some(Some(feedback))`: a project failed.
    pub(super) fn run_xpp_command(
        &self,
        c: &CommandSpec,
        tc: &xpp::Toolchain,
        at: &VerifyAt,
        results: &mut Vec<Value>,
    ) -> Result<Option<Option<String>>> {
        let out = at.co.build().join("xpp");
        // the response files go to the task's verify folder, which the build cannot write
        let runs = xpp::runs(c, tc, &at.co.dir, &out, &at.co.base).unwrap_or_default();
        std::fs::create_dir_all(&out)?;
        let deadline = std::time::Instant::now() + xpp::command_timeout(c);
        for r in runs {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let res = if left.is_zero() {
                xpp::not_started(&r.name)
            } else {
                r.write_response()?;
                let mut run = at.spec(&r.name, tc.msbuild.clone(), r.command.clone(), left);
                run.env
                    .extend(xpp::ENV.map(|(k, v)| (k.to_string(), v.to_string())));
                match self.run_verify_command(run, at.stop)? {
                    Some(res) => res,
                    None => return Ok(None),
                }
            };
            // R59/ARCH-10: the first real errors, ahead of a tail that may be 3 000 lines later
            let diags = xpp::diagnostics(&res.output, &at.co.dir);
            results.push(xpp::entry(&r, tc, &res, &diags));
            if !res.ok() {
                let errors = xpp::feedback_errors(&diags);
                // M13-08: a timeout names the limit this project was given, not the command's
                let how = if res.timed_out {
                    xpp_timeout_words(left, xpp::command_timeout(c))
                } else {
                    ended(&res)
                };
                return Ok(Some(Some(feedback_text(&res, &errors, &how))));
            }
        }
        Ok(Some(None))
    }

    /// M12-16: the output of the gate `name` goes to a log file in the run store, and a line
    /// says that the gate still runs every minute (`TORYO_GATE_HEARTBEAT_SECS` shortens it for
    /// the tests).
    pub(super) fn gate_io(&self, name: &str) -> GateIo {
        GateIo {
            log: self.task().ok().map(|t| gate_log_path(&t.run_store, name)),
            heartbeat: Some(Duration::from_secs(
                std::env::var("TORYO_GATE_HEARTBEAT_SECS")
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                    .filter(|s| *s > 0)
                    .unwrap_or(60),
            )),
        }
    }

    /// One verify process through `LocalRunner`, with its ✔/✘ line; `None` when a stop
    /// request ended it (R38). R50: LocalRunner strips ANSI (CSI/OSC) from what it captures.
    pub(super) fn run_verify_command(
        &self,
        run: RunSpec,
        stop: &StopWatch,
    ) -> Result<Option<CommandResult>> {
        self.run_gate_command(run, stop, None, None)
    }

    /// [`Self::run_verify_command`] for a gate of `kind` (`environment`, M11-E3: shown in its
    /// `verify_command` event; `None` keeps the M10 event).
    pub(super) fn run_gate_command(
        &self,
        run: RunSpec,
        stop: &StopWatch,
        kind: Option<&str>,
        side: Option<&str>,
    ) -> Result<Option<CommandResult>> {
        let name = run.name.clone();
        let io = self.gate_io(&name);
        let res = self.hub.block_on(LocalRunner.run_with(run, None, io))?;
        if res.cancelled || stop.stopped() {
            return Ok(None);
        }
        self.rec.emit(EngineEvent::VerifyCommand {
            name,
            exit: res.exit,
            timed_out: res.timed_out,
            ok: res.ok(),
            duration_ms: res.duration_ms as u64,
            kind: kind.map(str::to_string),
            side: side.map(str::to_string),
        });
        // (M17-A6: a red-side run must fail — its log is worth a look only when it did not)
        let wrong = if side == Some("red") {
            res.ok()
        } else {
            !res.ok()
        };
        if let (true, Some(log)) = (wrong, &res.log) {
            crate::errln!("    full output: {}", log.display());
        }
        Ok(Some(res))
    }
}

/// M12-16: where the output of the gate `name` run now is written: `<run store>/gate-logs/`.
pub(super) fn gate_log_path(run_store: &Path, name: &str) -> PathBuf {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    run_store.join("gate-logs").join(format!("{at}-{safe}.log"))
}

/// M13-04: the log of a gate run as `VERIFY_RESULT.json` names it — relative to the run store
/// (`gate-logs/<file>`). The file is in the reviewer's and the fix-cycle implementer's pack: an
/// absolute path under `%LOCALAPPDATA%` would send the Windows user name to the vendor. The
/// console line `full output:` keeps the absolute path.
pub fn gate_log_rel(res: &CommandResult) -> Option<String> {
    let name = res.log.as_ref()?.file_name()?.to_string_lossy().to_string();
    Some(format!("gate-logs/{name}"))
}

/// One verify command's entry in VERIFY_RESULT.json.
#[cfg(test)]
pub(super) fn command_result(c: &CommandSpec, res: &CommandResult) -> Value {
    command_result_with(c, res, None)
}

/// [`command_result`] for a `program` command with `diagnostics: msbuild`: the parsed errors
/// and the counts join the entry (the same keys as a `tool: xpp` project's, no `kind`/`scope`:
/// what a program proves is its own business).
pub(super) fn command_result_with(
    c: &CommandSpec,
    res: &CommandResult,
    diags: Option<&xpp::Diagnostics>,
) -> Value {
    let argv: Vec<String> = match c.kind() {
        CommandKind::Program { program, args } => std::iter::once(program.to_string())
            .chain(args.iter().cloned())
            .collect(),
        CommandKind::Xpp { .. } => Vec::new(),
    };
    let mut v = json!({"name": c.name, "argv": argv, "exit": res.exit, "ms": res.duration_ms, "output_tail": res.tail(40)});
    // M11-R17: the signal that ended it (Unix), only when there is one
    if let Some(s) = res.signal {
        v["signal"] = json!(s);
    }
    if let Some(d) = diags {
        v["diagnostics"] = json!(d.errors);
        v["errors_total"] = json!(d.errors_total);
        v["warnings_total"] = json!(d.warnings_total);
    }
    // M12-16: the file with the whole (bounded, redacted) output
    if let Some(l) = gate_log_rel(res) {
        v["log"] = json!(l);
    }
    v
}

/// The feedback of a failed verify for the implementer: the failing command, how it ended,
/// `errors` (the parsed X++ diagnostics, R59; empty for a program) and the tail of its output
/// (the `output_tail` of VERIFY_RESULT.json).
pub(super) fn verify_feedback(res: &CommandResult, errors: &str) -> String {
    verify_feedback_in(res, errors, None)
}

/// [`verify_feedback`] for a command that had `limit` to finish (M12-06): a timeout says how
/// long it was given — `timed out after 30 min`.
pub(super) fn verify_feedback_in(
    res: &CommandResult,
    errors: &str,
    limit: Option<Duration>,
) -> String {
    let how = match limit {
        Some(l) if res.timed_out => format!("timed out after {}", fmt_limit(l)),
        _ => ended(res),
    };
    feedback_text(res, errors, &how)
}

/// The feedback line of a failed verify command that ended as `how` says.
pub(super) fn feedback_text(res: &CommandResult, errors: &str, how: &str) -> String {
    format!(
        "- VERIFY FAILED: `{}` {how}. {errors}Last output:\n```\n{}\n```",
        res.name,
        res.tail(40)
    )
}

/// M13-08: how a project of a `tool: xpp` command ran out of time — the limit **it** was given
/// (`given`: what the projects before it left of the command's `timeout_min`, `total`).
pub(super) fn xpp_timeout_words(given: Duration, total: Duration) -> String {
    let given = Duration::from_secs(given.as_secs_f64().ceil() as u64);
    if given.is_zero() {
        format!(
            "was not started: the projects before it used up the command's {}",
            fmt_limit(total)
        )
    } else if given >= total {
        format!("timed out after {}", fmt_limit(total))
    } else {
        format!(
            "timed out after {} (what the projects before it left of the command's {})",
            fmt_limit(given),
            fmt_limit(total)
        )
    }
}

/// How a verify command ended, in words: `exited 1`, `timed out`, …
pub(super) fn ended(res: &CommandResult) -> String {
    match (res.timed_out, res.cancelled, res.exit) {
        (true, _, _) => "timed out".into(),
        (_, true, _) => "was cancelled".into(),
        // M11-R17: on Unix -1 means a signal ended it — say which
        (_, _, Some(code)) if res.signal.is_some() => format!(
            "exited {code} (killed by signal {})",
            res.signal.unwrap_or_default()
        ),
        (_, _, Some(code)) => format!("exited {code}"),
        (_, _, None) => "ended without an exit code (killed)".into(),
    }
}

/// The program a verify command runs (item 10): a relative path with a separator
/// (`scripts/gate.sh`, `.\build.cmd`) names a file of the checkpoint, so it is anchored at the
/// verify checkout — Windows would otherwise resolve it against toryo's own working
/// directory. Absolute programs and bare names (a PATH lookup) are unchanged.
pub fn verify_program(program: &str, checkout: &Path) -> PathBuf {
    use std::path::Component;
    let p = resolve_program(program);
    let relative_path = (program.contains('/') || program.contains('\\'))
        && p.components().all(|c| {
            matches!(
                c,
                Component::Normal(_) | Component::CurDir | Component::ParentDir
            )
        });
    if !relative_path {
        return p; // absolute, rooted (`\tools\x.exe`), drive-relative, or a bare name
    }
    program
        .split(['/', '\\'])
        .filter(|c| !c.is_empty() && *c != ".")
        .fold(checkout.to_path_buf(), |dir, c| dir.join(c))
}

/// A time limit in words: `30 min`, `90 s`.
pub(super) fn fmt_limit(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 && (s / 60) * 60 == s {
        format!("{} min", s / 60)
    } else {
        format!("{s} s")
    }
}
