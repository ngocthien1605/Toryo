//! Recovery snapshots (plan §10, R39): a list of file names is not a copy of the files.
//! Before any recovery checkpoint, and always before a discard, the worktree's uncommitted
//! state is written to a **new** directory `<run>/artifacts/partial/<attempt>/<n>-<sha12>/`:
//!
//! | file | restores |
//! |---|---|
//! | `tracked-index.patch` | staged changes (`git diff --cached <CANON_DIFF> HEAD`), incl. binary |
//! | `index-worktree.patch` | unstaged changes to tracked files (`git diff <CANON_DIFF>`) |
//! | `worktree/…` | byte copies of every staged / unstaged file as it was in the worktree |
//! | `files/…` | byte copies of untracked, non-ignored files |
//! | `protected/…` | byte copies of ignored protected files (for inspection) |
//! | `manifest.json` | head + every entry with size and SHA-256; written **last** |
//!
//! [`completion_check`] must pass before anything destructive; [`export`] re-creates the
//! state in a fresh detached checkout so nothing discarded is lost.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::checks::Protected;
use super::task::sha256_hex;
use super::worktree::Worktree;
use crate::git::{Loc, CANON_DIFF};

pub const TRACKED: &str = "tracked-index.patch";
pub const UNSTAGED: &str = "index-worktree.patch";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    /// staged | unstaged | untracked | ignored_protected
    pub kind: String,
    pub size: u64,
    /// SHA-256 of the file as it was in the worktree (`""` = deleted).
    pub sha256: String,
    pub mode: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub task: String,
    pub attempt: String,
    pub version: u32,
    pub head: String,
    pub entries: Vec<Entry>,
    pub tracked_patch_sha256: String,
    pub unstaged_patch_sha256: String,
    pub created_at: String,
}

impl Manifest {
    pub fn paths(&self, kind: &str) -> Vec<&str> {
        self.entries.iter().filter(|e| e.kind == kind).map(|e| e.path.as_str()).collect()
    }
}

fn split_z(b: &[u8]) -> Vec<String> {
    b.split(|c| *c == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).to_string()).collect()
}

#[cfg(unix)]
fn mode_of(p: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).ok().map(|m| m.permissions().mode())
}
#[cfg(not(unix))]
fn mode_of(_p: &Path) -> Option<u32> {
    None
}

fn file_entry(root: &Path, path: &str, kind: &str) -> Result<Entry> {
    let p = root.join(path);
    Ok(match std::fs::read(&p) {
        Ok(b) => Entry { path: path.into(), kind: kind.into(), size: b.len() as u64, sha256: sha256_hex(&b), mode: mode_of(&p) },
        Err(_) => Entry { path: path.into(), kind: kind.into(), size: 0, sha256: String::new(), mode: None },
    })
}

fn copy_into(src_root: &Path, path: &str, dst_root: &Path) -> Result<()> {
    let dst = dst_root.join(path);
    std::fs::create_dir_all(dst.parent().expect("parent"))?;
    std::fs::copy(src_root.join(path), &dst).with_context(|| format!("copying {path}"))?;
    Ok(())
}

