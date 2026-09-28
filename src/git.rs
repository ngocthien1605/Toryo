//! `GitCmd` — the only way the orchestrator runs Git (plan §8, R33, R37).
//!
//! Every call gets command-line overrides that stop repository configuration
//! from running programs: hooks (empty `core.hooksPath`), fsmonitor, signing,
//! external diff / textconv, pager, submodule recursion, and every filter driver
//! (trusted command from the machine config, or empty). Diffs that are stored or
//! compared use [`CANON_DIFF`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use anyhow::{bail, Context, Result};

use crate::config::TrustedFilter;
use crate::process::env::build_env;

/// `<CANON_DIFF>` (plan §8).
pub const CANON_DIFF: &[&str] = &[
    "--binary",
    "--full-index",
    "--no-ext-diff",
    "--no-textconv",
    "--no-color",
    "--no-renames",
    "--src-prefix=a/",
    "--dst-prefix=b/",
];

/// Where a command runs: discover from a directory, or explicit git-dir + work-tree
/// (mandatory for the verify checkout, whose `.git` pointer file is Low-writable).
#[derive(Debug, Clone)]
pub enum Loc {
    Discover(PathBuf),
    Explicit {
        git_dir: PathBuf,
        work_tree: PathBuf,
    },
}

/// Per-driver override values for one GitCmd.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FilterPlan {
    /// driver name → trusted filter (None = neutralised with empty commands)
    pub drivers: BTreeMap<String, Option<TrustedFilter>>,
    /// referenced by the tree but not trusted (admission fails unless neutralisation is on)
    pub untrusted_referenced: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct GitCmd {
    pub git: PathBuf,
    pub empty_hooks: PathBuf,
    pub filters: FilterPlan,
}

impl GitCmd {
    pub fn new(git: impl Into<PathBuf>, empty_hooks: impl Into<PathBuf>) -> Result<Self> {
        let empty_hooks = empty_hooks.into();
        std::fs::create_dir_all(&empty_hooks)
            .with_context(|| format!("creating empty hooks dir {}", empty_hooks.display()))?;
        if std::fs::read_dir(&empty_hooks)?.next().is_some() {
            bail!(
                "empty-hooks directory {} is not empty (R33)",
                empty_hooks.display()
            );
        }
        Ok(Self {
            git: git.into(),
            empty_hooks,
            filters: FilterPlan::default(),
        })
    }

    /// The hardening prefix (everything before the subcommand).
    pub fn prefix(&self, read_only: bool) -> Vec<String> {
        let mut a: Vec<String> = vec!["--no-pager".into()];
        let mut c = |k: &str, v: &str| {
            a.push("-c".into());
            a.push(format!("{k}={v}"));
        };
        c(
            "core.hooksPath",
            &self.empty_hooks.to_string_lossy().replace('\\', "/"),
        );
        c("core.fsmonitor", "false");
        c("commit.gpgSign", "false");
        c("tag.gpgSign", "false");
        c("user.name", "toryo");
        c("user.email", "toryo@local.invalid");
        c("submodule.recurse", "false");
        c("diff.external", "");
        c("color.ui", "never");
        c("diff.noprefix", "false");
        c("diff.mnemonicPrefix", "false");
        c("diff.relative", "false");
        c("core.quotePath", "false");
        for (name, t) in &self.filters.drivers {
            let (p, cl, sm, req) = match t {
                Some(t) => (
                    t.process.clone().unwrap_or_default(),
                    t.clean.clone().unwrap_or_default(),
                    t.smudge.clone().unwrap_or_default(),
                    t.required,
                ),
                None => (String::new(), String::new(), String::new(), false),
            };
            c(&format!("filter.{name}.process"), &p);
            c(&format!("filter.{name}.clean"), &cl);
            c(&format!("filter.{name}.smudge"), &sm);
            c(
                &format!("filter.{name}.required"),
                if req { "true" } else { "false" },
            );
        }
        if read_only {
            a.push("--no-optional-locks".into());
        }
        a
    }

    fn command(&self, loc: &Loc, read_only: bool) -> Command {
        let mut cmd = Command::new(&self.git);
        let mut env = build_env(std::env::vars(), &BTreeMap::new(), &BTreeMap::new());
        env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
        if read_only {
            env.insert("GIT_OPTIONAL_LOCKS".into(), "0".into());
        }
        cmd.env_clear().envs(env).stdin(Stdio::null());
        let mut args = self.prefix(read_only);
        match loc {
            Loc::Discover(dir) => {
                cmd.current_dir(dir);
            }
            Loc::Explicit { git_dir, work_tree } => {
                args.push(format!("--git-dir={}", git_dir.display()));
                args.push(format!("--work-tree={}", work_tree.display()));
                cmd.current_dir(work_tree);
            }
        }
        cmd.args(args);
        cmd
    }

