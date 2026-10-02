//! The org chart and the role files (M11): the `.toryo/` layout of a repository.
//!
//! - `.toryo/toryo-orgchart.yaml` ([`OrgChart`]) says **who** sits in each of the three fixed
//!   seats (a Toryo profile of the machine config, R60), which gates exist, the retry limits
//!   and the team-wide checks.
//! - `.toryo/roles/<seat>/toryo-role.yaml` ([`RoleFile`]) is the job card of a seat: its
//!   purpose, its instructions file, its skills, the implementer's write fence and the gates
//!   that must pass before its work is handed on.
//!
//! The flow is fixed (owner decision D1): plan → build → review. Repository files carry no
//! machine-only key and no YAML tag (review C1): each has its own `deny_unknown_fields` struct,
//! and [`crate::config::refuse_tags`] runs before the typed parse.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{CommandSpec, Effective, EnvGate, MachineConfig, Origin, Role};

/// The org chart, relative to the repository root.
pub const ORGCHART: &str = ".toryo/toryo-orgchart.yaml";
/// A role file's name inside its seat folder (`.toryo/roles/<seat>/`).
pub const ROLE_FILE: &str = "toryo-role.yaml";
/// The three seats of the fixed flow, in flow order.
pub const SEATS: [&str; 3] = ["planner", "implementer", "reviewer"];

/// `gates.<name>.kind` (owner decision D6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GateType {
    /// Defined in the repository; runs in the clean verify checkout at Low IL, no secrets.
    Check,
    /// The command is defined only in the machine config; runs with the owner's normal rights
    /// behind a machine-wide lock.
    Environment,
}

/// `gates.<name>.if_missing` of an environment gate the machine does not define.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IfMissing {
    /// Refuse to create the task (the default).
    Error,
    /// Run without it; the final gate says so.
    Skip,
}

/// `off | warn | error` of a team-wide check.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Off,
    #[default]
    Warn,
    Error,
}

/// One entry of `gates:`. Which fields each kind may have is checked by the validation pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateDecl {
    pub kind: GateType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<String>,
    /// `30s`, `20m`, `1h` ([`parse_duration`]).
    pub timeout: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_missing: Option<IfMissing>,
}

/// One seat: which Toryo profile sits there (owner decision D2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seat {
    pub profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Ordered profiles to switch to when the primary cannot run; omitted = none (D7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<Vec<String>>,
}

/// Exactly the three seats of the fixed flow: a missing, extra or misspelt seat is an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seats {
    pub planner: Seat,
    pub implementer: Seat,
    pub reviewer: Seat,
}

impl Seats {
    /// `(seat name, seat)` in flow order.
    pub fn iter(&self) -> [(&'static str, &Seat); 3] {
        [
            ("planner", &self.planner),
            ("implementer", &self.implementer),
            ("reviewer", &self.reviewer),
        ]
    }
}

/// `limits:` (owner decision D7: the retry limit is split).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Build: how often failing gates go back to the implementer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_gate_retries: Option<u32>,
    /// Review: how many rejections (review → build) are allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_review_rounds: Option<u32>,
    /// Today's single counter (R4); alone only — with either split key it is an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_fix_cycles: Option<u32>,
}

impl Limits {
    /// The split counters are in use.
    pub fn split(&self) -> bool {
        self.max_gate_retries.is_some() || self.max_review_rounds.is_some()
    }
}

/// `checks:` — team-wide sanity rules.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checks {
    #[serde(rename = "reviewer-differs-from-builder", default)]
    pub reviewer_differs_from_builder: Level,
}

/// `.toryo/toryo-orgchart.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgChart {
    /// Schema version; must be 1.
    pub toryo: u32,
    pub team: String,
    /// Team-wide norms (Markdown), relative to the repository root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub culture: Option<String>,
    #[serde(default)]
    pub gates: BTreeMap<String, GateDecl>,
    pub seats: Seats,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub checks: Checks,
}

/// `.toryo/roles/<seat>/toryo-role.yaml` — the job card of a seat (agent-agnostic).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleFile {
    /// Schema version; must be 1.
    pub toryo: u32,
    /// One sentence for the UI and logs; never sent to the agent.
    pub purpose: String,
    /// Markdown added after Toryo's built-in role text, relative to the role folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Skill names (the implementer's: a menu the planner chooses from, D4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// Implementer only: the only files it may change (globs, case-insensitive).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writes: Vec<String>,
    /// Implementer only: files it must never change, even inside `writes`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub never: Vec<String>,
    /// Implementer only: the gates that must pass before review, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub before_handoff: Vec<String>,
}

/// A `timeout:` as seconds: `<n>s`, `<n>m` or `<n>h`, n ≥ 1. Anything else is an error that
/// names the value.
pub fn parse_duration(s: &str) -> Result<u64> {
    let t = s.trim();
    let bad = || {
        anyhow::anyhow!("`{s}` is not a duration — write a whole number ≥ 1 followed by s, m or h (30s, 20m, 1h)")
    };
    let unit = t.chars().last().ok_or_else(bad)?;
    let mult = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        _ => return Err(bad()),
    };
    let digits = &t[..t.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let n: u64 = digits.parse().map_err(|_| bad())?;
    if n == 0 {
        return Err(bad());
    }
    n.checked_mul(mult).ok_or_else(bad)
}

/// Which repository file a text is, for the removed-key messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    OrgChart,
    Role,
}

/// Keys that existed in a draft or in an older layout and are removed, each with its own
/// message instead of the generic unknown-field error (M11-B1). `path` is the key path, the
/// message says what to do.
pub fn removed_keys(doc: &serde_yaml::Value, kind: FileKind) -> Vec<(String, &'static str)> {
    use crate::config::untagged;
    let mut out = Vec::new();
    let Some(map) = untagged(doc).as_mapping() else {
        return out;
    };
    let has = |m: &serde_yaml::Mapping, k: &str| m.keys().any(|x| untagged(x).as_str() == Some(k));
    match kind {
        FileKind::OrgChart => {
            if has(map, "flow") {
                out.push(("flow".into(), "the flow is fixed: plan → build → review; there is no `flow:` key — move `max_gate_retries` / `max_rounds` to `limits:` (`max_gate_retries`, `max_review_rounds`)"));
            }
            if has(map, "skills_dir") {
                out.push(("skills_dir".into(), "`skills_dir` belongs in the machine config (%USERPROFILE%\\.toryo\\toryo.local.yaml); repository skills go in .toryo/skills/<name>/SKILL.md"));
            }
            let seats = map
                .iter()
                .find(|(k, _)| untagged(k).as_str() == Some("seats"))
                .and_then(|(_, v)| untagged(v).as_mapping());
            for (name, seat) in seats.into_iter().flatten() {
                let Some(seat) = untagged(seat).as_mapping() else {
                    continue;
                };
                if has(seat, "agent") {
                    let name = untagged(name).as_str().unwrap_or("?");
                    out.push((format!("seats.{name}.agent"), "a seat names a Toryo profile, not an agent: `profile: codex-openai` (profiles are defined in the machine config, R60)"));
                }
            }
        }
        FileKind::Role => {
            if has(map, "reads") {
                out.push(("reads".into(), "Toryo decides what each stage sees (the task, the approved plan, the review, the gate output); remove `reads`"));
            }
        }
    }
    out
}

/// Parse a repository YAML file of `kind` into `T`: no tags (C1), no removed key, then the
/// typed parse. Every problem names the file and the key path.
pub fn parse_repo_file<T: serde::de::DeserializeOwned>(
    text: &str,
    file: &Path,
    kind: FileKind,
) -> Result<T> {
    let doc: serde_yaml::Value =
        serde_yaml::from_str(text).with_context(|| format!("parsing {}", file.display()))?;
    crate::config::refuse_tags(&doc, file)?;
    let removed = removed_keys(&doc, kind);
    if !removed.is_empty() {
        let lines: Vec<String> = removed
            .iter()
            .map(|(k, why)| format!("{k}: {why}"))
            .collect();
        bail!("{}:\n  - {}", file.display(), lines.join("\n  - "));
    }
    // the typed parse of the text keeps serde's key path and line for the friendly message
    serde_yaml::from_str(text).map_err(|e| match friendly_serde(&e.to_string()) {
        Some(f) => anyhow::anyhow!("{}: {f}", file.display()),
        None => anyhow::Error::new(e).context(format!("parsing {}", file.display())),
    })
}

// ── B2: org chart mode — load, merge, freeze ─────────────────────────────────────────

/// A file the configuration was read from, with the SHA-256 of its bytes (`config show`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileRead {
    /// What the file is: `toryo.yaml`, `machine config`, `org chart`, `role <seat>`, …
    pub what: String,
    pub path: String,
    pub sha256: String,
}

impl FileRead {
    pub fn of(what: &str, path: &Path, bytes: &[u8]) -> Self {
        Self {
            what: what.to_string(),
            path: path.display().to_string(),
            sha256: crate::engine::task::sha256_hex(bytes),
        }
    }
}

/// One gate as frozen with a task: the org chart's declaration and, for an environment
/// gate, the machine's command (`None`: not defined on this machine).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gate {
    pub decl: GateDecl,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<EnvGate>,
}

/// A role file as frozen with a task. `instructions` is relative to the repository root.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamRole {
    pub purpose: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub never: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub before_handoff: Vec<String>,
}

/// Where skill names are looked up (owner decision D3): the repository's `.toryo/skills`,
/// then the machine's `skills_dir`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillRoots {
    pub repo: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
}

/// The `team` section of the frozen configuration: everything a run in org chart mode
/// depends on besides the `repo` section (which holds the seats as `roles`, the check gates
/// as `commands.verify` and a single `max_fix_cycles`). Serialised only in org chart mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Team {
    pub name: String,
    /// The team's culture file, relative to the repository root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub culture: Option<String>,
    pub gates: BTreeMap<String, Gate>,
    pub limits: Limits,
    pub checks: Checks,
    pub roles: BTreeMap<String, TeamRole>,
    pub skill_roots: SkillRoots,
}

impl Team {
    pub fn role(&self, seat: &str) -> Option<&TeamRole> {
        self.roles.get(seat)
    }

    /// The implementer's gates in `before_handoff` order: `(name, gate)`.
    pub fn handoff_gates(&self) -> Vec<(&str, &Gate)> {
        self.role("implementer")
            .map(|r| r.before_handoff.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|n| self.gates.get(n).map(|g| (n.as_str(), g)))
            .collect()
    }
}

/// `<root>/.toryo/toryo-orgchart.yaml` with the platform's separators (for messages).
pub fn org_chart_path(root: &Path) -> PathBuf {
    root.join(".toryo").join("toryo-orgchart.yaml")
}

/// The repository root of org chart mode: `project.root` (expanded) when it holds
/// `.toryo/toryo-orgchart.yaml`; `None` is `toryo.yaml`-only mode, byte for byte as in M10.
/// `.toryo/` is read from the repository checkout, never from a task worktree (§7).
pub fn org_chart_root(project_root: &str) -> Option<PathBuf> {
    let root = PathBuf::from(crate::config::expand_vars(project_root).ok()?);
    org_chart_path(&root).is_file().then_some(root)
}

/// What org chart mode adds to a configuration (M11-B2).
#[derive(Debug, Clone)]
pub struct Loaded {
    pub team: Team,
    /// The seats as `roles` (an omitted `fallback` is `[]`: no fallback, D7).
    pub roles: BTreeMap<String, Role>,
    /// The check gates of the implementer's `before_handoff`, in order (`commands.verify`).
    pub verify: Vec<CommandSpec>,
    pub files: Vec<FileRead>,
    /// Problems found while loading, reported with the validation pass (all at once).
    pub problems: Vec<String>,
}

