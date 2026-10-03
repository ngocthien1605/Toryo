//! The verify checkout and its execution boundary (R29, R36, R45, R47, R50; plan §8, §13).
//!
//! - **Where:** `<verify_root>\<task>\<task>.verify`, a detached `git worktree` of the
//!   checkpoint SHA, with `build`, `cache`, `tmp`, `home` beside it. `verify_root` must be a
//!   folder where the account may set a Low label (WRITE_OWNER, R45) — the default
//!   `%LOCALAPPDATA%\toryo-verify` is.
//! - **Per run:** `checkout --detach --force <sha>`, `clean -ffdx` (`verify.checkout: clean`)
//!   or `-ffd` (`tracked`), `tmp`/`home` emptied (and `build`/`cache` in clean mode), then
//!   HEAD = checkpoint and a clean tree are asserted.
//! - **Boundary:** `low_integrity` (default) runs every command with a Low-IL copy of the
//!   user's token: only the Low-labelled checkout and its dirs are writable. `none` is
//!   trusted-code mode and is stated on every result. A boundary that cannot be established
//!   is `verify_boundary_unavailable` — never a silent downgrade.
//! - **R47:** the checkout's `.git` pointer file is writable at Low IL, so the orchestrator
//!   never discovers the repository from the checkout: every Git call passes the admin dir
//!   recorded when the checkout was created, and the pointer's bytes are compared after the
//!   run. A moved HEAD, a modified tracked file or a rewritten pointer is
//!   `verify_mutated_tree`.

use crate::git::short;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::store::TaskRow;
use super::task::sha256_hex;
use crate::config::{data_dir, expand_vars, Effective};
use crate::git::{GitCmd, Loc};
use crate::process::env::build_env;
use crate::process::Boundary;

pub const UNAVAILABLE: &str = "verify_boundary_unavailable";
pub const MUTATED: &str = "verify_mutated_tree";

/// M11-R16 (X8): why the verify stage cannot start — the boundary is unavailable or the checkout
/// is not the checkpoint. The escalation **class** travels with the error, typed; the engine
/// never reads it out of the reason text. `reason` begins with the class, as before.
#[derive(Debug)]
pub struct Stop {
    pub class: &'static str,
    pub reason: String,
}

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for Stop {}

fn stop(class: &'static str, reason: String) -> anyhow::Error {
    anyhow::Error::new(Stop { class, reason })
}

/// `verify.verify_root`, expanded (default `%LOCALAPPDATA%\toryo-verify`; `<data>/verify`
/// where `LOCALAPPDATA` does not exist). The machine's `verify.verify_root` replaces the
/// repository's (review H2, M11-A2).
pub fn verify_root(cfg: &Effective) -> PathBuf {
    expand_vars(verify_root_setting(cfg))
        .map(PathBuf::from)
        .unwrap_or_else(|_| data_dir().join("verify"))
}

/// The `verify_root` in force, as written: the machine's, else the repository's (M11-A2).
pub fn verify_root_setting(cfg: &Effective) -> &str {
    cfg.machine_verify
        .verify_root
        .as_deref()
        .unwrap_or(&cfg.repo.verify.verify_root)
}

/// Review H2 (M11-A2): the reason a repository's `verify.boundary` is below this machine's
/// floor (`verify.min_boundary`, default `low_integrity`), or `None`. Asked at `task create`
/// (and at `task config --reload`), before anything is created.
pub fn below_floor(cfg: &Effective) -> Option<String> {
    let floor = cfg
        .machine_verify
        .min_boundary
        .as_deref()
        .unwrap_or("low_integrity");
    if cfg.repo.verify.boundary != "none" || floor == "none" {
        return None;
    }
    let machine = crate::config::machine_config_hint(cfg.machine_path.as_deref());
    let repo = cfg.repo_path.display();
    Some(format!(
        "verify_boundary_below_floor: {repo} sets `verify.boundary: none` (trusted-code mode: the code under test runs with your full rights), below this machine's floor `low_integrity` (review H2). Either keep `verify.boundary: low_integrity` in {repo}, or — only if you trust this repository's code — let the machine's owner set `verify: {{ min_boundary: none }}` in {machine}. Environment gates (`kind: environment`, commands in the machine config) are the only sanctioned way to run something with your full rights."
    ))
}

/// The configured boundary; `separate_user` is not in v0.1.
pub fn boundary(cfg: &Effective) -> Result<Boundary> {
    match cfg.repo.verify.boundary.as_str() {
        "none" => Ok(Boundary::None),
        "low_integrity" => Ok(Boundary::LowIntegrity),
        other => Err(stop(
            UNAVAILABLE,
            format!("{UNAVAILABLE}: verify.boundary `{other}` is not available in v0.1 (low_integrity | none)"),
        )),
    }
}

/// How a result describes its boundary.
pub fn describe(b: Boundary) -> &'static str {
    match b {
        Boundary::LowIntegrity => "low_integrity",
        Boundary::None => "none (trusted-code mode: verification not isolated)",
    }
}

fn label(dirs: &[PathBuf]) -> Result<()> {
    // test hook: inject the error a folder without WRITE_OWNER gives (F7)
    if let Ok(code) = std::env::var("TORYO_TEST_LABEL_ERROR") {
        bail!(
            "cannot give {} a Low integrity label: os error {code} (access denied: the account lacks WRITE_OWNER there)",
            dirs.first().map(|d| d.display().to_string()).unwrap_or_default()
        );
    }
    crate::process::label_low(dirs)
}

/// Admission (R45) and `doctor`: can the configured boundary be established here?
pub fn probe(cfg: &Effective) -> Result<()> {
    match boundary(cfg)? {
        Boundary::None => Ok(()),
        Boundary::LowIntegrity => {
            if !cfg!(windows) {
                return Err(stop(
                    UNAVAILABLE,
                    format!("{UNAVAILABLE}: the Low integrity boundary exists only on Windows — set `verify.boundary: none` for trusted-code mode (verification not isolated)"),
                ));
            }
            let root = verify_root(cfg);
            let scratch = root.join(format!(".label-probe-{}", std::process::id()));
            let r = label(std::slice::from_ref(&scratch));
            // (R13 review) the control of the write probe below: in a folder that does carry a
            // Low label the same Low process must succeed — else a "cannot write" answer would
            // only mean that the probe itself does not work on this machine
            let control = match &r {
                Ok(()) => low_can_write(&scratch),
                Err(_) => Ok(true),
            };
            let _ = std::fs::remove_dir_all(&scratch);
            r.map_err(|e| {
                stop(
                    UNAVAILABLE,
                    format!(
                        "{UNAVAILABLE}: verify_root {} — {e:#} (R45)",
                        root.display()
                    ),
                )
            })?;
            match control {
                Ok(true) => {}
                Ok(false) => {
                    return Err(stop(
                        UNAVAILABLE,
                        format!(
                            "{UNAVAILABLE}: verify_root {} — a Low-integrity process could not create a folder inside a Low-labelled folder there, so verify commands could not write their checkout either (M11-R13)",
                            root.display()
                        ),
                    ))
                }
                Err(e) => {
                    return Err(stop(
                        UNAVAILABLE,
                        format!(
                            "{UNAVAILABLE}: could not start a Low-integrity process under verify_root {}: {e:#}",
                            root.display()
                        ),
                    ))
                }
            }
            not_low_writable(&root, &root)
        }
    }
}