    /// Run and return raw output (no status check).
    pub fn output(&self, loc: &Loc, read_only: bool, args: &[&str]) -> Result<Output> {
        let mut cmd = self.command(loc, read_only);
        cmd.args(args);
        cmd.output()
            .with_context(|| format!("running git {}", args.join(" ")))
    }

    /// Like [`GitCmd::output`] with extra environment (e.g. `GIT_INDEX_FILE` for a scratch
    /// index) and optional stdin.
    pub fn output_env(&self, loc: &Loc, read_only: bool, args: &[&str], env: &[(&str, &Path)]) -> Result<Output> {
        let mut cmd = self.command(loc, read_only);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.args(args);
        cmd.output()
            .with_context(|| format!("running git {}", args.join(" ")))
    }

    /// Run, require exit 0, return stdout bytes.
    pub fn run(&self, loc: &Loc, read_only: bool, args: &[&str]) -> Result<Vec<u8>> {
        let out = self.output(loc, read_only, args)?;
        if !out.status.success() {
            bail!(
                "git {} failed ({}): {}",
                args.join(" "),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(out.stdout)
    }

    pub fn run_str(&self, loc: &Loc, read_only: bool, args: &[&str]) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.run(loc, read_only, args)?)
            .trim()
            .to_string())
    }

    /// Canonical diff `a..b` (or `a` vs worktree when `b` is None).
    pub fn canonical_diff(&self, loc: &Loc, a: &str, b: Option<&str>) -> Result<Vec<u8>> {
        let mut args: Vec<&str> = vec!["diff"];
        args.extend_from_slice(CANON_DIFF);
        let range;
        match b {
            Some(b) => {
                range = format!("{a}..{b}");
                args.push(&range);
            }
            None => args.push(a),
        }
        self.run(loc, true, &args)
    }

    /// R37 admission: collect driver names (defined in any config, or referenced by the
    /// tree) and resolve each against the trusted table.
    pub fn plan_filters(
        &self,
        repo: &Path,
        trusted: &BTreeMap<String, TrustedFilter>,
        neutralize_untrusted: bool,
    ) -> Result<FilterPlan> {
        let loc = Loc::Discover(repo.to_path_buf());
        // Plan with an empty override set first: listing config/attributes runs no filters.
        let defined = defined_filter_names(self, &loc)?;
        let referenced = referenced_filter_names(self, &loc)?;
        let mut plan = FilterPlan::default();
        for name in defined.union(&referenced) {
            match trusted.get(name) {
                Some(t) if t.process.as_deref().unwrap_or("").is_empty() => {
                    // M0 finding (Git convert.c): a non-NULL `process` wins over clean/smudge, and an
                    // empty one disables the driver. `-c` cannot unset a key, so the only way to
                    // override a repo-configured `process` is a trusted `process` of our own.
                    bail!("trusted filter `{name}` must define `process` (clean/smudge-only trusted drivers cannot be enforced, R37)");
                }
                Some(t) => {
                    plan.drivers.insert(name.clone(), Some(t.clone()));
                }
                None => {
                    plan.drivers.insert(name.clone(), None);
                    if referenced.contains(name) {
                        plan.untrusted_referenced.push(name.clone());
                    }
                }
            }
        }
        if !plan.untrusted_referenced.is_empty() && !neutralize_untrusted {
            bail!(
                "untrusted_git_filter: {:?} referenced by .gitattributes but not in the machine's git.trusted_filters (R37)",
                plan.untrusted_referenced
            );
        }
        Ok(plan)
    }

    /// Refuse repositories with submodules unless allowed (R37).
    pub fn check_submodules(repo: &Path, allow: bool) -> Result<()> {
        if !allow && repo.join(".gitmodules").exists() {
            bail!("repository has .gitmodules; submodules are refused in v0.1 (policy.allow_submodules)");
        }
        Ok(())
    }
}

