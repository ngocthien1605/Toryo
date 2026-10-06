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
        // M12-13: a D365FO metadata path plus the worktree or the verify root passes 260
        // characters; without this Git refuses ("Filename too long") on every checkout
        if cfg!(windows) {
            c("core.longpaths", "true");
        }
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
        // M13-R1 (X30): Toryo's Git never looks through a replace ref. Every diff-based check
        // (found commit, anchor, path policy) is only as good as what Git reports; a replaced
        // commit would report another tree than the one the export deploys.
        env.insert("GIT_NO_REPLACE_OBJECTS".into(), "1".into());
        if read_only {
            env.insert("GIT_OPTIONAL_LOCKS".into(), "0".into());
        }
        cmd.env_clear().envs(env).stdin(Stdio::null());
        // M13-R6: Git never needs the console. Ctrl+C goes to every process of the console
        // (Windows) or of the foreground process group (Unix): a Git call in flight would die
        // of it, and the engine would see a Git error instead of the stop M13-12 promises
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(GIT_CREATION_FLAGS);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
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
    pub fn output_env(
        &self,
        loc: &Loc,
        read_only: bool,
        args: &[&str],
        env: &[(&str, &Path)],
    ) -> Result<Output> {
        let mut cmd = self.command(loc, read_only);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.args(args);
        cmd.output()
            .with_context(|| format!("running git {}", args.join(" ")))
    }

    /// Start `git` with pipes on stdin, stdout and stderr, for a caller that drives a
    /// long-running command itself (`cat-file --batch`, M11-R20); extra environment as in
    /// [`GitCmd::output_env`].
    pub fn spawn_piped(
        &self,
        loc: &Loc,
        read_only: bool,
        args: &[&str],
        env: &[(&str, &Path)],
    ) -> Result<std::process::Child> {
        let mut cmd = self.command(loc, read_only);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.spawn()
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
    /// tree) and resolve each against the trusted table. Config files the repository's own
    /// config reaches through `include` / `includeIf` count whatever their condition (EC-21):
    /// the drivers they define are planned (trusted command or neutralised), and a
    /// `core.attributesFile` they set is checked against the tree like the configured one.
    /// Includes in system and global config are the user's own, trusted configuration (R37)
    /// and count only as Git evaluates them.
    pub fn plan_filters(
        &self,
        repo: &Path,
        trusted: &BTreeMap<String, TrustedFilter>,
        neutralize_untrusted: bool,
    ) -> Result<FilterPlan> {
        let loc = Loc::Discover(repo.to_path_buf());
        // Plan with an empty override set first: listing config/attributes runs no filters.
        let mut defined = defined_filter_names(self, &loc)?;
        let mut referenced = referenced_filter_names(self, &loc)?;
        for f in included_config_files(self, &loc)? {
            let fs = f.display().to_string();
            let out = self.output(&loc, true, &["config", "-z", "--file", &fs, "--list"])?;
            let entries = config_entries(&out.stdout);
            defined.extend(filter_names(&entries));
            for (_, attrs) in entries.iter().filter(|(k, _)| k == "core.attributesfile") {
                referenced.extend(referenced_with(self, &loc, Some(attrs))?);
            }
        }
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

/// Names from `filter.<name>.<var>` keys in every config scope.
pub fn defined_filter_names(g: &GitCmd, loc: &Loc) -> Result<BTreeSet<String>> {
    let out = g.output(loc, true, &["config", "-z", "--get-regexp", r"^filter\."])?;
    // exit 1 = no matching key
    Ok(filter_names(&config_entries(&out.stdout)))
}

/// `git config -z` output (`key\nvalue\0` …) as `(key, value)` pairs; keys come canonical
/// (section and variable lower-case).
fn config_entries(z: &[u8]) -> Vec<(String, String)> {
    z.split(|b| *b == 0)
        .filter(|e| !e.is_empty())
        .map(|e| {
            let s = String::from_utf8_lossy(e);
            match s.split_once('\n') {
                Some((k, v)) => (k.to_string(), v.to_string()),
                None => (s.to_string(), String::new()),
            }
        })
        .collect()
}

fn filter_names(entries: &[(String, String)]) -> BTreeSet<String> {
    entries
        .iter()
        .filter_map(|(k, _)| k.strip_prefix("filter."))
        .filter_map(|rest| rest.rfind('.').map(|dot| rest[..dot].to_string()))
        .collect()
}

/// `include.path` and `includeIf.<condition>.path` (canonical key form).
const INCLUDE_KEYS: &str = r"^include(if\..*)?\.path$";
/// Git's own limit on nested includes.
const MAX_INCLUDE_DEPTH: usize = 10;
/// The config scopes (`git config --show-scope`) that belong to the repository: its
/// `config` and, with `extensions.worktreeConfig`, `config.worktree` — untrusted, data only
/// (R37). `system` and `global` are the user's machine (trusted); `command` is toryo's own
/// `-c` overrides.
const REPO_SCOPES: [&str; 2] = ["local", "worktree"];

/// Where an include directive in `from` points: relative to the including file's
/// directory, `~/` in the home directory (Git's rules).
fn include_target(from: &Path, value: &str) -> PathBuf {
    let v = value.trim();
    if let Some(rest) = v.strip_prefix("~/").or_else(|| v.strip_prefix("~\\")) {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return PathBuf::from(home).join(rest);
        }
    }
    let p = PathBuf::from(v);
    if p.is_absolute() {
        p
    } else {
        from.parent().map(|d| d.join(&p)).unwrap_or(p)
    }
}

/// Every existing config file reachable through `include.path` / `includeIf.*.path` from
/// the repository's own config ([`REPO_SCOPES`]), **whatever the condition** (R37:
/// repository config and "anything they include/includeIf" is untrusted; EC-21). A
/// condition such as `onbranch:toryo/**` or `gitdir:…` is false where admission runs and
/// true in the task worktree or the verify checkout, so it is never evaluated here — every
/// target counts. System and global config are trusted (R37): an inactive per-client
/// include in `~/.gitconfig` is not repository data and never fails admission.
pub fn included_config_files(g: &GitCmd, loc: &Loc) -> Result<Vec<PathBuf>> {
    let cwd = match loc {
        Loc::Discover(d) => d.clone(),
        Loc::Explicit { work_tree, .. } => work_tree.clone(),
    };
    let args = [
        "config",
        "-z",
        "--show-scope",
        "--show-origin",
        "--get-regexp",
        INCLUDE_KEYS,
    ];
    let out = g.output(loc, true, &args)?;
    // exit 1 = no directive; any other failure must not read as "no include"
    if !out.status.success() && out.status.code() != Some(1) {
        bail!(
            "git {} failed ({}): {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    // with --show-scope --show-origin: `<scope>\0file:<path>\0key\nvalue\0` per directive;
    // one in a file an active include reached carries the scope of the including file
    let fields: Vec<&[u8]> = out.stdout.split(|b| *b == 0).collect();
    let mut todo: Vec<(PathBuf, usize)> = Vec::new();
    for triple in fields.chunks(3) {
        let [scope, origin, kv] = triple else {
            continue;
        };
        let scope = String::from_utf8_lossy(scope);
        if !REPO_SCOPES.contains(&&*scope) {
            continue; // system/global: trusted (R37); command line: toryo's own overrides
        }
        let origin = String::from_utf8_lossy(origin);
        let Some(file) = origin.strip_prefix("file:") else {
            continue;
        };
        let from = cwd.join(file); // a relative origin is relative to Git's cwd
        for (_, v) in config_entries(kv) {
            todo.push((include_target(&from, &v), 1));
        }
    }
    let mut seen = BTreeSet::new();
    let mut files = Vec::new();
    while let Some((p, depth)) = todo.pop() {
        if depth > MAX_INCLUDE_DEPTH || !p.is_file() {
            continue;
        }
        // each file once, however it is spelled (include cycles)
        if !seen.insert(std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone())) {
            continue;
        }
        let ps = p.display().to_string();
        let nested = g.output(
            loc,
            true,
            &["config", "-z", "--file", &ps, "--get-regexp", INCLUDE_KEYS],
        )?;
        for (_, v) in config_entries(&nested.stdout) {
            todo.push((include_target(&p, &v), depth + 1));
        }
        files.push(p);
    }
    Ok(files)
}

/// Driver names that `filter` attributes select for tracked or untracked-not-ignored files.
pub fn referenced_filter_names(g: &GitCmd, loc: &Loc) -> Result<BTreeSet<String>> {
    referenced_with(g, loc, None)
}

/// [`referenced_filter_names`], with `core.attributesFile` set to `attributes_file` (one
/// named by an included config file) instead of the configured one.
fn referenced_with(
    g: &GitCmd,
    loc: &Loc,
    attributes_file: Option<&str>,
) -> Result<BTreeSet<String>> {
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
    if let Some(f) = attributes_file {
        // Git expands `~/` and resolves a relative path itself, as for the configured value
        cmd.arg("-c").arg(format!("core.attributesFile={f}"));
    }
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

/// The first 12 characters of a SHA or digest, for display. Never panics on short input.
pub fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// Parse `git status --porcelain=v1 -z` into (XY, path) pairs.
///
/// A rename or copy — `R`/`C` in the index (X) **or** the worktree (Y) column — is followed
/// by its source path. Both paths are returned, the destination first, then the source with
/// the same XY: a path policy must see the deny-listed or protected file a rename took away
/// (EC-26), and a worktree rename (` R`, e.g. after `git add -N`) is never misread as an
/// entry of its own. A listed path need not exist: a rename's source is usually gone.
pub fn parse_status_z(out: &[u8]) -> Vec<(String, String)> {
    let mut v = Vec::new();
    let mut it = out.split(|b| *b == 0).filter(|s| !s.is_empty());
    while let Some(entry) = it.next() {
        if entry.len() < 4 {
            continue;
        }
        let xy = String::from_utf8_lossy(&entry[..2]).to_string();
        let path = String::from_utf8_lossy(&entry[3..]).to_string();
        let source = xy
            .chars()
            .any(|c| c == 'R' || c == 'C')
            .then(|| it.next())
            .flatten()
            .map(|s| String::from_utf8_lossy(s).to_string());
        v.push((xy.clone(), path));
        if let Some(s) = source {
            v.push((xy, s));
        }
    }
    v
}

/// M13-R6 (Windows): `CREATE_NO_WINDOW` — the Git child gets a console of its own (hidden), so a
/// Ctrl+C aimed at Toryo's console does not reach it. (On Unix the child is in a process group
/// of its own: `process_group(0)`.)
#[cfg(windows)]
pub(crate) const GIT_CREATION_FLAGS: u32 = 0x0800_0000;

pub mod canary;
pub mod filter_process;

#[cfg(test)]
mod tests {
    #[test]
    fn short_never_panics() {
        assert_eq!(super::short("0123456789abcdef"), "0123456789ab");
        assert_eq!(super::short("abc"), "abc");
        assert_eq!(super::short(""), "");
    }

    use super::*;

    /// M13-R6: the Git children Toryo starts do not share its console (Windows) or its process
    /// group (Unix), so Ctrl+C on `toryo run` never reaches a Git call in flight.
    #[cfg(windows)]
    #[test]
    fn toryos_git_children_do_not_share_the_console() {
        // the flag is `CREATE_NO_WINDOW` of the Win32 API
        assert_eq!(super::GIT_CREATION_FLAGS, 0x0800_0000);
        // and it is applied: a Git call through `GitCmd` still works with it
        let d = tempfile::tempdir().unwrap();
        let g = GitCmd::new("git", d.path().join("hooks")).unwrap();
        let loc = Loc::Discover(d.path().to_path_buf());
        g.run(&loc, false, &["init", "-q", "-b", "main"]).unwrap();
        assert!(d.path().join(".git").exists());
    }

    #[cfg(unix)]
    #[test]
    fn toryos_git_children_do_not_share_the_console() {
        let d = tempfile::tempdir().unwrap();
        let g = GitCmd::new("git", d.path().join("hooks")).unwrap();
        let loc = Loc::Discover(d.path().to_path_buf());
        g.run(&loc, false, &["init", "-q", "-b", "main"]).unwrap();
        let pgid = |pid: &str| {
            let o = Command::new("ps")
                .args(["-o", "pgid=", "-p", pid])
                .output()
                .unwrap();
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        let ours = pgid(&std::process::id().to_string());
        // an alias runs in a shell that is Git's child: it has Git's process group
        let theirs = g
            .run_str(&loc, true, &["-c", "alias.pg=!ps -o pgid= -p $$", "pg"])
            .unwrap();
        assert!(!ours.is_empty() && !theirs.trim().is_empty());
        assert_ne!(ours, theirs.trim(), "the Git child is in our process group");
    }

    /// M13-R1 (X30): Toryo's Git never looks through a replace ref. A replacement of a commit
    /// (the anchor, a checkpoint) by one with another tree would blind every diff-based check.
    #[test]
    fn toryos_git_ignores_replace_refs() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path().join("r");
        std::fs::create_dir_all(&r).unwrap();
        let g = GitCmd::new("git", d.path().join("hooks")).unwrap();
        let loc = Loc::Discover(r.clone());
        g.run(&loc, false, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(r.join("a.txt"), "a\n").unwrap();
        g.run(&loc, false, &["add", "-A"]).unwrap();
        g.run(&loc, false, &["commit", "-q", "-m", "one"]).unwrap();
        let one = g.run_str(&loc, true, &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(r.join("b.txt"), "b\n").unwrap();
        g.run(&loc, false, &["add", "-A"]).unwrap();
        g.run(&loc, false, &["commit", "-q", "-m", "two"]).unwrap();
        let two = g.run_str(&loc, true, &["rev-parse", "HEAD"]).unwrap();
        let (one, two) = (one.trim(), two.trim());
        g.run(&loc, false, &["replace", two, one]).unwrap();
        // what a replace-aware Git says: `two` has the tree of `one`
        let seen = Command::new("git")
            .arg("-C")
            .arg(&r)
            .args(["ls-tree", "--name-only", two])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&seen.stdout).trim(), "a.txt");
        // what Toryo says: the tree of `two`
        let tree = g
            .run_str(&loc, true, &["ls-tree", "--name-only", two])
            .unwrap();
        assert_eq!(tree.trim().replace('\r', ""), "a.txt\nb.txt");
        let diff = g
            .run_str(&loc, true, &["diff", "--name-only", one, two])
            .unwrap();
        assert_eq!(diff.trim(), "b.txt");
    }

    #[test]
    fn status_z_parsing_handles_spaces_and_renames() {
        // EC-26: the source of a rename is returned too (this test used to pin it dropped)
        let raw = b" M a b.txt\0?? new file\0R  new.txt\0old.txt\0";
        let p = parse_status_z(raw);
        assert_eq!(
            p,
            vec![
                (" M".into(), "a b.txt".into()),
                ("??".into(), "new file".into()),
                ("R ".into(), "new.txt".into()),
                ("R ".into(), "old.txt".into()),
            ]
        );
    }

    #[test]
    fn status_z_worktree_renames_and_copies_return_both_paths() {
        // ` R` (worktree rename after `git add -N`) used to leave its source to be misread
        // as an entry: `AGENTS.md` → ("AG", "NTS.md")
        let raw = b" R readme.txt\0AGENTS.md\0RM b.xml\0ZZZ/a.xml\0C  copy.txt\0orig.txt\0?? x\0";
        assert_eq!(
            parse_status_z(raw),
            vec![
                (" R".into(), "readme.txt".into()),
                (" R".into(), "AGENTS.md".into()),
                ("RM".into(), "b.xml".into()),
                ("RM".into(), "ZZZ/a.xml".into()),
                ("C ".into(), "copy.txt".into()),
                ("C ".into(), "orig.txt".into()),
                ("??".into(), "x".into()),
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

    /// EC-21: drivers and attributes files reached through `include`/`includeIf` count
    /// whatever the condition — an `onbranch:toryo/**` include is inactive where admission
    /// runs (on main) and active in the task worktree.
    #[test]
    fn plan_filters_sees_conditional_includes() {
        use std::io::Write;
        let d = tempfile::tempdir().unwrap();
        let r = d.path().join("r");
        std::fs::create_dir_all(&r).unwrap();
        let g = GitCmd::new("git", d.path().join("hooks")).unwrap();
        let loc = Loc::Discover(r.clone());
        g.run(&loc, false, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(r.join("a.txt"), "a\n").unwrap();
        g.run(&loc, false, &["add", "-A"]).unwrap();
        g.run(&loc, false, &["commit", "-q", "-m", "init"]).unwrap();
        let attrs = d.path().join("evil.attributes");
        std::fs::write(&attrs, "*.txt filter=evil\n").unwrap();
        let dot = r.join(".git");
        std::fs::write(
            dot.join("evil.inc"),
            format!(
                "[filter \"evil\"]\n\tclean = cat\n[core]\n\tattributesFile = {}\n[includeIf \"gitdir:/nowhere/\"]\n\tpath = deep.inc\n",
                attrs.display().to_string().replace('\\', "/")
            ),
        )
        .unwrap();
        // nested, conditional, and a cycle back to evil.inc
        std::fs::write(
            dot.join("deep.inc"),
            "[filter \"deep\"]\n\tsmudge = cat\n[include]\n\tpath = evil.inc\n",
        )
        .unwrap();
        let mut cfg = std::fs::OpenOptions::new()
            .append(true)
            .open(dot.join("config"))
            .unwrap();
        writeln!(cfg, "[includeIf \"onbranch:toryo/**\"]\n\tpath = evil.inc").unwrap();
        drop(cfg);
        // inactive on main: plain `git config` does not see the driver
        assert!(!defined_filter_names(&g, &loc).unwrap().contains("evil"));
        let files = included_config_files(&g, &loc).unwrap();
        for f in ["evil.inc", "deep.inc"] {
            assert_eq!(
                files.iter().filter(|p| p.ends_with(f)).count(),
                1,
                "{f}: {files:?}"
            );
        }
        let e = g
            .plan_filters(&r, &BTreeMap::new(), false)
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("untrusted_git_filter") && e.contains("evil"),
            "{e}"
        );
        let plan = g.plan_filters(&r, &BTreeMap::new(), true).unwrap();
        assert_eq!(plan.drivers.get("evil"), Some(&None));
        assert_eq!(plan.drivers.get("deep"), Some(&None));
        assert!(plan.untrusted_referenced.contains(&"evil".to_string()));
        let prefix = GitCmd {
            filters: plan,
            ..g.clone()
        }
        .prefix(false)
        .join(" ");
        assert!(prefix.contains("filter.evil.clean= ") && prefix.contains("filter.deep.smudge="));
    }

    #[test]
    fn include_targets_follow_gits_rules() {
        let from = Path::new("/r/.git/config");
        assert_eq!(
            include_target(from, "evil.inc"),
            PathBuf::from("/r/.git/evil.inc")
        );
        assert_eq!(
            include_target(from, "../shared/x.inc"),
            PathBuf::from("/r/.git/../shared/x.inc")
        );
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            assert_eq!(
                include_target(from, "~/work.inc"),
                PathBuf::from(home).join("work.inc")
            );
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
