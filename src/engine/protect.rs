//! The protected-state snapshot (R35, plan §8): protected files are compared **on the
//! filesystem**, not through Git, because ignored files never appear in `git status`. An
//! ignored `.claude/settings.local.json` or `toryo.local.yaml` written by an agent would
//! otherwise go unseen and be picked up by the next tool that loads it.
//!
//! - **What:** every file in the worktree that the policy protects (tracked, untracked or
//!   ignored; `.git` and `.toryo-tmp` excluded; links are not followed), plus Git metadata
//!   outside it: the common `config`, `info/exclude`, `info/attributes`, `hooks/*`, and the
//!   worktree's `config.worktree`.
//! - **Record:** path, size, SHA-256, tracked or not; untracked/ignored files and Git
//!   metadata also get a baseline copy (≤ 1 MB each, ≤ 16 MB per snapshot) so a change can be
//!   shown as a diff.
//! - **Baseline versions:** v1 at task creation; a new version only after a checkpoint that
//!   committed an allowed tracked change, or `resume --accept-protected-change`. Stored as
//!   `protected/<n>.json` (artifact `protected`, the head is the version in force).
//! - **Outcome:** a change to an untracked/ignored protected file or to Git metadata is
//!   `protected_config_changed` → `Escalated` before another process starts. A change to a
//!   tracked protected file follows the path policy (restored, back to the Implementer).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::checks::Protected;
use super::store::{self, Store, TaskRow};
use super::task::sha256_hex;
use crate::git::{GitCmd, Loc};

pub const REASON: &str = "protected_config_changed";
pub const MAX_COPY: u64 = 1 << 20;
pub const MAX_TOTAL: u64 = 16 << 20;
const META: &str = "@git/";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PEntry {
    pub size: u64,
    pub sha256: String,
    pub tracked: bool,
    /// A baseline copy is stored under `protected-blobs/<sha256>`.
    pub copy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PManifest {
    pub version: u32,
    pub created_at: String,
    pub reason: String,
    pub entries: BTreeMap<String, PEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub path: String,
    /// `added`, `modified` or `deleted`
    pub kind: &'static str,
    pub tracked: bool,
}

impl Change {
    /// Escalates (R35): untracked/ignored or Git metadata. Tracked → path policy.
    pub fn is_hard(&self) -> bool {
        !self.tracked || self.path.starts_with(META)
    }
}

/// The current state of everything protected.
pub struct Scan {
    pub entries: BTreeMap<String, (PathBuf, PEntry)>,
}

/// Git metadata outside the worktree that tools read on their own.
fn git_meta(g: &GitCmd, wt: &Path) -> Result<Vec<(String, PathBuf)>> {
    let loc = Loc::Discover(wt.to_path_buf());
    let common = PathBuf::from(g.run_str(&loc, true, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?.trim());
    let own = PathBuf::from(g.run_str(&loc, true, &["rev-parse", "--absolute-git-dir"])?.trim());
    let mut v = vec![
        (format!("{META}config"), common.join("config")),
        (format!("{META}info/exclude"), common.join("info").join("exclude")),
        (format!("{META}info/attributes"), common.join("info").join("attributes")),
        (format!("{META}config.worktree"), own.join("config.worktree")),
    ];
    if let Ok(d) = std::fs::read_dir(common.join("hooks")) {
        for e in d.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if !n.ends_with(".sample") {
                v.push((format!("{META}hooks/{n}"), e.path()));
            }
        }
    }
    Ok(v)
}

fn walk(root: &Path, dir: &Path, protect: &Protected, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Ok(()) };
    for e in rd.flatten() {
        let ft = e.file_type()?;
        let p = e.path();
        let rel = p.strip_prefix(root).map(|r| r.display().to_string().replace('\\', "/")).unwrap_or_default();
        let name = e.file_name().to_string_lossy().to_string();
        if name == ".git" || rel == ".toryo-tmp" {
            continue;
        }
        if ft.is_symlink() {
            // junctions and symlinks are not followed; a protected link itself is recorded
            if protect.matches(&rel) {
                out.push((rel, p));
            }
        } else if ft.is_dir() {
            walk(root, &p, protect, out)?;
        } else if protect.matches(&rel) {
            out.push((rel, p));
        }
    }
    Ok(())
}

