//! Git canaries (plan R33 hook canary, R37 `git_filter_textconv_canary`, M0-VERIFY 10).
//!
//! Builds a throw-away repository whose configuration tries to run programs at every
//! opportunity — every client hook (in `.git/hooks` *and* a repo-level
//! `core.hooksPath`), `core.fsmonitor`, filter drivers (`clean`/`smudge`/`process`),
//! a textconv helper and `diff.external` — each writing a marker file. It then runs
//! the orchestrator's Git lifecycle through [`GitCmd`] and reports which markers exist.
//!
//! Variants:
//! * `unhardened` — the same operations with plain `git` (negative control: markers MUST appear,
//!   otherwise the canary proves nothing on this machine);
//! * `neutralized` — no trusted filters, untrusted ones neutralised: NO marker may appear;
//! * `trusted-lfs` — `lfs` resolved from a trusted table: only `trusted-*` markers may appear;
//! * `admission` — untrusted referenced filter with neutralisation off: admission must refuse.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::Serialize;

use super::{GitCmd, Loc, CANON_DIFF};
use crate::config::TrustedFilter;

pub const CLIENT_HOOKS: &[&str] = &[
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-auto-gc",
    "post-rewrite",
    "reference-transaction",
    "post-index-change",
    "push-to-checkout",
    "fsmonitor-watchman",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
pub enum Variant {
    Unhardened,
    Neutralized,
    TrustedLfs,
    Admission,
}

#[derive(Debug, Serialize)]
pub struct CanaryReport {
    pub variant: Variant,
    pub git_version: String,
    pub markers: Vec<String>,
    pub unexpected_markers: Vec<String>,
    pub expected_missing: Vec<String>,
    pub steps: Vec<String>,
    pub diff_has_binary_patch: Option<bool>,
    pub admission_error: Option<String>,
    pub pass: bool,
    pub note: String,
}

fn fwd(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

fn write_script(dir: &Path, name: &str, body: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(p)
}

struct Fixture {
    repo: PathBuf,
    markers: PathBuf,
}

fn marker_body(markers: &Path, name: &str) -> String {
    format!("echo hit >> '{}/{}'", fwd(markers), name)
}

/// Create the booby-trapped repository with plain git (config writes run no programs).
fn build_fixture(git: &Path, root: &Path, process_cmd: Option<&str>) -> Result<Fixture> {
    let repo = root.join("repo");
    let markers = root.join("markers");
    let scripts = root.join("scripts");
    std::fs::create_dir_all(&repo)?;
    std::fs::create_dir_all(&markers)?;
    let plain = |args: &[&str]| -> Result<()> {
        let st = Command::new(git)
            .args(args)
            .current_dir(&repo)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()?;
        if !st.status.success() {
            anyhow::bail!("git {:?}: {}", args, String::from_utf8_lossy(&st.stderr));
        }
        Ok(())
    };
    plain(&["init", "-q", "-b", "main", "."])?;
    // repo-local identity: the control's commits must not fail on a machine without a global one
    plain(&["config", "user.name", "Canary"])?;
    plain(&["config", "user.email", "canary@local.invalid"])?;
    // hooks in .git/hooks and in a repo-level core.hooksPath
    for dir in [repo.join(".git").join("hooks"), repo.join(".husky")] {
        for h in CLIENT_HOOKS {
            let tag = if dir.ends_with("hooks") {
                "githooks"
            } else {
                "husky"
            };
            write_script(
                &dir,
                h,
                &format!(
                    "{}\nexit 0",
                    marker_body(&markers, &format!("hook-{tag}-{h}"))
                ),
            )?;
        }
    }
    let s =
        |n: &str, body: &str| write_script(&scripts, n, body).map(|p| format!("sh '{}'", fwd(&p)));
    let lfs_clean = s(
        "lfs-clean.sh",
        &format!("{}\ncat", marker_body(&markers, "repo-filter-lfs-clean")),
    )?;
    let lfs_smudge = s(
        "lfs-smudge.sh",
        &format!("{}\ncat", marker_body(&markers, "repo-filter-lfs-smudge")),
    )?;
    let evil_clean = s(
        "evil-clean.sh",
        &format!("{}\ncat", marker_body(&markers, "repo-filter-evil-clean")),
    )?;
    // The review's C4 case: a program configured under the *trusted name* `lfs`.
    // With the toryo binary available this is a well-behaved process filter (so the plain-git
    // control runs the whole lifecycle and reaches every hook and the textconv helper);
    // otherwise a script that records the call and fails.
    let lfs_process = match process_cmd {
        Some(pc) => format!("{pc} '{}/repo-filter-lfs-process'", fwd(&markers)),
        None => s(
            "lfs-process.sh",
            &format!(
                "{}\nexit 1",
                marker_body(&markers, "repo-filter-lfs-process")
            ),
        )?,
    };
    let textconv = s(
        "textconv.sh",
        &format!("{}\ncat \"$1\"", marker_body(&markers, "repo-textconv")),
    )?;
    let ext = s(
        "extdiff.sh",
        &format!("{}\nexit 0", marker_body(&markers, "repo-diff-external")),
    )?;
    let fsmon = s(
        "fsmonitor.sh",
        &format!("{}\nexit 1", marker_body(&markers, "repo-fsmonitor")),
    )?;
    for (k, v) in [
        ("core.hooksPath", ".husky"),
        ("core.fsmonitor", fsmon.as_str()),
        ("filter.lfs.clean", lfs_clean.as_str()),
        ("filter.lfs.smudge", lfs_smudge.as_str()),
        ("filter.evil.clean", evil_clean.as_str()),
        ("filter.lfs.process", lfs_process.as_str()),
        ("diff.hex.textconv", textconv.as_str()),
        ("diff.external", ext.as_str()),
        // unused driver defined only in config: must be neutralised as well
        ("filter.dormant.clean", evil_clean.as_str()),
    ] {
        plain(&["config", k, v])?;
    }
    std::fs::write(
        repo.join(".gitattributes"),
        "*.bin filter=lfs diff=hex\n*.dat filter=evil\n",
    )?;
    std::fs::write(repo.join("a.txt"), "hello\n")?;
    std::fs::write(repo.join("img.bin"), [0u8, 1, 2, 3, 255, 0, 10, 13, 0])?;
    std::fs::write(repo.join("data.dat"), "data\n")?;
    Ok(Fixture { repo, markers })
}

fn list_markers(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Run one canary variant in a fresh directory under `work`.
/// `trusted_process`: command for the trusted `lfs` process driver, e.g.
/// `'<toryo.exe>' spike filter-process --marker` (the marker path is appended).
pub fn run(
    git: &Path,
    work: &Path,
    variant: Variant,
    trusted_process: Option<&str>,
) -> Result<CanaryReport> {
    let root = work.join(format!("{variant:?}").to_lowercase());
    if root.exists() {
        std::fs::remove_dir_all(&root).ok();
    }
    std::fs::create_dir_all(&root)?;
    let fx = build_fixture(git, &root, trusted_process)?;
    let git_version = String::from_utf8_lossy(&Command::new(git).arg("--version").output()?.stdout)
        .trim()
        .to_string();
    let mut steps = Vec::new();
    let mut report = CanaryReport {
        variant,
        git_version,
        markers: vec![],
        unexpected_markers: vec![],
        expected_missing: vec![],
        steps: vec![],
        diff_has_binary_patch: None,
        admission_error: None,
        pass: false,
        note: String::new(),
    };

    let mut trusted: BTreeMap<String, TrustedFilter> = BTreeMap::new();
    if matches!(variant, Variant::TrustedLfs | Variant::Admission) {
        let Some(tp) = trusted_process else {
            report.note = "skipped: needs the toryo binary as the trusted process filter".into();
            report.pass = false;
            return Ok(report);
        };
        trusted.insert(
            "lfs".into(),
            TrustedFilter {
                process: Some(format!("{tp} '{}/trusted-lfs-process'", fwd(&fx.markers))),
                clean: None,
                smudge: None,
                required: true,
                exe_sha256: None,
            },
        );
    }

    let g_empty = GitCmd::new(git, root.join("empty-hooks"))?;
    let wt = root.join("wt");
    let vwt = root.join("wt.verify");

    if variant == Variant::Unhardened {
        let plain = |dir: &Path, args: &[&str]| -> Result<Vec<u8>> {
            let o = Command::new(git)
                .args(args)
                .current_dir(dir)
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .with_context(|| format!("plain git {args:?} in {}", dir.display()))?;
            // Failures are expected here (the booby-trapped process filter exits 1);
            // the control only needs to show that repository programs ran.
            Ok(o.stdout)
        };
        for (dir, args) in [
            (&fx.repo, vec!["add", "-A"]),
            (&fx.repo, vec!["commit", "-q", "-m", "base"]),
        ] {
            plain(dir, &args)?;
        }
        plain(
            &fx.repo,
            &["worktree", "add", "-q", "-b", "toryo/t1", &fwd(&wt), "HEAD"],
        )?;
        if wt.exists() {
            std::fs::write(wt.join("img.bin"), [9u8, 0, 9, 0, 9])?;
            plain(&wt, &["add", "-A"])?;
            plain(&wt, &["commit", "-q", "-m", "checkpoint"])?;
            plain(&wt, &["diff", "main..HEAD"])?;
            plain(&wt, &["diff", "--no-ext-diff", "main..HEAD"])?; // reaches the textconv helper
                                                                   // second control: the repo's own .git/hooks (the first run used core.hooksPath=.husky)
            let githooks = format!(
                "core.hooksPath={}",
                fwd(&fx.repo.join(".git").join("hooks"))
            );
            plain(
                &wt,
                &[
                    "-c",
                    &githooks,
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-m",
                    "control-2",
                ],
            )?;
        }
        report.markers = list_markers(&fx.markers);
        // Every mechanism the hardened variants claim to stop must be shown to run here,
        // otherwise their "no marker" result would prove nothing for that mechanism.
        let required = [
            ("hook-husky-", "repo-level core.hooksPath hooks"),
            ("hook-githooks-", ".git/hooks hooks"),
            ("repo-filter-", "repository filter commands"),
            ("repo-textconv", "textconv helper"),
            ("repo-diff-external", "diff.external"),
        ];
        let missing: Vec<&str> = required
            .iter()
            .filter(|(prefix, _)| !report.markers.iter().any(|m| m.starts_with(prefix)))
            .map(|(_, what)| *what)
            .collect();
        report.expected_missing = missing.iter().map(|s| s.to_string()).collect();
        report.pass = missing.is_empty();
        report.note = if report.pass {
            "negative control OK: plain git ran hooks (both locations), filters, textconv and diff.external".into()
        } else {
            format!("negative control incomplete — not shown to run with plain git: {missing:?}; the hardened result proves nothing for those")
        };
        report.steps = vec!["plain git: add, commit, worktree add, commit, diff".into()];
        return Ok(report);
    }

    let neutralize = variant != Variant::Admission;
    let plan = match g_empty.plan_filters(&fx.repo, &trusted, neutralize) {
        Ok(p) => p,
        Err(e) => {
            report.admission_error = Some(format!("{e:#}"));
            report.markers = list_markers(&fx.markers);
            report.pass = variant == Variant::Admission
                && report
                    .admission_error
                    .as_deref()
                    .unwrap_or("")
                    .contains("evil")
                && report.markers.is_empty();
            report.note =
                "admission must refuse the untrusted `evil` filter without running anything".into();
            return Ok(report);
        }
    };
    if variant == Variant::Admission {
        report.note = "admission unexpectedly accepted an untrusted referenced filter".into();
        return Ok(report);
    }
    steps.push(format!(
        "filter plan: {:?} (untrusted referenced: {:?})",
        plan.drivers.keys().collect::<Vec<_>>(),
        plan.untrusted_referenced
    ));
    let mut g = g_empty.clone();
    g.filters = plan;

    let repo = Loc::Discover(fx.repo.clone());
    g.run(&repo, false, &["add", "-A"])?;
    g.run(&repo, false, &["commit", "-q", "--no-verify", "-m", "base"])?;
    steps.push("add -A; commit (base)".into());
    g.run(
        &repo,
        false,
        &["worktree", "add", "-q", "-b", "toryo/t1", &fwd(&wt), "HEAD"],
    )?;
    steps.push("worktree add -b toryo/t1".into());
    let wtl = Loc::Discover(wt.clone());
    std::fs::write(wt.join("img.bin"), [9u8, 0, 9, 0, 9])?;
    std::fs::write(wt.join("a.txt"), "hello\nworld\n")?;
    g.run(&wtl, false, &["add", "-A"])?;
    g.run(
        &wtl,
        false,
        &[
            "commit",
            "-q",
            "--no-verify",
            "-m",
            "toryo(t1): implement attempt 1",
        ],
    )?;
    steps.push("checkpoint commit in worktree".into());
    let diff = g.canonical_diff(&wtl, "main", Some("HEAD"))?;
    report.diff_has_binary_patch =
        Some(String::from_utf8_lossy(&diff).contains("GIT binary patch"));
    steps.push(format!("canonical diff main..HEAD ({} bytes)", diff.len()));
    let q = g.output(
        &wtl,
        true,
        &["diff", "--quiet", "--no-ext-diff", "--no-textconv", "HEAD"],
    )?;
    steps.push(format!("diff --quiet HEAD → {}", q.status));
    g.run(
        &wtl,
        true,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    steps.push("status --porcelain=v1 -z".into());
    g.run(
        &wtl,
        false,
        &["worktree", "add", "-q", "--detach", &fwd(&vwt), "HEAD"],
    )?;
    let head = g.run_str(&wtl, true, &["rev-parse", "HEAD"])?;
    let admin = fx.repo.join(".git").join("worktrees").join("wt.verify");
    let vloc = Loc::Explicit {
        git_dir: admin,
        work_tree: vwt.clone(),
    };
    g.run(
        &vloc,
        false,
        &["checkout", "-q", "--detach", "--force", &head],
    )?;
    g.run(&vloc, false, &["clean", "-ffdx", "-q"])?;
    g.run(
        &vloc,
        false,
        &["diff", "--quiet", "--no-ext-diff", "--no-textconv", "HEAD"],
    )?;
    steps.push("verify checkout via explicit --git-dir/--work-tree: checkout --force, clean -ffdx, diff --quiet".into());
    let mut show = vec!["show", "HEAD"];
    show.extend_from_slice(CANON_DIFF);
    g.run(&wtl, true, &show)?;
    g.run(&wtl, false, &["reset", "-q", "--soft", "HEAD~1"])?;
    g.run(
        &wtl,
        false,
        &[
            "commit",
            "-q",
            "--no-verify",
            "--allow-empty",
            "-m",
            "toryo(t1): no change",
        ],
    )?;
    steps.push("show (canonical); reset --soft; empty commit".into());

    report.markers = list_markers(&fx.markers);
    let allowed_prefix = if variant == Variant::TrustedLfs {
        "trusted-"
    } else {
        "\u{0}"
    };
    report.unexpected_markers = report
        .markers
        .iter()
        .filter(|m| !m.starts_with(allowed_prefix))
        .cloned()
        .collect();
    if variant == Variant::TrustedLfs && !report.markers.iter().any(|m| m == "trusted-lfs-process")
    {
        report.expected_missing.push("trusted-lfs-process".into());
    }
    report.steps = steps;
    report.pass = report.unexpected_markers.is_empty()
        && report.expected_missing.is_empty()
        && report.diff_has_binary_patch == Some(true);
    report.note = match variant {
        Variant::Neutralized => {
            "no repository program may run; canonical diff must contain a binary patch".into()
        }
        Variant::TrustedLfs => {
            "only the trusted lfs process command may run; the repo's filter.lfs.process must not"
                .into()
        }
        _ => String::new(),
    };
    Ok(report)
}

/// Run all variants; `pass` only if every variant passes (incl. the negative control).
pub fn run_all(
    git: &Path,
    work: &Path,
    trusted_process: Option<&str>,
) -> Result<Vec<CanaryReport>> {
    let mut v = Vec::new();
    for var in [
        Variant::Unhardened,
        Variant::Neutralized,
        Variant::TrustedLfs,
        Variant::Admission,
    ] {
        v.push(
            run(git, work, var, trusted_process)
                .with_context(|| format!("canary variant {var:?}"))?,
        );
    }
    Ok(v)
}
