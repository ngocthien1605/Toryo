//! `CommandRunner` (plan §6.1, R10): how verify commands run. v0.1 ships `LocalRunner`
//! (this machine, optional Low-IL boundary, R36); a `RemoteRunner` for the D365FO dev VM
//! can implement the same trait later.

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc::UnboundedSender;

use crate::process::redact::Redactions;
use crate::process::{spawn, strip_ansi, Boundary, Chunk, SpawnSpec, WaitOptions};

#[derive(Debug, Clone)]
pub struct RunSpec {
    pub name: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub boundary: Boundary,
    pub timeout: Duration,
    /// M11-E2 (review M2): the task's secrets, redacted from the output as it is read.
    pub redact: Redactions,
    /// Set to `true` to terminate the command's Job.
    pub cancel: Option<Arc<AtomicBool>>,
}

/// M12-05: the command could not be started at all (no such program, not an executable) — a
/// problem of the machine, never a failing gate. Carried typed, so the engine never reads it
/// out of the message.
#[derive(Debug)]
pub struct StartFailed {
    pub name: String,
    pub program: String,
    /// The OS error, with its context.
    pub error: String,
}

impl std::fmt::Display for StartFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "starting `{}` ({}): {}",
            self.name, self.program, self.error
        )
    }
}

impl std::error::Error for StartFailed {}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CommandResult {
    pub name: String,
    /// `None` when the command was stopped (timeout, cancel).
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub duration_ms: u128,
    /// stdout + stderr, ANSI-stripped (R50).
    pub output: String,
    /// M11-R17: the signal that ended the command (Unix; `exit` is then -1). Left out when
    /// there is none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    /// M12-16: the file holding `output` (redacted) when the caller asked for one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<PathBuf>,
}

/// M12-16: the first part of a gate's output that is kept ...
pub const OUTPUT_HEAD_KEEP: usize = 1024 * 1024;
/// ... and the last part; what lies between is dropped and marked.
pub const OUTPUT_TAIL_KEEP: usize = 8 * 1024 * 1024;

/// M12-16: a gate's raw output, bounded — the first [`OUTPUT_HEAD_KEEP`] bytes and the last
/// [`OUTPUT_TAIL_KEEP`] bytes; the rest is counted and replaced by one marker line.
#[derive(Default)]
pub struct BoundedOutput {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    omitted: u64,
    total: u64,
}

impl BoundedOutput {
    pub fn push(&mut self, bytes: &[u8]) {
        self.total += bytes.len() as u64;
        let room = OUTPUT_HEAD_KEEP - self.head.len();
        let (to_head, rest) = bytes.split_at(room.min(bytes.len()));
        self.head.extend_from_slice(to_head);
        self.tail.extend(rest.iter().copied());
        if self.tail.len() > OUTPUT_TAIL_KEEP {
            let over = self.tail.len() - OUTPUT_TAIL_KEEP;
            self.tail.drain(..over);
            self.omitted += over as u64;
        }
    }

    /// Bytes seen so far, kept or not.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// The kept bytes, with the marker line where bytes were dropped (cut as
    /// [`Self::into_sides`] cuts them).
    pub fn into_bytes(self) -> Vec<u8> {
        let (mut head, cut) = self.into_sides();
        if let Some((omitted, tail)) = cut {
            head.extend(omitted_marker(omitted).bytes());
            head.extend(tail);
        }
        head
    }

    /// The kept bytes as the head and — when bytes were dropped — `(omitted, tail)`, cut on line
    /// boundaries (M13-04, X27): the head ends at its last line break and the tail starts after
    /// its first one, so no line and no multi-byte character is torn. A side with no line
    /// break at all (one line longer than the bound) is kept whole, less a torn UTF-8
    /// character at the cut. What is dropped counts as omitted. Nothing here knows the
    /// secrets: [`bounded_text`] redacts each side and takes the bytes a secret that straddled
    /// the cut could have left.
    pub fn into_sides(self) -> (Vec<u8>, Option<(u64, Vec<u8>)>) {
        let mut head = self.head;
        let mut tail: Vec<u8> = self.tail.into();
        let mut omitted = self.omitted;
        if omitted == 0 {
            head.extend(tail);
            return (head, None);
        }
        let keep = match head.iter().rposition(|b| *b == b'\n') {
            Some(i) => i + 1,
            // an unfinished UTF-8 character at the very end, or nothing
            None => match std::str::from_utf8(&head) {
                Err(e) if e.error_len().is_none() => e.valid_up_to(),
                _ => head.len(),
            },
        };
        omitted += (head.len() - keep) as u64;
        head.truncate(keep);
        let skip = match tail.iter().position(|b| *b == b'\n') {
            Some(i) => i + 1,
            // the rest of a UTF-8 character whose start was dropped, or nothing
            None => tail
                .iter()
                .take(3)
                .take_while(|b| **b & 0xC0 == 0x80)
                .count(),
        };
        omitted += skip as u64;
        tail.drain(..skip);
        (head, Some((omitted, tail)))
    }
}

