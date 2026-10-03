//! Checks done in code, never trusted to a model (plan §8, §9; R28, R29): the task's
//! acceptance ids, review validity, protected paths, verify commands.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{bail, Result};
use regex::Regex;
use serde_json::Value;

use crate::config::{expand_vars, CommandKind, CommandSpec, Effective};

/// An acceptance-criteria item: an optional bullet (`-`, `*`, `+`), number (`1.`, `1)`) or
/// task-list box (`[ ]`), then the id (optionally bold) and `:`, `.` or `)` (or a full-width
/// `：`). The id group is deliberately loose so a malformed id is reported, not skipped.
static AC_ITEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\s*(?:[-*+]\s*|[0-9]{1,9}[.)]\s*)?(?:\[[ xX]\]\s*)?\**(?P<id>(?i:ac)-[0-9A-Za-z_]*)\**\s*[:.)：]",
    )
    .expect("regex")
});
static AC_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^AC-[1-9][0-9]*$").expect("regex"));

/// A Markdown heading as the AC parser sees it: `(level, text)`.
fn heading(line: &str) -> Option<(usize, &str)> {
    let t = line.trim();
    let level = t.bytes().take_while(|b| *b == b'#').count();
    (level > 0).then(|| (level, t[level..].trim()))
}

/// Tracks fenced code blocks (``` or ~~~, CommonMark rules): `true` while `line` is part of
/// a fence (its opening and closing lines included).
#[derive(Default)]
struct Fences(Option<(u8, usize)>);

impl Fences {
    fn in_fence(&mut self, line: &str) -> bool {
        let t = line.trim_start().as_bytes();
        let run = |c: u8| t.iter().take_while(|b| **b == c).count();
        match self.0 {
            Some((c, n)) => {
                let k = run(c);
                if k >= n && t[k..].iter().all(|b| b.is_ascii_whitespace()) {
                    self.0 = None;
                }
                true
            }
            None => {
                for c in *b"`~" {
                    let k = run(c);
                    // an info string after backticks may not contain a backtick (inline code)
                    if k >= 3 && !(c == b'`' && t[k..].contains(&b'`')) {
                        self.0 = Some((c, k));
                        return true;
                    }
                }
                false
            }
        }
    }
}

/// `AC-<n>` ids under `## Acceptance criteria` (R28): at least one, unique, and exactly the
/// items a reader of TASK.md sees, because the reviewer must return exactly this set (EC-22):
/// - the section ends at the next heading of the **same or a higher** level: sub-headings
///   (`### Functional`) stay inside it;
/// - items may be bulleted, numbered (`1. AC-2: …`) or task-list boxes, the id may be bold;
/// - fenced code blocks are examples: ids (and `#` lines) inside them do not count;
/// - an item with a malformed id (`AC-01`, `AC-0`, `ac-1`, `AC-1a`) is an error, never
///   silently dropped.
pub fn parse_acceptance_ids(task: &str) -> Result<Vec<String>> {
    let mut section: Option<usize> = None;
    let mut fences = Fences::default();
    let mut ids = Vec::new();
    for (n, line) in task.lines().enumerate() {
        if fences.in_fence(line) {
            continue;
        }
        if let Some((level, text)) = heading(line) {
            if section.is_none_or(|l| level <= l) {
                section = text
                    .to_lowercase()
                    .starts_with("acceptance criteria")
                    .then_some(level);
            }
            continue;
        }
        if section.is_none() {
            continue;
        }
        if let Some(c) = AC_ITEM.captures(line) {
            let id = &c["id"];
            if !AC_ID.is_match(id) {
                bail!(
                    "TASK.md line {}: malformed acceptance-criteria id `{id}` in `{}` — ids are `AC-<n>` with n = 1, 2, … (no leading zero), e.g. `- AC-1: …` (R28)",
                    n + 1,
                    line.trim()
                );
            }
            ids.push(id.to_string());
        }
    }
    if ids.is_empty() {
        bail!("TASK.md has no `AC-<n>:` items under `## Acceptance criteria` (R28)");
    }
    let uniq: BTreeSet<&String> = ids.iter().collect();
    if uniq.len() != ids.len() {
        bail!("TASK.md has duplicate acceptance-criteria ids: {ids:?}");
    }
    Ok(ids)
}

/// Task id: `--id`, else the `# <ID> —` heading, else the file stem.
pub fn task_id(task_text: &str, task_path: &Path, explicit: Option<&str>) -> String {
    if let Some(i) = explicit {
        return i.to_string();
    }
    let re = regex::Regex::new(r"(?m)^#\s+([A-Za-z][A-Za-z0-9_-]*-[0-9]+)\b").expect("regex");
    if let Some(c) = re.captures(task_text) {
        return c[1].to_string();
    }
    task_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "TASK".into())
}

/// Review checks in code (plan §9 R28). Returns violations (empty = valid).
pub fn review_violations(v: &Value, ac_ids: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let decision = v["decision"].as_str().unwrap_or("");
    if !matches!(decision, "approved" | "changes_requested" | "blocked") {
        out.push(format!(
            "decision must be approved|changes_requested|blocked, got `{decision}`"
        ));
    }
    let acs = v["acceptance_criteria"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let got: Vec<String> = acs
        .iter()
        .filter_map(|a| a["id"].as_str().map(str::to_string))
        .collect();
    let want: BTreeSet<&String> = ac_ids.iter().collect();
    let have: BTreeSet<&String> = got.iter().collect();
    let missing: Vec<&&String> = want.difference(&have).collect();
    let extra: Vec<&&String> = have.difference(&want).collect();
    if !missing.is_empty() {
        out.push(format!("missing acceptance criteria: {missing:?}"));
    }
    if !extra.is_empty() {
        out.push(format!("unknown acceptance criteria: {extra:?}"));
    }
    if have.len() != got.len() {
        out.push("duplicate acceptance-criteria ids".into());
    }
    for a in &acs {
        let ev = a["evidence"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if ev.is_empty()
            || (a["met"].as_bool() == Some(true)
                && matches!(ev.as_str(), "n/a" | "-" | "ok" | "yes"))
        {
            out.push(format!(
                "{}: evidence is empty or a placeholder",
                a["id"].as_str().unwrap_or("?")
            ));
        }
    }
    let issues = v["issues"].as_array().cloned().unwrap_or_default();
    if decision == "changes_requested"
        && issues.is_empty()
        && acs.iter().all(|a| a["met"].as_bool() == Some(true))
    {
        out.push("changes_requested needs at least one unmet criterion or one issue".into());
    }
    out
}

/// Protected paths (plan §8/§14, simplified): verify scripts, the task file, instruction /
/// config files, and `policy.protected_paths` globs from toryo.yaml.
///
/// Every comparison is **case-insensitive** (EC-09): NTFS is, so on Windows `agents.md` *is*
/// `AGENTS.md` (Codex loads it as project instructions) and `.Claude\` is `.claude\`. The
/// paths are repo-relative with `/` or `\`.
pub struct Protected {
    /// Normalised (`/`, no `./`), lower-case.
    exact: Vec<String>,
    globs: Vec<regex::Regex>,
    /// `## Allow protected` in TASK.md (R28): tracked protected paths this task may change.
    allowed: Vec<regex::Regex>,
    /// `policy.deny_paths` (§8): never changed, never planned (e.g. `**/ZZZ/**`).
    deny: Vec<regex::Regex>,
    /// M11-C2: the implementer's write fence (org chart mode only).
    fence: Option<Fence>,
    /// M11-R14 (X9): entry scripts admitted as build output — `(path, gate)`, the path
    /// normalised and lower-case. Not in `exact`: see [`Protected::with_build_entries`].
    build: Vec<(String, String)>,
}

/// The implementer's write fence (M11-C2): `writes` / `never` of its role file, as written
/// (for the messages) and as case-insensitive matchers.
struct Fence {
    writes: Vec<regex::Regex>,
    never: Vec<regex::Regex>,
    writes_src: Vec<String>,
    never_src: Vec<String>,
}

/// Which fence rule a path breaks (M11-C2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceRule {
    /// Outside every `writes` glob.
    Outside(String),
    /// Inside a `never` glob (`never` wins over `writes`).
    Never(String),
}

impl FenceRule {
    /// For a plan that lists the path.
    pub fn plan_reason(&self) -> String {
        match self {
            Self::Outside(w) => format!("outside the implementer's write fence (`writes: [{w}]`)"),
            Self::Never(n) => format!("a `never` path (`never: [{n}]`)"),
        }
    }

    /// For a change the implementer made.
    pub fn change_reason(&self) -> String {
        match self {
            Self::Outside(w) => format!("outside your write fence (`writes: [{w}]`)"),
            Self::Never(n) => format!("touched a never path (`never: [{n}]`)"),
        }
    }
}

/// `policy.protected_paths` entry for "every repo file referenced by `commands.verify[]`"
/// (plan §14, R29). Not a glob: its expansion is [`verify_files`], which every caller passes
/// to [`Protected::new`] as exact paths (verify files are protected whether or not the list
/// names the macro).
pub const VERIFY_SCRIPTS: &str = "@verify-scripts";

/// File names protected at any depth (R35: tools discover them on their own).
const PROTECTED_NAMES: [&str; 9] = [
    "agents.md",
    "agents.override.md",
    "claude.md",
    "claude.local.md",
    ".mcp.json",
    ".gitattributes",
    "toryo.yaml",
    "toryo.local.yaml",
    ".gitmodules",
];

/// Directories protected at any depth — the entry itself (a link or junction put in its
/// place, EC-08) as well as everything under it.
const PROTECTED_DIRS: [&str; 5] = [".claude", ".codex", ".toryo", ".githooks", ".git"];

/// A glob as a case-insensitive regex (see [`glob_to_regex`]). `dir/**` also matches `dir`
/// itself: a link or junction put in place of the directory is a change to it (EC-08).
fn glob_ci(g: &str) -> regex::Regex {
    let g = g.replace('\\', "/");
    let re = match g.strip_suffix("/**").filter(|d| !d.is_empty()) {
        Some(dir) => {
            let base = glob_to_regex(dir);
            let base = base.as_str();
            format!("(?i){}(?:/.*)?$", base.strip_suffix('$').unwrap_or(base))
        }
        None => format!("(?i){}", glob_to_regex(&g).as_str()),
    };
    regex::Regex::new(&re).unwrap_or_else(|_| glob_to_regex(&g))
}

/// M11-R4: a `writes` / `never` pattern in the form the matcher compares paths in, or why it
/// is refused. `\` is `/`; a leading `./` and empty or `.` segments go; a trailing `/` — or a
/// last segment without `*` and `?` — matches that path and everything under it (`x/**`); a
/// wildcard pattern without a `/` matches at any depth (`*.pfx` is `**/*.pfx`). Refused: empty,
/// absolute, `..`, and syntax the matcher does not have.
pub fn fence_pattern(p: &str) -> std::result::Result<String, String> {
    let t = p.trim().replace('\\', "/");
    if t.is_empty() {
        return Err("is empty".into());
    }
    if t.starts_with('/') || t.chars().nth(1) == Some(':') {
        return Err("is absolute — a pattern is relative to the repository root".into());
    }
    if let Some(c) = t.chars().find(|c| "{}[]!".contains(*c)) {
        return Err(format!(
            "uses `{c}`, which the matcher does not support — only `*` (within one folder), `**` (any depth) and `?` (one character)"
        ));
    }
    // M11-R12: a variable or home shortcut is not expanded for a fence, so it would match
    // nothing — refuse it rather than accept a pattern that silently protects no file
    if let Some(c) = t.chars().find(|c| "%~$".contains(*c)) {
        return Err(format!(
            "uses `{c}` — a fence pattern is not variable-expanded; write the repository-relative path"
        ));
    }
    let parts: Vec<&str> = t
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    if parts.contains(&"..") {
        return Err("may not contain `..`".into());
    }
    if parts.is_empty() {
        return Err("names no path".into());
    }
    let mut out = parts.join("/");
    // (R4–R8 review) gitignore-like: a wildcard pattern without a `/` matches at any depth
    // (`*.pfx` is `**/*.pfx`), and a last segment without a wildcard names a folder as well
    // as a file (`**/secrets` is `**/secrets/**`)
    if parts.len() == 1 && out.contains(['*', '?']) && out != "**" {
        out = format!("**/{out}");
    }
    let last = out.rsplit('/').next().unwrap_or_default();
    if (t.ends_with('/') || !last.contains(['*', '?'])) && last != "**" {
        out.push_str("/**");
    }
    Ok(out)
}

/// M11-R4: why a `policy.deny_paths` / `protected_paths` glob (compared as written) can never
/// match a repository path, or None. Those keep their behaviour; `doctor` warns.
pub fn dead_pattern(p: &str) -> Option<String> {
    let t = p.trim().replace('\\', "/");
    if t == VERIFY_SCRIPTS {
        return None;
    }
    // M11-R12: the legacy matcher (`glob_to_regex`) escapes `{ } [ ] ! % ~ $`, so a pattern
    // with them matches those characters literally — do not call it dead. Only a pattern that
    // can never match a repository-relative path is dead: empty, absolute, or with a `..`
    // segment (a normalised repo path has none).
    if t.is_empty() {
        return Some("is empty, so it never matches".into());
    }
    if t.starts_with('/') || t.chars().nth(1) == Some(':') {
        return Some("is absolute — a repository path is relative, so it never matches".into());
    }
    if t.split('/').any(|c| c == "..") {
        return Some(
            "has a `..` segment — a normalised repository path has none, so it never matches"
                .into(),
        );
    }
    let tidy = t.split('/').all(|c| !c.is_empty() && c != ".");
    (!tidy).then(|| {
        let fix = t
            .split('/')
            .filter(|c| !c.is_empty() && *c != ".")
            .collect::<Vec<_>>()
            .join("/");
        let fix = if t.ends_with('/') && !fix.ends_with("**") {
            format!("{fix}/**")
        } else {
            fix
        };
        format!("never matches — paths are compared without `./`, `//` or a trailing `/`; write `{fix}`")
    })
}

/// The form `git status` prints a path in: `/` separators, `.` and `..` segments resolved
/// (`./gradlew` is `gradlew`). A leading `/` (an absolute path, never a repo path) and a
/// trailing `/` (a directory, as `ls-files` prints a nested repository) are kept.
fn norm_path(p: &str) -> String {
    let s = p.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for c in s.split('/') {
        match c {
            "" | "." => {}
            ".." if parts.last().is_some_and(|l| *l != "..") => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    let mut out = parts.join("/");
    if s.starts_with('/') {
        out.insert(0, '/');
    }
    if s.ends_with('/') && !parts.is_empty() {
        out.push('/');
    }
    out
}

/// Paths/globs listed under `## Allow protected` in TASK.md (R28).
pub fn parse_allow_protected(task: &str) -> Vec<String> {
    let mut in_section = false;
    let mut out = Vec::new();
    for line in task.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            in_section = t
                .trim_start_matches('#')
                .trim()
                .eq_ignore_ascii_case("allow protected");
            continue;
        }
        if in_section {
            if let Some(item) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
                let p = item.trim().trim_matches('`').trim().replace('\\', "/");
                // (R4–R8 review) `./CULTURE.md` names CULTURE.md
                let p = p.trim_start_matches("./");
                if !p.is_empty() {
                    out.push(p.to_string());
                }
            }
        }
    }
    out
}

