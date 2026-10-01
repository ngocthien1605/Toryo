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

use crate::config::{CommandSpec, EnvGate, MachineConfig, Role};

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
    serde_yaml::from_value(doc).with_context(|| format!("parsing {}", file.display()))
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

/// The repository root of org chart mode: `project.root` (expanded) when it holds
/// `.toryo/toryo-orgchart.yaml`; `None` is `toryo.yaml`-only mode, byte for byte as in M10.
/// `.toryo/` is read from the repository checkout, never from a task worktree (§7).
pub fn org_chart_root(project_root: &str) -> Option<PathBuf> {
    let root = PathBuf::from(crate::config::expand_vars(project_root).ok()?);
    root.join(ORGCHART).is_file().then_some(root)
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
) -> Result<Loaded> {
    use crate::config::untagged;
    let org_path = root.join(ORGCHART);
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
        let machine = std::fs::read_to_string(src.join("toryo.local.yaml"))
            .unwrap()
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
