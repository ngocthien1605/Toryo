//! `toryo doctor` (plan §15): environment, profiles, repository configuration and the
//! M0 evidence recorded so far. Exit code 1 when any check FAILs.

use crate::git::short;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::config::{
    data_dir, expand_vars, resolve_profile_secrets, Billing, Effective, Origin, ProfileKind,
};
use crate::evidence::{self as results, evidence_status, run_version, EvidenceStatus};
use crate::git::{GitCmd, Loc};
use crate::process::env::{build_env, validate_provider_selection};

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
    /// Where the checked entry came from (`repo`, `machine`, …; R60), for `--json`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Origin>,
}

#[derive(Default)]
struct Report(Vec<Check>);

impl Report {
    fn add(&mut self, level: Level, area: &str, message: impl Into<String>) {
        self.0.push(Check {
            level,
            area: area.into(),
            message: message.into(),
            source: None,
        });
    }
}

/// R60: where each profile, toolchain and role of the effective configuration came from —
/// profiles and toolchains only from the machine config; roles from the repository, the
/// machine, or the repository overridden by the machine. One `info` line each.
pub fn origin_checks(cfg: &Effective) -> Vec<Check> {
    let machine = cfg
        .machine_path
        .as_ref()
        .map(|p| format!(" ({})", p.display()))
        .unwrap_or_default();
    let place = |o: Origin| match o {
        Origin::Repo => format!("the repository config ({})", cfg.repo_path.display()),
        Origin::Machine => format!("the machine config{machine}"),
        Origin::RepoOverriddenByMachine => {
            format!("the repository config, overridden by the machine config{machine}")
        }
    };
    let mut out = Vec::new();
    for name in cfg.repo.profiles.keys() {
        let source = cfg.origins.profiles.get(name).copied();
        out.push(Check {
            level: Level::Info,
            area: format!("profile {name}"),
            message: match source {
                Some(o) => format!("source: {}", place(o)),
                None => "source: frozen configuration".into(),
            },
            source,
        });
    }
    for name in cfg.toolchains.names() {
        let source = cfg.origins.toolchains.get(name).copied();
        out.push(Check {
            level: Level::Info,
            area: format!("toolchain {name}"),
            message: match source {
                Some(o) => format!("source: {}", place(o)),
                None => "source: frozen configuration".into(),
            },
            source,
        });
    }
    for (name, role) in &cfg.repo.roles {
        let source = cfg.origins.roles.get(name).copied();
        out.push(Check {
            level: Level::Info,
            area: format!("role {name}"),
            message: format!(
                "profile {} — source: {}",
                role.profile,
                source
                    .map(place)
                    .unwrap_or_else(|| "frozen configuration".into())
            ),
            source,
        });
    }
    // M11-A2: the verify boundary, the machine's floor and the verify root in force
    for (key, source) in &cfg.origins.verify {
        let value = match key.as_str() {
            "boundary" => cfg.repo.verify.boundary.clone(),
            "min_boundary" => cfg.machine_verify.min_boundary.clone().unwrap_or_default(),
            _ => crate::engine::verify::verify_root(cfg)
                .display()
                .to_string(),
        };
        out.push(Check {
            level: Level::Info,
            area: format!("verify {key}"),
            message: format!("{value} — source: {}", place(*source)),
            source: Some(*source),
        });
    }
    out
}

/// M11-R4: a WARN for each `policy.deny_paths` / `protected_paths` glob that can never match a
/// repository path (`./x/**`, `x/`, `/x/**`, `{a,b}` …). They keep their behaviour — and the
/// config its hash; the org chart's `writes` / `never` are normalised or refused at load.
pub fn policy_checks(cfg: &Effective) -> Vec<Check> {
    let p = &cfg.repo.policy;
    let root = expand_vars(&cfg.repo.project.root).map(PathBuf::from).ok();
    // (R4–R8 review) a pattern without a wildcard that names a folder of the repository
    // matches the folder entry only, none of the files in it
    let folder = |g: &str| {
        let t = g.trim().replace('\\', "/");
        let t = t.trim_end_matches('/');
        (!t.is_empty()
            && !t.contains(['*', '?'])
            && t != crate::engine::checks::VERIFY_SCRIPTS
            && root.as_ref().is_some_and(|r| r.join(t).is_dir()))
        .then(|| {
            format!("names the folder `{t}`: as written it matches the folder entry only, none of the files in it — write `{t}/**`")
        })
    };
    [
        ("deny_paths", &p.deny_paths),
        ("protected_paths", &p.protected_paths),
    ]
    .into_iter()
    .flat_map(|(key, list)| list.iter().map(move |g| (key, g)))
    .flat_map(|(key, g)| {
        let dead = crate::engine::checks::dead_pattern(g)
            .or_else(|| folder(g))
            .map(|why| format!("`{g}` {why} (this pattern protects nothing)"));
        // M11-R17: glob syntax Toryo does not have is matched literally — say so (the
        // behaviour is unchanged)
        let literal = crate::engine::checks::literal_pattern(g).map(|why| format!("`{g}` {why}"));
        dead.into_iter().chain(literal).map(move |what| Check {
            level: Level::Warn,
            area: "policy".into(),
            message: format!("{}: policy.{key}: {what}", cfg.repo_path.display()),
            source: None,
        })
    })
    .collect()
}

