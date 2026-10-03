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
            })?;
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
        anyhow::anyhow!(
            "{UNAVAILABLE}: could not start a Low-integrity process to check {}: {e:#}",
            dir.display()
        )
    })?;
    if writable {
        bail!(
            "{UNAVAILABLE}: verify_root {} — a Low-integrity process can create files in {}, so the code under test could write what an environment gate deploys (the export of the checkpoint) and the folder the gate runs in. Set `verify.verify_root` to a folder outside Low-writable trees (not under AppData\\LocalLow; the default %LOCALAPPDATA%\\toryo-verify is fine) (M11-R13)",
            root.display(),
            dir.display()
        );
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
    spawn(&spec, &Redactions::empty())?.wait(
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
        low_base(&base, cfg, t)?;
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

/// M11-R13: [`not_low_writable`] for a task's folder, with what the owner does next.
fn low_base(base: &Path, cfg: &Effective, t: &TaskRow) -> Result<()> {
    not_low_writable(base, &verify_root(cfg)).map_err(|e| {
        anyhow::anyhow!(
            "{e:#} — then `toryo task config {} --reload` and `toryo resume {}`",
            t.id,
            t.id
        )
    })
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
/// `checkout-index -a` (without `-u`: the index keeps no size or timestamp, M11-R13). Git runs with the admin dir recorded when the checkout was created (R47)
/// and the hardened prefix of `g` (hooks off, fsmonitor off, the filter plan R37) and reads the
/// repository's own config, so the bytes are those of a clean checkout. Neither `deploy` nor
/// the index is Low-labelled, and the task's folder is proven not writable at Low integrity
/// (`prepare`, M11-R13). The export is accepted only when `write-tree` of the index equals
/// `<head>^{tree}` and every file on disk **hashes** to its entry ([`export_changed`]): what is
/// proven is "each file's content has the object id the commit names", by reading the file —
/// not by trusting the object store's bytes or Git's stat data.
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
    let e = Export { dir, index, tree };
    // (R9 review, R13) before any gate reads them, every file on disk must hash to the
    // commit's entry, and the export holds no link
    if let Some(why) = export_changed(g, co, &e)? {
        bail!("the export of {} is not the commit: {why}", short(head));
    }
    if let Some(l) = first_link(&e.dir)? {
        bail!(
            "the export of {} holds a link ({}) — links are exported as files",
            short(head),
            l.display()
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

/// M11-R9 / R13 (E3 AC-8 against the export): `Some(reason)` when the export does not hold
/// exactly the commit — a file whose **content** differs, a file missing or a file added. It
/// runs before the gate (in [`deploy_source`]) and after it. No stat shortcut: every file is
/// hashed against a throw-away index read from the export's tree, which holds no size or
/// timestamp, so Git has to read each file (the index of the export itself is never refreshed
/// either). A touched timestamp is no change. After the gate this guards the owner's own script
/// — the gate runs with the owner's rights, so it is a contract, not a boundary.
pub fn export_changed(g: &GitCmd, co: &Checkout, e: &Export) -> Result<Option<String>> {
    let loc = Loc::Explicit {
        git_dir: co.admin.clone(),
        work_tree: e.dir.clone(),
    };
    let check = co.base.join("deploy.check.index");
    let clear = || -> Result<()> {
        for f in [check.clone(), co.base.join("deploy.check.index.lock")] {
            if std::fs::symlink_metadata(&f).is_ok() {
                remove_link(&f).with_context(|| format!("removing {}", f.display()))?;
            }
        }
        Ok(())
    };
    clear()?;
    // the settings of the export (`deploy_source`)
    let scratch = [
        ("GIT_INDEX_FILE", check.as_path()),
        ("GIT_NO_REPLACE_OBJECTS", Path::new("1")),
    ];
    let c = ["-c", "core.symlinks=false"];
    let git = |read_only: bool, args: &[&str]| -> Result<Vec<u8>> {
        let mut full = c.to_vec();
        full.extend_from_slice(args);
        let o = g.output_env(&loc, read_only, &full, &scratch)?;
        if !o.status.success() {
            bail!(
                "git {} failed ({}): {}",
                args.join(" "),
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
        Ok(o.stdout)
    };
    let mut files = Vec::new();
    let found = (|| -> Result<()> {
        git(false, &["read-tree", &e.tree])?;
        // every entry is without stat data, so this hashes every file; an entry whose content
        // matches gets its stat data, one that does not stays "changed" (the exit status says
        // only that there is one)
        let _ = g.output_env(
            &loc,
            false,
            &[c[0], c[1], "update-index", "-q", "--refresh"],
            &scratch,
        )?;
        for args in [
            &["diff-files", "--name-only", "-z"][..],
            &["ls-files", "--others", "-z"][..],
        ] {
            files.extend(
                git(true, args)?
                    .split(|b| *b == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).to_string()),
            );
        }
        Ok(())
    })();
    let _ = clear();
    found?;
    if files.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "{MUTATED}: the export of the checkpoint an environment gate deploys from (TORYO_VERIFY_CHECKOUT) does not hold the commit's files: {files:?} — a gate script must never write there"
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
        assert_eq!(export_changed(&g, &co, &e).unwrap(), None);
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
        let why = export_changed(&g, &co, &e).unwrap().expect("seen");
        assert!(why.contains("app.txt"), "{why}");
        // and a file whose bytes are the commit's again is clean, whatever its times say
        std::fs::write(&f, "committed\n").unwrap();
        assert_eq!(export_changed(&g, &co, &e).unwrap(), None);
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
        assert_eq!(export_changed(&g, &co, &e).unwrap(), None);
        for (name, text) in [
            ("crlf.txt", "a\r\nX\r\n"),
            ("lf.txt", "a\r\nX\r\n"),
            ("empty.txt", "x"),
        ] {
            let before = std::fs::read(e.dir.join(name)).unwrap();
            std::fs::write(e.dir.join(name), text).unwrap();
            let why = export_changed(&g, &co, &e).unwrap().expect(name);
            assert!(why.contains(name), "{why}");
            std::fs::write(e.dir.join(name), before).unwrap();
            assert_eq!(export_changed(&g, &co, &e).unwrap(), None, "{name}");
        }
    }
}
