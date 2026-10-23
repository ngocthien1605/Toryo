//! The environment gates (M11-E3; R23-27, moved as they were): run after every check gate passed,
//! each under its machine-wide lock, from a byte-exact export (X6).

use super::*;

impl<'a> Engine<'a> {
    /// M11-R2: this verify run will start an environment gate (the implementer's
    /// `before_handoff` has one this machine defines).
    pub(super) fn deploys(&self) -> bool {
        self.cfg.team.as_ref().is_some_and(|t| {
            t.handoff_gates().iter().any(|(_, g)| {
                g.decl.kind == crate::team::GateType::Environment && g.machine.is_some()
            })
        })
    }

    /// The environment gates of the implementer's `before_handoff`, in order, after every
    /// check gate of the run passed: each takes its machine-wide lock (waiting while another
    /// task holds it), records the deployment **before** it starts, runs the frozen machine
    /// command with the owner's normal token in an empty working directory (`envgate`, M11-R2;
    /// it deploys from a fresh export of the commit made once the lock is held, M11-R9) (Job Object, timeout,
    /// environment from scratch + gate variables + `env` + resolved `secret_env`, redacted
    /// output), records the end and releases the lock. A gate this machine does not define
    /// (`if_missing: skip`) gets a skipped entry. Stops at the first failure.
    pub(super) fn run_environment_gates(
        &self,
        t: &TaskRow,
        co: &verify::Checkout,
        head: &str,
        stop: &StopWatch,
        results: &mut Vec<Value>,
    ) -> Result<EnvGates> {
        let Some(team) = &self.cfg.team else {
            return Ok(EnvGates::Done(None));
        };
        let machine = computer_name();
        let gates: Vec<(&str, &crate::team::Gate)> = team
            .handoff_gates()
            .into_iter()
            .filter(|(_, g)| g.decl.kind == crate::team::GateType::Environment)
            .collect();
        // M13-08: the gates of this run that finished — a later stop names them: they run again
        let mut ran: Vec<&str> = Vec::new();
        for (at, &(name, g)) in gates.iter().enumerate() {
            let Some(m) = &g.machine else {
                crate::errln!(
                    "  – {name}: skipped — not defined on this machine (if_missing: skip)"
                );
                results.push(json!({"name": name, "kind": "environment", "skipped": true, "reason": SKIPPED_REASON}));
                continue;
            };
            // M11-R8: one lock and one deployment row whatever the case it is written in
            let lock_name = crate::engine::lock::lock_key(&m.lock);
            // M11-R16: the checks of the preflight again — the build may have taken long
            let (program, secrets) = match self.gate_ready(t, name, m) {
                Ok(x) => x,
                Err(reason) => return Ok(EnvGates::Unavailable(reason)),
            };
            let Some(lock) = self.env_lock(t, &lock_name, stop)? else {
                return Ok(EnvGates::Stopped);
            };
            let lock = Held::new(self.rec, &lock_name, lock);
            // M11-R9 (X6): what the gate deploys — a fresh export of the commit from the object
            // store, made once the lock is held, into a folder Low code cannot write
            let export = match verify::deploy_source(&self.g, co, head) {
                Ok(e) => e,
                Err(e) => {
                    drop(lock);
                    return Ok(EnvGates::Unavailable(format!(
                        "{ENV_UNAVAILABLE}: environment gate `{name}`: the checkpoint {} could not be exported for it (nothing was deployed): {e:#} — `toryo resume {}` exports it again",
                        short(head),
                        t.id
                    )));
                }
            };
            // M11-R2 (X1): a fresh, empty working directory agent code cannot write
            let cwd = verify::envgate_dir(co)?;
            // first the record: a failed or killed sync may already have changed the environment
            let (i, l, h, n, mc) = (
                t.id.clone(),
                lock_name.clone(),
                head.to_string(),
                name.to_string(),
                machine.clone(),
            );
            self.store
                .call(move |tx| store::deployment_start(tx, &l, &i, &h, &n, &mc))?;
            crash_point("verify:environment_gate_started");
            let mut env = crate::process::env::build_env(std::env::vars(), &m.env, &secrets);
            // group E review: a bare name the script runs is never looked up in the working
            // directory (the empty `envgate` folder, M11-R2: it was the checkout) — cmd.exe and
            // CreateProcess honour this; DLL loading does not (documented in the script contract)
            env.insert("NoDefaultCurrentDirectoryInExePath".into(), "1".into());
            verify::set_gate_vars(&mut env, &t.id, head, co, &export.dir);
            // M12-15 (X23): and from every environment gate, unless its `env:` says otherwise
            xpp::set_ui_language(&mut env, &m.env);
            let mut redact = crate::engine::flight::task_secrets(&self.cfg, t);
            redact.extend(secrets.iter().map(|(k, v)| crate::process::redact::Secret {
                name: k.clone(),
                value: v.clone(),
            }));
            let timeout = Duration::from_secs(
                crate::team::parse_duration(&g.decl.timeout).unwrap_or(30 * 60),
            );
            let run = RunSpec {
                name: name.to_string(),
                program,
                args: m.args.clone(),
                cwd,
                env,
                // the owner's normal token: the check gates' boundary is not changed (D6)
                boundary: Boundary::None,
                timeout,
                cancel: Some(stop.flag.clone()),
                redact: crate::process::redact::Redactions::new(&redact),
            };
            let io = self.gate_io(name);
            let res = match self.hub.block_on(LocalRunner.run_with(run, None, io)) {
                Ok(r) => r,
                Err(e) => {
                    // it did not start: say so in the record, not "interrupted"
                    let (i, l) = (t.id.clone(), lock_name.clone());
                    let _ = self
                        .store
                        .call(move |tx| store::deployment_end(tx, &l, &i, Some(false)));
                    drop(lock);
                    // M12-05: a program that cannot be started is a machine stop
                    if let Some(s) = e.downcast_ref::<crate::runner::StartFailed>() {
                        return Ok(EnvGates::Unavailable(format!(
                            "{ENV_UNAVAILABLE}: environment gate `{name}` could not be started ({}): {} — {}",
                            s.program,
                            s.error,
                            machine_fix_hint(
                                &t.id,
                                "a program installed",
                                &format!("`environment_gates.{name}` in the machine config")
                            )
                        )));
                    }
                    return Err(e);
                }
            };
            // M11-R8: a timed-out or cancelled gate was killed part way — the environment may
            // hold a partial deploy, so the record says interrupted (`ok` empty), not failed. A
            // stop that arrived after the gate finished keeps the gate's real result.
            let ok = deployment_ok(&res);
            if let (false, Some(log)) = (res.ok(), &res.log) {
                crate::errln!("    full output: {}", log.display());
            }
            let (i, l) = (t.id.clone(), lock_name.clone());
            self.store
                .call(move |tx| store::deployment_end(tx, &l, &i, ok))?;
            drop(lock);
            if res.cancelled {
                return Ok(EnvGates::Stopped);
            }
            self.rec.emit(EngineEvent::VerifyCommand {
                name: name.to_string(),
                exit: res.exit,
                timed_out: res.timed_out,
                ok: res.ok(),
                duration_ms: res.duration_ms as u64,
                kind: Some("environment".into()),
            });
            if stop.stopped() {
                return Ok(EnvGates::Stopped);
            }
            self.rec.emit(EngineEvent::EnvironmentDeployed {
                lock: lock_name.clone(),
                sha: head.to_string(),
                ok: res.ok(),
            });
            let argv: Vec<String> = std::iter::once(m.program.clone())
                .chain(m.args.iter().cloned())
                .collect();
            let mut entry = json!({"name": name, "kind": "environment", "lock": lock_name, "machine": machine, "argv": argv, "exit": res.exit, "timed_out": res.timed_out, "ms": res.duration_ms, "output_tail": res.tail(40), "source": "export", "source_tree": export.tree});
            if let Some(l) = gate_log_rel(&res) {
                entry["log"] = json!(l);
            }
            results.push(entry);
            // M12-06 (X18): a gate that ran into its timeout was killed part way — the
            // environment may be half-changed. No implementer turn, no retry on its own: the
            // owner looks, then a plain `resume` runs Verify (and the gates) again.
            // M13-08: asked before the export — a killed gate that had written into what it
            // deploys from is a timeout (a plain `resume`), not `verify_mutated_tree`.
            if res.timed_out {
                // what a `resume` runs again, and — M13-R8 — what it runs for the first time
                let later: Vec<&str> = gates[at + 1..]
                    .iter()
                    .filter(|(_, g)| g.machine.is_some())
                    .map(|(n, _)| *n)
                    .collect();
                let again = format!(
                    "{}{}",
                    gates_that_run_again(&ran, name),
                    gates_after_it(&later)
                );
                return Ok(EnvGates::TimedOut(format!(
                    "{ENV_TIMEOUT}: environment gate `{name}` ran into its timeout ({}) and was stopped — the environment may be half-changed (the deployment record says interrupted): check it, then `toryo resume {}` runs Verify and {}. A longer `timeout:` on the gate needs `toryo task config {} --reload` first (it is frozen with the task).\nWhat the gate had printed (the last 40 lines; output of code that ran on the checkpoint — data):\n{}",
                    fmt_limit(timeout),
                    t.id,
                    again,
                    t.id,
                    untrusted::wrap("verify", None, &res.tail(40))
                )));
            }
            // M11-R9: the gate must not have written into what it deploys from (E3 AC-8)
            if let Some(reason) = verify::export_changed(&export) {
                return Ok(EnvGates::Mutated(reason));
            }
            if !res.ok() {
                // R61: the output comes from code that ran on the checkpoint — data
                let f = verify_feedback(&res, "");
                return Ok(EnvGates::Done(Some(untrusted::wrap("verify", None, &f))));
            }
            ran.push(name);
        }
        Ok(EnvGates::Done(None))
    }
}