/// The repository-relative form of `p` joined to `base` (`./CULTURE.md` → `CULTURE.md`,
/// `\` → `/`).
pub fn repo_rel(base: &str, p: &str) -> String {
    let joined = if base.is_empty() {
        p.replace('\\', "/")
    } else {
        format!("{base}/{}", p.replace('\\', "/"))
    };
    joined
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// A check gate as a verify command (E1): it runs exactly like a `commands.verify[]` item.
pub fn gate_command(name: &str, d: &GateDecl) -> CommandSpec {
    CommandSpec {
        name: name.to_string(),
        program: d.program.clone(),
        args: d.args.clone(),
        timeout_min: None,
        timeout_sec: parse_duration(&d.timeout).ok(),
        tool: d.tool.clone(),
        metadata: d.metadata.clone(),
        projects: d.projects.clone(),
    }
}

/// Load `.toryo/` of the repository checkout `root` (M11-B2): the org chart, the three role
/// files, the seats as roles, the check gates as verify commands and the frozen [`Team`].
/// `toryo_yaml` / `doc`: the repository `toryo.yaml` (one place per setting); `machine`: the
/// machine config (environment gates, `skills_dir`). The org chart itself must parse; every
/// other problem is collected in [`Loaded::problems`].
pub fn load(
    root: &Path,
    toryo_yaml: &Path,
    doc: &serde_yaml::Value,
    machine: Option<&MachineConfig>,
    machine_hint: &str,
) -> Result<Loaded> {
    use crate::config::untagged;
    let org_path = org_chart_path(root);
    let bytes =
        std::fs::read(&org_path).with_context(|| format!("reading {}", org_path.display()))?;
    let org: OrgChart = parse_repo_file(
        &String::from_utf8_lossy(&bytes),
        &org_path,
        FileKind::OrgChart,
    )?;
    let mut files = vec![FileRead::of("org chart", &org_path, &bytes)];
    let mut problems = Vec::new();
    let at = |key: &str| format!("{}: {key}", org_path.display());
    if org.toryo != 1 {
        problems.push(format!(
            "{}: unsupported version {} (this Toryo reads `toryo: 1`)",
            at("toryo"),
            org.toryo
        ));
    }
    // one place per setting: toryo.yaml must not also set what the org chart holds
    let map = untagged(doc).as_mapping();
    let top = |k: &str| {
        map.and_then(|m| m.iter().find(|(x, _)| untagged(x).as_str() == Some(k)))
            .map(|(_, v)| v)
    };
    let pipeline_max = top("pipeline")
        .and_then(|v| untagged(v).as_mapping())
        .is_some_and(|p| {
            p.keys()
                .any(|k| untagged(k).as_str() == Some("max_fix_cycles"))
        });
    for (key, set, place) in [
        ("roles", top("roles").is_some(), "`seats:`"),
        (
            "commands",
            top("commands").is_some(),
            "`gates:` and the implementer's `before_handoff`",
        ),
        ("pipeline.max_fix_cycles", pipeline_max, "`limits:`"),
    ] {
        if set {
            problems.push(format!(
                "{}: {key}: set here and in the org chart {} — one place per setting (M11): with an org chart this lives in {place}; remove `{key}` from {}",
                toryo_yaml.display(),
                org_path.display(),
                toryo_yaml.display()
            ));
        }
    }
    // the role files; a leftover M4 override file is an error
    let mut roles = BTreeMap::new();
    let dir = root.join(".toryo").join("roles");
    for (seat, _) in org.seats.iter() {
        let legacy = dir.join(format!("{seat}.md"));
        if legacy.is_file() {
            problems.push(format!(
                "{}: the M4 role override file is not read in org chart mode — move it to .toryo/roles/{seat}/instructions.md and name it in `instructions:` of .toryo/roles/{seat}/{ROLE_FILE} (it is then added after Toryo's built-in role text, never instead of it)",
                legacy.display()
            ));
        }
        let p = dir.join(seat).join(ROLE_FILE);
        let Ok(bytes) = std::fs::read(&p) else {
            problems.push(format!(
                "{}: seat `{seat}` has no role file — every seat needs .toryo/roles/{seat}/{ROLE_FILE}",
                p.display()
            ));
            continue;
        };
        files.push(FileRead::of(&format!("role {seat}"), &p, &bytes));
        match parse_repo_file::<RoleFile>(&String::from_utf8_lossy(&bytes), &p, FileKind::Role) {
            Ok(r) => {
                if r.toryo != 1 {
                    problems.push(format!(
                        "{}: toryo: unsupported version {} (this Toryo reads `toryo: 1`)",
                        p.display(),
                        r.toryo
                    ));
                }
                roles.insert(seat.to_string(), r);
            }
            Err(e) => problems.push(format!("{e:#}")),
        }
    }
    // the seats as roles: an omitted fallback is none (D7); planner/reviewer have none (R49)
    let mut out_roles = BTreeMap::new();
    for (seat, s) in org.seats.iter() {
        if seat != "implementer" && s.fallback.as_ref().is_some_and(|f| !f.is_empty()) {
            problems.push(format!(
                "{}: a fallback on the {seat} is not supported yet — only the implementer may switch profiles (R49); remove it",
                at(&format!("seats.{seat}.fallback"))
            ));
        }
        out_roles.insert(
            seat.to_string(),
            Role {
                profile: s.profile.clone(),
                model: s.model.clone(),
                fallback: Some(s.fallback.clone().unwrap_or_default()),
            },
        );
    }
    // relative paths, checked as written (an absolute one would read as relative once
    // normalised; group B review)
    if let Some(why) = org
        .culture
        .as_deref()
        .and_then(crate::config::not_a_repo_path)
    {
        let c = org.culture.as_deref().unwrap_or_default();
        problems.push(format!("{}: `{c}` {why}", at("culture")));
    }
    for (seat, r) in roles.iter_mut() {
        let rf = dir.join(seat.as_str()).join(ROLE_FILE);
        if let Some(i) = &r.instructions {
            if let Some(why) = crate::config::not_a_repo_path(i) {
                problems.push(format!(
                    "{}: instructions: `{i}` {why} (it is relative to the role folder)",
                    rf.display()
                ));
            }
        }
        // M11-R4: the fence patterns in the form they are matched in (and frozen in); a
        // pattern that could never match is refused instead of failing open
        for (key, list) in [("writes", &mut r.writes), ("never", &mut r.never)] {
            for p in list.iter_mut() {
                match crate::engine::checks::fence_pattern(p) {
                    Ok(n) => *p = n,
                    Err(why) => problems.push(format!("{}: {key}: `{p}` {why}", rf.display())),
                }
            }
        }
    }
    if org.limits.max_fix_cycles.is_some() && org.limits.split() {
        problems.push(format!(
            "{}: `max_fix_cycles` is the single counter — use it alone, or `max_gate_retries` / `max_review_rounds` without it",
            at("limits.max_fix_cycles")
        ));
    }
    for (name, g) in &org.gates {
        if let Err(e) = parse_duration(&g.timeout) {
            problems.push(format!("{}: {e:#}", at(&format!("gates.{name}.timeout"))));
        }
    }
    let handoff = roles
        .get("implementer")
        .map(|r| r.before_handoff.clone())
        .unwrap_or_default();
    let verify = handoff
        .iter()
        .filter_map(|n| {
            org.gates
                .get(n)
                .filter(|g| g.kind == GateType::Check)
                .map(|g| gate_command(n, g))
        })
        .collect();
    let gates = org
        .gates
        .iter()
        .map(|(name, d)| {
            let machine = (d.kind == GateType::Environment)
                .then(|| machine.and_then(|m| m.environment_gates.get(name)).cloned())
                .flatten();
            // M11-R3 (X3): `%VAR%` expanded once, here; what is validated, frozen and run
            let machine = machine.map(|g| expand_env_gate(name, &g, machine_hint, &mut problems));
            (
                name.clone(),
                Gate {
                    decl: d.clone(),
                    machine,
                },
            )
        })
        .collect();
    let team_roles = roles
        .iter()
        .map(|(seat, r)| {
            let base = format!(".toryo/roles/{seat}");
            let role = TeamRole {
                purpose: r.purpose.clone(),
                instructions: r.instructions.as_deref().map(|i| repo_rel(&base, i)),
                skills: r.skills.clone(),
                writes: r.writes.clone(),
                never: r.never.clone(),
                before_handoff: r.before_handoff.clone(),
            };
            (seat.clone(), role)
        })
        .collect();
    let team = Team {
        name: org.team.clone(),
        culture: org.culture.as_deref().map(|c| repo_rel("", c)),
        gates,
        limits: org.limits.clone(),
        checks: org.checks.clone(),
        roles: team_roles,
        skill_roots: SkillRoots {
            repo: ".toryo/skills".into(),
            machine: machine.and_then(|m| m.skills_dir.clone()),
        },
    };
    Ok(Loaded {
        team,
        roles: out_roles,
        verify,
        files,
        problems,
    })
}

// ── B3: the validation pass ──────────────────────────────────────────────────────────

/// The edit distance between `a` and `b` (Levenshtein, on chars).
pub fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != *cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The one candidate `name` was probably meant to be: a case-insensitive match, else the
/// single candidate within edit distance 2. `None` when nothing is close or two candidates
/// are equally close.
pub fn did_you_mean<'a>(name: &str, candidates: &[&'a str]) -> Option<&'a str> {
    let lower = name.to_lowercase();
    let ci: Vec<&str> = candidates
        .iter()
        .copied()
        .filter(|c| c.to_lowercase() == lower)
        .collect();
    if ci.len() == 1 {
        return Some(ci[0]);
    }
    let mut best: Option<(usize, &str)> = None;
    let mut tie = false;
    for c in candidates {
        let d = edit_distance(&lower, &c.to_lowercase());
        if d > 2 {
            continue;
        }
        match best {
            Some((bd, _)) if d > bd => {}
            Some((bd, _)) if d == bd => tie = true,
            _ => {
                best = Some((d, c));
                tie = false;
            }
        }
    }
    best.filter(|_| !tie).map(|(_, c)| c)
}

/// ` Did you mean 'x'?` when [`did_you_mean`] finds one, else empty.
pub fn hint(name: &str, candidates: &[&str]) -> String {
    did_you_mean(name, candidates)
        .map(|c| format!(" Did you mean '{c}'?"))
        .unwrap_or_default()
}

/// serde's `unknown field` / `missing field` errors in the friendly form, with the key path
/// and a "did you mean" hint: `seats: unknown field `reviwer`, expected one of …` →
/// `seats.reviwer: seat 'reviwer' not found. Did you mean 'reviewer'? (the seats are …)`.
/// `None` for any other error.
pub fn friendly_serde(msg: &str) -> Option<String> {
    static UNKNOWN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^(?:(?P<path>[^ ]+): )?unknown field `(?P<f>[^`]+)`, expected (?P<list>.*?)(?: at line (?P<line>\d+) column \d+)?$").expect("regex")
    });
    static MISSING: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^(?:(?P<path>[^ ]+): )?missing field `(?P<f>[^`]+)`(?: at line (?P<line>\d+) column \d+)?$").expect("regex")
    });
    let line = |c: &regex::Captures| {
        c.name("line")
            .map(|l| format!(" (line {})", l.as_str()))
            .unwrap_or_default()
    };
    if let Some(c) = UNKNOWN.captures(msg) {
        let f = &c["f"];
        let list: Vec<&str> = c["list"].split('`').skip(1).step_by(2).collect();
        let path = c.name("path").map(|p| p.as_str());
        let key = match path {
            Some(p) => format!("{p}.{f}"),
            None => f.to_string(),
        };
        let valid = list.join(", ");
        let what = if path == Some("seats") {
            format!("seat '{f}' not found.{} (the seats are {valid}; the flow is fixed: plan → build → review)", hint(f, &list))
        } else {
            format!("unknown key '{f}'.{} (valid: {valid})", hint(f, &list))
        };
        return Some(format!("{key}: {what}{}", line(&c)));
    }
    if let Some(c) = MISSING.captures(msg) {
        let f = &c["f"];
        let path = c.name("path").map(|p| p.as_str());
        if path == Some("seats") {
            return Some(format!(
                "seats: seat '{f}' is missing — exactly three seats: planner, implementer, reviewer{}",
                line(&c)
            ));
        }
        let key = path
            .map(|p| format!("{p}.{f}"))
            .unwrap_or_else(|| f.to_string());
        return Some(format!("{key}: required{}", line(&c)));
    }
    None
}

