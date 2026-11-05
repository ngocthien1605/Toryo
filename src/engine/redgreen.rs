//! M17 Part A — the red-green proof gate (X47–X54): the pure parts. A check gate with a
//! `red_green` block ([`crate::config::RedGreen`]) proves, per acceptance criterion, that the
//! tests the implementer names in `IMPLEMENTATION.json` `proof` **fail** on the base commit with
//! the task's test files laid over it (red) and **pass** on the checkpoint (green).
//!
//! Here: the `## Proof exempt` section of TASK.md ([`parse_proof_exempt`]), the test-id rules
//! ([`test_id_problem`]), the proof map and its validation ([`plan`]), the red commit
//! ([`red_commit`]), the result in `VERIFY_RESULT.json` ([`RedGreenResult`]) and the words the
//! implementer, the reviewer and the owner read. The runs themselves are the verify stage's
//! (`runner/red_green.rs`).

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::RedGreen;
use crate::git::{GitCmd, Loc};

/// The heading of the section (case-insensitive, after the `#`s).
pub const EXEMPT_HEADING: &str = "proof exempt";

/// One `- AC-n: reason` item of `## Proof exempt` (AC-5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exempt {
    pub ac: String,
    pub reason: String,
}

/// M17-A2 (AC-5): the `## Proof exempt` section of TASK.md — `- AC-n: reason` items (a `-` or `*`
/// bullet; `:`, `：` or a dash set off by spaces after the id). Parsed and refused at `task create`
/// (the snapshot of TASK.md is what the task keeps): an id that is not one of `acceptance_ids`, an
/// id listed twice, an item without a reason and a bullet that is not such an item are refused
/// with their line numbers. The section ends at the next heading of the same or a higher level;
/// fenced code blocks are examples. Other text in the section is prose. `parse_acceptance_ids` is
/// untouched: a `##` section of its own is outside `## Acceptance criteria`.
pub fn parse_proof_exempt(task: &str, acceptance_ids: &[String]) -> Result<Vec<Exempt>> {
    let mut section: Option<usize> = None;
    let mut in_fence: Option<(u8, usize)> = None;
    let mut out: Vec<Exempt> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    for (n, line) in task.lines().enumerate() {
        let line_no = n + 1;
        let t = line.trim_start();
        // fenced code blocks (``` or ~~~): examples, never items
        let run = |c: u8| t.bytes().take_while(|b| *b == c).count();
        match in_fence {
            Some((c, k)) => {
                if run(c) >= k && t[run(c)..].trim().is_empty() {
                    in_fence = None;
                }
                continue;
            }
            None => {
                if let Some(c) = b"`~".iter().copied().find(|c| run(*c) >= 3) {
                    in_fence = Some((c, run(c)));
                    continue;
                }
            }
        }
        let trimmed = line.trim();
        let level = trimmed.bytes().take_while(|b| *b == b'#').count();
        if level > 0 {
            if section.is_none_or(|l| level <= l) {
                section = trimmed[level..]
                    .trim()
                    .eq_ignore_ascii_case(EXEMPT_HEADING)
                    .then_some(level);
            }
            continue;
        }
        if section.is_none() {
            continue;
        }
        let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        else {
            continue;
        };
        let item = item.trim().trim_start_matches("**");
        let id: String = item
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        let rest = item[id.len()..].trim_start_matches("**");
        let reason = ["：", ":", " — ", " – ", " - "]
            .iter()
            .find_map(|sep| rest.strip_prefix(sep))
            .map(str::trim);
        if !id.starts_with("AC-") || !acceptance_id(&id) {
            problems.push(format!(
                "line {line_no}: `{trimmed}` — an item of `## Proof exempt` is `- AC-<n>: reason`"
            ));
            continue;
        }
        let Some(reason) = reason.filter(|r| !r.is_empty()) else {
            problems.push(format!(
                "line {line_no}: `{id}` has no reason — write `- {id}: why this criterion has no test that proves it`"
            ));
            continue;
        };
        if !acceptance_ids.contains(&id) {
            problems.push(format!(
                "line {line_no}: `{id}` is not an acceptance criterion of this task (they are {})",
                acceptance_ids.join(", ")
            ));
            continue;
        }
        if out.iter().any(|e| e.ac == id) {
            problems.push(format!("line {line_no}: `{id}` is listed twice"));
            continue;
        }
        out.push(Exempt {
            ac: id,
            reason: reason.to_string(),
        });
    }
    if !problems.is_empty() {
        bail!("TASK.md: `## Proof exempt` (M17): {}", problems.join("; "));
    }
    Ok(out)
}

