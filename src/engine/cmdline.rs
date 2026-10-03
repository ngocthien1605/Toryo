//! M11-R15 (X10): reading a **command string** — what a gate hands to `pwsh -Command`,
//! `cmd /c` or `sh -c` — far enough to tell its entry scripts from its arguments.
//!
//! - The string is tokenised with quotes and split into **simple commands** at `&&`, `||`, `;`,
//!   `|`, `&`, newlines and `(` `)` `{` `}`.
//! - The **head** of a simple command is its first word after an optional `call`, `.`, `exec`,
//!   `time` or leading `NAME=value` assignments (PowerShell's call operator `&` is a separator,
//!   so the word after it is a head too).
//! - A `cd` / `chdir` / `pushd` / `Set-Location` / `sl` with one literal relative folder moves
//!   where the following commands resolve (`popd` goes back; a shell's `( … )` scopes it). With
//!   any other argument — a variable, an absolute path, `-`, nothing — the string cannot be
//!   followed: nothing more is found in the rest of it.
//! - A head that is a relative path with a path separator is an **entry script** (`./x.sh`,
//!   `scripts\x.cmd`, `./gradlew` — no extension needed): protected whether or not it exists,
//!   refused at admission when the base commit lacks it and the repository does not ignore it.
//! - A bare head of a `cmd` string that is not a built-in is **protected, never refused**, as
//!   `<name>`, `<name>.com`, `<name>.exe`, `<name>.bat`, `<name>.cmd` in the current folder:
//!   `cmd.exe` looks in the working directory before PATH, and a check gate's working directory
//!   is the checkout root. PowerShell and the `sh` family never run a bare name from the working
//!   directory.
//! - Every other word is an **argument** ([`Parsed::args`]): never an entry, never refused.
//!
//! A word that holds a variable, a glob or an escape is not literal: it is never an entry and
//! never an argument. When in doubt the reader refuses nothing.

use super::checks::{arg_values, Entry};

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
    /// The entry scripts (heads with a path separator: refusable) and, for `cmd`, the names a
    /// bare head can resolve to in the working directory (protected, never refused). Paths are
    /// as written while no `cd` moved the folder, else repository-relative with `/`.
    pub entries: Vec<Entry>,
    /// The other words that are literal relative paths (also the value of `KEY=value`,
    /// `--flag=value`, …), resolved like the entries; the value of an output flag (`-o`,
    /// `--out…`, `-OutFile`) and a redirection target are left out.
    pub args: Vec<String>,
}

enum Tok {
    Word {
        text: String,
        literal: bool,
    },
    /// The end of a simple command.
    Sep,
    Open,
    Close,
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

/// The word in progress and the tokens so far.
#[derive(Default)]
struct Lexer {
    out: Vec<Tok>,
    text: String,
    /// Nothing in the word expands (a variable, a glob, an escape).
    literal: bool,
    /// A word is in progress (also an empty quoted one).
    started: bool,
    /// The next word is a redirection target: dropped.
    drop_next: bool,
}

impl Lexer {
    fn push(&mut self, c: char, literal: bool) {
        if !self.started {
            self.started = true;
            self.literal = true;
        }
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
            self.out.push(Tok::Word {
                text,
                literal: self.literal,
            });
        }
    }

    fn sep(&mut self, t: Tok) {
        self.flush();
        self.drop_next = false;
        self.out.push(t);
    }

    /// Start a word without a character (an opening quote).
    fn start(&mut self) {
        if !self.started {
            self.started = true;
            self.literal = true;
        }
    }
}

