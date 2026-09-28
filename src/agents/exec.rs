//! Run one real agent turn inside a Job, capture the redacted stream, parse it.
//! (M0 kit `spike::turn`, moved to the core in M1 — the Claude/Codex adapters build on it.)

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::agents::claude::ClaudeParser;
use crate::agents::codex::CodexParser;
use crate::agents::{LineSplitter, TurnOutcome};
use crate::config::{resolve_profile_secrets, Profile, ProfileKind};
use crate::process::env::{build_env, non_secret_view, validate_provider_selection};
use crate::process::redact::{Redactions, Secret};
use crate::process::{spawn, Boundary, Chunk, ExitInfo, SpawnSpec, WaitOptions};
use crate::usage::CodexSubsets;

pub struct TurnOpts {
    pub max: Duration,
    pub stall: Duration,
    /// Terminate the Job this many seconds after start (simulates `toryo stop` / a crash).
    pub kill_after: Option<Duration>,
    /// Print text deltas / items to the console while running.
    pub echo: bool,
    /// Extra environment for this turn (e.g. a per-task TEMP inside the worktree),
    /// applied after the profile env; keys replace case-insensitively.
    pub env: BTreeMap<String, String>,
    /// Set to `true` to terminate the turn's Job (adapter `interrupt()`, `toryo stop`).
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for TurnOpts {
    fn default() -> Self {
        Self {
            max: Duration::from_secs(30 * 60),
            stall: Duration::from_secs(10 * 60),
            kill_after: None,
            echo: true,
            env: BTreeMap::new(),
            cancel: None,
        }
    }
}

pub struct TurnRun {
    pub outcome: TurnOutcome,
    pub exit: ExitInfo,
    pub run_dir: PathBuf,
    pub argv: Vec<String>,
    pub argv_sha256: String,
}

/// Build the child environment for a profile (R23) and the redaction set (R42).
pub fn profile_env(
    name: &str,
    p: &Profile,
) -> Result<(BTreeMap<String, String>, Redactions, Vec<String>)> {
    let secrets = resolve_profile_secrets(p)?;
    let env = build_env(std::env::vars(), &p.env, &secrets);
    validate_provider_selection(name, p.kind, p.billing, &env)?;
    let mut red: Vec<Secret> = secrets
        .iter()
        .map(|(k, v)| Secret {
            name: k.clone(),
            value: v.clone(),
        })
        .collect();
    // OAuth token of the parent session, if any, is never passed but must never be printed either.
    if let Ok(t) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
        red.push(Secret {
            name: "CLAUDE_CODE_OAUTH_TOKEN".into(),
            value: t,
        });
    }
    Ok((
        env,
        Redactions::new(&red),
        secrets.keys().cloned().collect(),
    ))
}

#[allow(clippy::too_many_arguments)]
pub fn run_turn(
    profile_name: &str,
    profile: &Profile,
    args: Vec<String>,
    cwd: &Path,
    prompt: &str,
    run_dir: &Path,
    label: &str,
    opts: TurnOpts,
) -> Result<TurnRun> {
    run_turn_with(
        profile_name, profile, args, cwd, prompt, run_dir, label, opts, &mut |_| {},
    )
}

