//! ProjectContextLoader (plan §7). The isolation flags (`--restricted --safe-mode`, R30)
//! stop Claude from loading CLAUDE.md, hooks and skills on its own; this puts the project
//! knowledge back **explicitly, deterministically and auditably**: every agent attempt gets
//! one assembled context whose bytes are stored as the artifact `context/<stage>-<attempt>`
//! and whose SHA-256 is recorded in `stage_attempts.context_sha256`.
//!
//! Sources, in fixed order (missing files are skipped and reported):
//!
//! 1. `%USERPROFILE%\.toryo\context\global.md` — personal rules.
//! 2. Repo `AGENTS.md`, then `CLAUDE.md` — **Claude only**: Codex reads `AGENTS.md` natively
//!    (R20), so the loader omits source 2 for Codex.
//! 3. Repo `.toryo/context/*.md`, sorted by name — project policy, **never trimmed**.
//! 4. Role template: repo `.toryo/roles/<role>.md`, else the embedded one — never trimmed.
//! 5. Task pack (run store): paths are listed; files under 8 KB are inlined. The Claude
//!    implementer cannot read the run store (R49), so for it everything is inlined.
//!
//! Sources 1–4 are read from the **repository checkout** (`tasks.repo`), never from the task
//! worktree, so an agent cannot change the instructions the next agent receives.
//!
//! Budget: 40 KB per role by default (`context.budget_kb.<role>`). Over budget, the loader
//! first turns inlined task-pack files back into paths (nothing is lost: the agent can read
//! them), then truncates global.md, CLAUDE.md and AGENTS.md, and — only when the task pack
//! must be inlined — the task pack, largest last. Every trim is reported.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use serde::Serialize;

use super::contracts;
use super::store::TaskRow;
use super::task::sha256_hex;
use crate::config::{home_dir, Effective, ProfileKind};
use crate::domain::Role;

pub const DEFAULT_BUDGET_KB: u64 = 40;
/// Task-pack files smaller than this are inlined (§7 source 5).
pub const INLINE_MAX: usize = 8 * 1024;

/// `%USERPROFILE%\.toryo\context\global.md`; `TORYO_GLOBAL_CONTEXT` overrides it (tests).
pub fn global_context_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TORYO_GLOBAL_CONTEXT") {
        return Some(PathBuf::from(p));
    }
    home_dir().map(|h| h.join(".toryo").join("context").join("global.md"))
}

/// The role of a stage label (`plan`, `implement`, `fix-N`, `review`).
pub fn stage_role(stage: &str) -> Result<Role> {
    Ok(match stage {
        "plan" => Role::Planner,
        "review" => Role::Reviewer,
        s if s == "implement" || s.starts_with("fix-") => Role::Implementer,
        other => bail!("unknown stage `{other}` (plan, implement, fix-N, review)"),
    })
}

/// What one attempt's context is assembled from.
#[derive(Debug, Clone)]
pub struct Request {
    pub role: Role,
    pub kind: ProfileKind,
    /// The repository checkout: sources 2–4.
    pub repo: PathBuf,
    /// Source 1 (None: no home directory).
    pub global: Option<PathBuf>,
    /// Source 5: (label, path) in the run store; missing files are skipped.
    pub pack: Vec<(String, PathBuf)>,
    /// Inline every task-pack file (the Claude implementer has no run-store access, R49).
    pub inline_all: bool,
    pub budget: usize,
}

/// One source as it went into the context.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Source {
    /// Source number in the §7 order (1–5).
    pub n: u8,
    pub label: String,
    pub path: String,
    pub bytes: usize,
    pub sha256: String,
    /// `inline`, `path` (listed only), `trimmed` (truncated) or `path (budget)`.
    pub mode: String,
    pub included_bytes: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Assembled {
    pub role: String,
    pub kind: String,
    pub budget: usize,
    pub sources: Vec<Source>,
    /// Sources that were looked for and not found (logged, not an error).
    pub skipped: Vec<String>,
    pub warnings: Vec<String>,
    pub sha256: String,
    #[serde(skip)]
    pub text: String,
}

