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
//!
//! No entry type stops a snapshot (EC-20) — a recovery that cannot snapshot can never close
//! the crashed attempt. Links are never followed:
//! - a **symlink or junction** (dangling or not) is recorded as a link: its copy holds the
//!   link target, like a Git symlink blob; [`export`] re-creates the link;
//! - a **nested Git repository** (`ls-files` prints `vendor/lib/`) is recorded — path and
//!   type — but not copied: it can be any size and brings another repository's `.git`
//!   (config, hooks) with it, and nothing destructive touches it: `--discard-partial`'s
//!   `git clean -fd` never deletes a nested repository (only `-ff` would), and a recovery
//!   checkpoint records it as a gitlink, like any checkpoint;
//! - another **directory** is walked, so its files are copied one by one;
//! - a FIFO, socket or device is recorded without being opened (reading one can block).

use crate::git::short;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::checks::Protected;
use super::task::sha256_hex;
use super::worktree::Worktree;
use crate::git::{Loc, CANON_DIFF};

pub const TRACKED: &str = "tracked-index.patch";
pub const UNSTAGED: &str = "index-worktree.patch";

/// [`Entry::file_type`] of a symlink or junction.
pub const SYMLINK: &str = "symlink";
/// [`Entry::file_type`] of a nested Git repository (recorded, not copied).
pub const NESTED_REPO: &str = "repo";
/// [`Entry::file_type`] of a FIFO, socket or device (recorded, not copied).
pub const SPECIAL: &str = "special";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    /// staged | unstaged | untracked | ignored_protected
    pub kind: String,
    pub size: u64,
    /// SHA-256 of the file as it was in the worktree (`""` = deleted, or nothing copied); of
    /// the link target for a symlink.
    pub sha256: String,
    pub mode: Option<u32>,
    /// `None`: a regular file (or deleted). Else [`SYMLINK`], [`NESTED_REPO`] or [`SPECIAL`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_type: Option<String>,
    /// The target of a symlink or junction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

impl Entry {
    /// The snapshot holds a byte copy of this entry: a file, or a link's target text — not
    /// a deleted path, a nested repository or a special file.
    pub fn has_copy(&self) -> bool {
        !self.sha256.is_empty() && matches!(self.file_type.as_deref(), None | Some(SYMLINK))
    }

    /// The snapshot subdirectory of its copy.
    fn subdir(&self) -> &'static str {
        match self.kind.as_str() {
            "untracked" => "files",
            "ignored_protected" => "protected",
            _ => "worktree",
        }
    }
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
        self.entries
            .iter()
            .filter(|e| e.kind == kind)
            .map(|e| e.path.as_str())
            .collect()
    }
}

fn split_z(b: &[u8]) -> Vec<String> {
    b.split(|c| *c == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).to_string())
        .collect()
}

#[cfg(unix)]
fn mode_of(m: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(m.permissions().mode())
}
#[cfg(not(unix))]
fn mode_of(_m: &std::fs::Metadata) -> Option<u32> {
    None
}

/// Is a directory on the way to `rel` a link? Git then sees nothing at `rel`, and reading it
/// would follow the link out of the worktree.
fn behind_link(root: &Path, rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').filter(|c| !c.is_empty()).collect();
    let mut p = root.to_path_buf();
    parts[..parts.len().saturating_sub(1)].iter().any(|c| {
        p.push(c);
        std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink())
    })
}