fn hash_file(p: &Path) -> Result<(u64, String)> {
    let md = std::fs::symlink_metadata(p)?;
    if md.file_type().is_symlink() {
        let t = std::fs::read_link(p).map(|t| t.display().to_string()).unwrap_or_default();
        return Ok((0, sha256_hex(format!("symlink:{t}").as_bytes())));
    }
    let b = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
    Ok((b.len() as u64, sha256_hex(&b)))
}

/// Walk the worktree + Git metadata now.
pub fn scan(g: &GitCmd, wt: &Path, protect: &Protected) -> Result<Scan> {
    let mut found = Vec::new();
    walk(wt, wt, protect, &mut found)?;
    let tracked: BTreeSet<String> = {
        let out = g.run(&Loc::Discover(wt.to_path_buf()), true, &["ls-files", "-z"])?;
        out.split(|b| *b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).to_string()).collect()
    };
    let mut entries = BTreeMap::new();
    for (rel, p) in found {
        let (size, sha256) = hash_file(&p)?;
        let t = tracked.contains(&rel);
        entries.insert(rel, (p, PEntry { size, sha256, tracked: t, copy: false }));
    }
    for (k, p) in git_meta(g, wt)? {
        if p.is_file() {
            let (size, sha256) = hash_file(&p)?;
            entries.insert(k, (p, PEntry { size, sha256, tracked: false, copy: false }));
        }
    }
    Ok(Scan { entries })
}

/// Differences between a baseline and a scan.
pub fn compare(base: &PManifest, now: &Scan) -> Vec<Change> {
    let keys: BTreeSet<&String> = base.entries.keys().chain(now.entries.keys()).collect();
    let mut out = Vec::new();
    for k in keys {
        let b = base.entries.get(k);
        let n = now.entries.get(k).map(|x| &x.1);
        let kind = match (b, n) {
            (Some(_), None) => "deleted",
            (None, Some(_)) => "added",
            (Some(b), Some(n)) if b.sha256 != n.sha256 => "modified",
            _ => continue,
        };
        let tracked = b.is_some_and(|e| e.tracked) || n.is_some_and(|e| e.tracked);
        out.push(Change { path: k.clone(), kind, tracked });
    }
    out
}

fn blobs(t: &TaskRow) -> PathBuf {
    t.artifacts().join("protected-blobs")
}

/// Take a new baseline version from the current state and make it the one in force.
pub fn take_baseline(store: &Store, t: &TaskRow, g: &GitCmd, protect: &Protected, reason: &str) -> Result<PManifest> {
    let now = scan(g, &t.worktree, protect)?;
    let version = super::artifacts::next_version(store, t, "protected")?;
    let mut total = 0u64;
    let mut entries = BTreeMap::new();
    std::fs::create_dir_all(blobs(t))?;
    for (k, (p, mut e)) in now.entries {
        if (!e.tracked || k.starts_with(META)) && e.size <= MAX_COPY && total + e.size <= MAX_TOTAL {
            let dst = blobs(t).join(&e.sha256);
            if !dst.exists() && std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
                std::fs::copy(&p, &dst)?;
            }
            e.copy = dst.exists();
            total += e.size;
        }
        entries.insert(k, e);
    }
    let m = PManifest { version, created_at: crate::evidence::now_rfc3339(), reason: reason.to_string(), entries };
    let bytes = serde_json::to_vec_pretty(&m)?;
    super::artifacts::put(store, t, None, "protected", "json", "protected_manifest", &bytes, Some(&format!("protected/{version}.json")))?;
    let (tid, r) = (t.id.clone(), reason.to_string());
    let n = m.entries.len();
    store.call(move |tx| store::event(tx, Some(&tid), None, "protected_baseline", &serde_json::json!({"version": version, "reason": r, "entries": n})))?;
    Ok(m)
}

/// The baseline in force (the head of `protected`), hash-checked; `None` for a task created
/// before M5 (the caller takes v1 then).
pub fn baseline(store: &Store, t: &TaskRow) -> Result<Option<(String, PManifest)>> {
    match super::artifacts::read_head(store, t, "protected")? {
        Some((v, bytes)) => Ok(Some((v.id, serde_json::from_slice(&bytes)?))),
        None => Ok(None),
    }
}

/// The baseline in force, taking v1 if the task has none yet.
pub fn baseline_or_init(store: &Store, t: &TaskRow, g: &GitCmd, protect: &Protected) -> Result<(String, PManifest)> {
    if let Some(b) = baseline(store, t)? {
        return Ok(b);
    }
    take_baseline(store, t, g, protect, "v1 (task created before M5)")?;
    baseline(store, t)?.context("protected baseline")
}

