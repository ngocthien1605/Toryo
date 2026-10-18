//! M16 acceptance tests with a real console (M16-AT): Ctrl+C on `toryo run A B C --jobs 2`.
//!
//! `toryo` runs inside a pseudo-console (ConPTY, `expectrl`); Ctrl+C is the byte `0x03` written
//! to the console's input, which the console turns into the CTRL_C_EVENT every process attached
//! to it receives — the parent and each `toryo run <id>` child — exactly as a key press does.
//! Nothing here calls toryo's cancel path directly, and no test hook is set.
//!
//! Slow (a minute or two each) and Windows-only: `#[ignore]`, run by `scripts/test-e2e.ps1` as
//! `cargo test --test e2e_m16 -- --ignored --test-threads=1`. Each test builds its own workspace
//! with `tests/e2e/New-M16Fixture.ps1` in a fresh temp folder, removed when the test passed and
//! kept (path printed, and appended to `test-results/kept-dirs.txt`) when it failed.
#![cfg(all(windows, feature = "m0-kit"))]

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use expectrl::session::OsSession;
use expectrl::{ControlCode, Expect, Session};
use serde_json::Value;

/// How long the whole run may take to end after the one Ctrl+C (AT-5).
const EXIT_AFTER_CTRL_C: Duration = Duration::from_secs(15);

/// A workspace of `New-M16Fixture.ps1` (its `m16.json`), removed on drop unless kept.
struct Ws {
    root: PathBuf,
    data: PathBuf,
    config: String,
    machine: String,
    env: Vec<(String, String)>,
    fakes: Value,
    at: &'static str,
    passed: bool,
}