/// Record `path` (worktree-relative, as Git lists it) as a `kind` entry and copy what can
/// be restored into `dst_root` — following no link and failing on no entry type (EC-20, see
/// the module doc). A path that is gone (or behind a link) is recorded as deleted.
fn capture(
    root: &Path,
    path: &str,
    kind: &str,
    dst_root: &Path,
    out: &mut Vec<Entry>,
) -> Result<()> {
    let rel = path.trim_end_matches('/');
    let src = root.join(rel);
    let entry = |size: u64, sha256: String, mode, file_type: Option<&str>, link| Entry {
        path: rel.to_string(),
        kind: kind.to_string(),
        size,
        sha256,
        mode,
        file_type: file_type.map(str::to_string),
        link,
    };
    let md = match std::fs::symlink_metadata(&src) {
        Ok(m) if !behind_link(root, rel) => m,
        Ok(_) => {
            out.push(entry(0, String::new(), None, None, None));
            return Ok(());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            out.push(entry(0, String::new(), None, None, None));
            return Ok(());
        }
        Err(e) => return Err(e).with_context(|| format!("snapshot of {rel}")),
    };
    let dst = dst_root.join(rel);
    let ft = md.file_type();
    if ft.is_symlink() {
        let target = std::fs::read_link(&src)
            .with_context(|| format!("reading the link {rel}"))?
            .to_string_lossy()
            .to_string();
        std::fs::create_dir_all(dst.parent().expect("parent"))?;
        std::fs::write(&dst, target.as_bytes())?;
        out.push(entry(
            target.len() as u64,
            sha256_hex(target.as_bytes()),
            mode_of(&md),
            Some(SYMLINK),
            Some(target),
        ));
    } else if ft.is_dir() {
        if std::fs::symlink_metadata(src.join(".git")).is_ok() {
            out.push(entry(0, String::new(), None, Some(NESTED_REPO), None));
        } else if matches!(kind, "staged" | "unstaged") {
            // a tracked file replaced by a directory: the file is gone, the directory's
            // files are untracked and listed on their own
            out.push(entry(0, String::new(), None, None, None));
        } else {
            let mut names: Vec<String> = std::fs::read_dir(&src)
                .with_context(|| format!("listing {rel}"))?
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            names.sort();
            for n in names {
                capture(root, &format!("{rel}/{n}"), kind, dst_root, out)?;
            }
        }
    } else if ft.is_file() {
        std::fs::create_dir_all(dst.parent().expect("parent"))?;
        std::fs::copy(&src, &dst).with_context(|| format!("copying {rel}"))?;
        let b = std::fs::read(&dst)?; // the manifest describes exactly the stored copy
        out.push(entry(
            b.len() as u64,
            sha256_hex(&b),
            mode_of(&md),
            None,
            None,
        ));
    } else {
        out.push(entry(0, String::new(), mode_of(&md), Some(SPECIAL), None));
    }
    Ok(())
}

/// `dst` is about to be written by [`export`]: its parent exists and nothing — above all no
/// link the checkout of `head` put there — stands at `dst`, so a copy never writes through a
/// link. A directory is left alone (the copy then fails, as it should).
fn clear_for_write(dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst.parent().expect("parent"))?;
    if std::fs::symlink_metadata(dst).is_ok_and(|m| !m.is_dir()) {
        remove_link_or_file(dst)?;
    }
    Ok(())
}

fn remove_link_or_file(p: &Path) -> Result<()> {
    if std::fs::remove_file(p).is_err() && std::fs::symlink_metadata(p).is_ok() {
        // a directory symlink or junction on Windows is removed as a directory
        std::fs::remove_dir(p).with_context(|| format!("removing {}", p.display()))?;
    }
    Ok(())
}

/// Re-create a recorded link (export).
#[cfg(unix)]
fn make_link(target: &str, dst: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, dst)
        .with_context(|| format!("re-creating the link {} -> {target}", dst.display()))
}

/// Re-create a recorded link (export). Without the symlink privilege, Git's own fallback
/// (`core.symlinks=false`): a file holding the target.
#[cfg(windows)]
fn make_link(target: &str, dst: &Path) -> Result<()> {
    let t = Path::new(target);
    let resolved = dst.parent().map(|d| d.join(t)).unwrap_or_else(|| t.into());
    let made = if resolved.is_dir() {
        std::os::windows::fs::symlink_dir(t, dst)
    } else {
        std::os::windows::fs::symlink_file(t, dst)
    };
    if made.is_err() {
        std::fs::write(dst, target.as_bytes())?;
    }
    Ok(())
}

/// Take a new snapshot version under `root` (the attempt's snapshot directory).
pub fn take(
    wt: &Worktree,
    protect: &Protected,
    root: &Path,
    task: &str,
    attempt: &str,
) -> Result<(PathBuf, Manifest)> {
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
        (
            "staged",
            vec![
                "diff",
                "--cached",
                "--name-only",
                "-z",
                "--no-renames",
                "HEAD",
            ],
        ),
        (
            "unstaged",
            vec!["diff", "--name-only", "-z", "--no-renames"],
        ),
    ] {
        for p in split_z(&g.run(&wt.loc, true, &args)?) {
            capture(&wt.path, &p, kind, &staging.join("worktree"), &mut entries)?;
        }
    }
    // a nested repository is listed as `dir/`, a link as a file (never followed)
    for p in split_z(&g.run(
        &wt.loc,
        true,
        &["ls-files", "-z", "--others", "--exclude-standard"],
    )?) {
        capture(
            &wt.path,
            &p,
            "untracked",
            &staging.join("files"),
            &mut entries,
        )?;
    }
    for p in split_z(&g.run(
        &wt.loc,
        true,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
        ],
    )?) {
        if p.starts_with(".toryo-tmp/") || !protect.matches(&p) {
            continue;
        }
        capture(
            &wt.path,
            &p,
            "ignored_protected",
            &staging.join("protected"),
            &mut entries,
        )?;
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
    let dir = root.join(format!("{version}-{}", short(&sha256_hex(&bytes))));
    std::fs::rename(&staging, &dir)?; // a new path: never overwrites an earlier version (R34)
    Ok((dir, m))
}

