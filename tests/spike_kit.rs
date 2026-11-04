//! The hidden helpers of the M0 kit that the canaries start as child processes —
//! `spike canary-child` (the process the Low-integrity canary runs at Low IL, `spike/lowil.rs`)
//! and `spike job-child` (the process tree the Job Object test owns, `spike/job.rs`) — and
//! `spike lowil-canary` where the Low integrity boundary does not exist. Each is driven through
//! the `toryo` binary the way the canary drives it: arguments in, files and JSON out.
#![cfg(feature = "m0-kit")]

use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

/// `toryo spike …` with its own data folder; never a config of the developer's.
fn spike(data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_toryo"))
        .arg("spike")
        .args(args)
        .env("TORYO_DATA_DIR", data)
        .env_remove("TORYO_CONFIG")
        .current_dir(data.parent().unwrap())
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// `canary-child` tries each target and reports, per target, whether it was written — a
/// writable target (its folders made on the way) holds the canary text afterwards, an
/// unwritable one (its parent is a file) is `written: false` with the error, and the report
/// keeps the order and the labels of the targets file.
#[test]
fn canary_child_reports_each_target_it_could_and_could_not_write() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let ok = tmp.path().join("deep").join("er").join("ok.txt");
    let blocker = tmp.path().join("a-file");
    std::fs::write(&blocker, "not a folder").unwrap();
    let denied = blocker.join("x.txt");
    let targets = tmp.path().join("targets.json");
    std::fs::write(
        &targets,
        serde_json::to_vec(&serde_json::json!([
            { "label": "writable", "path": ok, "expect": "allow" },
            { "label": "under-a-file", "path": denied, "expect": "deny", "info": true },
        ]))
        .unwrap(),
    )
    .unwrap();
    let o = spike(
        &data,
        &["canary-child", "--targets", targets.to_str().unwrap()],
    );
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{stderr}");
    let report: Vec<Value> = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(report.len(), 2, "{report:?}");
    assert_eq!(report[0]["label"], "writable");
    assert_eq!(report[0]["written"], true);
    assert!(report[0]["error"].is_null(), "{report:?}");
    assert_eq!(
        std::fs::read_to_string(&ok).unwrap(),
        "low-integrity canary\n"
    );
    assert_eq!(report[1]["label"], "under-a-file");
    assert_eq!(report[1]["written"], false);
    assert!(
        report[1]["error"].as_str().is_some_and(|e| !e.is_empty()),
        "{report:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&blocker).unwrap(),
        "not a folder",
        "a failed target leaves what stood there"
    );
}

/// A targets file that is not the canary's JSON is an error of the child (exit non-zero), not
/// an empty report the canary would read as "nothing was written".
#[test]
fn canary_child_refuses_a_targets_file_it_cannot_read() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let targets = tmp.path().join("targets.json");
    std::fs::write(&targets, "{ not json").unwrap();
    let o = spike(
        &data,
        &["canary-child", "--targets", targets.to_str().unwrap()],
    );
    assert_ne!(o.status.code(), Some(0));
    assert!(
        o.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
    let missing = tmp.path().join("missing.json");
    let o = spike(
        &data,
        &["canary-child", "--targets", missing.to_str().unwrap()],
    );
    assert_ne!(o.status.code(), Some(0));
}

/// Off Windows there is no Low integrity boundary: `spike lowil-canary` does not pass (exit 1)
/// and leaves an evidence record that says it was not run — `pass` empty, never `true`, so no
/// doctor can read it as a verified boundary.
#[cfg(not(windows))]
#[test]
fn the_lowil_canary_off_windows_records_that_it_did_not_run() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let o = spike(&data, &["lowil-canary"]);
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("not run: the Low integrity boundary exists only on Windows"),
        "{stderr}"
    );
    let results = std::fs::read_to_string(data.join("m0").join("results.jsonl")).unwrap();
    let recs: Vec<Value> = results
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(recs.len(), 1, "{results}");
    let r = &recs[0];
    assert_eq!(r["spike"], "lowil-canary");
    assert!(r["pass"].is_null(), "{r}");
    assert!(
        r["summary"]
            .as_str()
            .unwrap()
            .starts_with("not run: the Low integrity boundary exists only on Windows"),
        "{r}"
    );
    assert!(r["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i == "M0-VERIFY 9"));
    // nothing was made under the verify root (the default, without %LOCALAPPDATA%: <data>/verify):
    // the canary stopped before its fixture
    assert!(!data.join("verify").exists());
}

/// `job-child` at depth 0 is the leaf of the Job test's tree: it writes its pid to the marker
/// and to the ready file (the tester snapshots the tree only then) and ends after its sleep.
#[test]
fn a_job_child_leaf_writes_its_pid_to_the_marker_and_the_ready_file() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let marker = tmp.path().join("leaf.pid");
    let ready = tmp.path().join("leaf.ready");
    let o = spike(
        &data,
        &[
            "job-child",
            "--depth",
            "0",
            "--sleep",
            "0",
            "--marker",
            marker.to_str().unwrap(),
            "--ready",
            ready.to_str().unwrap(),
        ],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let pid: u32 = std::fs::read_to_string(&marker).unwrap().parse().unwrap();
    assert!(pid > 0);
    assert_eq!(
        std::fs::read_to_string(&ready).unwrap(),
        pid.to_string(),
        "the leaf writes the ready file itself"
    );
}

/// Above depth 0 the child starts the next level and only the leaf writes the ready file: the
/// marker holds the parent's pid, the ready file another process's — so a tester waiting for
/// `ready` sees the whole tree. The parent ends its child when its own sleep is over.
#[test]
fn a_job_child_above_the_leaf_starts_the_next_level_and_only_the_leaf_is_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let marker = tmp.path().join("top.pid");
    let ready = tmp.path().join("tree.ready");
    let o = spike(
        &data,
        &[
            "job-child",
            "--depth",
            "1",
            "--sleep",
            "2",
            "--marker",
            marker.to_str().unwrap(),
            "--ready",
            ready.to_str().unwrap(),
        ],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let top: u32 = std::fs::read_to_string(&marker).unwrap().parse().unwrap();
    let leaf: u32 = std::fs::read_to_string(&ready)
        .expect("the leaf of the tree wrote the ready file")
        .parse()
        .unwrap();
    assert_ne!(top, leaf, "the ready file is the leaf's, not the parent's");
}