/// M11-R13: `dir` (the verify root, or a task's folder under it) must **not** be writable at
/// Low integrity. Only the checkout and its `build`/`cache`/`tmp`/`home` get a Low label; the
/// export an environment gate deploys from (`deploy`, `deploy.index`) and its working folder
/// (`envgate`) are created next to them and are safe only while Low code cannot write there
/// (X6, X1). A `verify_root` inside a Low-labelled tree (for example under `AppData\LocalLow`)
/// would hand them to the code under test without a word. The proof is the real thing: a Low
/// process tries to create a folder in `dir` and must fail. Not Windows: nothing to check.
fn not_low_writable(dir: &Path, root: &Path) -> Result<()> {
    let writable = low_can_write(dir).map_err(|e| {
        stop(
            UNAVAILABLE,
            format!(
                "{UNAVAILABLE}: could not start a Low-integrity process to check {}: {e:#}",
                dir.display()
            ),
        )
    })?;
    if writable {
        return Err(stop(
            UNAVAILABLE,
            format!(
                "{UNAVAILABLE}: verify_root {} — a Low-integrity process can create files in {}, so the code under test could write what an environment gate deploys (the export of the checkpoint) and the folder the gate runs in. Set `verify.verify_root` to a folder outside Low-writable trees (not under AppData\\LocalLow; the default %LOCALAPPDATA%\\toryo-verify is fine) (M11-R13)",
                root.display(),
                dir.display()
            ),
        ));
    }
    Ok(())
}

/// Can a Low-integrity process create a folder in `dir`? (It is removed again.)
#[cfg(windows)]
fn low_can_write(dir: &Path) -> Result<bool> {
    use crate::process::{redact::Redactions, spawn, SpawnSpec, WaitOptions};
    let name = format!(".toryo-low-probe-{}", std::process::id());
    let target = dir.join(&name);
    let _ = std::fs::remove_dir(&target);
    let windows = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let spec = SpawnSpec {
        program: Path::new(&windows).join("System32").join("cmd.exe"),
        args: vec!["/d".into(), "/c".into(), "mkdir".into(), name],
        cwd: dir.to_path_buf(),
        env: build_env(std::env::vars(), &BTreeMap::new(), &BTreeMap::new()),
        stdin: None,
        boundary: Boundary::LowIntegrity,
    };
    let exit = spawn(&spec, &Redactions::empty())?.wait(
        WaitOptions {
            max: Some(std::time::Duration::from_secs(30)),
            ..Default::default()
        },
        |_| {},
    )?;
    let made = std::fs::symlink_metadata(&target).is_ok();
    if made {
        let _ = std::fs::remove_dir(&target);
    }
    // (R13 review) a probe that did not finish proves nothing: never read it as "cannot write"
    if !made && (exit.timed_out || exit.code.is_none()) {
        bail!(
            "the Low-integrity write probe did not finish (timed out: {})",
            exit.timed_out
        );
    }
    Ok(made)
}

#[cfg(not(windows))]
fn low_can_write(_dir: &Path) -> Result<bool> {
    Ok(false)
}

/// Recorded when the checkout is created (before any verify code ran), in the run store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkout {
    pub base: PathBuf,
    pub dir: PathBuf,
    /// `<repo>/.git/worktrees/<name>` — never re-discovered from the checkout (R47).
    pub admin: PathBuf,
    /// The original bytes of the checkout's `.git` pointer file.
    pub pointer: String,
    pub low: bool,
}

impl Checkout {
    pub fn loc(&self) -> Loc {
        Loc::Explicit {
            git_dir: self.admin.clone(),
            work_tree: self.dir.clone(),
        }
    }
    pub fn build(&self) -> PathBuf {
        self.base.join("build")
    }
    pub fn cache(&self) -> PathBuf {
        self.base.join("cache")
    }
    pub fn tmp(&self) -> PathBuf {
        self.base.join("tmp")
    }
    pub fn home(&self) -> PathBuf {
        self.base.join("home")
    }
}

fn record_path(t: &TaskRow) -> PathBuf {
    t.artifacts().join("verify-checkout.json")
}

fn empty_dir(d: &Path) -> Result<()> {
    // never through a link: a junction put in place of `d` (or inside it) is removed as a
    // link, its target is left alone (M11-R2 review)
    if std::fs::symlink_metadata(d).is_ok_and(|m| m.file_type().is_symlink()) {
        remove_link(d).with_context(|| format!("removing the link {}", d.display()))?;
    }
    std::fs::create_dir_all(d)?;
    for e in std::fs::read_dir(d)?.flatten() {
        let p = e.path();
        let ft = e.file_type().ok();
        let r = if ft.is_some_and(|f| f.is_symlink()) {
            remove_link(&p)
        } else if ft.is_some_and(|f| f.is_dir()) {
            std::fs::remove_dir_all(&p)
        } else {
            std::fs::remove_file(&p)
        };
        r.with_context(|| format!("emptying {}", d.display()))?;
    }
    Ok(())
}

/// Remove a symbolic link or junction itself (a directory link on Windows needs `remove_dir`).
fn remove_link(p: &Path) -> std::io::Result<()> {
    std::fs::remove_file(p).or_else(|_| std::fs::remove_dir(p))
}

/// M11-R21: remove the build-output entry `rel` (a frozen `BuildEntry` path: repository-relative,
/// as written) from the checkout `root`: a file, or a folder with what is in it. Never through
/// a link — a link or junction on the way (code under test may have planted one) is removed as a
/// link, its target is left alone. Missing, absolute or leaving the folder: nothing to do.
fn remove_entry(root: &Path, rel: &str) -> Result<()> {
    let rel = rel.replace('\\', "/");
    let parts: Vec<&str> = rel
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    if parts.is_empty()
        || parts.contains(&"..")
        || rel.starts_with('/')
        || rel.as_bytes().get(1) == Some(&b':')
    {
        return Ok(());
    }
    let mut p = root.to_path_buf();
    for (i, c) in parts.iter().enumerate() {
        p.push(c);
        let Ok(m) = std::fs::symlink_metadata(&p) else {
            return Ok(());
        };
        let removed = if m.file_type().is_symlink() {
            remove_link(&p)
        } else if i + 1 < parts.len() {
            continue;
        } else if m.is_dir() {
            std::fs::remove_dir_all(&p)
        } else {
            std::fs::remove_file(&p)
        };
        return removed.with_context(|| format!("removing {}", p.display()));
    }
    Ok(())
}

