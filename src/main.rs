//! `toryo` CLI (plan v0.7 §15): the engine commands (`task`, `run`, `approve`, `reject`,
//! `resume`, `stop`, `status`, `partial`), `doctor`, `secrets`, `gate list|run` (M14), `xpp build`
//! (M9-B4), and the M0 kit (`spike`, feature `m0-kit`).

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

use toryo::agents::claude::LaunchMode;
use toryo::config::{default_machine_config_path, Effective};

mod engine_cli;
mod gate_cli;
#[cfg(feature = "m0-kit")]
mod spike_cli;
mod xpp_cli;

use engine_cli::{BudgetCmd, ContextCmd, DriveArgs, Gate, PartialCmd, ResumeArgs, TaskCmd};
#[cfg(feature = "m0-kit")]
use spike_cli::{run_canaries, spike, SpikeCmd};

#[derive(Parser)]
#[command(
    name = "toryo",
    version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("TORYO_COMMIT"), ")"),
    about = "Toryo (棟梁) — orchestrates Claude Code and Codex through a gated, verified workflow"
)]
pub(crate) struct Cli {
    /// Repository config (default: ./toryo.yaml, then $TORYO_CONFIG)
    #[arg(long, global = true)]
    pub(crate) config: Option<PathBuf>,
    /// Machine config (default: %USERPROFILE%\.toryo\toryo.local.yaml)
    #[arg(long, global = true)]
    pub(crate) machine_config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Cmd {
    /// Check executables, profiles, secrets, repository config and recorded M0 evidence.
    Doctor {
        #[arg(long)]
        json: bool,
        /// First re-run the canaries (R51): job-test, git-canary, lowil-canary (Windows),
        /// claude-isolation per Claude profile, codex-canary per Codex profile.
        #[arg(long)]
        canaries: bool,
        /// With --canaries: only these profiles (default: every profile in the config)
        #[arg(long, requires = "canaries")]
        profile: Vec<String>,
        /// With --canaries: the Claude launch mode to verify (the pipeline's `--mode`)
        #[arg(long, value_enum, default_value = "restricted", requires = "canaries")]
        claude_mode: LaunchMode,
    },
    /// The effective configuration (M11): mode, files read, and where each seat, profile,
    /// gate, toolchain and verify setting came from.
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Create, list, show or clean up tasks.
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    /// 🔒 Run a task until a gate, an escalation, a stop or completion (recovers first).
    Run {
        id: String,
        #[command(flatten)]
        drive: DriveArgs,
    },
    /// 🔒 Approve the open gate (R32: refuses if its subject changed).
    Approve {
        id: String,
        #[arg(long, value_enum)]
        gate: Gate,
        /// Refuse unless the gate's subject digest starts with this
        #[arg(long)]
        expect: Option<String>,
        /// From Escalated: skip the reviewer and open the final gate (needs a verified checkpoint)
        #[arg(long)]
        force: bool,
        /// Approve, then run on
        #[arg(long = "continue")]
        cont: bool,
        #[command(flatten)]
        drive: DriveArgs,
    },
    /// 🔒 Reject the open gate with a reason (the revision opens a new occurrence, R44a).
    Reject {
        id: String,
        #[arg(long, value_enum)]
        gate: Gate,
        #[arg(long)]
        reason: String,
    },
    /// 🔒 Recover after a crash or stop and continue; without an id, every task whose lock is
    /// free (active tasks are reported and skipped, R38).
    Resume {
        id: Option<String>,
        /// Leave Escalated → Fixing with this note for the Implementer
        #[arg(long)]
        note: Option<String>,
        /// After a recovery policy violation: reset to the attempt's start (snapshot kept, R39)
        #[arg(long, conflicts_with_all = ["note", "keep_partial"])]
        discard_partial: bool,
        /// After fixing the worktree by hand: snapshot, re-check, recovery checkpoint (R31)
        #[arg(long, conflicts_with = "note")]
        keep_partial: bool,
        /// After `protected_config_changed`: accept this changed protected path (repeat for
        /// each); a new protected baseline is taken and the decision goes to DECISIONS.md (R35)
        #[arg(long = "accept-protected-change", value_name = "PATH", conflicts_with_all = ["note", "discard_partial", "keep_partial"])]
        accept_protected: Vec<String>,
        #[command(flatten)]
        drive: DriveArgs,
    },
    /// Ask the task's current owner to stop (R38); waits for the answer.
    Stop {
        id: String,
        #[arg(long, default_value_t = 15)]
        wait_secs: u64,
    },
    /// Tasks, their state and the next human action.
    Status {
        id: Option<String>,
        /// One JSON document on stdout: `{ "tasks": [ … ] }` (schema in the README)
        #[arg(long)]
        json: bool,
    },
    /// Follow a task's flight recorder (`events.jsonl`) like `tail -f` — no task lock needed.
    /// Stops when the task waits for a human, is finished or nobody drives it (2 s without an
    /// event), or on Ctrl+C.
    Watch {
        id: String,
        /// The file's JSON lines instead of the rendered text
        #[arg(long)]
        json: bool,
        /// Print the whole history first (default: the last 20 events)
        #[arg(long)]
        from_start: bool,
        /// Print the history and exit
        #[arg(long)]
        no_follow: bool,
    },
    /// Tokens per stage × profile × model and the estimated cost (plan §12): prices from
    /// `%USERPROFILE%\.toryo\prices.toml` (or --prices) for rows billed foundry/api.
    Usage {
        id: String,
        /// One JSON document on stdout
        #[arg(long)]
        json: bool,
        /// The price table (default: %USERPROFILE%\.toryo\prices.toml; none → no estimates)
        #[arg(long)]
        prices: Option<PathBuf>,
    },
    /// 🔒 The token admission cap of a task (R41).
    Budget {
        #[command(subcommand)]
        cmd: BudgetCmd,
    },
    /// What an agent receives: the assembled project context (plan §7).
    Context {
        #[command(subcommand)]
        cmd: ContextCmd,
    },
    /// Recovery snapshots (R39).
    Partial {
        #[command(subcommand)]
        cmd: PartialCmd,
    },
    /// Check gates outside a task: `toryo gate list`, `toryo gate run NAME [--print-only]`
    /// proves a build or test script before a task depends on it.
    Gate {
        #[command(subcommand)]
        cmd: gate_cli::GateCmd,
    },
    /// X++ (D365FO) builds outside a task: `toryo xpp build --print-only` shows what Verify runs.
    Xpp {
        #[command(subcommand)]
        cmd: xpp_cli::XppCmd,
    },
    /// Windows Credential Manager secrets.
    Secrets {
        #[command(subcommand)]
        cmd: SecretsCmd,
    },
    /// Show a sample Windows toast (M10-D1): check that notifications reach you.
    #[command(hide = true)]
    NotifyTest {
        /// Build the toast in PowerShell, do not show it
        #[arg(long)]
        dry_run: bool,
    },
    /// M0 spike commands.
    #[cfg(feature = "m0-kit")]
    Spike {
        #[command(subcommand)]
        cmd: SpikeCmd,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Print the effective configuration of this repository and machine (M11-B2).
    Show {
        /// One JSON document on stdout (schema in the README)
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum SecretsCmd {
    /// Store a secret read from stdin (one line), e.g. `Read-Host -MaskInput | toryo secrets set toryo/foundry-openai`.
    Set { target: String },
}

/// The repository config in use (M12-09, X21): `--config`, then `./toryo.yaml` when it exists,
/// then `$TORYO_CONFIG` — as `--help` says. A variable in the shell profile used to win over the
/// file of the repository the owner stood in, and `task create` silently made its worktree for
/// another one. When both exist and are different files one note line names both.
pub(crate) fn config_path(cli: &Cli) -> PathBuf {
    if let Some(p) = &cli.config {
        return p.clone();
    }
    let local = PathBuf::from("toryo.yaml");
    let var = std::env::var_os("TORYO_CONFIG").map(PathBuf::from);
    if local.is_file() {
        if let Some(v) = var.filter(|v| v.is_file()) {
            let same = match (std::fs::canonicalize(&local), std::fs::canonicalize(&v)) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            };
            if !same {
                static ONCE: std::sync::Once = std::sync::Once::new();
                ONCE.call_once(|| {
                    eprintln!(
                        "note: using {} (the file in this folder); $TORYO_CONFIG ({}) is ignored here — pass --config to choose",
                        std::fs::canonicalize(&local).unwrap_or(local.clone()).display(),
                        v.display()
                    );
                });
            }
        }
        return local;
    }
    var.unwrap_or(local)
}

/// Why [`config_path`] chose its file, in words (M12-17: `doctor` says it).
fn config_source(cli: &Cli) -> String {
    if cli.config.is_some() {
        "`--config`".into()
    } else if PathBuf::from("toryo.yaml").is_file() {
        "the toryo.yaml of the folder you are in".into()
    } else if std::env::var_os("TORYO_CONFIG").is_some() {
        "$TORYO_CONFIG (there is no toryo.yaml in this folder)".into()
    } else {
        "the default `./toryo.yaml`".into()
    }
}

pub(crate) fn load_config(cli: &Cli) -> Result<Effective> {
    let path = config_path(cli);
    let machine = cli
        .machine_config
        .clone()
        .or_else(default_machine_config_path);
    Effective::load(&path, machine.as_deref())
}

/// The drive options with the machine config in use, where `notify:` is read (M10-D1).
fn drive_with(cli: &Cli, d: &DriveArgs) -> DriveArgs {
    let mut d = d.clone();
    d.machine_config = cli
        .machine_config
        .clone()
        .or_else(default_machine_config_path);
    d
}

fn run(cli: Cli) -> Result<i32> {
    match &cli.cmd {
        Cmd::Doctor {
            json,
            canaries,
            profile,
            claude_mode,
        } => {
            #[cfg(feature = "m0-kit")]
            let failed = if *canaries {
                run_canaries(&cli, profile, *claude_mode)?
            } else {
                vec![]
            };
            #[cfg(not(feature = "m0-kit"))]
            let failed: Vec<String> = if *canaries {
                let _ = (profile, claude_mode);
                bail!("this toryo was built without the m0-kit feature: no canaries")
            } else {
                vec![]
            };
            let (cfg, err) = match load_config(&cli) {
                Ok(c) => (Some(c), None),
                Err(e) => (None, Some(format!("{e:#}"))),
            };
            // `$TORYO_CONFIG` is only worth a word when it could have been the choice
            let variable = std::env::var_os("TORYO_CONFIG")
                .map(PathBuf::from)
                .filter(|_| cli.config.is_none());
            let code = toryo::doctor::run(
                cfg.as_ref(),
                err,
                *json,
                &config_source(&cli),
                variable.as_deref(),
            )?;
            if !failed.is_empty() {
                eprintln!("canaries that did not pass: {}", failed.join(", "));
                return Ok(code.max(1));
            }
            Ok(code)
        }
        Cmd::Secrets {
            cmd: SecretsCmd::Set { target },
        } => {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            let v = line.trim_end_matches(['\r', '\n']);
            if v.is_empty() {
                bail!("empty secret on stdin");
            }
            toryo::config::store_secret(target, v)?;
            eprintln!("stored credman:{target}");
            Ok(0)
        }
        #[cfg(feature = "m0-kit")]
        Cmd::Spike { cmd } => spike(&cli, cmd),
        Cmd::Config {
            cmd: ConfigCmd::Show { json },
        } => {
            let cfg = load_config(&cli)?;
            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&toryo::config_show::json(&cfg))?
                );
            } else {
                print!("{}", toryo::config_show::text(&cfg));
            }
            Ok(0)
        }
        Cmd::Task { cmd } => engine_cli::task(&cli, cmd),
        Cmd::Run { id, drive } => engine_cli::run(id, &drive_with(&cli, drive)),
        Cmd::Approve {
            id,
            gate,
            expect,
            force,
            cont,
            drive,
        } => engine_cli::approve(
            id,
            *gate,
            expect.as_deref(),
            *force,
            *cont,
            &drive_with(&cli, drive),
        ),
        Cmd::Reject { id, gate, reason } => engine_cli::reject(id, *gate, reason),
        Cmd::Resume {
            id,
            note,
            discard_partial,
            keep_partial,
            accept_protected,
            drive,
        } => engine_cli::resume(
            id.as_deref(),
            &ResumeArgs {
                note: note.clone(),
                discard_partial: *discard_partial,
                keep_partial: *keep_partial,
                accept_protected: accept_protected.clone(),
            },
            &drive_with(&cli, drive),
        ),
        Cmd::Stop { id, wait_secs } => engine_cli::stop(id, *wait_secs),
        Cmd::Status { id, json } => engine_cli::status(id.as_deref(), *json),
        Cmd::Watch {
            id,
            json,
            from_start,
            no_follow,
        } => engine_cli::watch(id, *json, *from_start, *no_follow),
        Cmd::Usage { id, json, prices } => engine_cli::usage(id, *json, prices.as_deref()),
        Cmd::Partial { cmd } => engine_cli::partial(cmd),
        Cmd::Context { cmd } => engine_cli::context(cmd),
        Cmd::Budget { cmd } => engine_cli::budget(cmd),
        Cmd::Gate { cmd } => gate_cli::run(&cli, cmd),
        Cmd::Xpp { cmd } => xpp_cli::run(&cli, cmd),
        Cmd::NotifyTest { dry_run } => {
            let code = toryo::notify::test_toast(*dry_run)?;
            eprintln!("{}", toryo::notify::test_message(code, *dry_run));
            Ok(if code == 0 { 0 } else { 1 })
        }
    }
}