/// An argument that names a file: not a flag, and with a path separator or a script or
/// program extension.
fn named_path(a: &str) -> Option<&str> {
    // the value of a `--name=value` / `-Name:value` flag; a flag alone names no file
    let v = if a.starts_with('-') {
        a.split_once(['=', ':']).map(|(_, v)| v)?
    } else {
        a
    };
    let lower = v.to_ascii_lowercase();
    // a Windows switch (`/c`, `/q`) is not a path
    let switch = v.starts_with('/') && v.len() <= 3 && !v[1..].contains(['/', '\\']);
    let file = v.contains('/')
        || v.contains('\\')
        || [".ps1", ".psm1", ".cmd", ".bat", ".sh", ".exe", ".py", ".js"]
            .iter()
            .any(|x| lower.ends_with(x));
    (!v.is_empty() && !switch && file).then_some(v)
}

/// The lexical form the reach check compares (group B review): `/` separators, no `\\?\`
/// prefix, `.` dropped, `..` resolved, trailing dots and spaces of a segment dropped (Windows
/// ignores them), lower case. An existing path is canonicalised first (junctions, 8.3 names).
fn lexical(p: &str) -> String {
    let canon = std::fs::canonicalize(p)
        .map(|c| c.display().to_string())
        .unwrap_or_else(|_| p.to_string());
    let s = canon.replace('\\', "/");
    let s = s
        .strip_prefix("//?/")
        .or_else(|| s.strip_prefix("//./"))
        .unwrap_or(&s);
    let mut parts: Vec<&str> = Vec::new();
    for (i, c) in s.split('/').enumerate() {
        match c {
            "" if i == 0 => parts.push(""),
            "" | "." => {}
            ".." => {
                if parts.len() > 1 {
                    parts.pop();
                }
            }
            c => parts.push(c.trim_end_matches(['.', ' '])),
        }
    }
    parts.join("/").to_lowercase()
}

/// `p` is `root` or inside it, compared in [`lexical`] form.
fn inside(p: &str, root: &Path) -> bool {
    let (p, r) = (lexical(p), lexical(&root.display().to_string()));
    !r.is_empty() && (p == r || p.starts_with(&format!("{r}/")))
}

/// Which agent-reachable folder `p` is inside, if any: the repository (`project.root`), the
/// task worktrees or the verify root — `(what, root)`. Used at load (B3) and, for a bare
/// program resolved through PATH, before an environment gate runs (M11-R3).
pub fn reach(cfg: &Effective, p: &str) -> Option<(&'static str, PathBuf)> {
    let expand = |s: &str| crate::config::expand_vars(s).map(PathBuf::from).ok();
    [
        ("the repository", expand(&cfg.repo.project.root)),
        (
            "the task worktrees",
            expand(&cfg.repo.project.worktrees_root),
        ),
        (
            "the verify root",
            Some(crate::engine::verify::verify_root(cfg)),
        ),
    ]
    .into_iter()
    .filter_map(|(w, r)| r.map(|r| (w, r)))
    .find(|(_, r)| inside(p, r))
}

/// M11-R3 (X3): an environment gate's `program` and `args` with `%VAR%` expanded — once, at
/// load; the expanded text is what the reach check sees, what is frozen and hashed, and what
/// runs. An unset variable is a problem naming the key (the text is then kept as written).
fn expand_env_gate(name: &str, g: &EnvGate, machine: &str, problems: &mut Vec<String>) -> EnvGate {
    let mut one = |key: String, s: &str| match crate::config::expand_vars(s) {
        Ok(v) => v,
        Err(e) => {
            problems.push(format!(
                "{machine}: environment_gates.{name}.{key}: {e:#} — set it, or write the path out"
            ));
            s.to_string()
        }
    };
    let program = one("program".into(), &g.program);
    let args = g
        .args
        .iter()
        .enumerate()
        .map(|(i, a)| one(format!("args[{i}]"), a))
        .collect();
    EnvGate {
        program,
        args,
        ..g.clone()
    }
}

/// The problems of a machine `EnvGate` (B3): a lock; a program that is absolute or a PATH
/// name; every path-like argument absolute and out of the agents' reach — not inside the
/// repository, the task worktrees or the verify root.
fn env_gate_problems(
    out: &mut Vec<String>,
    name: &str,
    g: &EnvGate,
    cfg: &Effective,
    machine: &str,
) {
    let key = format!("{machine}: environment_gates.{name}");
    if g.lock.trim().is_empty() {
        out.push(format!(
            "{key}.lock: required — the machine-wide lock name (e.g. d365-env): one environment gate per lock runs at a time"
        ));
    } else if let Some(why) = crate::engine::lock::lock_name_problem(&g.lock) {
        // M11-E3: the name is part of the lock's file name
        out.push(format!("{key}.lock: `{}` — {why}", g.lock));
    }
    // M11-R3 (X3): `program` and `args` were expanded at load — this is the text that runs
    // (a value with a variable that is not set was reported at load, once)
    let unset = |s: &str| crate::config::expand_vars(s).is_err();
    let reach = |p: &str| reach(cfg, p);
    let program = g.program.clone();
    if (program.contains('/') || program.contains('\\')) && !unset(&program) {
        let dots = program.replace('\\', "/").split('/').any(|c| c == "..");
        if dots || !Path::new(&program).is_absolute() {
            out.push(format!(
                "{key}.program: `{}` must be an absolute path or a bare name found on PATH",
                g.program
            ));
        } else if let Some((what, r)) = reach(&program) {
            out.push(format!(
                "{key}.program: `{}` is inside {what} ({}) — an agent could change it; keep it outside the repository, the task worktrees and the verify root",
                g.program,
                r.display()
            ));
        }
    }
    for a in g.args.iter().filter(|a| !unset(a)) {
        let Some(named) = named_path(a) else {
            continue;
        };
        let x = named.to_string();
        if x.replace('\\', "/").split('/').any(|c| c == "..") {
            out.push(format!(
                "{key}.args: `{a}` may not contain `..` — write the absolute path of the script, outside the repository, the task worktrees and the verify root"
            ));
        } else if !Path::new(&x).is_absolute() {
            out.push(format!(
                "{key}.args: `{a}` must be an absolute path — the script must be out of the agents' reach (not in the repository, a task worktree or the verify root)"
            ));
        } else if let Some((what, r)) = reach(&x) {
            out.push(format!(
                "{key}.args: `{a}` is inside {what} ({}) — an agent could change it; keep the script outside the repository, the task worktrees and the verify root",
                r.display()
            ));
        }
    }
}

/// Every cross-reference and kind problem of org chart mode (M11-B3), all at once, each with
/// its file and key path: role files and their instructions, seat profiles (R60), gate kinds
/// and fields, the implementer's `before_handoff` (declared gates, checks before
/// environments), the machine's environment gates, the culture file. `root`: the repository
/// checkout; `machine`: how to name the machine config.
pub fn problems(cfg: &Effective, root: &Path, machine: &str) -> Vec<String> {
    let Some(team) = &cfg.team else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let org = org_chart_path(root);
    let org_at = |key: &str| format!("{}: {key}", org.display());
    let role_file = |seat: &str| root.join(".toryo").join("roles").join(seat).join(ROLE_FILE);
    // seat profiles (R60): named in the org chart, or by a machine seat override
    let profiles: Vec<&str> = cfg.repo.profiles.keys().map(String::as_str).collect();
    for seat in SEATS {
        let Some(r) = cfg.repo.roles.get(seat) else {
            continue;
        };
        let from_repo = cfg.origins.roles.get(seat) == Some(&Origin::Repo);
        let place = |k: &str| {
            if from_repo {
                org_at(&format!("seats.{seat}.{k}"))
            } else {
                format!("{machine}: seats.{seat}.{k}")
            }
        };
        let names = std::iter::once(("profile", &r.profile))
            .chain(r.fallback.iter().flatten().map(|f| ("fallback", f)));
        for (k, p) in names {
            if !cfg.repo.profiles.contains_key(p) {
                out.push(format!(
                    "{}: profile '{p}' is not defined in the machine config ({machine}) under `profiles:` — profiles live only there (R60).{}",
                    place(k),
                    hint(p, &profiles)
                ));
            }
        }
    }
    // the role files: their instructions; the implementer-only keys
    for seat in SEATS {
        let Some(r) = team.role(seat) else {
            continue;
        };
        let rf = role_file(seat);
        if let Some(i) = &r.instructions {
            match crate::config::not_a_repo_path(i) {
                Some(why) => out.push(format!("{}: instructions: `{i}` {why}", rf.display())),
                None if !root.join(i).is_file() => out.push(format!(
                    "{}: instructions: {} not found (the path is relative to the role folder)",
                    rf.display(),
                    root.join(i).display()
                )),
                None => {}
            }
        }
        if seat != "implementer" {
            for (k, set) in [
                ("before_handoff", !r.before_handoff.is_empty()),
                ("writes", !r.writes.is_empty()),
                ("never", !r.never.is_empty()),
            ] {
                if set {
                    out.push(format!(
                        "{}: {k}: only the implementer may have `{k}` — the {seat} is read-only; remove it",
                        rf.display()
                    ));
                }
            }
        }
    }
    // M11-R8: the machine's `skills_dir` holds trusted text — absolute, out of the agents' reach
    if let Some(sd) = &team.skill_roots.machine {
        let key = format!("{machine}: skills_dir");
        match crate::config::expand_vars(sd) {
            Err(e) => out.push(format!("{key}: {e:#} — set it, or write the path out")),
            Ok(p) if !Path::new(&p).is_absolute() => out.push(format!(
                "{key}: `{sd}` must be an absolute path (it is the machine's shared skills folder)"
            )),
            Ok(p) => {
                if let Some((what, r)) = reach(cfg, &p) {
                    out.push(format!(
                        "{key}: `{sd}` is inside {what} ({}) — an agent could change the skills; keep the folder outside the repository, the task worktrees and the verify root",
                        r.display()
                    ));
                }
            }
        }
    }
    // the gates: which fields each kind has; environment gates on this machine
    for (name, g) in &team.gates {
        let key = org_at(&format!("gates.{name}"));
        let d = &g.decl;
        match d.kind {
            GateType::Check => {
                let c = gate_command(name, d);
                crate::config::verify_command_problems(
                    &mut out,
                    &key,
                    &c,
                    &cfg.toolchains,
                    machine,
                );
                if d.if_missing.is_some() {
                    out.push(format!(
                        "{key}.if_missing: only for an environment gate — a check gate is defined in the repository and always runs"
                    ));
                }
            }
            GateType::Environment => {
                for (k, set) in [
                    ("program", d.program.is_some()),
                    ("args", !d.args.is_empty()),
                    ("tool", d.tool.is_some()),
                    ("metadata", d.metadata.is_some()),
                    ("projects", !d.projects.is_empty()),
                ] {
                    if set {
                        out.push(format!(
                            "{key}.{k}: an environment gate has no command in the repository — it runs with your full rights, so its command is `environment_gates.{name}` in the machine config ({machine}); remove `{k}`"
                        ));
                    }
                }
                match &g.machine {
                    None if d.if_missing != Some(IfMissing::Skip) => out.push(format!(
                        "{key}: environment gate '{name}' is not defined on this machine — add `environment_gates.{name}` to the machine config ({machine}), or set `if_missing: skip` to run without it here"
                    )),
                    None => {}
                    Some(m) => env_gate_problems(&mut out, name, m, cfg, machine),
                }
            }
        }
    }
    // the implementer's before_handoff: declared gates, every check before every environment
    if let Some(imp) = team.role("implementer") {
        let rf = role_file("implementer");
        let names: Vec<&str> = team.gates.keys().map(String::as_str).collect();
        // at least one check gate: environment gates alone (or skipped on this machine) would
        // hand unverified work to review (group B review)
        let checks = imp
            .before_handoff
            .iter()
            .filter(|n| {
                team.gates
                    .get(*n)
                    .is_some_and(|g| g.decl.kind == GateType::Check)
            })
            .count();
        if checks == 0 {
            out.push(format!(
                "{}: before_handoff: no check gate — at least one `kind: check` gate must pass before the work is handed to review (declare one under `gates:` of {})",
                rf.display(),
                org.display()
            ));
        }
        let mut first_env: Option<&str> = None;
        for n in &imp.before_handoff {
            match team.gates.get(n) {
                None => out.push(format!(
                    "{}: before_handoff: gate '{n}' is not declared under `gates:` of {}.{}",
                    rf.display(),
                    org.display(),
                    hint(n, &names)
                )),
                Some(g) if g.decl.kind == GateType::Environment => {
                    first_env.get_or_insert(n);
                }
                Some(_) => {
                    if let Some(e) = first_env {
                        out.push(format!(
                            "{}: before_handoff: check gate '{n}' comes after environment gate '{e}' — every check gate must come before every environment gate (never deploy code that does not compile)",
                            rf.display()
                        ));
                    }
                }
            }
        }
    }
    // M11-D1: every skill name resolves (the repository, then the machine's `skills_dir`); a
    // skill on the implementer's menu has a `description` — the planner chooses by it
    let machine_dir = machine_skills_dir(&team.skill_roots);
    let names = skill_names(root, machine_dir.as_deref());
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    for seat in SEATS {
        let Some(r) = team.role(seat) else {
            continue;
        };
        let rf = role_file(seat);
        for s in &r.skills {
            match resolve_skill(s, root, machine_dir.as_deref()) {
                Err(why) => out.push(format!(
                    "{}: skills: {why}.{}",
                    rf.display(),
                    hint(s, &name_refs)
                )),
                Ok(k) if seat == "implementer" && k.description.is_none() => out.push(format!(
                    "{}: skills: skill '{s}' ({}) has no `description` in its front matter — the planner chooses the implementer's skills by it; start {} with `---`, `description: <one line>`, `---`",
                    rf.display(),
                    k.origin.as_str(),
                    k.path.display()
                )),
                Ok(_) => {}
            }
        }
    }
    if let Some(c) = &team.culture {
        match crate::config::not_a_repo_path(c) {
            Some(why) => out.push(format!("{}: `{c}` {why}", org_at("culture"))),
            None if !root.join(c).is_file() => out.push(format!(
                "{}: {} not found (the path is relative to the repository root)",
                org_at("culture"),
                root.join(c).display()
            )),
            None => {}
        }
    }
    out
}

