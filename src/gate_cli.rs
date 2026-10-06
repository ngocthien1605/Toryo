//! `toryo gate list|run` (M14): the check gates of the configuration — `commands.verify`, or
//! the `gates:` of the org chart — **outside a task**: no worktree, lock or database. `list`
//! shows what Verify would run; `run NAME` runs one gate in the repository folder with your own
//! rights (`--print-only` shows the command and the `TORYO_*` variables and runs nothing), so a
//! build or test script can be proven before a task depends on it. Environment gates change
//! your environment and are never run here.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};

use toryo::config::{expand_vars, CommandKind, CommandSpec, Effective};
use toryo::engine::checks::verify_commands;
use toryo::engine::runner::verify_program;
use toryo::engine::verify::{self, Checkout};
use toryo::engine::xpp;
use toryo::process::Boundary;
use toryo::runner::{CommandRunner, LocalRunner, RunSpec};
use toryo::team::GateType;

use crate::xpp_cli::{self, absolute, BuildOpts};
use crate::{load_config, Cli};

/// `TORYO_SHA` of a gate run outside a task: the files are the working tree, not a commit.
const WORKING_TREE: &str = "working-tree";

#[derive(Subcommand)]
pub enum GateCmd {
    /// List the gates of the configuration: what Verify runs, and the environment gates it
    /// does not run here.
    List,
    /// Run one check gate outside a task, in the repository folder with your own rights.
    Run(RunArgs),
}

#[derive(Args)]
pub struct RunArgs {
    /// The gate: a `commands.verify` name, or a check gate of the org chart (`toryo gate list`)
    name: String,
    /// The folder to run it in (default: `project.root` of the configuration)
    #[arg(long)]
    repo: Option<PathBuf>,
    /// Print the command and the TORYO_* variables (PowerShell) and run nothing
    #[arg(long)]
    print_only: bool,
    /// Windows: run at Low integrity (R36), as Verify does. Only for `tool: xpp` gates for now:
    /// a program gate at Low integrity needs a clean checkout, which only a task makes
    #[arg(long)]
    low: bool,
    /// Time for the gate, in minutes (default: the gate's own `timeout`)
    #[arg(long)]
    timeout_min: Option<u64>,
}

pub fn run(cli: &Cli, cmd: &GateCmd) -> Result<i32> {
    match cmd {
        GateCmd::List => list(cli),
        GateCmd::Run(a) => run_gate(cli, a),
    }
}

/// The folder the gates run in by default: `project.root`.
fn project_root(cfg: &Effective) -> Result<PathBuf> {
    Ok(PathBuf::from(
        expand_vars(&cfg.repo.project.root).context("project.root")?,
    ))
}

/// A command as one line: `pwsh -NoProfile -File scripts/build.ps1`, or the X++ tool's inputs.
fn describe(c: &CommandSpec) -> String {
    match c.kind() {
        CommandKind::Program { program, args } => std::iter::once(program.to_string())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" "),
        CommandKind::Xpp { metadata, projects } => {
            format!(
                "tool: xpp  metadata: {metadata}  projects: {}",
                projects.join(", ")
            )
        }
    }
}

fn limit_words(c: &CommandSpec) -> String {
    let s = xpp::command_timeout(c).as_secs();
    if s.is_multiple_of(60) {
        format!("{} min", s / 60)
    } else {
        format!("{s} s")
    }
}

fn list(cli: &Cli) -> Result<i32> {
    let cfg = load_config(cli)?;
    let root = project_root(&cfg)?;
    match verify_commands(&cfg, &root) {
        Ok(cmds) if !cmds.is_empty() => {
            println!("check gates (run by Verify, in this order):");
            for c in &cmds {
                let reads = if c.reads_msbuild() {
                    ", diagnostics: msbuild"
                } else {
                    ""
                };
                println!(
                    "  {}  [{}{reads}]  {}",
                    c.name,
                    limit_words(c),
                    describe(c)
                );
            }
        }
        _ => println!(
            "no check gate configured — Verify has nothing to run (skipped): add `commands.verify` to toryo.yaml, or `gates:` to the org chart"
        ),
    }
    if let Some(team) = &cfg.team {
        for (name, g) in &team.gates {
            if g.decl.kind == GateType::Environment {
                println!(
                    "  {name}  (environment gate — runs with your full rights; not run by this command)"
                );
            }
        }
    }
    Ok(0)
}

