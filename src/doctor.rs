//! `toryo doctor` (plan §15): environment, profiles, repository configuration and the
//! M0 evidence recorded so far. Exit code 1 when any check FAILs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::config::{
    data_dir, expand_vars, resolve_profile_secrets, Billing, Effective, ProfileKind,
};
use crate::git::{GitCmd, Loc};
use crate::process::env::{build_env, validate_provider_selection};
use crate::evidence::{self as results, evidence_status, run_version, EvidenceStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Info,
    Warn,
    Fail,
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub level: Level,
    pub area: String,
    pub message: String,
}

#[derive(Default)]
struct Report(Vec<Check>);

impl Report {
    fn add(&mut self, level: Level, area: &str, message: impl Into<String>) {
        self.0.push(Check {
            level,
            area: area.into(),
            message: message.into(),
        });
    }
}

/// Minimum Claude Code versions for flags the plan relies on (§14).
const CLAUDE_MIN_RESTRICTED: (u64, u64, u64) = (2, 1, 248);
const CLAUDE_MIN_PERMISSION_PROMPTS: (u64, u64, u64) = (2, 1, 259);

pub fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let re = regex::Regex::new(r"(\d+)\.(\d+)\.(\d+)").ok()?;
    let c = re.captures(s)?;
    Some((c[1].parse().ok()?, c[2].parse().ok()?, c[3].parse().ok()?))
}

fn version_matches(pattern: &str, actual: &str) -> bool {
    let p = pattern.trim_end_matches('x').trim_end_matches('.');
    actual.contains(p)
}