/// The tokens of `s` as `shell` reads it. Quotes are removed (single and double; `cmd` knows
/// only double quotes); a redirection (`> out.log`, `2>&1`) is dropped with its target.
fn tokens(s: &str, shell: Shell) -> Vec<Tok> {
    let cs: Vec<char> = s.chars().collect();
    let mut lx = Lexer::default();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        i += 1;
        match c {
            '\'' if shell != Shell::Cmd => {
                lx.start();
                while i < cs.len() && cs[i] != '\'' {
                    lx.push(cs[i], true);
                    i += 1;
                }
                i += 1; // the closing quote
            }
            '"' => {
                lx.start();
                while i < cs.len() && cs[i] != '"' {
                    let d = cs[i];
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
            '\n' => lx.sep(Tok::Sep),
            c if c.is_whitespace() => lx.flush(),
            '#' if !lx.started && shell != Shell::Cmd => {
                // a comment, to the end of the line
                while i < cs.len() && cs[i] != '\n' {
                    i += 1;
                }
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
                    lx.drop_next = true;
                }
            }
            ';' => lx.sep(Tok::Sep),
            '&' | '|' => {
                if i < cs.len() && cs[i] == c {
                    i += 1;
                }
                lx.sep(Tok::Sep);
            }
            '(' => lx.sep(Tok::Open),
            ')' => lx.sep(Tok::Close),
            '{' | '}' => match shell {
                Shell::Pwsh => lx.sep(Tok::Sep),
                // a reserved word only when it stands alone; else brace expansion
                Shell::Sh => {
                    let alone = !lx.started
                        && cs.get(i).is_none_or(|n| {
                            n.is_whitespace() || matches!(n, ';' | ')' | '&' | '|')
                        });
                    if alone {
                        lx.sep(Tok::Sep);
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

/// One simple command; `false`: the string cannot be followed any further.
fn simple(
    words: &[(String, bool)],
    shell: Shell,
    cur: &mut String,
    dirs: &mut Vec<String>,
    p: &mut Parsed,
) -> bool {
    let backslash = shell != Shell::Sh;
    // `@call x.cmd`, `@echo off`: `cmd` only hides the echo
    let bare = |w: &'_ str| -> String {
        if shell == Shell::Cmd {
            w.trim_start_matches('@').to_string()
        } else {
            w.to_string()
        }
    };
    let mut i = 0;
    while let Some((w, literal)) = words.get(i) {
        let w = &bare(w);
        let prefix = (*literal
            && (one_of(w, &["call", ".", "exec", "time"])
                || (shell == Shell::Cmd && one_of(w, &["do", "else"]))))
            || (shell == Shell::Sh && assignment(w));
        if !prefix {
            break;
        }
        i += 1;
    }
    let Some((head, literal)) = words.get(i) else {
        return true;
    };
    let rest = &words[i + 1..];
    let head = bare(head);
    let head = head.as_str();
    if *literal && one_of(head, &["cd", "chdir", "pushd", "set-location", "sl"]) {
        let flag = |w: &str| match shell {
            Shell::Cmd => w.eq_ignore_ascii_case("/d"),
            Shell::Pwsh => one_of(w, &["-path", "-literalpath"]),
            Shell::Sh => w == "--",
        };
        let dir: Vec<&(String, bool)> = rest.iter().filter(|w| !(w.1 && flag(&w.0))).collect();
        let [(dir, true)] = dir[..] else {
            return false;
        };
        if !relative(dir) || dir.starts_with('-') {
            return false;
        }
        let Some(next) = join(cur, dir, backslash) else {
            return false;
        };
        if head.eq_ignore_ascii_case("pushd") {
            dirs.push(cur.clone());
        }
        *cur = next;
        return true;
    }
    if *literal && head.eq_ignore_ascii_case("popd") {
        return match dirs.pop() {
            Some(d) => {
                *cur = d;
                true
            }
            None => false,
        };
    }
    let mut entry = |path: String, refusable: bool| {
        if !p.entries.iter().any(|e| e.path == path) {
            p.entries.push(Entry { path, refusable });
        }
    };
    if *literal && !head.is_empty() {
        if head.contains('/') || (backslash && head.contains('\\')) {
            if !relative(head) {
                // absolute: as written (one outside the repository is dropped by the caller)
                entry(head.to_string(), true);
            } else if let Some(path) = resolve(cur, head, backslash) {
                entry(path, true);
            }
        } else if shell == Shell::Cmd && !head.contains(':') && !one_of(head, CMD_BUILTINS) {
            for ext in ["", ".com", ".exe", ".bat", ".cmd"] {
                if let Some(path) = resolve(cur, &format!("{head}{ext}"), backslash) {
                    entry(path, false);
                }
            }
        }
    }
    let mut skip = false;
    for (w, literal) in rest {
        let skipped = std::mem::replace(&mut skip, *literal && output_flag(w));
        if skipped || !*literal {
            continue;
        }
        for v in arg_values(w) {
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
    true
}

/// Read the command string `s` (see the module text).
pub fn parse(s: &str, shell: Shell) -> Parsed {
    let mut p = Parsed::default();
    let mut cur = String::new();
    let mut scopes: Vec<String> = Vec::new();
    let mut dirs: Vec<String> = Vec::new();
    let mut words: Vec<(String, bool)> = Vec::new();
    for t in tokens(s, shell).into_iter().chain([Tok::Sep]) {
        if let Tok::Word { text, literal } = t {
            words.push((text, literal));
            continue;
        }
        let cmd = std::mem::take(&mut words);
        if !simple(&cmd, shell, &mut cur, &mut dirs, &mut p) {
            break;
        }
        // a shell's `( … )` is a subshell: a `cd` inside does not leave it
        match t {
            Tok::Open if shell == Shell::Sh => scopes.push(cur.clone()),
            Tok::Close if shell == Shell::Sh => {
                if let Some(c) = scopes.pop() {
                    cur = c;
                }
            }
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

    fn e(v: &[&str]) -> Vec<(String, bool)> {
        v.iter().map(|p| (p.to_string(), true)).collect()
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
            ("\"$PSScriptRoot/x.ps1\"", Pwsh),
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
            entries("cd /d sub && call .\\run.bat", Cmd),
            e(&["sub/run.bat"])
        );
        assert_eq!(
            entries("pushd sub && .\\a.cmd && popd && .\\b.cmd", Cmd),
            e(&["sub/a.cmd", ".\\b.cmd"])
        );
        // a subshell scopes the `cd` (sh); `cmd` parentheses only group
        assert_eq!(
            entries("(cd ci && ./x.sh); ./y.sh", Sh),
            e(&["ci/x.sh", "./y.sh"])
        );
        assert_eq!(
            entries("(cd ci && .\\x.cmd) & .\\y.cmd", Cmd),
            e(&["ci/x.cmd", "ci/y.cmd"])
        );
        // arguments resolve in the folder too
        assert_eq!(parse("cd ci && make -f x.mk", Sh).args, ["ci/x.mk"]);
        // not followed: nothing more is found — what came before stays
        for s in [
            "cd $DIR && ./x.sh",
            "cd && ./x.sh",
            "cd - && ./x.sh",
            "cd /opt/ci && ./x.sh",
            "cd .. && ./x.sh",
            "cd a b && ./x.sh",
            "popd && ./x.sh",
        ] {
            assert_eq!(parse(s, Sh), Parsed::default(), "{s}");
        }
        assert_eq!(entries("./a.sh; cd \"$D\"; ./b.sh", Sh), e(&["./a.sh"]));
        assert_eq!(entries("cd C:\\ci && x.cmd", Cmd), Vec::new());
        assert_eq!(entries("cd %CI% && x.cmd", Cmd), Vec::new());
    }

    /// A bare head of a `cmd` string: every name `cmd.exe` would try in the working directory,
    /// protected and never refused; built-ins are none; other shells run no bare name from it.
    #[test]
    fn a_bare_cmd_head_is_protected_never_refused() {
        use Shell::*;
        let bare = |name: &str| -> Vec<(String, bool)> {
            ["", ".com", ".exe", ".bat", ".cmd"]
                .iter()
                .map(|x| (format!("{name}{x}"), false))
                .collect()
        };
        assert_eq!(entries("build", Cmd), bare("build"));
        assert_eq!(entries("npm.cmd run build", Cmd), bare("npm.cmd"));
        assert_eq!(entries("cd sub && run.bat", Cmd), bare("sub/run.bat"));
        assert_eq!(entries("@echo off & call x.cmd arg", Cmd), bare("x.cmd"));
        let mut two = bare("setup.exe");
        two.extend(bare("run.cmd"));
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
}
