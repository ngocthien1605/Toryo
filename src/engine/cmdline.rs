//! M11-R15 (X10): reading a **command string** — what a gate hands to `pwsh -Command`,
//! `cmd /c` or `sh -c` — far enough to tell its entry scripts from its arguments.
//!
//! - The string is tokenised with quotes and split into **simple commands** at `&&`, `||`, `;`,
//!   `|`, `&`, newlines and `(` `)` `{` `}`.
//! - The **head** of a simple command is its first word after an optional `call`, `.`, `exec`,
//!   `time` or leading `NAME=value` assignments (PowerShell's call operator `&` is a separator,
//!   so the word after it is a head too).
//! - A `cd` / `chdir` / `pushd` / `Set-Location` / `sl` / `Push-Location` with one literal
//!   relative folder moves where the following commands resolve (`popd` / `Pop-Location` go
//!   back; a shell's `( … )` scopes it). With any other argument — a variable, an absolute path,
//!   `-`, nothing — or with a `cd` that runs only sometimes (after `&&` / `||`, in a pipeline, in a
//!   block) or is not the head of its command (`if … then cd x`), the
//!   string cannot be followed: no more entries are found in the rest of it (its words are kept
//!   as `maybe`: protected when the base commit has them, never refused). A here-document stops
//!   the reading too.
//! - A head that is a relative path with a path separator is an **entry script**: protected
//!   whether or not it exists. It is **refusable** (admission refuses it when the base commit
//!   lacks it and the repository does not ignore it) only when it clearly names a script: it
//!   starts with `./` or `../` (`.\`, `..\`) or has a script extension. `./gradlew` needs no
//!   extension in a `sh` string. In a `cmd` or PowerShell string a head without an extension
//!   (`scripts\build`, `.\gradlew`) runs `<head>.cmd`, `<head>.ps1`, …: every such name is
//!   protected and none is refused.
//! - A bare head of a `cmd` string that is not a built-in is **protected, never refused**, as
//!   `<name>` and `<name>` + each default `PATHEXT` extension in the current folder: `cmd.exe`
//!   looks in the working directory before PATH, and a check gate's working directory is the
//!   checkout root. PowerShell and the `sh` family never run a bare name from the working
//!   directory.
//! - In PowerShell a **quoted** string in head position is an expression, not a command
//!   (`'out/x.txt' | Should -Exist`, `@('a/b','c/d')`), unless it follows `&` or `.`.
//! - Every other word is an **argument** ([`Parsed::args`]): never an entry, never refused.
//!
//! A word that holds a variable, a glob or an escape is not literal: it is never an entry and
//! never an argument. When in doubt the reader refuses nothing.

use super::checks::{arg_values, has_script_ext, Entry};

/// Which shell reads the command string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// `sh`, `bash`, `zsh`, `dash`, `ksh` (`-c`).
    Sh,
    /// `pwsh`, `powershell` (`-Command`).
    Pwsh,
    /// `cmd` (`/c`, `/k`).
    Cmd,
}

/// What a command string holds.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Parsed {
    /// The entry scripts (heads with a path separator) and, for `cmd`, the names a bare head
    /// can resolve to in the working directory (protected, never refused). Paths are as written
    /// while no `cd` moved the folder, else repository-relative with `/`.
    pub entries: Vec<Entry>,
    /// The other words that are literal relative paths (also the value of `KEY=value`,
    /// `--flag=value`, …), resolved like the entries; the value of an output flag (`-o`,
    /// `--out…`, `-OutFile`) and a redirection target are left out.
    pub args: Vec<String>,
    /// (R16–R17 review) Where the reading stopped: the literal words of the rest, resolved in
    /// the folder reached so far (and in the folder of a `cd` that may or may not have run).
    /// They are only ever **protected when they name a file of the base commit** — never an
    /// entry, never refused, no note.
    pub maybe: Vec<String>,
}

/// One word: its text without quotes, whether nothing in it expands, and whether it began
/// with a quote.
#[derive(Debug, Clone)]
struct Word {
    text: String,
    literal: bool,
    quoted: bool,
}

enum Tok {
    Word(Word),
    /// `;` or a line break: the next command runs whatever the last one did.
    Seq,
    /// `&&`, `||`: the next command runs depending on the last one.
    Cond,
    /// `|`: both sides run in a pipeline.
    Pipe,
    /// A single `&`: a separator, and PowerShell's call operator for the next word.
    Amp,
    Open,
    Close,
    /// `{` / `}` of a block: PowerShell's script block, a shell's group or function body.
    BlockOpen,
    BlockClose,
    /// The rest cannot be read (a here-document).
    Stop,
}

/// `cmd` built-ins: a bare head that is one of them runs no file.
const CMD_BUILTINS: &[&str] = &[
    "assoc",
    "break",
    "call",
    "cd",
    "chdir",
    "cls",
    "color",
    "copy",
    "date",
    "defined",
    "del",
    "dir",
    "do",
    "echo",
    "else",
    "endlocal",
    "erase",
    "errorlevel",
    "exist",
    "exit",
    "for",
    "ftype",
    "goto",
    "if",
    "in",
    "md",
    "mkdir",
    "mklink",
    "move",
    "not",
    "path",
    "pause",
    "popd",
    "prompt",
    "pushd",
    "rd",
    "rem",
    "ren",
    "rename",
    "rmdir",
    "set",
    "setlocal",
    "shift",
    "start",
    "time",
    "title",
    "type",
    "ver",
    "verify",
    "vol",
];

/// The default `PATHEXT` of Windows: what `cmd.exe` appends to a name without an extension,
/// in the working directory first. The list is fixed (not this machine's `%PATHEXT%`), so a
/// task's protected set is the same on every machine and every start.
pub const PATHEXT: [&str; 11] = [
    ".com", ".exe", ".bat", ".cmd", ".vbs", ".vbe", ".js", ".jse", ".wsf", ".wsh", ".msc",
];

