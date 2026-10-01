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
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests {
    use super::*;

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