/// Create the checkout once (or again, if it is gone), then reset it to `sha` for this run.
pub fn prepare(
    g: &GitCmd,
    t: &TaskRow,
    cfg: &Effective,
    sha: &str,
    b: Boundary,
) -> Result<Checkout> {
    let low = b == Boundary::LowIntegrity;
    let rec: Option<Checkout> = std::fs::read(record_path(t))
        .ok()
        .and_then(|x| serde_json::from_slice(&x).ok());
    // (R13) a recorded checkout is used while it is under the verify root in force: after a
    // `verify_root` change (`task config --reload`) a new one is made there; the old folder is
    // left alone
    let base = verify_root(cfg).join(&t.id);
    if low {
        std::fs::create_dir_all(&base)?;
        not_low_writable(&base, &verify_root(cfg))?;
    }
    let co = match rec
        .filter(|c| c.dir.exists() && c.admin.exists() && c.low == low && c.base == base)
    {
        Some(c) => c,
        None => {
            let dir = base.join(format!("{}.verify", t.id));
            let repo = Loc::Discover(t.repo.clone());
            let fwd = dir.display().to_string().replace('\\', "/");
            if dir.exists() {
                let _ = g.output(&repo, false, &["worktree", "remove", "--force", &fwd]);
                let _ = std::fs::remove_dir_all(&dir);
            }
            let _ = g.output(&repo, false, &["worktree", "prune"]);
            std::fs::create_dir_all(&base)?;
            g.run(
                &repo,
                false,
                &["worktree", "add", "-q", "--detach", &fwd, sha],
            )?;
            // trustworthy: no verify code has run in the checkout yet
            let admin = PathBuf::from(
                g.run_str(
                    &Loc::Discover(dir.clone()),
                    true,
                    &["rev-parse", "--absolute-git-dir"],
                )?
                .trim(),
            );
            let pointer = std::fs::read_to_string(dir.join(".git"))?;
            let c = Checkout {
                base,
                dir,
                admin,
                pointer,
                low,
            };
            for d in [c.build(), c.cache(), c.tmp(), c.home()] {
                std::fs::create_dir_all(&d)?;
            }
            if low {
                label(&[c.dir.clone(), c.build(), c.cache(), c.tmp(), c.home()])
                    .map_err(|e| stop(UNAVAILABLE, format!("{UNAVAILABLE}: {e:#} (R45)")))?;
            }
            std::fs::write(record_path(t), serde_json::to_vec_pretty(&c)?)?;
            c
        }
    };
    // a pointer rewritten by an earlier run is put back from the recorded bytes
    if std::fs::read_to_string(co.dir.join(".git")).ok().as_deref() != Some(co.pointer.as_str()) {
        std::fs::write(co.dir.join(".git"), &co.pointer)?;
    }
    let loc = co.loc();
    g.run(&loc, false, &["checkout", "-q", "--detach", "--force", sha])?;
    let clean = cfg.repo.verify.checkout != "tracked";
    g.run(
        &loc,
        false,
        &["clean", "-q", if clean { "-ffdx" } else { "-ffd" }],
    )?;
    empty_dir(&co.tmp())?;
    empty_dir(&co.home())?;
    if clean {
        empty_dir(&co.build())?;
        empty_dir(&co.cache())?;
    } else {
        // (R21) `clean -ffd` keeps ignored files, so an entry script built from a checkpoint the
        // reviewer rejected would be the entry of the next run when no gate builds it again: the
        // task's build-output entries start every run absent, before any gate
        for e in &cfg.build_entries {
            // a locked file is no reason to stop: a stale entry would show in the next run's
            // own checks
            if let Err(err) = remove_entry(&co.dir, &e.path) {
                eprintln!("warning: could not remove the build-output entry `{}` from the verify checkout: {err:#}", e.path);
            }
        }
    }
    let head = g.run_str(&loc, true, &["rev-parse", "HEAD"])?;
    let status = g.run(
        &loc,
        true,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    if head.trim() != sha || !status.is_empty() {
        return Err(stop(
            MUTATED,
            format!(
                "{MUTATED}: the verify checkout is not a clean checkout of {} before the run (HEAD {})",
                short(sha),
                head.trim()
            ),
        ));
    }
    Ok(co)
}

/// The environment of a verify command: caches and temp in the Low directories.
pub fn env(co: &Checkout) -> BTreeMap<String, String> {
    let mut env = build_env(std::env::vars(), &BTreeMap::new(), &BTreeMap::new());
    let mut set = |k: &str, v: &Path| {
        env.retain(|e, _| !e.eq_ignore_ascii_case(k));
        env.insert(k.to_string(), v.display().to_string());
    };
    set("TEMP", &co.tmp());
    set("TMP", &co.tmp());
    set("NUGET_PACKAGES", &co.cache().join("nuget"));
    set("DOTNET_CLI_HOME", &co.home());
    set("npm_config_cache", &co.cache().join("npm"));
    set("XDG_CACHE_HOME", &co.cache());
    set("TORYO_VERIFY_BUILD", &co.build());
    if co.low {
        // tools that insist on the profile get Low locations (M0-VERIFY 9)
        set("APPDATA", &co.home().join("AppData").join("Roaming"));
        set("LOCALAPPDATA", &co.home().join("AppData").join("Local"));
        set(
            "PSModuleAnalysisCachePath",
            &co.cache().join("psmodule-analysis"),
        );
    }
    for (k, v) in [
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("NO_COLOR", "1"),
        ("POWERSHELL_TELEMETRY_OPTOUT", "1"),
        ("POWERSHELL_UPDATECHECK", "Off"),
    ] {
        env.insert(k.into(), v.into());
    }
    env
}

/// M11-E1: the variables every gate gets (check and environment gates, and the verify
/// commands of `toryo.yaml`-only mode), next to `TORYO_VERIFY_BUILD`: `TORYO_TASK` (the task
/// id), `TORYO_SHA` (the checkpoint commit being verified) and `TORYO_VERIFY_CHECKOUT` (the
/// clean checkout of that commit: `checkout` — the Low verify checkout for a check gate, the
/// fresh export for an environment gate, M11-R9).
pub fn set_gate_vars(
    env: &mut BTreeMap<String, String>,
    task: &str,
    sha: &str,
    co: &Checkout,
    checkout: &Path,
) {
    for (k, v) in [
        ("TORYO_TASK", task.to_string()),
        ("TORYO_SHA", sha.to_string()),
        ("TORYO_VERIFY_CHECKOUT", checkout.display().to_string()),
        ("TORYO_VERIFY_BUILD", co.build().display().to_string()),
    ] {
        env.retain(|e, _| !e.eq_ignore_ascii_case(k));
        env.insert(k.into(), v);
    }
}

/// After the run (R29/R47): `Some(reason)` when the checkout was mutated.
pub fn mutated(g: &GitCmd, co: &Checkout, sha: &str) -> Result<Option<String>> {
    if std::fs::read_to_string(co.dir.join(".git")).ok().as_deref() != Some(co.pointer.as_str()) {
        return Ok(Some(format!("{MUTATED}: a verify command rewrote the checkout's .git pointer file (R47); toryo kept using {}", co.admin.display())));
    }
    let loc = co.loc();
    let head = g.run_str(&loc, true, &["rev-parse", "HEAD"])?;
    if head.trim() != sha {
        return Ok(Some(format!(
            "{MUTATED}: a verify command moved HEAD of the verify checkout to {}",
            head.trim()
        )));
    }
    let changed = g.run(
        &loc,
        true,
        &[
            "diff",
            "--name-only",
            "-z",
            "--no-ext-diff",
            "--no-textconv",
            "HEAD",
        ],
    )?;
    if !changed.is_empty() {
        let files: Vec<String> = changed
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).to_string())
            .collect();
        return Ok(Some(format!(
            "{MUTATED}: a verify command modified tracked files {files:?}"
        )));
    }
    Ok(None)
}

/// M11-R9 (X6): what a check gate left in the Low verify checkout that the checkpoint does not
/// hold — untracked or ignored files, a gitlink (Git never looks inside it), an entry named
/// `.git` below the root, a link or junction that is not tracked. It is **not deployed**: the
/// environment gates read a fresh export of the commit ([`deploy_source`]), so this is one
/// warning and `checkout_extra` in `VERIFY_RESULT.json`, never an escalation. The checkout is
/// walked without following links and compared with `git ls-files` (explicit admin dir, R47).
pub fn checkout_extra(g: &GitCmd, co: &Checkout) -> Result<Vec<String>> {
    let key = |p: &str| {
        if cfg!(windows) {
            p.to_lowercase()
        } else {
            p.to_string()
        }
    };
    let out = g.run(&co.loc(), true, &["ls-files", "-s", "-z"])?;
    let mut tracked = std::collections::HashSet::new();
    let mut found = Vec::new();
    for e in out.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let e = String::from_utf8_lossy(e);
        let Some((_, path)) = e.split_once('\t') else {
            continue;
        };
        // a gitlink of the checkpoint itself is not something a check gate left (R9 review)
        tracked.insert(key(path));
    }
    // (R9 review) best effort and capped: a check gate must never make verify *error* — an
    // unreadable entry is skipped, and the walk stops after `CAP` finds
    const CAP: usize = 1000;
    fn walk(
        dir: &Path,
        rel: &str,
        tracked: &std::collections::HashSet<String>,
        key: &dyn Fn(&str) -> String,
        found: &mut Vec<String>,
    ) {
        if found.len() >= CAP {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            if found.len() >= CAP {
                return;
            }
            let name = e.file_name().to_string_lossy().to_string();
            // the pointer to the recorded admin dir (R47; `mutated` checks its bytes)
            if rel.is_empty() && name.eq_ignore_ascii_case(".git") {
                continue;
            }
            let r = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let Ok(ft) = e.file_type() else { continue };
            if name.eq_ignore_ascii_case(".git") {
                found.push(format!("{r} (a `.git` entry)"));
            } else if ft.is_symlink() {
                if !tracked.contains(&key(&r)) {
                    found.push(format!("{r} (a link or junction)"));
                }
            } else if ft.is_dir() {
                walk(&e.path(), &r, tracked, key, found);
            } else if !tracked.contains(&key(&r)) {
                found.push(r);
            }
        }
    }
    walk(&co.dir, "", &tracked, &key, &mut found);
    Ok(found)
}

