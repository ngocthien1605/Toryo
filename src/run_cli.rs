//! `toryo run A B C [--jobs N]` (M16-02): several tasks at once.
//!
//! One id is the `run` of before, in this process. Several ids each get a `toryo run <id>` child
//! process — the same binary, the same `--config` / `--machine-config` and the same drive flags —
//! at most `--jobs` at a time, the rest queued in the order given (X37). A child is exactly the
//! `run` of one task: it takes that task's lock (a task another process drives is refused with
//! `task_active`, exit 3, and the others go on), recovers, drives, ends with `run_ended`, and its
//! Ctrl+C handling (X29) is the one of every `run`. No engine state is shared between two tasks
//! in one process. What two tasks share on the machine is guarded where it is shared: the store
//! (SQLite, WAL, busy timeout), an environment gate's lock, and the verify lock (X36: one task at
//! a time runs its check gates).
//!
//! The parent prints every line of a child with the task's id in front, then one end block with
//! each task's state and next action, and exits with the highest exit code of its children.
//! Ctrl+C reaches every child through the console (each stops its task, X29); the parent starts
//! no queued task after it and waits for the running ones to end.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use toryo::engine::runner::next_action;
use toryo::engine::store::{self, Store};
use toryo::engine::task;

use crate::engine_cli::{interrupted, watch_interrupt, DriveArgs};
use crate::Cli;

/// `--jobs` without a value: as many as there are ids, at most this.
pub const DEFAULT_JOBS: usize = 4;
/// The most `--jobs` allows (the agents' own rate limits are the real bound).
pub const MAX_JOBS: usize = 8;

/// The arguments of one child: the global options of this process, then `run <id>` and the
/// drive flags.
pub(crate) fn child_args(cli: &Cli, id: &str, d: &DriveArgs) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    if let Some(p) = &cli.config {
        a.extend(["--config".into(), p.display().to_string()]);
    }
    if let Some(p) = &cli.machine_config {
        a.extend(["--machine-config".into(), p.display().to_string()]);
    }
    // the child writes into a pipe: plain text, whatever the environment says — this process
    // prints its lines (inert, X33)
    a.extend([
        "--color".into(),
        "never".into(),
        "run".into(),
        id.to_string(),
    ]);
    a.extend(d.flags());
    a
}

/// The ids, checked before anything starts: each a valid id of an existing task, none twice.
fn checked(st: &Store, ids: &[String]) -> Result<()> {
    let mut seen: Vec<&str> = Vec::new();
    let mut unknown = Vec::new();
    for id in ids {
        task::valid_task_id(id)?;
        if seen.contains(&id.as_str()) {
            bail!("`{id}` is given twice — each task runs once");
        }
        seen.push(id);
        let i = id.clone();
        if st
            .read(move |tx| Ok(store::get_task(tx, &i).is_ok()))?
            .eq(&false)
        {
            unknown.push(id.clone());
        }
    }
    if !unknown.is_empty() {
        bail!(
            "no task {} — nothing was started (`toryo task list`)",
            unknown.join(", ")
        );
    }
    Ok(())
}

/// What a child's exit code counts as (M16-R1): itself when it is 0 to 255, else a failure
/// (1) — on Windows a crash, an abort or a Ctrl+C that ended the child before it took the
/// console event is a negative NTSTATUS (`0xC000013A` is -1073741510), and on Unix a child
/// ended by a signal has no code at all (passed as -1). Never a code that a `max` would rank
/// below success.
fn counted(code: i32) -> i32 {
    if (0..=255).contains(&code) {
        code
    } else {
        1
    }
}

/// A line of a child, for the parent to print.
enum Line {
    Out(usize, String),
    Err(usize, String),
}