pub fn glob_to_regex(g: &str) -> regex::Regex {
    let g = g.replace('\\', "/");
    let mut re = String::from("^");
    let mut chars = g.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    chars.next();
                    re.push_str("(?:.*/)?");
                } else {
                    re.push_str(".*");
                }
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    regex::Regex::new(&re).unwrap_or_else(|_| regex::Regex::new("^$").expect("regex"))
}

impl Protected {
    /// `exact`: repo-relative files (the verify files and the task file); `globs`:
    /// `policy.protected_paths`, where [`VERIFY_SCRIPTS`] stands for the verify files in
    /// `exact` rather than a path of its own.
    pub fn new(exact: Vec<String>, globs: &[String]) -> Self {
        Self {
            exact: exact
                .iter()
                .map(|e| norm_path(e).trim_end_matches('/').to_lowercase())
                .collect(),
            globs: globs
                .iter()
                .filter(|g| g.trim() != VERIFY_SCRIPTS)
                .map(|g| glob_ci(g))
                .collect(),
            allowed: Vec::new(),
            deny: Vec::new(),
            fence: None,
            build: Vec::new(),
        }
    }

    /// M11-R14 (X9): the entry scripts admitted as build output (missing at the base commit,
    /// ignored by the repository). They leave the R35 filesystem snapshot — an ignored file the
    /// implementer builds in its worktree is no change to protected state — and the path policy
    /// takes over: such a path in a change list (someone un-ignored it, or force-added it) is a
    /// violation, so a checkpoint never contains it. `## Allow protected` opens it like any
    /// protected path.
    pub fn with_build_entries(mut self, entries: &[crate::config::BuildEntry]) -> Self {
        for e in entries {
            let p = norm_path(&e.path).trim_end_matches('/').to_lowercase();
            self.exact.retain(|x| *x != p);
            if !self.build.iter().any(|(b, _)| *b == p) {
                self.build.push((p, e.gate.clone()));
            }
        }
        self
    }

    /// The gate whose build-output entry script `path` is (M11-R14), if any.
    pub fn build_entry(&self, path: &str) -> Option<&str> {
        let p = norm_path(path).trim_end_matches('/').to_lowercase();
        self.build
            .iter()
            .find(|(b, _)| *b == p)
            .map(|(_, g)| g.as_str())
    }

    /// The exact protected paths (verify files, the task file, the culture), normalised and
    /// lower-case, sorted — what [`Protected::matches`] compares besides the globs and names.
    pub fn exact_paths(&self) -> Vec<String> {
        let mut v = self.exact.clone();
        v.sort();
        v.dedup();
        v
    }

    /// M11-C2: the implementer's write fence — `writes` (an allowlist, when it lists anything)
    /// and `never` (always wins over `writes`), case-insensitive, `/` and `\` alike. It only
    /// adds violations: `policy.deny_paths` and the protected paths work exactly as before, and
    /// the task's `## Allow protected` list opens fence entries like protected paths (R61: it is
    /// human-written). Planner and reviewer get no fence (they are read-only).
    pub fn with_fence(mut self, writes: &[String], never: &[String]) -> Self {
        if writes.is_empty() && never.is_empty() {
            return self;
        }
        // (R4–R8 review) normalised here too: a task frozen before M11-R4 holds the patterns
        // as written; for the others this changes nothing
        let norm = |v: &[String]| -> Vec<String> {
            v.iter()
                .map(|g| fence_pattern(g).unwrap_or_else(|_| g.clone()))
                .collect()
        };
        let (writes, never) = (norm(writes), norm(never));
        self.fence = Some(Fence {
            writes: writes.iter().map(|g| glob_ci(g)).collect(),
            never: never.iter().map(|g| glob_ci(g)).collect(),
            writes_src: writes,
            never_src: never,
        });
        self
    }

    /// The fence rule a change to `path` breaks, or None (no fence, inside it, or allowed by
    /// the task).
    pub fn fence_rule(&self, path: &str) -> Option<FenceRule> {
        let f = self.fence.as_ref()?;
        if self.is_allowed(path) {
            return None;
        }
        let p = norm_path(path);
        if f.never.iter().any(|g| g.is_match(&p)) {
            return Some(FenceRule::Never(f.never_src.join(", ")));
        }
        if !f.writes.is_empty() && !f.writes.iter().any(|g| g.is_match(&p)) {
            return Some(FenceRule::Outside(f.writes_src.join(", ")));
        }
        None
    }

    /// M11-R8: paths that break the policy, in words — each path outside the write fence or on a
    /// `never` path with its fence rule, the others as `protected paths [...]`.
    pub fn violation_words(&self, paths: &[String]) -> String {
        let (fenced, plain): (Vec<&String>, Vec<&String>) =
            paths.iter().partition(|p| self.fence_rule(p).is_some());
        let fence: Vec<String> = fenced
            .iter()
            .filter_map(|p| {
                self.fence_rule(p)
                    .map(|r| format!("`{p}` ({})", r.plan_reason()))
            })
            .collect();
        let fence = fence.join(", ");
        match (plain.is_empty(), fence.is_empty()) {
            (_, true) => format!("protected paths {plain:?}"),
            (true, false) => format!("paths the implementer may not change: {fence}"),
            (false, false) => format!(
                "protected paths {plain:?} and paths the implementer may not change: {fence}"
            ),
        }
    }