/// `AC-<n>`, n = 1, 2, … without a leading zero (the ids `parse_acceptance_ids` accepts).
fn acceptance_id(id: &str) -> bool {
    id.strip_prefix("AC-").is_some_and(|n| {
        !n.is_empty() && !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit())
    })
}

// ── test ids (TASK.md "Test id rules") ─────────────────────────────────────────────────────

/// The longest test id, in bytes.
pub const MAX_TEST_ID: usize = 200;

/// The characters a test id may hold: `[A-Za-z0-9_:./\-\[\]=,@+#]`.
fn id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_:./-[]=,@+#".contains(c)
}

/// M17-A4: why `id` is not a test id Toryo passes to a program, or `None`. The ids come from an
/// agent and become arguments — sometimes of a `.cmd` wrapper — so they must be inert: 1..=200
/// bytes, ASCII from the charset above, not starting with `-` (an option) or `/` (a path or a
/// Windows switch). No whitespace, quote, `% ! ^ & | < > ; $ ( ) { }` or backtick can pass.
pub fn test_id_problem(id: &str) -> Option<String> {
    let shown: String = id.chars().take(40).collect();
    let shown = format!(
        "{:?}{}",
        shown,
        if id.chars().count() > 40 { "…" } else { "" }
    );
    if id.is_empty() {
        return Some("an empty test id".into());
    }
    if id.len() > MAX_TEST_ID {
        return Some(format!(
            "test id {shown} is {} bytes long (at most {MAX_TEST_ID})",
            id.len()
        ));
    }
    if let Some(c) = id.chars().find(|c| !id_char(*c)) {
        let what = if c.is_ascii() {
            format!("{c:?}")
        } else {
            format!("the non-ASCII character {c:?}")
        };
        return Some(format!(
            "test id {shown} holds {what} — a test id is ASCII letters, digits and `_ : . / - [ ] = , @ + #`"
        ));
    }
    if id.starts_with('-') || id.starts_with('/') {
        return Some(format!(
            "test id {shown} starts with `{}` — it would be read as an option or a path",
            &id[..1]
        ));
    }
    None
}

// ── the proof map (IMPLEMENTATION.json `proof`) ────────────────────────────────────────────

/// One acceptance criterion of a valid proof map: its tests (none for an exempt criterion the
/// implementer did not map), in the order written, once each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcProof {
    pub ac: String,
    pub exempt: bool,
    pub tests: Vec<String>,
}

/// M17-A4: a proof map that passed [`plan`]: every acceptance criterion in TASK.md order, and the
/// distinct test ids in first-seen order (one run per side each, whatever number of criteria it
/// proves — edge case 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProofPlan {
    pub acs: Vec<AcProof>,
    pub tests: Vec<String>,
}