/// The words that change the folder (head position).
const CD: &[&str] = &["cd", "chdir", "set-location", "sl"];
const PUSHD: &[&str] = &["pushd", "push-location"];
const POPD: &[&str] = &["popd", "pop-location"];

/// The word in progress and the tokens so far.
#[derive(Default)]
struct Lexer {
    out: Vec<Tok>,
    text: String,
    /// Nothing in the word expands (a variable, a glob, an escape).
    literal: bool,
    /// The word began with a quote.
    quoted: bool,
    /// A word is in progress (also an empty quoted one).
    started: bool,
    /// The next word is a redirection target: dropped.
    drop_next: bool,
}

impl Lexer {
    fn begin(&mut self, quoted: bool) {
        if !self.started {
            self.started = true;
            self.literal = true;
            self.quoted = quoted;
        }
    }

    fn push(&mut self, c: char, literal: bool) {
        self.begin(false);
        self.literal &= literal;
        self.text.push(c);
    }

    /// End the word in progress, if any.
    fn flush(&mut self) {
        if !std::mem::take(&mut self.started) {
            return;
        }
        let text = std::mem::take(&mut self.text);
        if !std::mem::take(&mut self.drop_next) {
            self.out.push(Tok::Word(Word {
                text,
                literal: self.literal,
                quoted: self.quoted,
            }));
        }
    }

    fn sep(&mut self, t: Tok) {
        self.flush();
        self.drop_next = false;
        self.out.push(t);
    }
}

