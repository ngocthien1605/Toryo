//! Git operations on the task worktree (plan §8) through `GitCmd`: HEAD, changed paths,
//! checkpoint commits with trailers, and the trailer search that makes checkpointing and
//! recovery idempotent (§10: commits are keyed by `(task, stage, attempt)`).

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::git::{parse_status_z, GitCmd, Loc};

pub const T_TASK: &str = "Toryo-Task";
pub const T_STAGE: &str = "Toryo-Stage";
pub const T_ATTEMPT: &str = "Toryo-Attempt";
/// The agent profile that wrote a checkpoint (R49; M3 wrote `Toryo-Implementer`).
pub const T_AGENT: &str = "Toryo-Agent";
pub const T_NO_CHANGE: &str = "Toryo-No-Change";
pub const T_PARTIAL: &str = "Toryo-Partial";

pub struct Worktree<'a> {
    pub g: &'a GitCmd,
    pub path: PathBuf,
    pub loc: Loc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointCommit {
    pub sha: String,
    pub partial: bool,
    pub no_change: bool,
}

impl<'a> Worktree<'a> {
    pub fn new(g: &'a GitCmd, path: &Path) -> Self {
        Self {
            g,
            path: path.to_path_buf(),
            loc: Loc::Discover(path.to_path_buf()),
        }
    }

    pub fn head(&self) -> Result<String> {
        self.g.run_str(&self.loc, true, &["rev-parse", "HEAD"])
    }