// ── D1: skills ───────────────────────────────────────────────────────────────────────

/// Where a skill was found (owner decision D3: the repository first, then the machine).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillOrigin {
    Repo,
    Machine,
}

impl SkillOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Repo => "repo",
            Self::Machine => "machine",
        }
    }
}

/// A skill as resolved for one attempt (M11-D1): `{ name, origin, path, sha256, bytes }`, the
/// `description` of its front matter and its text. Only `SKILL.md` itself is delivered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skill {
    pub name: String,
    pub origin: SkillOrigin,
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: usize,
    pub description: Option<String>,
    #[serde(skip)]
    pub text: String,
}

/// The front matter of a SKILL.md: the YAML mapping between the first two `---` lines (a UTF-8
/// BOM and CRLF line ends allowed). `None` without front matter, or when it is not a mapping.
pub fn front_matter(text: &str) -> Option<serde_yaml::Mapping> {
    let t = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n");
    let mut lines = t.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let mut yaml = String::new();
    for l in lines {
        if l.trim_end() == "---" {
            let v: serde_yaml::Value = serde_yaml::from_str(&yaml).ok()?;
            return v.as_mapping().cloned();
        }
        yaml.push_str(l);
        yaml.push('\n');
    }
    None
}

/// The `description` of a SKILL.md's front matter, trimmed; `None` when missing or empty.
pub fn skill_description(text: &str) -> Option<String> {
    front_matter(text)?
        .get("description")?
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// A skill name is a folder name: not empty, no separator, no `..`, no drive.
pub fn valid_skill_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && name.trim() == name
}

/// The machine's skills folder of the frozen roots, expanded (`%VAR%`).
pub fn machine_skills_dir(roots: &SkillRoots) -> Option<PathBuf> {
    roots
        .machine
        .as_deref()
        .map(|m| PathBuf::from(crate::config::expand_vars(m).unwrap_or_else(|_| m.to_string())))
}

/// Where `name` is looked up, in order (D3): `<repo>/.toryo/skills/<name>/SKILL.md`, then
/// `<machine>/<name>/SKILL.md`.
pub fn skill_candidates(
    name: &str,
    repo: &Path,
    machine: Option<&Path>,
) -> Vec<(SkillOrigin, PathBuf)> {
    let mut out = vec![(
        SkillOrigin::Repo,
        repo.join(".toryo")
            .join("skills")
            .join(name)
            .join("SKILL.md"),
    )];
    if let Some(m) = machine {
        out.push((SkillOrigin::Machine, m.join(name).join("SKILL.md")));
    }
    out
}

/// Resolve `name`: the first place that has its SKILL.md wins. `Err`: why not, naming both
/// places.
pub fn resolve_skill(
    name: &str,
    repo: &Path,
    machine: Option<&Path>,
) -> std::result::Result<Skill, String> {
    if !valid_skill_name(name) {
        return Err(format!(
            "skill '{name}' is not a folder name — a skill is `<name>/SKILL.md`; use a name without `/`, `\\`, `:` or `..`"
        ));
    }
    let places = skill_candidates(name, repo, machine);
    for (origin, path) in &places {
        if let Ok(bytes) = std::fs::read(path) {
            let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
            return Ok(Skill {
                name: name.to_string(),
                origin: *origin,
                path: path.clone(),
                sha256: crate::engine::task::sha256_hex(text.as_bytes()),
                bytes: text.len(),
                description: skill_description(&text),
                text,
            });
        }
    }
    let shown: Vec<String> = places
        .iter()
        .map(|(_, p)| p.display().to_string())
        .collect();
    let machine_hint = if machine.is_none() {
        " (no `skills_dir` in the machine config)"
    } else {
        ""
    };
    Err(format!(
        "skill '{name}' not found: neither {}{machine_hint}",
        shown.join(" nor ")
    ))
}