    /// Add the task's `## Allow protected` list and `policy.deny_paths`.
    pub fn with_policy(mut self, allowed: &[String], deny: &[String]) -> Self {
        self.allowed = allowed.iter().map(|g| glob_ci(g)).collect();
        self.deny = deny.iter().map(|g| glob_ci(g)).collect();
        self
    }

    /// Protected and not allowed for this task: changing it is a path-policy violation. A
    /// build-output entry script (M11-R14) counts: it may exist, ignored, but never be changed
    /// in a way Git sees.
    pub fn is_protected(&self, path: &str) -> bool {
        (self.matches(path) || self.build_entry(path).is_some()) && !self.is_allowed(path)
    }

    /// A change here breaks the path policy: protected (and not allowed), denied, or outside
    /// the implementer's write fence (M11-C2).
    pub fn violates(&self, path: &str) -> bool {
        self.is_protected(path) || self.is_denied(path) || self.fence_rule(path).is_some()
    }

    /// Listed under `## Allow protected` in TASK.md (case-insensitive, like the protection
    /// it lifts).
    pub fn is_allowed(&self, path: &str) -> bool {
        let p = norm_path(path);
        self.allowed.iter().any(|g| g.is_match(&p))
    }

    /// Matches `policy.deny_paths` (case-insensitive).
    pub fn is_denied(&self, path: &str) -> bool {
        let p = norm_path(path);
        self.deny.iter().any(|g| g.is_match(&p))
    }

    /// Protected by the policy, whether or not this task may change it (the R35 snapshot
    /// covers every match). A protected directory name matches the entry itself at any
    /// depth — a symlink or junction `.claude -> docs/agent` included — as well as every
    /// path under it (EC-08). A build-output entry script (M11-R14) is not a match: it is
    /// left out of the snapshot (see [`Protected::with_build_entries`]).
    pub fn matches(&self, path: &str) -> bool {
        let p = norm_path(path);
        let lower = p.to_lowercase();
        let parts: Vec<&str> = lower.split('/').filter(|c| !c.is_empty()).collect();
        let name = parts.last().copied().unwrap_or_default();
        self.exact.iter().any(|e| e == lower.trim_end_matches('/'))
            || self.globs.iter().any(|g| g.is_match(&p))
            || PROTECTED_NAMES.contains(&name)
            || parts.iter().any(|c| PROTECTED_DIRS.contains(c))
    }
}

pub fn resolve_program(p: &str) -> PathBuf {
    let pb = PathBuf::from(p);
    if pb.is_absolute() || p.contains('/') || p.contains('\\') {
        return pb;
    }
    which(p).unwrap_or(pb)
}

/// First match on PATH (with `.exe` / `.cmd` on Windows).
pub fn which(name: &str) -> Option<PathBuf> {
    which_in(name, &std::env::var_os("PATH")?)
}