#[derive(Clone)]
struct Part {
    n: u8,
    label: String,
    path: PathBuf,
    heading: String,
    body: String,
    original: usize,
    sha256: String,
    /// Lower = trimmed first; None = never trimmed.
    rank: Option<u32>,
    /// Task-pack file shown as a path only.
    path_only: bool,
    truncated: bool,
}

fn read(p: &Path) -> Option<String> {
    std::fs::read(p).ok().map(|b| String::from_utf8_lossy(&b).replace("\r\n", "\n"))
}

fn disp(p: &Path) -> String {
    p.display().to_string()
}

/// Assemble the context (no I/O besides reading the sources).
pub fn assemble(req: &Request) -> Result<Assembled> {
    let mut parts: Vec<Part> = Vec::new();
    let mut skipped = Vec::new();
    let mut add = |n: u8, label: &str, path: &Path, heading: String, rank: Option<u32>, skipped: &mut Vec<String>| match read(path) {
        Some(body) => parts.push(Part {
            n,
            label: label.to_string(),
            path: path.to_path_buf(),
            heading,
            original: body.len(),
            sha256: sha256_hex(body.as_bytes()),
            body,
            rank,
            path_only: false,
            truncated: false,
        }),
        None => skipped.push(format!("{n}: {}", disp(path))),
    };
    // trim ranks: task-pack inline → path (10+), global (100), CLAUDE.md (110), AGENTS.md (120),
    // must-inline task pack (200+)
    if let Some(g) = &req.global {
        add(1, "global.md", g, "Personal rules (global.md)".into(), Some(100), &mut skipped);
    } else {
        skipped.push("1: no home directory for global.md".into());
    }
    if req.kind == ProfileKind::Claude {
        add(2, "AGENTS.md", &req.repo.join("AGENTS.md"), "Project instructions (AGENTS.md)".into(), Some(120), &mut skipped);
        add(2, "CLAUDE.md", &req.repo.join("CLAUDE.md"), "Project instructions (CLAUDE.md)".into(), Some(110), &mut skipped);
    }
    let pol = req.repo.join(".toryo").join("context");
    let mut policy: Vec<PathBuf> = std::fs::read_dir(&pol)
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "md")).collect())
        .unwrap_or_default();
    policy.sort();
    if policy.is_empty() {
        skipped.push(format!("3: {}", disp(&pol.join("*.md"))));
    }
    for p in &policy {
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        add(3, &format!(".toryo/context/{name}"), p, format!("Project policy ({name})"), None, &mut skipped);
    }
    let role = req.role.as_str();
    let custom = req.repo.join(".toryo").join("roles").join(format!("{role}.md"));
    if custom.is_file() {
        add(4, &format!(".toryo/roles/{role}.md"), &custom, format!("Role: {role}"), None, &mut skipped);
    } else {
        let body = contracts::role(&format!("{role}.md")).replace("\r\n", "\n");
        parts.push(Part {
            n: 4,
            label: format!("embedded {role}.md"),
            path: PathBuf::from(format!("(embedded)/{role}.md")),
            heading: format!("Role: {role}"),
            original: body.len(),
            sha256: sha256_hex(body.as_bytes()),
            body,
            rank: None,
            path_only: false,
            truncated: false,
        });
    }
    let pack_start = parts.len();
    for (i, (label, path)) in req.pack.iter().enumerate() {
        let Some(body) = read(path) else {
            skipped.push(format!("5: {} ({})", label, disp(path)));
            continue;
        };
        let small = body.len() < INLINE_MAX;
        // inline → path first, larger files first; must-inline pack last, later items first
        let rank = if req.inline_all { 200 + (req.pack.len() - i) as u32 } else { 10 + i as u32 };
        parts.push(Part {
            n: 5,
            label: label.clone(),
            path: path.clone(),
            heading: format!("{label} (task pack, inlined)"),
            original: body.len(),
            sha256: sha256_hex(body.as_bytes()),
            path_only: !(small || req.inline_all),
            body,
            rank: Some(rank),
            truncated: false,
        });
    }
    let mut warnings = Vec::new();
    let mut text = render(req, &parts, pack_start);
    if text.len() > req.budget {
        let mut order: Vec<usize> = (0..parts.len()).filter(|&i| parts[i].rank.is_some() && !parts[i].path_only).collect();
        order.sort_by_key(|&i| parts[i].rank);
        for i in order {
            let over = text.len().saturating_sub(req.budget);
            if over == 0 {
                break;
            }
            let p = &mut parts[i];
            if p.n == 5 && !req.inline_all {
                p.path_only = true;
                warnings.push(format!("context budget: {} is listed as a path instead of inlined ({} bytes)", p.label, p.original));
            } else {
                let keep = floor_char(&p.body, p.body.len().saturating_sub(over + 120));
                let cut = p.body.len() - keep;
                p.body.truncate(keep);
                p.body.push_str(&format!("\n\n[… {cut} bytes trimmed by toryo: context budget {} KB]\n", req.budget / 1024));
                p.truncated = true;
                warnings.push(format!("context budget: {} truncated by {cut} of {} bytes", p.label, p.original));
            }
            text = render(req, &parts, pack_start);
        }
        if text.len() > req.budget {
            warnings.push(format!(
                "context budget: {} bytes still exceed {} KB — policy files and the role template are never trimmed",
                text.len(),
                req.budget / 1024
            ));
        }
    }
    let sources = parts
        .iter()
        .map(|p| Source {
            n: p.n,
            label: p.label.clone(),
            path: disp(&p.path),
            bytes: p.original,
            sha256: p.sha256.clone(),
            mode: match (p.path_only, p.truncated) {
                (true, _) if p.original < INLINE_MAX => "path (budget)".into(),
                (true, _) => "path".into(),
                (false, true) => "trimmed".into(),
                _ => "inline".into(),
            },
            included_bytes: if p.path_only { 0 } else { p.body.len() },
        })
        .collect();
    Ok(Assembled {
        role: role.into(),
        kind: format!("{:?}", req.kind).to_lowercase(),
        budget: req.budget,
        sources,
        skipped,
        warnings,
        sha256: sha256_hex(text.as_bytes()),
        text,
    })
}