pub fn run(cfg: Option<&Effective>, cfg_err: Option<String>, json: bool) -> Result<i32> {
    let mut r = Report::default();
    r.add(
        Level::Info,
        "toryo",
        format!(
            "version {} on {}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS
        ),
    );
    let dd = data_dir();
    match std::fs::create_dir_all(&dd).and_then(|_| std::fs::write(dd.join(".doctor-probe"), b"x"))
    {
        Ok(()) => {
            let _ = std::fs::remove_file(dd.join(".doctor-probe"));
            r.add(Level::Ok, "data dir", format!("{} writable", dd.display()));
        }
        Err(e) => r.add(Level::Fail, "data dir", format!("{}: {e}", dd.display())),
    }

    // Git + GitCmd prerequisites (R33)
    let git = crate::git::git_exe();
    match std::process::Command::new(&git).arg("--version").output() {
        Ok(o) => r.add(
            Level::Ok,
            "git",
            String::from_utf8_lossy(&o.stdout).trim().to_string(),
        ),
        Err(e) => r.add(Level::Fail, "git", format!("{}: {e}", git.display())),
    }
    let gc = GitCmd::new(&git, dd.join("empty-hooks"));
    match &gc {
        Ok(_) => r.add(
            Level::Ok,
            "git",
            format!(
                "empty hooks dir {} exists and is empty (R33)",
                dd.join("empty-hooks").display()
            ),
        ),
        Err(e) => r.add(Level::Fail, "git", format!("{e:#}")),
    }

    let Some(cfg) = cfg else {
        r.add(
            Level::Fail,
            "config",
            cfg_err.unwrap_or_else(|| "toryo.yaml not loaded".into()),
        );
        return finish(r, json);
    };
    r.add(
        Level::Ok,
        "config",
        format!(
            "{} (effective sha256 {})",
            cfg.repo_path.display(),
            &cfg.sha256()[..12]
        ),
    );
    match &cfg.machine_path {
        Some(p) => r.add(
            Level::Ok,
            "config",
            format!("machine config {}", p.display()),
        ),
        None => r.add(
            Level::Info,
            "config",
            "no machine config (%USERPROFILE%\\.toryo\\toryo.local.yaml): no trusted Git filters",
        ),
    }
    verify_root_check(&mut r, cfg);

    // Profiles
    let records = results::load();
    for (name, p) in &cfg.repo.profiles {
        let area = format!("profile {name}");
        let exe = match expand_vars(&p.exe) {
            Ok(e) => PathBuf::from(e),
            Err(e) => {
                r.add(Level::Fail, &area, format!("{e:#}"));
                continue;
            }
        };
        if !exe.is_file() {
            r.add(
                Level::Fail,
                &area,
                format!("exe not found: {}", exe.display()),
            );
            continue;
        }
        let secrets = match resolve_profile_secrets(p) {
            Ok(s) => {
                if !p.secret_env.is_empty() {
                    r.add(Level::Ok, &area, format!("{} secret(s) resolved", s.len()));
                }
                s
            }
            Err(e) => {
                r.add(Level::Fail, &area, format!("secret: {e:#}"));
                BTreeMap::new()
            }
        };
        let env = build_env(std::env::vars(), &p.env, &secrets);
        match validate_provider_selection(name, p.kind, p.billing, &env) {
            Ok(()) => r.add(
                Level::Ok,
                &area,
                format!(
                    "{:?}/{:?}: no provider-selector conflicts (R23)",
                    p.kind, p.billing
                ),
            ),
            Err(e) => r.add(Level::Fail, &area, format!("{e}")),
        }
        let ver = run_version(&exe, build_env(std::env::vars(), &p.env, &BTreeMap::new()));
        let current = ver.as_ref().ok().and_then(|v| results::parse_version(v));
        match ver {
            Ok(v) => {
                let lvl = match &p.version {
                    Some(pat) if !version_matches(pat, &v) => Level::Warn,
                    _ => Level::Ok,
                };
                r.add(
                    lvl,
                    &area,
                    format!(
                        "{} --version → {v}{}",
                        exe.display(),
                        p.version
                            .as_ref()
                            .map(|x| format!(" (pinned {x})"))
                            .unwrap_or_default()
                    ),
                );
                if p.kind == ProfileKind::Claude {
                    if let Some(sv) = parse_semver(&v) {
                        if sv < CLAUDE_MIN_RESTRICTED {
                            r.add(
                                Level::Fail,
                                &area,
                                "Claude Code < 2.1.248: --restricted unavailable (R30)",
                            );
                        } else if sv < CLAUDE_MIN_PERMISSION_PROMPTS {
                            r.add(
                                Level::Fail,
                                &area,
                                "Claude Code < 2.1.259: --permission-prompts unavailable",
                            );
                        }
                    }
                }
            }
            Err(e) => r.add(Level::Fail, &area, format!("--version: {e:#}")),
        }
        match p.kind {
            ProfileKind::Claude => {
                if cfg!(windows) {
                    match p.env.get("CLAUDE_CODE_GIT_BASH_PATH") {
                        Some(b) if Path::new(b).is_file() => {
                            r.add(Level::Ok, &area, format!("Git Bash {b}"))
                        }
                        Some(b) => r.add(
                            Level::Fail,
                            &area,
                            format!("CLAUDE_CODE_GIT_BASH_PATH not found: {b}"),
                        ),
                        None => r.add(
                            Level::Warn,
                            &area,
                            "CLAUDE_CODE_GIT_BASH_PATH not set (Claude on Windows needs Git Bash)",
                        ),
                    }
                }
                if p.billing == Billing::Subscription && p.secret_env.is_empty() {
                    r.add(
                        Level::Info,
                        &area,
                        "subscription auth from the interactive /login of this Windows user",
                    );
                }
                profile_evidence(
                    &mut r,
                    &records,
                    "claude-isolation",
                    name,
                    current.as_deref(),
                    &area,
                    "launch-mode isolation (R44b); run `toryo doctor --canaries`",
                );
            }
            ProfileKind::Codex => {
                let c = p.codex.clone().unwrap_or_default();
                match &c.windows_sandbox {
                    Some(ws) => r.add(Level::Ok, &area, format!("windows.sandbox = {ws} (R18)")),
                    None if cfg!(windows) => r.add(
                        Level::Fail,
                        &area,
                        "codex.windows_sandbox not set: the Windows sandbox is OFF (R18)",
                    ),
                    None => r.add(
                        Level::Warn,
                        &area,
                        "codex.windows_sandbox not set (required on Windows, R18)",
                    ),
                }
                if c.provider.is_none()
                    && !c.overrides.contains_key("model_provider")
                    && p.billing == Billing::Foundry
                {
                    r.add(
                        Level::Fail,
                        &area,
                        "foundry billing but no codex.provider block",
                    );
                }
                profile_evidence(
                    &mut r,
                    &records,
                    "codex-canary",
                    name,
                    current.as_deref(),
                    &area,
                    "sandbox canary (R18/R27); run `toryo doctor --canaries` — profile is UNUSABLE until it passes",
                );
            }
        }
    }
    for (role, rl) in &cfg.repo.roles {
        r.add(
            Level::Info,
            "roles",
            format!(
                "{role} → {}{}",
                rl.profile,
                rl.model
                    .as_ref()
                    .map(|m| format!(" ({m})"))
                    .unwrap_or_default()
            ),
        );
    }

    // Repository (project.root)
    match expand_vars(&cfg.repo.project.root) {
        Ok(root) => repo_checks(&mut r, cfg, Path::new(&root), gc.ok()),
        Err(e) => r.add(Level::Fail, "repo", format!("{e:#}")),
    }

    // Host evidence
    evidence(
        &mut r,
        &records,
        "job-test",
        "",
        "process",
        "Job Object tests (R23); run `toryo spike job-test`",
    );
    evidence(
        &mut r,
        &records,
        "lowil-canary",
        "",
        "verify boundary",
        "Low-IL boundary (R36, M0-VERIFY 9); run `toryo spike lowil-canary`",
    );
    match evidence_status(&records, "git-canary", "", "git", results::git_version().as_deref()) {
        st @ EvidenceStatus::Stale { .. } => r.add(
            Level::Warn,
            "git",
            format!("{} — run `toryo doctor --canaries`", st.describe("git-canary")),
        ),
        _ => evidence(
            &mut r,
            &records,
            "git-canary",
            "",
            "git",
            "hook/filter/textconv canary (R33/R37, M0-VERIFY 10); run `toryo doctor --canaries`",
        ),
    }
    finish(r, json)
}

