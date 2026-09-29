//! `toryo spike …` and `toryo doctor --canaries` — the M0 kit's commands (cargo feature
//! `m0-kit`, on by default). The kit is the behavioural reference for M2–M8 (R52).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Subcommand;

use toryo::agents::claude::LaunchMode;
use toryo::config::Effective;
use toryo::spike::agents::{self as sa, ClaudeArgs, CodexArgs};
use toryo::spike::results::{self, new_run_dir};
use toryo::spike::turn::TurnOpts;
use toryo::spike::{fixture, job, lowil, render};

use crate::{load_config, Cli};

#[derive(Subcommand)]
pub enum SpikeCmd {
    /// One Claude turn with the plan's exact launch line (R30); prompt from a file.
    Claude {
        #[arg(long)]
        profile: String,
        #[arg(long)]
        prompt_file: PathBuf,
        #[arg(long)]
        worktree: Option<PathBuf>,
        #[arg(long)]
        run_store: PathBuf,
        /// JSON schema file (passed inline to --json-schema)
        #[arg(long)]
        schema: Option<PathBuf>,
        #[arg(long)]
        context_file: Option<PathBuf>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, value_enum, default_value = "restricted")]
        mode: LaunchMode,
        #[arg(long)]
        resume: Option<String>,
        /// Drop a flag (and its value) from the launch line — diagnostics only.
        #[arg(long = "drop-flag", allow_hyphen_values = true)]
        drop_flags: Vec<String>,
        #[arg(long = "extra-arg", allow_hyphen_values = true)]
        extra_args: Vec<String>,
        #[arg(long, default_value = "turn")]
        label: String,
        #[arg(long)]
        kill_after_secs: Option<u64>,
        /// Also write the structured output here (e.g. plan.json for the dry-run).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// One Codex turn with the plan's canonical argv (R19); prompt from a file.
    Codex {
        #[arg(long)]
        profile: String,
        #[arg(long)]
        prompt_file: PathBuf,
        #[arg(long)]
        worktree: Option<PathBuf>,
        /// Output schema file (strict dialect), e.g. implementation.schema.json
        #[arg(long)]
        schema: Option<PathBuf>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        resume: Option<String>,
        #[arg(long = "extra-arg", allow_hyphen_values = true)]
        extra_args: Vec<String>,
        #[arg(long, default_value = "turn")]
        label: String,
        #[arg(long)]
        kill_after_secs: Option<u64>,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// claude_launch_isolation (R44b) + M0-VERIFY 1/6, 2, 7 for one launch mode.
    ClaudeIsolation {
        #[arg(long)]
        profile: String,
        #[arg(long, value_enum, default_value = "restricted")]
        mode: LaunchMode,
        #[arg(long)]
        model: Option<String>,
    },
    /// claude_implementer_isolation (R49, V07-VERIFY 4): Claude in the fallback Implementer mode.
    ClaudeImplementerIsolation {
        #[arg(long)]
        profile: String,
        #[arg(long, value_enum, default_value = "restricted")]
        mode: LaunchMode,
        #[arg(long)]
        model: Option<String>,
    },
    /// V07-VERIFY 2 (R46): does the per-task TEMP reach Codex's sandboxed commands?
    CodexTempProbe {
        #[arg(long)]
        profile: String,
        #[arg(long)]
        model: Option<String>,
    },
    /// Codex sandbox canary (M0-VERIFY 3, R18/R27).
    CodexCanary {
        #[arg(long)]
        profile: String,
        #[arg(long)]
        model: Option<String>,
    },
    /// Is a repo-level `.codex/config.toml` loaded under --ignore-user-config? (M0-VERIFY 8)
    CodexProjectConfig {
        #[arg(long)]
        profile: String,
        #[arg(long)]
        model: Option<String>,
    },
    /// Kill a running turn with its Job, resume it, compare usage (M0-VERIFY 4/5 or 12).
    KilledResume {
        #[arg(long)]
        profile: String,
        #[arg(long, default_value_t = 25)]
        kill_after_secs: u64,
        #[arg(long)]
        model: Option<String>,
    },
    /// Write toryo.yaml (repository part) and a machine config with the profiles (R60);
    /// translate Codex `[profiles.<name>]` into -c overrides.
    InitConfig {
        #[arg(long)]
        out: PathBuf,
        /// Where the machine part (profiles) goes; default: toryo.local.yaml next to --out.
        /// An existing file is never overwritten.
        #[arg(long)]
        machine_out: Option<PathBuf>,
        #[arg(long)]
        project_root: String,
        #[arg(long)]
        worktrees_root: String,
        #[arg(long)]
        claude_exe: String,
        #[arg(long)]
        git_bash: Option<String>,
        #[arg(long)]
        codex_exe: String,
        /// Usually %USERPROFILE%\.codex\config.toml
        #[arg(long)]
        codex_config: Option<PathBuf>,
        /// Codex profile names to import, e.g. openai,foundry → toryo profiles codex-openai, codex-foundry
        #[arg(long, value_delimiter = ',')]
        codex_profiles: Vec<String>,
        #[arg(long, default_value = "unelevated")]
        windows_sandbox: String,
        /// CODEX_HOME to pass to Codex (default: the current CODEX_HOME, if set)
        #[arg(long)]
        codex_home: Option<String>,
        /// Fall back / skip when a Codex profile is missing instead of failing
        #[arg(long)]
        lenient: bool,
    },
    /// End-to-end smoke test: Claude plans → Codex implements → toryo checkpoints/verifies → Claude reviews.
    Smoke {
        #[arg(long, default_value = "claude-max")]
        claude_profile: String,
        #[arg(long, default_value = "codex")]
        codex_profile: String,
        #[arg(long)]
        claude_model: Option<String>,
        #[arg(long)]
        codex_model: Option<String>,
        /// Launch modes to try for Claude, in order.
        #[arg(
            long,
            value_enum,
            value_delimiter = ',',
            default_value = "restricted,setting-sources,cwd-run-store"
        )]
        modes: Vec<LaunchMode>,
    },
    /// Reference prototype (R52) — use `toryo run`. Runs one task through the whole workflow:
    /// Claude plans → you approve → Codex implements → checkpoint + verify → review → fix cycles.
    Pipeline {
        /// The Git repository (its current branch is the base unless --base is given)
        #[arg(long)]
        repo: PathBuf,
        /// The task file (Markdown with `## Acceptance criteria` / `- AC-1: …`)
        #[arg(long)]
        task: PathBuf,
        /// Task id (default: the `# XYZ-123` heading of the task, else the file name)
        #[arg(long)]
        id: Option<String>,
        /// Base branch/commit (default: the repo's current branch)
        #[arg(long)]
        base: Option<String>,
        /// Claude profile (default: roles.planner from toryo.yaml)
        #[arg(long)]
        claude_profile: Option<String>,
        /// Implementer profile (default: roles.implementer from toryo.yaml)
        #[arg(long, alias = "codex-profile")]
        implementer: Option<String>,
        /// Fallback implementers when the current one is out of quota, in order
        /// (default: roles.implementer.fallback, else the Claude profile)
        #[arg(long, value_delimiter = ',')]
        implementer_fallback: Vec<String>,
        /// Do not switch implementers; stop when the implementer is out of quota
        #[arg(long)]
        no_fallback: bool,
        #[arg(long)]
        claude_model: Option<String>,
        #[arg(long)]
        codex_model: Option<String>,
        /// Claude launch mode (the one `spike smoke` reported as working)
        #[arg(long, value_enum, default_value = "restricted")]
        mode: LaunchMode,
        #[arg(long, default_value_t = 2)]
        max_fix_cycles: u32,
        /// Skip the human plan gate (demos / CI only)
        #[arg(long)]
        yes: bool,
        /// Try the fallback path: pretend the primary implementer is out of quota
        #[arg(long)]
        simulate_quota: bool,
        /// Run even if a profile's isolation canary did not pass for its current CLI
        /// version (R51) — prefer `toryo doctor --canaries`
        #[arg(long)]
        accept_unverified: bool,
        /// Scripted agents instead of the real CLIs (tests / demos; plan §6.4)
        #[arg(long, hide = true)]
        fake_agents: Option<PathBuf>,
    },
    /// Job Object tests: stop one stage, toryo.exe crash, assign-failure injection (R23).
    JobTest,
    /// Low-integrity verify boundary canary + fixture build at Low IL (M0-VERIFY 9, R36).
    LowilCanary,
    /// Hook / filter / textconv canaries through GitCmd (R33/R37, M0-VERIFY 10).
    GitCanary {
        /// Keep the canary repositories here instead of a run directory.
        #[arg(long)]
        work: Option<PathBuf>,
    },
    /// Materialise a sample app as a Git repo (+ schemas and role templates).
    InitFixture {
        #[arg(long)]
        into: PathBuf,
        /// calculator (M0 smoke fixture) or batch-rename (pipeline walkthrough)
        #[arg(long, default_value = "calculator")]
        app: String,
        /// Also write schemas/ and roles/ here (for the manual dry-run)
        #[arg(long)]
        contracts: Option<PathBuf>,
    },
    /// Render plan.json / review.json / implementation.json as Markdown.
    Render {
        #[arg(long)]
        kind: String,
        input: PathBuf,
    },
    /// Canonical diff for the dry-run: DIFF.patch between two revisions (R37 flags).
    Diff {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        from: String,
        #[arg(long, default_value = "HEAD")]
        to: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Render the recorded M0 evidence as Markdown.
    Report {
        #[arg(long)]
        out: Option<PathBuf>,
    },
    #[command(hide = true)]
    JobChild {
        #[arg(long)]
        depth: u32,
        #[arg(long)]
        sleep: u64,
        #[arg(long)]
        marker: Option<PathBuf>,
        #[arg(long)]
        ready: Option<PathBuf>,
    },
    #[command(hide = true)]
    JobSupervisor {
        #[arg(long)]
        snapshot: PathBuf,
    },
    #[command(hide = true)]
    CanaryChild {
        #[arg(long)]
        targets: PathBuf,
    },
    #[command(hide = true)]
    FilterProcess {
        #[arg(long)]
        marker: PathBuf,
    },
}