/// The process exit status for `code`. A code that does not fit a `u8` (negative, or 256 and
/// up) is a failure (1), never a truncated value such as 256 → 0 (CODE-08).
fn exit_status(code: i32) -> u8 {
    u8::try_from(code).unwrap_or(1)
}

/// The stack of the thread that runs the CLI: clap's derived builder for the whole command tree
/// needs more than the 1 MB main-thread stack of a Windows debug build (M10 added `watch` and the
/// build overflowed on `--help`).
const CLI_STACK: usize = 8 << 20;

fn main() -> ExitCode {
    match std::thread::Builder::new()
        .name("toryo".into())
        .stack_size(CLI_STACK)
        .spawn(cli_main)
    {
        Ok(h) => h.join().unwrap_or_else(|_| ExitCode::from(101)),
        Err(e) => {
            eprintln!("error: cannot start the CLI thread: {e}");
            ExitCode::from(2)
        }
    }
}

fn cli_main() -> ExitCode {
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(code) => ExitCode::from(exit_status(code)),
        Err(e) => match e.downcast_ref::<toryo::engine::lock::TaskActive>() {
            Some(a) => {
                eprintln!("task_active: {a}");
                ExitCode::from(exit_status(toryo::engine::lock::EXIT_TASK_ACTIVE))
            }
            None => {
                eprintln!("error: {e:#}");
                ExitCode::from(2)
            }
        },
    };
    // M10-R4, R8: settle the toasts this toryo started (each within its 10 s deadline) — after
    // the error is on the screen, not before
    toryo::notify::wait_pending();
    code
}

#[cfg(test)]
mod tests {
    use super::exit_status;

    #[test]
    fn exit_codes_that_do_not_fit_a_byte_are_failures() {
        assert_eq!(exit_status(0), 0);
        assert_eq!(exit_status(3), 3);
        assert_eq!(exit_status(255), 255);
        for code in [256, 512, 1000, -1, i32::MIN, i32::MAX] {
            assert_eq!(exit_status(code), 1, "{code}");
        }
    }
}
