//! Notifications (M10-D1): a Windows toast when a task needs the owner — a gate opens, the task
//! escalates or pauses, or a run ends (`Completed`, `Failed`, or with an error). A [`Notifier`] is
//! an engine event sink ([`crate::engine::flight`]); what it shows is decided by the pure
//! [`toast_for`], and a [`Toaster`] shows it (a recording double in tests).
//!
//! The Windows toaster starts `powershell.exe -NoProfile -NonInteractive -Command <fixed script>`:
//! the script is a constant and reads the title and the body from environment variables, so
//! agent-written text (a reason, R61) is data — never part of a command line or a script. The
//! text is cleaned first (ANSI sequences and characters XML 1.0 forbids, R4). The process inherits
//! no handle and has no console window (R4); it is not in a task's Job; toryo waits for it at exit
//! at most until its 10 s deadline and kills it after that. Off by default (`notify: { toast:
//! true }` in the machine config, or `--notify`). Other platforms: nothing is shown.

#[cfg(windows)]
use std::sync::Mutex;
use std::time::Duration;
#[cfg(windows)]
use std::time::Instant;

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

impl Toast {
    /// A toast with cleaned text ([`clean`]) and the body shortened to [`BODY_MAX`].
    pub fn new(title: &str, body: &str) -> Self {
        Self {
            title: clean(title),
            body: shorten(&clean(body), BODY_MAX),
        }
    }
}

/// Is `c` a character XML 1.0 allows (`Char`), other than the tab and line breaks?
fn xml_char(c: char) -> bool {
    matches!(c, '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}

/// Text for a toast (R4): ANSI CSI/OSC sequences removed, every character XML 1.0 forbids (control
/// characters, U+FFFE, U+FFFF) and tabs and line breaks turned into spaces, runs of spaces
/// collapsed — the toast XML would be dropped silently otherwise.
pub fn clean(s: &str) -> String {
    let s = crate::process::strip_ansi(s);
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let c = if xml_char(c) { c } else { ' ' };
        if !(c == ' ' && out.ends_with(' ')) {
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// At most `max` characters (an ellipsis marks a cut); NUL characters, which an environment
/// variable cannot hold, are dropped.
pub fn shorten(s: &str, max: usize) -> String {
    let s: String = s.chars().filter(|c| *c != '\0').collect();
    crate::engine::flight::shorten(&s, max)
}

/// The toast an event raises, if any: the gates, an escalation, a pause, the end of a run that
/// stopped with an error, and the end of a run that reached `Completed` or `Failed`. Title
/// `toryo — <task>`; body: the state and the next action.
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
            format!(
                "{:?} (`toryo status {id}` for what to do) — {reason}",
                St::Escalated
            )
        }
        EngineEvent::Paused { reason } if reason == "budget" => format!(
            "{:?} — budget: `toryo budget set {id} --tokens N`, then `toryo resume {id}`",
            St::Paused
        ),
        EngineEvent::Paused { reason } => {
            format!("{:?} (`toryo resume {id}`) — {reason}", St::Paused)
        }
        // R1: a drive that stopped with an error always tells the owner
        EngineEvent::RunEnded {
            state,
            error: Some(err),
            ..
        } => format!("{state:?} — stopped with an error: {err}"),
        // the end state only when this drive reached it (not `run` on a finished task, not the
        // `--continue` after the final approval that completed it)
        EngineEvent::RunEnded {
            state,
            next_action,
            from,
            error: None,
        } if matches!(state, St::Completed | St::Failed) && *from != Some(*state) => {
            format!("{state:?} — {next_action}")
        }
        _ => return None,
    };
    Some(Toast::new(&format!("toryo — {id}"), &body))
}

/// Whether toasts are on for a drive command: `--notify` and `--no-notify` (mutually exclusive)
/// override the machine config (default off).
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
    /// `false` under `--yes` (owner decision Q2): the gates are approved automatically, nobody
    /// has to act on them.
    gates: bool,
}

