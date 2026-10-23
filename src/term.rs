//! What Toryo prints on a console (M15, review M9).
//!
//! A plan, a review, an escalation reason, a gate's output or a vendor's error message is text
//! somebody else wrote. Printed unchanged, an escape sequence in it drives the owner's terminal:
//! OSC 52 writes the clipboard, `ESC[1A ESC[2K` erases the line above and a forged
//! `══ T-1: Completed ══` takes its place, a carriage return overwrites the start of the line, a
//! bidi override reverses what is read. [`text`] turns such text into inert text, and the
//! macros [`errln!`](crate::errln) / [`outln!`](crate::outln) — `eprintln!` / `println!` through
//! [`text`] — are what the engine and the CLI print human-readable lines with.
//!
//! Not for data: `--json` output is printed as it is (JSON escapes control characters).
//! `toryo context show` prints the agent's context through [`data`]: byte for byte into a pipe
//! or a file, as inert text when stdout is a terminal.
//!
//! Colour (M15-02) is a property of the stream, never of the text: the macros paint a line
//! ([`paint`]) only when the stream takes colour ([`painted`]). Into a pipe or a file the
//! output is the plain text. Nothing outside this module writes an escape sequence.

use std::sync::OnceLock;

pub mod paint;

/// What stands where a control character was.
pub const MARK: char = '\u{FFFD}';

/// May `c` not reach a terminal? Every control character but the tab and the line feed (C0,
/// DEL, C1 — the 8-bit CSI/OSC among them), and the invisible characters that change what is
/// read: zero-width and direction marks, bidi embeddings, overrides and isolates.
fn unsafe_char(c: char) -> bool {
    match c {
        '\n' | '\t' => false,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => true,
        // R23-20: the other invisible ones of the same kind — the Arabic letter mark (a bidi
        // control), the word joiner and the zero-width no-break space (a BOM inside a text)
        '\u{061C}' | '\u{2060}' | '\u{FEFF}' => true,
        c => c.is_control(),
    }
}

/// `s` as inert console text: ANSI escape sequences (CSI / OSC) are removed whole, a carriage
/// return is dropped (a line can no longer be overwritten; `\r\n` reads as `\n`), and every
/// other character [`unsafe_char`] names becomes [`MARK`]. Text without any is returned as it is.
pub fn text(s: &str) -> String {
    if !s.chars().any(unsafe_char) {
        return s.to_string();
    }
    crate::process::strip_ansi(s)
        .chars()
        .filter(|c| *c != '\r')
        .map(|c| if unsafe_char(c) { MARK } else { c })
        .collect()
}

/// Data for stdout (`toryo context show`): unchanged when stdout is a pipe or a file — what is
/// stored and hashed is what is written —, [`text`] when a person reads it on a terminal.
pub fn data(s: &str) -> String {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        text(s)
    } else {
        s.to_string()
    }
}

/// `--color`: when Toryo's own lines are painted (M15-02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Color {
    /// On a terminal, unless `NO_COLOR` is set (`CLICOLOR_FORCE` forces it into a pipe).
    #[default]
    Auto,
    /// Also into a pipe or a file.
    Always,
    /// Never: the plain text.
    Never,
}

static COLOR: OnceLock<Color> = OnceLock::new();

/// Set once by `main`, before the first line is printed (`--color`); later calls change nothing.
pub fn set_color(c: Color) {
    let _ = COLOR.set(c);
}

/// The stream a line goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Out,
    Err,
}

/// Are lines for `s` painted? Decided once per stream: `--color always` / `never` as said;
/// `auto` by what the stream is — a terminal that takes colour, with the `NO_COLOR`,
/// `CLICOLOR_FORCE` and `TERM=dumb` conventions (`anstream`). A pipe or a file gets the plain
/// text, byte for byte what Toryo printed before it had colours.
pub fn painted(s: Stream) -> bool {
    static OUT: OnceLock<bool> = OnceLock::new();
    static ERR: OnceLock<bool> = OnceLock::new();
    let cell = match s {
        Stream::Out => &OUT,
        Stream::Err => &ERR,
    };
    *cell.get_or_init(|| match COLOR.get().copied().unwrap_or_default() {
        Color::Always => true,
        Color::Never => false,
        Color::Auto => {
            let choice = match s {
                Stream::Out => anstream::AutoStream::choice(&std::io::stdout()),
                Stream::Err => anstream::AutoStream::choice(&std::io::stderr()),
            };
            choice != anstream::ColorChoice::Never
        }
    })
}