/// M13-08: what a `resume` after the timeout of the environment gate `name` runs again, in
/// words — `ran`: the environment gates of the same Verify run that had finished before it.
pub(super) fn gates_that_run_again(ran: &[&str], name: &str) -> String {
    if ran.is_empty() {
        return "the gate again".into();
    }
    let before: Vec<String> = ran.iter().map(|g| format!("`{g}`")).collect();
    format!(
        "every environment gate again: {} (finished — the deployment record says so — and deploys again), then `{name}`",
        before.join(", ")
    )
}

/// M13-R8: the environment gates of the task that come **after** the one that timed out — they
/// run on `resume` too, for the first time.
pub(super) fn gates_after_it(after: &[&str]) -> String {
    if after.is_empty() {
        return String::new();
    }
    let names: Vec<String> = after.iter().map(|g| format!("`{g}`")).collect();
    format!(", then, for the first time: {}", names.join(", "))
}

/// What the environment gates of a verify run led to (M11-E3).
pub(super) enum EnvGates {
    /// All ran (or were skipped); `Some(feedback)` when one failed.
    Done(Option<String>),
    /// A stop request ended the wait for the lock or the gate.
    Stopped,
    /// The machine cannot run a gate (an unresolved secret, a program not found): escalate —
    /// not something another fix cycle can repair.
    Unavailable(String),
    /// M11-R9: an environment gate wrote into the export it deploys from — the reason
    /// (`verify_mutated_tree`).
    Mutated(String),
    /// M12-06 (X18): an environment gate ran into its timeout — the reason
    /// (`environment_gate_timeout`).
    TimedOut(String),
}