/// M12-05: one line per gate — can it start on this machine? A check gate's program (a bare
/// name on the PATH, an absolute path that exists) and an environment gate's program and
/// script (`-File`, an absolute script path). FAIL with the way out; a gate that can start is
/// an OK line. A repository path is not asked here (the M18 rules).
pub fn gate_checks(cfg: &Effective) -> Vec<Check> {
    use crate::engine::gates;
    let path = std::env::var_os("PATH");
    let root = expand_vars(&cfg.repo.project.root).map(PathBuf::from).ok();
    let mut out = Vec::new();
    let mut add = |level: Level, message: String| {
        out.push(Check {
            level,
            area: "gates".into(),
            message,
            source: None,
        })
    };
    let cmds = root
        .as_deref()
        .and_then(|r| crate::engine::checks::verify_commands(cfg, r).ok())
        .unwrap_or_else(|| cfg.repo.commands.verify.clone());
    for c in &cmds {
        if !matches!(c.kind(), crate::config::CommandKind::Program { .. }) {
            continue;
        }
        match gates::check_gate_problem(c, path.as_deref()) {
            Some(why) => add(
                Level::Fail,
                format!(
                    "check gate `{}`: {why} — install it or put it on the PATH, or change `commands.verify` / the gate in the org chart; a run would stop with `{}` before any gate ran",
                    c.name,
                    gates::GATE_UNAVAILABLE
                ),
            ),
            None => add(Level::Ok, format!("check gate `{}` can start here", c.name)),
        }
    }
    if let Some(team) = &cfg.team {
        for (name, g) in team.handoff_gates() {
            if g.decl.kind != crate::team::GateType::Environment {
                continue;
            }
            let Some(m) = &g.machine else { continue };
            let mut problems: Vec<String> = Vec::new();
            if let Err(e) =
                crate::engine::runner::env_gate_program(cfg, &m.program, path.as_deref())
            {
                problems.push(e);
            }
            problems.extend(gates::script_problems(&m.args));
            if problems.is_empty() {
                add(
                    Level::Ok,
                    format!(
                        "environment gate `{name}` can start here (confirm: {})",
                        m.confirm.as_str()
                    ),
                );
            } else {
                add(
                    Level::Fail,
                    format!(
                        "environment gate `{name}`: {} — fix `environment_gates.{name}` in the machine config; `task create` refuses it and a run would stop with `{}` before any gate ran",
                        problems.join("; "),
                        crate::engine::runner::ENV_UNAVAILABLE
                    ),
                );
            }
        }
    }
    out
}

/// M11-R8: the machine's `skills_dir` (org chart mode) — a WARN when the folder does not
/// exist: a skill only there resolves nowhere, and the turn that needs it escalates
/// (`skill_unavailable`).
pub fn skills_dir_checks(cfg: &Effective) -> Vec<Check> {
    let Some(dir) = cfg
        .team
        .as_ref()
        .and_then(|t| crate::team::machine_skills_dir(&t.skill_roots))
    else {
        return Vec::new();
    };
    let (level, message) = if dir.is_dir() {
        (Level::Ok, format!("{} exists", dir.display()))
    } else {
        (
            Level::Warn,
            format!(
                "{} does not exist — a skill found only there cannot be delivered (the turn escalates with skill_unavailable)",
                dir.display()
            ),
        )
    };
    vec![Check {
        level,
        area: "skills_dir".into(),
        message,
        source: None,
    }]
}

/// Minimum Claude Code versions for flags the plan relies on (§14).
const CLAUDE_MIN_RESTRICTED: (u64, u64, u64) = (2, 1, 248);
const CLAUDE_MIN_PERMISSION_PROMPTS: (u64, u64, u64) = (2, 1, 259);

pub fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let re = regex::Regex::new(r"(\d+)\.(\d+)\.(\d+)").ok()?;
    let c = re.captures(s)?;
    Some((c[1].parse().ok()?, c[2].parse().ok()?, c[3].parse().ok()?))
}

/// A profile's `version:` pin (plan §14: `"2.1.x"`, `"0.15x"`) against a `--version` output,
/// component by component (EC-27: never a substring test). The output's version is the first
/// `x.y.z` in it ([`results::parse_version`]); its numeric components are compared in order
/// with the pin's: a number must be equal, `x` matches any number, and a number followed by
/// `x` any number that starts with those digits (`15x`: 15, 150–159, …). Components the pin
/// leaves out are not constrained; an empty pin matches everything.
fn version_matches(pattern: &str, actual: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return true;
    }
    let Some(v) = results::parse_version(actual) else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let got: Vec<&str> = v.split(['.', '-', '+']).take_while(|c| digits(c)).collect();
    let pin: Vec<&str> = pattern.split('.').collect();
    pin.len() <= got.len()
        && pin
            .iter()
            .zip(&got)
            .all(|(p, g)| match p.strip_suffix('x') {
                Some(prefix) => prefix.bytes().all(|b| b.is_ascii_digit()) && g.starts_with(prefix),
                None => digits(p) && p.trim_start_matches('0') == g.trim_start_matches('0'),
            })
}

/// M12-17: which repository config file is in use and why (M12-09), and whether another
/// file `$TORYO_CONFIG` names is being ignored in this folder.
pub fn config_source_checks(cfg: &Effective, source: &str, variable: Option<&Path>) -> Vec<Check> {
    let mut out = vec![Check {
        level: Level::Info,
        area: "config".into(),
        message: format!(
            "{} is in use — chosen by {source}; `--config <file>` chooses another",
            cfg.repo_path.display()
        ),
        source: None,
    }];
    if let Some(v) = variable.filter(|v| v.is_file()) {
        let same = match (
            std::fs::canonicalize(&cfg.repo_path),
            std::fs::canonicalize(v),
        ) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        };
        if !same {
            out.push(Check {
                level: Level::Warn,
                area: "config".into(),
                message: format!(
                    "$TORYO_CONFIG names {}, which is not the file in use and is ignored here — a `task create` in this folder uses {}; remove the variable from your profile or pass `--config`",
                    v.display(),
                    cfg.repo_path.display()
                ),
                source: None,
            });
        }
    }
    out
}