/// A readable diff of the changes against the baseline copies (`git diff --no-index`,
/// canonical flags); tracked files diff against HEAD.
pub fn render_changes(g: &GitCmd, t: &TaskRow, base: &PManifest, now: &Scan, changes: &[Change]) -> Result<String> {
    let mut s = String::new();
    let empty = t.artifacts().join("protected-blobs").join(".empty");
    std::fs::create_dir_all(empty.parent().expect("parent"))?;
    if !empty.exists() {
        std::fs::write(&empty, b"")?;
    }
    for c in changes {
        s.push_str(&format!("### {} {} ({})\n", c.kind, c.path, if c.path.starts_with(META) { "Git metadata" } else if c.tracked { "tracked" } else { "untracked/ignored" }));
        let before = base.entries.get(&c.path).filter(|e| e.copy).map(|e| blobs(t).join(&e.sha256));
        let after = now.entries.get(&c.path).map(|x| x.0.clone());
        let a = match (&before, base.entries.contains_key(&c.path)) {
            (Some(p), _) => p.clone(),
            (None, false) => empty.clone(),
            (None, true) => {
                s.push_str("(no baseline copy: file too large or tracked — compare with `git diff`)\n\n");
                continue;
            }
        };
        let b = after.unwrap_or_else(|| empty.clone());
        let out = g.output(
            &Loc::Discover(t.artifacts()),
            true,
            &["diff", "--no-index", "--no-textconv", "--no-ext-diff", "--no-color", "--", &a.display().to_string(), &b.display().to_string()],
        )?;
        let d = String::from_utf8_lossy(&out.stdout);
        let mut cut = d.len().min(16_000);
        while !d.is_char_boundary(cut) {
            cut -= 1;
        }
        s.push_str(&d[..cut]);
        s.push('\n');
    }
    Ok(s)
}

/// The outcome of a check.
pub struct Check {
    pub baseline_id: String,
    pub changes: Vec<Change>,
    /// Written to the run store when there are hard changes.
    pub diff_path: Option<PathBuf>,
}

impl Check {
    pub fn hard(&self) -> Vec<&Change> {
        self.changes.iter().filter(|c| c.is_hard()).collect()
    }
}

/// The `Escalated` reason for a list of changes (§11 `protected_config_changed`).
pub fn escalation_reason(task: &str, bad: &[&Change], diff: Option<&Path>) -> String {
    let list: Vec<String> = bad.iter().map(|x| format!("{} {}{}", x.kind, x.path, if x.is_hard() { "" } else { " (tracked)" })).collect();
    format!(
        "{REASON}: {} (diff: {}) — restore and `toryo resume {task}`, or accept: `toryo resume {task} --accept-protected-change <path>`",
        list.join(", "),
        diff.map(|p| p.display().to_string()).unwrap_or_else(|| "`git diff` in the worktree".into()),
    )
}

/// Compare the current state with the baseline in force; on hard changes write the diff.
pub fn check(store: &Store, t: &TaskRow, g: &GitCmd, protect: &Protected) -> Result<Check> {
    let (id, base) = baseline_or_init(store, t, g, protect)?;
    let now = scan(g, &t.worktree, protect)?;
    let changes = compare(&base, &now);
    let mut diff_path = None;
    if changes.iter().any(Change::is_hard) {
        let hard: Vec<Change> = changes.iter().filter(|c| c.is_hard()).cloned().collect();
        let text = render_changes(g, t, &base, &now, &hard)?;
        let p = t.artifacts().join(format!("protected-changes-{}.md", super::artifacts::next_version(store, t, "protected")?));
        std::fs::write(&p, format!("# Protected state changed (R35)\n\nbaseline v{} ({})\n\n{text}", base.version, base.reason))?;
        diff_path = Some(p);
    }
    Ok(Check { baseline_id: id, changes, diff_path })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hard_changes() {
        assert!(Change { path: ".claude/settings.local.json".into(), kind: "added", tracked: false }.is_hard());
        assert!(Change { path: "@git/config".into(), kind: "modified", tracked: false }.is_hard());
        assert!(!Change { path: "AGENTS.md".into(), kind: "modified", tracked: true }.is_hard());
    }
}