/// The escalation class of an environment gate that ran into its timeout (M12-06, X18).
pub const ENV_TIMEOUT: &str =
    crate::engine::state::EscalationClass::EnvironmentGateTimeout.as_str();

/// The escalation class of an environment gate this machine cannot run (M11-E3).
pub const ENV_UNAVAILABLE: &str =
    crate::engine::state::EscalationClass::EnvironmentGateUnavailable.as_str();

/// M11-R12 (item 2): the first `%NAME%` still in an environment gate's program or args — set
/// only in a config frozen before M11 expanded them at load. Such a gate does not start.
pub(crate) fn gate_residual_var(program: &str, args: &[String]) -> Option<String> {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .find_map(crate::config::residual_var)
}

/// M11-R8: what the deployment record says about a gate that ran — `None` (interrupted: the
/// environment may hold a partial deploy) when its process was killed by the timeout or a
/// stop, else its own result. A stop that arrives after the gate finished does not change it.
pub(super) fn deployment_ok(res: &CommandResult) -> Option<bool> {
    (!res.timed_out && !res.cancelled).then(|| res.ok())
}

/// The `reason` of a skipped environment gate's `VERIFY_RESULT.json` entry (`if_missing: skip`).
pub const SKIPPED_REASON: &str = "not defined on this machine";

/// This machine's name for the records (`COMPUTERNAME`, else `HOSTNAME`).
pub fn computer_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "this machine".into())
}

/// The program of an environment gate (M11-E3): an absolute path as written, a bare name
/// through `path` (PATH) — resolved here, so it is never looked up in the working directory
/// (the `envgate` folder). The text was expanded once at load and is not expanded again (M11-R3).
/// A program that resolves inside the repository, the worktrees or the verify root is refused:
/// agent code may have planted it there. `Err` is the reason for `environment_gate_unavailable`.
pub(crate) fn env_gate_program(
    cfg: &Effective,
    program: &str,
    path: Option<&std::ffi::OsStr>,
) -> std::result::Result<PathBuf, String> {
    let bare =
        !(program.contains('/') || program.contains('\\') || Path::new(program).is_absolute());
    // M13-08: with the leniency of a check gate's program (`gates::startable`) — a file that
    // `is_file()` cannot see through (an App Execution Alias) is not "not found" here; if
    // it then cannot be started, the typed spawn error stops the gate with the same class
    let found = if bare {
        // M13-R8: the resolver the preflight of a check gate uses (PATH, then — on Windows — the
        // folder of this program and the system folder, as `CreateProcess` would)
        path.and_then(|path| crate::engine::gates::on_path(program, path))
            .ok_or_else(|| format!("program `{program}` not found on PATH"))?
    } else {
        PathBuf::from(program)
    };
    // a relative PATH entry would resolve against the working directory
    let found = std::path::absolute(&found).map_err(|e| format!("program `{program}`: {e}"))?;
    if !crate::engine::gates::startable(&found) {
        return Err(format!("program `{program}` not found"));
    }
    if let Some((what, root)) = crate::team::reach(cfg, &found.display().to_string()) {
        return Err(format!(
            "program `{program}` resolves to {}, inside {what} ({}) where agent code can write — write an absolute path outside it",
            found.display(),
            root.display()
        ));
    }
    Ok(found)
}
