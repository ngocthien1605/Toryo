//! Calculator fixture repository (embedded) and helpers to materialise it.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::config::data_dir;
use crate::git::{GitCmd, Loc};

pub const FILES: &[(&str, &str)] = &[
    (
        "README.md",
        include_str!("../../fixtures/calculator/README.md"),
    ),
    (
        "AGENTS.md",
        include_str!("../../fixtures/calculator/AGENTS.md"),
    ),
    ("TASK.md", include_str!("../../fixtures/calculator/TASK.md")),
    (
        ".gitignore",
        include_str!("../../fixtures/calculator/.gitignore"),
    ),
    (
        "src/Calculator.psm1",
        include_str!("../../fixtures/calculator/src/Calculator.psm1"),
    ),
    (
        "scripts/build.ps1",
        include_str!("../../fixtures/calculator/scripts/build.ps1"),
    ),
    (
        "scripts/test.ps1",
        include_str!("../../fixtures/calculator/scripts/test.ps1"),
    ),
];

pub const SCHEMAS: &[(&str, &str)] = &[
    (
        "plan.schema.json",
        include_str!("../../schemas/plan.schema.json"),
    ),
    (
        "review.schema.json",
        include_str!("../../schemas/review.schema.json"),
    ),
    (
        "implementation.schema.json",
        include_str!("../../schemas/implementation.schema.json"),
    ),
    (
        "probe.schema.json",
        include_str!("../../schemas/probe.schema.json"),
    ),
];

pub const ROLES: &[(&str, &str)] = &[
    (
        "planner.md",
        include_str!("../../templates/roles/planner.md"),
    ),
    (
        "implementer.md",
        include_str!("../../templates/roles/implementer.md"),
    ),
    (
        "reviewer.md",
        include_str!("../../templates/roles/reviewer.md"),
    ),
];

pub fn git_exe() -> PathBuf {
    std::env::var_os("THIEN_GIT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("git"))
}

pub fn gitcmd() -> Result<GitCmd> {
    GitCmd::new(git_exe(), data_dir().join("empty-hooks"))
}

/// Write the fixture into `dir` (must not exist or be empty) and commit it on `main`.
pub fn init_repo(dir: &Path) -> Result<String> {
    if dir.exists() && std::fs::read_dir(dir)?.next().is_some() {
        bail!("{} is not empty", dir.display());
    }
    for (rel, content) in FILES {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().expect("parent"))?;
        std::fs::write(&p, content.replace("\r\n", "\n"))?;
    }
    let g = gitcmd()?;
    let loc = Loc::Discover(dir.to_path_buf());
    g.run(&loc, false, &["init", "-q", "-b", "main"])?;
    // Repo-local identity, so an agent's `git commit` attempt can only fail because of the
    // sandbox (canary), never because the machine has no global user.email.
    g.run(&loc, false, &["config", "user.name", "Calculator Fixture"])?;
    g.run(
        &loc,
        false,
        &["config", "user.email", "fixture@local.invalid"],
    )?;
    g.run(&loc, false, &["add", "-A"])?;
    g.run(
        &loc,
        false,
        &["commit", "-q", "--no-verify", "-m", "calculator fixture"],
    )?;
    g.run_str(&loc, true, &["rev-parse", "HEAD"])
}

/// Write schemas and role templates to `dir` (for the manual dry-run).
pub fn write_contracts(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir.join("schemas"))?;
    std::fs::create_dir_all(dir.join("roles"))?;
    for (n, c) in SCHEMAS {
        std::fs::write(dir.join("schemas").join(n), c)?;
    }
    for (n, c) in ROLES {
        std::fs::write(dir.join("roles").join(n), c)?;
    }
    Ok(())
}

pub fn schema(name: &str) -> Result<&'static str> {
    SCHEMAS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
        .ok_or_else(|| anyhow::anyhow!("no schema {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_commits_on_main() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("THIEN_DATA_DIR", d.path().join("data"));
        let repo = d.path().join("calc");
        let head = init_repo(&repo).unwrap();
        assert_eq!(head.len(), 40);
        assert!(repo.join("scripts/test.ps1").exists());
        for (_, s) in SCHEMAS {
            let _: serde_json::Value = serde_json::from_str(s).unwrap();
        }
    }
}