/// `doctor --canaries` (R51): run each canary as a child `toryo spike …` (the Job tests
/// must not share this process's Job) and return the ones that failed.
pub fn run_canaries(cli: &Cli, only: &[String], mode: LaunchMode) -> Result<Vec<String>> {
    let cfg = load_config(cli)?;
    let exe = std::env::current_exe()?;
    let mut base: Vec<String> = Vec::new();
    if let Some(c) = &cli.config {
        base.extend(["--config".into(), c.display().to_string()]);
    }
    if let Some(m) = &cli.machine_config {
        base.extend(["--machine-config".into(), m.display().to_string()]);
    }
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let mut steps: Vec<(String, Vec<String>)> = vec![
        ("job-test".into(), s(&["spike", "job-test"])),
        ("git-canary".into(), s(&["spike", "git-canary"])),
    ];
    if cfg!(windows) {
        steps.push(("lowil-canary".into(), s(&["spike", "lowil-canary"])));
    }
    let mode_arg = clap::ValueEnum::to_possible_value(&mode)
        .map(|v| v.get_name().to_string())
        .unwrap_or_else(|| "restricted".into());
    for (name, p) in &cfg.repo.profiles {
        if !only.is_empty() && !only.contains(name) {
            continue;
        }
        let args = match p.kind {
            toryo::config::ProfileKind::Claude => s(&[
                "spike",
                "claude-isolation",
                "--profile",
                name,
                "--mode",
                &mode_arg,
            ]),
            toryo::config::ProfileKind::Codex => s(&["spike", "codex-canary", "--profile", name]),
        };
        steps.push((format!("{} {name}", args[1]), args));
        // R49: a Claude profile in the implementer chain also runs the implementer mode
        let in_chain = toryo::engine::task::roles(&cfg)
            .map(|(_, chain, _)| chain.contains(name))
            .unwrap_or(false);
        if p.kind == toryo::config::ProfileKind::Claude && in_chain {
            let a2 = s(&[
                "spike",
                "claude-implementer-isolation",
                "--profile",
                name,
                "--mode",
                &mode_arg,
            ]);
            steps.push((format!("claude-implementer-isolation {name}"), a2));
        }
    }
    for o in only {
        if !cfg.repo.profiles.contains_key(o) {
            bail!("unknown profile `{o}`");
        }
    }
    let mut failed = Vec::new();
    for (label, args) in steps {
        eprintln!("\n════ canary: {label} ════");
        let st = std::process::Command::new(&exe)
            .args(&base)
            .args(&args)
            .status()
            .with_context(|| format!("running {label}"))?;
        eprintln!(
            "════ {label}: {} ════",
            if st.success() { "PASS" } else { "FAIL" }
        );
        if !st.success() {
            failed.push(label);
        }
    }
    eprintln!();
    Ok(failed)
}