/// M11-R9: a fresh export of a checkpoint commit — what the environment gates read as
/// `TORYO_VERIFY_CHECKOUT`. `dir` has no `.git` entry; `index` is the scratch index it was
/// checked out from; `tree` the tree id it holds; `manifest` every entry of `dir` as it was
/// when the export was proven (M11-R20) — what the after-gate check compares with.
#[derive(Debug, Clone)]
pub struct Export {
    pub dir: PathBuf,
    pub index: PathBuf,
    pub tree: String,
    pub manifest: Manifest,
}

/// M11-R9 (X6): export commit `head` into `<base>/deploy` from the repository's **object
/// store** — nothing is copied from the Low checkout, which Low code may have forged (Git's
/// "unchanged" is a size and timestamp test). The folder is emptied link-safely, then a scratch
/// index next to it (`deploy.index`, `GIT_INDEX_FILE`) gets `read-tree <head>` and
/// `checkout-index -a` (without `-u`: the index keeps no size or timestamp, M11-R13). Git runs with the admin dir recorded when the checkout was created (R47)
/// and the hardened prefix of `g` (hooks off, fsmonitor off, the filter plan R37) and reads the
/// repository's own config, so the bytes are those of a clean checkout. Neither `deploy` nor
/// the index is Low-labelled, and the task's folder is proven not writable at Low integrity
/// (`prepare`, M11-R13). The export is accepted only when `write-tree` of the index equals
/// `<head>^{tree}`, a manifest of the folder (every entry, files with the SHA-256 of their bytes)
/// has exactly the commit's entries, and each file's bytes are the bytes Git must write for the
/// commit's blob (`git cat-file --filters`, M11-R20, X15) — compared as bytes, not through Git's
/// view after its clean conversion; no stat data, no timestamp.
pub fn deploy_source(g: &GitCmd, co: &Checkout, head: &str) -> Result<Export> {
    let dir = co.base.join("deploy");
    let index = co.base.join("deploy.index");
    empty_dir(&dir)?;
    // the scratch index and a lock a killed git left behind (owner-only files)
    for f in [index.clone(), co.base.join("deploy.index.lock")] {
        if std::fs::symlink_metadata(&f).is_ok() {
            // a link is removed as a link (its target is left alone)
            remove_link(&f).with_context(|| format!("removing {}", f.display()))?;
        }
    }
    let loc = Loc::Explicit {
        git_dir: co.admin.clone(),
        work_tree: dir.clone(),
    };
    // (R9 review) the commit's bytes only: no replacement objects, and a link in the commit
    // is written as a plain file holding its target text, never as a link
    let scratch = [
        ("GIT_INDEX_FILE", index.as_path()),
        ("GIT_NO_REPLACE_OBJECTS", Path::new("1")),
    ];
    let git = |args: &[&str]| -> Result<String> {
        let mut full = vec!["-c", "core.symlinks=false"];
        full.extend_from_slice(args);
        let args = &full[..];
        let o = g.output_env(&loc, false, args, &scratch)?;
        if !o.status.success() {
            bail!(
                "git {} failed ({}): {}",
                args.join(" "),
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    git(&["read-tree", head])?;
    git(&["checkout-index", "-a", "-f"])?;
    let tree = git(&["write-tree"])?;
    let want = git(&["rev-parse", &format!("{head}^{{tree}}")])?;
    if tree != want {
        bail!(
            "the export of {} holds tree {tree}, not the commit's {want}",
            short(head)
        );
    }
    if let Some(l) = first_link(&dir)? {
        bail!(
            "the export of {} holds a link ({}) — links are exported as files",
            short(head),
            l.display()
        );
    }
    // (R9 review, R13, R20) before any gate reads them: the folder holds the commit's entries
    // and no others, and each file holds the bytes Git must write for it
    let manifest = manifest(&dir).context("reading the export")?;
    let e = Export {
        dir,
        index,
        tree,
        manifest,
    };
    if let Some(why) = bytes_are_the_commits(g, co, &e, &e.manifest)? {
        bail!(
            "the export of {} does not hold the bytes Git writes for the commit: {why}",
            short(head)
        );
    }
    Ok(e)
}

/// The first symbolic link or junction under `dir` (not followed), if any.
fn first_link(dir: &Path) -> Result<Option<PathBuf>> {
    for e in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let e = e?;
        let ft = e.file_type()?;
        if ft.is_symlink() {
            return Ok(Some(e.path()));
        }
        if ft.is_dir() {
            if let Some(l) = first_link(&e.path())? {
                return Ok(Some(l));
            }
        }
    }
    Ok(None)
}

/// M11-R20 (X15): one entry of an export as found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A file: its size and the SHA-256 of its bytes.
    File { size: u64, sha256: String },
    /// A folder, an empty one too.
    Dir,
    /// A symbolic link or junction — never followed: the text of its target.
    Link(String),
    /// Anything else (a pipe, a socket, a device): never opened.
    Other,
}

/// Every entry under an export folder: its path (relative, `/`) and what it is.
pub type Manifest = BTreeMap<String, Item>;

/// SHA-256 of a file's bytes, read in pieces; with its size.
fn hash_file(p: &Path) -> Result<(u64, String)> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let ctx = || format!("reading {}", p.display());
    let mut f = std::fs::File::open(p).with_context(ctx)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = f.read(&mut buf).with_context(ctx)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        size += n as u64;
    }
    Ok((size, hex::encode(h.finalize())))
}

/// M11-R20 (X15): walk `dir` — no link is followed — and record **every** entry: files with the
/// SHA-256 of their bytes, folders (empty ones too), links, and a `.git` entry like any other.
/// No timestamp, no Git index, no line-ending conversion is involved: bytes.
pub fn manifest(dir: &Path) -> Result<Manifest> {
    fn walk(dir: &Path, rel: &str, out: &mut Manifest) -> Result<()> {
        for e in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let e = e?;
            let name = e.file_name().to_string_lossy().to_string();
            let r = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            let ft = e
                .file_type()
                .with_context(|| format!("reading {}", e.path().display()))?;
            if ft.is_symlink() {
                let target = std::fs::read_link(e.path())
                    .map(|t| t.to_string_lossy().to_string())
                    .unwrap_or_default();
                out.insert(r, Item::Link(target));
            } else if ft.is_dir() {
                out.insert(r.clone(), Item::Dir);
                walk(&e.path(), &r, out)?;
            } else if ft.is_file() {
                let (size, sha256) = hash_file(&e.path())?;
                out.insert(r, Item::File { size, sha256 });
            } else {
                out.insert(r, Item::Other);
            }
        }
        Ok(())
    }
    let mut out = Manifest::new();
    walk(dir, "", &mut out)?;
    Ok(out)
}

/// The first few names of `v`, and how many more there are.
fn some(v: &[String]) -> String {
    const SHOWN: usize = 8;
    let mut s = v.iter().take(SHOWN).cloned().collect::<Vec<_>>().join(", ");
    if v.len() > SHOWN {
        s.push_str(&format!(", … (+{} more)", v.len() - SHOWN));
    }
    s
}

/// What [`batch_hashes`] read: the SHA-256 of each answer it could frame, and the first item
/// whose answer could not be framed — its bytes are not the file's — after which nothing is known.
struct Hashed {
    sums: Vec<String>,
    lost: Option<usize>,
}