impl Notifier {
    pub fn new(toaster: Box<dyn Toaster>) -> Self {
        Self {
            toaster,
            gates: true,
        }
    }

    /// The platform's toaster: the Windows toast, elsewhere nothing.
    pub fn system() -> Self {
        Self::new(Box::new(SystemToaster))
    }

    /// No toast when a gate opens (`--yes`); escalations, pauses and the end still toast.
    pub fn without_gates(mut self) -> Self {
        self.gates = false;
        self
    }
}

impl EventSink for Notifier {
    fn name(&self) -> &str {
        "notifier"
    }

    fn emit(&self, e: &Envelope) -> Result<()> {
        if !self.gates && matches!(e.event, EngineEvent::GateOpened { .. }) {
            return Ok(());
        }
        match toast_for(e) {
            Some(t) => self.toaster.show(&t),
            None => Ok(()),
        }
    }
}

/// The toast script (Windows PowerShell 5.1, which loads WinRT types). Constant: the title and
/// the body come from [`ENV_TITLE`] and [`ENV_BODY`] and become XML text nodes (`CreateTextNode`),
/// never code or markup. It holds no double quote, so it passes as one `-Command` argument without
/// any escaping (R4: `-EncodedCommand` from an unsigned exe is a common EDR alert pattern).
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

/// What the toaster starts: the program, its (constant) arguments, and the variables that carry
/// the toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastCommand {
    pub program: std::path::PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// The toast command for `t`. Only `env` depends on the toast.
pub fn toast_command(t: &Toast, dry_run: bool) -> ToastCommand {
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
            "-Command".into(),
            SCRIPT.into(),
        ],
        env: vec![
            (ENV_TITLE.into(), t.title.clone()),
            (ENV_BODY.into(), t.body.clone()),
            (ENV_DRY_RUN.into(), if dry_run { "1" } else { "0" }.into()),
        ],
    }
}

/// toryo's environment with `extra` set (names compare case-insensitively, as on Windows).
pub fn child_env(
    base: impl Iterator<Item = (String, String)>,
    extra: &[(String, String)],
) -> std::collections::BTreeMap<String, String> {
    let mut env: std::collections::BTreeMap<String, String> = base
        .filter(|(k, _)| !extra.iter().any(|(e, _)| e.eq_ignore_ascii_case(k)))
        .collect();
    env.extend(extra.iter().cloned());
    env
}

/// A running toast process and its deadline.
#[cfg(windows)]
struct Pending {
    process: crate::process::windows::Detached,
    deadline: Instant,
}

/// The toast processes started by this toryo; [`wait_pending`] settles them before it exits.
#[cfg(windows)]
static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());

/// Start the toast process (Windows): no inherited handle, no console window.
#[cfg(windows)]
fn spawn(t: &Toast, dry_run: bool) -> Result<crate::process::windows::Detached> {
    let c = toast_command(t, dry_run);
    let base = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)));
    crate::process::windows::spawn_detached(&c.program, &c.args, &child_env(base, &c.env))
}

/// Wait for `p` until `deadline`, then kill it: the exit code, `None` when killed.
#[cfg(windows)]
fn settle(p: &crate::process::windows::Detached, deadline: Instant) -> Option<u32> {
    let left = deadline.saturating_duration_since(Instant::now());
    let code = p.wait(left);
    if code.is_none() {
        p.kill();
    }
    code
}

/// Before toryo exits: wait for the toast processes it started, each until its 10 s deadline
/// (usually well under a second), and kill those still running (R4). The watcher is toryo itself:
/// a thread would die with the process right after most toasts.
pub fn wait_pending() {
    #[cfg(windows)]
    {
        let pending = std::mem::take(&mut *PENDING.lock().unwrap_or_else(|p| p.into_inner()));
        for p in pending {
            settle(&p.process, p.deadline);
        }
    }
}