fn run_gate(cli: &Cli, a: &RunArgs) -> Result<i32> {
    let cfg = load_config(cli)?;
    let repo = match &a.repo {
        Some(r) => absolute(r)?,
        None => absolute(&project_root(&cfg)?)?,
    };
    let cmds = verify_commands(&cfg, &repo).unwrap_or_default();
    let Some(c) = cmds.iter().find(|c| c.name == a.name) else {
        let env_gate = cfg.team.as_ref().is_some_and(|t| {
            t.gates
                .get(&a.name)
                .is_some_and(|g| g.decl.kind == GateType::Environment)
        });
        if env_gate {
            bail!(
                "`{}` is an environment gate: it runs with your full rights, so only a task runs it (after the check gates, under its lock and `confirm`) — not this command",
                a.name
            );
        }
        let names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
        if names.is_empty() {
            bail!(
                "no gate `{}`: no check gate is configured (`toryo gate list`)",
                a.name
            );
        }
        bail!(
            "no gate `{}` — the check gates are: {}",
            a.name,
            names.join(", ")
        );
    };
    let timeout = match a.timeout_min {
        Some(m) => Duration::from_secs(60 * m),
        None => xpp::command_timeout(c),
    };
    match c.kind() {
        CommandKind::Xpp { metadata, projects } => xpp_cli::build(
            cli,
            &BuildOpts {
                name: c.name.clone(),
                repo,
                projects: projects.to_vec(),
                metadata: metadata.to_string(),
                out: None,
                print_only: a.print_only,
                low: a.low,
                timeout,
            },
        ),
        CommandKind::Program { program, args } => {
            if a.low {
                bail!("--low is for `tool: xpp` gates for now: a program gate at Low integrity needs a clean checkout of a commit, which a task makes (`toryo run`)");
            }
            run_program(&cfg, c, program, args, &repo, a.print_only, timeout)
        }
    }
}

/// A `program` gate: the command Verify would run, in `repo` with your own rights.
fn run_program(
    cfg: &Effective,
    c: &CommandSpec,
    program: &str,
    args: &[String],
    repo: &std::path::Path,
    print_only: bool,
    timeout: Duration,
) -> Result<i32> {
    if !repo.is_dir() {
        bail!("{} is not a folder (--repo)", repo.display());
    }
    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".into());
    // `TORYO_VERIFY_BUILD`, the temp, home and cache folders: a scratch folder, not the repository
    let scratch = verify::verify_root(cfg).join("gate-run").join(name);
    let co = Checkout {
        base: scratch.clone(),
        dir: repo.to_path_buf(),
        admin: PathBuf::new(),
        pointer: String::new(),
        low: false,
    };
    let mut env: BTreeMap<String, String> = verify::env(&co);
    verify::set_gate_vars(&mut env, "gate-run", WORKING_TREE, &co, &co.dir);
    xpp::set_ui_language(&mut env, &BTreeMap::new());
    let program = verify_program(&expand_vars(program)?, &co.dir);
    if print_only {
        println!("# {} — runs in {}", c.name, repo.display());
        for (k, v) in env.iter().filter(|(k, _)| k.starts_with("TORYO_")) {
            println!("#   {k}={v}");
        }
        println!("{}", xpp::powershell_line(&program, args));
        return Ok(0);
    }
    for d in [
        co.tmp(),
        co.home(),
        co.cache(),
        co.build(),
        scratch.join("logs"),
    ] {
        std::fs::create_dir_all(&d)?;
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    println!(
        "{} · {} · no boundary (your own rights), working tree of {}",
        c.name,
        describe(c),
        repo.display()
    );
    let res = rt.block_on(LocalRunner.run(
        RunSpec {
            name: c.name.clone(),
            program,
            args: args.to_vec(),
            cwd: co.dir.clone(),
            env,
            boundary: Boundary::None,
            timeout,
            cancel: None,
            redact: toryo::process::redact::Redactions::empty(),
        },
        None,
    ))?;
    let log = scratch
        .join("logs")
        .join(format!("{}.log", xpp::file_name(&c.name)));
    std::fs::write(&log, &res.output)?;
    let ended = match (res.exit, res.timed_out) {
        (Some(code), _) => format!("exit {code}"),
        (None, true) => "timed out".into(),
        (None, false) => "no exit code".into(),
    };
    let mark = if res.ok() { "✔" } else { "✘" };
    let secs = res.duration_ms as f64 / 1000.0;
    if c.reads_msbuild() {
        let d = xpp::diagnostics(&res.output, &co.dir);
        println!(
            "{mark} {} ({ended}, {secs:.1}s) — {} error(s), {} warning(s) — full output: {}",
            c.name,
            d.errors_total,
            d.warnings_total,
            log.display()
        );
        for e in &d.errors {
            println!("  {}", e.render());
        }
        if d.errors_total > d.errors.len() {
            println!("  … and {} more", d.errors_total - d.errors.len());
        }
        if !res.ok() && d.errors.is_empty() {
            println!("last output:\n{}", res.tail(40));
        }
    } else {
        println!(
            "{mark} {} ({ended}, {secs:.1}s) — full output: {}",
            c.name,
            log.display()
        );
        if !res.ok() {
            println!("last output:\n{}", res.tail(40));
        }
    }
    Ok(if res.ok() { 0 } else { 1 })
}