/// M17-A4 (AC-4): check the implementer's proof map before any process starts — each problem in
/// one line: a non-exempt criterion without a test, an unknown `AC-n`, a criterion listed twice,
/// an entry with empty `tests`, an invalid test id, more distinct ids than `max_tests`, and an
/// entry that is not `{"ac": "...", "tests": [...]}`. `proof` is the field as the implementer
/// returned it (`None`: missing, read as `[]`). An exempt criterion may still be mapped; its tests
/// run. `Err`: every problem, for one fix feedback.
pub fn plan(
    proof: Option<&Value>,
    acceptance_ids: &[String],
    exempt: &[Exempt],
    max_tests: usize,
) -> std::result::Result<ProofPlan, Vec<String>> {
    let mut problems = Vec::new();
    let empty = Vec::new();
    let entries = match proof {
        None | Some(Value::Null) => &empty,
        Some(Value::Array(a)) => a,
        Some(other) => {
            return Err(vec![format!(
                "`proof` is not a list (it is {}) — write `[{{\"ac\": \"AC-1\", \"tests\": [\"…\"]}}, …]`",
                kind_of(other)
            )])
        }
    };
    let mut mapped: Vec<(String, Vec<String>)> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let at = format!("proof[{i}]");
        let (Some(ac), Some(tests)) = (e.get("ac").and_then(Value::as_str), e.get("tests")) else {
            problems.push(format!(
                "{at} is not `{{\"ac\": \"AC-n\", \"tests\": [...]}}`"
            ));
            continue;
        };
        if !acceptance_ids.iter().any(|a| a == ac) {
            problems.push(format!(
                "{at}: `{}` is not an acceptance criterion of this task (they are {})",
                crate::engine::flight::shorten(ac, 40),
                acceptance_ids.join(", ")
            ));
            continue;
        }
        if mapped.iter().any(|(a, _)| a == ac) {
            problems.push(format!("{at}: `{ac}` is listed twice"));
            continue;
        }
        let Some(list) = tests.as_array() else {
            problems.push(format!("{at}: `tests` of `{ac}` is not a list of test ids"));
            continue;
        };
        if list.is_empty() {
            problems.push(format!(
                "{at}: `{ac}` has an empty `tests` — name at least one test that proves it"
            ));
            continue;
        }
        let mut ids: Vec<String> = Vec::new();
        for (j, x) in list.iter().enumerate() {
            match x.as_str() {
                None => problems.push(format!("{at}.tests[{j}] of `{ac}` is not a string")),
                Some(id) => match test_id_problem(id) {
                    Some(why) => problems.push(format!("{at} (`{ac}`): {why}")),
                    None if !ids.iter().any(|x| x == id) => ids.push(id.to_string()),
                    None => {}
                },
            }
        }
        mapped.push((ac.to_string(), ids));
    }
    let is_exempt = |ac: &str| exempt.iter().any(|e| e.ac == ac);
    let missing: Vec<&String> = acceptance_ids
        .iter()
        .filter(|a| !is_exempt(a) && !mapped.iter().any(|(m, _)| m == *a))
        .collect();
    if !missing.is_empty() {
        problems.push(format!(
            "no test proves {} — every acceptance criterion that is not under `## Proof exempt` needs at least one",
            missing
                .iter()
                .map(|a| format!("`{a}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let mut tests: Vec<String> = Vec::new();
    for (_, ids) in &mapped {
        for id in ids {
            if !tests.contains(id) {
                tests.push(id.clone());
            }
        }
    }
    if tests.len() > max_tests {
        problems.push(format!(
            "{} distinct test ids — at most {max_tests} (`red_green.max_tests`)",
            tests.len()
        ));
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    let acs = acceptance_ids
        .iter()
        .map(|a| AcProof {
            ac: a.clone(),
            exempt: is_exempt(a),
            tests: mapped
                .iter()
                .find(|(m, _)| m == a)
                .map(|(_, t)| t.clone())
                .unwrap_or_default(),
        })
        .collect();
    Ok(ProofPlan { acs, tests })
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

/// M17-A3 (X48): the red-green gate of a configuration — the one verify command with a
/// `red_green` block (load refuses a second one).
pub fn gate_of(cmds: &[crate::config::CommandSpec]) -> Option<(&str, &RedGreen)> {
    cmds.iter()
        .find_map(|c| c.red_green.as_ref().map(|rg| (c.name.as_str(), rg)))
}

/// M17-A3 (AC-3): the section of the implementer's prompt that explains the proof rule — only
/// when a red-green gate is in the verify commands. It names the gate, its per-test command, the
/// test globs and the exempt criteria; the ids and the reasons of `## Proof exempt` are the
/// owner's TASK.md (trusted text).
pub fn proof_rule(
    gate: &str,
    program: &str,
    rg: &RedGreen,
    acceptance_ids: &[String],
    exempt: &[Exempt],
) -> String {
    let argv: Vec<&str> = std::iter::once(program)
        .chain(rg.args.iter().map(String::as_str))
        .collect();
    let mut s = format!(
        "\n## Proof map (red-green gate `{gate}`)\nIn IMPLEMENTATION.json, `proof` lists for each acceptance criterion the tests that prove it: `[{{\"ac\": \"AC-1\", \"tests\": [\"<test id>\", …]}}, …]` — every criterion of TASK.md ({}) except the exempt ones, each with at least one test, and the whole map every time you hand over (not only what changed). Toryo runs each test once per side with `{}` (`{{test}}` is the test id): on your checkpoint it must pass; on the base commit with only your test files laid over it (the files matching {}) it must FAIL — a test that passes without your change proves nothing and fails the gate. So write or extend the tests that prove the criteria, and name them so that the command runs exactly them. A test id is 1–{MAX_TEST_ID} ASCII characters from `A-Z a-z 0-9 _ : . / - [ ] = , @ + #`, not starting with `-` or `/`; at most {} distinct ids.\n",
        acceptance_ids.join(", "),
        argv.join(" "),
        rg.tests
            .iter()
            .map(|g| format!("`{g}`"))
            .collect::<Vec<_>>()
            .join(", "),
        rg.max_tests()
    );
    if exempt.is_empty() {
        s.push_str("No criterion of this task is exempt.\n");
    } else {
        s.push_str("Exempt (no test required; a test you list for one still runs):\n");
        for e in exempt {
            s.push_str(&format!("- {}: {}\n", e.ac, e.reason));
        }
    }
    s
}

// ── the red commit (A5) ────────────────────────────────────────────────────────────────────

/// M17-A5: the red commit of a checkpoint — the base tree with every test file replaced by its
/// checkpoint state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedCommit {
    pub sha: String,
    /// The overlaid paths (repository-relative, `/`), sorted: added, modified, deleted.
    pub overlay: Vec<String>,
}

/// M17-A5 (AC-7, X50): build the red commit of `head` on `base` — read `base` into a **private
/// index** (`index`, a file only Toryo writes), replace every path that matches `globs` and
/// differs between `base` and `head` by its state in `head` (added and modified: the checkpoint's
/// mode and blob, byte-exact; deleted: removed), write the tree and commit it with `base` as its
/// only parent. The commit is on **no branch and no ref** (`commit-tree` writes none); Toryo's Git
/// never looks through replace refs (`GIT_NO_REPLACE_OBJECTS=1`, X30). An empty overlay is allowed
/// (a bug-fix task whose test already exists and fails on base). Rename detection is off: a moved
/// test file is its old path deleted and its new path added.
pub fn red_commit(
    g: &GitCmd,
    repo: &Loc,
    base: &str,
    head: &str,
    globs: &[regex::Regex],
    index: &Path,
    message: &str,
) -> Result<RedCommit> {
    for f in [index.to_path_buf(), index.with_extension("index.lock")] {
        if std::fs::symlink_metadata(&f).is_ok() {
            std::fs::remove_file(&f).with_context(|| format!("removing {}", f.display()))?;
        }
    }
    if let Some(d) = index.parent() {
        std::fs::create_dir_all(d)?;
    }
    let env = [("GIT_INDEX_FILE", index)];
    let git = |args: &[&str]| -> Result<Vec<u8>> {
        let o = g.output_env(repo, false, args, &env)?;
        if !o.status.success() {
            bail!(
                "git {} failed ({}): {}",
                args.join(" "),
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
        Ok(o.stdout)
    };
    // `:<old mode> <new mode> <old blob> <new blob> <status>\0<path>\0` per changed path
    let raw = git(&[
        "diff-tree",
        "-r",
        "-z",
        "--no-renames",
        "--no-ext-diff",
        "--no-textconv",
        base,
        head,
    ])?;
    let mut fields = raw.split(|b| *b == 0).filter(|f| !f.is_empty());
    let mut overlay: BTreeSet<String> = BTreeSet::new();
    // one `--index-info` entry per overlaid path; the path stays the bytes Git printed
    let mut infos: Vec<Vec<u8>> = Vec::new();
    while let (Some(meta), Some(raw_path)) = (fields.next(), fields.next()) {
        let meta = String::from_utf8_lossy(meta);
        let path = String::from_utf8_lossy(raw_path).to_string();
        let parts: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
        let [_, new_mode, _, new_blob, status] = parts[..] else {
            bail!("git diff-tree: unexpected line `{meta}`");
        };
        if !crate::engine::checks::is_test_path(globs, &path) {
            continue;
        }
        overlay.insert(path);
        let mut entry = if status.starts_with('D') {
            // mode 0 removes the entry from the index
            format!("0 {} 0\t", "0".repeat(new_blob.len())).into_bytes()
        } else {
            format!("{new_mode} {new_blob} 0\t").into_bytes()
        };
        entry.extend_from_slice(raw_path);
        infos.push(entry);
    }
    git(&["read-tree", base])?;
    if !infos.is_empty() {
        // `--index-info` reads the entries from stdin: one call, byte-exact paths
        let mut child =
            g.spawn_piped(repo, false, &["update-index", "-z", "--index-info"], &env)?;
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().context("update-index stdin")?;
            for i in &infos {
                stdin.write_all(i)?;
                stdin.write_all(&[0])?;
            }
        }
        let o = child.wait_with_output()?;
        if !o.status.success() {
            bail!(
                "git update-index --index-info failed ({}): {}",
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
    }
    let tree = String::from_utf8_lossy(&git(&["write-tree"])?)
        .trim()
        .to_string();
    let sha = String::from_utf8_lossy(&git(&["commit-tree", &tree, "-p", base, "-m", message])?)
        .trim()
        .to_string();
    let _ = std::fs::remove_file(index);
    Ok(RedCommit {
        sha,
        overlay: overlay.into_iter().collect(),
    })
}

// ── the result (A7) ────────────────────────────────────────────────────────────────────────

/// One side of one test's run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Side {
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub ms: u64,
}

/// One test of one criterion in `VERIFY_RESULT.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestResult {
    pub id: String,
    pub green: Option<Side>,
    pub red: Option<Side>,
}

/// One criterion in `VERIFY_RESULT.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcResult {
    pub ac: String,
    pub exempt: bool,
    pub tests: Vec<TestResult>,
}

/// The most overlay paths `VERIFY_RESULT.json` lists (`overlay_count` has them all).
pub const OVERLAY_SHOWN: usize = 50;

/// M17-A7 (AC-11): `red_green` of `VERIFY_RESULT.json` — relative paths only (N-12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedGreenResult {
    pub gate: String,
    pub base: String,
    pub red_commit: Option<String>,
    pub overlay: Vec<String>,
    pub overlay_count: usize,
    pub acs: Vec<AcResult>,
    /// Criteria whose every test passed green and failed red (an exempt one with tests too).
    pub proven: usize,
    pub total: usize,
    pub exempt: usize,
    pub pass: bool,
    /// Why the map was refused before any run (AC-4), when it was.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}

impl RedGreenResult {
    /// The result of `plan` before any run: every criterion with its tests, nothing run yet.
    pub fn planned(gate: &str, base: &str, p: &ProofPlan) -> Self {
        Self {
            gate: gate.to_string(),
            base: base.to_string(),
            red_commit: None,
            overlay: Vec::new(),
            overlay_count: 0,
            acs: p
                .acs
                .iter()
                .map(|a| AcResult {
                    ac: a.ac.clone(),
                    exempt: a.exempt,
                    tests: a
                        .tests
                        .iter()
                        .map(|id| TestResult {
                            id: id.clone(),
                            green: None,
                            red: None,
                        })
                        .collect(),
                })
                .collect(),
            proven: 0,
            total: p.acs.len(),
            exempt: p.acs.iter().filter(|a| a.exempt).count(),
            pass: false,
            problems: Vec::new(),
        }
    }

    /// A map refused before any run.
    pub fn refused(
        gate: &str,
        base: &str,
        acceptance_ids: &[String],
        exempt: &[Exempt],
        problems: Vec<String>,
    ) -> Self {
        Self {
            gate: gate.to_string(),
            base: base.to_string(),
            red_commit: None,
            overlay: Vec::new(),
            overlay_count: 0,
            acs: Vec::new(),
            proven: 0,
            total: acceptance_ids.len(),
            exempt: exempt.len(),
            pass: false,
            problems,
        }
    }

    /// Record one side of test `id` (every criterion that names it).
    pub fn record(&mut self, id: &str, green: bool, side: Side) {
        for a in &mut self.acs {
            for t in a.tests.iter_mut().filter(|t| t.id == id) {
                if green {
                    t.green = Some(side);
                } else {
                    t.red = Some(side);
                }
            }
        }
    }

    /// The overlay of the red commit.
    pub fn set_red(&mut self, r: &RedCommit) {
        self.red_commit = Some(r.sha.clone());
        self.overlay_count = r.overlay.len();
        self.overlay = r.overlay.iter().take(OVERLAY_SHOWN).cloned().collect();
    }

    /// Count the proven criteria (`green_ok`: a green side that passed, the output rule
    /// included, which `record` cannot see) and set `pass`.
    pub fn finish(&mut self, green_ok: &dyn Fn(&str) -> bool, pass: bool) {
        self.proven = self
            .acs
            .iter()
            .filter(|a| {
                !a.tests.is_empty()
                    && a.tests.iter().all(|t| {
                        t.green.is_some()
                            && green_ok(&t.id)
                            && t.red.is_some_and(|r| r.timed_out || r.exit != Some(0))
                    })
            })
            .count();
        self.pass = pass;
    }

    /// `{ proven, total, exempt, pass }` of `status` and `task show` (AC-11).
    pub fn summary(&self) -> Value {
        serde_json::json!({
            "proven": self.proven,
            "total": self.total,
            "exempt": self.exempt,
            "pass": self.pass,
        })
    }
}

/// M17-A7: the red-green summary of a `VERIFY_RESULT.json` value, or `None` (no red-green gate).
pub fn summary_of(vr: &Value) -> Option<Value> {
    let rg = vr.get("red_green")?;
    let r: RedGreenResult = serde_json::from_value(rg.clone()).ok()?;
    Some(r.summary())
}

/// M17-A7: `proven/total` in words for `status`, the final gate and the event.
pub fn summary_words(s: &Value) -> String {
    let n = |k: &str| s[k].as_u64().unwrap_or(0);
    format!(
        "{} of {} acceptance criteria proven red-green{}{}",
        n("proven"),
        n("total"),
        match n("exempt") {
            0 => String::new(),
            e => format!(" ({e} exempt)"),
        },
        if s["pass"] == true {
            ""
        } else {
            " — the gate did not pass"
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acs(n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("AC-{i}")).collect()
    }

    /// AC-5 / edge case 6: items are parsed; an unknown id, a duplicate, a missing reason and a
    /// bullet that is no item are refused with their line numbers, all at once.
    #[test]
    fn m17_ac05_proof_exempt_is_parsed_and_refused_with_line_numbers() {
        let ok = "# T-1\n## Acceptance criteria\n- AC-1: a\n- AC-2: b\n- AC-3: c\n\n## Proof exempt\nProse is fine.\n- AC-2: a manual check of the installer\n* AC-3 — only documentation\n```\n- AC-9: an example\n```\n## Notes\n- AC-1: not in the section\n";
        let got = parse_proof_exempt(ok, &acs(3)).unwrap();
        assert_eq!(
            got,
            vec![
                Exempt {
                    ac: "AC-2".into(),
                    reason: "a manual check of the installer".into()
                },
                Exempt {
                    ac: "AC-3".into(),
                    reason: "only documentation".into()
                },
            ]
        );
        // the section is outside `## Acceptance criteria`: its parser sees AC-1..AC-3 only
        assert_eq!(
            crate::engine::checks::parse_acceptance_ids(ok).unwrap(),
            acs(3)
        );
        assert!(
            parse_proof_exempt("# T\n## Acceptance criteria\n- AC-1: a\n", &acs(1))
                .unwrap()
                .is_empty()
        );
        let bad = "# T-1\n## Acceptance criteria\n- AC-1: a\n- AC-2: b\n## Proof exempt\n- AC-9: unknown\n- AC-1: once\n- AC-1: twice\n- AC-2:\n- AC-2\n- not an item\n";
        let e = format!("{:#}", parse_proof_exempt(bad, &acs(2)).unwrap_err());
        for want in [
            "line 6: `AC-9` is not an acceptance criterion of this task (they are AC-1, AC-2)",
            "line 8: `AC-1` is listed twice",
            "line 9: `AC-2` has no reason",
            "line 10: `AC-2` has no reason",
            "line 11: `- not an item` — an item of `## Proof exempt` is `- AC-<n>: reason`",
        ] {
            assert!(e.contains(want), "`{want}` not in {e}");
        }
        assert!(!e.contains("line 7"), "{e}");
    }

    /// Edge case 3: every refused id, and the ids that pass.
    #[test]
    fn m17_ec03_test_ids_are_inert_or_refused() {
        let long = "a".repeat(201);
        for bad in [
            "--nocapture",
            "/x",
            "a b",
            "a;b",
            "a&b",
            "a|b",
            "%PATH%",
            "$(x)",
            "a`b",
            "a\"b",
            "a\nb",
            "café",
            long.as_str(),
            "",
            "a!b",
            "a^b",
            "a<b",
            "a>b",
            "a(b",
            "a{b}",
            "a'b",
            "a\tb",
            "a\\b",
        ] {
            assert!(test_id_problem(bad).is_some(), "{bad:?} must be refused");
        }
        for good in [
            "calc::adds_two",
            "Calc.Tests.Divide[0]",
            "a=b,c@d+e#f",
            "tests/x.rs:12",
            &"a".repeat(200),
            "x-y",
        ] {
            assert_eq!(test_id_problem(good), None, "{good:?}");
        }
    }

    /// AC-4 / edge case 4: every problem of a proof map is named in one list.
    #[test]
    fn m17_ec04_every_problem_of_a_proof_map_is_named_at_once() {
        let proof = serde_json::json!([
            {"ac": "AC-99", "tests": ["t1"]},
            {"ac": "AC-1", "tests": ["t1", "t2"]},
            {"ac": "AC-1", "tests": ["t3"]},
            {"ac": "AC-2", "tests": []},
            {"ac": "AC-3", "tests": ["--nocapture", "t4", "t5"]},
            {"oops": 1},
        ]);
        let e = plan(Some(&proof), &acs(4), &[], 3).unwrap_err();
        let all = e.join("\n");
        for want in [
            "proof[0]: `AC-99` is not an acceptance criterion",
            "proof[2]: `AC-1` is listed twice",
            "proof[3]: `AC-2` has an empty `tests`",
            "proof[4] (`AC-3`): test id \"--nocapture\" starts with `-`",
            "proof[5] is not `{\"ac\": \"AC-n\", \"tests\": [...]}`",
            "no test proves `AC-2`, `AC-4`",
            "4 distinct test ids — at most 3",
        ] {
            assert!(all.contains(want), "`{want}` not in\n{all}");
        }
        assert!(plan(Some(&serde_json::json!("x")), &acs(1), &[], 3).is_err());
        assert!(plan(None, &acs(1), &[], 3).unwrap_err()[0].contains("no test proves `AC-1`"));
    }

    /// Edge cases 5, 6: one test proving two criteria is one id; an exempt criterion may be
    /// mapped (its tests run) or not.
    #[test]
    fn m17_ec05_one_test_for_two_criteria_and_exempt_criteria() {
        let exempt = vec![
            Exempt {
                ac: "AC-2".into(),
                reason: "r".into(),
            },
            Exempt {
                ac: "AC-3".into(),
                reason: "r".into(),
            },
        ];
        let proof = serde_json::json!([
            {"ac": "AC-1", "tests": ["shared"]},
            {"ac": "AC-2", "tests": ["shared", "own"]},
        ]);
        let p = plan(Some(&proof), &acs(3), &exempt, 32).unwrap();
        assert_eq!(p.tests, ["shared", "own"]);
        assert_eq!(
            p.acs
                .iter()
                .map(|a| (a.ac.as_str(), a.exempt, a.tests.len()))
                .collect::<Vec<_>>(),
            [("AC-1", false, 1), ("AC-2", true, 2), ("AC-3", true, 0)]
        );
        // one run per side of `shared` counts for AC-1 and AC-2
        let mut r = RedGreenResult::planned("g", "b", &p);
        let side = |exit| Side {
            exit: Some(exit),
            timed_out: false,
            ms: 1,
        };
        r.record("shared", true, side(0));
        r.record("own", true, side(0));
        r.record("shared", false, side(1));
        r.record("own", false, side(1));
        r.finish(&|_| true, true);
        assert_eq!((r.proven, r.total, r.exempt), (2, 3, 2));
    }
}
