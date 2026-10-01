//! M0 evidence store: every spike appends one JSON record to
//! `<data_dir>/m0/results.jsonl`; `toryo spike report` renders the latest record
//! per spike/profile as Markdown for the v0.7 plan update.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::config::{data_dir, expand_vars, Effective, Profile, ProfileKind};
use crate::process::env::build_env;
use crate::process::redact::Redactions;
use crate::process::{spawn, Boundary, Chunk, SpawnSpec, WaitOptions};

/// First `x.y.z` in a `--version` output (e.g. `2.1.283 (Claude Code)`, `codex-cli 0.157.1`).
pub fn parse_version(text: &str) -> Option<String> {
    let re = regex::Regex::new(r"\d+\.\d+\.\d+(?:[-+.][0-9A-Za-z.]+)?").ok()?;
    re.find(text).map(|m| m.as_str().to_string())
}

pub fn m0_dir() -> PathBuf {
    data_dir().join("m0")
}

pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".into())
}

/// Compact timestamp for directory names.
pub fn stamp() -> String {
    let t = time::OffsetDateTime::now_utc();
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        t.year(),
        t.month() as u8,
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

pub fn new_run_dir(kind: &str, label: &str) -> Result<PathBuf> {
    let safe: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let d = m0_dir()
        .join("runs")
        .join(format!("{}-{kind}-{safe}", stamp()));
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

#[derive(Debug, Serialize)]
pub struct Record {
    pub ts: String,
    pub spike: String,
    pub profile: Option<String>,
    /// e.g. ["M0-VERIFY 3", "R18"]
    pub items: Vec<String>,
    /// None = data only, a human interprets it
    pub pass: Option<bool>,
    pub summary: String,
    pub details: Value,
    pub run_dir: Option<String>,
    pub toryo_version: String,
    pub host_os: String,
    /// R51: the CLI versions this evidence is about (`git`, and each profile name).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub cli_versions: BTreeMap<String, String>,
}

impl Record {
    pub fn new(spike: &str, profile: Option<&str>, items: &[&str]) -> Self {
        Self {
            ts: now_rfc3339(),
            spike: spike.into(),
            profile: profile.map(str::to_string),
            items: items.iter().map(|s| s.to_string()).collect(),
            pass: None,
            summary: String::new(),
            details: Value::Null,
            run_dir: None,
            toryo_version: env!("CARGO_PKG_VERSION").into(),
            host_os: std::env::consts::OS.into(),
            cli_versions: BTreeMap::new(),
        }
    }

    /// R51: record the Git version and the current CLI version of each profile.
    pub fn bind_versions(&mut self, cfg: Option<&Effective>, profiles: &[&str]) {
        self.cli_versions.extend(collect_versions(cfg, profiles));
    }
}

/// `git` and each profile's current CLI version (profiles whose version is unknown are left out).
pub fn collect_versions(cfg: Option<&Effective>, profiles: &[&str]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(v) = git_version() {
        out.insert("git".into(), v);
    }
    for name in profiles {
        if out.contains_key(*name) {
            continue;
        }
        if let Some(v) = cfg
            .and_then(|c| c.profile(name).ok())
            .and_then(profile_version)
        {
            out.insert((*name).to_string(), v);
        }
    }
    out
}

/// The redaction set of a profile's own secrets (M11-E2, review M2): what the R51 evidence run
/// of its program redacts. A secret that cannot be resolved here is left out.
pub fn profile_redactions(p: &Profile) -> Redactions {
    let secrets: Vec<crate::process::redact::Secret> = crate::config::resolve_profile_secrets(p)
        .map(|m| {
            m.into_iter()
                .map(|(name, value)| crate::process::redact::Secret { name, value })
                .collect()
        })
        .unwrap_or_default();
    Redactions::new(&secrets)
}

/// First line of `<exe> --version` (60 s limit, no secrets in the environment; `redact`: the
/// profile's secrets all the same, M11-E2).
pub fn run_version(
    exe: &Path,
    env: BTreeMap<String, String>,
    redact: &Redactions,
) -> Result<String> {
    let spec = SpawnSpec {
        program: exe.to_path_buf(),
        args: vec!["--version".into()],
        cwd: std::env::temp_dir(),
        env,
        stdin: None,
        boundary: Boundary::None,
    };
    let r = spawn(&spec, redact)?;
    let mut out = Vec::new();
    let info = r.wait(
        WaitOptions {
            max: Some(Duration::from_secs(60)),
            ..Default::default()
        },
        |c| {
            let (Chunk::Stdout(b) | Chunk::Stderr(b)) = c;
            out.extend(b)
        },
    )?;
    if info.timed_out {
        anyhow::bail!("--version timed out");
    }
    Ok(String::from_utf8_lossy(&out)
        .trim()
        .lines()
        .next()
        .unwrap_or("")
        .to_string())
}

/// The profile's CLI version (`x.y.z`), from `--version` with the profile's plain env.
pub fn profile_version(p: &Profile) -> Option<String> {
    let exe = PathBuf::from(expand_vars(&p.exe).ok()?);
    let env = build_env(std::env::vars(), &p.env, &BTreeMap::new());
    parse_version(&run_version(&exe, env, &profile_redactions(p)).ok()?)
}

pub fn git_version() -> Option<String> {
    let o = std::process::Command::new(crate::git::git_exe())
        .arg("--version")
        .output()
        .ok()?;
    parse_version(&String::from_utf8_lossy(&o.stdout))
}

/// The canary that verifies a profile's isolation (R18 / R44b).
pub fn canary_spike(kind: ProfileKind) -> &'static str {
    match kind {
        ProfileKind::Claude => "claude-isolation",
        ProfileKind::Codex => "codex-canary",
    }
}

/// R51: the state of one piece of evidence for the version in use now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceStatus {
    /// Last record passed, for this version.
    Verified {
        ts: String,
    },
    Missing,
    /// Last record failed.
    Failed {
        ts: String,
        summary: String,
    },
    /// Last record passed, but for another (or an unrecorded) version.
    Stale {
        ts: String,
        recorded: Option<String>,
        current: Option<String>,
    },
}