/// The index after the `)` that closes the `(` before `i` (nested parentheses counted), or the
/// end of the text.
fn skip_parens(cs: &[char], mut i: usize) -> usize {
    let mut depth = 1;
    while i < cs.len() {
        match cs[i] {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    i
}

/// The tokens of `s` as `shell` reads it. Quotes are removed (single and double; `cmd` knows
/// only double quotes); a redirection (`> out.log`, `2>&1`) is dropped with its target; a line
/// continuation joins its lines; a shell's arithmetic (`$(( … ))`, `(( … ))`) and array
/// (`NAME=( … )`) are skipped — nothing in them is a command.
fn tokens(s: &str, shell: Shell) -> Vec<Tok> {
    let cs: Vec<char> = s.chars().collect();
    let mut lx = Lexer::default();
    let mut i = 0;
    // a line break (`\n` or `\r\n`) at `i`: how many characters it takes
    let newline = |i: usize| match (cs.get(i), cs.get(i + 1)) {
        (Some('\n'), _) => 1,
        (Some('\r'), Some('\n')) => 2,
        _ => 0,
    };
    while i < cs.len() {
        let c = cs[i];
        i += 1;
        // a line continuation: `\` (sh), a backtick (PowerShell) or `^` (cmd) before a line break
        let continues = match shell {
            Shell::Sh => c == '\\',
            Shell::Pwsh => c == '`',
            Shell::Cmd => c == '^',
        };
        if continues && newline(i) > 0 {
            i += newline(i);
            continue;
        }
        if continues {
            // an escape: the next character is taken as it is, never as a separator or a
            // quote (`echo a \; ./x.sh`, `echo a ^& x.cmd`) — and the word is not literal
            lx.push(c, false);
            if let Some(n) = cs.get(i) {
                lx.push(*n, false);
                i += 1;
            }
            continue;
        }
        match c {
            '\'' if shell != Shell::Cmd => {
                lx.begin(true);
                loop {
                    while i < cs.len() && cs[i] != '\'' {
                        lx.push(cs[i], true);
                        i += 1;
                    }
                    i += 1; // the closing quote
                            // PowerShell: `''` inside single quotes is one quote
                    if shell == Shell::Pwsh && cs.get(i) == Some(&'\'') {
                        lx.push('\'', true);
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
            '"' => {
                lx.begin(true);
                while i < cs.len() && cs[i] != '"' {
                    let d = cs[i];
                    // an escaped character inside the quotes (`\"`, a backtick and `"`)
                    let escape = match shell {
                        Shell::Sh => d == '\\',
                        Shell::Pwsh => d == '`',
                        Shell::Cmd => false,
                    };
                    if escape && i + 1 < cs.len() {
                        lx.push(d, false);
                        lx.push(cs[i + 1], false);
                        i += 2;
                        continue;
                    }
                    let expands = match shell {
                        Shell::Sh => matches!(d, '$' | '`' | '\\'),
                        Shell::Pwsh => matches!(d, '$' | '`'),
                        Shell::Cmd => matches!(d, '%' | '!'),
                    };
                    lx.push(d, !expands);
                    i += 1;
                }
                i += 1;
            }
            '\n' => lx.sep(Tok::Seq),
            c if c.is_whitespace() => lx.flush(),
            '#' if !lx.started && shell != Shell::Cmd => {
                // a comment, to the end of the line
                while i < cs.len() && cs[i] != '\n' {
                    i += 1;
                }
            }
            '<' if shell == Shell::Sh && cs.get(i) == Some(&'<') && cs.get(i + 1) != Some(&'<') => {
                // a here-document: its body is data, and where it ends is not followed
                lx.sep(Tok::Stop);
                break;
            }
            '>' | '<' => {
                // an fd number in front (`2>`) belongs to the redirection
                if lx.started && !lx.text.is_empty() && lx.text.chars().all(|d| d.is_ascii_digit())
                {
                    lx.text.clear();
                    lx.started = false;
                } else {
                    lx.flush();
                }
                while i < cs.len() && matches!(cs[i], '>' | '<') {
                    i += 1;
                }
                if i < cs.len() && cs[i] == '&' {
                    // `>&1`, `<&-`: no target word
                    i += 1;
                    while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '-') {
                        i += 1;
                    }
                } else {
                    if i < cs.len() && cs[i] == '|' {
                        i += 1; // `>|`
                    }
                    lx.drop_next = true;
                }
            }
            // `cmd`: `;` separates arguments, not commands (`set CLASSPATH=a.jar;b.jar`)
            ';' if shell == Shell::Cmd => lx.push(c, true),
            ';' => lx.sep(Tok::Seq),
            '&' | '|' => {
                if i < cs.len() && cs[i] == c {
                    i += 1;
                    lx.sep(Tok::Cond);
                } else if c == '&' {
                    lx.sep(Tok::Amp);
                } else {
                    lx.sep(Tok::Pipe);
                }
            }
            '(' if shell == Shell::Sh
                && ((lx.started && lx.text.ends_with('$') && cs.get(i) == Some(&'('))
                    || (lx.started && lx.text.ends_with('='))
                    || (!lx.started && cs.get(i) == Some(&'('))) =>
            {
                // `$(( … ))`, `NAME=( … )`, `(( … ))`: no command inside
                i = skip_parens(&cs, i);
                lx.push('(', false);
            }
            '(' => lx.sep(Tok::Open),
            ')' => lx.sep(Tok::Close),
            '{' | '}' => match shell {
                Shell::Pwsh if c == '{' => lx.sep(Tok::BlockOpen),
                Shell::Pwsh => lx.sep(Tok::BlockClose),
                // a reserved word only when it stands alone; else brace expansion
                Shell::Sh => {
                    let alone = !lx.started
                        && cs.get(i).is_none_or(|n| {
                            n.is_whitespace() || matches!(n, ';' | ')' | '&' | '|')
                        });
                    if alone && c == '{' {
                        lx.sep(Tok::BlockOpen);
                    } else if alone {
                        lx.sep(Tok::BlockClose);
                    } else {
                        lx.push(c, false);
                    }
                }
                Shell::Cmd => lx.push(c, true),
            },
            c => {
                let special = match shell {
                    Shell::Sh => {
                        matches!(c, '$' | '`' | '\\' | '*' | '?' | '[' | ']')
                            || (c == '~' && !lx.started)
                    }
                    Shell::Pwsh => {
                        matches!(c, '$' | '`' | '*' | '?' | '[' | ']')
                            || (matches!(c, '~' | '@') && !lx.started)
                    }
                    Shell::Cmd => matches!(c, '%' | '^' | '!' | '*' | '?'),
                };
                lx.push(c, !special);
            }
        }
    }
    lx.flush();
    lx.out
}

/// A relative path as written: no root, no drive, no home.
fn relative(p: &str) -> bool {
    !p.is_empty() && !p.starts_with(['/', '\\', '~']) && p.as_bytes().get(1) != Some(&b':')
}

/// `p` under the folder `cur` (repository-relative, `/`, `""` = the root), normalised; `None`
/// when it leaves the repository.
fn join(cur: &str, p: &str, backslash: bool) -> Option<String> {
    let mut parts: Vec<&str> = cur.split('/').filter(|c| !c.is_empty()).collect();
    for c in p.split(|c| c == '/' || (backslash && c == '\\')) {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    Some(parts.join("/"))
}

/// `p` as it is listed: as written while no `cd` moved the folder, else under `cur`.
fn resolve(cur: &str, p: &str, backslash: bool) -> Option<String> {
    if cur.is_empty() {
        Some(p.to_string())
    } else {
        join(cur, p, backslash)
    }
}

/// `NAME=value` in front of a command (`sh`).
fn assignment(w: &str) -> bool {
    w.split_once('=').is_some_and(|(n, _)| {
        !n.is_empty()
            && !n.starts_with(|c: char| c.is_ascii_digit())
            && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn one_of(w: &str, names: &[&str]) -> bool {
    names.iter().any(|n| w.eq_ignore_ascii_case(n))
}

/// The value after it is an output file, not something the command reads.
pub(super) fn output_flag(a: &str) -> bool {
    let l = a.to_ascii_lowercase();
    l == "-o" || l == "-outfile" || l.starts_with("--out")
}

/// `cd..`, `cd\`, `chdir..` — `cmd` needs no space there: the folder glued to the word.
fn glued_cd(shell: Shell, w: &str) -> Option<&str> {
    if shell != Shell::Cmd {
        return None;
    }
    ["chdir", "cd"].iter().find_map(|p| {
        let (name, rest) = (w.get(..p.len())?, w.get(p.len()..)?);
        (name.eq_ignore_ascii_case(p) && rest.starts_with(['.', '\\'])).then_some(rest)
    })
}

/// A word that changes the folder, wherever it stands.
fn cd_word(shell: Shell, w: &Word) -> bool {
    w.literal
        && (one_of(&w.text, CD)
            || one_of(&w.text, PUSHD)
            || one_of(&w.text, POPD)
            || glued_cd(shell, &w.text).is_some())
}

/// The file name of `p` has no extension (`scripts\build`, `./gradlew`).
fn no_extension(p: &str) -> bool {
    !p.rsplit(['/', '\\'])
        .next()
        .unwrap_or(p)
        .trim_start_matches('.')
        .contains('.')
}

/// What one simple command means for the reading.
enum Step {
    Next,
    /// The string cannot be followed any further. With a folder: a `cd` that may or may not
    /// have run would have gone there.
    Stop(Option<String>),
}

/// One simple command. `called`: it follows PowerShell's call operator `&`. `sure`: it runs
/// whatever came before — at the top level, not after `&&` / `||`, not in a pipeline, not in a
/// block; only then is a change of folder followed.
fn simple(
    words: &[Word],
    shell: Shell,
    called: bool,
    sure: bool,
    cur: &mut String,
    dirs: &mut Vec<String>,
    p: &mut Parsed,
) -> Step {
    let backslash = shell != Shell::Sh;
    // `cmd`: `@call x.cmd` only hides the echo; an unquoted head ends at its first `/` (`rd/s/q`,
    // `cd/d`: the rest is a switch); `for /f` runs a command written in single quotes
    let mut words: Vec<Word> = words.to_vec();
    if shell == Shell::Cmd {
        for w in &mut words {
            if w.quoted || !w.literal {
                continue;
            }
            let t = w.text.trim_start_matches('@');
            let t = t
                .strip_prefix('\'')
                .and_then(|x| x.strip_suffix('\''))
                .or_else(|| t.strip_prefix('`').and_then(|x| x.strip_suffix('`')))
                .unwrap_or(t);
            w.text = t.to_string();
        }
    }
    let mut i = 0;
    let mut dotted = false;
    while let Some(w) = words.get(i) {
        let dot = w.literal && !w.quoted && w.text == ".";
        let prefix = (w.literal
            && !w.quoted
            && (one_of(&w.text, &["call", "exec", "time"])
                || dot
                || (shell == Shell::Cmd && one_of(&w.text, &["do", "else"]))))
            || (shell == Shell::Sh && assignment(&w.text));
        if !prefix {
            break;
        }
        dotted |= dot;
        i += 1;
    }
    let Some(head) = words.get(i).cloned() else {
        return Step::Next;
    };
    let mut rest: Vec<Word> = words[i + 1..].to_vec();
    let mut head_text = head.text.clone();
    if shell == Shell::Cmd && head.literal && !head.quoted {
        if let Some((name, switch)) = head_text.clone().split_once('/') {
            rest.insert(
                0,
                Word {
                    text: format!("/{switch}"),
                    literal: true,
                    quoted: false,
                },
            );
            head_text = name.to_string();
        }
    }
    let head_text = head_text.as_str();
    // a change of folder
    let glued = glued_cd(shell, head_text).filter(|_| head.literal && !head.quoted);
    let is_push = one_of(head_text, PUSHD);
    if head.literal && !head.quoted && (one_of(head_text, CD) || is_push || glued.is_some()) {
        let flag = |w: &str| match shell {
            Shell::Cmd => w.eq_ignore_ascii_case("/d"),
            Shell::Pwsh => one_of(w, &["-path", "-literalpath"]),
            Shell::Sh => w == "--",
        };
        let mut dir: Vec<(String, bool)> = rest
            .iter()
            .filter(|w| !(w.literal && flag(&w.text)))
            .map(|w| (w.text.clone(), w.literal))
            .collect();
        if let Some(g) = glued {
            dir.insert(0, (g.to_string(), true));
        }
        let [(dir, true)] = &dir[..] else {
            return Step::Stop(None);
        };
        if !relative(dir) || dir.starts_with('-') {
            return Step::Stop(None);
        }
        let Some(next) = join(cur, dir, backslash) else {
            return Step::Stop(None);
        };
        // (R16–R17 review) a `cd` that runs only sometimes — after `&&` / `||`, in a pipeline,
        // inside a block or a function body — is not followed: where the rest runs is not known
        if !sure {
            return Step::Stop(Some(next));
        }
        if is_push {
            dirs.push(cur.clone());
        }
        *cur = next;
        return Step::Next;
    }
    if head.literal && !head.quoted && one_of(head_text, POPD) {
        return match dirs.pop().filter(|_| sure) {
            Some(d) => {
                *cur = d;
                Step::Next
            }
            None => Step::Stop(None),
        };
    }
    // a change of folder that is not the head (`if … then cd x`, `command cd x`, `if exist x cd
    // x`): whether it happens is not known — the rest is not followed
    if rest.iter().any(|w| cd_word(shell, w)) {
        return Step::Stop(None);
    }
    // PowerShell: a quoted string in head position is an expression unless `&` or `.` runs it
    let expression = shell == Shell::Pwsh && head.quoted && !(called || dotted);
    let mut entry = |path: String, refusable: bool| {
        if !p.entries.iter().any(|e| e.path == path) {
            p.entries.push(Entry { path, refusable });
        }
    };
    if head.literal && !head_text.is_empty() && !expression {
        if head_text.contains('/') || (backslash && head_text.contains('\\')) {
            // refused when missing only when it clearly names a script
            let clear = has_script_ext(head_text)
                || ["./", "../", ".\\", "..\\"]
                    .iter()
                    .any(|d| head_text.starts_with(*d) && (backslash || d.ends_with('/')));
            if !relative(head_text) {
                // absolute: as written (one outside the repository is dropped by the caller)
                entry(head_text.to_string(), has_script_ext(head_text));
            } else if shell != Shell::Sh && no_extension(head_text) {
                // `scripts\build`, `.\gradlew`: `cmd` and PowerShell run `<head>.cmd`,
                // `<head>.ps1`, … — which one is not known: all protected, none refused
                let ps1 = (shell == Shell::Pwsh).then_some(".ps1");
                for ext in [""].into_iter().chain(ps1).chain(PATHEXT) {
                    if let Some(path) = resolve(cur, &format!("{head_text}{ext}"), backslash) {
                        entry(path, false);
                    }
                }
            } else if let Some(path) = resolve(cur, head_text, backslash) {
                entry(path, clear);
            }
        } else if shell == Shell::Cmd
            && !head_text.contains(':')
            && !one_of(head_text, CMD_BUILTINS)
        {
            for ext in [""].into_iter().chain(PATHEXT) {
                if let Some(path) = resolve(cur, &format!("{head_text}{ext}"), backslash) {
                    entry(path, false);
                }
            }
        }
    }
    let quoted_head = expression.then_some(&head);
    let mut skip = false;
    for w in quoted_head.into_iter().chain(rest.iter()) {
        let skipped = std::mem::replace(&mut skip, w.literal && output_flag(&w.text));
        if skipped || !w.literal {
            continue;
        }
        for v in arg_values(&w.text) {
            if v.starts_with('-') || !relative(&v) {
                continue;
            }
            if let Some(path) = resolve(cur, &v, backslash) {
                if !path.is_empty() && !p.args.contains(&path) {
                    p.args.push(path);
                }
            }
        }
    }
    Step::Next
}

/// Read the command string `s` (see the module text).
pub fn parse(s: &str, shell: Shell) -> Parsed {
    parse_in(s, shell, "")
}

/// [`parse`], starting in the folder `start` (repository-relative, `/`; `""` = the root).
pub fn parse_in(s: &str, shell: Shell, start: &str) -> Parsed {
    let backslash = shell != Shell::Sh;
    let mut toks = tokens(s, shell);
    toks.push(Tok::Seq);
    let mut p = Parsed::default();
    let mut cur = start.trim_matches('/').to_string();
    let mut scopes: Vec<String> = Vec::new();
    let mut dirs: Vec<String> = Vec::new();
    let mut words: Vec<Word> = Vec::new();
    // where the words of the simple command being read begin
    let mut begin = 0;
    let mut called = false;
    // the command being read runs whatever came before: the start, `;`, a line break, a
    // single `&`, the start of a group — not after `&&` / `||` / `|`
    let mut plain = true;
    // how deep it is in blocks (`{ … }`; for `cmd` and PowerShell also `( … )`)
    let mut depth = 0usize;
    // `cmd`: inside a `rem` line
    let mut rem = false;
    for (n, t) in toks.iter().enumerate() {
        if let Tok::Word(w) = t {
            if !rem {
                words.push(w.clone());
            }
            continue;
        }
        if rem {
            // the comment runs to the end of the line, separators included
            rem = !matches!(t, Tok::Seq);
            (begin, plain) = (n + 1, true);
            continue;
        }
        let cmd = std::mem::take(&mut words);
        if shell == Shell::Cmd
            && cmd.first().is_some_and(|w| {
                !w.quoted && w.text.trim_start_matches('@').eq_ignore_ascii_case("rem")
            })
        {
            rem = !matches!(t, Tok::Seq);
            (begin, plain) = (n + 1, true);
            continue;
        }
        let sure = plain && depth == 0 && !matches!(t, Tok::Pipe);
        if let Step::Stop(other) = simple(&cmd, shell, called, sure, &mut cur, &mut dirs, &mut p) {
            // what is left is only ever protected when it names a file of the base commit —
            // as written from the folder reached so far, and from the folder a `cd` that may
            // or may not have run would have gone to
            let folders: Vec<String> = std::iter::once(cur.clone()).chain(other).collect();
            for w in toks[begin..].iter().filter_map(|t| match t {
                Tok::Word(w) if w.literal => Some(w),
                _ => None,
            }) {
                for v in arg_values(w.text.trim_start_matches('@')) {
                    if v.starts_with('-') || !relative(&v) {
                        continue;
                    }
                    for f in &folders {
                        if let Some(path) = resolve(f, &v, backslash) {
                            if !path.is_empty() && !p.maybe.contains(&path) {
                                p.maybe.push(path);
                            }
                        }
                    }
                }
            }
            break;
        }
        called = matches!(t, Tok::Amp) && shell == Shell::Pwsh;
        plain = !matches!(t, Tok::Cond | Tok::Pipe);
        begin = n + 1;
        match t {
            Tok::Stop => break,
            // a shell's `( … )` is a subshell: a `cd` inside does not leave it
            Tok::Open if shell == Shell::Sh => scopes.push(cur.clone()),
            Tok::Close if shell == Shell::Sh => {
                if let Some(c) = scopes.pop() {
                    cur = c;
                }
            }
            Tok::Open | Tok::BlockOpen => depth += 1,
            Tok::Close | Tok::BlockClose => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(s: &str, shell: Shell) -> Vec<(String, bool)> {
        parse(s, shell)
            .entries
            .into_iter()
            .map(|e| (e.path, e.refusable))
            .collect()
    }

    /// Refusable entries.
    fn e(v: &[&str]) -> Vec<(String, bool)> {
        v.iter().map(|p| (p.to_string(), true)).collect()
    }

    /// `name` and `name` + each `PATHEXT` extension: protected, not refusable.
    fn names(name: &str) -> Vec<(String, bool)> {
        [""].into_iter()
            .chain(PATHEXT)
            .map(|x| (format!("{name}{x}"), false))
            .collect()
    }

    /// Quotes keep a path with a space in one word; variables, globs and escapes make a word
    /// non-literal (never an entry, never an argument); redirections and comments are dropped.
    #[test]
    fn words_quotes_and_what_is_not_literal() {
        use Shell::*;
        assert_eq!(
            entries("& './my scripts/x.ps1' -A 1", Pwsh),
            e(&["./my scripts/x.ps1"])
        );
        assert_eq!(entries("\"./a b/x.sh\" --fast", Sh), e(&["./a b/x.sh"]));
        assert_eq!(
            entries("./my' scripts'/x.sh", Sh),
            e(&["./my scripts/x.sh"])
        );
        // not literal
        for (s, shell) in [
            ("$HOME/bin/check.sh", Sh),
            ("& \"$PSScriptRoot/x.ps1\"", Pwsh),
            ("%~dp0x.cmd", Cmd),
            ("./ci/*.sh", Sh),
            ("ci\\x.sh", Sh),
            ("~/x.sh", Sh),
            ("./x.{sh,bash}", Sh),
            ("\"%TOOLS%\\x.cmd\"", Cmd),
        ] {
            assert_eq!(parse(s, shell), Parsed::default(), "{s}");
        }
        // single quotes keep `$` literal
        assert_eq!(entries("'./a$b/x.sh'", Sh), e(&["./a$b/x.sh"]));
        // redirections: no target, no stray `1` or `&` command
        let p = parse("./x.sh > build/out.log 2>&1", Sh);
        assert_eq!((p.entries.len(), p.args.len()), (1, 0), "{p:?}");
        let p = parse("echo x >| out/log.sh", Sh);
        assert_eq!(
            p,
            Parsed {
                args: vec!["x".into()],
                ..Default::default()
            }
        );
        let p = parse("scripts\\a.cmd 1>nul 2>&1 & scripts\\b.cmd", Cmd);
        assert_eq!(
            p.entries
                .iter()
                .map(|e| e.path.as_str())
                .collect::<Vec<_>>(),
            ["scripts\\a.cmd", "scripts\\b.cmd"]
        );
        assert!(p.args.is_empty(), "{p:?}");
        // a comment
        assert_eq!(
            entries("./a.sh # then ./b.sh\n./c.sh", Sh),
            e(&["./a.sh", "./c.sh"])
        );
        // every separator starts a new simple command
        assert_eq!(
            entries(
                "./a.sh && ./b.sh || ./c.sh; ./d.sh | ./e.sh & ./f.sh\n(./g.sh); { ./h.sh; }",
                Sh
            ),
            e(&["./a.sh", "./b.sh", "./c.sh", "./d.sh", "./e.sh", "./f.sh", "./g.sh", "./h.sh"])
        );
        assert_eq!(
            entries("if (Test-Path x) { ./a.ps1 } else { & ./b.ps1 }", Pwsh),
            e(&["./a.ps1", "./b.ps1"])
        );
    }

    /// The head follows `call`, `.`, `exec`, `time` and `NAME=value`; other words are arguments.
    #[test]
    fn heads_and_arguments() {
        use Shell::*;
        assert_eq!(entries("call scripts\\x.cmd", Cmd), e(&["scripts\\x.cmd"]));
        assert_eq!(entries(". .\\lib\\x.ps1; Go", Pwsh), e(&[".\\lib\\x.ps1"]));
        assert_eq!(entries("exec ./run.sh", Sh), e(&["./run.sh"]));
        assert_eq!(entries("time ./run.sh", Sh), e(&["./run.sh"]));
        assert_eq!(entries("CI=1 FOO=$BAR ./run.sh", Sh), e(&["./run.sh"]));
        assert_eq!(entries("@call scripts\\x.cmd", Cmd), e(&["scripts\\x.cmd"]));
        assert_eq!(entries("./gradlew test", Sh), e(&["./gradlew"]));
        // an argument is never an entry
        let p = parse("Invoke-Pester tests/New.Tests.ps1 -Output Detailed", Pwsh);
        assert!(p.entries.is_empty());
        assert_eq!(p.args, ["tests/New.Tests.ps1", "Detailed"]);
        let p = parse("tsc --outFile build/x.js src/a.ts", Sh);
        assert!(p.entries.is_empty());
        assert_eq!(p.args, ["src/a.ts"]);
        let p = parse("make SCRIPT=ci/x.sh --file=ci/b.mk /abs/x.sh -k", Sh);
        assert_eq!(p.args, ["ci/x.sh", "ci/b.mk"]);
        // an absolute head is kept as written (the caller knows the repository)
        assert_eq!(
            entries("\"C:\\Program Files\\x\\tool.exe\" a", Cmd),
            e(&["C:\\Program Files\\x\\tool.exe"])
        );
        // a line continuation joins its lines: the next line is no new command
        let p = parse("pytest \\\n  --junitxml \\\n  reports/junit.xml", Sh);
        assert_eq!(
            (p.entries.len(), p.args),
            (0, vec!["reports/junit.xml".to_string()])
        );
        let p = parse("dotnet test `\r\n  tests/My.Tests.csproj", Pwsh);
        assert_eq!(
            (p.entries.len(), p.args),
            (
                0,
                vec!["test".to_string(), "tests/My.Tests.csproj".to_string()]
            )
        );
        let p = parse("msbuild ^\r\n  src\\App.sln", Cmd);
        assert_eq!(p.args, ["src\\App.sln"]);
    }

    /// (R13–R15 review) refusable only when the head clearly names a script: `./`, `../` or a
    /// script extension. Another head with a separator is protected, not refused; in a `cmd` or
    /// PowerShell string a head without an extension stands for `<head>.cmd`, `<head>.ps1`, ….
    #[test]
    fn what_is_refused_when_missing() {
        use Shell::*;
        assert_eq!(entries("./gradlew", Sh), e(&["./gradlew"]));
        assert_eq!(entries("../tools/x", Sh), e(&["../tools/x"]));
        assert_eq!(entries("ci/run.sh", Sh), e(&["ci/run.sh"]));
        assert_eq!(entries("ci/run", Sh), [("ci/run".to_string(), false)]);
        assert_eq!(
            entries("node_modules/.bin/jest --ci", Sh),
            [("node_modules/.bin/jest".to_string(), false)]
        );
        assert_eq!(entries("scripts\\build", Cmd), names("scripts\\build"));
        assert_eq!(entries(".\\gradlew build", Cmd), names(".\\gradlew"));
        let mut ps: Vec<(String, bool)> =
            vec![(".\\build".into(), false), (".\\build.ps1".into(), false)];
        ps.extend(names(".\\build").into_iter().skip(1));
        assert_eq!(entries(".\\build -Configuration Release", Pwsh), ps);
        assert_eq!(entries("scripts\\x.cmd", Cmd), e(&["scripts\\x.cmd"]));
        assert_eq!(entries("tools\\gen.py", Pwsh), e(&["tools\\gen.py"]));
        // `sh`: a backslash is an escape, never a separator
        assert_eq!(entries(".\\x.sh", Sh), Vec::new());
    }

    /// (R13–R15 review) what is not a command: a quoted string in head position in PowerShell,
    /// a shell's arithmetic and arrays, a here-document, a `for /f` or attached-switch form in
    /// `cmd`.
    #[test]
    fn what_is_not_a_command() {
        use Shell::*;
        for s in [
            "Invoke-Pester -Path @('tests/unit','tests/int')",
            "if ([IO.File]::Exists('build/app.exe')) { exit 0 }",
            "'out/result.txt' | Should -Exist",
            "Copy-Item -Path ('build/out' + $x) -Destination dist",
            "switch ($env:OS) { 'a/b' { exit 0 } }",
        ] {
            assert_eq!(entries(s, Pwsh), Vec::new(), "{s}");
        }
        // … it is an argument (protected when the base commit has it)
        assert_eq!(
            parse("'out/result.txt' | Should -Exist", Pwsh).args,
            ["out/result.txt"]
        );
        // `&` and `.` run a quoted path
        assert_eq!(entries("& 'ci/x.ps1'", Pwsh), e(&["ci/x.ps1"]));
        assert_eq!(entries(". 'ci/x.ps1'", Pwsh), e(&["ci/x.ps1"]));
        assert_eq!(entries("&'ci/x.ps1'", Pwsh), e(&["ci/x.ps1"]));
        // in `sh` and `cmd` a quoted head is a command
        assert_eq!(entries("'ci/x.sh' --fast", Sh), e(&["ci/x.sh"]));
        assert_eq!(
            entries("\"scripts\\b.bat\" -q", Cmd),
            e(&["scripts\\b.bat"])
        );
        // arithmetic, arrays
        assert_eq!(entries("sleep $((TIMEOUT/2)); ./x.sh", Sh), e(&["./x.sh"]));
        assert_eq!(
            entries("OUT=(build/a.o build/b.o); ./x.sh", Sh),
            e(&["./x.sh"])
        );
        assert_eq!(entries("(( n = a/b )); ./x.sh", Sh), e(&["./x.sh"]));
        // command substitution still holds commands
        assert_eq!(entries("echo $(./version.sh)", Sh), e(&["./version.sh"]));
        // a here-document: what came before stays, the body and the rest are not read
        assert_eq!(
            entries(
                "./a.sh; cat > .env <<EOF\n./not/a/command.sh\nEOF\n./b.sh",
                Sh
            ),
            e(&["./a.sh"])
        );
        assert_eq!(
            entries("python3 - <<'PY'\nopen('build/out.json')\nPY", Sh),
            Vec::new()
        );
        // `cmd`: switches attached to a built-in, `for /f` with a quoted command
        assert_eq!(
            entries("rd/s/q build & scripts\\build.cmd", Cmd),
            e(&["scripts\\build.cmd"])
        );
        assert_eq!(entries("dir/b src", Cmd), Vec::new());
        assert_eq!(
            entries("for /f %v in ('scripts\\version.cmd') do echo %v", Cmd),
            e(&["scripts\\version.cmd"])
        );
    }

    /// `cd` moves where the rest resolves; one that cannot be followed stops the reading.
    #[test]
    fn cd_is_followed_or_stops_the_reading() {
        use Shell::*;
        assert_eq!(entries("cd ci && ./x.sh", Sh), e(&["ci/x.sh"]));
        assert_eq!(entries("cd ./ci/ ; ../tools/y.sh", Sh), e(&["tools/y.sh"]));
        assert_eq!(
            entries("Set-Location -Path ci; ./x.ps1", Pwsh),
            e(&["ci/x.ps1"])
        );
        assert_eq!(
            entries("sl ci; cd sub; ./x.ps1", Pwsh),
            e(&["ci/sub/x.ps1"])
        );
        assert_eq!(
            entries("Push-Location ci; ./x.ps1; Pop-Location; ./y.ps1", Pwsh),
            e(&["ci/x.ps1", "./y.ps1"])
        );
        assert_eq!(
            entries("cd /d sub && call .\\run.bat", Cmd),
            e(&["sub/run.bat"])
        );
        assert_eq!(entries("cd/d sub && .\\run.bat", Cmd), e(&["sub/run.bat"]));
        assert_eq!(
            entries("pushd sub & .\\a.cmd & popd & .\\b.cmd", Cmd),
            e(&["sub/a.cmd", ".\\b.cmd"])
        );
        assert_eq!(
            entries("cd tools & .\\build.cmd & cd.. & .\\test.cmd", Cmd),
            e(&["tools/build.cmd", ".\\test.cmd"])
        );
        // a subshell scopes the `cd` (sh)
        assert_eq!(
            entries("(cd ci && ./x.sh); ./y.sh", Sh),
            e(&["ci/x.sh", "./y.sh"])
        );
        // arguments resolve in the folder too
        assert_eq!(parse("cd ci && make -f x.mk", Sh).args, ["ci/x.mk"]);
        assert_eq!(parse_in("./x.ps1 a.txt", Pwsh, "ci").args, ["ci/a.txt"]);
        assert_eq!(
            parse_in("./x.ps1", Pwsh, "ci").entries,
            [Entry {
                path: "ci/x.ps1".into(),
                refusable: true
            }]
        );
        // not followed: no entry, no argument — what came before stays
        let nothing = |s: &str, shell: Shell| {
            let p = parse(s, shell);
            assert!(p.entries.is_empty() && p.args.is_empty(), "{s}: {p:?}");
            p.maybe
        };
        for s in [
            "cd $DIR && ./x.sh",
            "cd && ./x.sh",
            "cd - && ./x.sh",
            "cd /opt/ci && ./x.sh",
            "cd .. && ./x.sh",
            "cd a b && ./x.sh",
            "popd && ./x.sh",
            "command cd ci && ./x.sh",
        ] {
            // … and the rest is kept only as "protect it if the base commit has it"
            assert!(nothing(s, Sh).contains(&"./x.sh".to_string()), "{s}");
        }
        assert_eq!(entries("./a.sh; cd \"$D\"; ./b.sh", Sh), e(&["./a.sh"]));
        assert_eq!(
            parse("./a.sh; cd \"$D\"; ./b.sh", Sh).maybe,
            ["cd", "./b.sh"]
        );
        for (s, shell) in [
            ("cd C:\\ci && x.cmd", Cmd),
            ("cd %CI% && x.cmd", Cmd),
            ("cd\\ && x.cmd", Cmd),
            ("cd.. && .\\run.cmd", Cmd),
            // a `cd` that is not the head of its command: whether it runs is not known
            ("if exist ci cd ci & .\\run.cmd", Cmd),
            (
                "if [ -d frontend ]; then cd frontend; fi; ./run-tests.sh",
                Sh,
            ),
        ] {
            let p = parse(s, shell);
            assert!(p.entries.is_empty(), "{s}: {p:?}");
        }
        assert_eq!(
            entries("test -d ci && builtin cd ci; ./x.sh", Sh),
            Vec::new()
        );
    }

    /// (R16–R17 review) a `cd` that runs only sometimes is not followed — after `&&` / `||`, in
    /// a pipeline, inside a block or a function body: where the rest runs is not known, so
    /// nothing after it is an entry. The words of the rest are kept as `maybe`, as written
    /// from the folder reached so far and from the folder the `cd` names.
    #[test]
    fn a_conditional_cd_is_not_followed() {
        use Shell::*;
        for (s, shell) in [
            ("[ -d ci ] && cd ci; ./x.sh", Sh),
            ("cd a || cd b; ./x.sh", Sh),
            ("cd a | ./x.sh", Sh),
            ("f() { cd sub; }; ./x.sh", Sh),
            ("{ cd sub; ./x.sh; }", Sh),
            ("npm ci && cd web && ./x.sh", Sh),
            ("if (Test-Path build) { cd build }; ./x.ps1", Pwsh),
            ("if exist sub (cd sub) & scripts\\x.cmd", Cmd),
            ("(cd ci && .\\x.cmd) & .\\y.cmd", Cmd),
            (
                "pushd sub && .\\a.cmd && popd && .\\b.cmd && cd x && .\\c.cmd",
                Cmd,
            ),
        ] {
            let p = parse(s, shell);
            assert!(
                !p.entries.iter().any(|e| e.refusable
                    && (e.path.ends_with("x.sh")
                        || e.path.ends_with("x.ps1")
                        || e.path.ends_with("x.cmd")
                        || e.path.ends_with("y.cmd")
                        || e.path.ends_with("b.cmd")
                        || e.path.ends_with("c.cmd"))),
                "{s}: {p:?}"
            );
        }
        // what came before a conditional `cd` is read as usual
        assert_eq!(
            entries("pushd sub && .\\a.cmd && popd && .\\b.cmd", Cmd),
            e(&["sub/a.cmd"])
        );
        // both folders are candidates for the rest
        let p = parse("[ -d ci ] && cd ci; ./x.sh arg.txt", Sh);
        for want in ["./x.sh", "ci/x.sh", "arg.txt", "ci/arg.txt"] {
            assert!(p.maybe.contains(&want.to_string()), "{want}: {p:?}");
        }
        // the first `cd` of `cd a || cd b` is followed, the second is not
        let p = parse("cd a || cd b; ./x.sh", Sh);
        assert!(
            p.maybe.contains(&"a/x.sh".to_string()) && p.maybe.contains(&"a/b/x.sh".to_string()),
            "{p:?}"
        );
    }

    /// (R16–R17 review) an escaped separator or quote is not a separator; `cmd`: `;` separates
    /// arguments, and `rem` comments out the rest of its line.
    #[test]
    fn escapes_and_cmd_comments() {
        use Shell::*;
        for (s, shell) in [
            ("echo a \\; ./nope.sh", Sh),
            ("echo a ^& tools\\x.cmd", Cmd),
            ("echo \"a \\\" ; ./nope.sh ; \\\" b\"", Sh),
            ("Write-Host \"a `\" ; ./nope.ps1 ; `\" b\"", Pwsh),
            ("Write-Host 'it''s ; ./nope.ps1'", Pwsh),
            ("rem see a.cmd & tools\\old2.cmd", Cmd),
            ("@rem tools\\old.cmd", Cmd),
        ] {
            assert_eq!(entries(s, shell), Vec::new(), "{s}");
        }
        assert_eq!(
            entries("set CLASSPATH=lib\\a.jar;lib\\b.cmd & java Main", Cmd),
            names("java")
        );
        assert_eq!(
            entries("rem first line & skip.cmd\nscripts\\real.cmd", Cmd),
            e(&["scripts\\real.cmd"])
        );
    }

    /// A bare head of a `cmd` string: every name `cmd.exe` would try in the working directory
    /// (the default `PATHEXT`), protected and never refused; built-ins are none; other shells
    /// run no bare name from it.
    #[test]
    fn a_bare_cmd_head_is_protected_never_refused() {
        use Shell::*;
        assert_eq!(names("x").len(), 12);
        assert_eq!(entries("build", Cmd), names("build"));
        assert_eq!(entries("npm.cmd run build", Cmd), names("npm.cmd"));
        assert!(names("npm").contains(&("npm.js".to_string(), false)));
        assert_eq!(entries("cd sub && run.bat", Cmd), names("sub/run.bat"));
        assert_eq!(entries("@echo off & call x.cmd arg", Cmd), names("x.cmd"));
        let mut two = names("setup.exe");
        two.extend(names("run.cmd"));
        assert_eq!(entries("setup.exe && run.cmd", Cmd), two);
        for s in [
            "echo hi",
            "set X=1",
            "if exist x.txt exit 1",
            "rem build",
            "type a.txt",
            "COPY a b",
            "start /wait app.exe",
            "setlocal",
        ] {
            assert_eq!(entries(s, Cmd), Vec::new(), "{s}");
        }
        assert_eq!(entries("build", Sh), Vec::new());
        assert_eq!(entries("npm.cmd run build", Pwsh), Vec::new());
        assert_eq!(entries("msbuild.exe App.sln", Pwsh), Vec::new());
        assert_eq!(parse("msbuild.exe App.sln", Pwsh).args, ["App.sln"]);
    }

    /// No input makes the reader panic.
    #[test]
    fn odd_input_does_not_panic() {
        for s in [
            "",
            "'",
            "\"",
            "&",
            "&&",
            "(",
            ")",
            "((",
            "$((",
            "=(",
            "<<",
            "<",
            ">",
            ">|",
            "2>&",
            "cd",
            "cdé",
            "chdiré..",
            "é/ü.sh",
            "🙂/x.sh",
            "@",
            "`",
            "^",
            "\\",
            "a\\\n",
            "x=(",
            "' ' ' '",
            "\"\"",
            "''",
            "{",
            "}",
            "{}",
            "a=b=c",
            "-o",
            "/",
            "\\\\",
            "C:",
            "é:",
        ] {
            for shell in [Shell::Sh, Shell::Pwsh, Shell::Cmd] {
                let _ = parse(s, shell);
                let _ = parse_in(s, shell, "a/b");
            }
        }
    }
}
