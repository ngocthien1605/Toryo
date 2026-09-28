//! Job Object tests (plan §13): stop one stage, crash of toryo.exe, assign-failure injection.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::json;

use crate::evidence::{append, new_run_dir, Record};
use crate::process::env::build_env;
use crate::process::redact::Redactions;
use crate::process::{is_alive, kill_pid, spawn, Boundary, ProcId, SpawnSpec};

fn self_exe() -> Result<PathBuf> {
    std::env::current_exe().context("current_exe")
}

fn child_spec(args: Vec<String>, cwd: &Path) -> Result<SpawnSpec> {
    Ok(SpawnSpec {
        program: self_exe()?,
        args,
        cwd: cwd.to_path_buf(),
        env: build_env(std::env::vars(), &BTreeMap::new(), &BTreeMap::new()),
        stdin: None,
        boundary: Boundary::None,
    })
}

/// Hidden `spike job-child`: optional marker, spawn a child of depth-1, sleep.
/// The leaf (depth 0) writes `ready` so the tester snapshots only a complete tree.
pub fn job_child(
    depth: u32,
    sleep_secs: u64,
    marker: Option<PathBuf>,
    ready: Option<PathBuf>,
) -> Result<()> {
    if let Some(m) = marker {
        std::fs::write(m, format!("{}", std::process::id()))?;
    }
    if depth == 0 {
        if let Some(r) = &ready {
            std::fs::write(r, format!("{}", std::process::id()))?;
        }
    }
    let mut kids = Vec::new();
    if depth > 0 {
        let mut c = std::process::Command::new(self_exe()?);
        c.args([
            "spike",
            "job-child",
            "--depth",
            &(depth - 1).to_string(),
            "--sleep",
            &sleep_secs.to_string(),
        ]);
        if let Some(r) = &ready {
            c.arg("--ready").arg(r);
        }
        c.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        kids.push(c.spawn()?);
    }
    std::thread::sleep(Duration::from_secs(sleep_secs));
    for mut k in kids {
        let _ = k.kill();
    }
    Ok(())
}

/// Hidden `spike job-supervisor`: plays toryo.exe — owns a Job with a 3-level tree,
/// writes the owned (pid, created) set, then waits to be killed from outside.
pub fn job_supervisor(snapshot: &Path) -> Result<()> {
    let ready = snapshot.with_extension("ready");
    let spec = child_spec(tree_args(&ready), &std::env::temp_dir())?;
    let running = spawn(&spec, &Redactions::empty())?;
    let ids = wait_for_tree(&running.job, &ready, Duration::from_secs(15))?;
    std::fs::write(snapshot, serde_json::to_vec(&ids)?)?;
    std::thread::sleep(Duration::from_secs(300));
    drop(running);
    Ok(())
}

fn tree_args(ready: &Path) -> Vec<String> {
    vec![
        "spike".into(),
        "job-child".into(),
        "--depth".into(),
        "2".into(),
        "--sleep".into(),
        "300".into(),
        "--ready".into(),
        ready.display().to_string(),
    ]
}

/// Wait until the leaf of the 3-level tree has written `ready`, then snapshot the Job.
fn wait_for_tree(job: &crate::process::Job, ready: &Path, max: Duration) -> Result<Vec<ProcId>> {
    let deadline = Instant::now() + max;
    while !ready.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    job.process_ids()
}