impl EvidenceStatus {
    pub fn is_verified(&self) -> bool {
        matches!(self, EvidenceStatus::Verified { .. })
    }

    pub fn describe(&self, spike: &str) -> String {
        match self {
            EvidenceStatus::Verified { ts } => format!("{spike} passed @ {ts}"),
            EvidenceStatus::Missing => format!("no {spike} record"),
            EvidenceStatus::Failed { ts, summary } => format!("{spike} FAILED @ {ts}: {summary}"),
            EvidenceStatus::Stale {
                ts,
                recorded,
                current,
            } => format!(
                "{spike} passed @ {ts} for version {} but the CLI is now {} (R51: unverified)",
                recorded.as_deref().unwrap_or("<not recorded>"),
                current.as_deref().unwrap_or("<unknown>")
            ),
        }
    }
}

/// Latest `spike` record for `key` (a profile name, or "" for host evidence), checked
/// against `current` = the version now in use under `version_key` in `cli_versions`.
pub fn evidence_status(
    records: &[Value],
    spike: &str,
    key: &str,
    version_key: &str,
    current: Option<&str>,
) -> EvidenceStatus {
    let Some(x) = records.iter().rev().find(|x| {
        x["spike"].as_str() == Some(spike) && (key.is_empty() || x["profile"].as_str() == Some(key))
    }) else {
        return EvidenceStatus::Missing;
    };
    let ts = x["ts"].as_str().unwrap_or("").to_string();
    if x["pass"].as_bool() != Some(true) {
        return EvidenceStatus::Failed {
            ts,
            summary: x["summary"].as_str().unwrap_or("").to_string(),
        };
    }
    let recorded = x["cli_versions"][version_key].as_str().map(str::to_string);
    match (&recorded, current) {
        (Some(r), Some(c)) if r == c => EvidenceStatus::Verified { ts },
        _ => EvidenceStatus::Stale {
            ts,
            recorded,
            current: current.map(str::to_string),
        },
    }
}

/// R51 gate: the profiles whose isolation canary did not pass for the CLI version
/// installed now, with the reason. Probes each profile's `--version`.
pub fn unverified_profiles(cfg: &Effective, profiles: &[&str]) -> Result<Vec<(String, String)>> {
    let records = load();
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for name in profiles {
        if !seen.insert(*name) {
            continue;
        }
        let p = cfg.profile(name)?;
        let spike = canary_spike(p.kind);
        let st = evidence_status(&records, spike, name, name, profile_version(p).as_deref());
        if !st.is_verified() {
            out.push(((*name).to_string(), st.describe(spike)));
        }
    }
    Ok(out)
}

/// R49 (V07-VERIFY 4): Claude profiles used as a fallback Implementer also need a passing
/// `claude-implementer-isolation` record for the version installed now.
pub fn unverified_implementers(
    cfg: &Effective,
    profiles: &[&str],
) -> Result<Vec<(String, String)>> {
    let records = load();
    let mut out = Vec::new();
    for name in profiles {
        let p = cfg.profile(name)?;
        if p.kind != ProfileKind::Claude {
            continue;
        }
        let spike = "claude-implementer-isolation";
        let st = evidence_status(&records, spike, name, name, profile_version(p).as_deref());
        if !st.is_verified() {
            out.push(((*name).to_string(), st.describe(spike)));
        }
    }
    Ok(out)
}

