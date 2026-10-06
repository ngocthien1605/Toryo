//! The protected-state snapshot (R35, plan §8): protected files are compared **on the
//! filesystem**, not through Git, because ignored files never appear in `git status`. An
//! ignored `.claude/settings.local.json` or `toryo.local.yaml` written by an agent would
//! otherwise go unseen and be picked up by the next tool that loads it.
//!
//! - **What:** every file in the worktree that the policy protects (tracked, untracked or
//!   ignored; `.git` and `.toryo-tmp` excluded), plus Git metadata outside it: the common
//!   `config`, `info/exclude`, `info/attributes`, `hooks/*`, and the worktree's
//!   `config.worktree`. Links and junctions are never followed: one at a protected name —
//!   `.claude -> docs/agent` included (EC-08) — is recorded as a link with its target.
//! - **Record:** path, size, SHA-256, tracked or not; untracked/ignored files and Git
//!   metadata also get a baseline copy (≤ 1 MB each, ≤ 16 MB per snapshot) so a change can be
//!   shown as a diff.
//! - **Baseline versions:** v1 at task creation; a new version only after a checkpoint that
//!   committed an allowed tracked change, or `resume --accept-protected-change`. Stored as
//!   `protected/<n>.json` (artifact `protected`, the head is the version in force).
//! - **Newly covered paths:** a baseline taken before the matcher widened (EC-07/08/09) lacks
//!   paths the scan now finds. A tracked regular file among them that is still its blob at
//!   the task's base commit has not changed and is no change ([`compare_since_base`]).
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
    /// The entry is a symlink or junction to this target (recorded, never followed; EC-08).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
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

/// M12-02 (H4): a protected folder or file the scan could not read. "Does not exist" is quiet;
/// anything else is an error that names the path — an unlistable folder is not an empty one.
#[derive(Debug)]
pub struct Unreadable {
    pub path: String,
    /// `list` or `read`
    pub what: &'static str,
    pub error: String,
}

impl std::fmt::Display for Unreadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot {} {}: {}", self.what, self.path, self.error)
    }
}

impl std::error::Error for Unreadable {}

fn unreadable(path: &Path, what: &'static str, e: &std::io::Error) -> anyhow::Error {
    anyhow::Error::new(Unreadable {
        path: path.display().to_string(),
        what,
        error: e.to_string(),
    })
}

/// The current state of everything protected.
pub struct Scan {
    pub entries: BTreeMap<String, (PathBuf, PEntry)>,
}

