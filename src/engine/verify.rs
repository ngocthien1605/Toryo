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
        let Some((meta, path)) = e.split_once('\t') else {
            continue;
        };
        if meta.starts_with("160000 ") {
            found.push(format!("{path} (a gitlink)"));
        }
        tracked.insert(key(path));
    }
    fn walk(
        dir: &Path,
        rel: &str,
        tracked: &std::collections::HashSet<String>,
        key: &dyn Fn(&str) -> String,
        found: &mut Vec<String>,
    ) -> Result<()> {
        for e in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let e = e?;
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
            let ft = e.file_type()?;
            if name.eq_ignore_ascii_case(".git") {
                found.push(format!("{r} (a `.git` entry)"));
            } else if ft.is_symlink() {
                if !tracked.contains(&key(&r)) {
                    found.push(format!("{r} (a link or junction)"));
                }
            } else if ft.is_dir() {
                walk(&e.path(), &r, tracked, key, found)?;
            } else if !tracked.contains(&key(&r)) {
                found.push(r);
            }
        }
        Ok(())
    }
    walk(&co.dir, "", &tracked, &key, &mut found)?;
    Ok(found)
}

/// M11-R9: a fresh export of a checkpoint commit — what the environment gates read as
/// `TORYO_VERIFY_CHECKOUT`. `dir` has no `.git` entry; `index` is the scratch index it was
/// checked out from; `tree` the tree id it holds.
#[derive(Debug, Clone)]
pub struct Export {
    pub dir: PathBuf,
    pub index: PathBuf,
    pub tree: String,
}

/// M11-R9 (X6): export commit `head` into `<base>/deploy` from the repository's **object
/// store** — nothing is copied from the Low checkout, which Low code may have forged (Git's
/// "unchanged" is a size and timestamp test). The folder is emptied link-safely, then a scratch
/// index next to it (`deploy.index`, `GIT_INDEX_FILE`) gets `read-tree <head>` and
/// `checkout-index -a`. Git runs with the admin dir recorded when the checkout was created (R47)
/// and the hardened prefix of `g` (hooks off, fsmonitor off, the filter plan R37) and reads the
/// repository's own config, so the bytes are those of a clean checkout. Neither `deploy` nor
/// the index is Low-labelled. The export is accepted only when `write-tree` of the index equals
/// `<head>^{tree}`.
pub fn deploy_source(g: &GitCmd, co: &Checkout, head: &str) -> Result<Export> {
    let dir = co.base.join("deploy");
    let index = co.base.join("deploy.index");
    empty_dir(&dir)?;
    if std::fs::symlink_metadata(&index).is_ok() {
        // a link is removed as a link (its target is left alone)
        remove_link(&index).with_context(|| format!("removing {}", index.display()))?;
    }
    let loc = Loc::Explicit {
        git_dir: co.admin.clone(),
        work_tree: dir.clone(),
    };
    let scratch = [("GIT_INDEX_FILE", index.as_path())];
    let git = |args: &[&str]| -> Result<String> {
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
    git(&["checkout-index", "-a", "-f", "-u"])?;
    let tree = git(&["write-tree"])?;
    let want = git(&["rev-parse", &format!("{head}^{{tree}}")])?;
    if tree != want {
        bail!(
            "the export of {} holds tree {tree}, not the commit's {want}",
            short(head)
        );
    }
    Ok(Export { dir, index, tree })
}

/// M11-R9 (E3 AC-8 against the export): after an environment gate, `Some(reason)` when the
/// export no longer holds exactly the commit — a tracked file changed (stat refreshed, then
/// compared by content) or a file added. The gate runs with the owner's rights, so this
/// catches a script that writes into `TORYO_VERIFY_CHECKOUT`, not a forgery.
pub fn export_changed(g: &GitCmd, co: &Checkout, e: &Export) -> Result<Option<String>> {
    let loc = Loc::Explicit {
        git_dir: co.admin.clone(),
        work_tree: e.dir.clone(),
    };
    let scratch = [("GIT_INDEX_FILE", e.index.as_path())];
    // a timestamp-only touch is no change: refresh the stat data, then compare by content
    let _ = g.output_env(&loc, false, &["update-index", "-q", "--refresh"], &scratch)?;
    let mut files = Vec::new();
    for args in [
        &["diff-files", "--name-only", "-z"][..],
        &["ls-files", "--others", "-z"][..],
    ] {
        let o = g.output_env(&loc, true, args, &scratch)?;
        if !o.status.success() {
            bail!(
                "git {} failed ({}): {}",
                args.join(" "),
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
        files.extend(
            o.stdout
                .split(|b| *b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).to_string()),
        );
    }
    if files.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "{MUTATED}: an environment gate changed the export of the checkpoint it deploys from (TORYO_VERIFY_CHECKOUT): {files:?} — a gate script must never write there"
    )))
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
        let repo = d.join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let sh = |dir: &Path, args: &[&str]| {
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
        };
        sh(&repo, &["init", "-q", "-b", "main"]);
        sh(&repo, &["config", "core.autocrlf", "false"]);
        std::fs::write(repo.join("app.txt"), "committed\n").unwrap();
        std::fs::write(repo.join("src").join("lib.txt"), "library\n").unwrap();
        sh(&repo, &["add", "-A"]);
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
        assert_eq!(export_changed(&g, &co, &e).unwrap(), None);
        // the checkout itself still says what it holds: the forged file is not hidden from
        // `checkout_extra`'s view of added files
        assert_eq!(checkout_extra(&g, &co).unwrap(), ["added.txt"]);
        // a gate that writes into the export: a changed and an added file are seen
        std::fs::write(e.dir.join("app.txt"), "synced\n").unwrap();
        std::fs::write(e.dir.join("new.txt"), "x").unwrap();
        let why = export_changed(&g, &co, &e).unwrap().expect("changed");
        assert!(
            why.starts_with("verify_mutated_tree:")
                && why.contains("app.txt")
                && why.contains("new.txt"),
            "{why}"
        );
        // the next export starts from scratch
        let e = deploy_source(&g, &co, &head).unwrap();
        assert!(!e.dir.join("new.txt").exists());
        assert_eq!(export_changed(&g, &co, &e).unwrap(), None);
    }
}
