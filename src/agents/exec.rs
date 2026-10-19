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
use crate::agents::{ErrorClass, LineSplitter, TurnOutcome};
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
    /// M16-AT (X39): called with the process id right after the agent started.
    pub on_spawn: Option<Arc<dyn Fn(u32) + Send + Sync>>,
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
            on_spawn: None,
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

/// One capture file of a turn's stream. Captures are evidence and replay fixtures (R42,
/// §16), so a failed write is never ignored (CODE-07): the first error is kept, nothing
/// more is written (the file stays an intact prefix of the stream, never one with a hole),
/// and the turn is failed once it has ended.
struct Capture {
    file: std::fs::File,
    name: String,
    written: u64,
    error: Option<String>,
}

impl Capture {
    fn create(path: &Path) -> Result<Self> {
        Ok(Self {
            file: std::fs::File::create(path)
                .with_context(|| format!("creating {}", path.display()))?,
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            written: 0,
            error: None,
        })
    }

    fn write(&mut self, bytes: &[u8]) {
        if self.error.is_some() {
            return;
        }
        match self.file.write_all(bytes) {
            Ok(()) => self.written += bytes.len() as u64,
            Err(e) => {
                self.error = Some(format!(
                    "writing {} failed after {} bytes: {e}",
                    self.name, self.written
                ))
            }
        }
    }
}

/// A turn whose capture is incomplete fails; a turn that failed anyway keeps its class
/// (the implementer fallback keys on it) and gets the capture error added to its detail.
fn fail_on_capture_errors(outcome: &mut TurnOutcome, errors: Vec<String>) {
    if errors.is_empty() {
        return;
    }
    let note = format!("capture incomplete: {}", errors.join("; "));
    outcome.protocol_errors.push(note.clone());
    outcome.success = false;
    outcome.error_class.get_or_insert(ErrorClass::Unknown);
    outcome.error_detail = Some(match outcome.error_detail.take() {
        Some(d) if !d.is_empty() => format!("{d}; {note}"),
        _ => note,
    });
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
        profile_name,
        profile,
        args,
        cwd,
        prompt,
        run_dir,
        label,
        opts,
        &mut |_| {},
    )
}

/// M13-13: the turn's process was never started — its profile's secret is not there (a missing
/// Credential Manager entry), its `exe` cannot be expanded, or the program cannot be started.
/// Nothing ran, so nothing was used: the turn fails with this text and holds no reservation.
#[derive(Debug)]
pub struct NotStarted(pub String);

impl std::fmt::Display for NotStarted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotStarted {}

