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

/// What stands where a control character was.
pub const MARK: char = '\u{FFFD}';

/// May `c` not reach a terminal? Every control character but the tab and the line feed (C0,
/// DEL, C1 — the 8-bit CSI/OSC among them), and the invisible characters that change what is
/// read: zero-width and direction marks, bidi embeddings, overrides and isolates.
fn unsafe_char(c: char) -> bool {
    match c {
        '\n' | '\t' => false,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => true,
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

/// `eprintln!` through [`text`](crate::term::text): a human-readable line on stderr.
#[macro_export]
macro_rules! errln {
    () => {
        eprintln!()
    };
    ($($arg:tt)*) => {
        eprintln!("{}", $crate::term::text(&format!($($arg)*)))
    };
}

/// `println!` through [`text`](crate::term::text): a human-readable line on stdout.
#[macro_export]
macro_rules! outln {
    () => {
        println!()
    };
    ($($arg:tt)*) => {
        println!("{}", $crate::term::text(&format!($($arg)*)))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_macros_format_like_println() {
        // compiles with and without arguments, with named and positional ones
        let n = 2;
        errln!();
        errln!("m15 macro test {n} {}", "x");
        outln!("m15 macro test {a}", a = 1);
    }
}
