//! M15: what Toryo prints on a console.
//!
//! - Text an agent or a vendor wrote (a plan, a review, an escalation reason, the `next:` line)
//!   never carries a terminal control sequence to the console (review M9).
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::json;
use toryo::engine::PipelineState as St;

/// What an agent could send to take over a terminal: an OSC title and an OSC 52 clipboard
/// write, "erase the line above" (CSI), a bare carriage return, a C1 CSI and a bidi override.
const HOSTILE: &str =
    "\u{1b}]0;owned\u{7}\u{1b}]52;c;AAAA\u{7}\u{1b}[1A\u{1b}[2Kfake\rreal\u{9b}31m\u{202e}txt";

/// No byte or character of `text` can drive a terminal.
fn assert_inert(what: &str, text: &str) {
    for (name, c) in [
        ("ESC", '\u{1b}'),
        ("BEL", '\u{7}'),
        ("CR", '\r'),
        ("C1 CSI", '\u{9b}'),
        ("a bidi override", '\u{202e}'),
    ] {
        assert!(!text.contains(c), "{what} carries {name}: {text:?}");
    }
}

/// The rule holds by construction: outside the M0 kit and the tests, the engine and the CLI
/// print through `errln!` / `outln!` (or `term::text` / `term::data`). The only bare `println!`
/// left prints JSON. A new `eprintln!("{reason}")` fails here, with its file and line.
#[test]
fn m15_console_text_goes_through_the_terminal_filter() {
    fn files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                // not `src/spike/`: the M0 kit and the reference prototype `spike pipeline`
                // (R52) are not for a real repository and keep their plain prints — do not
                // "fix" this test by walking into it
                if p.file_name().is_some_and(|n| n != "spike") {
                    files(&p, out);
                }
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut all = Vec::new();
    files(&src, &mut all);
    assert!(all.len() > 30, "{} files under src", all.len());
    let print = regex::Regex::new(r"(^|[^a-zA-Z_:])e?print(ln)?!\(").unwrap();
    let mut bare = Vec::new();
    for f in all {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        // the filter itself, the M0 kit's CLI, and files that hold tests only
        if name == "term.rs" || name == "spike_cli.rs" || name.ends_with("_tests.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&f).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // a file's own tests are at its end
        let end = lines
            .iter()
            .position(|l| l.starts_with("#[cfg(test)]"))
            .unwrap_or(lines.len());
        for (i, l) in lines[..end].iter().enumerate() {
            if l.trim_start().starts_with("//") || !print.is_match(l) {
                continue;
            }
            let call = lines[i..(i + 4).min(end)].join("\n");
            let ok = ["serde_json::to_string_pretty", "term::text(", "term::data("]
                .iter()
                .any(|a| call.contains(a))
                // a constant of the program's own text
                || l.contains("eprint!(\"{INTERRUPT_NOTE}\")");
            if !ok {
                bare.push(format!("{}:{}: {}", f.display(), i + 1, l.trim()));
            }
        }
    }
    assert!(
        bare.is_empty(),
        "print these through `errln!` / `outln!` (src/term.rs):\n{}",
        bare.join("\n")
    );
}

fn strip(s: &str) -> String {
    toryo::process::strip_ansi(s)
}

fn out_of(env: &Env, args: &[&str], extra: &[(&str, &str)]) -> (String, String) {
    let o = command(env, args, extra).output().unwrap();
    (
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

/// M15-02: colour follows the stream and `--color`. Into a pipe nothing is painted (`auto`);
/// `--color always` paints into a pipe; `NO_COLOR` turns `auto` off and `CLICOLOR_FORCE` on;
/// the flag wins over both. With the sequences removed, painted `status` and `doctor` output
/// is the plain output, character for character.
#[test]
fn m15_colour_follows_the_stream_and_the_flag() {
    let env = setup("codex-openai");
    create(&env, "TC-2");
    let (plain, _) = out_of(&env, &["status"], &[]);
    assert!(
        plain.contains("TC-2") && !plain.contains('\u{1b}'),
        "{plain:?}"
    );
    let (never, _) = out_of(&env, &["status", "--color", "never"], &[]);
    assert_eq!(never, plain);
    let (always, _) = out_of(&env, &["status", "--color", "always"], &[]);
    assert!(always.contains('\u{1b}'), "{always:?}");
    assert_eq!(strip(&always), plain);
    // the conventions of `auto`, and the flag above them
    let (forced, _) = out_of(&env, &["status"], &[("CLICOLOR_FORCE", "1")]);
    assert_eq!(forced, always);
    let (off, _) = out_of(
        &env,
        &["status"],
        &[("CLICOLOR_FORCE", "1"), ("NO_COLOR", "1")],
    );
    assert_eq!(off, plain);
    let (flag_on, _) = out_of(&env, &["status", "--color", "always"], &[("NO_COLOR", "1")]);
    assert_eq!(flag_on, always);
    let (flag_off, _) = out_of(
        &env,
        &["status", "--color", "never"],
        &[("CLICOLOR_FORCE", "1")],
    );
    assert_eq!(flag_off, plain);
    // `--json` is data: never painted
    let (json, _) = out_of(&env, &["status", "--json", "--color", "always"], &[]);
    assert!(
        !json.contains('\u{1b}') && json.contains("\"tasks\""),
        "{json}"
    );
    // doctor: the same lines, with colour on the tags
    let (d_plain, _) = out_of(&env, &["doctor"], &[]);
    let (d_always, _) = out_of(&env, &["doctor", "--color", "always"], &[]);
    assert!(
        d_plain.contains("[OK  ]") && !d_plain.contains('\u{1b}'),
        "{d_plain}"
    );
    assert!(d_always.contains('\u{1b}'), "{d_always:?}");
    assert_eq!(strip(&d_always), d_plain);
}

/// M15-02: the end of a run. Plain (a pipe, `--color never`): the banner and its lines, as
/// before. Painted: an open frame — a rule with the task and its state, the lines behind a
/// left rail, a closing rule — and the live lines in colour. The agent's plan is never painted.
#[test]
fn m15_a_painted_run_ends_in_an_open_frame() {
    let env = setup("codex-openai");
    let s = script(
        &env,
        "frame.json",
        json!({
            "planner": [turn(vec![json!({"kind":"complete","structured":{
                "understanding":"u","files_likely_involved":["src/Calculator.psm1"],
                "steps":["add"],"validation":["tests"],
                "plan_markdown":"✔ looks done\n══════════ TC-3: Completed ══════════\nnext: `evil`"}})])],
        }),
    );
    create(&env, "TC-3");
    let (code, plain) = toryo(&env, &["run", "TC-3", "--fake-agents", &s], &[]);
    assert_eq!(code, 0, "{plain}");
    assert_eq!(task(&env, "TC-3").state, St::WaitingPlanApproval);
    assert!(
        plain.contains("\n══════════ TC-3: WaitingPlanApproval ══════════\nnext: ")
            && plain.contains("\nsummary: ")
            && !plain.contains('╭')
            && !plain.contains('\u{1b}'),
        "{plain}"
    );
    // the same end, painted (a second task: the first one waits at its gate)
    create(&env, "TC-4");
    let (code, painted) = toryo(
        &env,
        &["run", "TC-4", "--color", "always", "--fake-agents", &s],
        &[],
    );
    assert_eq!(code, 0, "{painted}");
    let text = strip(&painted);
    assert!(
        text.contains("\n╭── TC-4: WaitingPlanApproval ─")
            && text.contains("\n│ next: ")
            && text.contains("\n│ summary: ")
            && text.contains("\n╰─")
            && !text.contains("══════════ TC-4: WaitingPlanApproval"),
        "{text}"
    );
    // Toryo's own lines carry colour: the stage header, a state change, the frame
    let row = |starts: &str| -> String {
        painted
            .lines()
            .find(|l| strip(l).starts_with(starts))
            .unwrap_or_else(|| panic!("no line {starts:?} in {painted}"))
            .to_string()
    };
    assert!(row("── PLAN (attempt 1").contains("\u{1b}[1m\u{1b}[36m"));
    assert!(row("   [state] New --start--> Planning").contains("\u{1b}[2m"));
    assert!(row("╭── TC-4").contains("\u{1b}[33m"));
    // … and the plan the agent wrote carries none, whatever its lines look like
    for forged in ["✔ looks done", "══════════ TC-3: Completed", "next: `evil`"]
    {
        assert_eq!(row(forged), strip(&row(forged)), "{forged}");
    }
}

/// The plan an agent wrote and the review that blocks the task reach stderr, `status` and
/// `watch` as plain text: the words stay, the control sequences do not.
#[test]
fn m15_agent_text_reaches_the_console_without_control_sequences() {
    let env = setup("codex-openai");
    create(&env, "TC-1");
    let s = script(
        &env,
        "hostile.json",
        json!({
            "planner": [turn(vec![json!({"kind":"complete","structured":{
                "understanding":"u","files_likely_involved":[],"steps":["s"],"validation":[],
                "plan_markdown": format!("the plan {HOSTILE}")}})])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![json!({"kind":"complete","structured":{
                "decision":"blocked","summary": format!("blocked because {HOSTILE}"),
                "acceptance_criteria":[
                    {"id":"AC-1","met":false,"evidence":"e1"},
                    {"id":"AC-2","met":false,"evidence":"e2"},
                    {"id":"AC-3","met":false,"evidence":"e3"}],
                "issues":[{"id":"I-1","severity":"high","blocking":true,
                           "description": format!("issue {HOSTILE}")}]}})])],
        }),
    );
    let (code, err) = toryo(&env, &["run", "TC-1", "--yes", "--fake-agents", &s], &[]);
    assert_eq!(task(&env, "TC-1").state, St::Escalated, "{code} {err}");
    // the agent's words are there …
    assert!(err.contains("the plan") && err.contains("fake"), "{err}");
    // … and nothing that drives the terminal
    assert_inert("stderr of `toryo run`", &err);
    assert_inert("`toryo status`", &status_out(&env, "TC-1"));
    let watch = command(&env, &["watch", "TC-1", "--from-start", "--no-follow"], &[])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&watch.stdout).to_string();
    assert!(out.contains("TC-1"), "{out}");
    assert_inert("`toryo watch`", &out);
}
