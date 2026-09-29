//! Notifications (M10-D1): a Windows toast when a task needs the owner — a gate opens, the task
//! escalates or pauses, or a run ends `Completed` or `Failed`. A [`Notifier`] is an engine event
//! sink ([`crate::engine::flight`]); what it shows is decided by the pure [`toast_for`], and a
//! [`Toaster`] shows it (a recording double in tests).
//!
//! The Windows toaster starts `powershell.exe -NoProfile -NonInteractive -EncodedCommand <fixed
//! script>`: the script is a constant and reads the title and the body from environment
//! variables, so agent-written text (a reason, R61) is data — never part of a command line or a
//! script. Fire and forget: the process is not in a task's Job, a watcher kills it after 10 s,
//! and nothing waits for it. Off by default (`notify: { toast: true }` in the machine config, or
//! `--notify`). Other platforms: nothing is shown.

use std::time::Duration;

use anyhow::Result;

use crate::config::Notify;
use crate::engine::flight::{EngineEvent, Envelope, EventSink};
use crate::engine::state::PipelineState as St;

/// The body is shortened to this many characters.
pub const BODY_MAX: usize = 200;

/// How long a toast process may run before it is killed.
pub const TOAST_TIMEOUT: Duration = Duration::from_secs(10);

/// The environment variables the toast script reads.
pub const ENV_TITLE: &str = "TORYO_TOAST_TITLE";
pub const ENV_BODY: &str = "TORYO_TOAST_BODY";
/// `1`: build the toast, do not show it (`notify-test --dry-run`, tests).
pub const ENV_DRY_RUN: &str = "TORYO_TOAST_DRY_RUN";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub title: String,
    pub body: String,
}

