//! `toryo xpp build` (M9-B4, proposed R72): the command line of a `tool: xpp` verify command,
//! run **outside a task** — no worktree, no lock, no database. The owner uses it to prove the
//! X++ build on the UDE machine (V08-VERIFY 2 and, with `--low`, 3) before a task depends on
//! it, and `--print-only` shows exactly what Verify will run.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};

use toryo::config::{
    data_dir, default_machine_config_path, expand_vars, not_a_repo_path, CommandSpec,
    MachineConfig, Verify,
};
use toryo::engine::verify::{self, Checkout};
use toryo::engine::xpp;
use toryo::process::Boundary;
use toryo::runner::{CommandRunner, LocalRunner, RunSpec};

use crate::{load_config, Cli};

#[derive(Subcommand)]
pub enum XppCmd {
    /// Build X++ projects outside a task with this machine's `toolchains.xpp` — the command line
    /// of a `tool: xpp` verify command (R59). No worktree, lock or database.
    Build(BuildArgs),
}

#[derive(Args)]
pub struct BuildArgs {
    /// The repository holding the X++ source and the projects
    #[arg(long)]
    repo: PathBuf,
    /// An X++ project, relative to the repository (repeat, in build order)
    #[arg(long = "project", required = true)]
    projects: Vec<String>,
    /// The X++ source folder, relative to the repository
    #[arg(long, default_value = "Metadata")]
    metadata: String,
    /// The compiler output folder (default: <verify_root>\xpp-build\<repo name>\build\xpp)
    #[arg(long)]
    out: Option<PathBuf>,
    /// Print the MSBuild command line of each project (PowerShell) and build nothing
    #[arg(long)]
    print_only: bool,
    /// Windows: build at Low integrity (R36), from a copy of the metadata folder and the
    /// projects in a Low-labelled scratch folder under verify_root (V08-VERIFY 3)
    #[arg(long)]
    low: bool,
    /// Time for all projects together, in minutes
    #[arg(long, default_value_t = xpp::DEFAULT_TIMEOUT_MIN)]
    timeout_min: u64,
}

/// `verify.verify_root` of the repository config when one loads, else its default.
fn verify_root(cli: &Cli) -> PathBuf {
    match load_config(cli) {
        Ok(cfg) => verify::verify_root(&cfg),
        Err(_) => expand_vars(&Verify::default().verify_root)
            .map(PathBuf::from)
            .unwrap_or_else(|_| data_dir().join("verify")),
    }
}

/// `p` as given when absolute (so a printed argv matches the one Verify recorded, whose
/// separators may be mixed), else made absolute.
fn absolute(p: &Path) -> Result<PathBuf> {
    if p.is_absolute() {
        Ok(p.to_path_buf())
    } else {
        Ok(std::path::absolute(p)?)
    }
}

/// Copy `src` (a file or a folder, recursively) to `dst`.
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    if src.is_file() {
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(src, dst).with_context(|| format!("copying {}", src.display()))?;
        return Ok(());
    }
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let e = e?;
        copy_tree(&e.path(), &dst.join(e.file_name()))?;
    }
    Ok(())
}