/// The skill names that exist under the roots (folders with a SKILL.md), for "did you mean".
pub fn skill_names(repo: &Path, machine: Option<&Path>) -> Vec<String> {
    let mut out = Vec::new();
    let dirs =
        std::iter::once(repo.join(".toryo").join("skills")).chain(machine.map(Path::to_path_buf));
    for d in dirs {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            if e.path().join("SKILL.md").is_file() {
                let n = e.file_name().to_string_lossy().to_string();
                if !out.contains(&n) {
                    out.push(n);
                }
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A checkout with `core.autocrlf` has CRLF line ends in the fixtures.
    const CRLF: &str = "\r\n";

    fn fixtures() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("m11")
    }

    /// M11-B1 AC-1: the three owner drafts parse into these types.
    #[test]
    fn the_owner_drafts_parse() {
        let d = fixtures();
        let org = d.join(".toryo").join("toryo-orgchart.yaml");
        let o: OrgChart = parse_repo_file(
            &std::fs::read_to_string(&org).unwrap(),
            &org,
            FileKind::OrgChart,
        )
        .unwrap();
        assert_eq!((o.toryo, o.team.as_str()), (1, "d365fo-dev"));
        assert_eq!(o.culture.as_deref(), Some("./CULTURE.md"));
        assert_eq!(o.gates["build"].kind, GateType::Check);
        assert_eq!(
            o.gates["build"].args,
            [
                "-NoProfile",
                "-NonInteractive",
                "-File",
                "scripts/build-model.ps1"
            ]
        );
        assert_eq!(o.gates["dbsync"].kind, GateType::Environment);
        assert_eq!(o.gates["dbsync"].if_missing, Some(IfMissing::Error));
        assert_eq!(parse_duration(&o.gates["dbsync"].timeout).unwrap(), 1800);
        assert_eq!(o.seats.planner.model.as_deref(), Some("opus"));
        assert_eq!(
            o.seats.implementer.fallback,
            Some(vec!["claude-max".to_string()])
        );
        assert_eq!(o.seats.reviewer.fallback, None);
        assert_eq!(o.limits.max_gate_retries, Some(3));
        assert!(o.limits.split());
        assert_eq!(o.checks.reviewer_differs_from_builder, Level::Warn);
        let role = d
            .join(".toryo")
            .join("roles")
            .join("implementer")
            .join(ROLE_FILE);
        let r: RoleFile = parse_repo_file(
            &std::fs::read_to_string(&role).unwrap(),
            &role,
            FileKind::Role,
        )
        .unwrap();
        assert_eq!(r.skills, ["xpp-axclass", "xpp-batch"]);
        assert_eq!(r.writes, ["Metadata/**"]);
        assert_eq!(r.never, ["**/Descriptor/*.xml"]);
        assert_eq!(r.before_handoff, ["build", "dbsync"]);
        assert_eq!(r.instructions.as_deref(), Some("./instructions.md"));
        let m = crate::config::MachineConfig::load(&d.join("toryo.local.yaml")).unwrap();
        let g = &m.environment_gates["dbsync"];
        assert_eq!(g.lock, "d365-env");
        assert_eq!(g.program, "powershell");
        assert!(m.skills_dir.is_some());
        assert!(m.profiles.contains_key("codex-openai"));
    }

    /// M11-B1 AC-2: each removed key gets its own message, naming the key path.
    #[test]
    fn removed_keys_have_their_own_message() {
        let f = Path::new(".toryo/toryo-orgchart.yaml");
        let base = "toryo: 1\nteam: t\nseats:\n  planner: { profile: a }\n  implementer: { profile: a }\n  reviewer: { profile: a }\n";
        let err = |text: &str, kind| {
            let e = match kind {
                FileKind::OrgChart => parse_repo_file::<OrgChart>(text, f, kind).map(|_| ()),
                FileKind::Role => parse_repo_file::<RoleFile>(text, f, kind).map(|_| ()),
            };
            format!("{:#}", e.unwrap_err())
        };
        let e = err(
            &format!("{base}flow: [plan, build, review]\n"),
            FileKind::OrgChart,
        );
        assert!(
            e.contains("flow: the flow is fixed: plan → build → review") && e.contains("limits:"),
            "{e}"
        );
        let e = err(&format!("{base}skills_dir: C:/x\n"), FileKind::OrgChart);
        assert!(
            e.contains("skills_dir: `skills_dir` belongs in the machine config"),
            "{e}"
        );
        let e = err(
            &base.replace(
                "reviewer: { profile: a }",
                "reviewer: { profile: a, agent: codex }",
            ),
            FileKind::OrgChart,
        );
        assert!(
            e.contains("seats.reviewer.agent: a seat names a Toryo profile")
                && e.contains("`profile: codex-openai`"),
            "{e}"
        );
        let e = err("toryo: 1\npurpose: p\nreads: [PLAN.md]\n", FileKind::Role);
        assert!(
            e.contains("reads: Toryo decides what each stage sees") && e.contains("remove `reads`"),
            "{e}"
        );
        // all removed keys of a file at once
        let e = err(
            &format!("{base}flow: x\nskills_dir: y\n"),
            FileKind::OrgChart,
        );
        assert!(e.contains("flow:") && e.contains("skills_dir:"), "{e}");
        // a tag is refused before anything else (C1)
        let e = err(&format!("{base}!x culture: c.md\n"), FileKind::OrgChart);
        assert!(e.contains("key `culture` has the tag `!x`"), "{e}");
    }

    /// M11-B1 AC-2: exactly three seats — a fourth (or a misspelt one) is refused.
    #[test]
    fn seats_are_exactly_three() {
        let f = Path::new("o.yaml");
        let base = "toryo: 1\nteam: t\nseats:\n  planner: { profile: a }\n  implementer: { profile: a }\n  reviewer: { profile: a }\n";
        parse_repo_file::<OrgChart>(base, f, FileKind::OrgChart).unwrap();
        let e = parse_repo_file::<OrgChart>(
            &format!("{base}  tester: {{ profile: a }}\n"),
            f,
            FileKind::OrgChart,
        )
        .unwrap_err();
        assert!(format!("{e:#}").contains("tester"), "{e:#}");
        let e = parse_repo_file::<OrgChart>(
            &base.replace("  reviewer: { profile: a }\n", ""),
            f,
            FileKind::OrgChart,
        )
        .unwrap_err();
        assert!(format!("{e:#}").contains("reviewer"), "{e:#}");
    }

    /// A fake repository made from the fixture set (the owner's drafts, B1) plus what they
    /// need to load: planner and reviewer role files. Returns (dir, toryo.yaml, machine config).
    pub(crate) fn draft_repo(
        files: &[(&str, &str)],
    ) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let tmp = d.path().display().to_string().replace('\\', "/");
        let repo = d.path().join("repo");
        let src = fixtures();
        let mut all: Vec<(String, String)> = [
            ".toryo/toryo-orgchart.yaml",
            ".toryo/roles/implementer/toryo-role.yaml",
        ]
        .iter()
        .map(|f| {
            let text = std::fs::read_to_string(src.join(f)).unwrap();
            (f.to_string(), text.replace(CRLF, "\n"))
        })
        .collect();
        for (seat, purpose) in [("planner", "Plan the change."), ("reviewer", "Review it.")] {
            all.push((
                format!(".toryo/roles/{seat}/toryo-role.yaml"),
                format!("toryo: 1\npurpose: {purpose}\ninstructions: ./instructions.md\n"),
            ));
        }
        // what the drafts name: the culture, each role's instructions, the implementer's two
        // skills (xpp-axclass in the repository, xpp-batch in the machine's skills_dir)
        all.push(("CULTURE.md".into(), "Be careful.\n".into()));
        all.push((
            ".toryo/skills/xpp-axclass/SKILL.md".into(),
            "---\nname: xpp-axclass\ndescription: Create or change an X++ class (AxClass).\n---\n# AxClass\n".into(),
        ));
        let batch = d.path().join("machine-skills").join("xpp-batch");
        std::fs::create_dir_all(&batch).unwrap();
        std::fs::write(
            batch.join("SKILL.md"),
            "---\nname: xpp-batch\ndescription: Write a SysOperation batch job.\n---\n# Batch\n",
        )
        .unwrap();
        for seat in SEATS {
            all.push((
                format!(".toryo/roles/{seat}/instructions.md"),
                format!("{seat} house rules\n"),
            ));
        }
        all.extend(files.iter().map(|(p, c)| (p.to_string(), c.to_string())));
        for (p, c) in &all {
            let f = repo.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, c).unwrap();
        }
        let y = d.path().join("toryo.yaml");
        std::fs::write(
            &y,
            format!("version: 1\nproject: {{ root: '{tmp}/repo', worktrees_root: '{tmp}/wts' }}\n"),
        )
        .unwrap();
        let m = d.path().join("toryo.local.yaml");
        // CRLF too (a Windows clone with autocrlf): the tests edit it by exact text
        let machine = std::fs::read_to_string(src.join("toryo.local.yaml"))
            .unwrap()
            .replace(CRLF, "\n")
            .replace("@TMP@", &tmp);
        std::fs::write(&m, machine).unwrap();
        (d, y, m)
    }

    fn load_err(y: &Path, m: &Path) -> String {
        format!(
            "{:#}",
            crate::config::Effective::load(y, Some(m)).expect_err("must be refused")
        )
    }

    /// M11-B2 AC-1: the fixture set loads in org chart mode; every origin is known; the
    /// frozen copy comes back (without reading `.toryo/`) with the same digest.
    #[test]
    fn the_draft_set_loads_freezes_and_reloads() {
        use crate::config::{Effective, Origin};
        let (d, y, m) = draft_repo(&[]);
        let cfg = Effective::load(&y, Some(&m)).unwrap();
        let team = cfg.team.as_ref().expect("org chart mode");
        assert_eq!(team.name, "d365fo-dev");
        assert_eq!(team.culture.as_deref(), Some("CULTURE.md"));
        assert_eq!(
            team.role("implementer").unwrap().instructions.as_deref(),
            Some(".toryo/roles/implementer/instructions.md")
        );
        assert_eq!(team.handoff_gates().len(), 2);
        // the seats became roles; the check gate became the verify command
        assert_eq!(cfg.repo.roles["planner"].model.as_deref(), Some("opus"));
        assert_eq!(cfg.repo.roles["reviewer"].fallback, Some(vec![]));
        let v = &cfg.repo.commands.verify;
        assert_eq!(v.len(), 1);
        assert_eq!(
            (v[0].name.as_str(), v[0].timeout_sec),
            ("build", Some(1200))
        );
        // the environment gate's command is the machine's, frozen with the task
        let db = &team.gates["dbsync"];
        assert_eq!(db.machine.as_ref().unwrap().lock, "d365-env");
        assert_eq!(cfg.origins.gates["dbsync"], Origin::Machine);
        assert_eq!(cfg.origins.gates["build"], Origin::Repo);
        assert_eq!(cfg.origins.skills_dir, Some(Origin::Machine));
        assert!(team
            .skill_roots
            .machine
            .as_deref()
            .unwrap()
            .ends_with("/machine-skills"));
        for seat in SEATS {
            assert_eq!(cfg.origins.roles[seat], Origin::Repo, "{seat}");
        }
        let whats: Vec<&str> = cfg.files.iter().map(|f| f.what.as_str()).collect();
        assert_eq!(
            whats,
            [
                "toryo.yaml",
                "machine config",
                "org chart",
                "role planner",
                "role implementer",
                "role reviewer"
            ]
        );
        // config show lists every origin
        let text = crate::config_show::text(&cfg);
        for want in [
            "configuration: org chart (team d365fo-dev)",
            "planner      claude-max (model opus) — repo",
            "implementer  codex-openai → fallback claude-max — repo",
            "claude-max       claude, subscription — machine",
            "build        check          20m — repo",
            "dbsync       environment    30m — command: machine config `environment_gates.dbsync`, lock d365-env",
            "      runs: powershell -NoProfile -NonInteractive -File ",
            "/gates/dbsync.ps1\n",
            "before_handoff: build → dbsync",
            "/machine-skills — machine",
            "limits: max_gate_retries 3, max_review_rounds 3",
            "checks: reviewer-differs-from-builder warn",
        ] {
            assert!(text.contains(want), "{want}\n---\n{text}");
        }
        // frozen and back: the same digest, without `.toryo/`
        let frozen = cfg.canonical_json();
        assert!(frozen.contains(r#""team":{"checks""#));
        std::fs::remove_dir_all(d.path().join("repo").join(".toryo")).unwrap();
        let back = Effective::from_frozen(&frozen, &y).unwrap();
        assert_eq!(back.sha256(), cfg.sha256());
        assert_eq!(back.team, cfg.team);
    }

    /// M11-B2 AC-3: `toryo.yaml` may not also set what the org chart holds — each of the
    /// three keys, naming both files.
    #[test]
    fn a_setting_in_both_places_is_refused() {
        for (extra, key) in [
            ("roles:\n  planner: { profile: claude-max }\n", "roles"),
            (
                "commands:\n  verify:\n    - { name: b, program: git, args: [status] }\n",
                "commands",
            ),
            (
                "pipeline: { max_fix_cycles: 3 }\n",
                "pipeline.max_fix_cycles",
            ),
        ] {
            let (_d, y, m) = draft_repo(&[]);
            let text = std::fs::read_to_string(&y).unwrap();
            std::fs::write(&y, format!("{text}{extra}")).unwrap();
            let e = load_err(&y, &m);
            assert!(
                e.contains(&format!(
                    "{}: {key}: set here and in the org chart",
                    y.display()
                )) && e.contains("toryo-orgchart.yaml")
                    && e.contains("one place per setting"),
                "{key}: {e}"
            );
        }
        // other pipeline keys stay in toryo.yaml
        let (_d, y, m) = draft_repo(&[]);
        let text = std::fs::read_to_string(&y).unwrap();
        std::fs::write(&y, format!("{text}pipeline: {{ budget_tokens: 1000 }}\n")).unwrap();
        crate::config::Effective::load(&y, Some(&m)).unwrap();
    }

    /// M11-B2 AC-3: an omitted implementer fallback is a chain of one; a planner fallback is
    /// refused; a machine seat override wins and says so; a leftover `.toryo/roles/<role>.md`
    /// is refused with where to move it.
    #[test]
    fn fallbacks_overrides_and_leftover_role_files() {
        use crate::config::{Effective, Origin};
        let org = std::fs::read_to_string(fixtures().join(ORGCHART))
            .unwrap()
            .replace(CRLF, "\n");
        let no_fb = org.replace(
            "    fallback: [claude-max]         # takes over if Codex is out of quota\n",
            "",
        );
        assert_ne!(no_fb, org);
        let (_d, y, m) = draft_repo(&[(ORGCHART, &no_fb)]);
        let cfg = Effective::load(&y, Some(&m)).unwrap();
        assert_eq!(cfg.repo.roles["implementer"].fallback, Some(vec![]));
        let (_, chain, _) = crate::engine::task::roles(&cfg).unwrap();
        assert_eq!(chain, ["codex-openai"]);
        // a planner fallback
        let fb = org.replace(
            "    model: opus\n",
            "    model: opus\n    fallback: [codex-openai]\n",
        );
        let (_d, y, m) = draft_repo(&[(ORGCHART, &fb)]);
        let e = load_err(&y, &m);
        assert!(
            e.contains("seats.planner.fallback: a fallback on the planner is not supported"),
            "{e}"
        );
        // a machine seat override
        let (_d, y, m) = draft_repo(&[]);
        let text = std::fs::read_to_string(&m).unwrap();
        std::fs::write(
            &m,
            format!("{text}seats:\n  reviewer: {{ profile: codex-openai }}\n"),
        )
        .unwrap();
        let cfg = Effective::load(&y, Some(&m)).unwrap();
        assert_eq!(cfg.repo.roles["reviewer"].profile, "codex-openai");
        assert_eq!(
            cfg.origins.roles["reviewer"],
            Origin::RepoOverriddenByMachine
        );
        assert!(crate::config_show::text(&cfg)
            .contains("reviewer     codex-openai — repo, overridden by machine"));
        // ... which cannot give the reviewer a fallback either
        std::fs::write(
            &m,
            format!(
                "{text}seats:\n  reviewer: {{ profile: codex-openai, fallback: [claude-max] }}\n"
            ),
        )
        .unwrap();
        let e = load_err(&y, &m);
        assert!(
            e.contains("seats.reviewer.fallback (machine config")
                && e.contains("not supported yet"),
            "{e}"
        );
        // the M4 override file
        let (_d, y, m) = draft_repo(&[(".toryo/roles/reviewer.md", "OLD REVIEWER\n")]);
        let e = load_err(&y, &m);
        assert!(
            e.contains("reviewer.md: the M4 role override file is not read in org chart mode")
                && e.contains("move it to .toryo/roles/reviewer/instructions.md"),
            "{e}"
        );
    }

    /// The draft org chart / implementer role file with `from` replaced by `to` (it must be
    /// there).
    fn draft_with(file: &str, from: &str, to: &str) -> String {
        let text = std::fs::read_to_string(fixtures().join(file))
            .unwrap()
            .replace(CRLF, "\n");
        assert!(text.contains(from), "{from:?} not in {file}");
        text.replacen(from, to, 1)
    }

    const IMPL_ROLE: &str = ".toryo/roles/implementer/toryo-role.yaml";

    /// M11-B3 AC-3: one close candidate is suggested; a tie or nothing close is not.
    #[test]
    fn did_you_mean_suggests_only_one_close_candidate() {
        let seats = ["planner", "implementer", "reviewer"];
        assert_eq!(did_you_mean("reviwer", &seats), Some("reviewer"));
        assert_eq!(did_you_mean("implementor", &seats), Some("implementer"));
        assert_eq!(did_you_mean("Reviewer", &seats), Some("reviewer"));
        assert_eq!(did_you_mean("tester", &seats), None);
        assert_eq!(did_you_mean("dbsnyc", &["dbsync", "build"]), Some("dbsync"));
        // two equally close: no guess
        assert_eq!(did_you_mean("bat", &["cat", "hat"]), None);
        assert_eq!(did_you_mean("x", &[]), None);
        // a case-insensitive match wins over a closer edit
        assert_eq!(did_you_mean("BUILD", &["build", "buil"]), Some("build"));
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("", "ab"), 2);
        assert_eq!(hint("tester", &seats), "");
        assert_eq!(hint("reviwer", &seats), " Did you mean 'reviewer'?");
    }

    /// M11-B3 AC-1: unknown keys get the friendly form with the key path and the hint — a
    /// misspelt seat, a top-level key, a gate field, a machine-config key.
    #[test]
    fn unknown_keys_name_the_path_and_the_hint() {
        let (_d, y, m) = draft_repo(&[(
            ORGCHART,
            &draft_with(ORGCHART, "  reviewer:\n", "  reviwer:\n"),
        )]);
        let e = load_err(&y, &m);
        assert!(e.contains("toryo-orgchart.yaml: seats.reviwer: seat 'reviwer' not found. Did you mean 'reviewer'?"), "{e}");
        let (_d, y, m) = draft_repo(&[(ORGCHART, &draft_with(ORGCHART, "\nlimits:", "\nlimts:"))]);
        let e = load_err(&y, &m);
        assert!(
            e.contains("toryo-orgchart.yaml: limts: unknown key 'limts'. Did you mean 'limits'?"),
            "{e}"
        );
        let (_d, y, m) = draft_repo(&[(
            ORGCHART,
            &draft_with(ORGCHART, "    timeout: 30m\n", "    timout: 30m\n"),
        )]);
        let e = load_err(&y, &m);
        assert!(
            e.contains("gates.dbsync.timout: unknown key 'timout'. Did you mean 'timeout'?"),
            "{e}"
        );
        let (_d, y, m) =
            draft_repo(&[(IMPL_ROLE, &draft_with(IMPL_ROLE, "\nskills:", "\nskils:"))]);
        let e = load_err(&y, &m);
        assert!(
            e.contains("toryo-role.yaml: skils: unknown key 'skils'. Did you mean 'skills'?"),
            "{e}"
        );
        let (_d, y, m) = draft_repo(&[]);
        let text = std::fs::read_to_string(&m).unwrap();
        std::fs::write(&m, text.replace("\nskills_dir:", "\nskils_dir:")).unwrap();
        let e = load_err(&y, &m);
        assert!(
            e.contains(&format!(
                "{}: skils_dir: unknown key 'skils_dir'. Did you mean 'skills_dir'?",
                m.display()
            )),
            "{e}"
        );
    }

    /// M11-B3 AC-1: seat profiles must exist in the machine config (R60), with a hint.
    #[test]
    fn a_seat_profile_must_exist_on_the_machine() {
        let (_d, y, m) = draft_repo(&[(
            ORGCHART,
            &draft_with(ORGCHART, "profile: codex-openai ", "profile: codex-opnai "),
        )]);
        let e = load_err(&y, &m);
        assert!(
            e.contains("toryo-orgchart.yaml: seats.implementer.profile: profile 'codex-opnai' is not defined in the machine config")
                && e.contains("profiles live only there (R60). Did you mean 'codex-openai'?"),
            "{e}"
        );
        let (_d, y, m) = draft_repo(&[(
            ORGCHART,
            &draft_with(ORGCHART, "fallback: [claude-max]", "fallback: [claude-mx]"),
        )]);
        let e = load_err(&y, &m);
        assert!(
            e.contains("seats.implementer.fallback: profile 'claude-mx'")
                && e.contains("Did you mean 'claude-max'?"),
            "{e}"
        );
    }

    /// M11-B3 AC-1: every `before_handoff` name is a declared gate (with a hint); the order
    /// rule; only the implementer has `before_handoff`, `writes` and `never`.
    #[test]
    fn before_handoff_names_order_and_owner() {
        let (_d, y, m) = draft_repo(&[(
            IMPL_ROLE,
            &draft_with(IMPL_ROLE, "[build, dbsync]", "[build, dbsnyc]"),
        )]);
        let e = load_err(&y, &m);
        assert!(
            e.contains("implementer")
                && e.contains(
                    "toryo-role.yaml: before_handoff: gate 'dbsnyc' is not declared under `gates:`"
                )
                && e.contains("Did you mean 'dbsync'?"),
            "{e}"
        );
        let (_d, y, m) = draft_repo(&[(
            IMPL_ROLE,
            &draft_with(IMPL_ROLE, "[build, dbsync]", "[dbsync, build]"),
        )]);
        let e = load_err(&y, &m);
        assert!(
            e.contains("before_handoff: check gate 'build' comes after environment gate 'dbsync'"),
            "{e}"
        );
        let (_d, y, m) = draft_repo(&[(
            ".toryo/roles/reviewer/toryo-role.yaml",
            "toryo: 1\npurpose: r\nbefore_handoff: [build]\nwrites: [x/**]\nnever: [y]\n",
        )]);
        let e = load_err(&y, &m);
        for k in ["before_handoff", "writes", "never"] {
            assert!(
                e.contains(&format!(
                    "reviewer{}toryo-role.yaml: {k}: only the implementer may have `{k}`",
                    std::path::MAIN_SEPARATOR
                )),
                "{k}: {e}"
            );
        }
        let (_d, y, m) = draft_repo(&[(
            IMPL_ROLE,
            &draft_with(
                IMPL_ROLE,
                "before_handoff: [build, dbsync]",
                "before_handoff: []",
            ),
        )]);
        let e = load_err(&y, &m);
        assert!(e.contains("before_handoff: no check gate"), "{e}");
        // environment gates alone do not verify the work either (group B review)
        let (_d, y, m) = draft_repo(&[(
            IMPL_ROLE,
            &draft_with(IMPL_ROLE, "[build, dbsync]", "[dbsync]"),
        )]);
        let e = load_err(&y, &m);
        assert!(e.contains("before_handoff: no check gate"), "{e}");
    }

    /// Group B review: the loader's own problems, each with its file and key path — a missing
    /// role file, a version other than 1, `max_fix_cycles` with a split limit, a bad timeout,
    /// a check gate with no command, absolute culture / instructions paths, a missing seat.
    #[test]
    fn the_loader_problems_name_their_key() {
        let (d, y, m) = draft_repo(&[]);
        std::fs::remove_file(
            d.path()
                .join("repo")
                .join(".toryo/roles/reviewer/toryo-role.yaml"),
        )
        .unwrap();
        let e = load_err(&y, &m);
        assert!(e.contains("seat `reviewer` has no role file"), "{e}");
        let org = draft_with(ORGCHART, "\ntoryo: 1\n", "\ntoryo: 2\n")
            .replace("    timeout: 20m\n", "    timeout: 20min\n")
            .replace(
                "  max_review_rounds: 3\n",
                "  max_review_rounds: 3\n  max_fix_cycles: 2\n",
            )
            .replace("culture: ./CULTURE.md", "culture: /etc/CULTURE.md");
        let role = draft_with(IMPL_ROLE, "\ntoryo: 1\n", "\ntoryo: 3\n").replace(
            "instructions: ./instructions.md",
            "instructions: ../../../x.md",
        );
        let (_d, y, m) = draft_repo(&[(ORGCHART, &org), (IMPL_ROLE, &role)]);
        let e = load_err(&y, &m);
        for want in [
            "toryo-orgchart.yaml: toryo: unsupported version 2",
            "toryo-role.yaml: toryo: unsupported version 3",
            "limits.max_fix_cycles: `max_fix_cycles` is the single counter",
            "gates.build.timeout: `20min` is not a duration",
            "culture: `/etc/CULTURE.md` must be relative to the repository",
            "instructions: `../../../x.md` may not contain `..`",
        ] {
            assert!(e.contains(want), "{want}\n---\n{e}");
        }
        let org = draft_with(
            ORGCHART,
            "    program: pwsh\n    args: [-NoProfile, -NonInteractive, -File, scripts/build-model.ps1]\n",
            "",
        );
        let (_d, y, m) = draft_repo(&[(ORGCHART, &org)]);
        let e = load_err(&y, &m);
        assert!(
            e.contains("gates.build: needs `program` (with `args`) or `tool`"),
            "{e}"
        );
        // a missing seat, in the friendly form
        let org = draft_with(
            ORGCHART,
            "  reviewer:\n    profile: claude-max ",
            "  # reviewer gone\n    #",
        );
        let (_d, y, m) = draft_repo(&[(ORGCHART, &org)]);
        let e = load_err(&y, &m);
        assert!(
            e.contains("seats: seat 'reviewer' is missing — exactly three seats: planner, implementer, reviewer"),
            "{e}"
        );
    }

    /// Group B review: a machine seat override without `fallback` has none (D7), not the
    /// planner's profile.
    #[test]
    fn a_machine_seat_override_has_no_implicit_fallback() {
        let (_d, y, m) = draft_repo(&[]);
        let text = std::fs::read_to_string(&m).unwrap();
        std::fs::write(
            &m,
            format!("{text}seats:\n  implementer: {{ profile: claude-max }}\n"),
        )
        .unwrap();
        let cfg = crate::config::Effective::load(&y, Some(&m)).unwrap();
        assert_eq!(cfg.repo.roles["implementer"].fallback, Some(vec![]));
        let (_, chain, _) = crate::engine::task::roles(&cfg).unwrap();
        assert_eq!(chain, ["claude-max"]);
    }

    /// Group B review: the reach check of the machine's environment-gate command — the
    /// worktrees and the verify root too, an absolute program inside a root, `..` segments, a
    /// path in a `--flag=value`; a Windows switch (`/c`) is not a path.
    #[test]
    fn an_environment_gate_command_stays_out_of_reach() {
        let (d, y, m) = draft_repo(&[]);
        let tmp = d.path().display().to_string().replace('\\', "/");
        let text = std::fs::read_to_string(&m).unwrap();
        let args = format!(
            "[/c, '{tmp}/wts/T-1/x.ps1', '{tmp}/elsewhere/../repo/y.ps1', '--script={tmp}/repo/z.ps1', '{tmp}/vr/T-1/v.ps1', '{tmp}/gates/ok.ps1']"
        );
        let bad = text
            .replace(
                "program: powershell ",
                &format!("program: '{tmp}/repo/tools/ps.exe' "),
            )
            .replace(
                &format!("[-NoProfile, -NonInteractive, -File, '{tmp}/gates/dbsync.ps1']"),
                &args,
            );
        std::fs::write(&m, format!("{bad}verify: {{ verify_root: '{tmp}/vr' }}\n")).unwrap();
        let e = load_err(&y, &m);
        for want in [
            "/repo/tools/ps.exe` is inside the repository",
            "/wts/T-1/x.ps1` is inside the task worktrees",
            "/elsewhere/../repo/y.ps1` may not contain `..`",
            "--script=",
            "is inside the repository",
            "/vr/T-1/v.ps1` is inside the verify root",
        ] {
            assert!(e.contains(want), "{want}\n---\n{e}");
        }
        assert!(!e.contains("`/c`"), "a switch is not a path: {e}");
        assert!(!e.contains("gates/ok.ps1"), "{e}");
    }

    /// M11-R3 AC-2/AC-3 (X3): `%VAR%` in an environment gate's program and args is expanded
    /// once, at load. An unset variable is a problem naming the key; a variable that expands
    /// into the repository is a reach problem; the expanded text is what the config keeps.
    #[test]
    fn environment_gate_variables_are_expanded_at_load() {
        let (d, y, m) = draft_repo(&[]);
        let tmp = d.path().display().to_string().replace('\\', "/");
        let text = std::fs::read_to_string(&m).unwrap();
        let with = |program: &str, script: &str| {
            text.replace("program: powershell ", &format!("program: '{program}' "))
                .replace(&format!("'{tmp}/gates/dbsync.ps1'"), &format!("'{script}'"))
        };
        // AC-2: unset — in program and in args[3], each named
        std::fs::write(
            &m,
            with("%TORYO_R3_UNSET_P%/ps.exe", "%TORYO_R3_UNSET_A%/dbsync.ps1"),
        )
        .unwrap();
        let e = load_err(&y, &m);
        assert!(
            e.contains("environment_gates.dbsync.program: environment variable %TORYO_R3_UNSET_P% is not set")
                && e.contains("environment_gates.dbsync.args[3]: environment variable %TORYO_R3_UNSET_A% is not set")
                && e.contains(&m.display().to_string()),
            "{e}"
        );
        // reported once: not also as a relative program or argument (R1–R3 review)
        assert!(
            !e.contains("must be an absolute path") && e.matches("TORYO_R3_UNSET_A").count() == 2,
            "{e}"
        );
        // AC-3: a variable that points into the repository
        std::env::set_var("TORYO_R3_INTO_REPO", format!("{tmp}/repo/tools"));
        std::fs::write(&m, with("powershell", "%TORYO_R3_INTO_REPO%/dbsync.ps1")).unwrap();
        let e = load_err(&y, &m);
        assert!(
            e.contains(&format!(
                "`{tmp}/repo/tools/dbsync.ps1` is inside the repository"
            )),
            "{e}"
        );
        // the expanded text is stored (and so frozen and hashed)
        std::env::set_var("TORYO_R3_GATES", format!("{tmp}/gates"));
        std::fs::write(&m, with("powershell", "%TORYO_R3_GATES%/dbsync.ps1")).unwrap();
        let cfg = crate::config::Effective::load(&y, Some(&m)).unwrap();
        let g = cfg.team.as_ref().unwrap().gates["dbsync"]
            .machine
            .clone()
            .unwrap();
        assert_eq!(g.args[3], format!("{tmp}/gates/dbsync.ps1"));
        assert_eq!(g.program, "powershell");
    }

    /// M11-R8: the machine's `skills_dir` is trusted text — after `%VAR%` it must be absolute
    /// and outside the repository, the worktrees and the verify root; a folder that does not
    /// exist is a `doctor` WARN, not a load problem.
    #[test]
    fn the_machine_skills_dir_is_absolute_and_out_of_reach() {
        let (d, y, m) = draft_repo(&[]);
        let tmp = d.path().display().to_string().replace('\\', "/");
        let text = std::fs::read_to_string(&m).unwrap();
        let line = format!("skills_dir: '{tmp}/machine-skills'");
        assert!(text.contains(&line), "{text}");
        let cfg = crate::config::Effective::load(&y, Some(&m)).unwrap();
        let ok = crate::doctor::skills_dir_checks(&cfg);
        assert_eq!(ok[0].level, crate::doctor::Level::Ok, "{:?}", ok[0].message);
        for (dir, want) in [
            ("machine-skills".to_string(), "must be an absolute path"),
            (format!("{tmp}/repo/.toryo/x"), "is inside the repository"),
            (format!("{tmp}/wts/shared"), "is inside the task worktrees"),
            (
                "%TORYO_R8_UNSET%/skills".to_string(),
                "%TORYO_R8_UNSET% is not set",
            ),
        ] {
            std::fs::write(&m, text.replace(&line, &format!("skills_dir: '{dir}'"))).unwrap();
            let e = load_err(&y, &m);
            assert!(
                e.contains(&format!("{}: skills_dir:", m.display())) && e.contains(want),
                "{dir}: {e}"
            );
        }
        // (the fixture's implementer needs xpp-batch, found only in the machine folder so far)
        let batch = d
            .path()
            .join("repo")
            .join(".toryo")
            .join("skills")
            .join("xpp-batch");
        std::fs::create_dir_all(&batch).unwrap();
        std::fs::write(
            batch.join("SKILL.md"),
            "---\ndescription: d\n---\n# Batch\n",
        )
        .unwrap();
        std::fs::write(
            &m,
            text.replace(&line, &format!("skills_dir: '{tmp}/no-such-folder'")),
        )
        .unwrap();
        let cfg = crate::config::Effective::load(&y, Some(&m)).unwrap();
        let w = crate::doctor::skills_dir_checks(&cfg);
        assert!(
            w[0].level == crate::doctor::Level::Warn && w[0].message.contains("does not exist"),
            "{:?}",
            w[0].message
        );
    }

    /// M11-R4: `writes` / `never` are normalised at load (the frozen config holds the
    /// normalised form); a pattern that could never match is a problem naming the role file,
    /// the key and the pattern as written.
    #[test]
    fn fence_patterns_are_normalised_at_load() {
        let role = draft_with(
            IMPL_ROLE,
            "writes: [Metadata/**]",
            "writes: ['./Metadata/', src]",
        );
        let (_d, y, m) = draft_repo(&[(IMPL_ROLE, &role)]);
        let cfg = crate::config::Effective::load(&y, Some(&m)).unwrap();
        let r = cfg.team.as_ref().unwrap().role("implementer").unwrap();
        assert_eq!(r.writes, ["Metadata/**", "src/**"]);
        assert!(cfg
            .canonical_json()
            .contains(r#""writes":["Metadata/**","src/**"]"#));
        let role = draft_with(
            IMPL_ROLE,
            r#"never:  ["**/Descriptor/*.xml"]"#,
            r#"never: ['/secrets/**', '**/*.{pfx,key}', '**/Descriptor/*.xml']"#,
        );
        let (_d, y, m) = draft_repo(&[(IMPL_ROLE, &role)]);
        let e = load_err(&y, &m);
        assert!(
            e.contains("toryo-role.yaml: never: `/secrets/**` is absolute")
                && e.contains("toryo-role.yaml: never: `**/*.{pfx,key}` uses `{`")
                && !e.contains("Descriptor"),
            "{e}"
        );
    }

    /// M11-R3 AC-4: a bare environment-gate program is resolved through PATH at run time; when
    /// PATH finds it inside the repository (or the worktrees, or the verify root) it is refused,
    /// naming the resolved file. Outside, it is used.
    #[test]
    fn a_bare_gate_program_found_inside_the_repository_is_refused() {
        let (d, y, m) = draft_repo(&[]);
        let cfg = crate::config::Effective::load(&y, Some(&m)).unwrap();
        let exe = if cfg!(windows) {
            "dbsync-r3.exe"
        } else {
            "dbsync-r3"
        };
        let (inside, outside) = (d.path().join("repo").join("tools"), d.path().join("bin"));
        for dir in [&inside, &outside] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(exe), "x").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let f = dir.join(exe);
                std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let path = |dirs: &[&std::path::PathBuf]| std::env::join_paths(dirs).unwrap();
        let program = "dbsync-r3";
        let e = crate::engine::runner::env_gate_program(&cfg, program, Some(&path(&[&inside])))
            .unwrap_err();
        assert!(
            e.contains("program `dbsync-r3` resolves to")
                && e.contains(&inside.join(exe).display().to_string())
                && e.contains("inside the repository"),
            "{e}"
        );
        // first on PATH wins: outside the roots it is fine
        let ok = crate::engine::runner::env_gate_program(
            &cfg,
            program,
            Some(&path(&[&outside, &inside])),
        )
        .unwrap();
        assert_eq!(ok, outside.join(exe));
        // not on PATH at all
        let e =
            crate::engine::runner::env_gate_program(&cfg, "no-such-r3", Some(&path(&[&outside])))
                .unwrap_err();
        assert!(e.contains("not found on PATH"), "{e}");
    }

    /// M11-R9 (X6 replaces X2; was `an_environment_gate_needs_a_clean_checkout`): the
    /// environment gates deploy from a fresh export of the commit, not from the checkout, so a
    /// `tracked` checkout with an environment gate loads.
    #[test]
    fn a_tracked_checkout_with_an_environment_gate_loads() {
        let (_d, y, m) = draft_repo(&[]);
        let text = std::fs::read_to_string(&y).unwrap();
        std::fs::write(&y, format!("{text}verify: {{ checkout: tracked }}\n")).unwrap();
        let cfg = crate::config::Effective::load(&y, Some(&m)).unwrap();
        assert_eq!(cfg.repo.verify.checkout, "tracked");
        assert!(cfg
            .team
            .unwrap()
            .handoff_gates()
            .iter()
            .any(|(n, _)| *n == "dbsync"));
        // without an environment gate in before_handoff, `tracked` is fine too
        let (_d, y, m) = draft_repo(&[(
            IMPL_ROLE,
            &draft_with(IMPL_ROLE, "[build, dbsync]", "[build]"),
        )]);
        let text = std::fs::read_to_string(&y).unwrap();
        std::fs::write(&y, format!("{text}verify: {{ checkout: tracked }}\n")).unwrap();
        crate::config::Effective::load(&y, Some(&m)).unwrap();
    }

    /// M11-B3 AC-1: which fields each gate kind has.
    #[test]
    fn gate_kinds_and_their_fields() {
        let org = draft_with(
            ORGCHART,
            "    timeout: 20m\n",
            "    timeout: 20m\n    tool: xpp\n    if_missing: skip\n",
        );
        let org = org.replacen(
            "    kind: environment     # command: toryo.local.yaml -> environment_gates.dbsync\n",
            "    kind: environment\n    program: pwsh\n    args: [-File, x.ps1]\n",
            1,
        );
        let (_d, y, m) = draft_repo(&[(ORGCHART, &org)]);
        let e = load_err(&y, &m);
        for want in [
            "gates.build: has both `program` and `tool` — use one",
            "gates.build.if_missing: only for an environment gate",
            "gates.dbsync.program: an environment gate has no command in the repository",
            "gates.dbsync.args: an environment gate has no command in the repository",
            "its command is `environment_gates.dbsync` in the machine config",
        ] {
            assert!(e.contains(want), "{want}\n---\n{e}");
        }
        // tool: xpp needs metadata, projects and the machine's toolchain, as in commands.verify
        let org = draft_with(
            ORGCHART,
            "    program: pwsh\n    args: [-NoProfile, -NonInteractive, -File, scripts/build-model.ps1]\n",
            "    tool: xpp\n",
        );
        let (_d, y, m) = draft_repo(&[(ORGCHART, &org)]);
        let e = load_err(&y, &m);
        assert!(
            e.contains("gates.build.metadata: required for `tool: xpp`")
                && e.contains("gates.build.projects: `tool: xpp` needs at least one project")
                && e.contains("gates.build.tool: `tool: xpp` needs `toolchains.xpp`"),
            "{e}"
        );
    }

    /// M11-B3 AC-1: an environment gate the machine does not define is refused with
    /// `if_missing: error` (the default), allowed with `skip`; the machine's command must be
    /// out of the agents' reach.
    #[test]
    fn environment_gates_on_this_machine() {
        let (_d, y, m) = draft_repo(&[]);
        let text = std::fs::read_to_string(&m).unwrap();
        let start = text.find("environment_gates:").unwrap();
        let end = start + text[start..].find("    lock: d365-env\n").unwrap() + 19;
        let without = format!("{}{}", &text[..start], &text[end..]);
        std::fs::write(&m, &without).unwrap();
        let e = load_err(&y, &m);
        assert!(
            e.contains("gates.dbsync: environment gate 'dbsync' is not defined on this machine")
                && e.contains("add `environment_gates.dbsync` to the machine config")
                && e.contains("`if_missing: skip`"),
            "{e}"
        );
        let (_d2, y2, m2) = draft_repo(&[(
            ORGCHART,
            &draft_with(ORGCHART, "if_missing: error", "if_missing: skip"),
        )]);
        std::fs::write(&m2, &without).unwrap();
        let cfg = crate::config::Effective::load(&y2, Some(&m2)).unwrap();
        assert!(cfg.team.unwrap().gates["dbsync"].machine.is_none());
        // the machine's command: a lock, an absolute or PATH program, absolute args outside
        // the repository, the worktrees and the verify root
        let (d, y, m) = draft_repo(&[]);
        let tmp = d.path().display().to_string().replace('\\', "/");
        let text = std::fs::read_to_string(&m).unwrap();
        let bad = text
            .replace("program: powershell ", "program: tools/ps.exe ")
            .replace(
                &format!("'{tmp}/gates/dbsync.ps1'"),
                &format!("gates/dbsync.ps1, '{tmp}/repo/scripts/sync.ps1'"),
            )
            .replace("lock: d365-env", "lock: ''");
        std::fs::write(&m, bad).unwrap();
        let e = load_err(&y, &m);
        for want in [
            "environment_gates.dbsync.lock: required",
            "environment_gates.dbsync.program: `tools/ps.exe` must be an absolute path or a bare name found on PATH",
            "environment_gates.dbsync.args: `gates/dbsync.ps1` must be an absolute path",
            "/repo/scripts/sync.ps1` is inside the repository",
        ] {
            assert!(e.contains(want), "{want}\n---\n{e}");
        }
    }

    /// M11-B3 AC-1: the culture and instructions files exist.
    #[test]
    fn culture_and_instructions_must_exist() {
        let (d, y, m) = draft_repo(&[]);
        std::fs::remove_file(d.path().join("repo").join("CULTURE.md")).unwrap();
        std::fs::remove_file(
            d.path()
                .join("repo")
                .join(".toryo/roles/reviewer/instructions.md"),
        )
        .unwrap();
        let e = load_err(&y, &m);
        assert!(
            e.contains("toryo-orgchart.yaml: culture:") && e.contains("CULTURE.md not found"),
            "{e}"
        );
        assert!(
            e.contains("toryo-role.yaml: instructions:")
                && e.contains("reviewer/instructions.md not found"),
            "{e}"
        );
    }

    /// M11-B3 AC-2: a file with three problems reports all three in one error.
    #[test]
    fn three_problems_are_reported_at_once() {
        let role = draft_with(IMPL_ROLE, "[build, dbsync]", "[dbsync, build, nope]");
        let org = draft_with(ORGCHART, "profile: codex-openai ", "profile: codex-opnai ");
        let (d, y, m) = draft_repo(&[(IMPL_ROLE, &role), (ORGCHART, &org)]);
        std::fs::remove_file(d.path().join("repo").join("CULTURE.md")).unwrap();
        let e = load_err(&y, &m);
        for want in [
            "profile 'codex-opnai' is not defined",
            "gate 'nope' is not declared",
            "check gate 'build' comes after environment gate 'dbsync'",
            "CULTURE.md not found",
        ] {
            assert!(e.contains(want), "{want}\n---\n{e}");
        }
        assert!(e.contains("invalid configuration:"), "{e}");
    }

    /// M11-D1 AC-1: a name in both places resolves to the repository copy; only in the
    /// machine folder → machine; nowhere → an error naming both paths.
    #[test]
    fn skills_resolve_repository_first() {
        let d = tempfile::tempdir().unwrap();
        let (repo, machine) = (d.path().join("repo"), d.path().join("shared"));
        let put = |p: PathBuf, body: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        put(
            repo.join(".toryo/skills/both/SKILL.md"),
            "---\ndescription: repo copy\n---\nR\n",
        );
        put(
            machine.join("both/SKILL.md"),
            "---\ndescription: machine copy\n---\nM\n",
        );
        put(
            machine.join("only-machine/SKILL.md"),
            "---\ndescription: m\n---\nX\n",
        );
        let s = resolve_skill("both", &repo, Some(&machine)).unwrap();
        assert_eq!(
            (s.origin, s.description.as_deref(), s.text.as_str()),
            (
                SkillOrigin::Repo,
                Some("repo copy"),
                "---\ndescription: repo copy\n---\nR\n"
            )
        );
        assert_eq!(s.sha256, crate::engine::task::sha256_hex(s.text.as_bytes()));
        assert_eq!(s.bytes, s.text.len());
        let m = resolve_skill("only-machine", &repo, Some(&machine)).unwrap();
        assert_eq!(m.origin, SkillOrigin::Machine);
        assert_eq!(m.path, machine.join("only-machine").join("SKILL.md"));
        let e = resolve_skill("nowhere", &repo, Some(&machine)).unwrap_err();
        for want in [
            "skill 'nowhere' not found",
            &repo
                .join(".toryo")
                .join("skills")
                .join("nowhere")
                .join("SKILL.md")
                .display()
                .to_string(),
            &machine
                .join("nowhere")
                .join("SKILL.md")
                .display()
                .to_string(),
        ] {
            assert!(e.contains(want), "{want}: {e}");
        }
        let e = resolve_skill("nowhere", &repo, None).unwrap_err();
        assert!(e.contains("no `skills_dir` in the machine config"), "{e}");
        for bad in ["../x", "a/b", r"a\b", "C:x", "", ".."] {
            assert!(resolve_skill(bad, &repo, Some(&machine)).is_err(), "{bad}");
        }
        assert_eq!(skill_names(&repo, Some(&machine)), ["both", "only-machine"]);
    }

    /// M11-D1 AC-2: front matter — missing, without `description`, CRLF line ends, a UTF-8
    /// BOM, an unclosed block.
    #[test]
    fn skill_front_matter() {
        assert_eq!(skill_description("# no front matter\n"), None);
        assert_eq!(skill_description("---\nname: x\n---\nbody\n"), None);
        assert_eq!(skill_description("---\ndescription: '  '\n---\n"), None);
        assert_eq!(
            skill_description(
                "---\r\nname: x\r\ndescription: Make a batch job.\r\n---\r\nbody\r\n"
            ),
            Some("Make a batch job.".into())
        );
        assert_eq!(
            skill_description("\u{feff}---\ndescription: with a BOM\n---\n"),
            Some("with a BOM".into())
        );
        assert_eq!(skill_description("---\ndescription: never closed\n"), None);
        assert_eq!(
            skill_description("text\n---\ndescription: later\n---\n"),
            None
        );
        let fm = front_matter("---\nname: xpp-batch\ndescription: d\n---\n").unwrap();
        assert_eq!(fm.get("name").and_then(|v| v.as_str()), Some("xpp-batch"));
    }

    /// M11-D1 (B3): an unknown skill names both places with a hint; an implementer skill without
    /// a description is refused.
    #[test]
    fn skills_are_validated_at_load() {
        let (_d, y, m) = draft_repo(&[(
            IMPL_ROLE,
            &draft_with(
                IMPL_ROLE,
                "[xpp-axclass, xpp-batch]",
                "[xpp-axclass, xpp-bach]",
            ),
        )]);
        let e = load_err(&y, &m);
        assert!(
            e.contains("toryo-role.yaml: skills: skill 'xpp-bach' not found: neither")
                && e.contains("Did you mean 'xpp-batch'?"),
            "{e}"
        );
        let (_d, y, m) =
            draft_repo(&[(".toryo/skills/xpp-axclass/SKILL.md", "# no front matter\n")]);
        let e = load_err(&y, &m);
        assert!(
            e.contains("skill 'xpp-axclass' (repo) has no `description` in its front matter"),
            "{e}"
        );
        // planner and reviewer skills need no description
        let (d, y, m) = draft_repo(&[
            (".toryo/skills/review-checklist/SKILL.md", "# checklist\n"),
            (
                ".toryo/roles/reviewer/toryo-role.yaml",
                "toryo: 1\npurpose: Review it.\ninstructions: ./instructions.md\nskills: [review-checklist]\n",
            ),
        ]);
        crate::config::Effective::load(&y, Some(&m)).unwrap();
        drop(d);
    }

    /// Copy the directory tree `from` into `to`.
    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap().flatten() {
            let (src, dst) = (e.path(), to.join(e.file_name()));
            if src.is_dir() {
                copy_tree(&src, &dst);
            } else {
                std::fs::copy(&src, &dst).unwrap();
            }
        }
    }

    /// M11-F1 AC-1: the D365FO org chart template set — `.toryo/` (org chart, three role
    /// files, instructions, skills), CULTURE.md and toryo.orgchart.yaml.example — loads with
    /// fake profiles and a machine `environment_gates.dbsync`; every new file starts with its
    /// TODO(owner) items.
    #[test]
    fn the_d365fo_org_chart_templates_load() {
        let t = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("templates")
            .join("d365fo");
        let d = tempfile::tempdir().unwrap();
        let tmp = d.path().display().to_string().replace('\\', "/");
        let repo = d.path().join("repo");
        copy_tree(&t.join(".toryo"), &repo.join(".toryo"));
        std::fs::copy(t.join("CULTURE.md"), repo.join("CULTURE.md")).unwrap();
        let yaml = std::fs::read_to_string(t.join("toryo.orgchart.yaml.example"))
            .unwrap()
            .replace(r"root: D:\D365-Git", &format!("root: '{tmp}/repo'"))
            .replace(
                r"worktrees_root: D:\D365-worktrees",
                &format!("worktrees_root: '{tmp}/wts'"),
            );
        let y = d.path().join("toryo.yaml");
        std::fs::write(&y, yaml).unwrap();
        let m = d.path().join("machine.yaml");
        std::fs::write(
            &m,
            format!("profiles:\n  claude-max: {{ kind: claude, billing: subscription, exe: claude }}\n  codex-openai: {{ kind: codex, billing: subscription, exe: codex, codex: {{ windows_sandbox: unelevated }} }}\nenvironment_gates:\n  dbsync: {{ program: powershell, args: [-NoProfile, -File, '{tmp}/gates/dbsync.ps1'], lock: d365-env }}\n"),
        )
        .unwrap();
        let cfg = crate::config::Effective::load(&y, Some(&m)).unwrap();
        let team = cfg.team.as_ref().expect("org chart mode");
        assert_eq!(team.name, "d365fo-dev");
        assert_eq!(
            team.role("implementer").unwrap().skills,
            ["xpp-axclass", "xpp-batch"]
        );
        assert!(team.gates["dbsync"].machine.is_some());
        assert_eq!(cfg.repo.commands.verify[0].name, "build");
        // the TODO(owner) items at the top of every new file
        let new_files = [
            t.join(".toryo/toryo-orgchart.yaml"),
            t.join(".toryo/roles/planner/toryo-role.yaml"),
            t.join(".toryo/roles/implementer/toryo-role.yaml"),
            t.join(".toryo/roles/reviewer/toryo-role.yaml"),
            t.join(".toryo/roles/planner/instructions.md"),
            t.join(".toryo/roles/implementer/instructions.md"),
            t.join(".toryo/roles/reviewer/instructions.md"),
            t.join("CULTURE.md"),
            t.join("toryo.orgchart.yaml.example"),
        ];
        for f in new_files {
            let text = std::fs::read_to_string(&f).unwrap();
            let head: Vec<&str> = if f.extension().is_some_and(|x| x == "md") {
                text.lines().take(1).collect()
            } else {
                text.lines().take_while(|l| l.starts_with('#')).collect()
            };
            assert!(
                head.iter().any(|l| l.contains("TODO(owner)")),
                "{}: TODO(owner) items at the top",
                f.display()
            );
        }
        for s in ["xpp-axclass", "xpp-batch"] {
            let text =
                std::fs::read_to_string(t.join(".toryo/skills").join(s).join("SKILL.md")).unwrap();
            assert!(skill_description(&text).is_some_and(|d| d.contains("TODO(owner)")));
        }
        // the toryo.yaml-only example does not load next to the org chart: one place per setting
        let old = std::fs::read_to_string(t.join("toryo.yaml.example"))
            .unwrap()
            .replace(r"root: D:\D365-Git", &format!("root: '{tmp}/repo'"));
        std::fs::write(&y, old).unwrap();
        let e = load_err(&y, &m);
        assert!(e.contains("one place per setting"), "{e}");
    }

    /// M11-B1 AC-2: the duration parser.
    #[test]
    fn durations() {
        for (s, want) in [
            ("30s", 30),
            ("20m", 1200),
            ("1h", 3600),
            (" 90m ", 5400),
            ("1s", 1),
        ] {
            assert_eq!(parse_duration(s).unwrap(), want, "{s}");
        }
        for bad in [
            "",
            "0s",
            "0m",
            "m",
            "20",
            "20min",
            "1.5h",
            "-5m",
            "20 m",
            "1d",
            "+3s",
            "h1",
            "99999999999999999999h",
        ] {
            let e = format!("{:#}", parse_duration(bad).unwrap_err());
            assert!(
                e.contains(&format!("`{bad}` is not a duration")),
                "{bad}: {e}"
            );
        }
    }
}
