//! Immutable artifact versions (R34, plan §9). Every version is published as its own file,
//! `runstore/artifacts/<logical>/<version>-<sha12>.<ext>`, never overwritten, **before** one
//! SQLite transaction registers it and moves the logical head (`artifact_heads`):
//!
//! 1. write to `artifacts/.staging/`, flush to disk;
//! 2. rename to the version path (publish);
//! 3. one transaction: `artifacts` row + head.
//!
//! A crash between 1 and 2 leaves a staging file (deleted at reconciliation); between 2 and 3
//! a published file with no row — an **orphan**, moved to `orphans/` and logged, never
//! `artifact_tampered`. The files at the run-store root (`PLAN.md`, `review.json`, …) are
//! derived **views** for humans, rewritten after each commit; they are never evidence.
//! Agents get the version paths (§7), and every hand-off re-hashes what it hands over.

use crate::git::short;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rusqlite::OptionalExtension;

use super::store::{self, Store, TaskRow};
use super::task::sha256_hex;

/// One registered version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub id: String,
    pub logical: String,
    pub version: u32,
    /// Absolute path of the immutable file.
    pub path: PathBuf,
    pub sha256: String,
}

pub fn root(t: &TaskRow) -> PathBuf {
    t.artifacts().join("artifacts")
}

fn staging(t: &TaskRow) -> PathBuf {
    root(t).join(".staging")
}

/// A registered path (run-store relative, `/`-separated) as a native absolute path.
fn abs(t: &TaskRow, rel: &str) -> PathBuf {
    rel.split(['/', '\\'])
        .filter(|c| !c.is_empty())
        .fold(t.run_store.clone(), |p, c| p.join(c))
}

fn rel(t: &TaskRow, p: &Path) -> String {
    p.strip_prefix(&t.run_store)
        .map(|r| r.display().to_string().replace('\\', "/"))
        .unwrap_or_else(|_| p.display().to_string())
}

/// The version number the next `put` of `logical` gets.
pub fn next_version(store: &Store, t: &TaskRow, logical: &str) -> Result<u32> {
    let (tid, l) = (t.id.clone(), logical.to_string());
    store.call(move |tx| {
        Ok(tx.query_row(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM artifacts WHERE task_id = ?1 AND logical_name = ?2",
            [&tid, &l],
            |r| r.get::<_, i64>(0),
        )? as u32)
    })
}

fn sync_dir(_d: &Path) {
    #[cfg(unix)]
    if let Ok(f) = std::fs::File::open(_d) {
        let _ = f.sync_all();
    }
}

/// Publish `bytes` as the next version of `logical` and register it (see the module doc).
/// `view`: also (re)write this derived file under the run store root.
#[allow(clippy::too_many_arguments)]
pub fn put(
    store: &Store,
    t: &TaskRow,
    attempt: Option<&str>,
    logical: &str,
    ext: &str,
    kind: &str,
    bytes: &[u8],
    view: Option<&str>,
) -> Result<Version> {
    use std::io::Write;
    let version = next_version(store, t, logical)?;
    let sha = sha256_hex(bytes);
    // 1. staging, durable
    std::fs::create_dir_all(staging(t))?;
    let tmp = staging(t).join(format!(
        "{}-{version}-{}.{ext}",
        logical.replace('/', "_"),
        short(&sha)
    ));
    {
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("staging {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    super::runner::crash_point(&format!("artifact:{logical}@{version}:before_publish"));
    // 2. publish (never overwrite)
    let dir = root(t).join(logical);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{version}-{}.{ext}", short(&sha)));
    if path.exists() {
        // a leftover of a crash between publish and commit that reconciliation has not seen
        if sha256_hex(&std::fs::read(&path)?) != sha {
            bail!(
                "artifact_tampered: {} exists with other content",
                path.display()
            );
        }
        std::fs::remove_file(&tmp)?;
    } else {
        std::fs::rename(&tmp, &path).with_context(|| format!("publishing {}", path.display()))?;
    }
    sync_dir(&dir);
    super::runner::crash_point(&format!("artifact:{logical}@{version}:after_publish"));
    // 3. one transaction: row + head
    let (tid, aid, l, k, r, s2, size) = (
        t.id.clone(),
        attempt.map(str::to_string),
        logical.to_string(),
        kind.to_string(),
        rel(t, &path),
        sha.clone(),
        bytes.len() as u64,
    );
    let id = store.call(move |tx| {
        store::register_artifact(tx, &tid, aid.as_deref(), &l, &k, &r, &s2, size)
    })?;
    if !id.ends_with(&format!("/{version}")) {
        bail!("artifact {id}: expected version {version} (concurrent writer?)");
    }
    super::runner::crash_point(&format!("artifact:{logical}@{version}:after_commit"));
    if let Some(v) = view {
        let p = t.artifacts().join(v);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, bytes)?;
    }
    Ok(Version {
        id,
        logical: logical.to_string(),
        version,
        path,
        sha256: sha,
    })
}

/// The head version of `logical`, if any (no hash check).
pub fn head(store: &Store, t: &TaskRow, logical: &str) -> Result<Option<Version>> {
    let (tid, l) = (t.id.clone(), logical.to_string());
    let row = store.call(move |tx| {
        Ok(tx
            .query_row(
                "SELECT a.id, a.version, a.path, a.sha256 FROM artifact_heads h JOIN artifacts a ON a.id = h.artifact_id WHERE h.task_id = ?1 AND h.logical_name = ?2",
                [&tid, &l],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)),
            )
            .optional()?)
    })?;
    Ok(row.map(|(id, v, p, sha)| Version {
        id,
        logical: logical.to_string(),
        version: v as u32,
        path: abs(t, &p),
        sha256: sha,
    }))
}