/// Write `t` and a line feed to `s`. Painted text goes through `anstream`, which turns on the
/// console's escape-sequence mode on Windows (or falls back to the console API).
fn emit(s: Stream, t: &str) -> std::io::Result<()> {
    use std::io::Write;
    match (s, painted(s)) {
        (Stream::Err, false) => writeln!(std::io::stderr(), "{t}"),
        (Stream::Out, false) => writeln!(std::io::stdout(), "{t}"),
        (Stream::Err, true) => writeln!(anstream::AutoStream::always(std::io::stderr()), "{t}"),
        (Stream::Out, true) => writeln!(anstream::AutoStream::always(std::io::stdout()), "{t}"),
    }
}

/// Print one block of Toryo's own text: inert ([`text`]), then painted when the stream is.
pub fn try_line(s: Stream, block: &str) -> std::io::Result<()> {
    let t = text(block);
    if painted(s) {
        emit(s, &paint::block(&t))
    } else {
        emit(s, &t)
    }
}

/// [`try_line`] for the macros: a stream that cannot be written is a panic, as with `println!`.
pub fn line(s: Stream, block: &str) {
    if let Err(e) = try_line(s, block) {
        let name = match s {
            Stream::Out => "stdout",
            Stream::Err => "stderr",
        };
        panic!("failed printing to {name}: {e}");
    }
}

/// Print text somebody else wrote as a whole — a plan, a review: inert, and **never painted**.
/// On a terminal colour marks the lines that are Toryo's own.
pub fn quoted(s: Stream, block: &str) {
    if let Err(e) = emit(s, &text(block)) {
        panic!("failed printing: {e}");
    }
}

/// The end of a run on stderr: the state and what comes next. Plain: an empty line, the banner
/// `══════════ <title> ══════════`, then each of `lines` — the text every version printed.
/// Painted: an open frame in the state's colour ([`paint::frame`]).
pub fn end_block(title: &str, state: &str, lines: &[String]) {
    if painted(Stream::Err) {
        let lines: Vec<String> = lines.iter().map(|l| text(l)).collect();
        let framed = paint::frame(&text(title), state, &lines);
        if let Err(e) = emit(Stream::Err, &format!("\n{framed}")) {
            panic!("failed printing to stderr: {e}");
        }
    } else {
        line(Stream::Err, &format!("\n══════════ {title} ══════════"));
        for l in lines {
            line(Stream::Err, l);
        }
    }
}

/// Is stdout a terminal — where a live view ([`screen`]) can be drawn in place?
pub fn stdout_is_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

/// The text of one [`screen`] frame: the cursor home, then each block — inert ([`text`]),
/// painted like any other block when `paint` — with every line erasing the rest of its row,
/// then the rest of the screen erased. The cursor moves and erasures are the only escape
/// sequences besides the colours, and they are Toryo's, never the text's.
pub fn frame_text(blocks: &[String], paint: bool) -> String {
    let mut s = String::from("\u{1b}[H");
    for b in blocks {
        let t = text(b);
        let t = if paint { paint::block(&t) } else { t };
        for l in t.split('\n') {
            s.push_str(l);
            s.push_str("\u{1b}[K\n");
        }
    }
    s.push_str("\u{1b}[J");
    s
}

/// Draw `blocks` as the whole screen of a live view (`toryo dashboard`, M16-01) over the
/// previous frame, without the flicker of a full clear. Only for a terminal
/// ([`stdout_is_terminal`]): into a pipe or a file a view prints its blocks once, as text.
pub fn screen(blocks: &[String]) -> std::io::Result<()> {
    use std::io::Write;
    let s = frame_text(blocks, painted(Stream::Out));
    // `always`: on Windows this turns on the console's escape-sequence mode
    let mut out = anstream::AutoStream::always(std::io::stdout().lock());
    out.write_all(s.as_bytes())?;
    out.flush()
}

/// `eprintln!` for a human-readable line: inert ([`text`](crate::term::text)), and painted when
/// stderr is ([`painted`](crate::term::painted)).
#[macro_export]
macro_rules! errln {
    () => {
        $crate::term::line($crate::term::Stream::Err, "")
    };
    ($($arg:tt)*) => {
        $crate::term::line($crate::term::Stream::Err, &format!($($arg)*))
    };
}