    /// Every changed path (tracked and untracked, not ignored): `(status, path)`. Rename
    /// detection is off (`--no-renames`), so a rename is the deletion of its source plus the
    /// addition of its destination: both paths reach the path policy with their own status,
    /// and repository config (`status.renames`) cannot reshape the list (R37; EC-26).
    pub fn changed(&self) -> Result<Vec<(String, String)>> {
        Ok(parse_status_z(&self.g.run(
            &self.loc,
            true,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--no-renames",
            ],
        )?))
    }

    pub fn is_clean(&self) -> Result<bool> {
        Ok(self.changed()?.is_empty())
    }

    /// The paths that differ between the commits `from` and `to`: `(status, path)`, the
    /// status a `diff --name-status` letter (`A`, `M`, `D`, `T`). Rename detection is off, as
    /// in [`Worktree::changed`].
    pub fn changed_between(&self, from: &str, to: &str) -> Result<Vec<(String, String)>> {
        let out = self.g.run(
            &self.loc,
            true,
            &[
                "diff",
                "--name-status",
                "-z",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
                from,
                to,
            ],
        )?;
        let mut fields = out
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).to_string());
        let mut v = Vec::new();
        while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
            v.push((status, path));
        }
        Ok(v)
    }

    /// Put protected paths back (tracked → HEAD, new untracked → removed; a link or
    /// junction is removed as a link, never followed). A staged path is unstaged first: the
    /// HEAD guard's soft reset leaves an agent's committed files staged, and a staged new
    /// file would otherwise count as tracked and stay (EC-18).
    pub fn restore(&self, paths: &[String]) -> Result<()> {
        for p in paths {
            let _ = self.g.output(&self.loc, false, &["reset", "-q", "--", p]);
            let _ = self
                .g
                .output(&self.loc, false, &["checkout", "-q", "HEAD", "--", p]);
            if self
                .g
                .output(&self.loc, true, &["ls-files", "--error-unmatch", p])?
                .status
                .success()
            {
                continue;
            }
            let f = self.path.join(p);
            if std::fs::remove_file(&f).is_err()
                && std::fs::symlink_metadata(&f).is_ok_and(|m| m.file_type().is_symlink())
            {
                // a directory symlink or junction on Windows is removed as a directory
                let _ = std::fs::remove_dir(&f);
            }
        }
        Ok(())
    }

    /// `git add -A` + commit with trailers (R40: `allow_empty` for a no-change checkpoint).
    pub fn commit(
        &self,
        message: &str,
        trailers: &[(&str, String)],
        allow_empty: bool,
    ) -> Result<String> {
        self.g.run(&self.loc, false, &["add", "-A"])?;
        let mut args: Vec<String> = vec![
            "commit".into(),
            "-q".into(),
            "--no-verify".into(),
            "-m".into(),
            message.into(),
        ];
        for (k, v) in trailers {
            args.push("--trailer".into());
            args.push(format!("{k}: {v}"));
        }
        if allow_empty {
            args.push("--allow-empty".into());
        }
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        self.g.run(&self.loc, false, &a)?;
        self.head()
    }

    /// Nothing staged after `git add -A`?
    pub fn nothing_to_commit(&self) -> Result<bool> {
        self.g.run(&self.loc, false, &["add", "-A"])?;
        Ok(self
            .g
            .output(&self.loc, true, &["diff", "--cached", "--quiet"])?
            .status
            .success())
    }

    /// The checkpoint commit of `(task, stage, attempt)` on this branch since `base`, if any.
    pub fn find_checkpoint(
        &self,
        base: &str,
        task: &str,
        stage: &str,
        attempt: u32,
    ) -> Result<Option<CheckpointCommit>> {
        let fmt = format!(
            "--format=%H%x1f%(trailers:key={T_TASK},valueonly)%x1f%(trailers:key={T_STAGE},valueonly)%x1f%(trailers:key={T_ATTEMPT},valueonly)%x1f%(trailers:key={T_PARTIAL},valueonly)%x1f%(trailers:key={T_NO_CHANGE},valueonly)%x1e"
        );
        let range = format!("{base}..HEAD");
        let out = self.g.run_str(&self.loc, true, &["log", &fmt, &range])?;
        for rec in out.split('\x1e') {
            let f: Vec<&str> = rec.split('\x1f').map(str::trim).collect();
            if f.len() < 6 || f[0].is_empty() {
                continue;
            }
            if f[1] == task && f[2] == stage && f[3] == attempt.to_string() {
                return Ok(Some(CheckpointCommit {
                    sha: f[0].to_string(),
                    partial: f[4] == "true",
                    no_change: f[5] == "true",
                }));
            }
        }
        Ok(None)
    }

    /// Is `commit` an ancestor of HEAD (or HEAD itself)?
    pub fn contains(&self, commit: &str) -> Result<bool> {
        Ok(self
            .g
            .output(
                &self.loc,
                true,
                &["merge-base", "--is-ancestor", commit, "HEAD"],
            )?
            .status
            .success())
    }

    /// The worktree's own Git dir (for unfinished-operation markers).
    pub fn git_dir(&self) -> Result<PathBuf> {
        let d = self
            .g
            .run_str(&self.loc, true, &["rev-parse", "--absolute-git-dir"])?;
        Ok(PathBuf::from(d))
    }

    /// R25/§10: an unfinished Git operation (merge, rebase, cherry-pick, revert, bisect).
    pub fn unfinished_operation(&self) -> Result<Option<String>> {
        let gd = self.git_dir()?;
        for m in [
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "BISECT_LOG",
            "rebase-merge",
            "rebase-apply",
        ] {
            if gd.join(m).exists() {
                return Ok(Some(m.to_string()));
            }
        }
        Ok(None)
    }

    /// The ref HEAD points at (`refs/heads/…`); `None` when HEAD is detached (§8 HEAD
    /// guard, R12: the agent must stay on the task branch).
    pub fn head_ref(&self) -> Result<Option<String>> {
        let o = self
            .g
            .output(&self.loc, true, &["symbolic-ref", "-q", "HEAD"])?;
        Ok(o.status
            .success()
            .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string()))
    }

    /// Paths with unresolved conflicts in the index (R25): a conflicting `git stash pop`
    /// or `checkout -m` leaves them without any operation marker.
    pub fn unmerged_paths(&self) -> Result<Vec<String>> {
        let out = self.g.run(
            &self.loc,
            true,
            &["diff", "--name-only", "-z", "--diff-filter=U"],
        )?;
        Ok(out
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).to_string())
            .collect())
    }

    /// Soft-reset HEAD to `to` (HEAD guard: agents must not commit).
    pub fn soft_reset(&self, to: &str) -> Result<()> {
        self.g
            .run(&self.loc, false, &["reset", "-q", "--soft", to])?;
        Ok(())
    }

    pub fn hard_reset_and_clean(&self, to: &str) -> Result<()> {
        self.g
            .run(&self.loc, false, &["reset", "-q", "--hard", to])?;
        self.g.run(&self.loc, false, &["clean", "-q", "-fd"])?;
        Ok(())
    }
}

/// Worktree-relative path with forward slashes, or an error if outside.
pub fn rel(root: &Path, p: &Path) -> Result<String> {
    match p.strip_prefix(root) {
        Ok(r) => Ok(r.display().to_string().replace('\\', "/")),
        Err(_) => bail!("{} is outside {}", p.display(), root.display()),
    }
}