impl Ws {
    fn new(at: &'static str) -> Ws {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "toryo-m16-{}-{}-{stamp}",
            at.to_lowercase(),
            std::process::id()
        ));
        let script = repo().join("tests").join("e2e").join("New-M16Fixture.ps1");
        let o = Command::new("pwsh")
            .args(["-NoProfile", "-NonInteractive", "-File"])
            .arg(&script)
            .arg("-Root")
            .arg(&root)
            .arg("-Toryo")
            .arg(env!("CARGO_BIN_EXE_toryo"))
            .output()
            .expect("pwsh");
        assert!(
            o.status.success(),
            "New-M16Fixture.ps1 failed ({}):\n{}{}",
            root.display(),
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        let info: Value =
            serde_json::from_slice(&std::fs::read(root.join("m16.json")).unwrap()).unwrap();
        let s = |k: &str| info[k].as_str().unwrap().to_string();
        let env = info["env"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
            .collect();
        Ws {
            data: PathBuf::from(s("data")),
            config: s("config"),
            machine: s("machine"),
            env,
            fakes: info,
            root,
            at,
            passed: false,
        }
    }

    fn fake(&self, which: &str) -> String {
        self.fakes[which].as_str().unwrap().to_string()
    }

    /// `toryo` with this workspace's environment, config and machine config (no console).
    fn toryo(&self, args: &[&str]) -> (i32, String) {
        let o = self
            .command(args)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stderr).into_owned() + &String::from_utf8_lossy(&o.stdout),
        )
    }

    /// The global options every toryo call of a test gets.
    fn global_args(&self) -> Vec<String> {
        [
            "--color",
            "never",
            "--config",
            &self.config,
            "--machine-config",
            &self.machine,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_toryo"));
        c.envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .current_dir(&self.root)
            .args(self.global_args())
            .args(args);
        c
    }

    fn events(&self, id: &str) -> Vec<Value> {
        std::fs::read_to_string(self.data.join("runs").join(id).join("events.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn has(&self, id: &str, f: impl Fn(&Value) -> bool) -> bool {
        self.events(id).iter().any(f)
    }
}

impl Drop for Ws {
    fn drop(&mut self) {
        if self.passed {
            let _ = std::fs::remove_dir_all(&self.root);
            return;
        }
        eprintln!("KEPT {} ({} failed)", self.root.display(), self.at);
        let results = repo().join("test-results");
        let _ = std::fs::create_dir_all(&results);
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(results.join("kept-dirs.txt"))
        {
            let _ = writeln!(f, "{}\t{}", self.at, self.root.display());
        }
    }
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `toryo` in a pseudo-console (`expectrl` over ConPTY), its output drained at every poll (an
/// undrained ConPTY blocks the program that writes to it).
struct Pty {
    s: OsSession,
    out: Vec<u8>,
    exit: Option<u32>,
    started: Instant,
    command: String,
}

impl Pty {
    fn spawn(ws: &Ws, args: &[&str]) -> Pty {
        let mut cmd = ws.command(args);
        // the pseudo-console gives a child only the variables set on the command
        cmd.envs(std::env::vars())
            .envs(ws.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let command = format!("toryo {}", args.join(" "));
        let mut s = spawn_on_the_console(cmd);
        // wide: ConPTY wraps long lines at the console width
        s.get_process_mut().resize(400, 60).expect("resize");
        Pty {
            s,
            out: Vec::new(),
            exit: None,
            started: Instant::now(),
            command,
        }
    }

    fn drain(&mut self) {
        let mut buf = [0u8; 16384];
        while let Ok(n) = self.s.try_read(&mut buf) {
            if n == 0 {
                break;
            }
            self.out.extend_from_slice(&buf[..n]);
        }
    }

    /// The console text so far, without the console's own escape sequences.
    fn text(&self) -> String {
        toryo::process::strip_ansi(&String::from_utf8_lossy(&self.out)).replace('\r', "")
    }

    fn alive(&mut self) -> bool {
        self.drain();
        if self.exit.is_some() {
            return false;
        }
        if self.s.get_process().is_alive() {
            return true;
        }
        self.exit = self.s.get_process().wait(Some(2000)).ok();
        false
    }

    /// What a failure prints: the command, how long it ran, the last 80 lines, the root.
    fn context(&self, ws: &Ws) -> String {
        let text = self.text();
        let lines: Vec<&str> = text.lines().collect();
        format!(
            "\n--- {} ({:?} after start), root {}\n{}",
            self.command,
            self.started.elapsed(),
            ws.root.display(),
            lines[lines.len().saturating_sub(80)..].join("\n")
        )
    }

    /// Drain until `f` holds; a timeout fails with the output.
    fn wait_for(&mut self, ws: &Ws, what: &str, max: Duration, mut f: impl FnMut(&Ws) -> bool) {
        let end = Instant::now() + max;
        loop {
            if f(ws) {
                return;
            }
            assert!(
                self.alive(),
                "toryo ended while waiting for {what}{}",
                self.context(ws)
            );
            assert!(
                Instant::now() < end,
                "timed out waiting for {what}{}",
                self.context(ws)
            );
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    /// Ctrl+C on the console: the byte 0x03 (ETX) on its input.
    fn ctrl_c(&mut self) {
        self.s
            .send(ControlCode::EndOfText)
            .expect("write 0x03 to the console");
    }

    /// Drain until the process ends; `None` when it did not end within `max`.
    fn exit_within(&mut self, max: Duration) -> Option<(u32, Duration)> {
        let t0 = Instant::now();
        while self.alive() {
            if t0.elapsed() > max {
                return None;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        let took = t0.elapsed();
        // what the console still shows of the end
        std::thread::sleep(Duration::from_millis(300));
        self.drain();
        Some((self.exit.expect("exit code"), took))
    }

    /// Never left running when a test failed: the whole tree.
    fn kill_tree(&mut self) {
        let pid = self.s.get_process().pid();
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .output();
    }
}

/// Start `cmd` attached to a new pseudo-console. `conpty` sets no `STARTF_USESTDHANDLES`, so
/// the child would take this process's standard handles — the test runner's pipes — and write
/// there instead of to the console; with none for that moment, Windows opens the console for it.
/// (One test at a time: `--test-threads=1`.)
fn spawn_on_the_console(cmd: Command) -> OsSession {
    use windows_sys::Win32::System::Console::{
        GetStdHandle, SetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    let kinds = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];
    // SAFETY: only this process's standard handle slots, put back right after the spawn
    unsafe {
        let saved: Vec<_> = kinds.iter().map(|k| GetStdHandle(*k)).collect();
        for k in kinds {
            SetStdHandle(k, std::ptr::null_mut());
        }
        let s = Session::spawn(cmd);
        for (k, h) in kinds.iter().zip(saved) {
            SetStdHandle(*k, h);
        }
        s.expect("spawn in a pseudo-console")
    }
}

/// What a Ctrl+C scenario leaves: the workspace, the run's exit code, how long it took to end
/// after the key, and its console text.
struct Stopped {
    ws: Ws,
    code: u32,
    took: Duration,
    text: String,
    context: String,
}

/// `toryo run INT-A INT-B INT-C --jobs 2 --yes --fake-agents <fake>` in a console; once INT-A
/// and INT-B both run their `stage` (its agent started), one Ctrl+C; then the run must end by
/// itself — never a second key.
fn ctrl_c_during(at: &'static str, fake: &str, stage: &'static str) -> Stopped {
    let ws = Ws::new(at);
    let fake = ws.fake(fake);
    let mut pty = Pty::spawn(
        &ws,
        &[
            "run",
            "INT-A",
            "INT-B",
            "INT-C",
            "--jobs",
            "2",
            "--yes",
            "--fake-agents",
            &fake,
        ],
    );
    let running =
        |ws: &Ws, id: &str| ws.has(id, |e| e["type"] == "agent_spawned" && e["stage"] == stage);
    pty.wait_for(
        &ws,
        &format!("INT-A and INT-B in their {stage} stage"),
        Duration::from_secs(120),
        |ws| running(ws, "INT-A") && running(ws, "INT-B"),
    );
    pty.ctrl_c();
    let Some((code, took)) = pty.exit_within(EXIT_AFTER_CTRL_C) else {
        let ctx = pty.context(&ws);
        pty.kill_tree();
        panic!("toryo did not end within {EXIT_AFTER_CTRL_C:?} of one Ctrl+C{ctx}");
    };
    Stopped {
        code,
        took,
        text: pty.text(),
        context: pty.context(&ws),
        ws,
    }
}

/// The end block line of a task: `INT-A  Implementing           (exit 0) — next: …`.
fn end_line<'a>(text: &'a str, id: &str) -> Option<&'a str> {
    let re = regex::Regex::new(&format!(r"^{id}\s{{2,}}[A-Za-z]+\s+\(")).unwrap();
    text.lines().rev().find(|l| re.is_match(l))
}

/// The guarantees of a run stopped by one Ctrl+C (AT-5, AT-9): it ended by itself in time, with
/// exit code 1 (toryo's parent: a queued task was not started — each stopped child exits 0);
/// INT-A and INT-B were stopped by the console's Ctrl+C and record it; INT-C never started.
fn assert_stopped_cleanly(s: &Stopped, stage: &str, state: &str) {
    let ctx = &s.context;
    assert!(s.took < EXIT_AFTER_CTRL_C, "{:?}{ctx}", s.took);
    assert_eq!(
        s.code, 1,
        "the exit code of `run A B C` stopped by Ctrl+C{ctx}"
    );
    assert!(
        s.text
            .contains("■ Ctrl+C — the running tasks stop; not started: INT-C"),
        "{ctx}"
    );
    for id in ["INT-A", "INT-B"] {
        let line = end_line(&s.text, id).unwrap_or_else(|| panic!("{id} end line{ctx}"));
        assert!(
            line.starts_with(&format!("{id}  {state}")) && line.contains("(exit 0)"),
            "{id}: [{line}]{ctx}"
        );
        assert!(
            s.text.contains(&format!("{id} │ stop requested — ")),
            "{id} saw the key{ctx}"
        );
        let ev = s.ws.events(id);
        let cancel = ev
            .iter()
            .position(|e| e["type"] == "cancel_requested")
            .unwrap_or_else(|| panic!("{id}: no cancel_requested{ctx}"));
        assert_eq!(ev[cancel]["source"], "ctrl_c", "{id}{ctx}");
        assert!(
            ev.iter().any(|e| e["type"] == "agent_killed"
                && e["stage"] == stage
                && e["reason"] == "stop"),
            "{id}: the {stage} agent was ended{ctx}"
        );
        let end = ev.last().unwrap();
        assert_eq!(end["type"], "run_ended", "{id}{ctx}");
        assert_eq!(
            (end["outcome"].as_str(), end["exit_code"].as_i64()),
            (Some("stopped"), Some(0)),
            "{id}: {end}{ctx}"
        );
        assert!(cancel < ev.len() - 1);
    }
    // INT-C was queued, never started: no run of its own, no line of its own
    assert!(
        !s.ws.has("INT-C", |e| e["type"] == "run_started"),
        "INT-C started{ctx}"
    );
    assert!(
        !s.text.lines().any(|l| l.starts_with("INT-C │")),
        "INT-C printed a run line{ctx}"
    );
    let c = end_line(&s.text, "INT-C").unwrap_or_else(|| panic!("INT-C end line{ctx}"));
    assert!(c.contains("(not started)"), "[{c}]{ctx}");
}

/// Is the process `pid` still running?
fn running(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: a query handle, closed below; a pid that no longer exists gives a null handle
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        ok && code == STILL_ACTIVE as u32
    }
}

/// AT-6 / AT-9: nothing of the run is left — no process it recorded (each child's
/// `run_started.pid`, every agent pid; a scripted agent has none), no lock that would refuse the
/// next run (each task lock and the verify lock can be taken), no Git lock in a worktree.
fn assert_nothing_left(s: &Stopped) {
    let ctx = &s.context;
    let mut pids = Vec::new();
    for id in ["INT-A", "INT-B"] {
        for e in s.ws.events(id) {
            if (e["type"] == "run_started" || e["type"] == "agent_spawned") && e["pid"].is_u64() {
                pids.push((
                    id,
                    e["type"].as_str().unwrap().to_string(),
                    e["pid"].as_u64().unwrap() as u32,
                ));
            }
        }
    }
    assert!(
        pids.iter().filter(|p| p.1 == "run_started").count() == 2,
        "{pids:?}{ctx}"
    );
    // a moment for the OS to reap what just ended
    let end = Instant::now() + Duration::from_secs(5);
    while pids.iter().any(|p| running(p.2)) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
    }
    for (id, kind, pid) in &pids {
        assert!(
            !running(*pid),
            "{id}: the process of its {kind} (pid {pid}) is still running{ctx}"
        );
    }
    let locks = s.ws.data.join("locks");
    for f in [
        locks.join("INT-A.lock"),
        locks.join("INT-B.lock"),
        locks.join("machine").join("verify.lock"),
    ] {
        if !f.exists() {
            continue;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&f)
            .unwrap();
        file.try_lock()
            .unwrap_or_else(|e| panic!("{} is still held: {e}{ctx}", f.display()));
        file.unlock().unwrap();
    }
    let admin = s.ws.root.join("repo").join(".git").join("worktrees");
    for id in ["INT-A", "INT-B"] {
        let lock = admin.join(id).join("index.lock");
        assert!(!lock.exists(), "{} was left{ctx}", lock.display());
        // resumable: the store says how to go on
        let (code, out) = s.ws.toryo(&["status", id]);
        assert_eq!(code, 0, "{out}");
        assert!(
            out.contains(&format!("`toryo run {id}`"))
                || out.contains(&format!("`toryo resume {id}`")),
            "{id}: {out}{ctx}"
        );
    }
}

#[test]
#[ignore = "slow, needs a pseudo-console: scripts/test-e2e.ps1"]
fn at5_one_ctrl_c_during_implement_stops_the_running_tasks_and_starts_no_other() {
    let mut s = ctrl_c_during("AT-5", "fake_slow", "implement");
    assert_stopped_cleanly(&s, "implement", "Implementing");
    s.ws.passed = true;
}

#[test]
#[ignore = "slow, needs a pseudo-console: scripts/test-e2e.ps1"]
fn at6_after_the_ctrl_c_no_process_and_no_lock_of_the_run_is_left() {
    let mut s = ctrl_c_during("AT-6", "fake_slow", "implement");
    assert_nothing_left(&s);
    s.ws.passed = true;
}

#[test]
#[ignore = "slow, needs a pseudo-console: scripts/test-e2e.ps1"]
fn at7_a_task_stopped_by_ctrl_c_resumes_to_completed_without_planning_again() {
    let mut s = ctrl_c_during("AT-7", "fake_slow", "implement");
    let ctx = s.context.clone();
    let before = s.ws.events("INT-A").len();
    let full = s.ws.fake("fake_full");
    let (code, out) =
        s.ws.toryo(&["resume", "INT-A", "--yes", "--fake-agents", &full]);
    assert_eq!(code, 0, "{out}{ctx}");
    assert!(
        out.contains("══════════ INT-A: Completed ══════════"),
        "{out}"
    );
    let ev = s.ws.events("INT-A");
    let resumed = &ev[before..];
    assert_eq!(resumed[0]["type"], "run_started", "{resumed:#?}");
    assert!(
        !resumed
            .iter()
            .any(|e| e["type"] == "stage_started" && e["stage"] == "plan"),
        "the plan, approved before the Ctrl+C, is not made again: {resumed:#?}"
    );
    assert!(
        resumed.iter().any(|e| e["type"] == "stage_started"
            && e["stage"] == "implement"
            && e["attempt"] == 2),
        "the interrupted implement attempt is followed by attempt 2: {resumed:#?}"
    );
    let end = resumed.last().unwrap();
    assert_eq!(
        (
            end["state"].as_str(),
            end["outcome"].as_str(),
            end["exit_code"].as_i64()
        ),
        (Some("Completed"), Some("done"), Some(0)),
        "{end}"
    );
    s.ws.passed = true;
}

#[test]
#[ignore = "slow, needs a pseudo-console: scripts/test-e2e.ps1"]
fn at9_one_ctrl_c_during_plan_is_as_clean_and_nothing_reaches_implement() {
    let mut s = ctrl_c_during("AT-9", "fake_slow_plan", "plan");
    assert_stopped_cleanly(&s, "plan", "Planning");
    assert_nothing_left(&s);
    for id in ["INT-A", "INT-B"] {
        assert!(
            !s.ws.has(id, |e| e["type"] == "stage_started"
                && e["stage"] == "implement"),
            "{id} reached implement{}",
            s.context
        );
    }
    s.ws.passed = true;
}