/// R36 precondition (M0 finding F7): the Low-IL verify boundary needs a `verify_root`
/// where this account may set a Low label (WRITE_OWNER). Probe it with a scratch dir.
fn verify_root_check(r: &mut Report, cfg: &Effective) {
    let area = "verify root";
    let root = match expand_vars(&cfg.repo.verify.verify_root) {
        Ok(v) => PathBuf::from(v),
        Err(e) => return r.add(Level::Fail, area, format!("{e:#}")),
    };
    if !cfg!(windows) {
        return r.add(
            Level::Info,
            area,
            format!("{} (Low label check is Windows-only)", root.display()),
        );
    }
    if cfg.repo.verify.boundary == "none" {
        return r.add(Level::Warn, area, "verify.boundary: none — trusted-code mode, verification is NOT isolated (R36)");
    }
    match crate::engine::verify::probe(cfg) {
        Ok(()) => r.add(
            Level::Ok,
            area,
            format!("{}: Low integrity label can be set (R36/R45)", root.display()),
        ),
        Err(e) => r.add(
            if cfg.repo.verify.boundary == "none" {
                Level::Warn
            } else {
                Level::Fail
            },
            area,
            format!("{e:#}"),
        ),
    }
}

/// R51: a profile's isolation canary must have passed for the CLI version installed now.
#[allow(clippy::too_many_arguments)]
fn profile_evidence(
    r: &mut Report,
    records: &[Value],
    spike: &str,
    profile: &str,
    current: Option<&str>,
    area: &str,
    hint: &str,
) {
    let st = evidence_status(records, spike, profile, profile, current);
    match &st {
        EvidenceStatus::Verified { .. } => r.add(
            Level::Ok,
            area,
            format!("{} for {} (R51)", st.describe(spike), current.unwrap_or("?")),
        ),
        EvidenceStatus::Missing => r.add(Level::Fail, area, format!("no M0 record: {hint}")),
        _ => r.add(
            Level::Fail,
            area,
            format!("{} — run `toryo doctor --canaries`", st.describe(spike)),
        ),
    }
}

fn evidence(r: &mut Report, records: &[Value], spike: &str, profile: &str, area: &str, hint: &str) {
    let last = records.iter().rev().find(|x| {
        x["spike"].as_str() == Some(spike)
            && (profile.is_empty() || x["profile"].as_str() == Some(profile))
    });
    match last {
        None => r.add(
            if spike == "codex-canary" {
                Level::Fail
            } else {
                Level::Warn
            },
            area,
            format!("no M0 record: {hint}"),
        ),
        Some(x) => {
            let lvl = match x["pass"].as_bool() {
                Some(true) => Level::Ok,
                Some(false) => Level::Fail,
                None => Level::Info,
            };
            r.add(
                lvl,
                area,
                format!(
                    "{spike} @ {}: {}",
                    x["ts"].as_str().unwrap_or(""),
                    x["summary"].as_str().unwrap_or("")
                ),
            );
        }
    }
}