pub fn append(rec: &Record) -> Result<PathBuf> {
    let dir = m0_dir();
    std::fs::create_dir_all(&dir)?;
    let p = dir.join("results.jsonl");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)?;
    writeln!(f, "{}", serde_json::to_string(rec)?)?;
    Ok(p)
}

pub fn load() -> Vec<Value> {
    let p = m0_dir().join("results.jsonl");
    std::fs::read_to_string(p)
        .map(|s| {
            s.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Latest record per (spike, profile, label-ish key).
pub fn latest(records: &[Value]) -> Vec<&Value> {
    let mut seen = std::collections::BTreeMap::new();
    for r in records {
        let key = format!(
            "{}|{}|{}",
            r["spike"].as_str().unwrap_or(""),
            r["profile"].as_str().unwrap_or(""),
            r["details"]["mode"]
                .as_str()
                .or(r["details"]["variant_key"].as_str())
                .unwrap_or("")
        );
        seen.insert(key, r);
    }
    seen.into_values().collect()
}

pub fn render_markdown(records: &[Value]) -> String {
    let mut s = String::from("# M0 results (generated by `toryo spike report`)\n\n");
    s.push_str(
        "| When (UTC) | Spike | Profile | Items | Result | Summary |\n|---|---|---|---|---|---|\n",
    );
    for r in latest(records) {
        let pass = match r["pass"].as_bool() {
            Some(true) => "PASS",
            Some(false) => "FAIL",
            None => "data",
        };
        let items: Vec<&str> = r["items"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        s.push_str(&format!(
            "| {} | {} | {} | {} | **{}** | {} |\n",
            r["ts"].as_str().unwrap_or(""),
            r["spike"].as_str().unwrap_or(""),
            r["profile"].as_str().unwrap_or("—"),
            items.join(", "),
            pass,
            r["summary"]
                .as_str()
                .unwrap_or("")
                .replace('|', "\\|")
                .replace('\n', " ")
        ));
    }
    s.push_str("\nFull records: `results.jsonl`; captured streams: `runs/<stamp>-…/` (copy the useful ones to `fixtures/captured/`).\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn evidence_is_bound_to_the_cli_version() {
        let pass = |v: Option<&str>| {
            let mut r = json!({"spike": "codex-canary", "profile": "cx", "pass": true, "ts": "t1", "summary": "ok"});
            if let Some(v) = v {
                r["cli_versions"] = json!({ "cx": v });
            }
            r
        };
        let st = |recs: &[Value], cur: Option<&str>| {
            evidence_status(recs, "codex-canary", "cx", "cx", cur)
        };
        assert_eq!(st(&[], Some("1.0.0")), EvidenceStatus::Missing);
        assert!(st(&[pass(Some("1.0.0"))], Some("1.0.0")).is_verified());
        // another version, an unrecorded version (pre-R51 record), an unknown current version
        for (rec, cur) in [
            (Some("1.0.0"), Some("1.1.0")),
            (None, Some("1.0.0")),
            (Some("1.0.0"), None),
        ] {
            assert!(
                matches!(st(&[pass(rec)], cur), EvidenceStatus::Stale { .. }),
                "{rec:?} {cur:?}"
            );
        }
        // the latest record decides: a later failure is not hidden by an older pass
        let fail = json!({"spike": "codex-canary", "profile": "cx", "pass": false, "ts": "t2", "summary": "escaped"});
        assert!(matches!(
            st(&[pass(Some("1.0.0")), fail], Some("1.0.0")),
            EvidenceStatus::Failed { .. }
        ));
        // a new pass on the new version clears it
        assert!(st(&[pass(Some("1.0.0")), pass(Some("1.1.0"))], Some("1.1.0")).is_verified());
        assert!(st(&[pass(Some("1.1.0"))], Some("1.1.0"))
            .describe("codex-canary")
            .contains("passed"));
    }

    #[test]
    fn version_parsing() {
        assert_eq!(
            parse_version("2.1.283 (Claude Code)").as_deref(),
            Some("2.1.283")
        );
        assert_eq!(
            parse_version("codex-cli 0.157.1").as_deref(),
            Some("0.157.1")
        );
        assert_eq!(
            parse_version("git version 2.47.1.windows.2").as_deref(),
            Some("2.47.1.windows.2")
        );
        assert_eq!(parse_version("no version"), None);
    }
}