fn not_started(e: anyhow::Error) -> anyhow::Error {
    anyhow::Error::new(NotStarted(format!("{e:#}")))
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
    let (mut env, red, secret_keys) = profile_env(profile_name, profile).map_err(not_started)?;
    for (k, v) in &opts.env {
        env.retain(|e, _| !e.eq_ignore_ascii_case(k));
        env.insert(k.clone(), v.clone());
    }
    let program = PathBuf::from(crate::config::expand_vars(&profile.exe).map_err(not_started)?);
    // M13-R7: all of this runs before any process exists — a failure here is `NotStarted` too
    // (no process ran, so no budget is held)
    std::fs::create_dir_all(run_dir)
        .with_context(|| format!("creating {}", run_dir.display()))
        .map_err(not_started)?;
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
    std::fs::write(run_dir.join(format!("{label}.prompt.md")), prompt)
        .context("writing the prompt file")
        .map_err(not_started)?;
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
    )
    .context("writing the meta file")
    .map_err(not_started)?;

    let spec = SpawnSpec {
        program: program.clone(),
        args: args.clone(),
        cwd: cwd.to_path_buf(),
        env,
        stdin: Some(prompt.as_bytes().to_vec()),
        boundary: Boundary::None,
    };
    let mut out_file =
        Capture::create(&run_dir.join(format!("{label}.stdout.jsonl"))).map_err(not_started)?;
    let mut err_file =
        Capture::create(&run_dir.join(format!("{label}.stderr.txt"))).map_err(not_started)?;
    let running = spawn(&spec, &red)
        .with_context(|| format!("starting {}", program.display()))
        .map_err(not_started)?;
    if let Some(f) = &opts.on_spawn {
        f(running.root.pid);
    }
    let mut splitter = LineSplitter::new(LineSplitter::DEFAULT_CAP);
    let mut claude = ClaudeParser::new();
    let mut codex = CodexParser::new();
    let kind = profile.kind;
    let echo = opts.echo;
    let started = Instant::now();
    let kill_after = opts.kill_after;
    let cancel_flag = opts.cancel.clone();
    let cancel: Option<Box<dyn Fn() -> bool + Send>> =
        if kill_after.is_some() || cancel_flag.is_some() {
            Some(Box::new(move || {
                kill_after.is_some_and(|d| started.elapsed() >= d)
                    || cancel_flag
                        .as_ref()
                        .is_some_and(|c| c.load(Ordering::SeqCst))
            }))
        } else {
            None
        };
    let wait = WaitOptions {
        max: Some(opts.max),
        stall: Some(opts.stall),
        cancel,
        heartbeat: None,
    };
    // M12-14: what the CLI printed to stderr, kept (the tail) for a turn that ends without a
    // result — a rejected flag is printed there and nowhere else
    let mut stderr_tail: Vec<u8> = Vec::new();
    let exit = running.wait(wait, |c| match c {
        Chunk::Stdout(b) => {
            out_file.write(&b);
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
            err_file.write(&b);
            stderr_tail.extend_from_slice(&b);
            if stderr_tail.len() > STDERR_TAIL_BYTES {
                stderr_tail.drain(..stderr_tail.len() - STDERR_TAIL_BYTES);
            }
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
    // M12-14: a turn that died without a result says why, in the CLI's own words (the chunks
    // are redacted already; the derived text is redacted again below)
    if !outcome.success && outcome.error_class == Some(crate::domain::ErrorClass::ProcessCrashed) {
        if let Some(lines) = stderr_last_lines(&stderr_tail, STDERR_LINES) {
            outcome.error_detail = Some(with_stderr(outcome.error_detail.take(), &lines));
        }
    }
    fail_on_capture_errors(
        &mut outcome,
        [out_file.error, err_file.error]
            .into_iter()
            .flatten()
            .collect(),
    );
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
        crate::errln!();
    }
    Ok(TurnRun {
        outcome,
        exit,
        run_dir: run_dir.to_path_buf(),
        argv: argv_view,
        argv_sha256,
    })
}

/// M12-14: the detail of a turn that died without a result, with what the CLI printed to
/// stderr. M13-14: also when the turn had no detail of its own — then the stderr lines are the
/// detail (was: the text began with a stray "; ").
fn with_stderr(detail: Option<String>, lines: &str) -> String {
    let tail = format!("the CLI's stderr (last lines):\n{lines}");
    match detail {
        Some(d) if !d.trim().is_empty() => format!("{d}; {tail}"),
        _ => tail,
    }
}

/// How much stderr is kept for [`stderr_last_lines`].
const STDERR_TAIL_BYTES: usize = 16 * 1024;
/// How many stderr lines a crashed turn's reason carries.
const STDERR_LINES: usize = 10;

/// The last `n` non-empty lines of `bytes` as text — colour codes stripped, each line cut at
/// 300 characters; `None` when there is none.
fn stderr_last_lines(bytes: &[u8], n: usize) -> Option<String> {
    let text = crate::process::strip_ansi(&crate::process::decode_output(bytes));
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.trim_end().chars().take(300).collect::<String>())
        .filter(|l| !l.trim().is_empty())
        .collect();
    let from = lines.len().saturating_sub(n);
    (!lines[from..].is_empty()).then(|| {
        lines[from..]
            .iter()
            .map(|l| format!("  | {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    })
}

fn echo_line(kind: ProfileKind, line: &[u8]) {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
        return;
    };
    match kind {
        ProfileKind::Claude => {
            if let Some(t) = v.pointer("/event/delta/text").and_then(|t| t.as_str()) {
                eprint!("{}", crate::term::text(t));
            } else if v["type"] == "system" {
                crate::errln!("\n[system/{}]", v["subtype"].as_str().unwrap_or("?"));
            } else if v["type"] == "result" {
                crate::errln!("\n[result/{}]", v["subtype"].as_str().unwrap_or("?"));
            }
        }
        ProfileKind::Codex => {
            let ty = v["type"].as_str().unwrap_or("?");
            let item = v["item"]["type"].as_str().unwrap_or("");
            if ty == "item.completed" && item == "command_execution" {
                crate::errln!(
                    "[cmd] {} → {} (exit {})",
                    v["item"]["command"].as_str().unwrap_or(""),
                    v["item"]["status"].as_str().unwrap_or("?"),
                    v["item"]["exit_code"]
                );
            } else if ty != "item.updated" {
                crate::errln!("[{ty}{}{}]", if item.is_empty() { "" } else { " " }, item);
            }
        }
    }
}

/// Print the human summary of one turn.
pub fn print_summary(r: &TurnRun) {
    let o = &r.outcome;
    crate::errln!("── turn summary ─────────────────────────────");
    crate::errln!("success          : {}", o.success);
    crate::errln!(
        "session/thread id: {}",
        o.session_id.as_deref().unwrap_or("-")
    );
    if let Some(c) = o.error_class {
        crate::errln!(
            "error            : {:?} — {}",
            c,
            o.error_detail.as_deref().unwrap_or("")
        );
    }
    crate::errln!("permission denials: {}", o.permission_denials.len());
    crate::errln!("usage ({:?})   : {:?}", o.accounting, o.usage);
    if let Some(raw) = &o.raw_usage {
        crate::errln!("raw usage        : {raw}");
    }
    crate::errln!(
        "exit             : {:?} timed_out={} stalled={} cancelled={} tree_gone={}",
        r.exit.code,
        r.exit.timed_out,
        r.exit.stalled,
        r.exit.cancelled,
        r.exit.tree_gone
    );
    if !o.protocol_errors.is_empty() {
        crate::errln!("protocol errors  : {:?}", o.protocol_errors);
    }
    if !o.warnings.is_empty() {
        crate::errln!("warnings         : {:?}", o.warnings);
    }
    crate::errln!("captured in      : {}", r.run_dir.display());
    if let Some(s) = &o.structured {
        println!("{}", serde_json::to_string_pretty(s).unwrap_or_default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M12-14: the last 10 non-empty lines, colour codes stripped, long lines cut.
    #[test]
    fn the_stderr_tail_is_the_last_ten_lines_without_colour_codes() {
        let mut text = String::new();
        for i in 1..=14 {
            text.push_str(&format!("line {i}\n\n"));
        }
        text.push_str("\u{1b}[31merror: unknown option --x\u{1b}[0m\n");
        text.push_str(&"y".repeat(500));
        let got = stderr_last_lines(text.as_bytes(), 10).unwrap();
        let lines: Vec<&str> = got.lines().collect();
        assert_eq!(lines.len(), 10, "{got}");
        assert_eq!(lines[8], "  | error: unknown option --x");
        assert_eq!(lines[9].chars().count(), 4 + 300);
        assert!(!got.contains("line 5") && got.contains("line 7"), "{got}");
        assert!(!got.contains('\u{1b}'));
        assert_eq!(stderr_last_lines(b"  \n\n", 10), None);
        assert_eq!(stderr_last_lines(b"", 10), None);
    }

    /// M13-14 (9): the stderr tail is attached also when the turn has no detail of its own —
    /// as the detail, not after a stray separator.
    #[test]
    fn the_stderr_tail_is_the_detail_when_there_is_none() {
        let lines = "  | error: unknown option --x";
        assert_eq!(
            with_stderr(Some("exit code 2 without a result".into()), lines),
            "exit code 2 without a result; the CLI's stderr (last lines):\n  | error: unknown option --x"
        );
        for none in [None, Some(String::new()), Some("  ".to_string())] {
            assert_eq!(
                with_stderr(none, lines),
                "the CLI's stderr (last lines):\n  | error: unknown option --x"
            );
        }
    }
}