/// The line that stands where output was dropped.
fn omitted_marker(omitted: u64) -> String {
    format!(
        "\n[... {omitted} bytes of output omitted: Toryo keeps the first {} MB and the last {} MB of a gate's output ...]\n",
        OUTPUT_HEAD_KEEP / (1024 * 1024),
        OUTPUT_TAIL_KEEP / (1024 * 1024)
    )
}

/// The text of a gate's bounded output: decode → strip the colour codes → redact (M11-R8,
/// M12-15) — each kept side on its own, and **then** the cut (M13-04, X27: a secret is whole or
/// gone, never a fragment). A secret that lay whole in a side is redacted before anything is
/// taken from it. A secret that straddled the cut left less than its length at the cut edge of
/// each side: that much goes (and the rest of that line, when the side has other lines) —
/// whether the secret is one line or several. With no secret configured nothing more goes.
pub fn bounded_text(out: BoundedOutput, redact: &Redactions) -> String {
    let clean = |b: &[u8]| redact.redact_str(&strip_ansi(&crate::process::decode_output(b)));
    let (head, cut) = out.into_sides();
    let mut head = clean(&head);
    let Some((mut omitted, tail)) = cut else {
        return head;
    };
    let mut tail = clean(&tail);
    if redact.max_len() > 0 {
        // (two bytes more than the longest secret: a torn character decodes to a longer mark;
        // M13-R3: and what the replacements of shorter secrets inside the fragment add)
        let hold = redact.cut_hold();
        let mut keep = head.len().saturating_sub(hold);
        while !head.is_char_boundary(keep) {
            keep -= 1;
        }
        if let Some(i) = head[..keep].rfind('\n') {
            keep = i + 1;
        }
        omitted += (head.len() - keep) as u64;
        head.truncate(keep);
        let mut skip = hold.min(tail.len());
        while !tail.is_char_boundary(skip) {
            skip += 1;
        }
        if let Some(i) = tail[skip..].find('\n') {
            skip += i + 1;
        }
        omitted += skip as u64;
        tail.drain(..skip);
    }
    head.push_str(&omitted_marker(omitted));
    head.push_str(&tail);
    head
}

/// M12-16: what the engine asks of a gate run besides [`RunSpec`]. Nothing by default.
#[derive(Default, Clone)]
pub struct GateIo {
    /// Write the (redacted, bounded) output here when the run is over.
    pub log: Option<PathBuf>,
    /// A line on the console every this long while the gate runs: its name, the time and the
    /// bytes of output so far.
    pub heartbeat: Option<Duration>,
}

impl CommandResult {
    pub fn ok(&self) -> bool {
        self.exit == Some(0)
    }

    /// M11-R17: how the command ended, in words — `exit 1`, `exit -1 (killed by signal 9,
    /// SIGKILL)`, `no exit code (timed out)`. On Unix an exit code of -1 always means a signal
    /// ended the process: a program that cannot be started is an error, never an exit code.
    pub fn ended(&self) -> String {
        let name = |n: i32| match n {
            1 => ", SIGHUP",
            2 => ", SIGINT",
            3 => ", SIGQUIT",
            4 => ", SIGILL",
            6 => ", SIGABRT",
            7 => ", SIGBUS",
            8 => ", SIGFPE",
            9 => ", SIGKILL",
            11 => ", SIGSEGV",
            13 => ", SIGPIPE",
            15 => ", SIGTERM",
            _ => "",
        };
        match (self.exit, self.signal) {
            (Some(c), Some(s)) => format!("exit {c} (killed by signal {s}{})", name(s)),
            (Some(c), None) => format!("exit {c}"),
            (None, _) if self.timed_out => "no exit code (timed out)".into(),
            (None, _) => "no exit code (stopped)".into(),
        }
    }

    /// The last `n` lines of output.
    pub fn tail(&self, n: usize) -> String {
        let lines: Vec<&str> = self.output.lines().collect();
        lines[lines.len().saturating_sub(n)..].join("\n")
    }
}

