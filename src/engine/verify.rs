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
        other => bail!("{UNAVAILABLE}: verify.boundary `{other}` is not available in v0.1 (low_integrity | none)"),
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
                bail!("{UNAVAILABLE}: the Low integrity boundary exists only on Windows — set `verify.boundary: none` for trusted-code mode (verification not isolated)");
            }
            let root = verify_root(cfg);
            let scratch = root.join(format!(".label-probe-{}", std::process::id()));
            let r = label(std::slice::from_ref(&scratch));
            let _ = std::fs::remove_dir_all(&scratch);
            r.map_err(|e| {
                anyhow::anyhow!(
                    "{UNAVAILABLE}: verify_root {} — {e:#} (R45)",
                    root.display()
                )
            })
        }
    }
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
    std::fs::create_dir_all(d)?;
    for e in std::fs::read_dir(d)?.flatten() {
        let p = e.path();
        let r = if e.file_type().map(|f| f.is_dir()).unwrap_or(false) {
            std::fs::remove_dir_all(&p)
        } else {
            std::fs::remove_file(&p)
        };
        r.with_context(|| format!("emptying {}", d.display()))?;
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
    let co = match rec.filter(|c| c.dir.exists() && c.admin.exists() && c.low == low) {
        Some(c) => c,
        None => {
            let base = verify_root(cfg).join(&t.id);
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
                    .map_err(|e| anyhow::anyhow!("{UNAVAILABLE}: {e:#} (R45)"))?;
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
    }
    let head = g.run_str(&loc, true, &["rev-parse", "HEAD"])?;
    let status = g.run(
        &loc,
        true,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    if head.trim() != sha || !status.is_empty() {
        bail!(
            "{MUTATED}: the verify checkout is not a clean checkout of {} before the run (HEAD {})",
            short(sha),
            head.trim()
        );
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