/// M12-17: the base branch of `task create` (M12-08) — `project.base_branch` exists in the
/// repository, or says which branch a task would start from.
pub fn base_branch_checks(cfg: &Effective, g: &GitCmd) -> Vec<Check> {
    let area = "base branch";
    let check = |level: Level, message: String| Check {
        level,
        area: area.into(),
        message,
        source: None,
    };
    let Ok(root) = expand_vars(&cfg.repo.project.root) else {
        return Vec::new();
    };
    let loc = Loc::Discover(PathBuf::from(&root));
    let checked_out = g
        .run_str(&loc, true, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string());
    match (&cfg.repo.project.base_branch, checked_out) {
        (Some(b), _) => {
            let found = g.output(
                &loc,
                true,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{b}^{{commit}}"),
                ],
            );
            match found {
                Ok(o) if o.status.success() => vec![check(
                    Level::Ok,
                    format!("project.base_branch `{b}` exists in {root}"),
                )],
                _ => vec![check(
                    Level::Fail,
                    format!(
                        "project.base_branch: the branch `{b}` does not exist in {root} — create it, or fix the key in {} (or remove it to start from the checked-out branch); `task create` refuses it",
                        cfg.repo_path.display()
                    ),
                )],
            }
        }
        (None, Ok(now)) => vec![check(
            Level::Info,
            format!(
                "no project.base_branch: `task create` starts from the checked-out branch (`{now}`) — set the key to start from a fixed branch whatever is checked out"
            ),
        )],
        (None, Err(e)) => vec![check(
            Level::Warn,
            format!("{root} is not a Git repository Toryo can read ({e:#}) — `task create` needs one"),
        )],
    }
}

/// The file extensions of the text files under `.toryo/` (the org chart, the roles, the skills,
/// the context).
const TEXT_EXTENSIONS: [&str; 6] = ["md", "yaml", "yml", "txt", "json", "toml"];

/// M12-17 (M12-10): every text file under `<project.root>/.toryo/` and the machine config are
/// UTF-8 — a byte-order mark is fine, UTF-16 and code-page files are named.
pub fn encoding_checks(cfg: &Effective) -> Vec<Check> {
    let area = "encoding";
    let mut out = Vec::new();
    let mut bad = 0usize;
    let mut seen = 0usize;
    let root = expand_vars(&cfg.repo.project.root)
        .map(|r| PathBuf::from(r).join(".toryo"))
        .ok();
    let mut stack: Vec<PathBuf> = root.into_iter().filter(|d| d.is_dir()).collect();
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let text = p
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| TEXT_EXTENSIONS.contains(&x.to_ascii_lowercase().as_str()));
            if !text {
                continue;
            }
            seen += 1;
            if let Err(e) = crate::textfile::read_text(&p) {
                bad += 1;
                out.push(Check {
                    level: Level::Fail,
                    area: area.into(),
                    message: format!("{e:#} — a turn that reads it would stop with `context_unreadable`; `task create` refuses an org chart, role file or skill that is not UTF-8"),
                    source: None,
                });
            }
        }
    }
    if seen > 0 && bad == 0 {
        out.push(Check {
            level: Level::Ok,
            area: area.into(),
            message: format!(
                "{seen} text file(s) under .toryo/ are UTF-8 (a byte-order mark is ignored)"
            ),
            source: None,
        });
    }
    if let Some(m) = &cfg.machine_path {
        out.push(match crate::textfile::read_text(m) {
            Ok(_) => Check {
                level: Level::Ok,
                area: area.into(),
                message: format!("machine config {} is UTF-8", m.display()),
                source: None,
            },
            Err(e) => Check {
                level: Level::Fail,
                area: area.into(),
                message: format!("{e:#}"),
                source: None,
            },
        });
    }
    out
}

/// M12-17: Toryo runs with an administrator token. The agents and the Low-integrity gates get
/// a copy of that token.
pub fn elevation_check(elevated: bool) -> Check {
    if elevated {
        Check {
            level: Level::Warn,
            area: "token".into(),
            message: "Toryo runs elevated: agents and the Low-integrity gates get an administrator token — start Toryo from a normal shell and elevate inside the environment gate script".into(),
            source: None,
        }
    } else {
        Check {
            level: Level::Ok,
            area: "token".into(),
            message: "Toryo runs with a normal (not elevated) token".into(),
            source: None,
        }
    }
}

/// Whether this process has an elevated token (Windows; `false` elsewhere).
pub fn is_elevated() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::Security::{
            GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        // SAFETY: the token handle is opened and closed here; the output buffer is a
        // TOKEN_ELEVATION of the size passed
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return false;
            }
            let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
            let mut len = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                &mut e as *mut TOKEN_ELEVATION as *mut _,
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            );
            CloseHandle(token);
            ok != 0 && e.TokenIsElevated != 0
        }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// M12-17: a profile `env:` value that contains `%NAME%` — Toryo passes it as written; only
/// the `exe` of a profile is expanded. (`CODEX_HOME: '%USERPROFILE%...'` was frozen literally
/// and Codex started without its login.)
pub fn profile_env_checks(cfg: &Effective) -> Vec<Check> {
    static VAR: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"%[A-Za-z_][A-Za-z0-9_]*%").expect("regex"));
    let mut out = Vec::new();
    for (name, p) in &cfg.repo.profiles {
        for (k, v) in &p.env {
            if let Some(m) = VAR.find(v) {
                out.push(Check {
                    level: Level::Warn,
                    area: format!("profile {name}"),
                    message: format!(
                        "env {k} contains {} — not expanded here: the value is passed to the agent as written; write the absolute path in the machine config",
                        m.as_str()
                    ),
                    source: None,
                });
            }
        }
    }
    out
}