pub fn run(cli: &Cli, cmd: &XppCmd) -> Result<i32> {
    let XppCmd::Build(a) = cmd;
    for (key, v) in std::iter::once(("--metadata", &a.metadata))
        .chain(a.projects.iter().map(|p| ("--project", p)))
    {
        if let Some(why) = not_a_repo_path(v) {
            bail!("{key} `{v}` {why}");
        }
    }
    let machine = cli
        .machine_config
        .clone()
        .or_else(default_machine_config_path)
        .context("no machine config: pass --machine-config")?;
    let m = MachineConfig::load(&machine)?;
    let Some(x) = &m.toolchains.xpp else {
        bail!(
            "no toolchains.xpp in {} (R59): add msbuild and nugets",
            machine.display()
        );
    };
    let tc = xpp::resolve(x)?;
    let repo = absolute(&a.repo)?;
    if a.low && !cfg!(windows) {
        bail!("--low needs Windows (the Low integrity boundary, R36)");
    }
    if a.low && a.out.is_some() {
        bail!("--out cannot be used with --low: at Low integrity the output goes to the Low-labelled scratch folder");
    }
    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".into());
    let scratch = verify_root(cli).join("xpp-build").join(name);
    let checkout = if a.low {
        scratch.join("src")
    } else {
        repo.clone()
    };
    let out = match &a.out {
        Some(o) => absolute(o)?,
        None => scratch.join("build").join("xpp"),
    };
    let projects: Vec<&str> = a.projects.iter().map(String::as_str).collect();
    let spec = CommandSpec::xpp("build", &a.metadata, &projects, Some(a.timeout_min));
    // the response files (the properties, see xpp::invocation) go to the scratch folder, which
    // the build itself cannot write
    let runs = xpp::runs(&spec, &tc, &checkout, &out, &scratch).unwrap_or_default();
    if a.print_only {
        for r in &runs {
            r.write_response()?;
            println!("# {} ({})", r.name, r.project);
            println!("# response file {}:", r.rsp.display());
            for l in r.response.lines() {
                println!("#   {l}");
            }
            println!("{}", xpp::powershell_line(&tc.msbuild, &r.command));
        }
        if a.low {
            println!(
                "# --low: builds a copy of {} and the projects in {}",
                a.metadata,
                checkout.display()
            );
        }
        return Ok(0);
    }
    xpp::check_msbuild(&tc)?;
    let co = Checkout {
        base: scratch.clone(),
        dir: checkout.clone(),
        admin: PathBuf::new(),
        pointer: String::new(),
        low: a.low,
    };
    for d in [
        co.tmp(),
        co.home(),
        co.cache(),
        co.build(),
        scratch.join("logs"),
    ] {
        std::fs::create_dir_all(&d)?;
    }
    if a.low {
        // a fresh copy of what the build reads, writable at Low IL like a verify checkout
        let _ = std::fs::remove_dir_all(&checkout);
        copy_tree(
            &xpp::join_rel(&repo, &a.metadata),
            &xpp::join_rel(&checkout, &a.metadata),
        )?;
        for p in &a.projects {
            copy_tree(&xpp::join_rel(&repo, p), &xpp::join_rel(&checkout, p))?;
        }
        toryo::process::label_low(&[
            checkout.clone(),
            co.build(),
            co.cache(),
            co.tmp(),
            co.home(),
        ])
        .context("labelling the scratch folder Low (R45: verify_root needs WRITE_OWNER)")?;
    }
    std::fs::create_dir_all(&out)?;
    let mut env = verify::env(&co);
    env.extend(xpp::ENV.map(|(k, v)| (k.to_string(), v.to_string())));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let deadline = Instant::now() + Duration::from_secs(60 * a.timeout_min);
    println!(
        "MSBuild {} · output {} · {}",
        tc.msbuild.display(),
        out.display(),
        if a.low {
            "Low integrity"
        } else {
            "no boundary (your own rights)"
        }
    );
    for r in runs {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            println!("✘ {}: not started — --timeout-min used up", r.name);
            return Ok(1);
        }
        eprintln!("── {} ({}) …", r.name, r.project);
        r.write_response()?;
        let res = rt.block_on(LocalRunner.run(
            RunSpec {
                name: r.name.clone(),
                program: tc.msbuild.clone(),
                args: r.command.clone(),
                cwd: checkout.clone(),
                env: env.clone(),
                boundary: if a.low {
                    Boundary::LowIntegrity
                } else {
                    Boundary::None
                },
                timeout: left,
                cancel: None,
            },
            None,
        ))?;
        let log = scratch
            .join("logs")
            .join(format!("{}.log", r.name.replace('/', "-")));
        std::fs::write(&log, &res.output)?;
        let d = xpp::diagnostics(&res.output, &checkout);
        let ended = match (res.exit, res.timed_out) {
            (Some(c), _) => format!("exit {c}"),
            (None, true) => "timed out".into(),
            (None, false) => "no exit code".into(),
        };
        println!(
            "{} {} ({ended}, {:.1}s) — {} error(s), {} warning(s) — full output: {}",
            if res.ok() { "✔" } else { "✘" },
            r.name,
            res.duration_ms as f64 / 1000.0,
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
        if !res.ok() {
            if d.errors.is_empty() {
                println!("last output:\n{}", res.tail(40));
            }
            return Ok(1);
        }
    }
    Ok(0)
}