/// The SHA-256 of what `git cat-file --batch` prints for each of `items` — `(object, path, size
/// of the file on disk)`; with `filters` the bytes Git writes to a work tree for that path
/// (`--filters`: attributes, `core.autocrlf`, filter drivers), else the stored bytes. Streamed:
/// a blob is hashed as it is read, never held.
///
/// With `--filters` the header names the size of the *stored* blob, not of what follows, so the
/// answers are framed by the size of the file on disk: when the bytes Git writes have another
/// length the frame does not close (no line break, no header of the next object) and that file
/// is the one reported as `lost` — a wrong length is a mismatch.
fn batch_hashes(
    g: &GitCmd,
    loc: &Loc,
    scratch: &[(&str, &Path)],
    filters: bool,
    items: &[(&str, &str, u64)],
) -> Result<Hashed> {
    use sha2::{Digest, Sha256};
    use std::io::{BufRead, Read, Write};
    let mut args = vec!["-c", "core.symlinks=false", "cat-file", "--batch"];
    if filters {
        args.push("--filters");
    }
    let mut child = g.spawn_piped(loc, true, &args, scratch)?;
    let (Some(mut stdin), Some(mut stderr), Some(stdout)) =
        (child.stdin.take(), child.stderr.take(), child.stdout.take())
    else {
        bail!("git cat-file has no pipes");
    };
    // the requests go out from a thread: git answers while it is still being asked
    let lines: Vec<String> = items
        .iter()
        .map(|(o, p, _)| {
            if filters {
                format!("{o} {p}\n")
            } else {
                format!("{o}\n")
            }
        })
        .collect();
    let writer = std::thread::spawn(move || {
        for l in lines {
            if stdin.write_all(l.as_bytes()).is_err() {
                break;
            }
        }
    });
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let mut out = std::io::BufReader::new(stdout);
    let mut read = Hashed {
        sums: Vec::with_capacity(items.len()),
        lost: None,
    };
    let mut buf = vec![0u8; 64 * 1024];
    'items: for (n, (oid, _, size)) in items.iter().enumerate() {
        // the header of this answer: `<object> blob <size>`
        let mut line = String::new();
        let got = out.read_line(&mut line).unwrap_or(0);
        let mut parts = line.trim_end().split(' ');
        let framed = got > 0
            && matches!(
                (parts.next(), parts.next(), parts.next(), parts.next()),
                (Some(o), Some("blob"), Some(s), None)
                    if o == *oid && !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
            );
        if !framed {
            // `<object> missing` names this item; else the answer before it was longer or
            // shorter than the file: it is the one lost
            let lost = if line.trim_end().ends_with(" missing") {
                n
            } else {
                n.saturating_sub(1)
            };
            read.lost = Some(lost);
            read.sums.truncate(lost);
            break;
        }
        let mut h = Sha256::new();
        let mut left = *size;
        while left > 0 {
            let want = buf.len().min(left as usize);
            match out.read(&mut buf[..want]) {
                Ok(k) if k > 0 => {
                    h.update(&buf[..k]);
                    left -= k as u64;
                }
                _ => {
                    read.lost = Some(n);
                    break 'items;
                }
            }
        }
        let mut nl = [0u8; 1];
        if out.read_exact(&mut nl).is_err() || nl[0] != b'\n' {
            read.lost = Some(n);
            break;
        }
        read.sums.push(hex::encode(h.finalize()));
    }
    // nothing may follow the last answer: bytes left over mean its length was not the file's
    if read.lost.is_none() && !items.is_empty() && out.fill_buf().is_ok_and(|b| !b.is_empty()) {
        read.lost = Some(items.len() - 1);
        read.sums.truncate(items.len() - 1);
    }
    drop(out); // git stops writing if it has not finished
    let _ = writer.join();
    let stderr = errors.join().unwrap_or_default();
    let status = child.wait()?;
    if read.lost.is_none() && !status.success() {
        bail!("git cat-file failed ({status}): {}", stderr.trim());
    }
    Ok(read)
}