/// `println!` for a human-readable line: inert ([`text`](crate::term::text)), and painted when
/// stdout is ([`painted`](crate::term::painted)).
#[macro_export]
macro_rules! outln {
    () => {
        $crate::term::line($crate::term::Stream::Out, "")
    };
    ($($arg:tt)*) => {
        $crate::term::line($crate::term::Stream::Out, &format!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R23-20 (review 2026-10-23): X33 says "every zero-width or direction-changing
    /// character"; U+061C, U+2060 and U+FEFF passed. Vietnamese (NFC and NFD) and CJK text is
    /// untouched.
    #[test]
    fn the_other_invisible_characters_are_marked_too() {
        for c in ['\u{061C}', '\u{2060}', '\u{FEFF}'] {
            assert_eq!(
                text(&format!("a{c}b")),
                format!("a{MARK}b"),
                "{:X}",
                c as u32
            );
        }
        for s in [
            "Cảm ơn, tiếng Việt",
            "Vie\u{323}\u{302}t Nam",
            "日本語のテキスト",
        ] {
            assert_eq!(text(s), s);
        }
    }

    #[test]
    fn plain_text_is_unchanged() {
        for s in [
            "",
            "next: `toryo approve BR-1 --gate plan`",
            "── PLAN (attempt 1, claude-max) ──\n\tindented ✔ ✘ ══ │ … —",
            "tiếng Việt · 日本語 · C:\\Users\\x\\50%.txt",
        ] {
            assert_eq!(text(s), s);
        }
    }

    #[test]
    fn escape_sequences_are_removed_whole() {
        // a colour, an OSC title (BEL and ST ended), an OSC 52 clipboard write, cursor moves
        assert_eq!(text("\u{1b}[31merror\u{1b}[0m: bad"), "error: bad");
        assert_eq!(
            text("a\u{1b}]0;title\u{7}b\u{1b}]8;;http://x\u{1b}\\c"),
            "abc"
        );
        assert_eq!(text("\u{1b}]52;c;QUJD\u{7}x"), "x");
        assert_eq!(
            text("real\n\u{1b}[1A\u{1b}[2K══ T-1: Completed ══"),
            "real\n══ T-1: Completed ══"
        );
    }

    #[test]
    fn what_is_left_of_a_control_character_is_a_mark() {
        // a lone ESC, BEL, backspace, NUL, DEL, the 8-bit CSI and OSC
        assert_eq!(text("a\u{1b}b"), "a\u{FFFD}b");
        assert_eq!(
            text("\u{7}\u{8}\u{0}\u{7f}\u{9b}31m\u{9d}"),
            "\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}31m\u{FFFD}"
        );
        // an escape sequence cut in two by a chunk boundary: the ESC is a mark, the rest text
        assert_eq!(text("\u{1b}") + &text("[31mred"), "\u{FFFD}[31mred");
    }

    #[test]
    fn a_carriage_return_cannot_overwrite_the_line() {
        assert_eq!(text("line one\r\nline two\r\n"), "line one\nline two\n");
        assert_eq!(text("!! escalated\rok        "), "!! escalatedok        ");
    }

    #[test]
    fn invisible_and_direction_changing_characters_are_marked() {
        assert_eq!(text("ab\u{202e}dc"), "ab\u{FFFD}dc");
        assert_eq!(
            text("\u{200b}\u{200f}\u{202a}\u{2066}\u{2069}"),
            "\u{FFFD}".repeat(5)
        );
        // the tab and the line feed stay
        assert_eq!(text("a\tb\nc"), "a\tb\nc");
    }

    #[test]
    fn a_frame_carries_no_escape_sequence_of_the_text() {
        let blocks = vec![
            "T-1            Escalated              fix 0/3 · tokens 5".to_string(),
            "evil\u{1b}]52;c;QUJD\u{7} \u{1b}[2J\rgone".to_string(),
        ];
        let f = frame_text(&blocks, false);
        assert!(
            f.starts_with("\u{1b}[H") && f.ends_with("\u{1b}[J"),
            "{f:?}"
        );
        // the text's own sequences are gone: what is left of the frame without Toryo's cursor
        // and erase sequences is the inert text
        let body = f
            .trim_start_matches("\u{1b}[H")
            .trim_end_matches("\u{1b}[J")
            .replace("\u{1b}[K", "");
        assert_eq!(
            body,
            "T-1            Escalated              fix 0/3 · tokens 5\nevil gone\n"
        );
        // painted: only colours are added — with every sequence removed it is the same text
        let p = frame_text(&blocks, true);
        assert_eq!(crate::process::strip_ansi(&p), body);
        assert!(p.contains("\u{1b}["), "the state is painted");
    }

    #[test]
    fn the_macros_format_like_println() {
        // compiles with and without arguments, with named and positional ones
        let n = 2;
        errln!();
        errln!("m15 macro test {n} {}", "x");
        outln!("m15 macro test {a}", a = 1);
    }
}
