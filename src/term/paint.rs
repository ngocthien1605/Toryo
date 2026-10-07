//! Colour for the lines Toryo prints (M15-02).
//!
//! Colour is presentation only: [`block`] wraps parts of an already inert line
//! ([`super::text`]) in SGR sequences and changes no character of it — with the sequences
//! removed, the painted text is the text. What a line means is read from its shape (`✔`, `✘`,
//! `!!`, a state name, a stage header), so the live lines of `toryo run` and the lines `toryo
//! watch` renders from the recorded events are painted the same.
//!
//! Only the **first** line of a printed block is looked at (and the `next:` line under an end
//! banner, the `→` line of a next action): the following lines of a reason or a note are text
//! somebody else may have written, and stay unpainted.

use std::sync::OnceLock;

use anstyle::{AnsiColor, Color, Style};
use regex::Regex;

const fn fg(c: AnsiColor) -> Style {
    Style::new().fg_color(Some(Color::Ansi(c)))
}

/// Passed, done, alive.
pub const OK: Style = fg(AnsiColor::Green);
/// Failed, escalated.
pub const BAD: Style = fg(AnsiColor::Red);
/// Waiting for the owner, paused, a warning.
pub const WAIT: Style = fg(AnsiColor::Yellow);
/// Bookkeeping: state transitions, turn and attempt lines.
pub const DIM: Style = Style::new().dimmed();
pub const BOLD: Style = Style::new().bold();
/// A stage header.
pub const HEAD: Style = fg(AnsiColor::Cyan).bold();
/// A command to type.
pub const CMD: Style = fg(AnsiColor::Cyan);

/// The names of `PipelineState`, as `{:?}` prints them.
const STATES: &str = "New|Planning|WaitingPlanApproval|Implementing|Checkpointing|Verifying|Fixing|Reviewing|WaitingFinalApproval|Escalated|Paused|Completed|Cancelled|Failed";

fn put(s: Style, t: &str) -> String {
    format!("{}{t}{}", s.render(), s.render_reset())
}

/// The colour of a state: green done, red needs a decision after a failure, yellow waits for
/// the owner, plain bold while it runs.
pub fn state(name: &str) -> Style {
    match name {
        "Completed" => OK.bold(),
        "Escalated" | "Failed" => BAD.bold(),
        "WaitingPlanApproval" | "WaitingFinalApproval" | "Paused" => WAIT.bold(),
        "Cancelled" => DIM,
        _ => BOLD,
    }
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("regex"))
}

fn banner() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"^══════════ .*: ([A-Za-z]+) ══════════$")
}

fn status_row() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, &format!(r"^(\S+\s+)({STATES})(\s+fix .*)$"))
}

fn doctor_tag() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"^\[(OK  |info|WARN|FAIL)\]( .*)$")
}

fn doctor_sum() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"^(\d+) FAIL, (\d+) WARN$")
}

fn attempt() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"^   \S+ attempt \d+: ")
}

fn diagnostic() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"^(.*\): )(error|warning)( .*)$")
}

fn gate_name() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"^( )(\S+)( \(.*)$")
}