/// Take a new snapshot version under `root` (the attempt's snapshot directory).
pub fn take(wt: &Worktree, protect: &Protected, root: &Path, task: &str, attempt: &str) -> Result<(PathBuf, Manifest)> {
    std::fs::create_dir_all(root)?;
    let head = wt.head()?;
    let staging = root.join(format!(".staging-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let g = wt.g;
    let mut args = vec!["diff", "--cached"];
    args.extend_from_slice(CANON_DIFF);
    args.push("HEAD");
    let tracked = g.run(&wt.loc, true, &args)?;
    let mut args = vec!["diff"];
    args.extend_from_slice(CANON_DIFF);
    let unstaged = g.run(&wt.loc, true, &args)?;
    std::fs::write(staging.join(TRACKED), &tracked)?;
    std::fs::write(staging.join(UNSTAGED), &unstaged)?;
    let mut entries = Vec::new();
    // Byte copies of the tracked changes too: with `core.autocrlf` (the Git for Windows
    // default) a patch round trip does not restore the exact bytes; the copies do.
    for (kind, args) in [
        ("staged", vec!["diff", "--cached", "--name-only", "-z", "--no-renames", "HEAD"]),
        ("unstaged", vec!["diff", "--name-only", "-z", "--no-renames"]),
    ] {
        for p in split_z(&g.run(&wt.loc, true, &args)?) {
            let e = file_entry(&wt.path, &p, kind)?;
            if !e.sha256.is_empty() {
                copy_into(&wt.path, &p, &staging.join("worktree"))?;
            }
            entries.push(e);
        }
    }
    for p in split_z(&g.run(&wt.loc, true, &["ls-files", "-z", "--others", "--exclude-standard"])?) {
        copy_into(&wt.path, &p, &staging.join("files"))?;
        entries.push(file_entry(&wt.path, &p, "untracked")?);
    }
    for p in split_z(&g.run(&wt.loc, true, &["ls-files", "-z", "--others", "--ignored", "--exclude-standard"])?) {
        if p.starts_with(".toryo-tmp/") || !protect.matches(&p) {
            continue;
        }
        copy_into(&wt.path, &p, &staging.join("protected"))?;
        entries.push(file_entry(&wt.path, &p, "ignored_protected")?);
    }
    let version = std::fs::read_dir(root)?
        .filter_map(|e| e.ok())
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .count() as u32
        + 1;
    let m = Manifest {
        task: task.into(),
        attempt: attempt.into(),
        version,
        head,
        entries,
        tracked_patch_sha256: sha256_hex(&tracked),
        unstaged_patch_sha256: sha256_hex(&unstaged),
        created_at: crate::evidence::now_rfc3339(),
    };
    let bytes = serde_json::to_vec_pretty(&m)?;
    std::fs::write(staging.join("manifest.json"), &bytes)?; // last
    let dir = root.join(format!("{version}-{}", &sha256_hex(&bytes)[..12]));
    std::fs::rename(&staging, &dir)?; // a new path: never overwrites an earlier version (R34)
    Ok((dir, m))
}

pub fn read_manifest(dir: &Path) -> Result<Manifest> {
    let b = std::fs::read(dir.join("manifest.json")).with_context(|| format!("snapshot_incomplete: no manifest in {}", dir.display()))?;
    Ok(serde_json::from_slice(&b)?)
}

/// Before anything destructive (§10): the manifest exists, every copied file re-hashes to
/// its entry, the patches are unchanged and apply in sequence in a scratch index (never
/// touching the worktree).
pub fn completion_check(wt: &Worktree, dir: &Path) -> Result<Manifest> {
    let m = read_manifest(dir)?;
    let fail = |what: String| -> Result<Manifest> { bail!("snapshot_incomplete: {what} ({})", dir.display()) };
    for (name, want) in [(TRACKED, &m.tracked_patch_sha256), (UNSTAGED, &m.unstaged_patch_sha256)] {
        match std::fs::read(dir.join(name)) {
            Ok(b) if sha256_hex(&b) == *want => {}
            _ => return fail(format!("{name} is missing or changed")),
        }
    }
    for e in &m.entries {
        let sub = match e.kind.as_str() {
            "untracked" => "files",
            "ignored_protected" => "protected",
            _ if e.sha256.is_empty() => continue, // deleted in the worktree
            _ => "worktree",
        };
        match std::fs::read(dir.join(sub).join(&e.path)) {
            Ok(b) if sha256_hex(&b) == e.sha256 => {}
            _ => return fail(format!("copy of `{}` is missing or changed", e.path)),
        }
    }
    let scratch = dir.parent().expect("parent").join(format!(".check-index-{}", std::process::id()));
    let _ = std::fs::remove_file(&scratch);
    let env = [("GIT_INDEX_FILE", scratch.as_path())];
    let g = wt.g;
    let res = (|| -> Result<()> {
        let o = g.output_env(&wt.loc, false, &["read-tree", &m.head], &env)?;
        if !o.status.success() {
            bail!("read-tree {}: {}", m.head, String::from_utf8_lossy(&o.stderr).trim());
        }
        for (name, check) in [(TRACKED, false), (UNSTAGED, true)] {
            let p = dir.join(name);
            if std::fs::metadata(&p)?.len() == 0 {
                continue;
            }
            let ps = p.display().to_string();
            let mut args = vec!["apply", "--cached", "--binary"];
            if check {
                args.push("--check");
            }
            args.push(&ps);
            let o = g.output_env(&wt.loc, false, &args, &env)?;
            if !o.status.success() {
                bail!("{name} does not apply: {}", String::from_utf8_lossy(&o.stderr).trim());
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&scratch);
    if let Err(e) = res {
        return fail(format!("{e:#}"));
    }
    Ok(m)
}

/// `toryo partial export` (R39): re-create the snapshot's state in a fresh detached checkout
/// of its head at `into` (which must not exist or be empty): the staged changes go into its
/// index, and every file gets its recorded bytes.
pub fn export(wt: &Worktree, dir: &Path, into: &Path) -> Result<Manifest> {
    let m = completion_check(wt, dir)?;
    if into.exists() && std::fs::read_dir(into)?.next().is_some() {
        bail!("{} is not empty", into.display());
    }
    let into_s = into.display().to_string().replace('\\', "/");
    wt.g.run(&wt.loc, false, &["worktree", "add", "--detach", "-q", &into_s, &m.head])?;
    let dst = Worktree { g: wt.g, path: into.to_path_buf(), loc: Loc::Discover(into.to_path_buf()) };
    let tracked = dir.join(TRACKED);
    if std::fs::metadata(&tracked)?.len() > 0 {
        let ps = tracked.display().to_string();
        dst.g.run(&dst.loc, false, &["apply", "--cached", "--binary", &ps])?;
    }
    for e in &m.entries {
        let sub = match e.kind.as_str() {
            "untracked" => "files",
            "ignored_protected" => "protected",
            _ if e.sha256.is_empty() => {
                let _ = std::fs::remove_file(into.join(&e.path));
                continue;
            }
            _ => "worktree",
        };
        copy_into(&dir.join(sub), &e.path, into)?;
    }
    Ok(m)
}