fn floor_char(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn render(req: &Request, parts: &[Part], pack_start: usize) -> String {
    let mut s = format!(
        "<!-- toryo context for the {} ({:?}); sources in fixed order (plan §7) -->\n",
        req.role.as_str(),
        req.kind
    );
    for p in &parts[..pack_start] {
        s.push_str(&format!("\n# {}\n\n{}\n", p.heading, p.body.trim_end()));
    }
    let pack = &parts[pack_start..];
    if !pack.is_empty() {
        s.push_str("\n# Task pack\n\n");
        if req.inline_all {
            s.push_str("The run store is not accessible in this session: the task pack is inlined below.\n");
        } else {
            s.push_str("Read-only files in the run store (outside the worktree). Small files are inlined below.\n\n");
            for p in pack {
                s.push_str(&format!("- {}: {}{}\n", p.label, disp(&p.path), if p.path_only { "" } else { " (inlined below)" }));
            }
        }
        for p in pack.iter().filter(|p| !p.path_only) {
            s.push_str(&format!("\n## {}\n\n{}\n", p.heading, p.body.trim_end()));
        }
    }
    s
}

/// The task pack of a stage (source 5), in the run store; files that do not exist yet are
/// skipped by `assemble`.
pub fn task_pack(t: &TaskRow, stage: &str) -> Result<Vec<(String, PathBuf)>> {
    let a = t.artifacts();
    let f = |n: &str| (n.to_string(), a.join(n));
    Ok(match stage_role(stage)? {
        Role::Planner => vec![f("TASK.md"), f("DECISIONS.md")],
        Role::Implementer => {
            let mut v = vec![f("TASK.md"), f("PLAN.md"), f("DECISIONS.md")];
            if stage != "implement" {
                v.extend([f("VERIFY_RESULT.json"), f("REVIEW.md")]);
            }
            v
        }
        Role::Reviewer => vec![
            f("TASK.md"),
            f("PLAN.md"),
            f("DECISIONS.md"),
            f("VERIFY_RESULT.json"),
            f("DIFF-incremental.patch"),
            f("DIFF.patch"),
        ],
    })
}

/// The request for the next attempt of `stage` with `profile` — used by the engine and by
/// `toryo context show`, so the preview is exactly what the agent gets.
pub fn request(t: &TaskRow, cfg: &Effective, stage: &str, profile: &str) -> Result<Request> {
    let role = stage_role(stage)?;
    let kind = cfg.profile(profile)?.kind;
    let kb = cfg.repo.context.budget_kb.get(role.as_str()).copied().unwrap_or(DEFAULT_BUDGET_KB);
    Ok(Request {
        role,
        kind,
        repo: t.repo.clone(),
        global: global_context_path(),
        pack: task_pack(t, stage)?,
        inline_all: role == Role::Implementer && kind == ProfileKind::Claude,
        budget: (kb as usize) * 1024,
    })
}

/// The profile that runs the next attempt of `stage`.
pub fn stage_profile(t: &TaskRow, stage: &str) -> Result<String> {
    Ok(match stage_role(stage)? {
        Role::Planner => t.planner.clone(),
        Role::Reviewer => t.reviewer.clone(),
        Role::Implementer => t.current_implementer().map(str::to_string).unwrap_or_else(|| t.implementers.last().cloned().unwrap_or_default()),
    })
}

/// Where the context of an attempt is stored (run store; immutable versions arrive in M6).
pub fn artifact_path(t: &TaskRow, stage: &str, attempt: u32) -> PathBuf {
    t.artifacts().join("context").join(format!("{stage}-{attempt}.md"))
}

/// A one-line-per-source summary for logs and `context show`.
pub fn describe(a: &Assembled) -> String {
    let mut s = format!(
        "context for the {} ({}): {} bytes of {} KB budget, sha256 {}\n",
        a.role,
        a.kind,
        a.text.len(),
        a.budget / 1024,
        &a.sha256[..12]
    );
    for x in &a.sources {
        s.push_str(&format!("  [{}] {:<28} {:>7} B  {:<13} {}\n", x.n, x.label, x.bytes, x.mode, x.path));
    }
    for k in &a.skipped {
        s.push_str(&format!("  (skipped, not found) {k}\n"));
    }
    for w in &a.warnings {
        s.push_str(&format!("  !! {w}\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(repo: &Path, kind: ProfileKind, role: Role) -> Request {
        Request { role, kind, repo: repo.to_path_buf(), global: Some(repo.join("global.md")), pack: vec![], inline_all: false, budget: 40 * 1024 }
    }

    fn setup() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        std::fs::write(r.join("global.md"), "Answer in English.\n").unwrap();
        std::fs::write(r.join("AGENTS.md"), "AGENTS-RULES\n").unwrap();
        std::fs::write(r.join("CLAUDE.md"), "CLAUDE-RULES\n").unwrap();
        std::fs::create_dir_all(r.join(".toryo/context")).unwrap();
        std::fs::write(r.join(".toryo/context/20-naming.md"), "NAMING\n").unwrap();
        std::fs::write(r.join(".toryo/context/10-models.md"), "MODELS: ZZZ is local-only\n").unwrap();
        std::fs::write(r.join(".toryo/context/notes.txt"), "not md\n").unwrap();
        d
    }

    #[test]
    fn fixed_order_and_codex_omits_agents_md() {
        let d = setup();
        let c = assemble(&req(d.path(), ProfileKind::Claude, Role::Planner)).unwrap();
        let t = &c.text;
        let pos = |x: &str| t.find(x).unwrap_or_else(|| panic!("{x} missing"));
        assert!(pos("Answer in English") < pos("AGENTS-RULES"));
        assert!(pos("AGENTS-RULES") < pos("CLAUDE-RULES"));
        assert!(pos("CLAUDE-RULES") < pos("MODELS"));
        assert!(pos("MODELS") < pos("NAMING"), "policy files sorted by name");
        assert!(pos("NAMING") < pos("Role: planner"));
        assert!(!t.contains("not md"));
        let x = assemble(&req(d.path(), ProfileKind::Codex, Role::Implementer)).unwrap();
        assert!(!x.text.contains("AGENTS-RULES") && !x.text.contains("CLAUDE-RULES"), "Codex reads AGENTS.md natively (R20)");
        assert!(x.text.contains("MODELS") && x.text.contains("Role: implementer"));
        // deterministic
        assert_eq!(assemble(&req(d.path(), ProfileKind::Claude, Role::Planner)).unwrap().sha256, c.sha256);
    }

    #[test]
    fn repo_role_template_overrides_the_embedded_one() {
        let d = setup();
        std::fs::create_dir_all(d.path().join(".toryo/roles")).unwrap();
        std::fs::write(d.path().join(".toryo/roles/reviewer.md"), "CUSTOM REVIEWER\n").unwrap();
        let c = assemble(&req(d.path(), ProfileKind::Claude, Role::Reviewer)).unwrap();
        assert!(c.text.contains("CUSTOM REVIEWER"));
        assert!(c.sources.iter().any(|s| s.n == 4 && s.label == ".toryo/roles/reviewer.md"));
    }

    #[test]
    fn small_pack_files_are_inlined_large_ones_listed() {
        let d = setup();
        std::fs::write(d.path().join("TASK.md"), "TASK-BODY\n").unwrap();
        std::fs::write(d.path().join("DIFF.patch"), "x".repeat(INLINE_MAX + 1)).unwrap();
        let mut r = req(d.path(), ProfileKind::Claude, Role::Reviewer);
        r.pack = vec![("TASK.md".into(), d.path().join("TASK.md")), ("DIFF.patch".into(), d.path().join("DIFF.patch")), ("PLAN.md".into(), d.path().join("PLAN.md"))];
        let c = assemble(&r).unwrap();
        assert!(c.text.contains("TASK-BODY"));
        assert!(!c.text.contains("xxxxxxxx"));
        assert!(c.text.contains(&format!("- DIFF.patch: {}\n", d.path().join("DIFF.patch").display())));
        assert!(c.skipped.iter().any(|s| s.contains("PLAN.md")));
        // the Claude implementer gets everything inline (no run-store access)
        r.inline_all = true;
        r.role = Role::Implementer;
        let c = assemble(&r).unwrap();
        assert!(c.text.contains("xxxxxxxx") && c.text.contains("not accessible"));
    }

    #[test]
    fn budget_trims_but_never_policy_or_role() {
        let d = setup();
        std::fs::write(d.path().join("global.md"), "G".repeat(30_000)).unwrap();
        std::fs::write(d.path().join("AGENTS.md"), "A".repeat(3_000)).unwrap();
        std::fs::write(d.path().join(".toryo/context/10-models.md"), "P".repeat(12_000)).unwrap();
        std::fs::write(d.path().join("PLAN.md"), "L".repeat(6_000)).unwrap();
        let mut r = req(d.path(), ProfileKind::Claude, Role::Implementer);
        r.pack = vec![("PLAN.md".into(), d.path().join("PLAN.md"))];
        r.budget = 20 * 1024;
        let c = assemble(&r).unwrap();
        assert!(c.text.len() <= r.budget, "{}", c.text.len());
        assert!(c.text.contains(&"P".repeat(12_000)), "policy is never trimmed");
        let modes: Vec<(String, String)> = c.sources.iter().map(|s| (s.label.clone(), s.mode.clone())).collect();
        assert!(modes.contains(&("PLAN.md".into(), "path (budget)".into())), "{modes:?}");
        assert!(modes.contains(&("global.md".into(), "trimmed".into())), "{modes:?}");
        assert!(modes.contains(&("AGENTS.md".into(), "inline".into())), "AGENTS.md is trimmed after global.md: {modes:?}");
        assert_eq!(c.warnings.len(), 2, "{:?}", c.warnings);
        // a budget below policy + role: reported, policy still intact
        r.budget = 4 * 1024;
        let c = assemble(&r).unwrap();
        assert!(c.text.contains(&"P".repeat(12_000)));
        assert!(c.warnings.iter().any(|w| w.contains("never trimmed")));
    }

    #[test]
    fn stage_roles() {
        assert_eq!(stage_role("fix-2").unwrap(), Role::Implementer);
        assert_eq!(stage_role("review").unwrap(), Role::Reviewer);
        assert!(stage_role("verify").is_err());
    }
}