/// M11-R20 (X15), **before the gate**: "the bytes are the commit's". The export folder holds
/// exactly the entries of the commit's tree — no other file, folder, link or `.git` entry — and
/// every file hashes to the bytes **Git must write** for its blob: `git cat-file --filters`, the
/// settings of the export (attributes, `core.autocrlf`, filter drivers), compared as bytes. Git's
/// own view after its clean conversion is no part of it: a repository whose `.gitattributes` says
/// `text eol=crlf` over blobs that still hold CRLF is clean. `None` when it holds; else the first
/// files that do not.
fn bytes_are_the_commits(
    g: &GitCmd,
    co: &Checkout,
    e: &Export,
    found: &Manifest,
) -> Result<Option<String>> {
    let loc = Loc::Explicit {
        git_dir: co.admin.clone(),
        work_tree: e.dir.clone(),
    };
    // the settings of the export (`deploy_source`)
    let scratch = [
        ("GIT_INDEX_FILE", e.index.as_path()),
        ("GIT_NO_REPLACE_OBJECTS", Path::new("1")),
    ];
    let o = g.output_env(
        &loc,
        true,
        &[
            "-c",
            "core.symlinks=false",
            "ls-tree",
            "-r",
            "-z",
            "--full-tree",
            &e.tree,
        ],
        &scratch,
    )?;
    if !o.status.success() {
        bail!(
            "git ls-tree failed ({}): {}",
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    // what the commit holds: files (object id, link?) and folders — the parents of every entry
    // and a submodule's (checked out as an empty folder, which may be missing)
    let mut files: BTreeMap<String, (String, bool)> = BTreeMap::new();
    let mut dirs: std::collections::BTreeSet<String> = Default::default();
    for ent in o.stdout.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        let ent = String::from_utf8_lossy(ent).to_string();
        let Some((meta, path)) = ent.split_once('\t') else {
            bail!("git ls-tree printed `{ent}`");
        };
        let m: Vec<&str> = meta.split(' ').collect();
        let [mode, kind, oid] = m[..] else {
            bail!("git ls-tree printed `{ent}`");
        };
        if kind == "commit" {
            dirs.insert(path.to_string());
        } else {
            files.insert(path.to_string(), (oid.to_string(), mode == "120000"));
        }
        let mut p = path;
        while let Some((parent, _)) = p.rsplit_once('/') {
            dirs.insert(parent.to_string());
            p = parent;
        }
    }
    let mut problems: Vec<String> = Vec::new();
    for (p, item) in found {
        let ok = match item {
            Item::File { .. } => files.contains_key(p),
            Item::Dir => dirs.contains(p),
            Item::Link(_) | Item::Other => false,
        };
        if !ok {
            problems.push(format!("{p} (the commit has no such entry)"));
        }
    }
    type Asked<'a> = Vec<(&'a str, &'a str, u64)>;
    let (mut plain, mut stored): (Asked, Asked) = (Vec::new(), Vec::new());
    for (p, (oid, link)) in &files {
        let Some(Item::File { size, .. }) = found.get(p) else {
            problems.push(format!("{p} (missing)"));
            continue;
        };
        if p.contains(['\n', '\r']) {
            // the request protocol cannot carry the name: its bytes are not compared (it is
            // present, and the commit's entry — no certainty, no complaint)
            continue;
        } else if *link {
            // a link is exported as a file holding its target text: the stored bytes
            stored.push((oid, p, *size));
        } else {
            plain.push((oid, p, *size));
        }
    }
    for (filters, items) in [(true, plain), (false, stored)] {
        if items.is_empty() {
            continue;
        }
        let read = batch_hashes(g, &loc, &scratch, filters, &items)?;
        for ((_, p, _), sha) in items.iter().zip(&read.sums) {
            if !matches!(found.get(*p), Some(Item::File { sha256, .. }) if sha256 == sha) {
                problems.push(format!(
                    "{p} (not the bytes Git writes for the commit's blob)"
                ));
            }
        }
        if let Some(n) = read.lost {
            let (_, p, _) = items[n];
            problems.push(format!(
                "{p} (not the bytes Git writes for the commit's blob — its length differs; the files after it were not compared)"
            ));
        }
    }
    problems.sort();
    Ok((!problems.is_empty()).then(|| some(&problems)))
}

/// M11-R20 (X15), **after the gate**: `Some(reason)` when the export is not what it was before
/// the gate ran — a file whose bytes changed, a file or folder added (an empty one, a `.git`
/// entry too) or removed. The manifest taken when the export was proven is compared with a new
/// walk: bytes, no timestamp, no Git. It guards the owner's own script — the gate runs with the
/// owner's rights, so it is a contract, not a boundary.
pub fn export_changed(e: &Export) -> Option<String> {
    let now = match manifest(&e.dir) {
        Ok(m) => m,
        Err(err) => {
            return Some(format!(
                "{MUTATED}: the export of the checkpoint an environment gate deploys from (TORYO_VERIFY_CHECKOUT) cannot be read after the gate ran: {err:#} — a gate script must never write there"
            ))
        }
    };
    let (mut changed, mut added, mut removed) = (Vec::new(), Vec::new(), Vec::new());
    for (p, item) in &now {
        match e.manifest.get(p) {
            None => added.push(p.clone()),
            Some(before) if before != item => changed.push(p.clone()),
            Some(_) => {}
        }
    }
    for p in e.manifest.keys().filter(|p| !now.contains_key(*p)) {
        removed.push(p.clone());
    }
    if changed.is_empty() && added.is_empty() && removed.is_empty() {
        return None;
    }
    let mut what = Vec::new();
    for (label, v) in [
        ("changed", &changed),
        ("added", &added),
        ("removed", &removed),
    ] {
        if !v.is_empty() {
            what.push(format!("{label}: {}", some(v)));
        }
    }
    Some(format!(
        "{MUTATED}: the export of the checkpoint an environment gate deploys from (TORYO_VERIFY_CHECKOUT) is not what it was before the gate ran — {} — a gate script must never write there",
        what.join("; ")
    ))
}

/// M11-R2 (X1): the working directory of an environment gate — `<base>/envgate`, created and
/// emptied before each gate. It is not among the Low-labelled folders, so code a check gate ran
/// at Low integrity cannot plant a file there; a relative argument, a bare tool name or a DLL
/// lookup never resolves to a file of the checkout.
pub fn envgate_dir(co: &Checkout) -> Result<PathBuf> {
    let d = co.base.join("envgate");
    empty_dir(&d)?;
    Ok(d)
}

/// SHA-256 of each verify script as checked out (R29: the result names what ran).
pub fn script_hashes(files: &[String], co: &Checkout) -> BTreeMap<String, String> {
    files
        .iter()
        .filter_map(|f| {
            std::fs::read(co.dir.join(f))
                .ok()
                .map(|b| (f.clone(), sha256_hex(&b)))
        })
        .collect()
}

/// `task cleanup`: remove the checkout and its directories. Without the frozen configuration
/// (`cfg` None, EC-12) only a recorded checkout is found.
pub fn cleanup(g: &GitCmd, t: &TaskRow, cfg: Option<&Effective>) -> Result<()> {
    let rec: Option<Checkout> = std::fs::read(record_path(t))
        .ok()
        .and_then(|x| serde_json::from_slice(&x).ok());
    let base = rec
        .as_ref()
        .map(|c| c.base.clone())
        .or_else(|| cfg.map(|c| verify_root(c).join(&t.id)));
    if let Some(c) = rec {
        let fwd = c.dir.display().to_string().replace('\\', "/");
        let _ = g.output(
            &Loc::Discover(t.repo.clone()),
            false,
            &["worktree", "remove", "--force", &fwd],
        );
    }
    if let Some(base) = base.filter(|b| b.exists()) {
        std::fs::remove_dir_all(&base).with_context(|| format!("removing {}", base.display()))?;
    }
    let _ = g.output(
        &Loc::Discover(t.repo.clone()),
        false,
        &["worktree", "prune"],
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M11-R2 review: emptying a folder never goes through a link — a junction put in place of
    /// the folder, or inside it, is removed as a link and its target keeps its files.
    // Windows only: junctions (`mklink /J`) need no privilege there
    #[cfg(windows)]
    #[test]
    fn emptying_a_folder_never_follows_a_junction() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("precious");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("keep.txt"), "x").unwrap();
        let junction = |link: &Path| {
            let ok = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(link)
                .arg(&target)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "mklink /J {}", link.display());
        };
        // in place of the folder
        let envgate = d.path().join("envgate");
        junction(&envgate);
        empty_dir(&envgate).unwrap();
        assert!(
            target.join("keep.txt").exists(),
            "the target keeps its files"
        );
        let m = std::fs::symlink_metadata(&envgate).unwrap();
        assert!(
            m.is_dir() && !m.file_type().is_symlink(),
            "a real folder now"
        );
        // inside it
        junction(&envgate.join("inner"));
        std::fs::write(envgate.join("stray.txt"), "x").unwrap();
        empty_dir(&envgate).unwrap();
        assert_eq!(std::fs::read_dir(&envgate).unwrap().count(), 0);
        assert!(target.join("keep.txt").exists());
    }

    /// A repository with one commit and a verify-style checkout of it (as `prepare` makes it:
    /// a detached worktree, its admin dir and pointer recorded), under `d`.
    fn repo_and_checkout(d: &Path) -> (GitCmd, Checkout, String) {
        repo_and_checkout_with(d, false)
    }

    /// git in `dir` (the test repository), checked.
    fn tgit(dir: &Path, args: &[&str]) -> String {
        let o = std::process::Command::new(crate::git::git_exe())
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "core.autocrlf=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    /// [`repo_and_checkout`]; `with_link`: the commit also holds a link entry `lnk` (mode
    /// 120000) whose target text is `../outside`.
    fn repo_and_checkout_with(d: &Path, with_link: bool) -> (GitCmd, Checkout, String) {
        let repo = d.join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let sh = tgit;
        sh(&repo, &["init", "-q", "-b", "main"]);
        sh(&repo, &["config", "core.autocrlf", "false"]);
        std::fs::write(repo.join("app.txt"), "committed\n").unwrap();
        std::fs::write(repo.join("src").join("lib.txt"), "library\n").unwrap();
        sh(&repo, &["add", "-A"]);
        if with_link {
            std::fs::write(d.join("target.txt"), "../outside").unwrap();
            let blob = sh(
                &repo,
                &[
                    "hash-object",
                    "-w",
                    &d.join("target.txt").display().to_string(),
                ],
            );
            sh(
                &repo,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("120000,{blob},lnk"),
                ],
            );
        }
        sh(&repo, &["commit", "-q", "-m", "one"]);
        let head = sh(&repo, &["rev-parse", "HEAD"]);
        let base = d.join("vr").join("T-1");
        std::fs::create_dir_all(&base).unwrap();
        let dir = base.join("T-1.verify");
        sh(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                &dir.display().to_string(),
                &head,
            ],
        );
        let admin = PathBuf::from(sh(&dir, &["rev-parse", "--absolute-git-dir"]));
        let pointer = std::fs::read_to_string(dir.join(".git")).unwrap();
        let g = GitCmd::new(crate::git::git_exe(), d.join("hooks")).unwrap();
        let co = Checkout {
            base,
            dir,
            admin,
            pointer,
            low: false,
        };
        (g, co, head)
    }

    /// M11-R9 AC-1: the export holds the committed bytes — not what a check gate wrote into the
    /// checkout — and no added file and no `.git`; its tree is the commit's. A gate that then
    /// writes into it is seen (`export_changed`); a fresh export is clean again.
    #[test]
    fn the_export_holds_the_commit_not_the_checkout() {
        let d = tempfile::tempdir().unwrap();
        let (g, co, head) = repo_and_checkout(d.path());
        // a forged tracked file (same length) and an added one, in the checkout
        std::fs::write(co.dir.join("app.txt"), "FORGED!!!\n").unwrap();
        std::fs::write(co.dir.join("added.txt"), "x").unwrap();
        let e = deploy_source(&g, &co, &head).unwrap();
        assert_eq!(e.dir, co.base.join("deploy"));
        assert_eq!(
            std::fs::read_to_string(e.dir.join("app.txt")).unwrap(),
            "committed\n"
        );
        assert_eq!(
            std::fs::read_to_string(e.dir.join("src").join("lib.txt")).unwrap(),
            "library\n"
        );
        assert!(!e.dir.join("added.txt").exists() && !e.dir.join(".git").exists());
        let mut names: Vec<String> = std::fs::read_dir(&e.dir)
            .unwrap()
            .map(|x| x.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        assert_eq!(names, ["app.txt", "src"]);
        let want = g
            .run_str(&co.loc(), true, &["rev-parse", &format!("{head}^{{tree}}")])
            .unwrap();
        assert_eq!(e.tree, want);
        assert_eq!(export_changed(&e), None);
        // the checkout itself still says what it holds: the forged file is not hidden from
        // `checkout_extra`'s view of added files
        assert_eq!(checkout_extra(&g, &co).unwrap(), ["added.txt"]);
        // a gate that writes into the export: a changed and an added file are seen
        std::fs::write(e.dir.join("app.txt"), "synced\n").unwrap();
        std::fs::write(e.dir.join("new.txt"), "x").unwrap();
        let why = export_changed(&e).expect("changed");
        assert!(
            why.starts_with("verify_mutated_tree:")
                && why.contains("app.txt")
                && why.contains("new.txt"),
            "{why}"
        );
        // the next export starts from scratch
        let e = deploy_source(&g, &co, &head).unwrap();
        assert!(!e.dir.join("new.txt").exists());
        assert_eq!(export_changed(&e), None);
    }

    /// R9 review: the export holds only the commit's bytes — a link entry of the commit is a
    /// plain file holding its target text (never a link, whatever `core.symlinks` says), a
    /// replacement object of the repository is not used, and a stale `deploy.index.lock` left by
    /// a killed git does not block the next export.
    #[test]
    fn the_export_is_plain_files_of_the_commit() {
        let d = tempfile::tempdir().unwrap();
        let (g, co, head) = repo_and_checkout_with(d.path(), true);
        let repo = d.path().join("repo");
        tgit(&repo, &["config", "core.symlinks", "true"]);
        // a replacement for app.txt's blob in the repository
        let blob = tgit(&repo, &["rev-parse", &format!("{head}:app.txt")]);
        std::fs::write(d.path().join("other.txt"), "replaced\n").unwrap();
        let other = tgit(
            &repo,
            &[
                "hash-object",
                "-w",
                &d.path().join("other.txt").display().to_string(),
            ],
        );
        tgit(&repo, &["replace", &blob, &other]);
        std::fs::write(co.base.join("deploy.index.lock"), "").unwrap();
        let e = deploy_source(&g, &co, &head).unwrap();
        let lnk = e.dir.join("lnk");
        let m = std::fs::symlink_metadata(&lnk).unwrap();
        assert!(m.is_file() && !m.file_type().is_symlink(), "a plain file");
        assert_eq!(std::fs::read_to_string(&lnk).unwrap(), "../outside");
        assert_eq!(
            std::fs::read_to_string(e.dir.join("app.txt")).unwrap(),
            "committed\n",
            "the commit's blob, not its replacement"
        );
        assert_eq!(first_link(&e.dir).unwrap(), None);
        assert_eq!(export_changed(&e), None);
    }

    /// M11-R13 AC-1: the export check compares **content**. A file replaced inside a finished
    /// export by bytes of the same length, with its timestamps put back — and an index that
    /// looks newer than every file, as after a long export — is reported: Git's stat data
    /// (size, times) would call it unchanged.
    #[test]
    fn a_same_length_change_with_restored_timestamps_is_seen() {
        let d = tempfile::tempdir().unwrap();
        let (g, co, head) = repo_and_checkout(d.path());
        // Linux: the change time cannot be put back; Git for Windows does not look at it
        tgit(
            &d.path().join("repo"),
            &["config", "core.trustctime", "false"],
        );
        let e = deploy_source(&g, &co, &head).unwrap();
        let f = e.dir.join("app.txt");
        let m = std::fs::metadata(&f).unwrap();
        let times = std::fs::FileTimes::new()
            .set_modified(m.modified().unwrap())
            .set_accessed(m.accessed().unwrap());
        assert_eq!(m.len(), "FORGED!!!\n".len() as u64);
        std::fs::write(&f, "FORGED!!!\n").unwrap();
        let fh = std::fs::File::options().write(true).open(&f).unwrap();
        fh.set_times(times).unwrap();
        drop(fh);
        // the index is newer than the file by more than a second: no "racy" second look
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
        std::fs::File::options()
            .write(true)
            .open(&e.index)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let why = export_changed(&e).expect("seen");
        assert!(why.contains("app.txt"), "{why}");
        // and a file whose bytes are the commit's again is clean, whatever its times say
        std::fs::write(&f, "committed\n").unwrap();
        assert_eq!(export_changed(&e), None);
    }

    /// M11-R20 AC-1 (X15): `.gitattributes` says `text eol=crlf` and the blob still holds CRLF
    /// (committed with `core.autocrlf=false`, never renormalised). The bytes on disk are the
    /// blob's; the export is accepted and the after-gate check finds nothing.
    #[test]
    fn crlf_blobs_under_eol_crlf_export_clean() {
        let d = tempfile::tempdir().unwrap();
        let (g, co, _) = repo_and_checkout(d.path());
        let repo = d.path().join("repo");
        std::fs::write(repo.join(".gitattributes"), "*.xml text eol=crlf\n").unwrap();
        std::fs::write(d.path().join("model.xml"), "<a>\r\n<b/>\r\n</a>\r\n").unwrap();
        let blob = tgit(
            &repo,
            &[
                "hash-object",
                "-w",
                "--no-filters",
                &d.path().join("model.xml").display().to_string(),
            ],
        );
        tgit(
            &repo,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{blob},model.xml"),
            ],
        );
        tgit(&repo, &["add", ".gitattributes"]);
        tgit(&repo, &["commit", "-q", "-m", "xml"]);
        let head = tgit(&repo, &["rev-parse", "HEAD"]);
        let e = deploy_source(&g, &co, &head).unwrap();
        assert_eq!(
            std::fs::read(e.dir.join("model.xml")).unwrap(),
            b"<a>\r\n<b/>\r\n</a>\r\n"
        );
        assert_eq!(export_changed(&e), None);
        // and a blob with LF only is written with CRLF there, and is just as clean
        std::fs::write(repo.join("lf.xml"), "<a>\n</a>\n").unwrap();
        tgit(&repo, &["add", "lf.xml"]);
        tgit(&repo, &["commit", "-q", "-m", "lf"]);
        let head = tgit(&repo, &["rev-parse", "HEAD"]);
        let e = deploy_source(&g, &co, &head).unwrap();
        assert_eq!(
            std::fs::read(e.dir.join("lf.xml")).unwrap(),
            b"<a>\r\n</a>\r\n"
        );
        assert_eq!(export_changed(&e), None);
    }

    /// M11-R20 AC-2 (X15): inside a finished export each of these is reported, on every
    /// platform — a file whose bytes changed (same length, timestamps put back: the other test),
    /// an added file, a removed file, an empty folder, a `.git` file — and the export is clean
    /// again once it is put back.
    #[test]
    fn every_change_to_a_finished_export_is_reported() {
        let d = tempfile::tempdir().unwrap();
        let (g, co, head) = repo_and_checkout(d.path());
        let e = deploy_source(&g, &co, &head).unwrap();
        assert_eq!(export_changed(&e), None);
        let said = |want: &str, name: &str| {
            let why = export_changed(&e).expect(name);
            assert!(
                why.starts_with("verify_mutated_tree:") && why.contains(want),
                "{name}: {why}"
            );
        };
        let lib = e.dir.join("src").join("lib.txt");
        std::fs::write(&lib, "LIBRARY\n").unwrap(); // same length
        said("changed: src/lib.txt", "same-length change");
        std::fs::write(&lib, "library\n").unwrap();
        assert_eq!(export_changed(&e), None);
        std::fs::write(e.dir.join("new.txt"), "x").unwrap();
        said("added: new.txt", "added file");
        std::fs::remove_file(e.dir.join("new.txt")).unwrap();
        std::fs::remove_file(e.dir.join("app.txt")).unwrap();
        said("removed: app.txt", "removed file");
        std::fs::write(e.dir.join("app.txt"), "committed\n").unwrap();
        assert_eq!(export_changed(&e), None);
        std::fs::create_dir(e.dir.join("empty")).unwrap();
        said("added: empty", "empty folder");
        std::fs::remove_dir(e.dir.join("empty")).unwrap();
        std::fs::create_dir_all(e.dir.join("src").join("deeper")).unwrap();
        said("added: src/deeper", "empty folder inside a folder");
        std::fs::remove_dir(e.dir.join("src").join("deeper")).unwrap();
        std::fs::write(e.dir.join(".git"), "gitdir: elsewhere\n").unwrap();
        said("added: .git", ".git file");
        std::fs::remove_file(e.dir.join(".git")).unwrap();
        assert_eq!(export_changed(&e), None);
    }

    /// M11-R20 Do 2: before the gate, the folder holds exactly the commit's entries and each
    /// file holds the bytes Git writes for its blob — a file of the same length with other
    /// bytes, one of another length (the answers can no longer be framed by it), an extra and a
    /// missing file, a `.git` entry and a link are all named.
    #[test]
    fn the_bytes_are_the_commits_before_the_gate() {
        let d = tempfile::tempdir().unwrap();
        let (g, co, head) = repo_and_checkout(d.path());
        let e = deploy_source(&g, &co, &head).unwrap();
        let check = |e: &Export| {
            let found = manifest(&e.dir).unwrap();
            bytes_are_the_commits(&g, &co, e, &found).unwrap()
        };
        assert_eq!(check(&e), None);
        let app = e.dir.join("app.txt");
        std::fs::write(&app, "COMMITTED\n").unwrap();
        let why = check(&e).expect("same length");
        assert!(why.contains("app.txt") && !why.contains("lib.txt"), "{why}");
        std::fs::write(&app, "short\n").unwrap();
        let why = check(&e).expect("shorter");
        assert!(why.contains("app.txt"), "{why}");
        std::fs::write(&app, "committed and longer than the blob\n").unwrap();
        let why = check(&e).expect("longer");
        assert!(why.contains("app.txt"), "{why}");
        std::fs::write(&app, "committed\n").unwrap();
        assert_eq!(check(&e), None);
        // (review) the last file, one byte short of what Git writes: bytes are left over
        let lib = e.dir.join("src").join("lib.txt");
        std::fs::write(&lib, "library").unwrap();
        let why = check(&e).expect("a prefix of the last file");
        assert!(why.contains("src/lib.txt"), "{why}");
        std::fs::write(&lib, "library\n").unwrap();
        assert_eq!(check(&e), None);
        std::fs::write(e.dir.join("extra.txt"), "x").unwrap();
        std::fs::write(e.dir.join(".git"), "gitdir: x\n").unwrap();
        std::fs::create_dir(e.dir.join("empty")).unwrap();
        let why = check(&e).expect("extra");
        assert!(
            why.contains("extra.txt") && why.contains(".git") && why.contains("empty"),
            "{why}"
        );
        std::fs::remove_file(e.dir.join("extra.txt")).unwrap();
        std::fs::remove_file(e.dir.join(".git")).unwrap();
        std::fs::remove_dir(e.dir.join("empty")).unwrap();
        std::fs::remove_file(e.dir.join("src").join("lib.txt")).unwrap();
        let why = check(&e).expect("missing");
        assert!(why.contains("src/lib.txt (missing)"), "{why}");
    }

    /// M11-R21: a frozen build-output entry is removed from a `tracked` checkout — a file, a
    /// folder, one under a folder — and never through a link; what is missing is no error.
    #[test]
    fn a_build_output_entry_is_removed_without_following_a_link() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("co");
        std::fs::create_dir_all(root.join("dist").join("deep")).unwrap();
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::write(root.join("dist").join("cli.js"), "x").unwrap();
        std::fs::write(root.join("dist").join("deep").join("a.txt"), "x").unwrap();
        std::fs::write(root.join("build").join("keep.txt"), "x").unwrap();
        remove_entry(&root, "./dist/cli.js").unwrap();
        assert!(!root.join("dist").join("cli.js").exists());
        assert!(root.join("dist").join("deep").exists(), "only the entry");
        remove_entry(&root, r"dist\deep").unwrap();
        assert!(!root.join("dist").join("deep").exists(), "a folder entry");
        remove_entry(&root, "nowhere/at/all.js").unwrap();
        for bad in ["", "..", "../x", "/etc/passwd", "C:/x"] {
            remove_entry(&root, bad).unwrap();
        }
        assert!(root.join("build").join("keep.txt").exists());
        // a link on the way is removed as a link, whatever it points to
        let target = d.path().join("precious");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("cli.js"), "keep").unwrap();
        std::fs::remove_dir_all(root.join("dist")).unwrap();
        #[cfg(windows)]
        let linked = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(root.join("dist"))
            .arg(&target)
            .output()
            .unwrap()
            .status
            .success();
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&target, root.join("dist")).is_ok();
        assert!(linked, "a link could be made");
        remove_entry(&root, "dist/cli.js").unwrap();
        assert!(target.join("cli.js").exists(), "the target keeps its file");
        assert!(std::fs::symlink_metadata(root.join("dist")).is_err());
    }

    /// M11-R13: hashing every file is not fooled by line-ending conversion — with
    /// `core.autocrlf=true` (Git for Windows' default) a blob stored with CRLF, one stored with
    /// LF and an empty file all export clean, and a real change in each is still seen.
    #[test]
    fn the_content_check_follows_the_line_ending_settings() {
        let d = tempfile::tempdir().unwrap();
        let (g, co, _) = repo_and_checkout(d.path());
        let repo = d.path().join("repo");
        std::fs::write(repo.join("crlf.txt"), "a\r\nb\r\n").unwrap();
        std::fs::write(repo.join("lf.txt"), "a\nb\n").unwrap();
        std::fs::write(repo.join("empty.txt"), "").unwrap();
        tgit(&repo, &["add", "crlf.txt", "lf.txt", "empty.txt"]);
        tgit(&repo, &["commit", "-q", "-m", "two"]);
        let head = tgit(&repo, &["rev-parse", "HEAD"]);
        tgit(&repo, &["config", "core.autocrlf", "true"]);
        let e = deploy_source(&g, &co, &head).unwrap();
        assert_eq!(
            std::fs::read(e.dir.join("lf.txt")).unwrap(),
            b"a\r\nb\r\n",
            "converted on the way out, like a clean checkout"
        );
        assert_eq!(export_changed(&e), None);
        for (name, text) in [
            ("crlf.txt", "a\r\nX\r\n"),
            ("lf.txt", "a\r\nX\r\n"),
            ("empty.txt", "x"),
        ] {
            let before = std::fs::read(e.dir.join(name)).unwrap();
            std::fs::write(e.dir.join(name), text).unwrap();
            let why = export_changed(&e).expect(name);
            assert!(why.contains(name), "{why}");
            std::fs::write(e.dir.join(name), before).unwrap();
            assert_eq!(export_changed(&e), None, "{name}");
        }
    }
}