/// Git metadata outside the worktree that tools read on their own.
fn git_meta(g: &GitCmd, wt: &Path) -> Result<Vec<(String, PathBuf)>> {
    let loc = Loc::Discover(wt.to_path_buf());
    let common = PathBuf::from(
        g.run_str(
            &loc,
            true,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?
        .trim(),
    );
    let own = PathBuf::from(
        g.run_str(&loc, true, &["rev-parse", "--absolute-git-dir"])?
            .trim(),
    );
    let mut v = vec![
        (format!("{META}config"), common.join("config")),
        (
            format!("{META}info/exclude"),
            common.join("info").join("exclude"),
        ),
        (
            format!("{META}info/attributes"),
            common.join("info").join("attributes"),
        ),
        (
            format!("{META}config.worktree"),
            own.join("config.worktree"),
        ),
    ];
    let hooks = common.join("hooks");
    match std::fs::read_dir(&hooks) {
        Ok(d) => {
            for e in d {
                let e = e.map_err(|x| unreadable(&hooks, "list", &x))?;
                let n = e.file_name().to_string_lossy().to_string();
                if !n.ends_with(".sample") {
                    v.push((format!("{META}hooks/{n}"), e.path()));
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(unreadable(&hooks, "list", &e)),
    }
    // M13-R1 (X30): a replace ref or a graft makes a Git that honours it show another history.
    // Toryo's Git ignores both; a task still needs neither, so a change is hard.
    v.push((
        format!("{META}info/grafts"),
        common.join("info").join("grafts"),
    ));
    replace_refs(&common.join("refs").join("replace"), "", &mut v)?;
    Ok(v)
}

/// Every loose replace ref (`refs/replace/<sha>`; nested names are walked too).
fn replace_refs(dir: &Path, prefix: &str, v: &mut Vec<(String, PathBuf)>) -> Result<()> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(unreadable(dir, "list", &e)),
    };
    for e in rd {
        let e = e.map_err(|x| unreadable(dir, "list", &x))?;
        let name = format!("{prefix}{}", e.file_name().to_string_lossy());
        let ft = e
            .file_type()
            .map_err(|x| unreadable(&e.path(), "read", &x))?;
        if ft.is_dir() {
            replace_refs(&e.path(), &format!("{name}/"), v)?;
        } else {
            v.push((format!("{META}refs/replace/{name}"), e.path()));
        }
    }
    Ok(())
}

fn walk(
    root: &Path,
    dir: &Path,
    protect: &Protected,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<()> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(unreadable(dir, "list", &e)),
    };
    for e in rd {
        let e = e.map_err(|x| unreadable(dir, "list", &x))?;
        let ft = match e.file_type() {
            Ok(ft) => ft,
            // gone between the listing and now: nothing to protect
            Err(x) if x.kind() == std::io::ErrorKind::NotFound => continue,
            Err(x) => return Err(unreadable(&e.path(), "read", &x)),
        };
        let p = e.path();
        let rel = p
            .strip_prefix(root)
            .map(|r| r.display().to_string().replace('\\', "/"))
            .unwrap_or_default();
        let name = e.file_name().to_string_lossy().to_string();
        if name == ".git" || rel == ".toryo-tmp" {
            continue;
        }
        if ft.is_symlink() {
            // junctions and symlinks are not followed; a link at a protected name (a
            // protected directory name included: `.claude -> docs/agent`) is recorded
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

/// `(size, sha256, link target)` of a protected entry. A link is hashed by its target
/// (never followed); a FIFO, socket or device is recorded without being opened — reading
/// one could block forever.
fn hash_file(p: &Path) -> Result<(u64, String, Option<String>)> {
    let md = std::fs::symlink_metadata(p).map_err(|e| unreadable(p, "read", &e))?;
    if md.file_type().is_symlink() {
        let t = std::fs::read_link(p)
            .map(|t| t.display().to_string())
            .unwrap_or_default();
        return Ok((0, sha256_hex(format!("symlink:{t}").as_bytes()), Some(t)));
    }
    if !md.is_file() {
        return Ok((0, sha256_hex(b"special file"), None));
    }
    let b = std::fs::read(p).map_err(|e| unreadable(p, "read", &e))?;
    Ok((b.len() as u64, sha256_hex(&b), None))
}

/// Walk the worktree + Git metadata now.
pub fn scan(g: &GitCmd, wt: &Path, protect: &Protected) -> Result<Scan> {
    let mut found = Vec::new();
    walk(wt, wt, protect, &mut found)?;
    let tracked: BTreeSet<String> = {
        let out = g.run(&Loc::Discover(wt.to_path_buf()), true, &["ls-files", "-z"])?;
        out.split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).to_string())
            .collect()
    };
    let mut entries = BTreeMap::new();
    for (rel, p) in found {
        let (size, sha256, link) = hash_file(&p)?;
        let t = tracked.contains(&rel);
        entries.insert(
            rel,
            (
                p,
                PEntry {
                    size,
                    sha256,
                    tracked: t,
                    copy: false,
                    link,
                },
            ),
        );
    }
    for (k, p) in git_meta(g, wt)? {
        // (M12-02 review) `is_file()` is false on any error: a Git folder that cannot be
        // listed would hide its `info/exclude`. Only "not there" (or not a file) is quiet.
        let is_file = match std::fs::metadata(&p) {
            Ok(m) => m.is_file(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(unreadable(&p, "read", &e)),
        };
        if is_file {
            let (size, sha256, link) = hash_file(&p)?;
            entries.insert(
                k,
                (
                    p,
                    PEntry {
                        size,
                        sha256,
                        tracked: false,
                        copy: false,
                        link,
                    },
                ),
            );
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
        out.push(Change {
            path: k.clone(),
            kind,
            tracked,
        });
    }
    out
}

/// [`compare`] for a task worktree whose base commit is `base_commit`, where a path that is
/// only newly *covered* is no change (review F2).
///
/// A baseline taken before the matcher widened — repo verify programs (EC-07), a protected
/// directory entry itself (EC-08), case variants (EC-09) — lacks paths the scan now finds.
/// Such an `added` entry is dropped only when it is a tracked regular file whose content is
/// its blob at `base_commit`: the file has not changed since the task began. A new or
/// modified file, a link, an untracked or ignored entry and Git metadata are reported as
/// before. The baseline is not re-taken: that could hide an agent's change.
pub fn compare_since_base(
    g: &GitCmd,
    wt: &Path,
    base_commit: &str,
    base: &PManifest,
    now: &Scan,
) -> Vec<Change> {
    compare(base, now)
        .into_iter()
        .filter(|c| {
            let newly_covered = c.kind == "added"
                && now
                    .entries
                    .get(&c.path)
                    .is_some_and(|(p, e)| unchanged_since_base(g, wt, base_commit, &c.path, p, e));
            !newly_covered
        })
        .collect()
}

/// `rel` (scanned as `e` at `p`) is a tracked regular file whose bytes are exactly its blob at
/// `base_commit` — as stored, or as checked out: `core.autocrlf` / `eol` turn an LF blob into
/// a CRLF file on Windows (`cat-file --filters`, under the task's filter plan, R37). Any Git
/// error (no such path at the base, a tree, a gitlink) is "no".
fn unchanged_since_base(
    g: &GitCmd,
    wt: &Path,
    base_commit: &str,
    rel: &str,
    p: &Path,
    e: &PEntry,
) -> bool {
    if !e.tracked
        || e.link.is_some()
        || rel.starts_with(META)
        || !std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file())
    {
        return false;
    }
    let loc = Loc::Discover(wt.to_path_buf());
    let object = format!("{base_commit}:{rel}");
    let object = object.as_str();
    [
        ["cat-file", "blob", object],
        ["cat-file", "--filters", object],
    ]
    .iter()
    .any(|args| {
        g.output(&loc, true, args).is_ok_and(|o| {
            o.status.success()
                && o.stdout.len() as u64 == e.size
                && sha256_hex(&o.stdout) == e.sha256
        })
    })
}

fn blobs(t: &TaskRow) -> PathBuf {
    t.artifacts().join("protected-blobs")
}

/// Take a new baseline version from the current state and make it the one in force.
pub fn take_baseline(
    store: &Store,
    t: &TaskRow,
    g: &GitCmd,
    protect: &Protected,
    reason: &str,
) -> Result<PManifest> {
    let now = scan(g, &t.worktree, protect)?;
    let version = super::artifacts::next_version(store, t, "protected")?;
    let mut total = 0u64;
    let mut entries = BTreeMap::new();
    std::fs::create_dir_all(blobs(t))?;
    for (k, (p, mut e)) in now.entries {
        if (!e.tracked || k.starts_with(META)) && e.size <= MAX_COPY && total + e.size <= MAX_TOTAL
        {
            let dst = blobs(t).join(&e.sha256);
            if !dst.exists() && std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
                std::fs::copy(&p, &dst)?;
            }
            e.copy = dst.exists();
            total += e.size;
        }
        entries.insert(k, e);
    }
    let m = PManifest {
        version,
        created_at: crate::evidence::now_rfc3339(),
        reason: reason.to_string(),
        entries,
    };
    let bytes = serde_json::to_vec_pretty(&m)?;
    super::artifacts::put(
        store,
        t,
        None,
        "protected",
        "json",
        "protected_manifest",
        &bytes,
        Some(&format!("protected/{version}.json")),
    )?;
    let (tid, r) = (t.id.clone(), reason.to_string());
    let n = m.entries.len();
    store.call(move |tx| {
        store::event(
            tx,
            Some(&tid),
            None,
            "protected_baseline",
            &serde_json::json!({"version": version, "reason": r, "entries": n}),
        )
    })?;
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
pub fn baseline_or_init(
    store: &Store,
    t: &TaskRow,
    g: &GitCmd,
    protect: &Protected,
) -> Result<(String, PManifest)> {
    if let Some(b) = baseline(store, t)? {
        return Ok(b);
    }
    take_baseline(store, t, g, protect, "v1 (task created before M5)")?;
    baseline(store, t)?.context("protected baseline")
}

/// A readable diff of the changes against the baseline copies (`git diff --no-index`,
/// canonical flags); tracked files diff against HEAD.
pub fn render_changes(
    g: &GitCmd,
    t: &TaskRow,
    base: &PManifest,
    now: &Scan,
    changes: &[Change],
) -> Result<String> {
    let mut s = String::new();
    let empty = t.artifacts().join("protected-blobs").join(".empty");
    std::fs::create_dir_all(empty.parent().expect("parent"))?;
    if !empty.exists() {
        std::fs::write(&empty, b"")?;
    }
    for c in changes {
        s.push_str(&format!(
            "### {} {} ({})\n",
            c.kind,
            c.path,
            if c.path.starts_with(META) {
                "Git metadata"
            } else if c.tracked {
                "tracked"
            } else {
                "untracked/ignored"
            }
        ));
        // a link is described, never diffed: `git diff --no-index` would follow it (EC-08)
        let was = base.entries.get(&c.path);
        let is = now.entries.get(&c.path).map(|x| &x.1);
        if [was, is].iter().flatten().any(|e| e.link.is_some()) {
            let side = |e: Option<&PEntry>| match e {
                None => "(absent)".to_string(),
                Some(PEntry { link: Some(t), .. }) => {
                    format!("a symlink/junction → `{t}` (not followed)")
                }
                Some(e) => format!("a file of {} bytes", e.size),
            };
            s.push_str(&format!("before: {}\nafter:  {}\n\n", side(was), side(is)));
            continue;
        }
        let before = base
            .entries
            .get(&c.path)
            .filter(|e| e.copy)
            .map(|e| blobs(t).join(&e.sha256));
        let after = now.entries.get(&c.path).map(|x| x.0.clone());
        let a = match (&before, base.entries.contains_key(&c.path)) {
            (Some(p), _) => p.clone(),
            (None, false) => empty.clone(),
            (None, true) => {
                s.push_str(
                    "(no baseline copy: file too large or tracked — compare with `git diff`)\n\n",
                );
                continue;
            }
        };
        let b = after.unwrap_or_else(|| empty.clone());
        let out = g.output(
            &Loc::Discover(t.artifacts()),
            true,
            &[
                "diff",
                "--no-index",
                "--no-textconv",
                "--no-ext-diff",
                "--no-color",
                "--",
                &a.display().to_string(),
                &b.display().to_string(),
            ],
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
    // M12-02: a path the scan could not read is no change to restore or accept — the rights on
    // it (or the folder itself) are the problem
    if bad.iter().any(|x| x.kind.starts_with("cannot ")) {
        let list: Vec<String> = bad
            .iter()
            .map(|x| format!("{} {}", x.kind, x.path))
            .collect();
        return format!(
            "{REASON}: {} — fix the permissions on it (or remove the folder from the worktree), then `toryo resume {task}`",
            list.join(", ")
        );
    }
    let list: Vec<String> = bad
        .iter()
        .map(|x| {
            format!(
                "{} {}{}",
                x.kind,
                x.path,
                if x.is_hard() { "" } else { " (tracked)" }
            )
        })
        .collect();
    format!(
        "{REASON}: {} (diff: {}) — restore and `toryo resume {task}`, or accept: `toryo resume {task} --accept-protected-change <path>`",
        list.join(", "),
        diff.map(|p| p.display().to_string()).unwrap_or_else(|| "`git diff` in the worktree".into()),
    )
}

/// Compare the current state with the baseline in force; on hard changes write the diff.
pub fn check(store: &Store, t: &TaskRow, g: &GitCmd, protect: &Protected) -> Result<Check> {
    let (id, base) = baseline_or_init(store, t, g, protect)?;
    let now = match scan(g, &t.worktree, protect) {
        Ok(now) => now,
        Err(e) => {
            // M12-02: a protected folder that cannot be listed is a protected-state problem,
            // never an empty folder: a hard change, so the engine escalates (any other error
            // is not ours to turn into one)
            let u = e.downcast::<Unreadable>()?;
            return Ok(Check {
                baseline_id: id,
                changes: vec![Change {
                    path: format!("{} ({})", u.path, u.error),
                    kind: if u.what == "list" {
                        "cannot list"
                    } else {
                        "cannot read"
                    },
                    tracked: false,
                }],
                diff_path: None,
            });
        }
    };
    let changes = compare_since_base(g, &t.worktree, &t.base_commit, &base, &now);
    let mut diff_path = None;
    if changes.iter().any(Change::is_hard) {
        let hard: Vec<Change> = changes.iter().filter(|c| c.is_hard()).cloned().collect();
        let text = render_changes(g, t, &base, &now, &hard)?;
        let p = t.artifacts().join(format!(
            "protected-changes-{}.md",
            super::artifacts::next_version(store, t, "protected")?
        ));
        std::fs::write(
            &p,
            format!(
                "# Protected state changed (R35)\n\nbaseline v{} ({})\n\n{text}",
                base.version, base.reason
            ),
        )?;
        diff_path = Some(p);
    }
    Ok(Check {
        baseline_id: id,
        changes,
        diff_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hard_changes() {
        assert!(Change {
            path: ".claude/settings.local.json".into(),
            kind: "added",
            tracked: false
        }
        .is_hard());
        assert!(Change {
            path: "@git/config".into(),
            kind: "modified",
            tracked: false
        }
        .is_hard());
        assert!(!Change {
            path: "AGENTS.md".into(),
            kind: "modified",
            tracked: true
        }
        .is_hard());
    }

    /// EC-08: a link or junction at a protected name (a protected directory name included)
    /// is recorded with its target and never followed; a FIFO at a protected name is
    /// recorded without being opened.
    #[cfg(unix)]
    #[test]
    fn links_at_protected_names_are_recorded_not_followed() {
        use std::os::unix::fs::symlink;
        let d = tempfile::tempdir().unwrap();
        let g = GitCmd::new("git", d.path().join("hooks")).unwrap();
        let wt = d.path().join("repo");
        std::fs::create_dir_all(wt.join("docs").join("agent")).unwrap();
        std::fs::create_dir_all(wt.join("sub")).unwrap();
        let loc = Loc::Discover(wt.clone());
        g.run(&loc, false, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(wt.join("docs/agent/settings.local.json"), "{}\n").unwrap();
        std::fs::write(wt.join("sub/keep.txt"), "k\n").unwrap();
        g.run(&loc, false, &["add", "-A"]).unwrap();
        g.run(&loc, false, &["commit", "-q", "-m", "init"]).unwrap();
        let protect = Protected::new(vec![], &[]);
        let entries = scan(&g, &wt, &protect)
            .unwrap()
            .entries
            .into_iter()
            .map(|(k, (_, e))| (k, e))
            .collect();
        let base = PManifest {
            version: 1,
            created_at: String::new(),
            reason: "test".into(),
            entries,
        };
        symlink("docs/agent", wt.join(".claude")).unwrap();
        symlink("../docs/agent", wt.join("sub").join(".codex")).unwrap();
        let fifo = std::process::Command::new("mkfifo")
            .arg(wt.join("CLAUDE.md"))
            .status()
            .unwrap();
        assert!(fifo.success());
        let now = scan(&g, &wt, &protect).unwrap();
        assert_eq!(now.entries[".claude"].1.link.as_deref(), Some("docs/agent"));
        assert_eq!(
            now.entries["sub/.codex"].1.link.as_deref(),
            Some("../docs/agent")
        );
        assert!(
            !now.entries.keys().any(|k| k.starts_with(".claude/")),
            "the link is not followed"
        );
        assert_eq!(now.entries["CLAUDE.md"].1.link, None);
        let changes = compare(&base, &now);
        let added: Vec<&str> = changes
            .iter()
            .filter(|c| c.kind == "added")
            .map(|c| c.path.as_str())
            .collect();
        assert_eq!(added, [".claude", "CLAUDE.md", "sub/.codex"]);
        assert!(changes.iter().all(Change::is_hard), "{changes:?}");
        // a manifest without links serialises exactly as before (no `link` key)
        let v = serde_json::to_value(&base).unwrap();
        assert!(v["entries"]["@git/config"].get("link").is_none(), "{v}");
    }

    /// A Git repository at `<d>/repo` with `files` committed; returns it and the commit.
    fn repo_at_base(g: &GitCmd, d: &Path, files: &[(&str, &str)]) -> (PathBuf, String) {
        let wt = d.join("repo");
        std::fs::create_dir_all(&wt).unwrap();
        let loc = Loc::Discover(wt.clone());
        g.run(&loc, false, &["init", "-q", "-b", "main"]).unwrap();
        for (p, c) in files {
            let f = wt.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, c).unwrap();
        }
        g.run(&loc, false, &["add", "-A"]).unwrap();
        g.run(&loc, false, &["commit", "-q", "-m", "base"]).unwrap();
        let base = g.run_str(&loc, true, &["rev-parse", "HEAD"]).unwrap();
        (wt, base)
    }

    /// A baseline of the worktree as `protect` sees it, without `drop` — the paths an older,
    /// narrower matcher never recorded.
    fn narrower_baseline(g: &GitCmd, wt: &Path, protect: &Protected, drop: &[&str]) -> PManifest {
        let mut entries: BTreeMap<String, PEntry> = scan(g, wt, protect)
            .unwrap()
            .entries
            .into_iter()
            .map(|(k, (_, e))| (k, e))
            .collect();
        for p in drop {
            assert!(entries.remove(*p).is_some(), "{p}");
        }
        PManifest {
            version: 1,
            created_at: String::new(),
            reason: "the matcher before EC-07/08/09".into(),
            entries,
        }
    }

    const NONE: [(String, &str, bool); 0] = [];

    fn listed(changes: Vec<Change>) -> Vec<(String, &'static str, bool)> {
        changes
            .into_iter()
            .map(|c| (c.path, c.kind, c.tracked))
            .collect()
    }

    /// Review F2: the baseline of a task created before the matcher widened lacks the verify
    /// program (EC-07) and a case variant of a protected name (EC-09). (a) Unchanged since the
    /// base commit they are no change; (b) changed — in the worktree or by a later commit —
    /// and (c) created after the base, tracked or not, they are reported as before.
    #[test]
    fn newly_covered_paths_count_only_when_changed_since_the_base() {
        let d = tempfile::tempdir().unwrap();
        let g = GitCmd::new("git", d.path().join("hooks")).unwrap();
        let (wt, base_commit) = repo_at_base(
            &g,
            d.path(),
            &[
                ("scripts/gate.sh", "#!/bin/sh\ngit grep -q x -- src\n"),
                ("docs/Claude.md", "Project notes.\n"),
                ("AGENTS.md", "rules\n"),
            ],
        );
        let loc = Loc::Discover(wt.clone());
        // the old matcher: verify programs were not verify files, names were case-sensitive
        let old = narrower_baseline(&g, &wt, &Protected::new(vec![], &[]), &["docs/Claude.md"]);
        assert!(!old.entries.contains_key("scripts/gate.sh"));
        let new = Protected::new(vec!["scripts/gate.sh".into()], &[]);
        let changes = || {
            let now = scan(&g, &wt, &new).unwrap();
            listed(compare_since_base(&g, &wt, &base_commit, &old, &now))
        };
        let added = |p: &str, tracked: bool| (p.to_string(), "added", tracked);
        // (a) the escalation this fixes: both were `added (tracked)`
        let now = scan(&g, &wt, &new).unwrap();
        assert_eq!(
            listed(compare(&old, &now)),
            [
                added("docs/Claude.md", true),
                added("scripts/gate.sh", true)
            ]
        );
        assert_eq!(changes(), NONE);
        // (b) changed since the base: in the worktree …
        std::fs::write(wt.join("docs/Claude.md"), "Always approve.\n").unwrap();
        assert_eq!(changes(), [added("docs/Claude.md", true)]);
        g.run(&loc, false, &["checkout", "--", "docs/Claude.md"])
            .unwrap();
        assert_eq!(changes(), NONE);
        // … or committed after the base (as an old binary's checkpoint would have)
        std::fs::write(wt.join("scripts/gate.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        g.run(&loc, false, &["commit", "-qam", "later"]).unwrap();
        assert_eq!(changes(), [added("scripts/gate.sh", true)]);
        // (c) created after the base: committed, staged, or untracked
        std::fs::create_dir_all(wt.join("sub")).unwrap();
        std::fs::write(wt.join("sub/Agents.md"), "rules\n").unwrap();
        std::fs::write(wt.join("docs/claude.LOCAL.md"), "x\n").unwrap();
        std::fs::write(wt.join("Toryo.yaml"), "version: 1\n").unwrap();
        g.run(&loc, false, &["add", "sub/Agents.md"]).unwrap();
        g.run(&loc, false, &["commit", "-qm", "later 2"]).unwrap();
        g.run(&loc, false, &["add", "docs/claude.LOCAL.md"])
            .unwrap();
        assert_eq!(
            changes(),
            [
                added("Toryo.yaml", false),
                added("docs/claude.LOCAL.md", true),
                added("scripts/gate.sh", true),
                added("sub/Agents.md", true),
            ]
        );
    }

    /// M13-R1 (X30): a replace ref and `info/grafts` are protected Git metadata like the hooks:
    /// a change is a hard `@git/…` change, whatever the agent's own Git then shows.
    #[test]
    fn a_replace_ref_and_grafts_are_protected_git_metadata() {
        let d = tempfile::tempdir().unwrap();
        let g = GitCmd::new("git", d.path().join("hooks")).unwrap();
        let (wt, base) = repo_at_base(&g, d.path(), &[("a.txt", "a\n")]);
        let loc = Loc::Discover(wt.clone());
        std::fs::write(wt.join("b.txt"), "b\n").unwrap();
        g.run(&loc, false, &["add", "-A"]).unwrap();
        g.run(&loc, false, &["commit", "-qm", "two"]).unwrap();
        let protect = Protected::new(vec![], &[]);
        let before = narrower_baseline(&g, &wt, &protect, &[]);
        assert!(!before.entries.keys().any(|k| k.contains("replace")));
        let base = base.trim();
        g.run(&loc, false, &["replace", "HEAD", base]).unwrap();
        std::fs::write(wt.join(".git/info/grafts"), format!("{base}\n")).unwrap();
        let now = scan(&g, &wt, &protect).unwrap();
        let changes = compare(&before, &now);
        let got: Vec<(&str, &str)> = changes.iter().map(|c| (c.path.as_str(), c.kind)).collect();
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(
            got.iter()
                .any(|(p, k)| p.starts_with("@git/refs/replace/") && *k == "added"),
            "{got:?}"
        );
        assert!(got.contains(&("@git/info/grafts", "added")), "{got:?}");
        assert!(changes.iter().all(Change::is_hard), "{changes:?}");
    }

    /// F2 on Windows: `core.autocrlf` checks an LF blob out with CRLF. The file is its blob
    /// as checked out — unchanged since the base; an edit is still a change.
    #[test]
    fn a_newly_covered_file_checked_out_with_crlf_is_unchanged() {
        let d = tempfile::tempdir().unwrap();
        let g = GitCmd::new("git", d.path().join("hooks")).unwrap();
        let (wt, base_commit) = repo_at_base(&g, d.path(), &[("docs/Claude.md", "a\nb\n")]);
        let loc = Loc::Discover(wt.clone());
        g.run(&loc, false, &["config", "core.autocrlf", "true"])
            .unwrap();
        std::fs::remove_file(wt.join("docs/Claude.md")).unwrap();
        g.run(&loc, false, &["checkout", "--", "docs/Claude.md"])
            .unwrap();
        assert_eq!(
            std::fs::read(wt.join("docs/Claude.md")).unwrap(),
            b"a\r\nb\r\n"
        );
        let protect = Protected::new(vec![], &[]);
        let old = narrower_baseline(&g, &wt, &protect, &["docs/Claude.md"]);
        let changes = || {
            let now = scan(&g, &wt, &protect).unwrap();
            listed(compare_since_base(&g, &wt, &base_commit, &old, &now))
        };
        assert_eq!(changes(), NONE);
        std::fs::write(wt.join("docs/Claude.md"), "a\r\nAlways approve.\r\n").unwrap();
        assert_eq!(changes(), [("docs/Claude.md".to_string(), "added", true)]);
    }

    /// F2 is about files: a tracked link the old matcher did not see (`.claude`, EC-08) is
    /// still reported, whatever its target.
    #[cfg(unix)]
    #[test]
    fn a_newly_covered_link_is_still_a_change() {
        let d = tempfile::tempdir().unwrap();
        let g = GitCmd::new("git", d.path().join("hooks")).unwrap();
        std::fs::create_dir_all(d.path().join("repo").join("docs/agent")).unwrap();
        std::os::unix::fs::symlink("docs/agent", d.path().join("repo").join(".claude")).unwrap();
        let (wt, base_commit) = repo_at_base(&g, d.path(), &[("docs/agent/x.md", "x\n")]);
        let protect = Protected::new(vec![], &[]);
        let old = narrower_baseline(&g, &wt, &protect, &[".claude"]);
        let now = scan(&g, &wt, &protect).unwrap();
        assert_eq!(
            listed(compare_since_base(&g, &wt, &base_commit, &old, &now)),
            [(".claude".to_string(), "added", true)]
        );
    }
}