fn repo_checks(r: &mut Report, cfg: &Effective, root: &Path, gc: Option<GitCmd>) {
    let area = "repo";
    if !root.join(".git").exists() {
        r.add(
            Level::Fail,
            area,
            format!("{} is not a Git repository", root.display()),
        );
        return;
    }
    r.add(Level::Ok, area, format!("{}", root.display()));
    if let Err(e) = GitCmd::check_submodules(root, cfg.repo.policy.allow_submodules) {
        r.add(Level::Fail, area, format!("{e:#}"));
    }
    if let Some(g) = gc {
        match g.plan_filters(
            root,
            &cfg.trusted_filters,
            cfg.repo.policy.neutralize_untrusted_filters,
        ) {
            Ok(plan) if plan.drivers.is_empty() => r.add(
                Level::Ok,
                area,
                "no filter drivers defined or referenced (R37)",
            ),
            Ok(plan) => r.add(
                if plan.untrusted_referenced.is_empty() {
                    Level::Ok
                } else {
                    Level::Warn
                },
                area,
                format!(
                    "filter drivers: trusted {:?}, neutralised {:?} (R37)",
                    plan.drivers
                        .iter()
                        .filter(|(_, t)| t.is_some())
                        .map(|(n, _)| n)
                        .collect::<Vec<_>>(),
                    plan.drivers
                        .iter()
                        .filter(|(_, t)| t.is_none())
                        .map(|(n, _)| n)
                        .collect::<Vec<_>>()
                ),
            ),
            Err(e) => r.add(Level::Fail, area, format!("{e:#}")),
        }
        if let Ok(keys) = g.run_str(
            &Loc::Discover(root.to_path_buf()),
            true,
            &["config", "--local", "--name-only", "--list"],
        ) {
            let risky: Vec<&str> = keys
                .lines()
                .filter(|k| {
                    [
                        "command",
                        "program",
                        "helper",
                        "driver",
                        "cmd",
                        "editor",
                        "pager",
                        "hook",
                        "textconv",
                        "fsmonitor",
                        "external",
                    ]
                    .iter()
                    .any(|w| k.to_lowercase().contains(w))
                })
                .collect();
            if !risky.is_empty() {
                r.add(Level::Warn, area, format!("repo config keys that name programs (neutralised or never invoked by GitCmd, R37): {risky:?}"));
            }
        }
    }
    let codex_limit = cfg
        .repo
        .profiles
        .values()
        .filter_map(|p| p.codex.as_ref().and_then(|c| c.project_doc_max_bytes))
        .max()
        .unwrap_or(32 * 1024);
    if let Ok(md) = std::fs::metadata(root.join("AGENTS.md")) {
        let lvl = if md.len() > codex_limit {
            Level::Warn
        } else {
            Level::Ok
        };
        r.add(
            lvl,
            area,
            format!(
                "AGENTS.md {} bytes (Codex project_doc_max_bytes {codex_limit}, R20)",
                md.len()
            ),
        );
    }
    for (file, lvl, why) in [
        ("AGENTS.override.md", Level::Warn, "takes precedence in Codex (R20)"),
        ("toryo.local.yaml", Level::Warn, "in-repo machine override is ignored; use %USERPROFILE%\\.toryo\\toryo.local.yaml (R35)"),
        (".mcp.json", Level::Warn, "project MCP config — must have no effect under --safe-mode (R30)"),
        (".claude/settings.json", Level::Warn, "project Claude settings — must have no effect under --restricted (R30)"),
        (".claude/settings.local.json", Level::Warn, "project Claude settings — must have no effect under --restricted (R30)"),
    ] {
        let p = root.join(file);
        if p.exists() {
            let keys = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|v| {
                ["env", "apiKeyHelper", "awsAuthRefresh", "awsCredentialExport", "gcpAuthRefresh", "hooks", "mcpServers"]
                    .iter()
                    .filter(|k| v.get(**k).is_some())
                    .map(|k| k.to_string())
                    .collect::<Vec<_>>()
            });
            r.add(lvl, area, format!("{file} present: {why}{}", keys.map(|k| format!(" — risky keys {k:?}")).unwrap_or_default()));
        }
    }
    if root.join(".codex").join("config.toml").exists() {
        let lvl = if cfg.repo.policy.allow_codex_project_config {
            Level::Warn
        } else {
            Level::Fail
        };
        r.add(lvl, area, ".codex/config.toml present: refused until M0-VERIFY 8 shows it is not loaded (policy.allow_codex_project_config)");
    }
}

fn finish(r: Report, json: bool) -> Result<i32> {
    let worst = r.0.iter().map(|c| c.level).max().unwrap_or(Level::Ok);
    if json {
        println!("{}", serde_json::to_string_pretty(&r.0)?);
    } else {
        for c in &r.0 {
            let tag = match c.level {
                Level::Ok => "OK  ",
                Level::Info => "info",
                Level::Warn => "WARN",
                Level::Fail => "FAIL",
            };
            println!("[{tag}] {:<22} {}", c.area, c.message);
        }
        let fails = r.0.iter().filter(|c| c.level == Level::Fail).count();
        let warns = r.0.iter().filter(|c| c.level == Level::Warn).count();
        println!("\n{fails} FAIL, {warns} WARN");
    }
    Ok(if worst == Level::Fail { 1 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_and_pattern() {
        assert_eq!(parse_semver("2.1.260 (Claude Code)"), Some((2, 1, 260)));
        assert!(parse_semver("2.1.247").unwrap() < CLAUDE_MIN_RESTRICTED);
        assert!(version_matches("2.1.x", "2.1.260 (Claude Code)"));
        assert!(!version_matches("0.15x", "codex-cli 0.16.0"));
        assert!(version_matches("0.15x", "codex-cli 0.150.1"));
    }
}