#[async_trait]
pub trait CommandRunner: Send + Sync {
    /// Run to completion; `sink` receives output chunks (ANSI-stripped) as they arrive.
    async fn run(
        &self,
        spec: RunSpec,
        sink: Option<UnboundedSender<String>>,
    ) -> Result<CommandResult>;
}

/// Runs on this machine inside a per-command Job Object.
pub struct LocalRunner;

#[async_trait]
impl CommandRunner for LocalRunner {
    async fn run(
        &self,
        spec: RunSpec,
        sink: Option<UnboundedSender<String>>,
    ) -> Result<CommandResult> {
        self.run_with(spec, sink, GateIo::default()).await
    }
}

impl LocalRunner {
    /// [`CommandRunner::run`] with the log and the heartbeat of M12-16.
    pub async fn run_with(
        &self,
        spec: RunSpec,
        sink: Option<UnboundedSender<String>>,
        io: GateIo,
    ) -> Result<CommandResult> {
        tokio::task::spawn_blocking(move || {
            let s = SpawnSpec {
                program: spec.program.clone(),
                args: spec.args.clone(),
                cwd: spec.cwd.clone(),
                env: spec.env.clone(),
                stdin: None,
                boundary: spec.boundary,
            };
            // the raw streams, interleaved as they arrive; the whole output is redacted at the
            // end (M11-E2): a per-stream redactor holds back bytes and would reorder stdout and
            // stderr when any secret is configured (group E review)
            let running = spawn(&s, &Redactions::empty()).map_err(|e| {
                anyhow::Error::new(StartFailed {
                    name: spec.name.clone(),
                    program: spec.program.display().to_string(),
                    error: format!("{e:#}"),
                })
            })?;
            let cancel_flag = spec.cancel.clone();
            // M12-16: bounded — the first 1 MB and the last 8 MB; the count is shared with the
            // heartbeat, which runs on this thread's `wait` loop
            let mut out = BoundedOutput::default();
            let seen = Arc::new(AtomicU64::new(0));
            let heartbeat = io.heartbeat.map(|every| {
                let (name, seen) = (spec.name.clone(), seen.clone());
                let beat: crate::process::Heartbeat = Box::new(move |elapsed| {
                    crate::errln!(
                        "  ... {name} still running: {}:{:02} elapsed, {} bytes of output so far",
                        elapsed.as_secs() / 60,
                        elapsed.as_secs() % 60,
                        seen.load(Ordering::Relaxed)
                    );
                });
                (every, beat)
            });
            let info = running.wait(
                WaitOptions {
                    max: Some(spec.timeout),
                    stall: None,
                    cancel: cancel_flag.map(|c| {
                        Box::new(move || c.load(Ordering::SeqCst)) as Box<dyn Fn() -> bool + Send>
                    }),
                    heartbeat,
                },
                |c| {
                    let (Chunk::Stdout(b) | Chunk::Stderr(b)) = c;
                    if let Some(tx) = &sink {
                        // live chunks (no engine caller uses them) are redacted chunk by chunk
                        // M11-R8: colour codes go first, so a secret split by one is found
                        let _ = tx.send(
                            spec.redact
                                .redact_str(&strip_ansi(&crate::process::decode_output(&b))),
                        );
                    }
                    out.push(&b);
                    seen.store(out.total(), Ordering::Relaxed);
                },
            )?;
            // M11-R8: strip the colour codes, then redact: a secret split by a colour code is found
            // M12-15 (X23): decode (a cp932 line is not `�`), then colour codes, then redact
            let output = bounded_text(out, &spec.redact);
            // M12-16: the log holds exactly the text the result holds; a log that cannot be
            // written is not a failing gate
            let log = io.log.as_ref().and_then(|p| {
                if let Some(dir) = p.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                match std::fs::write(p, &output) {
                    Ok(()) => Some(p.clone()),
                    Err(e) => {
                        crate::errln!("WARNING: could not write the gate log {}: {e}", p.display());
                        None
                    }
                }
            });
            Ok(CommandResult {
                name: spec.name,
                exit: if info.timed_out || info.cancelled {
                    None
                } else {
                    info.code
                },
                timed_out: info.timed_out,
                cancelled: info.cancelled,
                duration_ms: info.duration_ms,
                output,
                signal: info.signal,
                log,
            })
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M12-16: below the bound nothing is dropped; above it the first 1 MB and the last 8 MB
    /// stay with one marker between.
    /// M13-04 (X27): once bytes are dropped, the head ends at a line break and the tail starts
    /// after one; a side without any line break is kept whole, less a torn UTF-8 character.
    #[test]
    fn bounded_output_is_cut_on_line_boundaries() {
        let line = "0123456789abcdefghijklmnopqrstuvwxyz\n"; // 37 bytes
        let mut b = BoundedOutput::default();
        let n = (OUTPUT_HEAD_KEEP + OUTPUT_TAIL_KEEP) / line.len() + 1000;
        for _ in 0..n {
            b.push(line.as_bytes());
        }
        let total = b.total();
        let text = String::from_utf8(b.into_bytes()).unwrap();
        let (head, rest) = text.split_once("\n[... ").expect("the marker");
        let (marker, tail) = rest.split_once(" ...]\n").expect("the marker's end");
        assert!(head.len() % line.len() == 0, "the head ends on a line");
        assert!(
            tail.len() % line.len() == 0 && tail.starts_with("0123"),
            "the tail starts on a line"
        );
        let omitted: u64 = marker.split(' ').next().unwrap().parse().unwrap();
        assert_eq!(head.len() as u64 + tail.len() as u64 + omitted, total);
        // a side with no line break: a character torn by the bound goes, nothing else
        let mut b = BoundedOutput::default();
        b.push(&vec![b'a'; OUTPUT_HEAD_KEEP - 1]);
        b.push("é".as_bytes()); // its first byte is the last of the head
        b.push(b"bbb");
        b.push("é".as_bytes()); // its second byte is the first of the tail
        b.push(&vec![b'b'; OUTPUT_TAIL_KEEP - 1]);
        let bytes = b.into_bytes();
        let text = String::from_utf8(bytes.clone()).expect("no torn character");
        assert!(
            text.contains("\n[... 7 bytes of output omitted"),
            "5 dropped by the bound, and the half of a character left on each side"
        );
        assert!(bytes[..OUTPUT_HEAD_KEEP - 1].iter().all(|b| *b == b'a'));
        assert_eq!(bytes[OUTPUT_HEAD_KEEP - 1], b'\n');
        // nothing dropped: nothing is cut
        let mut b = BoundedOutput::default();
        b.push(b"no line break at all");
        assert_eq!(b.into_bytes(), b"no line break at all");
    }

    /// Review of M13-04: the cut must not make a fragment out of a **whole** secret either. Was
    /// (the first version of M13-04): a side without a line break lost "the longest secret minus
    /// one" raw bytes at the cut — which cut a whole secret lying there in two; and a secret of
    /// several lines that straddled a line-boundary cut kept its first lines. Now each side is
    /// redacted first and only then loses the bytes a straddling secret could have left.
    #[test]
    fn a_secret_next_to_the_cut_is_whole_or_gone() {
        use crate::process::redact::Secret;
        const SECRET: &str = "SECRET-VALUE-20CHARS";
        let one = Redactions::new(&[Secret {
            name: "S".into(),
            value: SECRET.into(),
        }]);
        let no_fragment = |text: &str, secret: &str| {
            assert!(
                text.contains("bytes of output omitted"),
                "the output was cut"
            );
            for i in 0..=secret.len() - 8 {
                assert!(
                    !text.contains(&secret[i..i + 8]),
                    "a fragment is left: {:?}",
                    &secret[i..i + 8]
                );
            }
        };
        // a whole secret that ends 10 bytes before the end of a head with no line break
        let mut b = BoundedOutput::default();
        b.push(&vec![b'a'; OUTPUT_HEAD_KEEP - 30]);
        b.push(SECRET.as_bytes());
        b.push(&[b'a'; 10]);
        b.push(&vec![b'b'; OUTPUT_TAIL_KEEP + 5]);
        no_fragment(&bounded_text(b, &one), SECRET);
        // a whole secret that starts 10 bytes into a tail with no line break
        let mut b = BoundedOutput::default();
        b.push(&vec![b'a'; OUTPUT_HEAD_KEEP + 5]);
        b.push(&[b'b'; 10]);
        b.push(SECRET.as_bytes());
        b.push(&vec![b'b'; OUTPUT_TAIL_KEEP - 30]);
        no_fragment(&bounded_text(b, &one), SECRET);
        // a secret of three lines; the head's bound falls into its third line
        const LINES: &str = "LINE-ONE-AAAA\nLINE-TWO-BBBB\nLINE-THREE-CC";
        let many = Redactions::new(&[Secret {
            name: "KEY".into(),
            value: LINES.into(),
        }]);
        let mut b = BoundedOutput::default();
        let filler = "0123456789abcdef\n".repeat(OUTPUT_HEAD_KEEP / 17);
        b.push(&filler.as_bytes()[..OUTPUT_HEAD_KEEP - 34]);
        b.push(LINES.as_bytes());
        b.push(b"\n");
        b.push(
            "0123456789abcdef\n"
                .repeat(OUTPUT_TAIL_KEEP / 17 + 10)
                .as_bytes(),
        );
        let text = bounded_text(b, &many);
        assert!(
            text.contains("bytes of output omitted"),
            "the output was cut"
        );
        for line in LINES.lines() {
            assert!(!text.contains(line), "a line of the secret is left: {line}");
        }
        // nothing dropped: nothing is cut, and the secret is redacted as ever
        let mut b = BoundedOutput::default();
        b.push(format!("before {SECRET} after").as_bytes());
        assert_eq!(bounded_text(b, &one), "before [REDACTED:S] after");
    }

    /// M13-R3: a shorter secret B inside the fragment of a longer secret A that straddles the
    /// cut (a password inside a connection string). Redacting B grows the fragment past the
    /// bytes that were held at the cut: the start (head) or the end (tail) of A's fragment
    /// survived. The hold is measured in raw terms: it grows with what a replacement adds.
    #[test]
    fn a_secret_inside_the_fragment_of_another_is_no_reason_to_keep_the_fragment() {
        use crate::process::redact::Secret;
        const A: &str = "AAAAAAAAAAbbCCCCCCCCCC";
        let r = Redactions::new(&[
            Secret {
                name: "A".into(),
                value: A.into(),
            },
            Secret {
                name: "VERY_LONG_SECRET_NAME".into(),
                value: "bb".into(),
            },
        ]);
        let no_window = |text: &str| {
            assert!(
                text.contains("bytes of output omitted"),
                "the output was cut"
            );
            for i in 0..=A.len() - 8 {
                assert!(
                    !text.contains(&A[i..i + 8]),
                    "a fragment of A is left: {:?}",
                    &A[i..i + 8]
                );
            }
        };
        // the cut falls after `AAAAAAAAAAbbCCCCCC`: the head holds the first 18 bytes of A
        let mut b = BoundedOutput::default();
        b.push(&vec![b'x'; OUTPUT_HEAD_KEEP - 18]);
        b.push(A.as_bytes());
        b.push(&vec![b'y'; OUTPUT_TAIL_KEEP + 5]);
        no_window(&bounded_text(b, &r));
        // the tail starts inside A: its first 10 bytes were dropped, `bbCCCCCCCCCC` is kept
        let mut b = BoundedOutput::default();
        b.push(&vec![b'x'; OUTPUT_HEAD_KEEP]);
        b.push(A.as_bytes());
        b.push(&vec![b'y'; OUTPUT_TAIL_KEEP - 12]);
        no_window(&bounded_text(b, &r));
    }

    #[test]
    fn bounded_output_keeps_the_head_and_the_tail() {
        let mut small = BoundedOutput::default();
        small.push(b"abc");
        small.push(b"def");
        assert_eq!(small.total(), 6);
        assert_eq!(small.into_bytes(), b"abcdef");
        // exactly head + tail: nothing omitted
        let mut edge = BoundedOutput::default();
        edge.push(&vec![b'a'; OUTPUT_HEAD_KEEP]);
        edge.push(&vec![b'b'; OUTPUT_TAIL_KEEP]);
        let bytes = edge.into_bytes();
        assert_eq!(bytes.len(), OUTPUT_HEAD_KEEP + OUTPUT_TAIL_KEEP);
        // one byte more: one byte omitted, marked
        let mut over = BoundedOutput::default();
        over.push(&vec![b'a'; OUTPUT_HEAD_KEEP]);
        over.push(&vec![b'b'; OUTPUT_TAIL_KEEP + 1]);
        let bytes = over.into_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("\n[... 1 bytes of output omitted"),
            "{}",
            &text[OUTPUT_HEAD_KEEP..OUTPUT_HEAD_KEEP + 120]
        );
        assert!(bytes.starts_with(b"aaaa") && bytes.ends_with(b"bbbb"));
        assert!(bytes.len() < OUTPUT_HEAD_KEEP + OUTPUT_TAIL_KEEP + 200);
        // many small chunks
        let mut many = BoundedOutput::default();
        for i in 0..1_000_000u32 {
            many.push(format!("line {i:06}\n").as_bytes());
        }
        let n = many.total();
        let bytes = many.into_bytes();
        assert!(n > (OUTPUT_HEAD_KEEP + OUTPUT_TAIL_KEEP) as u64);
        assert!(bytes.starts_with(b"line 000000\n") && bytes.ends_with(b"line 999999\n"));
    }
}