/// [`which`] on the given `PATH` value.
pub fn which_in(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    for dir in std::env::split_paths(path) {
        for ext in ["", ".exe", ".cmd"] {
            let p = dir.join(format!("{name}{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// `commands.verify` from the config, else `scripts/build.ps1` + `scripts/test.ps1` if they exist.
pub fn verify_commands(cfg: &Effective, repo: &Path) -> Result<Vec<CommandSpec>> {
    // M11: in org chart mode the check gates are the verify commands — never the scripts
    // fallback below
    if cfg.team.is_some() || !cfg.repo.commands.verify.is_empty() {
        return Ok(cfg.repo.commands.verify.clone());
    }
    let mut v = Vec::new();
    for (name, rel) in [("build", "scripts/build.ps1"), ("test", "scripts/test.ps1")] {
        if repo.join(rel).exists() {
            v.push(CommandSpec::program(
                name,
                "pwsh",
                &["-NoProfile", "-NonInteractive", "-File", rel],
                Some(30),
            ));
        }
    }
    if v.is_empty() {
        bail!("no verify commands: add `commands.verify` to toryo.yaml (or scripts/build.ps1 / scripts/test.ps1 to the repo)");
    }
    Ok(v)
}

/// `p` as a path inside `repo`, in the form `git status` prints (`./gradlew` → `gradlew`,
/// `scripts\build.cmd` → `scripts/build.cmd`); `None` when it points outside.
fn repo_relative(repo: &Path, p: &str) -> Option<String> {
    let path = Path::new(p);
    let rel = if path.is_absolute() || path.has_root() {
        path.strip_prefix(repo).ok()?.to_string_lossy().to_string()
    } else {
        p.to_string()
    };
    let n = norm_path(&rel);
    let n = n.trim_end_matches('/');
    (!n.is_empty() && !n.starts_with('/') && n.split('/').all(|c| c != "..")).then(|| n.to_string())
}

/// Every repo file a verify command runs or reads — the expansion of
/// [`VERIFY_SCRIPTS`] (R29). Protected, and hashed into `VERIFY_RESULT.json`'s `scripts`:
/// - the **program** when it is a path inside the repo (`scripts/gate.sh`, `./gradlew`,
///   `scripts\build.cmd`): an implementer who rewrites the gate to `exit 0` breaks the path
///   policy instead of passing verify (EC-07). A bare name (`pwsh`, `git`) is a PATH lookup;
/// - every argument naming a file in the repo;
/// - (R59) every project of a `tool: xpp` command, whether or not it exists yet: the
///   implementer may not change what is built. [`verify_globs`] adds `Directory.Build.*`.
///
/// Repo files are listed in `git status` form, once each; an argument naming a file outside
/// the repo (an absolute path) is kept as written.
///
/// "Naming a file" is asked of the folder `repo` as it is now — right for the verify checkout,
/// whose scripts are hashed into the result. A task's **protected set** uses
/// [`verify_files_at_base`] instead (M11-R14).
pub fn verify_files(cmds: &[CommandSpec], repo: &Path) -> Vec<String> {
    verify_files_with(cmds, repo, &|v| {
        repo_relative(repo, v)
            .filter(|r| repo.join(r).is_file())
            .or_else(|| repo.join(v).is_file().then(|| v.replace('\\', "/")))
    })
}

/// M11-R14 (X9): [`verify_files`] for a task's protected set — "an argument naming a repository
/// file" is decided against the **base commit** (`base`: its files, lower-case, `/`), never
/// against what happens to exist in the task worktree. So the answer is the same on every
/// engine start: build output that appears in the worktree later (`build/app.exe`,
/// `out/test/index.js`) does not become protected. Entry scripts and repository programs are
/// in the set whether or not they exist, as before.
pub fn verify_files_at_base(
    cmds: &[CommandSpec],
    repo: &Path,
    base: &std::collections::HashSet<String>,
) -> Vec<String> {
    verify_files_with(cmds, repo, &|v| {
        repo_relative(repo, v).filter(|r| base.contains(&r.to_lowercase()))
    })
}

/// [`verify_files`]; `names_file`: the file an argument (as written) names, in the form it is
/// listed, or `None`.
fn verify_files_with(
    cmds: &[CommandSpec],
    repo: &Path,
    names_file: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |f: String| {
        if !out.contains(&f) {
            out.push(f);
        }
    };
    for c in cmds {
        let (program, args) = match c.kind() {
            CommandKind::Program { program, args } => (program, args),
            CommandKind::Xpp { projects, .. } => {
                // the project and the `.user` file MSBuild imports next to it
                for p in projects {
                    for f in [p.clone(), format!("{p}.user")] {
                        if let Some(r) = repo_relative(repo, &f) {
                            add(r);
                        }
                    }
                }
                continue;
            }
        };
        let program = expand_vars(program).unwrap_or_else(|_| program.to_string());
        if program.contains('/') || program.contains('\\') || Path::new(&program).is_absolute() {
            // M18: a repository program is protected whether or not it exists yet
            if let Some(r) = repo_relative(repo, &program) {
                add(r);
            }
        }
        // M18 (M11-R10): every entry script the program runs, whether or not it exists yet —
        // also one without a separator or an extension (`sh check`)
        for e in entry_scripts(&program, args) {
            if let Some(r) = repo_relative(repo, &e.path) {
                add(r);
            }
        }
        // every other argument naming a file of the repository (also the value of
        // `KEY=value`, `--flag=value`, `-Flag:value`, `/p:Name=value`, M11-R10)
        for a in args {
            for v in arg_values(a) {
                if let Some(r) = names_file(&v) {
                    add(r);
                }
            }
        }
        // what the net finds (inside a command line, too) — protected when it exists (X7)
        for (_, r) in script_like_args(std::slice::from_ref(c), repo) {
            if let Some(r) = names_file(&r) {
                add(r);
            }
        }
    }
    out
}

/// PowerShell's `-File` parameter (and its abbreviations), case-insensitive.
fn is_file_flag(a: &str) -> bool {
    ["-file", "-fil", "-fi", "-f", "/file"]
        .iter()
        .any(|f| a.eq_ignore_ascii_case(f))
}

/// A program's base name in lower case, without a directory or `.exe` / `.cmd` / `.bat`.
fn program_base(program: &str) -> String {
    let name = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat"] {
        if let Some(n) = name.strip_suffix(ext) {
            return n.to_string();
        }
    }
    name
}

/// Script interpreters (M18, group E review): the first argument that is not a flag is the
/// script they run — unless one of the flags runs inline code or a module instead (second
/// list). M11-R6: the third list are the flags that take a value (`bash -o pipefail x.sh`).
const INTERPRETERS: &[(&str, &[&str], &[&str])] = &[
    ("sh", &["-c"], &["-o", "-O"]),
    ("bash", &["-c"], &["-o", "-O", "--rcfile", "--init-file"]),
    ("zsh", &["-c"], &["-o", "-O"]),
    ("dash", &["-c"], &["-o", "-O"]),
    ("ksh", &["-c"], &["-o", "-O"]),
    ("python", &["-c", "-m"], &["-W", "-X"]),
    ("python3", &["-c", "-m"], &["-W", "-X"]),
    ("py", &["-c", "-m"], &["-W", "-X"]),
    (
        "node",
        &["-e", "-p", "--eval", "--print"],
        &["-r", "--require", "--import", "--loader", "--env-file"],
    ),
    ("ruby", &["-e"], &["-I", "-r"]),
    ("perl", &["-e", "-E"], &["-I"]),
];

/// M11-R6: what a script or program file ends with — a candidate with one of these (or a path
/// separator) is a script; one without is not refused on a misparse.
pub const SCRIPT_EXTENSIONS: [&str; 11] = [
    ".ps1", ".psm1", ".cmd", ".bat", ".sh", ".py", ".js", ".ts", ".rb", ".pl", ".exe",
];

/// The shells of [`INTERPRETERS`] (short-flag clusters, `-c` command strings).
const SH_FAMILY: [&str; 5] = ["sh", "bash", "zsh", "dash", "ksh"];

/// One literal path: no whitespace, no glob and no variable (`* ? [ ] $ % ~`).
fn plain_path(p: &str) -> bool {
    !p.chars()
        .any(|c| c.is_whitespace() || "*?[]$%~".contains(c))
}

fn has_script_ext(p: &str) -> bool {
    let lower = p.to_ascii_lowercase();
    SCRIPT_EXTENSIONS.iter().any(|x| lower.ends_with(x))
}

/// M11-R10 (X7): a script a gate starts — its **entry script**, which decides pass or fail, so
/// the implementer must not be able to write it. `path` as written; `refusable`: admission
/// refuses it when the base commit lacks it and the repository does not ignore it (it has a
/// path separator or a script extension). Every entry is protected, whether or not it exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub refusable: bool,
}

/// A literal path: no whitespace, glob or variable, not a folder (`test/`).
fn is_literal(t: &str) -> bool {
    !t.is_empty() && plain_path(t) && !t.ends_with(['/', '\\'])
}

/// A candidate admission may refuse when missing: a separator or a script extension.
fn sure(t: &str) -> bool {
    t.contains(['/', '\\']) || has_script_ext(t)
}

/// The tokens of a command line: split at whitespace and `; & | ( ) { }`, quotes removed.
fn command_tokens(s: &str) -> Vec<String> {
    s.split(|c: char| c.is_whitespace() || ";&|(){}".contains(c))
        .map(|w| w.trim_matches(['\'', '"']).to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// The entry scripts of a command line (X7): every token that is a literal path with a
/// separator and a script extension (`& { ./x.ps1 }`, `call scripts\x.cmd`, `./ci/x.sh
/// --fast`). PowerShell never runs a bare name from the working directory, `cmd` does: for
/// `cmd` the head command of **each** segment of a `&&`/`&`/`|`/`;`/parenthesised chain (after
/// an optional `call`), without a separator, is an entry too when it has a script extension and
/// no program of that name is on PATH (R10 review: not only the first segment).
fn command_line_entries(s: &str, cmd: bool, on_path: &dyn Fn(&str) -> bool) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    if cmd {
        for seg in s.split(|c| "&|;()".contains(c)) {
            let mut words = seg.split_whitespace().map(|w| w.trim_matches(['\'', '"']));
            let mut head = words.next();
            if head.is_some_and(|h| h.eq_ignore_ascii_case("call")) {
                head = words.next();
            }
            if let Some(f) = head.filter(|f| {
                !f.is_empty()
                    && !f.contains(['/', '\\'])
                    && is_literal(f)
                    && has_script_ext(f)
                    && !on_path(f)
            }) {
                if !out.iter().any(|e| e.path == f) {
                    out.push(Entry {
                        path: f.to_string(),
                        refusable: true,
                    });
                }
            }
        }
    }
    for t in command_tokens(s) {
        if is_literal(&t)
            && t.contains(['/', '\\'])
            && has_script_ext(&t)
            && !out.iter().any(|e| e.path == t)
        {
            out.push(Entry {
                path: t,
                refusable: true,
            });
        }
    }
    out
}

/// PowerShell's `-Command` / `-c`.
fn is_command_flag(a: &str) -> bool {
    ["-command", "-c", "/command", "/c"]
        .iter()
        .any(|f| a.eq_ignore_ascii_case(f))
}

/// M18 (M11-R6, R10): the entry scripts of a verify command, as written ([`Entry`]):
/// - `pwsh` / `powershell`: the argument after the first `-File` (later arguments belong to the
///   script); else the entry scripts of the `-Command` / `-c` line ([`command_line_entries`]);
///   else the first positional `.ps1`;
/// - `cmd`: the entry scripts of the command line after `/c` (`/k`);
/// - a table interpreter (`sh`, `bash`, `python`, `node`, …): the first argument that is neither
///   a flag nor a flag's value — protected whether or not it exists, also without a separator
///   or an extension (`sh check`), refusable only with one; a shell's `-c` (also inside a flag
///   cluster: `-ec`, `-euxc`) gives the entries of its command line; inline code and modules
///   (`python -m`, `node -e`) give none; a folder or a glob is no script;
/// - any other program: none (`-f` of `dotnet`, `docker` or `grep` is not a script).
///
/// `on_path`: whether a program of that name is on PATH (the `cmd` case).
pub fn entry_scripts_with(
    program: &str,
    args: &[String],
    on_path: &dyn Fn(&str) -> bool,
) -> Vec<Entry> {
    let base = program_base(program);
    if base == "pwsh" || base == "powershell" {
        if let Some(i) = args.iter().position(|a| is_file_flag(a)) {
            return args
                .get(i + 1)
                .filter(|a| !a.ends_with(['/', '\\']))
                .map(|a| {
                    vec![Entry {
                        path: a.clone(),
                        refusable: sure(a),
                    }]
                })
                .unwrap_or_default();
        }
        if let Some(i) = args.iter().position(|a| is_command_flag(a)) {
            return command_line_entries(&args[i + 1..].join(" "), false, on_path);
        }
        return args
            .iter()
            .find(|a| {
                !a.starts_with('-') && is_literal(a) && a.to_ascii_lowercase().ends_with(".ps1")
            })
            .map(|a| {
                vec![Entry {
                    path: a.clone(),
                    refusable: true,
                }]
            })
            .unwrap_or_default();
    }
    if base == "cmd" {
        let Some(i) = args
            .iter()
            .position(|a| a.eq_ignore_ascii_case("/c") || a.eq_ignore_ascii_case("/k"))
        else {
            return Vec::new();
        };
        return command_line_entries(&args[i + 1..].join(" "), true, on_path);
    }
    let Some((_, inline, valued)) = INTERPRETERS.iter().find(|(n, _, _)| *n == base) else {
        return Vec::new();
    };
    let shell = SH_FAMILY.contains(&base.as_str());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        // a shell's command line: `-c`, also inside a short-flag cluster (`-ec`, `-euxc`)
        let cluster = a.len() > 2 && a.starts_with('-') && !a.starts_with("--");
        if shell && (a == "-c" || (cluster && a.contains('c'))) {
            return it
                .next()
                .map(|s| command_line_entries(s, false, on_path))
                .unwrap_or_default();
        }
        if inline.iter().any(|f| a == f) {
            return Vec::new();
        }
        if valued.iter().any(|f| a == f) {
            it.next(); // the flag's value
            continue;
        }
        // a shell's short-flag cluster ending in `o` / `O` takes a value (`-euo pipefail`)
        if shell && cluster {
            if a.ends_with(['o', 'O']) {
                it.next();
            }
            continue;
        }
        if !a.starts_with('-') {
            return if is_literal(a) {
                vec![Entry {
                    path: a.clone(),
                    refusable: sure(a),
                }]
            } else {
                Vec::new()
            };
        }
    }
    Vec::new()
}

/// [`entry_scripts_with`] with the real PATH ([`which`]).
pub fn entry_scripts(program: &str, args: &[String]) -> Vec<Entry> {
    entry_scripts_with(program, args, &|n| which(n).is_some())
}

/// The first entry script of a verify command, as written (see [`entry_scripts`]).
pub fn script_of(program: &str, args: &[String]) -> Option<String> {
    entry_scripts(program, args)
        .into_iter()
        .next()
        .map(|e| e.path)
}

/// [`script_of`] when the script is a whole argument (`-File x.ps1`, `bash x.sh`) — that
/// argument; `None` otherwise (also for the first token of `cmd /c "x.cmd arg"`).
pub fn script_arg<'a>(program: &str, args: &'a [String]) -> Option<&'a String> {
    let s = script_of(program, args)?;
    args.iter().find(|a| **a == s)
}

/// M11-R10: what an argument names — for `KEY=value`, `--flag=value`, `-Flag:value` and
/// `/p:Name=value` the value (not the whole argument), else the argument.
fn arg_values(a: &str) -> Vec<String> {
    if let Some((_, val)) = a.split_once('=') {
        return vec![val.to_string()];
    }
    if a.starts_with(['-', '/']) {
        if let Some((_, val)) = a.split_once(':') {
            return vec![val.to_string()];
        }
    }
    vec![a.to_string()]
}

/// M11-R6/R10: the net under [`entry_scripts`] — every other argument (also the value of
/// `KEY=value` and the like, and the tokens of a command line) of a verify command that is a
/// literal repository path with a separator and a script or program extension, as `(command
/// name, path)`. Never a refusal (X7): it is protected when it exists, and a missing one gets a
/// note at admission (build output is fine). The value of `-o` / `--out…` / `-OutFile` is
/// skipped.
pub fn script_like_args(cmds: &[CommandSpec], repo: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for c in cmds {
        let CommandKind::Program { program, args } = c.kind() else {
            continue;
        };
        let entries: Vec<String> = entry_scripts(program, args)
            .into_iter()
            .filter_map(|e| repo_relative(repo, &e.path))
            .collect();
        let output_flag = |a: &str| {
            let l = a.to_ascii_lowercase();
            l == "-o" || l == "-outfile" || l.starts_with("--out")
        };
        let mut prev: Option<&String> = None;
        for a in args {
            let skip = prev.is_some_and(|p| output_flag(p));
            prev = Some(a);
            if skip {
                continue;
            }
            let mut candidates = arg_values(a);
            if a.contains(char::is_whitespace) {
                candidates.extend(command_tokens(a));
            }
            for t in candidates {
                if t.starts_with('-')
                    || !is_literal(&t)
                    || !t.contains(['/', '\\'])
                    || !has_script_ext(&t)
                {
                    continue;
                }
                let Some(r) = repo_relative(repo, &t) else {
                    continue;
                };
                let e = (c.name.clone(), r);
                if !entries.contains(&e.1) && !out.contains(&e) {
                    out.push(e);
                }
            }
        }
    }
    out
}

/// M18: the repository files a verify command starts that admission may refuse — its program
/// when it is a path in the repository, and its refusable entry scripts ([`entry_scripts`]) —
/// as `(command name, path)`, in `git status` form. Task admission (and `task config
/// --reload`) refuses a configuration whose base commit lacks one of them (and does not ignore
/// it): a script the task itself would create could be written by the implementer before
/// verify runs it.
pub fn verify_entry_points(cmds: &[CommandSpec], repo: &Path) -> Vec<(String, String)> {
    verify_entries(cmds, repo)
        .into_iter()
        .filter(|e| e.2)
        .map(|e| (e.0, e.1))
        .collect()
}

/// M11-R14: every repository file a verify command starts — its program when it is a path in
/// the repository, and each of its entry scripts ([`entry_scripts`]) — as `(command name, path,
/// refusable)`. All of them are protected; [`verify_entry_points`] are the refusable ones. One
/// that the base commit lacks and the repository ignores is build output (X9).
pub fn verify_entries(cmds: &[CommandSpec], repo: &Path) -> Vec<(String, String, bool)> {
    let mut out: Vec<(String, String, bool)> = Vec::new();
    for c in cmds {
        let CommandKind::Program { program, args } = c.kind() else {
            continue;
        };
        let program = expand_vars(program).unwrap_or_else(|_| program.to_string());
        if program.contains('/') || program.contains('\\') || Path::new(&program).is_absolute() {
            if let Some(r) = repo_relative(repo, &program) {
                out.push((c.name.clone(), r, true));
            }
        }
        for e in entry_scripts(&program, args) {
            if let Some(r) = repo_relative(repo, &e.path) {
                match out.iter_mut().find(|o| o.0 == c.name && o.1 == r) {
                    Some(o) => o.2 |= e.refusable,
                    None => out.push((c.name.clone(), r, e.refusable)),
                }
            }
        }
    }
    out
}

/// MSBuild reads `Directory.Build.props`/`.targets`/`.rsp` from every parent folder of a
/// project (R59): protected, with [`VERIFY_SCRIPTS`], whenever a `tool: xpp` command runs.
pub const XPP_PROTECTED_GLOB: &str = "**/Directory.Build.*";

/// The protected globs the verify commands add to `policy.protected_paths` (R59): for
/// `tool: xpp`, [`XPP_PROTECTED_GLOB`] and the `obj` folder next to each project (MSBuild
/// imports `obj/<project>.*.props|targets`). Like the files of [`verify_files`], they are
/// protected whether or not the list names [`VERIFY_SCRIPTS`].
pub fn verify_globs(cmds: &[CommandSpec]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in cmds {
        let CommandKind::Xpp { projects, .. } = c.kind() else {
            continue;
        };
        if out.is_empty() {
            out.push(XPP_PROTECTED_GLOB.to_string());
        }
        for p in projects {
            let n = norm_path(p);
            let obj = match n.rsplit_once('/') {
                Some((dir, _)) => format!("{dir}/obj/**"),
                None => "obj/**".to_string(),
            };
            if !out.contains(&obj) {
                out.push(obj);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ac_parser() {
        let t = "# BR-001 — x\n\n## Acceptance criteria\n- AC-1: a\n- **AC-2**: b\n* AC-3. c\n\n## Scope\n- AC-9: not an AC here\n";
        assert_eq!(
            parse_acceptance_ids(t).unwrap(),
            vec!["AC-1", "AC-2", "AC-3"]
        );
        assert!(parse_acceptance_ids("# X\n## Acceptance criteria\nnone\n").is_err());
        assert!(parse_acceptance_ids("## Acceptance criteria\n- AC-1: a\n- AC-1: b\n").is_err());
        assert_eq!(task_id(t, Path::new("x.md"), None), "BR-001");
        assert_eq!(
            task_id("no heading", Path::new("tasks/foo.md"), None),
            "foo"
        );
    }

    #[test]
    fn review_checks() {
        let ids = vec!["AC-1".to_string(), "AC-2".to_string()];
        let ok = json!({"decision":"approved","acceptance_criteria":[{"id":"AC-1","met":true,"evidence":"file.ps1:3"},{"id":"AC-2","met":true,"evidence":"test x"}],"issues":[]});
        assert!(review_violations(&ok, &ids).is_empty());
        let missing = json!({"decision":"approved","acceptance_criteria":[{"id":"AC-1","met":true,"evidence":"e"}],"issues":[]});
        assert!(review_violations(&missing, &ids)[0].contains("missing"));
        let extra = json!({"decision":"approved","acceptance_criteria":[{"id":"AC-1","met":true,"evidence":"e"},{"id":"AC-2","met":true,"evidence":"e"},{"id":"AC-7","met":true,"evidence":"e"}],"issues":[]});
        assert!(review_violations(&extra, &ids)
            .iter()
            .any(|v| v.contains("unknown")));
        let placeholder = json!({"decision":"approved","acceptance_criteria":[{"id":"AC-1","met":true,"evidence":"ok"},{"id":"AC-2","met":true,"evidence":"e"}],"issues":[]});
        assert!(!review_violations(&placeholder, &ids).is_empty());
        let empty_cr = json!({"decision":"changes_requested","acceptance_criteria":[{"id":"AC-1","met":true,"evidence":"e"},{"id":"AC-2","met":true,"evidence":"e"}],"issues":[]});
        assert!(!review_violations(&empty_cr, &ids).is_empty());
        assert!(!review_violations(&json!({}), &ids).is_empty());
    }

    #[test]
    fn protected_paths() {
        let p = Protected::new(
            vec!["scripts/test.ps1".into(), "tasks/BR-001.md".into()],
            &["docs/**".into(), "*.lock".into()],
        );
        assert!(p.is_protected("scripts/test.ps1"));
        assert!(p.is_protected("tasks/BR-001.md"));
        assert!(p.is_protected("AGENTS.md"));
        assert!(p.is_protected("sub/.claude/settings.local.json"));
        assert!(p.is_protected("docs/a/b.md"));
        assert!(p.is_protected("x.lock"));
        assert!(!p.is_protected("a/x.lock"));
        assert!(!p.is_protected("src/BatchRename.psm1"));
        assert!(!p.is_protected("scripts/other.ps1"));
        assert!(!p.is_protected("tasks/BR-002.md"));
        let p = p.with_policy(&["AGENTS.md".into()], &["**/ZZZ/**".into()]);
        assert!(
            !p.is_protected("AGENTS.md") && p.matches("AGENTS.md"),
            "allowed, still snapshotted"
        );
        assert!(
            p.is_denied("Metadata/zzz/Foo.xml") && p.is_denied("ZZZ/a") && p.violates("x/ZZZ/y")
        );
        assert!(!p.is_denied("src/ZZZa/b"));
        let t =
            "# T\n## Allow protected\n- `scripts\\test.ps1`\n- AGENTS.md\n\n## Notes\n- not this\n";
        assert_eq!(
            parse_allow_protected(t),
            vec!["scripts/test.ps1".to_string(), "AGENTS.md".to_string()]
        );
    }

    /// EC-22: the AC set is exactly what a reader sees.
    #[test]
    fn ac_parser_sections_numbered_fenced_and_malformed() {
        let ids = |t: &str| parse_acceptance_ids(t).unwrap();
        // sub-headings stay inside the section; a heading of the same level ends it
        assert_eq!(
            ids("# T-1\n## Acceptance criteria\n### Functional\n- AC-1: a\n#### Edge\n- AC-2: b\n### Non-functional\n- AC-3: c\n## Scope\n- AC-9: no\n"),
            ["AC-1", "AC-2", "AC-3"]
        );
        // a higher-level heading ends it too; a later AC section adds to it
        assert_eq!(
            ids("## Acceptance criteria\n- AC-1: a\n# Notes\n- AC-8: no\n## Acceptance criteria (more)\n- AC-2: b\n"),
            ["AC-1", "AC-2"]
        );
        // numbered items, task-list boxes, bold, a full-width colon
        assert_eq!(
            ids("## Acceptance criteria\n1. AC-1: a\n2) AC-2) b\n- [ ] AC-3: c\n- [x] **AC-4**: d\n- AC-5：日本語の基準\n"),
            ["AC-1", "AC-2", "AC-3", "AC-4", "AC-5"]
        );
        // fenced examples are not contract ids, and a `#` line inside a fence is no heading
        assert_eq!(
            ids("## Acceptance criteria\n- AC-1: a\n```md\n- AC-9: example\n# not a heading\n```\n- AC-2: b\n~~~\n- AC-7: x\n~~~\n````\n```\n- AC-6: still fenced\n````\n"),
            ["AC-1", "AC-2"]
        );
        // inline code is not a fence
        assert_eq!(
            ids("## Acceptance criteria\n```x``` inline\n- AC-1: a\n"),
            ["AC-1"]
        );
        // malformed ids are errors with the id and the line, never dropped
        for (bad, id) in [
            ("- AC-01: a", "AC-01"),
            ("- AC-0: a", "AC-0"),
            ("1. ac-2: a", "ac-2"),
            ("- **AC-1a**: a", "AC-1a"),
            ("- AC-: a", "AC-"),
        ] {
            let e = parse_acceptance_ids(&format!("## Acceptance criteria\n- AC-1: ok\n{bad}\n"))
                .unwrap_err()
                .to_string();
            assert!(
                e.contains(&format!("`{id}`")) && e.contains("line 3"),
                "{bad}: {e}"
            );
        }
        // outside the section a malformed id is just text
        assert_eq!(
            ids("## Notes\n- AC-01: x\n## Acceptance criteria\n- AC-1: a\n"),
            ["AC-1"]
        );
        // an id that is not at the start of an item is text
        assert_eq!(
            ids("## Acceptance criteria\n- AC-1: covers AC-01 and ac-2\n- Note: AC-3 follows\n"),
            ["AC-1"]
        );
    }

    /// EC-09: NTFS is case-insensitive — so is every protected-path comparison.
    #[test]
    fn protected_paths_are_case_insensitive() {
        let p = Protected::new(
            vec!["scripts/Test.ps1".into(), "tasks/BR-001.md".into()],
            &["**/AGENTS.md".into(), "Build-Cfg/**".into()],
        )
        .with_policy(&["docs/ALLOWED.md".into()], &["**/*.pfx".into()]);
        for path in [
            "AGENTS.md",
            "agents.md",
            "Agents.md",
            "sub/agents.MD",
            "claude.md",
            "CLAUDE.LOCAL.md",
            ".Claude/settings.json",
            ".CODEX/config.toml",
            "Toryo.yaml",
            "TORYO.LOCAL.YAML",
            ".GitAttributes",
            ".MCP.json",
            "SCRIPTS/test.ps1",
            "scripts\\TEST.ps1",
            "Tasks/br-001.md",
            "build-cfg/tool.yaml",
            ".githooks/pre-commit",
            "x/.GIT/config",
        ] {
            assert!(p.matches(path) && p.violates(path), "{path}");
        }
        assert!(p.is_allowed("DOCS/allowed.md") && !p.is_protected("docs/allowed.md"));
        assert!(p.is_denied("cert.PFX") && p.is_denied("a/B.Pfx"));
        for path in [
            "src/agents.mdx",
            "scripts/test.ps2",
            ".github/workflows/ci.yml",
        ] {
            assert!(!p.matches(path), "{path}");
        }
    }

    /// EC-08: a protected directory name matches the entry itself (a link or junction put in
    /// its place) at any depth, not only the paths under it.
    #[test]
    fn protected_dirs_match_the_entry_itself() {
        let p = Protected::new(vec![], &[]);
        for path in [
            ".claude",
            "sub/.claude",
            ".claude/",
            ".codex",
            "a/b/.codex",
            ".toryo",
            ".githooks",
            ".git",
            "vendor/lib/.git",
            ".claude/settings.local.json",
            "sub/.toryo/policy.md",
            "./.claude",
        ] {
            assert!(p.matches(path) && p.violates(path), "{path}");
        }
        for path in [
            ".claude-backup/x",
            "claude/x",
            ".github/x",
            "docs/.gitkeep",
            "x.claude",
        ] {
            assert!(!p.matches(path), "{path}");
        }
        // the same for a configured directory glob (`dir/**`), and for deny/allow globs
        let p = Protected::new(vec![], &["build-cfg/**".into(), "**/$weird$/**".into()])
            .with_policy(&["docs/**".into()], &["**/ZZZ/**".into()]);
        for path in ["build-cfg", "Build-Cfg", "build-cfg/x.yaml", "a/$weird$"] {
            assert!(p.matches(path), "{path}");
        }
        for path in ["build-cfgx", "build-cfg-old/x", "a/weird"] {
            assert!(!p.matches(path), "{path}");
        }
        for path in ["ZZZ", "Metadata/zzz", "Metadata/ZZZ/Foo.xml", "ZZZ/"] {
            assert!(p.is_denied(path), "{path}");
        }
        for path in ["ZZZa", "src/ZZZa/b", "ZZZ.xml"] {
            assert!(!p.is_denied(path), "{path}");
        }
        assert!(p.is_allowed("docs") && p.is_allowed("DOCS/a.md") && !p.is_allowed("docsx"));
    }

    /// M11-R4 AC-1: the five forms that matched nothing — each one either matches
    /// `secrets/key.pfx` once normalised (as `never` and as `writes`) or is refused at load.
    #[test]
    fn fence_patterns_are_normalised_or_refused() {
        let ok = [
            ("./secrets/**", "secrets/**"),
            ("secrets/", "secrets/**"),
            ("secrets", "secrets/**"),
            (r".\secrets\", "secrets/**"),
            ("secrets//*.pfx", "secrets/*.pfx"),
            ("**/*.pfx", "**/*.pfx"),
            ("**", "**"),
            ("CULTURE.md", "CULTURE.md/**"),
            // R4–R8 review: gitignore-like forms
            ("*.pfx", "**/*.pfx"),
            ("**/secrets", "**/secrets/**"),
            ("*/secrets", "*/secrets/**"),
            ("src/*/", "src/*/**"),
        ];
        for (written, want) in ok {
            let n = fence_pattern(written).unwrap();
            assert_eq!(n, want, "{written}");
            if n.starts_with("secrets") || n.starts_with("**") {
                let never = Protected::new(vec![], &[]).with_fence(&[], std::slice::from_ref(&n));
                assert!(never.violates("secrets/key.pfx"), "never {written}");
                assert!(never.violates(r"Secrets\Key.PFX"), "never {written}");
                let writes = Protected::new(vec![], &[]).with_fence(std::slice::from_ref(&n), &[]);
                assert!(!writes.violates("secrets/key.pfx"), "writes {written}");
            }
        }
        // a pattern without a wildcard: the path itself and everything under it
        let f =
            Protected::new(vec![], &[]).with_fence(&[], &[fence_pattern("CULTURE.md").unwrap()]);
        assert!(f.violates("culture.md"));
        assert!(!f.violates("CULTURE.md.bak"));
        // R4–R8 review: `**/secrets` and `*.pfx` fence what is below and deeper
        let deep = |p: &str| fence_pattern(p).unwrap();
        let f = Protected::new(vec![], &[]).with_fence(&[], &[deep("**/secrets"), deep("*.pfx")]);
        assert!(f.violates("a/secrets/key.txt") && f.violates("secrets/x"));
        assert!(f.violates("deep/dir/cert.pfx") && !f.violates("deep/dir/cert.pem"));
        // a task frozen before R4 holds `./secrets/`: normalised by the fence itself
        let old = Protected::new(vec![], &[]).with_fence(&[], &["./secrets/".into()]);
        assert!(old.violates("secrets/key.pfx"));
        // `## Allow protected` with `./`
        let allow =
            parse_allow_protected("## Allow protected\n- ./CULTURE.md\n- `.\\docs\\x.md`\n");
        assert_eq!(allow, ["CULTURE.md", "docs/x.md"]);
        for (written, why) in [
            ("/secrets/**", "is absolute"),
            (r"C:\secrets\**", "is absolute"),
            (r"\\server\share", "is absolute"),
            (
                "**/*.{pfx,key}",
                "uses `{`, which the matcher does not support — only `*`",
            ),
            ("secrets/[ab].pfx", "uses `[`"),
            ("!secrets/**", "uses `!`"),
            // M11-R12: a variable or home shortcut is refused (never expanded for a fence)
            ("%APPDATA%/x", "uses `%`"),
            ("~/secrets/**", "uses `~`"),
            ("$HOME/x", "uses `$`"),
            ("src/../secrets/**", "may not contain `..`"),
            ("", "is empty"),
            ("  ", "is empty"),
            ("./", "names no path"),
        ] {
            let e = fence_pattern(written).unwrap_err();
            assert!(e.contains(why), "{written}: {e}");
        }
    }

    /// M11-R4 AC-3: the legacy `deny_paths` / `protected_paths` forms that can never match.
    #[test]
    fn legacy_patterns_that_never_match_are_named() {
        for (p, want) in [
            ("./x/**", "write `x/**`"),
            ("x/", "write `x/**`"),
            ("a//b", "write `a/b`"),
            ("x/**/", "write `x/**`"),
            ("/x/**", "is absolute"),
            ("../x", "has a `..` segment"),
        ] {
            let w = dead_pattern(p).unwrap_or_else(|| panic!("{p}"));
            assert!(w.contains(want), "{p}: {w}");
        }
        // M11-R12: the legacy matcher escapes `{ } [ ] ! % ~ $`, so these match literally and
        // are not dead (unlike a fence, which refuses them)
        for p in [
            "**/ZZZ/**",
            "secrets",
            "docs/*.md",
            "**/*.{pfx,key}",
            "secrets/[ab].pfx",
            "!x",
            VERIFY_SCRIPTS,
        ] {
            assert_eq!(dead_pattern(p), None, "{p}");
        }
    }

    /// M11-C2 AC-3/AC-4: the write fence — outside `writes` or inside `never` violates, `never`
    /// wins, matched case-insensitively with `/` and `\` alike; `## Allow protected` opens it;
    /// it only adds violations (a protected path inside `writes` stays protected, a denied one
    /// stays denied even when allowed); nothing changes for `matches` (the R35 snapshot).
    #[test]
    fn the_write_fence() {
        let p = Protected::new(vec![], &["Metadata/Pkg/Config/**".into()])
            .with_policy(&[], &["**/ZZZ/**".into()])
            .with_fence(&["Metadata/**".into()], &["**/Descriptor/*.xml".into()]);
        for ok in [
            "Metadata/Pkg/Model/AxClass/Foo.xml",
            "metadata/pkg/model/axclass/foo.xml",
            "Metadata\\Pkg\\Model\\AxClass\\Foo.xml",
            "Metadata",
        ] {
            assert_eq!(p.fence_rule(ok), None, "{ok}");
            assert!(!p.violates(ok), "{ok}");
        }
        for (bad, never) in [
            ("docs/x.md", false),
            ("README.md", false),
            ("Metadata/Pkg/Model/Descriptor/Model.xml", true),
            ("metadata/pkg/model/descriptor/model.XML", true),
            ("Metadata\\Pkg\\Model\\Descriptor\\Model.xml", true),
        ] {
            let r = p.fence_rule(bad).unwrap_or_else(|| panic!("{bad}"));
            assert_eq!(matches!(r, FenceRule::Never(_)), never, "{bad}: {r:?}");
            assert!(p.violates(bad), "{bad}");
            assert!(!p.matches(bad), "the fence is not a protected path: {bad}");
        }
        // AC-4: protected inside `writes` stays protected; denied stays denied
        assert!(p.fence_rule("Metadata/Pkg/Config/x.xml").is_none());
        assert!(p.violates("Metadata/Pkg/Config/x.xml"));
        assert!(p.violates("Metadata/ZZZ/x.xml"));
        // `## Allow protected` opens a fence entry, never a denied path
        let allowed = Protected::new(vec![], &[])
            .with_policy(
                &[
                    "Metadata/Pkg/Model/Descriptor/Model.xml".into(),
                    "Metadata/ZZZ/x.xml".into(),
                ],
                &["**/ZZZ/**".into()],
            )
            .with_fence(&["Metadata/**".into()], &["**/Descriptor/*.xml".into()]);
        assert!(!allowed.violates("Metadata/Pkg/Model/Descriptor/Model.xml"));
        assert!(allowed.violates("Metadata/ZZZ/x.xml"));
        // `never` alone fences nothing else; no fence at all: unchanged
        let never_only = Protected::new(vec![], &[]).with_fence(&[], &["**/*.pfx".into()]);
        assert!(!never_only.violates("docs/x.md") && never_only.violates("a/b.PFX"));
        let none = Protected::new(vec![], &[]).with_fence(&[], &[]);
        assert!(!none.violates("anything/at/all.txt"));
    }

    /// EC-07: a verify program that is a repo file is a verify file (protected, hashed);
    /// `@verify-scripts` stands for the verify files, not for a path of that name.
    #[test]
    fn verify_files_include_a_repo_program() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path();
        for f in [
            "scripts/gate.sh",
            "gradlew",
            "scripts/build.cmd",
            "scripts/test.ps1",
            "@verify-scripts",
        ] {
            let p = repo.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x").unwrap();
        }
        let outside = d.path().parent().unwrap().join("outside-tool.sh");
        let cmd = |program: &str, args: &[&str]| CommandSpec::program(program, program, args, None);
        let cmds = [
            cmd("scripts/gate.sh", &[]),
            cmd("./gradlew", &["build"]),
            cmd("scripts\\build.cmd", &[]),
            cmd("pwsh", &["-NoProfile", "-File", "./scripts/test.ps1"]),
            cmd("git", &["grep", "-q", "x", "--", "src"]),
            cmd(&outside.display().to_string(), &[]),
            cmd("scripts/missing.sh", &["../escape.txt"]),
            cmd("scripts/gate.sh", &["scripts/gate.sh"]),
        ];
        let files = verify_files(&cmds, repo);
        // M18 (M11-E1): `scripts/missing.sh` is protected although it does not exist yet
        assert_eq!(
            files,
            [
                "scripts/gate.sh",
                "gradlew",
                "scripts/build.cmd",
                "scripts/test.ps1",
                "scripts/missing.sh"
            ]
        );
        let p = Protected::new(files, &[VERIFY_SCRIPTS.into(), "docs/**".into()]);
        for f in [
            "scripts/gate.sh",
            "gradlew",
            "scripts/build.cmd",
            "scripts/test.ps1",
        ] {
            assert!(p.violates(f), "{f}");
        }
        assert!(
            !p.matches("@verify-scripts"),
            "the macro is not a literal path"
        );
        assert!(p.matches("docs/x.md"), "the other globs still apply");
    }

    /// M11-R14 AC-4 (X9): a task's protected set is decided from the base commit — files that
    /// appear in the worktree later (build output passed to a gate) do not change it, while
    /// the filesystem form (`verify_files`, right for the verify checkout) does see them. An
    /// entry script admitted as build output leaves the snapshot matcher and stays a violation.
    #[test]
    fn the_protected_set_comes_from_the_base_commit() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path();
        let cmd = |name: &str, program: &str, args: &[&str]| {
            CommandSpec::program(name, program, args, None)
        };
        let cmds = [
            cmd(
                "smoke",
                "pwsh",
                &["-File", "scripts/smoke.ps1", "-Exe", "build/app.exe"],
            ),
            cmd(
                "unit",
                "mocha",
                &["out/test/index.js", "Config=conf/ci.json"],
            ),
            cmd("cli", "node", &["dist/cli.js"]),
        ];
        let base: std::collections::HashSet<String> =
            ["scripts/smoke.ps1", "conf/ci.json", "src/a.ts"]
                .map(String::from)
                .into();
        let want = [
            "scripts/smoke.ps1",
            "conf/ci.json",
            "dist/cli.js", // an entry script: protected whether or not it exists
        ];
        assert_eq!(verify_files_at_base(&cmds, repo, &base), want);
        for f in ["build/app.exe", "out/test/index.js", "dist/cli.js"] {
            let p = repo.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x").unwrap();
        }
        assert_eq!(
            verify_files_at_base(&cmds, repo, &base),
            want,
            "the same set after the files appeared"
        );
        let now = verify_files(&cmds, repo);
        assert!(
            now.contains(&"build/app.exe".to_string())
                && now.contains(&"out/test/index.js".to_string()),
            "the filesystem form sees them: {now:?}"
        );
        // `dist/cli.js` admitted as build output: out of the snapshot, still never committed
        let entry = crate::config::BuildEntry {
            gate: "cli".into(),
            path: "dist/cli.js".into(),
        };
        let p = Protected::new(verify_files_at_base(&cmds, repo, &base), &[]);
        assert!(p.matches("dist/cli.js"));
        let p = p.with_build_entries(std::slice::from_ref(&entry));
        assert!(!p.matches("dist/cli.js") && !p.matches("DIST\\CLI.js"));
        assert!(p.violates("dist/cli.js") && p.is_protected("Dist/Cli.js"));
        assert_eq!(p.build_entry("dist\\cli.js"), Some("cli"));
        assert_eq!(p.exact_paths(), ["conf/ci.json", "scripts/smoke.ps1"]);
        assert!(!p.violates("build/app.exe") && !p.violates("dist/other.js"));
        let p = p.with_policy(&["dist/**".to_string()], &[]);
        assert!(!p.violates("dist/cli.js"), "`## Allow protected` opens it");
    }

    /// M18 (M11-E1): the repository program and the `-File` script of a verify command are
    /// protected whether or not they exist yet, and they are the entry points task admission
    /// looks for in the base commit; a bare program, an absolute one outside the repository
    /// and the other arguments are not entry points.
    #[test]
    fn m18_entry_points_are_protected_before_they_exist() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path();
        std::fs::create_dir_all(repo.join("scripts")).unwrap();
        std::fs::write(repo.join("scripts/existing.ps1"), "x").unwrap();
        let outside = d.path().parent().unwrap().join("tool.exe");
        let cmd = |name: &str, program: &str, args: &[&str]| {
            CommandSpec::program(name, program, args, None)
        };
        let cmds = [
            cmd(
                "build",
                "pwsh",
                &["-NoProfile", "-File", "scripts/build-model.ps1"],
            ),
            cmd(
                "lint",
                "powershell",
                &["-file", "scripts\\lint.ps1", "-Strict"],
            ),
            cmd("gate", "scripts/gate.sh", &[]),
            cmd(
                "ext",
                &outside.display().to_string(),
                &["-File", "scripts/existing.ps1"],
            ),
            cmd("plain", "git", &["status", "scripts/not-a-script.txt"]),
        ];
        let files = verify_files(&cmds, repo);
        for f in [
            "scripts/build-model.ps1",
            "scripts/lint.ps1",
            "scripts/gate.sh",
            "scripts/existing.ps1",
        ] {
            assert!(files.contains(&f.to_string()), "{f}: {files:?}");
        }
        assert!(
            !files.iter().any(|f| f.contains("not-a-script")),
            "{files:?}"
        );
        let p = Protected::new(files, &[VERIFY_SCRIPTS.into()]);
        assert!(p.violates("scripts/build-model.ps1") && p.violates("Scripts/Lint.ps1"));
        // `ext` is not PowerShell: its `-File` is not a script (the existing file is still
        // protected, as any existing argument)
        assert_eq!(
            verify_entry_points(&cmds, repo),
            [
                ("build".to_string(), "scripts/build-model.ps1".to_string()),
                ("lint".to_string(), "scripts/lint.ps1".to_string()),
                ("gate".to_string(), "scripts/gate.sh".to_string()),
            ]
        );
    }

    /// Group E review: the script of an interpreter is an entry point; `-f` of other programs,
    /// inline code, modules and a script's own arguments are not.
    #[test]
    fn m18_script_arguments_per_program() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let cases: &[(&str, &[&str], Option<&str>)] = &[
            (
                "pwsh",
                &["-NoProfile", "-File", "s.ps1", "-f", "x"],
                Some("s.ps1"),
            ),
            ("C:/PS7/pwsh.exe", &["-file", "a/b.ps1"], Some("a/b.ps1")),
            ("powershell", &["-Command", "x"], None),
            ("sh", &["scripts/later.sh"], Some("scripts/later.sh")),
            ("bash", &["-e", "-x", "ci/run.sh", "arg"], Some("ci/run.sh")),
            ("sh", &["-c", "exit 0"], None),
            ("python3", &["tools/check.py", "-v"], Some("tools/check.py")),
            ("python", &["-m", "pytest", "tests"], None),
            ("node", &["-e", "1"], None),
            ("cmd", &["/c", r"scripts\b.cmd"], Some(r"scripts\b.cmd")),
            ("cmd.exe", &["/C", "echo", "hi"], None),
            ("dotnet", &["test", "-f", "net8.0"], None),
            ("docker", &["compose", "-f", "build/gen.yml", "up"], None),
            ("grep", &["-F", "TODO", "src"], None),
            ("cargo", &["test", "-F", "feat"], None),
        ];
        for (program, args, want) in cases {
            let a = s(args);
            assert_eq!(
                script_arg(program, &a).map(String::as_str),
                *want,
                "{program} {args:?}"
            );
        }
    }

    /// M11-R6 AC-1: no false script for a flag's value, and the scripts the table missed —
    /// pwsh positional / `-Command`, `cmd /c "x.cmd arg"`; a candidate without a separator or a
    /// script extension is no script (never a refusal on a misparse).
    #[test]
    fn script_detection_skips_flag_values_and_finds_more_scripts() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let cases: &[(&str, &[&str], Option<&str>)] = &[
            (
                "bash",
                &["-o", "pipefail", "scripts/x.sh"],
                Some("scripts/x.sh"),
            ),
            ("sh", &["-e", "-O", "extglob", "ci/t.sh"], Some("ci/t.sh")),
            ("node", &["-r", "ts-node/register", "x.ts"], Some("x.ts")),
            (
                "node",
                &["--require", "a", "--import", "b", "t/x.js"],
                Some("t/x.js"),
            ),
            ("python", &["-W", "error", "x.py"], Some("x.py")),
            (
                "python3",
                &["-X", "utf8", "tools/c.py", "-v"],
                Some("tools/c.py"),
            ),
            ("ruby", &["-I", "lib", "t.rb"], Some("t.rb")),
            ("ruby", &["-r", "json", "t/a.rb"], Some("t/a.rb")),
            ("perl", &["-I", "lib", "t.pl"], Some("t.pl")),
            ("pwsh", &["scripts/x.ps1"], Some("scripts/x.ps1")),
            (
                "pwsh",
                &["-NoProfile", "-ExecutionPolicy", "Bypass", "new.ps1"],
                Some("new.ps1"),
            ),
            ("pwsh", &["-Command", "./x.ps1"], Some("./x.ps1")),
            (
                "powershell",
                &["-c", "& './s/x.ps1' -A 1"],
                Some("./s/x.ps1"),
            ),
            (
                "pwsh",
                &["-Command", ". .\\lib\\x.ps1; Go"],
                Some(".\\lib\\x.ps1"),
            ),
            ("pwsh", &["-Command", "Get-ChildItem"], None),
            ("cmd", &["/c", "x.cmd arg"], Some("x.cmd")),
            (
                "cmd",
                &["/c", "\"scripts\\b.bat\" -q"],
                Some("scripts\\b.bat"),
            ),
            ("cmd", &["/c", "echo hi"], None),
            // a misparse is no script: no separator, no extension
            ("bash", &["-o", "pipefail"], None),
            // M11-R10 (changed): the first argument of a table interpreter is its entry script
            // also without a separator or an extension — protected, not refusable
            ("sh", &["--posix", "later"], Some("later")),
            ("dotnet", &["test", "-f", "net8.0"], None),
            // R4–R8 review: shell flag clusters, a folder or a glob in the script position
            ("bash", &["-euo", "pipefail", "ci/run"], Some("ci/run")),
            ("sh", &["-ec", "exit 0"], None),
            ("node", &["--test", "test/"], None),
            ("node", &["--test", "test/**/*.test.js"], None),
        ];
        for (program, args, want) in cases {
            assert_eq!(
                script_of(program, &s(args)).as_deref(),
                *want,
                "{program} {args:?}"
            );
        }
        // the net: arguments naming repository scripts or programs, other files are not
        let d = tempfile::tempdir().unwrap();
        let outside = d
            .path()
            .parent()
            .unwrap()
            .join("a.ps1")
            .display()
            .to_string();
        let cmds = [
            CommandSpec::program(
                "t",
                "dotnet",
                &["run", "tools/gen.exe", "--out", "build/report.xml"],
                None,
            ),
            // (the table finds this one; the net wants a path separator)
            CommandSpec::program("c", "cmd", &["/c", "x.cmd arg"], None),
            CommandSpec::program("abs", "pwsh", &["-File", &outside], None),
            CommandSpec::program("sh", "bash", &["-c", "./ci/x.sh --fast"], None),
            // R4–R8 review: none of these is a script the base commit must hold
            CommandSpec::program("lint", "npx", &["eslint", "src/**/*.js"], None),
            CommandSpec::program("tsc", "tsc", &["--outFile", "build/x.js"], None),
            CommandSpec::program("out", "gen", &["-o", "build/x.ps1"], None),
            CommandSpec::program("cd", "bash", &["-c", "cd src && ./test.sh"], None),
            CommandSpec::program(
                "pester",
                "pwsh",
                &["-Command", "Invoke-Pester tests/x.Tests.ps1"],
                None,
            ),
            CommandSpec::program("var", "bash", &["-c", "$HOME/bin/check.sh"], None),
            CommandSpec::program("bare", "where", &["git.exe"], None),
            CommandSpec::program("call", "cmd", &["/c", "call x.cmd"], None),
        ];
        let net = script_like_args(&cmds, d.path());
        assert_eq!(
            net,
            // M11-R10 (changed): `./ci/x.sh` in `bash -c` is an entry script now, not the net
            [("t".to_string(), "tools/gen.exe".to_string())],
            "{net:?}"
        );
        assert!(verify_entry_points(&cmds, d.path())
            .contains(&("sh".to_string(), "ci/x.sh".to_string())));
        // the entry script is protected
        let files = verify_files(&cmds, d.path());
        assert!(files.contains(&"ci/x.sh".to_string()), "{files:?}");
    }

    /// M11-R10 AC-1 (X7): for each command line of the list — the entry scripts, whether a
    /// missing one is refused, the net (never refused) and what is protected (in a repository
    /// where only `ci/x.sh` exists).
    #[test]
    fn entry_scripts_refusals_and_protection() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let off_path = |_: &str| false;
        // (program, args, entries as (path, refusable), net)
        type Case<'a> = (&'a str, &'a [&'a str], &'a [(&'a str, bool)], &'a [&'a str]);
        let cases: &[Case] = &[
            (
                "bash",
                &["-o", "pipefail", "scripts/x.sh"],
                &[("scripts/x.sh", true)],
                &[],
            ),
            (
                "bash",
                &["-ec", "./ci/x.sh --fast"],
                &[("./ci/x.sh", true)],
                &[],
            ),
            ("sh", &["check"], &[("check", false)], &[]),
            ("sh", &["-c", "make test"], &[], &[]),
            (
                "node",
                &["-r", "ts-node/register", "x.ts"],
                &[("x.ts", true)],
                &[],
            ),
            (
                "node",
                &["--env-file", ".env", "x.js"],
                &[("x.js", true)],
                &[],
            ),
            ("node", &["dist/cli.js"], &[("dist/cli.js", true)], &[]),
            ("python", &["-m", "pytest", "tests/"], &[], &[]),
            (
                "pwsh",
                &[
                    "-NoProfile",
                    "-File",
                    "scripts/build.ps1",
                    "-Configuration",
                    "Release",
                ],
                &[("scripts/build.ps1", true)],
                &[],
            ),
            ("pwsh", &["scripts/x.ps1"], &[("scripts/x.ps1", true)], &[]),
            (
                "pwsh",
                &["-Command", "./x.ps1 -a 1"],
                &[("./x.ps1", true)],
                &[],
            ),
            (
                "pwsh",
                &["-Command", "& { ./x.ps1 }"],
                &[("./x.ps1", true)],
                &[],
            ),
            ("pwsh", &["-Command", "msbuild.exe App.sln"], &[], &[]),
            ("pwsh", &["-Command", "npm.cmd run build"], &[], &[]),
            (
                "pwsh",
                &["-File", "scripts/smoke.ps1", "-Exe", "build/app.exe"],
                &[("scripts/smoke.ps1", true)],
                &["build/app.exe"],
            ),
            ("cmd", &["/c", "x.cmd arg"], &[("x.cmd", true)], &[]),
            // R10 review: a bare entry in a later `&&` segment, not only the first
            (
                "cmd",
                &["/c", "setup.exe && run.cmd"],
                &[("setup.exe", true), ("run.cmd", true)],
                &[],
            ),
            (
                "cmd",
                &["/c", r"call scripts\x.cmd"],
                &[(r"scripts\x.cmd", true)],
                &[],
            ),
            ("make", &["SCRIPT=ci/x.sh", "test"], &[], &["ci/x.sh"]),
            ("dotnet", &["test", "-f", "net8.0"], &[], &[]),
            ("mocha", &["out/test/index.js"], &[], &["out/test/index.js"]),
        ];
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("ci")).unwrap();
        std::fs::write(d.path().join("ci").join("x.sh"), "exit 0\n").unwrap();
        for (program, args, entries, net) in cases {
            let a = s(args);
            let got: Vec<(String, bool)> = entry_scripts_with(program, &a, &off_path)
                .into_iter()
                .map(|e| (e.path, e.refusable))
                .collect();
            let want: Vec<(String, bool)> =
                entries.iter().map(|(p, r)| (p.to_string(), *r)).collect();
            assert_eq!(got, want, "{program} {args:?}");
            let cmd = [CommandSpec::program("g", program, args, None)];
            let got_net: Vec<String> = script_like_args(&cmd, d.path())
                .into_iter()
                .map(|(_, p)| p)
                .collect();
            assert_eq!(got_net, *net, "net: {program} {args:?}");
            // protected: every entry (also missing), the net only when it exists
            let files = verify_files(&cmd, d.path());
            for (p, _) in *entries {
                let r = repo_relative(d.path(), p).unwrap();
                assert!(
                    files.contains(&r),
                    "{r} protected: {program} {args:?} {files:?}"
                );
            }
            for p in *net {
                assert_eq!(
                    files.contains(&p.to_string()),
                    *p == "ci/x.sh",
                    "{p}: {files:?}"
                );
            }
        }
        // `cmd` runs a bare name from PATH when there is one: then it is no entry script
        let a = s(&["/c", "x.cmd arg"]);
        assert!(entry_scripts_with("cmd", &a, &|n| n == "x.cmd").is_empty());
    }

    /// R59 (as EC-07): `@verify-scripts` covers the projects of a `tool: xpp` command — also
    /// one that does not exist yet — and every `Directory.Build.*`; nothing for a program-only
    /// configuration.
    #[test]
    fn xpp_projects_and_directory_build_files_are_verify_files() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path();
        let xpp = CommandSpec::xpp(
            "build",
            "Metadata",
            &[
                "Projects/Build/Core.Build.rnrproj",
                "Projects\\Build\\Missing.rnrproj",
            ],
            None,
        );
        let git = CommandSpec::program("grep", "git", &["grep", "x"], None);
        assert!(verify_globs(std::slice::from_ref(&git)).is_empty());
        let cmds = [git, xpp];
        let files = verify_files(&cmds, repo);
        assert_eq!(
            files,
            [
                "Projects/Build/Core.Build.rnrproj",
                "Projects/Build/Core.Build.rnrproj.user",
                "Projects/Build/Missing.rnrproj",
                "Projects/Build/Missing.rnrproj.user"
            ]
        );
        let mut globs = vec![VERIFY_SCRIPTS.to_string()];
        globs.extend(verify_globs(&cmds));
        let p = Protected::new(files, &globs);
        for f in [
            "projects/build/core.build.rnrproj",
            "Projects/Build/Missing.rnrproj",
            "Directory.Build.props",
            "Metadata/Directory.Build.targets",
            "Projects/Build/directory.build.rsp",
            "Projects/Build/Core.Build.rnrproj.user",
            "Projects/Build/obj/Core.Build.rnrproj.x.targets",
        ] {
            assert!(p.violates(f), "{f}");
        }
        for f in [
            "Metadata/Core/AxClass/Foo.xml",
            "Projects/Work/Core.rnrproj",
        ] {
            assert!(!p.violates(f), "{f}");
        }
    }
}