pub fn read_manifest(dir: &Path) -> Result<Manifest> {
    let b = std::fs::read(dir.join("manifest.json"))
        .with_context(|| format!("snapshot_incomplete: no manifest in {}", dir.display()))?;
    Ok(serde_json::from_slice(&b)?)
}

/// Before anything destructive (§10): the manifest exists, every copied file re-hashes to
/// its entry, the patches are unchanged and apply in sequence in a scratch index (never
/// touching the worktree).
pub fn completion_check(wt: &Worktree, dir: &Path) -> Result<Manifest> {
    let m = read_manifest(dir)?;
    let fail = |what: String| -> Result<Manifest> {
        bail!("snapshot_incomplete: {what} ({})", dir.display())
    };
    for (name, want) in [
        (TRACKED, &m.tracked_patch_sha256),
        (UNSTAGED, &m.unstaged_patch_sha256),
    ] {
        match std::fs::read(dir.join(name)) {
            Ok(b) if sha256_hex(&b) == *want => {}
            _ => return fail(format!("{name} is missing or changed")),
        }
    }
    // deleted paths, nested repositories and special files have no copy to check
    for e in m.entries.iter().filter(|e| e.has_copy()) {
        match std::fs::read(dir.join(e.subdir()).join(&e.path)) {
            Ok(b) if sha256_hex(&b) == e.sha256 => {}
            _ => return fail(format!("copy of `{}` is missing or changed", e.path)),
        }
    }
    let scratch = dir
        .parent()
        .expect("parent")
        .join(format!(".check-index-{}", std::process::id()));
    let _ = std::fs::remove_file(&scratch);
    let env = [("GIT_INDEX_FILE", scratch.as_path())];
    let g = wt.g;
    let res = (|| -> Result<()> {
        let o = g.output_env(&wt.loc, false, &["read-tree", &m.head], &env)?;
        if !o.status.success() {
            bail!(
                "read-tree {}: {}",
                m.head,
                String::from_utf8_lossy(&o.stderr).trim()
            );
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
                bail!(
                    "{name} does not apply: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                );
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
/// index, every file gets its recorded bytes and every link its recorded target. A nested
/// repository is recorded, not copied: only its (empty) directory is created.
pub fn export(wt: &Worktree, dir: &Path, into: &Path) -> Result<Manifest> {
    let m = completion_check(wt, dir)?;
    if into.exists() && std::fs::read_dir(into)?.next().is_some() {
        bail!("{} is not empty", into.display());
    }
    let into_s = into.display().to_string().replace('\\', "/");
    wt.g.run(
        &wt.loc,
        false,
        &["worktree", "add", "--detach", "-q", &into_s, &m.head],
    )?;
    let dst = Worktree {
        g: wt.g,
        path: into.to_path_buf(),
        loc: Loc::Discover(into.to_path_buf()),
    };
    let tracked = dir.join(TRACKED);
    if std::fs::metadata(&tracked)?.len() > 0 {
        let ps = tracked.display().to_string();
        dst.g
            .run(&dst.loc, false, &["apply", "--cached", "--binary", &ps])?;
    }
    for e in &m.entries {
        let dst = into.join(&e.path);
        match e.file_type.as_deref() {
            None if e.sha256.is_empty() => {
                let _ = remove_link_or_file(&dst); // deleted in the worktree
            }
            None => {
                clear_for_write(&dst)?;
                std::fs::copy(dir.join(e.subdir()).join(&e.path), &dst)
                    .with_context(|| format!("copying {}", e.path))?;
            }
            Some(SYMLINK) => {
                clear_for_write(&dst)?;
                make_link(e.link.as_deref().unwrap_or_default(), &dst)?;
            }
            Some(NESTED_REPO) => std::fs::create_dir_all(&dst)?,
            Some(_) => {} // a special file is not re-created
        }
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::GitCmd;

    fn repo(d: &Path, g: &GitCmd, files: &[(&str, &str)]) -> PathBuf {
        let r = d.join("repo");
        std::fs::create_dir_all(&r).unwrap();
        let loc = Loc::Discover(r.clone());
        g.run(&loc, false, &["init", "-q", "-b", "main"]).unwrap();
        for (p, c) in files {
            let f = r.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, c).unwrap();
        }
        commit(g, &r);
        r
    }

    fn commit(g: &GitCmd, dir: &Path) {
        let loc = Loc::Discover(dir.to_path_buf());
        g.run(&loc, false, &["add", "-A"]).unwrap();
        g.run(&loc, false, &["commit", "-q", "-m", "commit"])
            .unwrap();
    }

    fn entry<'a>(m: &'a Manifest, path: &str) -> &'a Entry {
        m.entries
            .iter()
            .find(|e| e.path == path)
            .unwrap_or_else(|| panic!("no entry {path}: {:?}", m.entries))
    }

    fn take_v1(wt: &Worktree, snaps: &Path) -> (PathBuf, Manifest) {
        take(
            wt,
            &Protected::new(vec![], &[]),
            snaps,
            "T-1",
            "T-1/implement/1",
        )
        .unwrap()
    }

    /// EC-20: an untracked nested repository (`ls-files` prints `vendor/lib/`) no longer
    /// stops the snapshot: it is recorded, not copied — and the discard's `git clean -fd`
    /// leaves it where it is.
    #[test]
    fn a_nested_repo_is_recorded_not_copied_and_survives_the_discard() {
        let d = tempfile::tempdir().unwrap();
        let g = GitCmd::new("git", d.path().join("hooks")).unwrap();
        let r = repo(d.path(), &g, &[("a.txt", "a\n")]);
        let wt = Worktree::new(&g, &r);
        let lib = r.join("vendor").join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        g.run(&Loc::Discover(lib.clone()), false, &["init", "-q"])
            .unwrap();
        std::fs::write(lib.join("x.txt"), "x\n").unwrap();
        commit(&g, &lib);
        std::fs::write(r.join("a.txt"), "changed\n").unwrap();
        std::fs::create_dir_all(r.join("notes")).unwrap();
        std::fs::write(r.join("notes").join("n.txt"), "note\n").unwrap();
        let (dir, m) = take_v1(&wt, &d.path().join("snaps"));
        let e = entry(&m, "vendor/lib");
        assert_eq!(
            (e.kind.as_str(), e.file_type.as_deref()),
            ("untracked", Some(NESTED_REPO))
        );
        assert!(!e.has_copy() && !dir.join("files/vendor/lib").exists());
        assert!(entry(&m, "notes/n.txt").has_copy());
        assert_eq!(completion_check(&wt, &dir).unwrap(), m);
        wt.hard_reset_and_clean("HEAD").unwrap();
        assert!(
            lib.join(".git").exists() && lib.join("x.txt").exists(),
            "clean -fd keeps a nested repository"
        );
        assert!(!r.join("notes").exists());
        let into = d.path().join("export");
        export(&wt, &dir, &into).unwrap();
        assert_eq!(
            std::fs::read_to_string(into.join("a.txt")).unwrap(),
            "changed\n"
        );
        assert_eq!(
            std::fs::read_to_string(into.join("notes/n.txt")).unwrap(),
            "note\n"
        );
        assert!(into.join("vendor").join("lib").is_dir());
    }

    /// EC-20: links — dangling, to a directory, tracked and retargeted, at a protected name,
    /// ignored — are recorded as links (their copy holds the target), checked, re-created by
    /// the export, and removed by the discard like any untracked file.
    #[cfg(unix)]
    #[test]
    fn links_are_recorded_as_links_dangling_or_not() {
        use std::os::unix::fs::symlink;
        let d = tempfile::tempdir().unwrap();
        let g = GitCmd::new("git", d.path().join("hooks")).unwrap();
        let r = repo(
            d.path(),
            &g,
            &[
                ("a.txt", "a\n"),
                ("docs/x.md", "x\n"),
                (".gitignore", "toryo.local.yaml\n"),
            ],
        );
        symlink("a.txt", r.join("tracked-link")).unwrap();
        commit(&g, &r);
        let wt = Worktree::new(&g, &r);
        std::fs::create_dir_all(r.join("src")).unwrap();
        symlink("/nonexistent/build-output", r.join("src").join("out")).unwrap();
        symlink("docs", r.join("docs-link")).unwrap();
        symlink("docs", r.join(".claude")).unwrap();
        symlink("a.txt", r.join("toryo.local.yaml")).unwrap();
        std::fs::remove_file(r.join("tracked-link")).unwrap();
        symlink("docs/x.md", r.join("tracked-link")).unwrap();
        let protect = Protected::new(vec![], &[]);
        let (dir, m) = take(&wt, &protect, &d.path().join("snaps"), "T-1", "a").unwrap();
        for (path, kind, target) in [
            ("src/out", "untracked", "/nonexistent/build-output"),
            ("docs-link", "untracked", "docs"),
            (".claude", "untracked", "docs"),
            ("tracked-link", "unstaged", "docs/x.md"),
            ("toryo.local.yaml", "ignored_protected", "a.txt"),
        ] {
            let e = entry(&m, path);
            assert_eq!(
                (e.kind.as_str(), e.file_type.as_deref(), e.link.as_deref()),
                (kind, Some(SYMLINK), Some(target)),
                "{path}"
            );
            assert!(e.has_copy());
            assert_eq!(
                std::fs::read_to_string(dir.join(e.subdir()).join(path)).unwrap(),
                target
            );
        }
        assert_eq!(completion_check(&wt, &dir).unwrap(), m);
        // a changed copy of a link fails the check, like any other copy
        let copy = dir.join("files").join("src").join("out");
        std::fs::write(&copy, "/tmp/elsewhere").unwrap();
        let e = completion_check(&wt, &dir).unwrap_err().to_string();
        assert!(
            e.contains("snapshot_incomplete") && e.contains("src/out"),
            "{e}"
        );
        std::fs::write(&copy, "/nonexistent/build-output").unwrap();
        let into = d.path().join("export");
        export(&wt, &dir, &into).unwrap();
        for (path, target) in [
            ("src/out", "/nonexistent/build-output"),
            ("docs-link", "docs"),
            (".claude", "docs"),
            ("tracked-link", "docs/x.md"),
            ("toryo.local.yaml", "a.txt"),
        ] {
            assert_eq!(
                std::fs::read_link(into.join(path)).unwrap(),
                PathBuf::from(target),
                "{path}"
            );
        }
        wt.hard_reset_and_clean("HEAD").unwrap();
        for gone in ["src/out", "docs-link", ".claude"] {
            assert!(std::fs::symlink_metadata(r.join(gone)).is_err(), "{gone}");
        }
        assert_eq!(
            std::fs::read_link(r.join("tracked-link")).unwrap(),
            PathBuf::from("a.txt")
        );
    }

    /// A FIFO is recorded without being opened (reading one blocks); a directory Git did not
    /// list is walked; a path behind a link is gone as far as Git is concerned.
    #[cfg(unix)]
    #[test]
    fn special_files_directories_and_paths_behind_links() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("wt");
        std::fs::create_dir_all(root.join("dir")).unwrap();
        std::fs::write(root.join("dir").join("f.txt"), "f").unwrap();
        let fifo = std::process::Command::new("mkfifo")
            .arg(root.join("dir").join("pipe"))
            .status()
            .unwrap();
        assert!(fifo.success());
        std::os::unix::fs::symlink("dir", root.join("via-link")).unwrap();
        let dst = d.path().join("files");
        let mut out = Vec::new();
        capture(&root, "dir/", "untracked", &dst, &mut out).unwrap();
        capture(&root, "via-link/f.txt", "unstaged", &dst, &mut out).unwrap();
        capture(&root, "missing.txt", "unstaged", &dst, &mut out).unwrap();
        let got: Vec<(&str, Option<&str>, bool)> = out
            .iter()
            .map(|e| (e.path.as_str(), e.file_type.as_deref(), e.has_copy()))
            .collect();
        assert_eq!(
            got,
            [
                ("dir/f.txt", None, true),
                ("dir/pipe", Some(SPECIAL), false),
                ("via-link/f.txt", None, false),
                ("missing.txt", None, false),
            ]
        );
        assert_eq!(std::fs::read(dst.join("dir/f.txt")).unwrap(), b"f");
        assert!(
            !dst.join("via-link").exists(),
            "nothing read through the link"
        );
    }
}
