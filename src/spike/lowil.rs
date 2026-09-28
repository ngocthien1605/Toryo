//! Low-integrity verify boundary canary (plan R36, `verify_boundary_writes`, M0-VERIFY 9).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::fixture;
use super::results::{append, new_run_dir, Record};
use crate::config::{data_dir, home_dir};
use crate::git::Loc;
use crate::process::env::build_env;
use crate::process::redact::Redactions;
use crate::process::{label_low, spawn, Boundary, Chunk, SpawnSpec, WaitOptions};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    pub label: String,
    pub path: PathBuf,
    /// "deny" or "allow"
    pub expect: String,
    /// informational targets do not affect pass/fail
    #[serde(default)]
    pub info: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub label: String,
    pub path: PathBuf,
    pub written: bool,
    pub error: Option<String>,
}

/// Hidden `spike canary-child`: try to create/overwrite each target, report JSON on stdout.
pub fn canary_child(targets_file: &Path) -> Result<()> {
    let targets: Vec<Target> = serde_json::from_slice(&std::fs::read(targets_file)?)?;
    let mut out = Vec::new();
    for t in targets {
        let r = (|| -> std::io::Result<()> {
            if let Some(p) = t.path.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::write(&t.path, b"low-integrity canary\n")
        })();
        out.push(Attempt {
            label: t.label,
            path: t.path,
            written: r.is_ok(),
            error: r.err().map(|e| e.to_string()),
        });
    }
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}