/// Names from `filter.<name>.<var>` keys in every config scope.
/// The Git executable: `TORYO_GIT`, else `git` from `PATH`.
pub fn git_exe() -> PathBuf {
    std::env::var_os("TORYO_GIT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("git"))
}

/// `GitCmd` with the data dir's empty hooks directory (R33).
pub fn default_gitcmd() -> Result<GitCmd> {
    GitCmd::new(git_exe(), crate::config::data_dir().join("empty-hooks"))
}

pub fn defined_filter_names(g: &GitCmd, loc: &Loc) -> Result<BTreeSet<String>> {
    let out = g.output(loc, true, &["config", "-z", "--get-regexp", r"^filter\."])?;
    // exit 1 = no matching key
    let mut names = BTreeSet::new();
    for entry in out.stdout.split(|b| *b == 0) {
        let key = String::from_utf8_lossy(entry.split(|b| *b == b'\n').next().unwrap_or_default())
            .to_string();
        if let Some(rest) = key.strip_prefix("filter.") {
            if let Some(dot) = rest.rfind('.') {
                names.insert(rest[..dot].to_string());
            }
        }
    }
    Ok(names)
}

/// Driver names that `filter` attributes select for tracked or untracked-not-ignored files.
pub fn referenced_filter_names(g: &GitCmd, loc: &Loc) -> Result<BTreeSet<String>> {
    let files = g.run(
        loc,
        true,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    let mut names = BTreeSet::new();
    if files.is_empty() {
        return Ok(names);
    }
    let mut cmd = g.command(loc, true);
    cmd.args(["check-attr", "--stdin", "-z", "filter"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = cmd.spawn().context("git check-attr")?;
    // Feed stdin from a thread: writing everything before reading deadlocks once the
    // output pipe fills (large repositories).
    let mut stdin = child.stdin.take().expect("stdin");
    let writer = std::thread::spawn(move || {
        use std::io::Write;
        let r = stdin.write_all(&files);
        drop(stdin);
        r
    });
    let out = child.wait_with_output()?;
    writer
        .join()
        .map_err(|_| anyhow::anyhow!("check-attr writer panicked"))??;
    let fields: Vec<&[u8]> = out.stdout.split(|b| *b == 0).collect();
    for triple in fields.chunks(3) {
        if triple.len() == 3 {
            let info = String::from_utf8_lossy(triple[2]);
            if !matches!(info.as_ref(), "unspecified" | "unset" | "set" | "") {
                names.insert(info.to_string());
            }
        }
    }
    Ok(names)
}

/// Parse `git status --porcelain=v1 -z` into (XY, path) pairs.
pub fn parse_status_z(out: &[u8]) -> Vec<(String, String)> {
    let mut v = Vec::new();
    let mut it = out.split(|b| *b == 0).filter(|s| !s.is_empty());
    while let Some(entry) = it.next() {
        if entry.len() < 4 {
            continue;
        }
        let xy = String::from_utf8_lossy(&entry[..2]).to_string();
        let path = String::from_utf8_lossy(&entry[3..]).to_string();
        if xy.starts_with('R') || xy.starts_with('C') {
            it.next(); // origin path follows
        }
        v.push((xy, path));
    }
    v
}

pub mod canary;
pub mod filter_process;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_z_parsing_handles_spaces_and_renames() {
        let raw = b" M a b.txt\0?? new file\0R  new.txt\0old.txt\0";
        let p = parse_status_z(raw);
        assert_eq!(
            p,
            vec![
                (" M".into(), "a b.txt".into()),
                ("??".into(), "new file".into()),
                ("R ".into(), "new.txt".into()),
            ]
        );
    }

    #[test]
    fn prefix_contains_every_neutraliser() {
        let d = tempfile::tempdir().unwrap();
        let mut g = GitCmd::new("git", d.path().join("empty-hooks")).unwrap();
        g.filters.drivers.insert("evil".into(), None);
        let p = g.prefix(true).join(" ");
        for needle in [
            "--no-pager",
            "core.fsmonitor=false",
            "commit.gpgSign=false",
            "diff.external=",
            "submodule.recurse=false",
            "filter.evil.process=",
            "filter.evil.clean=",
            "filter.evil.required=false",
            "--no-optional-locks",
            "color.ui=never",
        ] {
            assert!(p.contains(needle), "missing {needle}");
        }
    }

    #[test]
    fn empty_hooks_dir_must_be_empty() {
        let d = tempfile::tempdir().unwrap();
        let h = d.path().join("h");
        std::fs::create_dir_all(&h).unwrap();
        std::fs::write(h.join("pre-commit"), "x").unwrap();
        assert!(GitCmd::new("git", &h).is_err());
    }
}

#[cfg(test)]
mod big_repo_tests {
    use super::*;

    /// Regression: `referenced_filter_names` used to deadlock on a few thousand files.
    #[test]
    fn filter_scan_does_not_deadlock_on_large_repos() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("r");
        std::fs::create_dir_all(&repo).unwrap();
        let g = GitCmd::new("git", d.path().join("hooks")).unwrap();
        let loc = Loc::Discover(repo.clone());
        g.run(&loc, false, &["init", "-q"]).unwrap();
        for i in 0..6000 {
            std::fs::write(
                repo.join(format!("file-with-a-longish-name-{i:05}.txt")),
                "x",
            )
            .unwrap();
        }
        std::fs::write(repo.join(".gitattributes"), "*.txt filter=lfs\n").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(referenced_filter_names(&g, &loc).map(|s| s.len()));
        });
        let n = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("deadlock")
            .unwrap();
        assert_eq!(n, 1);
    }
}
