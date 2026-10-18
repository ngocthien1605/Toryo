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
    toryo::errln!(
        "▶ run {} — {} at a time (`toryo dashboard` shows them all)",
        ids.join(" "),
        jobs
    );
    // Ctrl+C goes to every process of the console: the children stop their tasks; this
    // process only stops starting new ones
    watch_interrupt();
    let (tx, rx) = mpsc::channel::<Line>();
    let mut queue: VecDeque<usize> = (0..ids.len()).collect();
    let mut running: Vec<(usize, Child)> = Vec::new();
    let mut codes: Vec<Option<i32>> = vec![None; ids.len()];
    let mut said_stop = false;
    let print = |l: Line| match l {
        Line::Err(i, t) => toryo::errln!("{:<width$} │ {t}", ids[i]),
        Line::Out(i, t) => toryo::outln!("{:<width$} │ {t}", ids[i]),
    };
    while !queue.is_empty() || !running.is_empty() {
        if interrupted() {
            if !said_stop && !queue.is_empty() {
                toryo::errln!(
                    "■ Ctrl+C — the running tasks stop; not started: {}",
                    queue
                        .iter()
                        .map(|i| ids[*i].as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            said_stop = true;
            queue.clear();
        }
        while running.len() < jobs {
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
            match running[k].1.try_wait()? {
                Some(status) => {
                    let (i, _) = running.remove(k);
                    // a child ended by a signal or a forced kill has no code: a failure
                    codes[i] = Some(status.code().unwrap_or(1));
                }
                None => k += 1,
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
        worst = worst.max(code.unwrap_or(0));
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
    toryo::term::end_block(&title, colour, &lines);
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