/// Read a version and check its hash: `artifact_missing` / `artifact_tampered` otherwise.
pub fn read_version(v: &Version) -> Result<Vec<u8>> {
    let b = std::fs::read(&v.path)
        .map_err(|_| anyhow::anyhow!("artifact_missing: {} ({})", v.id, v.path.display()))?;
    if sha256_hex(&b) != v.sha256 {
        bail!(
            "artifact_tampered: {} ({}) does not match its registered SHA-256",
            v.id,
            v.path.display()
        );
    }
    Ok(b)
}

/// The head of `logical`, hash-checked.
pub fn read_head(store: &Store, t: &TaskRow, logical: &str) -> Result<Option<(Version, Vec<u8>)>> {
    match head(store, t, logical)? {
        Some(v) => {
            let b = read_version(&v)?;
            Ok(Some((v, b)))
        }
        None => Ok(None),
    }
}

/// Append-only text artifacts (DECISIONS.md): head content + `text` as a new version.
pub fn append(
    store: &Store,
    t: &TaskRow,
    logical: &str,
    ext: &str,
    kind: &str,
    text: &str,
    view: &str,
) -> Result<Version> {
    let mut cur = read_head(store, t, logical)?
        .map(|(_, b)| b)
        .unwrap_or_default();
    cur.extend_from_slice(text.as_bytes());
    put(store, t, None, logical, ext, kind, &cur, Some(view))
}

/// What reconciliation found.
#[derive(Debug, Default)]
pub struct Reconciled {
    pub notes: Vec<String>,
    /// `artifact_missing: …` / `artifact_tampered: …` → the task escalates.
    pub problem: Option<String>,
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if e.file_type().map(|f| f.is_dir()).unwrap_or(false) {
                files(&p, out);
            } else {
                out.push(p);
            }
        }
    }
}

/// §10 step 4 (R34): delete staging leftovers, move unregistered published files to
/// `orphans/`, check every registered version exists and hashes right.
pub fn reconcile(store: &Store, t: &TaskRow) -> Result<Reconciled> {
    let mut r = Reconciled::default();
    if let Ok(rd) = std::fs::read_dir(staging(t)) {
        for e in rd.flatten() {
            let _ = std::fs::remove_file(e.path());
            r.notes.push(format!(
                "staging leftover removed: {}",
                e.file_name().to_string_lossy()
            ));
        }
    }
    let tid = t.id.clone();
    let rows: Vec<(String, String, String, String)> = store.call(move |tx| {
        let mut q =
            tx.prepare("SELECT id, path, sha256, kind FROM artifacts WHERE task_id = ?1")?;
        let v = q
            .query_map([&tid], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    })?;
    let registered: std::collections::BTreeSet<String> =
        rows.iter().map(|x| x.1.replace('\\', "/")).collect();
    let mut published = Vec::new();
    files(&root(t), &mut published);
    for p in published.into_iter().filter(|p| !p.starts_with(staging(t))) {
        let rp = rel(t, &p);
        if !registered.contains(&rp) {
            let dst = t
                .artifacts()
                .join("orphans")
                .join(p.strip_prefix(root(t)).unwrap_or(&p));
            std::fs::create_dir_all(dst.parent().expect("parent"))?;
            std::fs::rename(&p, &dst)?;
            r.notes.push(format!(
                "orphan (published, never registered) moved to {}",
                dst.display()
            ));
            let (i, d) = (
                t.id.clone(),
                serde_json::json!({"file": rp, "moved_to": dst.display().to_string()}),
            );
            store.call(move |tx| store::event(tx, Some(&i), None, "artifact_orphan", &d))?;
        }
    }
    for (id, path, sha, kind) in rows {
        if kind == "partial_snapshot" {
            continue; // a directory; its manifest is checked by `snapshot::completion_check`
        }
        let v = Version {
            id: id.clone(),
            logical: String::new(),
            version: 0,
            path: abs(t, &path),
            sha256: sha,
        };
        if let Err(e) = read_version(&v) {
            r.problem.get_or_insert_with(|| format!("{e:#}"));
        }
    }
    Ok(r)
}

/// View name → logical name of the task-pack artifacts (§7 source 5).
pub fn logical_of_view(view: &str) -> Option<&'static str> {
    Some(match view {
        "TASK.md" => "task",
        "PLAN.md" => "plan-md",
        "DECISIONS.md" => "decisions",
        "VERIFY_RESULT.json" => "verify-result",
        "REVIEW.md" => "review-md",
        "DIFF.patch" => "diff",
        "DIFF-incremental.patch" => "diff-incremental",
        _ => return None,
    })
}