fn pwsh() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in ["pwsh.exe", "pwsh"] {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn run_low(
    program: PathBuf,
    args: Vec<String>,
    cwd: &Path,
    env: BTreeMap<String, String>,
    boundary: Boundary,
) -> Result<(Option<i32>, String, String)> {
    let spec = SpawnSpec {
        program,
        args,
        cwd: cwd.to_path_buf(),
        env,
        stdin: None,
        boundary,
    };
    let r = spawn(&spec, &Redactions::empty())?;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let info = r.wait(
        WaitOptions {
            max: Some(Duration::from_secs(600)),
            ..Default::default()
        },
        |c| match c {
            Chunk::Stdout(b) => out.extend(b),
            Chunk::Stderr(b) => err.extend(b),
        },
    )?;
    Ok((
        info.code,
        crate::process::strip_ansi(&String::from_utf8_lossy(&out)),
        crate::process::strip_ansi(&String::from_utf8_lossy(&err)),
    ))
}

/// The default `verify.verify_root` (R36): under the user profile, where a standard
/// (UAC-filtered) user has Full Control and therefore WRITE_OWNER, which setting a Low
/// label needs. M0 finding F7: folders created directly under `C:\` only give such a user
/// Modify, so labelling there fails with ERROR_ACCESS_DENIED.
pub fn default_verify_root() -> PathBuf {
    PathBuf::from(
        crate::config::expand_vars("%LOCALAPPDATA%\\thien-verify")
            .unwrap_or_else(|_| data_dir().join("verify").display().to_string()),
    )
}

/// `verify_root`: the Low-labelled verify checkout and its build/cache/tmp/home go here
/// (plan §8 / R36), not in the data dir.
pub fn lowil_canary(verify_root: Option<PathBuf>) -> Result<bool> {
    let root = new_run_dir("lowil-canary", "fixture")?;
    let verify_root = verify_root.unwrap_or_else(default_verify_root);
    match lowil_inner(&root, &verify_root) {
        Ok(pass) => Ok(pass),
        Err(e) => {
            // Record the failure too: an M0 spike that errors must still leave evidence.
            let msg = format!("{e:#}");
            let mut rec = Record::new(
                "lowil-canary",
                None,
                &["M0-VERIFY 9", "R36", "verify_boundary_writes (spike)"],
            );
            rec.pass = Some(false);
            rec.summary = format!("error: {msg}");
            rec.details = json!({"error": msg, "verify_root": verify_root});
            rec.run_dir = Some(root.display().to_string());
            append(&rec)?;
            eprintln!("[FAIL] lowil-canary: {msg}");
            Ok(false)
        }
    }
}

fn lowil_inner(root: &Path, verify_root: &Path) -> Result<bool> {
    let root = root.to_path_buf();
    if !cfg!(windows) {
        let mut rec = Record::new("lowil-canary", None, &["M0-VERIFY 9", "R36"]);
        rec.pass = None;
        rec.summary = "not run: the Low integrity boundary exists only on Windows".into();
        append(&rec)?;
        eprintln!("{}", rec.summary);
        return Ok(false);
    }
    // Medium (default-labelled) locations that verify code must not write.
    let repo = root.join("repo");
    fixture::init_repo(&repo)?;
    let g = fixture::gitcmd()?;
    let wt = root.join("wt");
    let vbase = verify_root.join(format!(
        "lowil-{}",
        root.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    ));
    let vwt = vbase.join("wt.verify");
    g.run(
        &Loc::Discover(repo.clone()),
        false,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "thien/lowil",
            &vwt_arg(&wt),
            "HEAD",
        ],
    )?;
    g.run(
        &Loc::Discover(wt.clone()),
        false,
        &["worktree", "add", "-q", "--detach", &vwt_arg(&vwt), "HEAD"],
    )?;
    let run_store = root.join("runstore");
    std::fs::create_dir_all(&run_store)?;
    // Low locations (R36): the verify checkout and its build/cache/tmp/home.
    let vroot = vbase.join("verify-root");
    let (build, cache, tmp, home) = (
        vroot.join("build"),
        vroot.join("cache"),
        vroot.join("tmp"),
        vroot.join("home"),
    );
    label_low(&[
        vwt.clone(),
        build.clone(),
        cache.clone(),
        tmp.clone(),
        home.clone(),
    ])?;

    let pid = std::process::id();
    let real_data = data_dir().join(format!("lowil-probe-{pid}.txt"));
    let real_machine = home_dir()
        .unwrap_or_default()
        .join(".thien")
        .join(format!("lowil-probe-{pid}.txt"));
    let targets = vec![
        Target { label: "run store".into(), path: run_store.join("probe.txt"), expect: "deny".into(), info: false },
        Target { label: "thien data dir (%LOCALAPPDATA%\\thien)".into(), path: real_data.clone(), expect: "deny".into(), info: false },
        Target { label: "machine config dir (%USERPROFILE%\\.thien)".into(), path: real_machine.clone(), expect: "deny".into(), info: false },
        Target { label: "task worktree".into(), path: wt.join("probe.txt"), expect: "deny".into(), info: false },
        Target { label: "repo .git".into(), path: repo.join(".git").join("probe.txt"), expect: "deny".into(), info: false },
        Target { label: "verify admin dir (.git/worktrees/wt.verify)".into(), path: repo.join(".git").join("worktrees").join("wt.verify").join("probe.txt"), expect: "deny".into(), info: false },
        Target { label: "verify checkout .git pointer file (B1: expected writable → thien uses explicit --git-dir)".into(), path: vwt.join(".git"), expect: "allow".into(), info: true },
        Target { label: "build output dir".into(), path: build.join("out.txt"), expect: "allow".into(), info: false },
        Target { label: "TEMP (Low tmp)".into(), path: tmp.join("t.txt"), expect: "allow".into(), info: false },
        Target { label: "verify checkout".into(), path: vwt.join("build").join("x.txt"), expect: "allow".into(), info: false },
    ];
    let targets_file = root.join("targets.json");
    std::fs::write(&targets_file, serde_json::to_vec_pretty(&targets)?)?;
    let mut env = build_env(std::env::vars(), &BTreeMap::new(), &BTreeMap::new());
    for (k, v) in [
        ("TEMP", &tmp),
        ("TMP", &tmp),
        ("NUGET_PACKAGES", &cache),
        ("DOTNET_CLI_HOME", &home),
        ("npm_config_cache", &cache),
        ("XDG_CACHE_HOME", &cache),
        ("APPDATA", &home.join("AppData").join("Roaming")),
        ("LOCALAPPDATA", &home.join("AppData").join("Local")),
        (
            "PSModuleAnalysisCachePath",
            &cache.join("psmodule-analysis"),
        ),
    ] {
        env.insert(k.to_string(), v.display().to_string());
    }
    env.insert("GIT_OPTIONAL_LOCKS".into(), "0".into());
    env.insert("POWERSHELL_TELEMETRY_OPTOUT".into(), "1".into());
    env.insert("POWERSHELL_UPDATECHECK".into(), "Off".into());
    let exe = std::env::current_exe()?;
    let child_args = vec![
        "spike".into(),
        "canary-child".into(),
        "--targets".into(),
        targets_file.display().to_string(),
    ];

    let mut phases = Vec::new();
    for boundary in [Boundary::LowIntegrity, Boundary::None] {
        let (code, out, err) =
            run_low(exe.clone(), child_args.clone(), &vwt, env.clone(), boundary)
                .with_context(|| format!("canary child ({})", boundary.as_str()))?;
        let attempts: Vec<Attempt> = serde_json::from_str(out.trim()).unwrap_or_default();
        let mut rows = Vec::new();
        let mut ok_all = !attempts.is_empty();
        for t in &targets {
            let a = attempts.iter().find(|a| a.label == t.label);
            let written = a.map(|a| a.written).unwrap_or(false);
            let ok = match boundary {
                Boundary::LowIntegrity => (t.expect == "allow") == written,
                Boundary::None => written, // negative control: everything writable without the boundary
            };
            if !t.info && !ok {
                ok_all = false;
            }
            rows.push(json!({"target": t.label, "expect": if boundary == Boundary::None { "allow" } else { &t.expect }, "written": written, "ok": ok, "info": t.info, "error": a.and_then(|a| a.error.clone())}));
        }
        // clean up probes in real locations
        for p in [&real_data, &real_machine] {
            let _ = std::fs::remove_file(p);
        }
        phases.push(json!({"boundary": boundary.as_str(), "exit": code, "ok": ok_all, "rows": rows, "stderr": err}));
    }

    // Phase 2: the fixture build + tests must succeed at Low IL (M0-VERIFY 9).
    let build_result = match pwsh() {
        None => json!({"ok": false, "error": "pwsh not found in PATH"}),
        Some(ps) => {
            let mut steps = Vec::new();
            let mut ok = true;
            for script in ["scripts/build.ps1", "scripts/test.ps1"] {
                let (code, out, err) = run_low(
                    ps.clone(),
                    vec![
                        "-NoProfile".into(),
                        "-NonInteractive".into(),
                        "-File".into(),
                        vwt.join(script).display().to_string(),
                    ],
                    &vwt,
                    env.clone(),
                    Boundary::LowIntegrity,
                )?;
                ok &= code == Some(0);
                steps.push(json!({"script": script, "exit": code, "stdout": out, "stderr": err}));
            }
            // After the run: the orchestrator's own check through explicit --git-dir (B1).
            let vloc = Loc::Explicit {
                git_dir: repo.join(".git").join("worktrees").join("wt.verify"),
                work_tree: vwt.clone(),
            };
            let clean = g
                .output(
                    &vloc,
                    true,
                    &["diff", "--quiet", "--no-ext-diff", "--no-textconv", "HEAD"],
                )?
                .status
                .success();
            json!({"ok": ok && clean, "steps": steps, "tracked_tree_unchanged": clean})
        }
    };

    let low_ok = phases[0]["ok"].as_bool() == Some(true);
    let control_ok = phases[1]["ok"].as_bool() == Some(true);
    let build_ok = build_result["ok"].as_bool() == Some(true);
    let pass = low_ok && control_ok && build_ok;
    for ph in &phases {
        eprintln!("── boundary = {} ──", ph["boundary"].as_str().unwrap_or(""));
        for r in ph["rows"].as_array().into_iter().flatten() {
            eprintln!(
                "[{}] {:<70} written={} {}",
                if r["ok"].as_bool() == Some(true) {
                    "PASS"
                } else if r["info"].as_bool() == Some(true) {
                    "info"
                } else {
                    "FAIL"
                },
                r["target"].as_str().unwrap_or(""),
                r["written"],
                r["error"].as_str().unwrap_or("")
            );
        }
    }
    eprintln!(
        "[{}] fixture build + tests at Low integrity",
        if build_ok { "PASS" } else { "FAIL" }
    );
    let mut rec = Record::new(
        "lowil-canary",
        None,
        &["M0-VERIFY 9", "R36", "verify_boundary_writes (spike)"],
    );
    rec.pass = Some(pass);
    rec.summary = format!("low boundary writes as expected={low_ok}; negative control={control_ok}; build+tests at Low={build_ok}");
    rec.details = json!({"phases": phases, "build": build_result, "verify_root": verify_root});
    rec.run_dir = Some(root.display().to_string());
    append(&rec)?;
    Ok(pass)
}

fn vwt_arg(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}