/// The platform's toaster: on Windows the toast process, settled at exit; nothing elsewhere.
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
        #[cfg(windows)]
        {
            let process = spawn(t, false)?;
            let mut pending = PENDING.lock().unwrap_or_else(|p| p.into_inner());
            // the ones that are done need no more waiting
            pending.retain(|p| p.process.wait(Duration::ZERO).is_none());
            pending.push(Pending {
                process,
                deadline: Instant::now() + TOAST_TIMEOUT,
            });
        }
        Ok(())
    }
}

/// Run the toast script for `t` and wait for it, up to [`TOAST_TIMEOUT`]: its exit code (`124`
/// when it was stopped). `dry_run`: build the toast, do not show it. An error off Windows.
pub fn run_script(t: &Toast, dry_run: bool) -> Result<i32> {
    #[cfg(windows)]
    {
        let p = spawn(t, dry_run)?;
        Ok(settle(&p, Instant::now() + TOAST_TIMEOUT).map_or(124, |c| c as i32))
    }
    #[cfg(not(windows))]
    {
        let _ = (t, dry_run);
        anyhow::bail!("toasts are Windows only: nothing to test on this platform")
    }
}

/// `toryo notify-test`: show a sample toast (or with `dry_run` only build it).
pub fn test_toast(dry_run: bool) -> Result<i32> {
    run_script(
        &Toast::new(
            "toryo — notify-test",
            "Toasts work: toryo shows one like this at a gate, an escalation, a pause and the end of a run.",
        ),
        dry_run,
    )
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
                from: Some(St::Reviewing),
                error: None,
            },
            EngineEvent::RunEnded {
                state: St::Escalated,
                next_action: "resume".into(),
                from: Some(St::Reviewing),
                error: None,
            },
            EngineEvent::RunEnded {
                state: St::Completed,
                next_action: "done — merge `toryo/BR-9` yourself".into(),
                from: Some(St::Reviewing),
                error: None,
            },
            EngineEvent::RunEnded {
                state: St::Failed,
                next_action: "failed: x — `toryo task cleanup BR-9`".into(),
                from: Some(St::Reviewing),
                error: None,
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

    /// R4 (Q2): under `--yes` the gates raise no toast; everything else still does.
    #[test]
    fn without_gates_the_gates_are_silent() {
        let r = Recorded::default();
        let n = Notifier::new(Box::new(r.clone())).without_gates();
        for e in every_event() {
            n.emit(&env(e)).unwrap();
        }
        let got = r.0.lock().unwrap().clone();
        assert_eq!(got.len(), 5, "{got:#?}");
        assert!(got.iter().all(|t| !t.body.starts_with("Waiting")));
    }

    /// R1 AC-2: an end with an error always toasts; the end state only when this drive reached it.
    #[test]
    fn a_run_end_toasts_on_an_error_or_a_new_end_state() {
        let end = |state, from, error: Option<&str>| {
            toast_for(&env(EngineEvent::RunEnded {
                state,
                next_action: "next".into(),
                from,
                error: error.map(str::to_string),
            }))
        };
        let t = end(St::New, Some(St::New), Some("unverified profiles (R51)")).unwrap();
        assert_eq!(
            t.body,
            "New — stopped with an error: unverified profiles (R51)"
        );
        assert!(end(St::Implementing, Some(St::Planning), Some("disk full")).is_some());
        // `run` on a task that was already Completed, and `approve --gate final --continue`
        // (the approval completed it before the drive started): no second toast
        assert!(end(St::Completed, Some(St::Completed), None).is_none());
        assert!(end(St::Failed, Some(St::Failed), None).is_none());
        assert!(end(St::Completed, Some(St::Reviewing), None).is_some());
        assert!(
            end(St::Completed, None, None).is_some(),
            "an older line without `from`"
        );
        assert!(end(St::WaitingPlanApproval, Some(St::Planning), None).is_none());
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

    /// R4: agent text carries ANSI colours, BEL, U+FFFE, tabs and line breaks; the toast XML
    /// allows none of them.
    #[test]
    fn toast_text_is_cleaned_for_xml() {
        assert_eq!(
            clean("\u{1b}[31merror\u{1b}[0m:\tbad\r\nline\u{7}\u{fffe}\u{ffff} end  "),
            "error: bad line end"
        );
        assert_eq!(
            clean("\u{1b}]0;title\u{7}x\u{1b}]8;;http://a\u{1b}\\y"),
            "xy",
            "OSC sequences"
        );
        assert_eq!(clean("a\u{0}b\u{b}c\u{c}d"), "a b c d");
        assert_eq!(clean("ありがとう — Cảm ơn"), "ありがとう — Cảm ơn");
        assert_eq!(
            clean("\u{1F600} ok"),
            "\u{1F600} ok",
            "astral characters are XML chars"
        );
        let t = toast_for(&env(EngineEvent::Escalated {
            reason_class: "other".into(),
            reason: "boom\u{1b}[1m\u{7}\n\nagain".into(),
        }))
        .unwrap();
        assert!(t.body.ends_with("— boom again"), "{}", t.body);
    }

    #[test]
    fn nothing_is_sent_when_toasts_are_off() {
        let off = Notify { toast: false };
        let on = Notify { toast: true };
        assert!(!enabled(false, false, off), "default off");
        assert!(enabled(false, false, on));
        assert!(enabled(true, false, off), "--notify");
        assert!(!enabled(false, true, on), "--no-notify");
    }

    /// AC-2: agent-written text is data. The program and the arguments do not depend on the
    /// toast; the text travels only in the environment, and the script reads it as text nodes.
    #[test]
    fn hostile_text_is_passed_as_data() {
        let hostile = Toast {
            title: "toryo — T\"; Remove-Item -Recurse C:\\ #".into(),
            body: "it's \"quoted\" $(Remove-Item x) `whoami` $env:PATH\r\nline 2 <text>&amp;</text> '; exit 1; '".into(),
        };
        let plain = Toast::new("toryo — T", "ok");
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
        // the script is passed as it is, and reads the text as data only
        assert_eq!(
            a.args,
            ["-NoProfile", "-NonInteractive", "-Command", SCRIPT]
        );
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
        assert!(a.program.ends_with("powershell.exe"));
    }

    /// R4: `-Command` takes the script as one argument; it must need no quoting of its own.
    #[test]
    fn the_script_holds_no_double_quote() {
        assert!(!SCRIPT.contains('"'));
        assert!(
            !SCRIPT.ends_with('\\'),
            "a trailing backslash would escape the closing quote"
        );
        let quoted = crate::process::quote_windows_arg(SCRIPT);
        assert_eq!(quoted, format!("\"{SCRIPT}\""), "wrapped, nothing escaped");
    }

    /// R4: a toast process still running at its deadline is killed.
    #[cfg(windows)]
    #[test]
    fn a_toast_process_is_killed_at_its_deadline() {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let ps = std::path::PathBuf::from(root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        let args: Vec<String> = [
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 30",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let p = crate::process::windows::spawn_detached(&ps, &args, &std::env::vars().collect())
            .unwrap();
        let t = Instant::now();
        assert_eq!(settle(&p, t + Duration::from_millis(500)), None);
        assert!(t.elapsed() < Duration::from_secs(5));
        assert!(p.wait(Duration::from_secs(5)).is_some(), "killed");
    }

    #[test]
    fn the_toast_variables_replace_inherited_ones() {
        let base = vec![
            ("Path".to_string(), "C:\\x".to_string()),
            (
                "toryo_toast_body".to_string(),
                "from the parent".to_string(),
            ),
        ];
        let extra = vec![(ENV_BODY.to_string(), "mine".to_string())];
        let env = child_env(base.into_iter(), &extra);
        assert_eq!(env.get(ENV_BODY).map(String::as_str), Some("mine"));
        assert!(!env.contains_key("toryo_toast_body"));
        assert_eq!(env.get("Path").map(String::as_str), Some("C:\\x"));
    }
}