pub fn run(
    cfg: Option<&Effective>,
    cfg_err: Option<String>,
    json: bool,
    config_source: &str,
    config_variable: Option<&Path>,
) -> Result<i32> {
    let mut r = Report::default();
    r.add(
        Level::Info,
        "toryo",
        format!(
            "version {} ({}) on {}",
            env!("CARGO_PKG_VERSION"),
            env!("TORYO_COMMIT"),
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
            crate::process::decode_output(&o.stdout).trim().to_string(),
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
            short(&cfg.sha256())
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
    path_length_check(&mut r, cfg);
    r.0.extend(origin_checks(cfg));
    r.0.extend(policy_checks(cfg));
    // M12-08 (X20): keys that are accepted and do nothing
    for key in cfg.keys_without_effect() {
        r.add(Level::Warn, "config", Effective::no_effect_line(key));
    }
    r.0.extend(config_source_checks(cfg, config_source, config_variable));
    if let Ok(g) = &gc {
        r.0.extend(base_branch_checks(cfg, g));
    }
    r.0.extend(encoding_checks(cfg));
    if cfg!(windows) {
        r.0.push(elevation_check(is_elevated()));
    }
    r.0.extend(profile_env_checks(cfg));
    r.0.extend(gate_checks(cfg));
    r.0.extend(skills_dir_checks(cfg));

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
        let ver = run_version(
            &exe,
            build_env(std::env::vars(), &p.env, &BTreeMap::new()),
            &results::profile_redactions(p),
        );
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
        Ok(root) => {
            repo_checks(&mut r, cfg, Path::new(&root), gc.ok());
            r.0.extend(xpp_checks(cfg, Path::new(&root), &vswhere_msbuild));
        }
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
    match evidence_status(
        &records,
        "git-canary",
        "",
        "git",
        results::git_version().as_deref(),
    ) {
        st @ EvidenceStatus::Stale { .. } => r.add(
            Level::Warn,
            "git",
            format!(
                "{} — run `toryo doctor --canaries`",
                st.describe("git-canary")
            ),
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

/// Above this many characters a root leaves too little of the 260 a Windows tool allows for the
/// repository's own paths (D365FO metadata paths reach 200).
const LONG_ROOT: usize = 70;

/// M12-13: the lengths of `worktrees_root` and `verify_root` — every file of a task lives
/// under `<worktrees_root>\<task id>\` and its verify checkout under `<verify_root>\…`, which
/// adds about 60 characters. Toryo's own Git calls allow long paths on Windows; the build tools
/// and the agents' tools may not.
fn path_length_check(r: &mut Report, cfg: &Effective) {
    let area = "paths";
    let (Ok(wt), Ok(vr)) = (
        expand_vars(&cfg.repo.project.worktrees_root),
        expand_vars(crate::engine::verify::verify_root_setting(cfg)),
    ) else {
        return;
    };
    let (w, v) = (wt.chars().count(), vr.chars().count());
    let long = w.max(v) > LONG_ROOT;
    r.add(
        if long && cfg!(windows) { Level::Warn } else { Level::Info },
        area,
        format!(
            "worktrees_root is {w} characters ({wt}), verify_root is {v} ({vr}); every file of a task sits under them and Windows tools stop at 260 characters — {}",
            if long {
                r"keep them short (for example D:\w and D:\v): a root of more than 70 characters leaves too little room for D365FO metadata paths"
            } else {
                "short enough"
            }
        ),
    );
}

/// R36 precondition (M0 finding F7): the Low-IL verify boundary needs a `verify_root`
/// where this account may set a Low label (WRITE_OWNER). Probe it with a scratch dir.
fn verify_root_check(r: &mut Report, cfg: &Effective) {
    let area = "verify root";
    // M11-A2: the machine's `verify.verify_root` replaces the repository's
    let root = match expand_vars(crate::engine::verify::verify_root_setting(cfg)) {
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
        // M11-A2: say when `task create` will refuse it
        let floor = match crate::engine::verify::below_floor(cfg) {
            Some(_) => " — below this machine's floor `low_integrity`: `task create` refuses it (the machine config may allow it with `verify: { min_boundary: none }`)",
            None => "",
        };
        return r.add(
            Level::Warn,
            area,
            format!(
                "verify.boundary: none — trusted-code mode, verification is NOT isolated (R36){floor}"
            ),
        );
    }
    match crate::engine::verify::probe(cfg) {
        Ok(()) => r.add(
            Level::Ok,
            area,
            format!(
                "{}: Low integrity label can be set (R36/R45); not writable at Low integrity itself (M11-R13)",
                root.display()
            ),
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
            format!(
                "{} for {} (R51)",
                st.describe(spike),
                current.unwrap_or("?")
            ),
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
        // M11-R10: M18 at HEAD — a missing entry script (the refusal `task create` would give)
        // and the notes for other arguments the commit lacks
        match crate::engine::task::entry_point_notes(cfg, &g, root, "HEAD") {
            Ok(notes) => {
                for n in notes {
                    r.add(Level::Info, "M18", format!("note: {n}"));
                }
            }
            Err(e) => r.add(Level::Warn, "M18", format!("{e:#}")),
        }
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

/// The MSBuild that `vswhere` finds (the latest Visual Studio with the MSBuild component), or
/// None — `doctor` proposes it, never writes it (R59).
fn vswhere_msbuild() -> Option<PathBuf> {
    let pf = std::env::var_os("ProgramFiles(x86)")?;
    let vswhere = Path::new(&pf)
        .join("Microsoft Visual Studio")
        .join("Installer")
        .join("vswhere.exe");
    if !vswhere.is_file() {
        return None;
    }
    let out = std::process::Command::new(vswhere)
        .args([
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.Component.MSBuild",
            "-find",
            r"MSBuild\**\Bin\MSBuild.exe",
        ])
        .output()
        .ok()?;
    crate::process::decode_output(&out.stdout)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(PathBuf::from)
}

/// Every `x.y.z.w` value of the UDE's `bin\InstalledVersion.json`, read tolerantly: the file's
/// keys are not documented, so any four-part version counts.
pub fn four_part_versions(text: &str) -> Vec<String> {
    let re = regex::Regex::new(r"\b\d+\.\d+\.\d+\.\d+\b").expect("version regex");
    let mut v: Vec<String> = Vec::new();
    for m in re.find_iter(text) {
        if !v.iter().any(|x| x == m.as_str()) {
            v.push(m.as_str().to_string());
        }
    }
    v
}

/// R59: the X++ toolchain of a configuration with a `tool: xpp` verify command — MSBuild
/// (with the path `vswhere` finds as a proposal when it is missing), the five CI/CD packages and
/// their versions, the UDE metadata version against the platform packages', the extra
/// references, and each command's metadata folder and projects in the repository (`root`).
/// Nothing when no verify command is `tool: xpp`.
pub fn xpp_checks(
    cfg: &Effective,
    root: &Path,
    vswhere: &dyn Fn() -> Option<PathBuf>,
) -> Vec<Check> {
    use crate::config::CommandKind;
    use crate::engine::xpp;
    let mut r = Report::default();
    let area = "xpp";
    let cmds: Vec<_> = cfg
        .repo
        .commands
        .verify
        .iter()
        .filter(|c| c.is_xpp())
        .collect();
    if cmds.is_empty() {
        return r.0;
    }
    let Some(x) = &cfg.toolchains.xpp else {
        r.add(
            Level::Fail,
            area,
            "a `tool: xpp` verify command but no toolchains.xpp in the machine config (R59)",
        );
        return r.0;
    };
    let machine = cfg
        .machine_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "the machine config".into());
    match expand_vars(&x.msbuild).map(PathBuf::from) {
        Ok(m) if m.is_file() => r.add(Level::Ok, area, format!("MSBuild {}", m.display())),
        Ok(m) => {
            let proposal = match vswhere() {
                Some(p) => format!(" — vswhere finds {}: set toolchains.xpp.msbuild to it in {machine} (toryo never changes the file)", p.display()),
                None => " — vswhere finds none: install the MSBuild component of Visual Studio 2022".into(),
            };
            r.add(
                Level::Fail,
                area,
                format!("MSBuild not found: {}{proposal}", m.display()),
            );
        }
        Err(e) => r.add(Level::Fail, area, format!("toolchains.xpp.msbuild: {e:#}")),
    }
    let packages = expand_vars(&x.nugets).and_then(|n| xpp::resolve_packages(Path::new(&n)));
    let platform = match packages {
        Ok(p) => {
            let short = |id: &str| {
                id.trim_start_matches("Microsoft.Dynamics.AX.")
                    .trim_end_matches(".DevALM.BuildXpp")
                    .to_string()
            };
            r.add(
                Level::Ok,
                area,
                format!(
                    "CI/CD packages: {}",
                    p.iter()
                        .map(|k| format!(
                            "{} {}",
                            short(&k.id),
                            k.version.as_deref().unwrap_or("(no version)")
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
            p[0].version.clone().or_else(|| p[1].version.clone())
        }
        Err(e) => {
            r.add(Level::Fail, area, format!("{e:#}"));
            None
        }
    };
    if let Some(ude) = &x.ude_metadata {
        let file =
            expand_vars(ude).map(|u| Path::new(&u).join("bin").join("InstalledVersion.json"));
        let read = file
            .as_ref()
            .map_err(|e| format!("{e:#}"))
            .and_then(|f| std::fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display())));
        match (read.map(|t| four_part_versions(&t)), platform) {
            (Err(e), _) => r.add(Level::Info, area, format!("UDE version not read ({e}): no version check")),
            (Ok(v), _) if v.is_empty() => r.add(Level::Info, area, "UDE InstalledVersion.json holds no x.y.z.w version: no version check"),
            (Ok(_), None) => r.add(Level::Info, area, "the platform packages have no version: no version check"),
            (Ok(v), Some(p)) if v.contains(&p) => r.add(Level::Ok, area, format!("UDE metadata version {} = the platform packages' {p}", v.join(", "))),
            (Ok(v), Some(p)) => r.add(Level::Warn, area, format!("UDE metadata version {} ≠ the platform packages' {p} — Microsoft's pages disagree on whether other NuGet versions are safe; prefer the NuGets of the release the UDE runs", v.join(", "))),
        }
    }
    for e in &x.extra_references {
        match expand_vars(e).map(PathBuf::from) {
            Ok(p) if p.is_dir() => {
                r.add(Level::Ok, area, format!("extra reference {}", p.display()))
            }
            Ok(p) => r.add(
                Level::Warn,
                area,
                format!("extra reference not found: {}", p.display()),
            ),
            Err(err) => r.add(
                Level::Fail,
                area,
                format!("toolchains.xpp.extra_references: {err:#}"),
            ),
        }
    }
    for c in cmds {
        let CommandKind::Xpp { metadata, projects } = c.kind() else {
            continue;
        };
        let meta = xpp::join_rel(root, metadata);
        if meta.is_dir() {
            r.add(
                Level::Ok,
                area,
                format!("{}: metadata {}", c.name, meta.display()),
            );
        } else {
            r.add(
                Level::Fail,
                area,
                format!("{}: metadata folder not found: {}", c.name, meta.display()),
            );
        }
        for p in projects {
            let f = xpp::join_rel(root, p);
            if f.is_file() {
                r.add(Level::Ok, area, format!("{}: project {p}", c.name));
            } else {
                r.add(
                    Level::Fail,
                    area,
                    format!(
                        "{}: project not found in the repository: {}",
                        c.name,
                        f.display()
                    ),
                );
            }
        }
    }
    r.0
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

    /// M11-A2 (group A review): the verify-root check probes the root in force — the machine's
    /// — not the repository's, which may not even expand on this machine.
    #[test]
    fn the_verify_root_check_uses_the_machine_root() {
        let d = tempfile::tempdir().unwrap();
        let (p, m) = (d.path().join("toryo.yaml"), d.path().join("machine.yaml"));
        std::fs::write(
            &p,
            "version: 1\nproject: { root: r, worktrees_root: w }\nverify: { verify_root: '%TORYO_TEST_NEVER_SET%\\vr' }\n",
        )
        .unwrap();
        let mvr = d.path().join("machine-vr");
        std::fs::write(
            &m,
            format!(
                "verify: {{ verify_root: '{}' }}\n",
                mvr.display().to_string().replace('\\', "/")
            ),
        )
        .unwrap();
        let cfg = Effective::load(&p, Some(&m)).unwrap();
        let mut r = Report::default();
        verify_root_check(&mut r, &cfg);
        let c = &r.0[0];
        assert!(
            !c.message.contains("TORYO_TEST_NEVER_SET"),
            "{:?}: {}",
            c.level,
            c.message
        );
        // a boundary below the floor says that `task create` refuses it
        std::fs::write(
            &p,
            "version: 1\nproject: { root: r, worktrees_root: w }\nverify: { boundary: none }\n",
        )
        .unwrap();
        std::fs::write(&m, "").unwrap();
        let cfg = Effective::load(&p, Some(&m)).unwrap();
        let mut r = Report::default();
        verify_root_check(&mut r, &cfg);
        if cfg!(windows) {
            assert!(
                r.0[0].message.contains("`task create` refuses it"),
                "{}",
                r.0[0].message
            );
        }
    }

    /// M11-R4 AC-3: a legacy `deny_paths` / `protected_paths` glob that can never match is a
    /// WARN; the pattern is kept as written, so the configuration's hash does not change.
    #[test]
    fn a_legacy_pattern_that_never_matches_is_a_warning() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("toryo.yaml");
        let base = "version: 1\nproject: { root: r, worktrees_root: w }\n";
        std::fs::write(
            &p,
            format!("{base}policy: {{ deny_paths: ['./x/**', '**/ZZZ/**'], protected_paths: ['@verify-scripts', 'cfg/'] }}\n"),
        )
        .unwrap();
        let cfg = Effective::load(&p, None).unwrap();
        let w: Vec<String> = policy_checks(&cfg)
            .into_iter()
            .inspect(|c| assert_eq!(c.level, Level::Warn))
            .map(|c| c.message)
            .collect();
        assert_eq!(w.len(), 2, "{w:#?}");
        assert!(
            w[0].contains("policy.deny_paths: `./x/**` never matches")
                && w[0].contains("write `x/**`"),
            "{w:#?}"
        );
        assert!(w[1].contains("policy.protected_paths: `cfg/`"), "{w:#?}");
        // kept as written: what is hashed is what was hashed before M11-R4
        assert_eq!(cfg.repo.policy.deny_paths, ["./x/**", "**/ZZZ/**"]);
        assert!(cfg
            .canonical_json()
            .contains(r#""deny_paths":["./x/**","**/ZZZ/**"]"#));
        // a clean policy: no warning
        std::fs::write(&p, format!("{base}policy: {{ deny_paths: ['x/**'] }}\n")).unwrap();
        assert!(policy_checks(&Effective::load(&p, None).unwrap()).is_empty());
        // M11-R17: braces, a class, a negation — matched literally, and `doctor` says so
        std::fs::write(
            &p,
            format!("{base}policy: {{ deny_paths: ['**/*.{{pfx,key}}', 'x/**'], protected_paths: ['secrets/[ab].pfx', '!keep'] }}\n"),
        )
        .unwrap();
        let cfg = Effective::load(&p, None).unwrap();
        let w: Vec<String> = policy_checks(&cfg)
            .into_iter()
            .inspect(|c| assert_eq!(c.level, Level::Warn))
            .map(|c| c.message)
            .collect();
        assert_eq!(w.len(), 3, "{w:#?}");
        assert!(
            w[0].contains("policy.deny_paths: `**/*.{pfx,key}` uses `{` `}` — matched literally: Toryo patterns have no braces, classes or negation")
                && !w[0].contains("protects nothing"),
            "{w:#?}"
        );
        assert!(
            w[1].contains("policy.protected_paths: `secrets/[ab].pfx` uses `[` `]`")
                && w[2].contains("`!keep` uses `!`"),
            "{w:#?}"
        );
        // behaviour unchanged: the pattern is kept as written and matches its own characters
        assert_eq!(cfg.repo.policy.deny_paths, ["**/*.{pfx,key}", "x/**"]);
        // R4–R8 review: `secrets` naming a folder of the repository matches none of its files
        let repo = d.path().join("repo");
        std::fs::create_dir_all(repo.join("secrets")).unwrap();
        let root = repo.display().to_string().replace('\\', "/");
        std::fs::write(
            &p,
            format!("version: 1\nproject: {{ root: '{root}', worktrees_root: w }}\npolicy: {{ deny_paths: [secrets, notes.md] }}\n"),
        )
        .unwrap();
        let w = policy_checks(&Effective::load(&p, None).unwrap());
        assert_eq!(
            w.len(),
            1,
            "{:?}",
            w.iter().map(|c| &c.message).collect::<Vec<_>>()
        );
        assert!(
            w[0].message
                .contains("policy.deny_paths: `secrets` names the folder `secrets`")
                && w[0].message.contains("write `secrets/**`"),
            "{}",
            w[0].message
        );
    }

    /// R60 (M9-A2, B1): each profile, toolchain and role says where it came from, in text and in `--json`;
    /// the config hash does not depend on it.
    #[test]
    fn profiles_toolchains_and_roles_name_their_source() {
        let d = tempfile::tempdir().unwrap();
        let (p, m) = (d.path().join("toryo.yaml"), d.path().join("machine.yaml"));
        std::fs::write(
            &p,
            "version: 1\nproject: { root: r, worktrees_root: w }\nroles:\n  planner: { profile: a }\n  reviewer: { profile: a }\n",
        )
        .unwrap();
        std::fs::write(
            &m,
            "profiles:\n  a: { kind: claude, billing: subscription, exe: c }\n  b: { kind: codex, billing: subscription, exe: x }\nroles:\n  reviewer: { profile: b }\n  implementer: { profile: b }\ntoolchains:\n  xpp: { msbuild: m, nugets: n }\n",
        )
        .unwrap();
        let cfg = Effective::load(&p, Some(&m)).unwrap();
        let checks = origin_checks(&cfg);
        let get = |area: &str| checks.iter().find(|c| c.area == area).unwrap();
        assert_eq!(get("profile a").source, Some(Origin::Machine));
        assert_eq!(get("toolchain xpp").source, Some(Origin::Machine));
        assert!(get("toolchain xpp")
            .message
            .starts_with("source: the machine config"));
        assert!(get("profile b").message.contains(&m.display().to_string()));
        assert_eq!(get("role planner").source, Some(Origin::Repo));
        assert_eq!(get("role implementer").source, Some(Origin::Machine));
        let r = get("role reviewer");
        assert_eq!(r.source, Some(Origin::RepoOverriddenByMachine));
        assert!(
            r.message.starts_with(
                "profile b — source: the repository config, overridden by the machine config"
            ),
            "{}",
            r.message
        );
        let json = serde_json::to_value(r).unwrap();
        assert_eq!(json["source"], "repo_overridden_by_machine");
        // not hashed: the frozen copy has no origins and the same digest
        let f = Effective::from_frozen(&cfg.canonical_json(), &p).unwrap();
        assert_eq!(f.sha256(), cfg.sha256());
        let frozen = origin_checks(&f);
        assert!(frozen.iter().all(|c| c.source.is_none()));
        assert!(frozen[0].message.contains("frozen configuration"));
        // a check without a source keeps the old JSON shape
        let mut rep = Report::default();
        rep.add(Level::Ok, "x", "y");
        assert!(serde_json::to_value(&rep.0[0])
            .unwrap()
            .get("source")
            .is_none());
    }

    /// A repository with `Metadata/` and one build project, the five packages (version
    /// 7.0.7198.40), an MSBuild file and a UDE metadata folder whose InstalledVersion.json is
    /// `installed` (none when `None`). Returns the loaded configuration and the repo root.
    fn xpp_fixture(d: &Path, installed: Option<&str>) -> (Effective, PathBuf, PathBuf) {
        let fwd = |p: &Path| p.display().to_string().replace('\\', "/");
        let repo = d.join("repo");
        std::fs::create_dir_all(repo.join("Metadata")).unwrap();
        std::fs::create_dir_all(repo.join("Projects").join("Build")).unwrap();
        std::fs::write(
            repo.join("Projects/Build/Core.Build.rnrproj"),
            "<Project />",
        )
        .unwrap();
        let nugets = d.join("nugets");
        for id in crate::engine::xpp::PACKAGES {
            let dir = nugets.join(format!("{id}.7.0.7198.40"));
            std::fs::create_dir_all(dir.join(crate::engine::xpp::marker(id))).unwrap();
        }
        let msbuild = d.join("MSBuild.exe");
        std::fs::write(&msbuild, "").unwrap();
        let ude = d.join("PackagesLocalDirectory");
        std::fs::create_dir_all(ude.join("bin")).unwrap();
        if let Some(text) = installed {
            std::fs::write(ude.join("bin").join("InstalledVersion.json"), text).unwrap();
        }
        let (p, m) = (d.join("toryo.yaml"), d.join("machine.yaml"));
        std::fs::write(&p, format!("version: 1\nproject: {{ root: \"{}\", worktrees_root: w }}\nroles:\n  planner: {{ profile: a }}\ncommands:\n  verify:\n    - {{ name: build, tool: xpp, metadata: Metadata, projects: [Projects/Build/Core.Build.rnrproj] }}\n", fwd(&repo))).unwrap();
        std::fs::write(&m, format!("profiles:\n  a: {{ kind: claude, billing: subscription, exe: c }}\ntoolchains:\n  xpp:\n    msbuild: \"{}\"\n    nugets: \"{}\"\n    ude_metadata: \"{}\"\n", fwd(&msbuild), fwd(&nugets), fwd(&ude))).unwrap();
        (Effective::load(&p, Some(&m)).unwrap(), repo, msbuild)
    }

    fn lines(checks: &[Check]) -> Vec<String> {
        checks
            .iter()
            .map(|c| format!("{:?} {}", c.level, c.message))
            .collect()
    }

    /// M9-B4 AC-1: each branch of the X++ checks, on temp folders, with a mocked vswhere.
    #[test]
    fn xpp_checks_cover_msbuild_packages_versions_and_projects() {
        let none = || None;
        // found, versions equal
        let d = tempfile::tempdir().unwrap();
        let (cfg, repo, msbuild) = xpp_fixture(
            d.path(),
            Some(r#"{"InstalledVersion":"7.0.7198.40","App":"10.0.2015.16"}"#),
        );
        let l = lines(&xpp_checks(&cfg, &repo, &none));
        assert!(l.iter().all(|x| x.starts_with("Ok ")), "{l:#?}");
        assert!(l.iter().any(|x| x.contains("CI/CD packages: Platform.CompilerPackage 7.0.7198.40, Platform 7.0.7198.40, Application1 7.0.7198.40")), "{l:#?}");
        assert!(l.iter().any(|x| x.contains("UDE metadata version 7.0.7198.40, 10.0.2015.16 = the platform packages' 7.0.7198.40")), "{l:#?}");
        assert!(l
            .iter()
            .any(|x| x.contains("build: project Projects/Build/Core.Build.rnrproj")));

        // MSBuild missing: vswhere's path is proposed, never written
        std::fs::remove_file(&msbuild).unwrap();
        let vs = || Some(PathBuf::from(r"C:\VS\MSBuild\Current\Bin\MSBuild.exe"));
        let l = lines(&xpp_checks(&cfg, &repo, &vs));
        let f = l
            .iter()
            .find(|x| x.starts_with("Fail MSBuild not found"))
            .unwrap();
        assert!(f.contains(r"vswhere finds C:\VS\MSBuild\Current\Bin\MSBuild.exe: set toolchains.xpp.msbuild to it"), "{f}");
        let l = lines(&xpp_checks(&cfg, &repo, &none));
        assert!(l.iter().any(|x| x.contains("vswhere finds none")), "{l:#?}");

        // versions differ → WARN; unreadable or without a version → INFO
        let d = tempfile::tempdir().unwrap();
        let (cfg, repo, _) = xpp_fixture(d.path(), Some(r#"{"PlatformVersion":"7.0.7300.1"}"#));
        let c = xpp_checks(&cfg, &repo, &none);
        let w = c.iter().find(|c| c.level == Level::Warn).unwrap();
        assert!(
            w.message
                .contains("UDE metadata version 7.0.7300.1 ≠ the platform packages' 7.0.7198.40"),
            "{}",
            w.message
        );
        let d = tempfile::tempdir().unwrap();
        let (cfg, repo, _) = xpp_fixture(d.path(), None);
        let l = lines(&xpp_checks(&cfg, &repo, &none));
        assert!(
            l.iter().any(|x| x.starts_with("Info UDE version not read")),
            "{l:#?}"
        );
        let d = tempfile::tempdir().unwrap();
        let (cfg, repo, _) = xpp_fixture(d.path(), Some("{ not json"));
        let l = lines(&xpp_checks(&cfg, &repo, &none));
        assert!(
            l.iter()
                .any(|x| x.starts_with("Info UDE InstalledVersion.json holds no")),
            "{l:#?}"
        );

        // a missing package, project and metadata folder fail
        std::fs::remove_dir_all(
            d.path()
                .join("nugets")
                .join(format!("{}.7.0.7198.40", crate::engine::xpp::PACKAGES[2])),
        )
        .unwrap();
        std::fs::remove_file(repo.join("Projects/Build/Core.Build.rnrproj")).unwrap();
        std::fs::remove_dir_all(repo.join("Metadata")).unwrap();
        let l = lines(&xpp_checks(&cfg, &repo, &none));
        for want in [
            "Fail xpp_toolchain_unavailable",
            "Fail build: metadata folder not found",
            "Fail build: project not found in the repository",
        ] {
            assert!(l.iter().any(|x| x.starts_with(want)), "{want}: {l:#?}");
        }

        // no tool: xpp command → no X++ checks
        let d = tempfile::tempdir().unwrap();
        let (p, m) = (d.path().join("toryo.yaml"), d.path().join("machine.yaml"));
        std::fs::write(&p, "version: 1\nproject: { root: r, worktrees_root: w }\n").unwrap();
        std::fs::write(&m, "toolchains:\n  xpp: { msbuild: m, nugets: n }\n").unwrap();
        let cfg = Effective::load(&p, Some(&m)).unwrap();
        assert!(xpp_checks(&cfg, d.path(), &none).is_empty());
    }

    #[test]
    fn installed_versions_are_read_tolerantly() {
        assert_eq!(
            four_part_versions(
                r#"{"a":"7.0.7198.40","b":"10.0.2015.16","c":"7.0.7198.40","d":"1.2.3"}"#
            ),
            ["7.0.7198.40", "10.0.2015.16"]
        );
        assert!(four_part_versions("").is_empty());
    }

    #[test]
    fn semver_and_pattern() {
        assert_eq!(parse_semver("2.1.260 (Claude Code)"), Some((2, 1, 260)));
        assert!(parse_semver("2.1.247").unwrap() < CLAUDE_MIN_RESTRICTED);
        assert!(version_matches("2.1.x", "2.1.260 (Claude Code)"));
        assert!(!version_matches("0.15x", "codex-cli 0.16.0"));
        assert!(version_matches("0.15x", "codex-cli 0.150.1"));
    }

    /// EC-27 (the review's probe, inverted): the pin is compared per component.
    #[test]
    fn version_pin_is_compared_per_component() {
        for (pin, actual) in [
            ("2.1.x", "2.10.5 (Claude Code)"),
            ("2.1.x", "12.1.0"),
            ("0.15x", "codex-cli 10.15.0"),
            ("0.15x", "codex-cli 0.14.9"),
            ("2.1.260", "2.1.261 (Claude Code)"),
            ("2.1.x", "no version here"),
            ("2.1.0.x", "2.1.0"),
        ] {
            assert!(!version_matches(pin, actual), "{pin} vs {actual}");
        }
        for (pin, actual) in [
            ("2.1.x", "2.1.283 (Claude Code)"),
            ("2.1", "2.1.283 (Claude Code)"),
            ("2.x", "2.10.5"),
            ("2.1.283", "2.1.283 (Claude Code)"),
            ("0.15x", "codex-cli 0.157.1"),
            ("0.15x", "codex-cli 0.15.0"),
            ("2.47.x", "git version 2.47.1.windows.2"),
            ("2.1.260", "2.1.260-beta.1"),
            ("", "anything"),
        ] {
            assert!(version_matches(pin, actual), "{pin} vs {actual}");
        }
    }
}
