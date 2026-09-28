//! `toryo` CLI (plan v0.7 §15): `doctor`, `secrets`, and the M0 kit (`spike`, feature `m0-kit`).

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

use toryo::agents::claude::LaunchMode;
use toryo::config::{default_machine_config_path, Effective};

#[cfg(feature = "m0-kit")]
mod spike_cli;
#[cfg(feature = "m0-kit")]
use spike_cli::{run_canaries, spike, SpikeCmd};

#[derive(Parser)]
#[command(
    name = "toryo",
    version,
    about = "Toryo — M0 spike kit (plan v0.6)"
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
    /// Windows Credential Manager secrets.
    Secrets {
        #[command(subcommand)]
        cmd: SecretsCmd,
    },
    /// M0 spike commands.
    #[cfg(feature = "m0-kit")]
    Spike {
        #[command(subcommand)]
        cmd: SpikeCmd,
    },
}

#[derive(Subcommand)]
enum SecretsCmd {
    /// Store a secret read from stdin (one line), e.g. `Read-Host -MaskInput | toryo secrets set toryo/foundry-openai`.
    Set { target: String },
}



pub(crate) fn load_config(cli: &Cli) -> Result<Effective> {
    let path = cli
        .config
        .clone()
        .or_else(|| std::env::var_os("TORYO_CONFIG").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("toryo.yaml"));
    let machine = cli
        .machine_config
        .clone()
        .or_else(default_machine_config_path);
    Effective::load(&path, machine.as_deref())
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
            let code = toryo::doctor::run(cfg.as_ref(), err, *json)?;
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
    }
}


fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}