fn all_dead(ids: &[ProcId], max: Duration) -> Vec<ProcId> {
    let deadline = Instant::now() + max;
    loop {
        let alive: Vec<ProcId> = ids.iter().copied().filter(|p| is_alive(*p)).collect();
        if alive.is_empty() || Instant::now() > deadline {
            return alive;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub fn job_test() -> Result<bool> {
    let root = new_run_dir("job-test", "all")?;
    let mut results = Vec::new();

    // 1. stop: TerminateJobObject for one stage while toryo keeps running; a second stage is untouched.
    {
        let (ready_a, ready_b) = (root.join("a.ready"), root.join("b.ready"));
        let a = spawn(
            &child_spec(tree_args(&ready_a), &root)?,
            &Redactions::empty(),
        )?;
        let b = spawn(
            &child_spec(tree_args(&ready_b), &root)?,
            &Redactions::empty(),
        )?;
        let ids_a = wait_for_tree(&a.job, &ready_a, Duration::from_secs(15))?;
        let ids_b = wait_for_tree(&b.job, &ready_b, Duration::from_secs(15))?;
        a.job.terminate(1)?;
        let gone = a.job.wait_empty(Duration::from_secs(5))?;
        let alive_a = all_dead(&ids_a, Duration::from_secs(5));
        let b_untouched = ids_b.iter().all(|p| is_alive(*p));
        b.job.terminate(1)?;
        let _ = b.job.wait_empty(Duration::from_secs(5));
        let ok = ids_a.len() >= 3 && gone && alive_a.is_empty() && b_untouched;
        results.push(json!({"test": "stop one stage (TerminateJobObject → ActiveProcesses == 0), other stage untouched",
            "ok": ok, "owned": ids_a.len(), "active_zero": gone, "still_alive": alive_a, "other_stage_untouched": b_untouched}));
    }

    // 2. crash: kill the supervisor (toryo.exe stand-in) from outside; KILL_ON_JOB_CLOSE must take the tree down.
    {
        let snap = root.join("supervisor-snapshot.json");
        let mut sup = std::process::Command::new(self_exe()?)
            .args(["spike", "job-supervisor", "--snapshot"])
            .arg(&snap)
            .stdin(std::process::Stdio::null())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(20);
        while !snap.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        std::thread::sleep(Duration::from_millis(200));
        let ids: Vec<ProcId> =
            serde_json::from_slice(&std::fs::read(&snap).unwrap_or_default()).unwrap_or_default();
        kill_pid(sup.id())?; // like Stop-Process -Force
        let _ = sup.wait();
        let alive = all_dead(&ids, Duration::from_secs(5));
        let ok = ids.len() >= 3 && alive.is_empty();
        let note = if cfg!(windows) {
            "Windows: Job handle closes with the process → KILL_ON_JOB_CLOSE"
        } else {
            "non-Windows: process groups have no kill-on-close; expected to FAIL here (Windows-only guarantee)"
        };
        results.push(json!({"test": "toryo.exe killed from outside → every owned (pid, creation time) is gone",
            "ok": ok, "owned": ids.len(), "still_alive": alive, "note": note}));
        // Clean up leftovers on non-Windows hosts.
        for p in alive {
            let _ = kill_pid(p.pid);
        }
    }

    // 3. fault injection: assignment fails → the child never runs a user instruction.
    {
        let marker = root.join("fault-marker.txt");
        std::env::set_var("TORYO_FAULT_ASSIGN", "1");
        let spec = child_spec(
            vec![
                "spike".into(),
                "job-child".into(),
                "--depth".into(),
                "0".into(),
                "--sleep".into(),
                "1".into(),
                "--marker".into(),
                marker.display().to_string(),
            ],
            &root,
        )?;
        let r = spawn(&spec, &Redactions::empty());
        std::env::remove_var("TORYO_FAULT_ASSIGN");
        std::thread::sleep(Duration::from_millis(1500));
        let ok = r.is_err() && !marker.exists();
        results.push(json!({"test": "AssignProcessToJobObject failure → child terminated while suspended (no marker written)",
            "ok": ok, "spawn_error": r.err().map(|e| format!("{e:#}")), "marker_exists": marker.exists()}));
    }

    let pass = results.iter().all(|r| r["ok"].as_bool() == Some(true));
    for r in &results {
        eprintln!(
            "[{}] {}",
            if r["ok"].as_bool() == Some(true) {
                "PASS"
            } else {
                "FAIL"
            },
            r["test"].as_str().unwrap_or("")
        );
    }
    let mut rec = Record::new("job-test", None, &["R23", "§13 Job Object test"]);
    rec.pass = Some(pass);
    rec.bind_versions(None, &[]); // R51
    rec.summary = results
        .iter()
        .map(|r| {
            format!(
                "{}={}",
                r["test"]
                    .as_str()
                    .unwrap_or("")
                    .split(' ')
                    .next()
                    .unwrap_or(""),
                r["ok"]
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    rec.details = json!({ "results": results });
    rec.run_dir = Some(root.display().to_string());
    append(&rec)?;
    Ok(pass)
}