/// Read a child's stream line by line to its end — bytes, never `lines()`: one line that is not
/// UTF-8 must not stop the reading (the child would then block on a full pipe), it is shown
/// with `�`. When this process no longer prints, the rest is read and dropped.
fn read_lines<R: std::io::Read + Send + 'static>(
    r: R,
    slot: usize,
    err: bool,
    tx: mpsc::Sender<Line>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut r = BufReader::new(r);
        let mut buf = Vec::new();
        let mut printing = true;
        loop {
            buf.clear();
            match r.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if !printing {
                continue;
            }
            let l = String::from_utf8_lossy(&buf)
                .trim_end_matches(['\r', '\n'])
                .to_string();
            let line = if err {
                Line::Err(slot, l)
            } else {
                Line::Out(slot, l)
            };
            if tx.send(line).is_err() {
                printing = false;
            }
        }
    })
}

/// `toryo run` with several ids.
pub(crate) fn run_many(
    cli: &Cli,
    ids: &[String],
    jobs: Option<usize>,
    d: &DriveArgs,
) -> Result<i32> {
    let st = Store::open_default()?;
    checked(&st, ids)?;
    let jobs = jobs
        .unwrap_or(ids.len().min(DEFAULT_JOBS))
        .clamp(1, MAX_JOBS);
    let exe: PathBuf = std::env::current_exe()?;
    let width = ids.iter().map(|i| i.chars().count()).max().unwrap_or(0);
    // R23-16: a stream this process can no longer write (`toryo run A B | head`) stops its
    // printing, never the process — a panic here would close the children's pipes, and each
    // would die at its next line, mid-task. The lines are still read and dropped. The opening
    // line too: closed before or while it is written, it was a panic before any child started.
    let silent = std::cell::Cell::new(false);
    let opening = format!(
        "▶ run {} — {} at a time (`toryo dashboard` shows them all)",
        ids.join(" "),
        jobs
    );
    if toryo::term::try_line(toryo::term::Stream::Err, &opening).is_err() {
        silent.set(true);
    }
    // Ctrl+C goes to every process of the console: the children stop their tasks; this
    // process only stops starting new ones
    watch_interrupt();
    let (tx, rx) = mpsc::channel::<Line>();
    let mut queue: VecDeque<usize> = (0..ids.len()).collect();
    let mut running: Vec<(usize, Child)> = Vec::new();
    let mut codes: Vec<Option<i32>> = vec![None; ids.len()];
    let mut said_stop = false;
    let print = |l: Line| {
        if silent.get() {
            return;
        }
        let (s, i, t) = match l {
            Line::Err(i, t) => (toryo::term::Stream::Err, i, t),
            Line::Out(i, t) => (toryo::term::Stream::Out, i, t),
        };
        if toryo::term::try_line(s, &format!("{:<width$} │ {t}", ids[i])).is_err() {
            silent.set(true);
        }
    };
    while !queue.is_empty() || !running.is_empty() {
        if interrupted() {
            if !said_stop && !queue.is_empty() {
                let _ = toryo::term::try_line(
                    toryo::term::Stream::Err,
                    &format!(
                        "■ Ctrl+C — the running tasks stop; not started: {}",
                        queue
                            .iter()
                            .map(|i| ids[*i].as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
            said_stop = true;
            queue.clear();
        }
        // M16-R2: asked again before each start — a Ctrl+C pressed after the check above
        // must not start a child that would never see it
        while running.len() < jobs && !interrupted() {
            let Some(i) = queue.pop_front() else { break };
            let spawned = Command::new(&exe)
                .args(child_args(cli, &ids[i], d))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            match spawned {
                Ok(mut c) => {
                    if let Some(o) = c.stdout.take() {
                        read_lines(o, i, false, tx.clone());
                    }
                    if let Some(e) = c.stderr.take() {
                        read_lines(e, i, true, tx.clone());
                    }
                    running.push((i, c));
                }
                Err(e) => {
                    toryo::errln!(
                        "{:<width$} │ error: cannot start {}: {e}",
                        ids[i],
                        exe.display()
                    );
                    codes[i] = Some(2);
                }
            }
        }
        if let Ok(l) = rx.recv_timeout(Duration::from_millis(100)) {
            print(l);
            while let Ok(l) = rx.try_recv() {
                print(l);
            }
        }
        let mut k = 0;
        while k < running.len() {
            // (R23-16: an error is that child's, not a reason to leave the others unwatched)
            match running[k].1.try_wait().map_err(|e| e.to_string()) {
                Ok(Some(status)) => {
                    let (i, _) = running.remove(k);
                    // the code as the child ended (shown); what it counts is `counted`
                    codes[i] = Some(status.code().unwrap_or(-1));
                }
                Ok(None) => k += 1,
                Err(e) => {
                    let (i, _) = running.remove(k);
                    let _ = toryo::term::try_line(
                        toryo::term::Stream::Err,
                        &format!("{:<width$} │ error: cannot wait for the child: {e}", ids[i]),
                    );
                    codes[i] = Some(2);
                }
            }
        }
    }
    // what the children printed last; a descendant that keeps a pipe open does not hold this
    // process for longer than a moment
    drop(tx);
    let until = Instant::now() + Duration::from_secs(2);
    while let Ok(l) = rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
        print(l);
    }
    let mut lines = Vec::new();
    let mut worst = 0;
    let mut all_done = true;
    for (i, id) in ids.iter().enumerate() {
        let i2 = id.clone();
        let state = st.read(move |tx| store::get_task(tx, &i2)).ok();
        let code = codes[i];
        worst = worst.max(code.map_or(0, counted));
        let what = match code {
            Some(c) => format!("exit {c}"),
            None => {
                all_done = false;
                "not started".into()
            }
        };
        match state {
            Some(t) => lines.push(format!(
                "{id:<width$}  {:<22} ({what}) — next: {}",
                format!("{:?}", t.state),
                next_action(&t)
            )),
            None => lines.push(format!("{id:<width$}  ({what})")),
        }
    }
    let title = format!(
        "{} task{}",
        ids.len(),
        if ids.len() == 1 { "" } else { "s" }
    );
    let colour = if worst == 0 && all_done {
        "Completed"
    } else if worst == 0 {
        "Paused"
    } else {
        "Escalated"
    };
    if !silent.get() {
        toryo::term::end_block(&title, colour, &lines);
    }
    Ok(if all_done { worst } else { worst.max(1) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// The drive options of a parsed `run`.
    fn run_drive(cli: &Cli) -> Option<DriveArgs> {
        match &cli.cmd {
            crate::Cmd::Run { drive, .. } => Some(drive.clone()),
            _ => None,
        }
    }

    #[test]
    fn a_crashed_or_killed_child_counts_as_a_failure() {
        assert_eq!(counted(0), 0);
        assert_eq!(counted(3), 3);
        assert_eq!(counted(255), 255);
        // STATUS_CONTROL_C_EXIT, STATUS_STACK_OVERFLOW, a fast-fail abort, a signal (-1)
        for c in [
            0xC000013Au32 as i32,
            0xC00000FDu32 as i32,
            0xC0000409u32 as i32,
            -1,
            256,
        ] {
            assert_eq!(counted(c), 1, "{c}");
            assert!(0.max(counted(c)) > 0, "{c} must rank above success");
        }
    }

    #[test]
    fn a_child_gets_the_global_options_and_the_drive_flags() {
        let cli = Cli::parse_from([
            "toryo",
            "--config",
            "C:/r/toryo.yaml",
            "--machine-config",
            "C:/m.yaml",
            "--color",
            "always",
            "run",
            "A-1",
            "B-1",
            "--yes",
            "--fake-agents",
            "s.json",
            "--no-notify",
        ]);
        let d = run_drive(&cli).expect("a run");
        let a = child_args(&cli, "B-1", &d);
        assert_eq!(
            a,
            [
                "--config",
                "C:/r/toryo.yaml",
                "--machine-config",
                "C:/m.yaml",
                "--color",
                "never",
                "run",
                "B-1",
                "--yes",
                "--fake-agents",
                "s.json",
                "--no-notify"
            ]
        );
        // nothing is passed that the child would choose the same way by itself
        let plain = Cli::parse_from(["toryo", "run", "A-1", "B-1"]);
        let d = run_drive(&plain).expect("a run");
        assert_eq!(
            child_args(&plain, "A-1", &d),
            ["--color", "never", "run", "A-1"]
        );
    }
}