fn opts(kill_after_secs: Option<u64>) -> TurnOpts {
    TurnOpts {
        kill_after: kill_after_secs.map(Duration::from_secs),
        ..Default::default()
    }
}

fn write_out(out: &Option<PathBuf>, r: &toryo::spike::turn::TurnRun) -> Result<()> {
    if let Some(p) = out {
        match &r.outcome.structured {
            Some(v) => {
                std::fs::write(p, serde_json::to_vec_pretty(v)?)?;
                eprintln!("structured output written to {}", p.display());
            }
            None => eprintln!("no structured output to write to {}", p.display()),
        }
    }
    Ok(())
}

pub fn spike(cli: &Cli, cmd: &SpikeCmd) -> Result<i32> {
    let ok = |b: bool| Ok(if b { 0 } else { 1 });
    match cmd {
        SpikeCmd::Claude {
            profile,
            prompt_file,
            worktree,
            run_store,
            schema,
            context_file,
            model,
            mode,
            resume,
            drop_flags,
            extra_args,
            label,
            kill_after_secs,
            out,
        } => {
            let cfg = load_config(cli)?;
            let prompt = std::fs::read_to_string(prompt_file)
                .with_context(|| format!("reading {}", prompt_file.display()))?;
            let schema = schema.as_ref().map(std::fs::read_to_string).transpose()?;
            let run_dir = new_run_dir("claude", label)?;
            let r = sa::claude_turn(
                &cfg,
                ClaudeArgs {
                    profile: profile.clone(),
                    worktree: sa::default_worktree_for_turn(worktree.clone())?,
                    run_store: run_store.clone(),
                    prompt,
                    schema,
                    context_file: context_file.clone(),
                    model: model.clone(),
                    mode: *mode,
                    resume: resume.clone(),
                    drop_flags: drop_flags.clone(),
                    extra_args: extra_args.clone(),
                    label: label.clone(),
                    opts: opts(*kill_after_secs),
                },
                &run_dir,
            )?;
            write_out(out, &r)?;
            sa::record_single("claude", profile, &r)?;
            ok(r.outcome.success)
        }
        SpikeCmd::Codex {
            profile,
            prompt_file,
            worktree,
            schema,
            model,
            resume,
            extra_args,
            label,
            kill_after_secs,
            out,
        } => {
            let cfg = load_config(cli)?;
            let prompt = std::fs::read_to_string(prompt_file)
                .with_context(|| format!("reading {}", prompt_file.display()))?;
            let run_dir = new_run_dir("codex", label)?;
            let schema = schema.as_ref().map(std::path::absolute).transpose()?;
            let r = sa::codex_turn(
                &cfg,
                CodexArgs {
                    profile: profile.clone(),
                    worktree: sa::default_worktree_for_turn(worktree.clone())?,
                    prompt,
                    schema_path: schema,
                    model: model.clone(),
                    resume: resume.clone(),
                    extra_args: extra_args.clone(),
                    label: label.clone(),
                    opts: opts(*kill_after_secs),
                },
                &run_dir,
            )?;
            write_out(out, &r)?;
            sa::record_single("codex", profile, &r)?;
            ok(r.outcome.success)
        }
        SpikeCmd::ClaudeIsolation {
            profile,
            mode,
            model,
        } => ok(sa::claude_isolation(
            &load_config(cli)?,
            profile,
            *mode,
            model.clone(),
        )?),
        SpikeCmd::ClaudeImplementerIsolation {
            profile,
            mode,
            model,
        } => ok(sa::claude_implementer_isolation(
            &load_config(cli)?,
            profile,
            *mode,
            model.clone(),
        )?),
        SpikeCmd::CodexTempProbe { profile, model } => ok(sa::codex_temp_probe(
            &load_config(cli)?,
            profile,
            model.clone(),
        )?),
        SpikeCmd::CodexCanary { profile, model } => ok(sa::codex_canary(
            &load_config(cli)?,
            profile,
            model.clone(),
        )?),
        SpikeCmd::CodexProjectConfig { profile, model } => {
            sa::codex_project_config(&load_config(cli)?, profile, model.clone())?;
            Ok(0)
        }
        SpikeCmd::KilledResume {
            profile,
            kill_after_secs,
            model,
        } => {
            sa::killed_resume(&load_config(cli)?, profile, *kill_after_secs, model.clone())?;
            Ok(0)
        }
        SpikeCmd::InitConfig {
            out,
            machine_out,
            project_root,
            worktrees_root,
            claude_exe,
            git_bash,
            codex_exe,
            codex_config,
            codex_profiles,
            windows_sandbox,
            codex_home,
            lenient,
        } => {
            toryo::spike::init_config::init_config(&toryo::spike::init_config::InitArgs {
                out: out.clone(),
                machine_out: machine_out.clone(),
                project_root: project_root.clone(),
                worktrees_root: worktrees_root.clone(),
                claude_exe: claude_exe.clone(),
                git_bash: git_bash.clone(),
                codex_exe: codex_exe.clone(),
                codex_config: codex_config.clone(),
                codex_profiles: codex_profiles.clone(),
                windows_sandbox: windows_sandbox.clone(),
                codex_home: codex_home
                    .clone()
                    .or_else(|| std::env::var("CODEX_HOME").ok()),
                lenient: *lenient,
            })?;
            Ok(0)
        }
        SpikeCmd::Smoke {
            claude_profile,
            codex_profile,
            claude_model,
            codex_model,
            modes,
        } => {
            let cfg = load_config(cli)?;
            toryo::spike::smoke::preflight(&cfg, &[claude_profile, codex_profile])?;
            ok(toryo::spike::smoke::smoke(
                &cfg,
                toryo::spike::smoke::SmokeArgs {
                    claude_profile: claude_profile.clone(),
                    codex_profile: codex_profile.clone(),
                    claude_model: claude_model.clone(),
                    codex_model: codex_model.clone(),
                    modes: modes.clone(),
                },
            )?)
        }
        SpikeCmd::Pipeline {
            repo,
            task,
            id,
            base,
            claude_profile,
            implementer,
            implementer_fallback,
            no_fallback,
            claude_model,
            codex_model,
            mode,
            max_fix_cycles,
            yes,
            simulate_quota,
            accept_unverified,
            fake_agents,
        } => {
            eprintln!(
                "WARNING: `toryo spike pipeline` is the M0 reference prototype (R52) and gets no new features \
                 (no project context, no protected snapshot, no Low-IL verify, no budget). Use `toryo task create` + `toryo run`."
            );
            let cfg = load_config(cli)?;
            let fake = fake_agents.is_some();
            let preflight = |cfg: &Effective, p: &[&str]| -> Result<()> {
                if fake {
                    // profiles must still exist; executables are not needed
                    for x in p {
                        cfg.profile(x)?;
                    }
                    Ok(())
                } else {
                    toryo::spike::smoke::preflight(cfg, p)
                }
            };
            let role = |r: &str, dflt: &str| {
                cfg.repo
                    .roles
                    .get(r)
                    .map(|x| x.profile.clone())
                    .unwrap_or_else(|| dflt.to_string())
            };
            let cp = claude_profile
                .clone()
                .unwrap_or_else(|| role("planner", "claude-max"));
            let xp = implementer
                .clone()
                .unwrap_or_else(|| role("implementer", "codex"));
            // owner decision 15: the reviewer is its own role, never changed by a fallback
            let rp = claude_profile
                .clone()
                .unwrap_or_else(|| role("reviewer", &cp));
            let model = |r: &str| cfg.repo.roles.get(r).and_then(|x| x.model.clone());
            preflight(&cfg, &[&cp, &xp, &rp])?;
            // R51: isolation evidence must match the CLI versions installed now
            let unverified = |p: &[&str]| -> Result<Vec<(String, String)>> {
                if fake {
                    Ok(vec![])
                } else {
                    toryo::evidence::unverified_profiles(&cfg, p)
                }
            };
            let bad = unverified(&[&cp, &xp, &rp])?;
            if !bad.is_empty() {
                let list = bad
                    .iter()
                    .map(|(p, why)| format!("  - {p}: {why}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                if *accept_unverified {
                    eprintln!(
                        "WARNING: running with unverified profiles (--accept-unverified):\n{list}"
                    );
                } else {
                    bail!("unverified profiles (R51):\n{list}\nrun `toryo doctor --canaries`, or pass --accept-unverified");
                }
            }
            let mut chain = vec![xp.clone()];
            if !*no_fallback {
                let configured = cfg
                    .repo
                    .roles
                    .get("implementer")
                    .and_then(|r| r.fallback.clone())
                    .unwrap_or_default();
                let fb = if !implementer_fallback.is_empty() {
                    implementer_fallback.clone()
                } else if !configured.is_empty() {
                    configured
                } else {
                    vec![cp.clone()]
                };
                for f in fb {
                    if chain.contains(&f) {
                        continue;
                    }
                    match preflight(&cfg, &[&f]).and_then(|()| match unverified(&[&f])?.pop() {
                        Some((_, why)) if !*accept_unverified => bail!("{why}"),
                        _ => Ok(()),
                    }) {
                        Ok(()) => chain.push(f),
                        Err(e) => eprintln!("WARNING: fallback implementer `{f}` skipped: {e:#}"),
                    }
                }
            }
            let cli_versions = if fake {
                Default::default()
            } else {
                let mut ps: Vec<&str> = vec![&cp, &rp];
                ps.extend(chain.iter().map(String::as_str));
                toryo::evidence::collect_versions(Some(&cfg), &ps)
            };
            let hub = toryo::agents::hub::AgentHub::new(&cfg, *mode, fake_agents.as_deref())?;
            ok(toryo::spike::pipeline::pipeline(
                &cfg,
                &hub,
                toryo::spike::pipeline::PipelineArgs {
                    repo: repo.clone(),
                    task: task.clone(),
                    id: id.clone(),
                    base: base.clone(),
                    claude_model: claude_model.clone().or_else(|| model("planner")),
                    codex_model: codex_model.clone().or_else(|| model("implementer")),
                    claude_profile: cp,
                    cli_versions,
                    reviewer_profile: rp,
                    implementers: chain,
                    simulate_quota: *simulate_quota,
                    max_fix_cycles: *max_fix_cycles,
                    yes: *yes,
                },
            )?)
        }
        SpikeCmd::JobTest => ok(job::job_test()?),
        SpikeCmd::LowilCanary => {
            // verify.verify_root from the config when there is one, else the default
            let vr = load_config(cli)
                .ok()
                .and_then(|c| toryo::config::expand_vars(&c.repo.verify.verify_root).ok())
                .map(PathBuf::from);
            ok(lowil::lowil_canary(vr)?)
        }
        SpikeCmd::GitCanary { work } => {
            let dir = match work {
                Some(w) => w.clone(),
                None => new_run_dir("git-canary", "all")?,
            };
            let exe = std::env::current_exe()?
                .display()
                .to_string()
                .replace('\\', "/");
            let trusted = format!("'{exe}' spike filter-process --marker");
            let reports = toryo::git::canary::run_all(&fixture::git_exe(), &dir, Some(&trusted))?;
            let pass = reports.iter().all(|r| r.pass);
            for r in &reports {
                eprintln!(
                    "[{}] {:?}: markers={:?} unexpected={:?} binary_patch={:?} {}",
                    if r.pass { "PASS" } else { "FAIL" },
                    r.variant,
                    r.markers,
                    r.unexpected_markers,
                    r.diff_has_binary_patch,
                    r.note
                );
                if let Some(e) = &r.admission_error {
                    eprintln!("       admission: {e}");
                }
            }
            let mut rec = results::Record::new("git-canary", None, &["M0-VERIFY 10", "R33", "R37"]);
            rec.pass = Some(pass);
            rec.bind_versions(None, &[]); // R51
            rec.summary = format!(
                "{}: {}",
                reports
                    .first()
                    .map(|r| r.git_version.clone())
                    .unwrap_or_default(),
                reports
                    .iter()
                    .map(|r| format!("{:?}={}", r.variant, if r.pass { "pass" } else { "FAIL" }))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            rec.details = serde_json::to_value(&reports)?;
            rec.run_dir = Some(dir.display().to_string());
            results::append(&rec)?;
            ok(pass)
        }
        SpikeCmd::InitFixture {
            into,
            app,
            contracts,
        } => {
            let head = match app.as_str() {
                "calculator" => fixture::init_repo(into)?,
                "batch-rename" => fixture::init_repo_with(
                    into,
                    fixture::BATCH_RENAME,
                    "BatchRename Sample",
                    "BatchRename: initial module",
                )?,
                other => anyhow::bail!("unknown --app `{other}` (calculator | batch-rename)"),
            };
            println!("{app} sample at {} (main = {head})", into.display());
            if let Some(c) = contracts {
                fixture::write_contracts(c)?;
                println!("schemas/ and roles/ written to {}", c.display());
            }
            Ok(0)
        }
        SpikeCmd::Render { kind, input } => {
            let v: serde_json::Value = serde_json::from_slice(&std::fs::read(input)?)?;
            print!("{}", render::render(kind, &v)?);
            Ok(0)
        }
        SpikeCmd::Diff {
            repo,
            from,
            to,
            out,
        } => {
            let g = fixture::gitcmd()?;
            let d = g.canonical_diff(&toryo::git::Loc::Discover(repo.clone()), from, Some(to))?;
            std::fs::write(out, &d)?;
            eprintln!("{} bytes → {}", d.len(), out.display());
            Ok(0)
        }
        SpikeCmd::Report { out } => {
            let recs = results::load();
            let md = results::render_markdown(&recs);
            let p = out
                .clone()
                .unwrap_or_else(|| results::m0_dir().join("M0-RESULTS.generated.md"));
            std::fs::write(&p, &md)?;
            print!("{md}");
            eprintln!("written to {}", p.display());
            Ok(0)
        }
        SpikeCmd::JobChild {
            depth,
            sleep,
            marker,
            ready,
        } => {
            job::job_child(*depth, *sleep, marker.clone(), ready.clone())?;
            Ok(0)
        }
        SpikeCmd::JobSupervisor { snapshot } => {
            job::job_supervisor(snapshot)?;
            Ok(0)
        }
        SpikeCmd::CanaryChild { targets } => {
            lowil::canary_child(targets)?;
            Ok(0)
        }
        SpikeCmd::FilterProcess { marker } => {
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            toryo::git::filter_process::serve(&mut stdin.lock(), &mut stdout.lock(), marker)?;
            Ok(0)
        }
    }
}