/// At most `max` characters (an ellipsis marks a cut); NUL characters, which an environment
/// variable cannot hold, are dropped.
pub fn shorten(s: &str, max: usize) -> String {
    let s: String = s.chars().filter(|c| *c != '\0').collect();
    if s.chars().count() <= max {
        return s;
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The toast an event raises, if any: the gates, an escalation, a pause, and the end of a run
/// in `Completed` or `Failed`. Title `toryo — <task>`; body: the state and the next action.
pub fn toast_for(e: &Envelope) -> Option<Toast> {
    let id = &e.task;
    let body = match &e.event {
        EngineEvent::GateOpened { gate, .. } if gate == "plan" => format!(
            "{:?} — review PLAN.md, then `toryo approve {id} --gate plan --continue` or `toryo reject {id} --gate plan --reason \"…\"`",
            St::WaitingPlanApproval
        ),
        EngineEvent::GateOpened { .. } => format!(
            "{:?} — review the branch, then `toryo approve {id} --gate final` or `toryo reject {id} --gate final --reason \"…\"`",
            St::WaitingFinalApproval
        ),
        EngineEvent::Escalated { reason, .. } => {
            format!("{:?} (`toryo status {id}` for what to do) — {reason}", St::Escalated)
        }
        EngineEvent::Paused { reason } if reason == "budget" => format!(
            "{:?} — budget: `toryo budget set {id} --tokens N`, then `toryo resume {id}`",
            St::Paused
        ),
        EngineEvent::Paused { reason } => {
            format!("{:?} (`toryo resume {id}`) — {reason}", St::Paused)
        }
        EngineEvent::RunEnded { state, next_action }
            if matches!(state, St::Completed | St::Failed) =>
        {
            format!("{state:?} — {next_action}")
        }
        _ => return None,
    };
    Some(Toast {
        title: format!("toryo — {id}"),
        body: shorten(&body, BODY_MAX),
    })
}

/// Whether toasts are on for a drive command: `--no-notify` wins, then `--notify`, then the
/// machine config (default off).
pub fn enabled(notify: bool, no_notify: bool, machine: Notify) -> bool {
    !no_notify && (notify || machine.toast)
}

/// Shows a toast.
pub trait Toaster: Send + Sync {
    fn show(&self, t: &Toast) -> Result<()>;
}

/// The event sink that raises the toasts.
pub struct Notifier {
    toaster: Box<dyn Toaster>,
}

impl Notifier {
    pub fn new(toaster: Box<dyn Toaster>) -> Self {
        Self { toaster }
    }

    /// The platform's toaster: the Windows toast, elsewhere nothing.
    pub fn system() -> Self {
        Self::new(Box::new(SystemToaster))
    }
}

impl EventSink for Notifier {
    fn name(&self) -> &str {
        "notifier"
    }

    fn emit(&self, e: &Envelope) -> Result<()> {
        match toast_for(e) {
            Some(t) => self.toaster.show(&t),
            None => Ok(()),
        }
    }
}

/// The toast script (Windows PowerShell 5.1, which loads WinRT types). Constant: the title and
/// the body come from [`ENV_TITLE`] and [`ENV_BODY`] and become XML text nodes (`CreateTextNode`),
/// never code or markup.
pub const SCRIPT: &str = r#"$ErrorActionPreference = 'Stop'
$null = [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime]
$null = [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime]
$xml = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02)
$text = $xml.GetElementsByTagName('text')
$null = $text.Item(0).AppendChild($xml.CreateTextNode([string]$env:TORYO_TOAST_TITLE))
$null = $text.Item(1).AppendChild($xml.CreateTextNode([string]$env:TORYO_TOAST_BODY))
$toast = [Windows.UI.Notifications.ToastNotification]::new($xml)
if ($env:TORYO_TOAST_DRY_RUN -eq '1') { exit 0 }
$app = '{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe'
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($app).Show($toast)
"#;

/// Base64 (RFC 4648, padded) — for `-EncodedCommand`; no crate for 20 lines.
pub fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// What the toaster starts: the program, its (constant) arguments, and the environment that
/// carries the toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastCommand {
    pub program: std::path::PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// The toast command for `t`. Only `env` depends on the toast.
pub fn toast_command(t: &Toast, dry_run: bool) -> ToastCommand {
    let utf16: Vec<u8> = SCRIPT.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    ToastCommand {
        program: std::path::PathBuf::from(root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe"),
        args: vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-EncodedCommand".into(),
            base64(&utf16),
        ],
        env: vec![
            (ENV_TITLE.into(), t.title.clone()),
            (ENV_BODY.into(), t.body.clone()),
            (ENV_DRY_RUN.into(), if dry_run { "1" } else { "0" }.into()),
        ],
    }
}

/// The process of a toast, without a console window and without stdin/stdout/stderr.
fn spawn(t: &Toast, dry_run: bool) -> Result<std::process::Child> {
    let c = toast_command(t, dry_run);
    let mut cmd = std::process::Command::new(&c.program);
    cmd.args(&c.args)
        .envs(c.env)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn()
        .map_err(|e| anyhow::anyhow!("cannot start {}: {e}", c.program.display()))
}

/// Wait for `child` up to `limit`, then kill it. The exit code, `None` when killed.
fn wait_or_kill(mut child: std::process::Child, limit: Duration) -> Option<i32> {
    let end = std::time::Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return Some(s.code().unwrap_or(1)),
            Ok(None) if std::time::Instant::now() < end => {
                std::thread::sleep(Duration::from_millis(100))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// The platform's toaster: fire and forget on Windows, nothing elsewhere.
pub struct SystemToaster;

/// Test hook: `TORYO_TEST_TOAST_LOG=<file>` appends each toast to that file as a JSON line
/// instead of showing it (end-to-end tests without toasts on the desktop).
pub const TEST_LOG: &str = "TORYO_TEST_TOAST_LOG";

impl Toaster for SystemToaster {
    fn show(&self, t: &Toast) -> Result<()> {
        if let Some(p) = std::env::var_os(TEST_LOG) {
            use std::io::Write;
            let line = serde_json::json!({"title": t.title, "body": t.body}).to_string();
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)?;
            return Ok(writeln!(f, "{line}")?);
        }
        if !cfg!(windows) {
            return Ok(());
        }
        let child = spawn(t, false)?;
        std::thread::spawn(move || wait_or_kill(child, TOAST_TIMEOUT));
        Ok(())
    }
}

/// `toryo notify-test`: show a sample toast (or with `dry_run` only build it) and wait for the
/// script, up to [`TOAST_TIMEOUT`]. The script's exit code; an error on other platforms.
pub fn test_toast(dry_run: bool) -> Result<i32> {
    if !cfg!(windows) {
        anyhow::bail!("toasts are Windows only: nothing to test on this platform");
    }
    let t = Toast {
        title: "toryo — notify-test".into(),
        body: "Toasts work: toryo shows one like this at a gate, an escalation, a pause and the end of a run.".into(),
    };
    let child = spawn(&t, dry_run)?;
    Ok(wait_or_kill(child, TOAST_TIMEOUT).unwrap_or(124))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Recorded(Arc<Mutex<Vec<Toast>>>);

    impl Toaster for Recorded {
        fn show(&self, t: &Toast) -> Result<()> {
            self.0.lock().unwrap().push(t.clone());
            Ok(())
        }
    }

    fn env(e: EngineEvent) -> Envelope {
        Envelope {
            v: 1,
            seq: 1,
            ts: "t".into(),
            task: "BR-9".into(),
            generation: 1,
            event: e,
            quiet: true,
        }
    }

    fn every_event() -> Vec<EngineEvent> {
        vec![
            EngineEvent::RunStarted { owner: "o".into() },
            EngineEvent::StateChanged {
                from: St::Planning,
                to: St::WaitingPlanApproval,
                event: "plan_ready".into(),
            },
            EngineEvent::GateOpened {
                gate: "plan".into(),
                occurrence: 1,
            },
            EngineEvent::GateOpened {
                gate: "final".into(),
                occurrence: 2,
            },
            EngineEvent::BudgetWarning { used: 8, cap: 10 },
            EngineEvent::Escalated {
                reason_class: "protected_config_changed".into(),
                reason: "protected_config_changed: .claude/x".into(),
            },
            EngineEvent::Paused {
                reason: "budget".into(),
            },
            EngineEvent::Paused {
                reason: "implementer_quota: all out".into(),
            },
            EngineEvent::RunEnded {
                state: St::WaitingPlanApproval,
                next_action: "review".into(),
            },
            EngineEvent::RunEnded {
                state: St::Escalated,
                next_action: "resume".into(),
            },
            EngineEvent::RunEnded {
                state: St::Completed,
                next_action: "done — merge `toryo/BR-9` yourself".into(),
            },
            EngineEvent::RunEnded {
                state: St::Failed,
                next_action: "failed: x — `toryo task cleanup BR-9`".into(),
            },
        ]
    }

    #[test]
    fn which_events_notify_and_what_the_toast_says() {
        let r = Recorded::default();
        let n = Notifier::new(Box::new(r.clone()));
        for e in every_event() {
            n.emit(&env(e)).unwrap();
        }
        let got = r.0.lock().unwrap().clone();
        let bodies: Vec<&str> = got.iter().map(|t| t.body.as_str()).collect();
        assert_eq!(got.len(), 7, "{bodies:#?}");
        assert!(got.iter().all(|t| t.title == "toryo — BR-9"));
        assert!(
            bodies[0].starts_with("WaitingPlanApproval — ")
                && bodies[0].contains("toryo approve BR-9 --gate plan --continue")
        );
        assert!(
            bodies[1].starts_with("WaitingFinalApproval — ") && bodies[1].contains("--gate final")
        );
        assert!(bodies[2].starts_with(
            "Escalated (`toryo status BR-9` for what to do) — protected_config_changed: .claude/x"
        ));
        assert!(bodies[3].contains("toryo budget set BR-9 --tokens N"));
        assert!(bodies[4].starts_with("Paused (`toryo resume BR-9`) — implementer_quota"));
        assert_eq!(bodies[5], "Completed — done — merge `toryo/BR-9` yourself");
        assert!(bodies[6].starts_with("Failed — "));
    }

    #[test]
    fn the_body_is_capped() {
        let long = "x".repeat(1000);
        let t = toast_for(&env(EngineEvent::Escalated {
            reason_class: "other".into(),
            reason: long,
        }))
        .unwrap();
        assert_eq!(t.body.chars().count(), BODY_MAX);
        assert!(t.body.ends_with('…'));
        assert_eq!(shorten("a\0b", 10), "ab");
        assert_eq!(shorten("ありがとう", 3), "あり…");
        assert_eq!(shorten("short", 200), "short");
    }

    #[test]
    fn nothing_is_sent_when_toasts_are_off() {
        let off = Notify { toast: false };
        let on = Notify { toast: true };
        assert!(!enabled(false, false, off), "default off");
        assert!(enabled(false, false, on));
        assert!(enabled(true, false, off), "--notify");
        assert!(!enabled(false, true, on), "--no-notify wins");
        assert!(!enabled(true, true, on));
    }

    /// AC-2: agent-written text is data. The program and the arguments do not depend on the
    /// toast; the text travels only in the environment, and the script reads it as text nodes.
    #[test]
    fn hostile_text_is_passed_as_data() {
        let hostile = Toast {
            title: "toryo — T\"; Remove-Item -Recurse C:\\ #".into(),
            body: "it's \"quoted\" $(Remove-Item x) `whoami` $env:PATH\r\nline 2 <text>&amp;</text> '; exit 1; '".into(),
        };
        let plain = Toast {
            title: "toryo — T".into(),
            body: "ok".into(),
        };
        let (a, b) = (toast_command(&hostile, false), toast_command(&plain, false));
        assert_eq!(
            (&a.program, &a.args),
            (&b.program, &b.args),
            "constant command line"
        );
        for arg in &a.args {
            assert!(
                !arg.contains("Remove-Item") && !arg.contains("whoami"),
                "{arg}"
            );
        }
        assert_eq!(a.env[0], (ENV_TITLE.to_string(), hostile.title.clone()));
        assert_eq!(a.env[1], (ENV_BODY.to_string(), hostile.body.clone()));
        assert_eq!(a.env[2], (ENV_DRY_RUN.to_string(), "0".to_string()));
        assert_eq!(toast_command(&plain, true).env[2].1, "1");
        // the encoded script is exactly SCRIPT, which reads the text as data only
        let utf16: Vec<u8> = SCRIPT.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(a.args.last().unwrap(), &base64(&utf16));
        assert!(SCRIPT.contains("CreateTextNode([string]$env:TORYO_TOAST_BODY)"));
        for bad in [
            "Invoke-Expression",
            "iex ",
            "LoadXml",
            "InnerXml",
            "-f $env",
        ] {
            assert!(!SCRIPT.contains(bad), "{bad}");
        }
        assert_eq!(
            a.args[..3],
            ["-NoProfile", "-NonInteractive", "-EncodedCommand"]
        );
        assert!(a.program.ends_with("powershell.exe"));
    }

    #[test]
    fn base64_matches_rfc_4648() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(i.as_bytes()), o, "{i}");
        }
    }
}