/// `run_turn`, calling `on_line` with every complete (redacted) stdout line as it arrives —
/// the adapters turn these into live `AgentEvent`s.
#[allow(clippy::too_many_arguments)]
pub fn run_turn_with(
    profile_name: &str,
    profile: &Profile,
    args: Vec<String>,
    cwd: &Path,
    prompt: &str,
    run_dir: &Path,
    label: &str,
    opts: TurnOpts,
    on_line: &mut (dyn FnMut(&[u8]) + Send),
) -> Result<TurnRun> {
    let (mut env, red, secret_keys) = profile_env(profile_name, profile)?;
    for (k, v) in &opts.env {
        env.retain(|e, _| !e.eq_ignore_ascii_case(k));
        env.insert(k.clone(), v.clone());
    }
    let program = PathBuf::from(crate::config::expand_vars(&profile.exe)?);
    std::fs::create_dir_all(run_dir)?;
    let argv_view: Vec<String> = std::iter::once(program.display().to_string())
        .chain(args.iter().cloned())
        .collect();
    let argv_sha256 = {
        let mut h = Sha256::new();
        h.update(serde_json::to_vec(
            &json!({ "argv": argv_view, "env": non_secret_view(&env, &secret_keys) }),
        )?);
        hex::encode(h.finalize())
    };
    std::fs::write(run_dir.join(format!("{label}.prompt.md")), prompt)?;
    let meta = json!({
        "profile": profile_name,
        "kind": profile.kind,
        "billing": profile.billing,
        "cwd": cwd,
        "argv": argv_view,
        "env": non_secret_view(&env, &secret_keys),
        "argv_sha256": argv_sha256,
    });
    std::fs::write(
        run_dir.join(format!("{label}.meta.json")),
        serde_json::to_vec_pretty(&meta)?,
    )?;

    let spec = SpawnSpec {
        program: program.clone(),
        args: args.clone(),
        cwd: cwd.to_path_buf(),
        env,
        stdin: Some(prompt.as_bytes().to_vec()),
        boundary: Boundary::None,
    };
    let mut out_file = std::fs::File::create(run_dir.join(format!("{label}.stdout.jsonl")))?;
    let mut err_file = std::fs::File::create(run_dir.join(format!("{label}.stderr.txt")))?;
    let running = spawn(&spec, &red).with_context(|| format!("starting {}", program.display()))?;
    let mut splitter = LineSplitter::new(LineSplitter::DEFAULT_CAP);
    let mut claude = ClaudeParser::new();
    let mut codex = CodexParser::new();
    let kind = profile.kind;
    let echo = opts.echo;
    let started = Instant::now();
    let kill_after = opts.kill_after;
    let cancel_flag = opts.cancel.clone();
    let cancel: Option<Box<dyn Fn() -> bool + Send>> = if kill_after.is_some() || cancel_flag.is_some() {
        Some(Box::new(move || {
            kill_after.is_some_and(|d| started.elapsed() >= d)
                || cancel_flag.as_ref().is_some_and(|c| c.load(Ordering::SeqCst))
        }))
    } else {
        None
    };
    let wait = WaitOptions {
        max: Some(opts.max),
        stall: Some(opts.stall),
        cancel,
    };
    let exit = running.wait(wait, |c| match c {
        Chunk::Stdout(b) => {
            let _ = out_file.write_all(&b);
            for line in splitter.push(&b) {
                if echo {
                    echo_line(kind, &line);
                }
                on_line(&line);
                match kind {
                    ProfileKind::Claude => claude.feed_line(&line),
                    ProfileKind::Codex => codex.feed_line(&line),
                }
            }
        }
        Chunk::Stderr(b) => {
            let _ = err_file.write_all(&b);
        }
    })?;
    if let Some(rest) = splitter.finish() {
        on_line(&rest);
        match kind {
            ProfileKind::Claude => claude.feed_line(&rest),
            ProfileKind::Codex => codex.feed_line(&rest),
        }
    }
    let mut outcome = match kind {
        ProfileKind::Claude => claude.finish(exit.code),
        ProfileKind::Codex => codex.finish(exit.code, CodexSubsets::default()),
    };
    outcome.oversized_lines = splitter.oversized;
    // Text rebuilt from separate delta events was never contiguous in the byte stream, so the
    // stream redactor could not see a secret split across two deltas: redact derived fields too.
    outcome.final_text = red.redact_str(&outcome.final_text);
    outcome.error_detail = outcome.error_detail.as_deref().map(|t| red.redact_str(t));
    if let Some(v) = &outcome.structured {
        let text = red.redact_str(&v.to_string());
        outcome.structured =
            Some(serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text)));
    }
    std::fs::write(
        run_dir.join(format!("{label}.outcome.json")),
        serde_json::to_vec_pretty(&json!({ "outcome": outcome, "exit": exit }))?,
    )?;
    if echo {
        eprintln!();
    }
    Ok(TurnRun {
        outcome,
        exit,
        run_dir: run_dir.to_path_buf(),
        argv: argv_view,
        argv_sha256,
    })
}

fn echo_line(kind: ProfileKind, line: &[u8]) {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
        return;
    };
    match kind {
        ProfileKind::Claude => {
            if let Some(t) = v.pointer("/event/delta/text").and_then(|t| t.as_str()) {
                eprint!("{t}");
            } else if v["type"] == "system" {
                eprintln!("\n[system/{}]", v["subtype"].as_str().unwrap_or("?"));
            } else if v["type"] == "result" {
                eprintln!("\n[result/{}]", v["subtype"].as_str().unwrap_or("?"));
            }
        }
        ProfileKind::Codex => {
            let ty = v["type"].as_str().unwrap_or("?");
            let item = v["item"]["type"].as_str().unwrap_or("");
            if ty == "item.completed" && item == "command_execution" {
                eprintln!(
                    "[cmd] {} → {} (exit {})",
                    v["item"]["command"].as_str().unwrap_or(""),
                    v["item"]["status"].as_str().unwrap_or("?"),
                    v["item"]["exit_code"]
                );
            } else if ty != "item.updated" {
                eprintln!("[{ty}{}{}]", if item.is_empty() { "" } else { " " }, item);
            }
        }
    }
}

/// Print the human summary of one turn.
pub fn print_summary(r: &TurnRun) {
    let o = &r.outcome;
    eprintln!("── turn summary ─────────────────────────────");
    eprintln!("success          : {}", o.success);
    eprintln!(
        "session/thread id: {}",
        o.session_id.as_deref().unwrap_or("-")
    );
    if let Some(c) = o.error_class {
        eprintln!(
            "error            : {:?} — {}",
            c,
            o.error_detail.as_deref().unwrap_or("")
        );
    }
    eprintln!("permission denials: {}", o.permission_denials.len());
    eprintln!("usage ({:?})   : {:?}", o.accounting, o.usage);
    if let Some(raw) = &o.raw_usage {
        eprintln!("raw usage        : {raw}");
    }
    eprintln!(
        "exit             : {:?} timed_out={} stalled={} cancelled={} tree_gone={}",
        r.exit.code, r.exit.timed_out, r.exit.stalled, r.exit.cancelled, r.exit.tree_gone
    );
    if !o.protocol_errors.is_empty() {
        eprintln!("protocol errors  : {:?}", o.protocol_errors);
    }
    eprintln!("captured in      : {}", r.run_dir.display());
    if let Some(s) = &o.structured {
        println!("{}", serde_json::to_string_pretty(s).unwrap_or_default());
    }
}