/// The commands of a line — the text between backticks — in the command colour.
fn commands(l: &str) -> String {
    let mut out = String::with_capacity(l.len() + 16);
    let mut rest = l;
    while let Some(a) = rest.find('`') {
        let Some(b) = rest[a + 1..].find('`') else {
            break;
        };
        let end = a + 1 + b + 1;
        out.push_str(&rest[..a]);
        out.push_str(&put(CMD, &rest[a..end]));
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// A `next:` line: the label bold, the commands in their colour.
fn next_line(indent: &str, rest: &str) -> String {
    format!("{indent}{}{}", put(BOLD, "next:"), commands(rest))
}

/// The first line of a block.
fn head(l: &str) -> String {
    let t = l.trim_start();
    let indent = &l[..l.len() - t.len()];
    if let Some(c) = banner().captures(l) {
        return put(state(&c[1]), l);
    }
    if t.starts_with("── ") && (t.ends_with(" ──") || t.ends_with(" …")) {
        return format!("{indent}{}", put(HEAD, t));
    }
    if l.starts_with("   [state] ")
        || l.starts_with("   turn ")
        || l.starts_with("   context: ")
        || attempt().is_match(l)
        || t.starts_with("summary: ")
        || t.starts_with("note: ")
        || (t.starts_with('(') && t.ends_with("approved automatically: --yes)"))
    {
        return format!("{indent}{}", put(DIM, t));
    }
    if t.starts_with("!! budget:")
        || l.starts_with("   gate open: ")
        || l.starts_with("   waiting for lock ")
    {
        return format!("{indent}{}", put(WAIT, t));
    }
    for (sym, style) in [
        ("!!", BAD.bold()),
        ("✔", OK),
        ("✘", BAD),
        ("■", WAIT),
        ("▶", BOLD),
    ] {
        if let Some(rest) = t.strip_prefix(sym) {
            // a gate's result line (`  ✔ build (exit 0, 1.2s)`): the gate's name stands out
            let rest = match gate_name().captures(rest) {
                Some(c) if indent == "  " => format!("{}{}{}", &c[1], put(BOLD, &c[2]), &c[3]),
                _ => rest.to_string(),
            };
            return format!("{indent}{}{rest}", put(style, sym));
        }
    }
    for (word, style) in [
        ("WARNING:", WAIT),
        ("warning:", WAIT),
        ("error:", BAD.bold()),
        ("task_active:", BAD),
    ] {
        if let Some(rest) = t.strip_prefix(word) {
            return format!("{indent}{}{rest}", put(style, word));
        }
    }
    if let Some(rest) = t.strip_prefix("next:") {
        return next_line(indent, rest);
    }
    if let Some(rest) = t.strip_prefix("final gate —") {
        return format!("{indent}{}{rest}", put(WAIT, "final gate —"));
    }
    if let Some(c) = doctor_tag().captures(l) {
        let style = match &c[1] {
            "OK  " => OK,
            "WARN" => WAIT,
            "FAIL" => BAD.bold(),
            _ => DIM,
        };
        return format!("[{}]{}", put(style, &c[1]), &c[2]);
    }
    if let Some(c) = doctor_sum().captures(l) {
        let style = match (&c[1], &c[2]) {
            ("0", "0") => OK,
            ("0", _) => WAIT,
            _ => BAD.bold(),
        };
        return put(style, l);
    }
    if let Some(c) = status_row().captures(l) {
        let rest = match c[3].split_once(" · ACTIVE (") {
            Some((a, b)) => format!("{a} · {}", put(OK, &format!("ACTIVE ({b}"))),
            None => c[3].to_string(),
        };
        return format!("{}{}{rest}", &c[1], put(state(&c[2]), &c[2]));
    }
    if let Some(c) = diagnostic().captures(l) {
        let style = if &c[2] == "error" { BAD } else { WAIT };
        return format!("{}{}{}", &c[1], put(style, &c[2]), &c[3]);
    }
    l.to_string()
}

/// A following line: only what Toryo itself puts there.
fn tail(l: &str, under_banner: bool) -> String {
    if under_banner {
        if let Some(rest) = l.strip_prefix("next:") {
            return next_line("", rest);
        }
    }
    if l.starts_with("  → ") {
        return commands(l);
    }
    if let Some(rest) = l.strip_prefix("!! stopped with an error:") {
        return format!("{}{rest}", put(BAD.bold(), "!! stopped with an error:"));
    }
    l.to_string()
}

/// A printed block (one `errln!` / `outln!`, one rendered event) with its colours.
pub fn block(t: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let (mut first, mut under_banner) = (true, false);
    for l in t.split('\n') {
        if l.trim().is_empty() {
            out.push(l.to_string());
            continue;
        }
        if first {
            out.push(head(l));
            under_banner = banner().is_match(l);
            first = false;
        } else {
            out.push(tail(l, under_banner));
            under_banner = false;
        }
    }
    out.join("\n")
}

/// How wide the rules of [`frame`] are, in characters.
const FRAME: usize = 64;

/// The end of a run as an open frame: a rule with `title`, the lines behind a left rail, a
/// closing rule — in the colour of `state_name`. No right edge: a line may be as long as it is
/// (a path, a reason) and wraps as the terminal wraps it. Each entry of `lines` is one block.
pub fn frame(title: &str, state_name: &str, lines: &[String]) -> String {
    let st = state(state_name);
    let rail = Style::new().fg_color(st.get_fg_color());
    let used = 4 + title.chars().count() + 1;
    let mut out = vec![format!(
        "{}{}{}",
        put(rail, "╭── "),
        put(st, title),
        put(
            rail,
            &format!(" {}", "─".repeat(FRAME.saturating_sub(used).max(2)))
        )
    )];
    for l in lines {
        for row in block(l).split('\n') {
            out.push(format!("{} {row}", put(rail, "│")));
        }
    }
    out.push(put(rail, &format!("╰{}", "─".repeat(FRAME - 1))));
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::strip_ansi;

    const GREEN: &str = "\u{1b}[32m";
    const RED: &str = "\u{1b}[31m";
    const YELLOW: &str = "\u{1b}[33m";

    /// Lines of every kind Toryo prints: the events as `render` writes them, `status`,
    /// `doctor`, a gate's result and diagnostics, the end of a run.
    fn corpus() -> Vec<&'static str> {
        vec![
            "▶ T-1 — run started (generation 2, me pid 7)",
            "   [state] Verifying --verify_failed--> Fixing",
            "\n── PLAN (attempt 1, claude-max) ──",
            "── build (Projects/Build/A.rnrproj) …",
            "   plan attempt 1: failed, timeout (12.0s)",
            "   turn implement/1 (codex-openai): completed · 520114 tokens, exact (250.3s)",
            "   context: 3063 bytes, 3 sources, sha256 fe677f31a2d1",
            "✔ checkpoint 1a2b3c4d5e6f (implement)",
            "  ✔ build (exit 0, 1.2s) — check gate",
            "  ✘ build (exit 1, 192.4s) — check gate",
            "   waiting for lock d365-env (held by T-12)",
            "   gate open: final (occurrence 1)",
            "!! budget: 1700000 of 2000000 tokens used (≥ 80 %; admission cap, may overshoot by one turn)",
            "!! the reviewer blocked the task\nsecond line of the reason\n══════════ T-1: Completed ══════════",
            "══════════ T-1: WaitingPlanApproval ══════════\nnext: review PLAN.md, then `toryo approve T-1 --gate plan --continue` or `toryo reject T-1 --gate plan --reason \"…\"`",
            "══════════ T-1: Escalated ══════════\nnext: max_fix_cycles: not accepted\n  → `toryo resume T-1 --note \"…\"`, or give up: `toryo task cleanup T-1`\n!! stopped with an error: boom",
            "next: done — merge `toryo/T-1` yourself",
            "final gate — verify: compile only (X++: no DB sync, no unit tests)",
            "summary: C:\\data\\runs\\T-1\\runstore\\SUMMARY.md",
            "(Plan gate approved automatically: --yes)",
            "SJP-1234       WaitingFinalApproval   fix 1/3 · tokens 812400",
            "SJP-1240       Implementing           fix 0/3 · tokens 96210 · ACTIVE (me pid 7)",
            "               next: `toryo approve SJP-1234 --gate final`",
            "[OK  ] config                 loaded",
            "[info] profile                claude-max — machine",
            "[WARN] paths                  core.longpaths is not set",
            "[FAIL] gates                  pwsh not found",
            "\n1 FAIL, 2 WARN",
            "  src\\A.xpp(12,5): error CS0103: The name 'x' does not exist",
            "  src\\A.xpp(14,1): warning BP1: unused",
            "WARNING: sandbox_mode is `read-only`",
            "error: no such task",
            "■ stopped at a stage boundary (nothing was interrupted)",
            "a plain line with `backticks` that is nobody's command",
            "",
        ]
    }

    /// Colour changes no character: without the sequences the painted text is the text.
    #[test]
    fn painting_changes_no_character() {
        for l in corpus() {
            assert_eq!(strip_ansi(&block(l)), l, "{l:?}");
        }
        let lines = vec![
            "final gate — note".to_string(),
            "next: reason\n  → `toryo resume T-1`".to_string(),
            "summary: C:\\x\\SUMMARY.md".to_string(),
        ];
        let f = strip_ansi(&frame("T-1: Escalated", "Escalated", &lines));
        assert_eq!(
            f,
            format!(
                "╭── T-1: Escalated {}\n│ final gate — note\n│ next: reason\n│   → `toryo resume T-1`\n│ summary: C:\\x\\SUMMARY.md\n╰{}",
                "─".repeat(64 - 19),
                "─".repeat(63)
            )
        );
    }

    #[test]
    fn a_line_is_painted_by_what_it_means() {
        let has = |l: &str, code: &str| {
            assert!(block(l).contains(code), "{:?} in {:?}", code, block(l));
        };
        has("✔ checkpoint 1a2b (implement)", GREEN);
        has("  ✘ build (exit 1, 2.0s)", RED);
        has("!! the reviewer blocked the task", RED);
        has("   gate open: plan (occurrence 1)", YELLOW);
        has("══════════ T-1: Completed ══════════", GREEN);
        has("══════════ T-1: Escalated ══════════", RED);
        has("══════════ T-1: WaitingFinalApproval ══════════", YELLOW);
        has(
            "T-1            Paused                 fix 0/2 · tokens 5",
            YELLOW,
        );
        has("[FAIL] gates                  pwsh not found", RED);
        has("\n0 FAIL, 0 WARN", GREEN);
        has("\n0 FAIL, 3 WARN", YELLOW);
        // a gate's name is bold; a state that runs is bold, not coloured
        assert!(block("  ✔ build (exit 0, 1.2s)").contains("\u{1b}[1mbuild\u{1b}[0m"));
        let running = block("T-1            Implementing           fix 0/2 · tokens 5");
        assert!(running.contains("\u{1b}[1mImplementing") && !running.contains(GREEN));
        // the command to type stands out in a `next:` line
        assert!(block("next: `toryo run T-1`").contains("\u{1b}[36m`toryo run T-1`\u{1b}[0m"));
    }

    /// Only the first line of a block is Toryo's own: a reason whose second line looks like
    /// an end banner, a check mark or a `next:` line is not painted like one.
    #[test]
    fn text_under_the_first_line_is_not_painted() {
        let forged = "!! the reviewer blocked the task\n══════════ T-1: Completed ══════════\n✔ all good\nnext: `evil`";
        let painted = block(forged);
        let mut rows = painted.split('\n');
        assert!(rows.next().unwrap().contains(RED));
        for row in rows {
            assert!(!row.contains('\u{1b}'), "{row:?}");
        }
        // a line that is nobody's: untouched
        assert_eq!(block("just text with `ticks`"), "just text with `ticks`");
        // `next:` is painted directly under a real banner only
        let end = block("══════════ T-1: Escalated ══════════\nnext: a\nnext: `b`");
        let rows: Vec<&str> = end.split('\n').collect();
        assert!(
            rows[1].contains('\u{1b}') && !rows[2].contains('\u{1b}'),
            "{rows:?}"
        );
    }
}
