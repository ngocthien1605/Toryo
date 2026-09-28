//! Calculator fixture repository (embedded) and helpers to materialise it.

use std::path::Path;

use anyhow::{bail, Result};

use crate::git::Loc;

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

/// Sample app for the pipeline walkthrough (docs/HUONG-DAN.md): a small PowerShell module.
pub const BATCH_RENAME: &[(&str, &str)] = &[
    (
        ".gitignore",
        include_str!("../../examples/batch-rename/.gitignore"),
    ),
    (
        "AGENTS.md",
        include_str!("../../examples/batch-rename/AGENTS.md"),
    ),
    (
        "README.md",
        include_str!("../../examples/batch-rename/README.md"),
    ),
    (
        "scripts/build.ps1",
        include_str!("../../examples/batch-rename/scripts/build.ps1"),
    ),
    (
        "scripts/test.ps1",
        include_str!("../../examples/batch-rename/scripts/test.ps1"),
    ),
    (
        "src/BatchRename.psm1",
        include_str!("../../examples/batch-rename/src/BatchRename.psm1"),
    ),
    (
        "tasks/BR-001.md",
        include_str!("../../examples/batch-rename/tasks/BR-001.md"),
    ),
    (
        "tasks/BR-002.md",
        include_str!("../../examples/batch-rename/tasks/BR-002.md"),
    ),
    (
        "tests/SafeFileName.Tests.ps1",
        include_str!("../../examples/batch-rename/tests/SafeFileName.Tests.ps1"),
    ),
];

pub use crate::engine::contracts::{schema, ROLES, SCHEMAS};
pub use crate::git::{default_gitcmd as gitcmd, git_exe};

/// Write the calculator fixture into `dir` (must not exist or be empty) and commit it on `main`.
pub fn init_repo(dir: &Path) -> Result<String> {
    init_repo_with(dir, FILES, "Calculator Fixture", "calculator fixture")
}

/// Write `files` into `dir` (must not exist or be empty), `git init -b main`, set a
/// repo-local identity and make the first commit.
pub fn init_repo_with(
    dir: &Path,
    files: &[(&str, &str)],
    who: &str,
    message: &str,
) -> Result<String> {
    if dir.exists() && std::fs::read_dir(dir)?.next().is_some() {
        bail!("{} is not empty", dir.display());
    }
    for (rel, content) in files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().expect("parent"))?;
        std::fs::write(&p, content.replace("\r\n", "\n"))?;
    }
    let g = gitcmd()?;
    let loc = Loc::Discover(dir.to_path_buf());
    g.run(&loc, false, &["init", "-q", "-b", "main"])?;
    // Repo-local identity, so an agent's `git commit` attempt can only fail because of the
    // sandbox (canary), never because the machine has no global user.email.
    g.run(&loc, false, &["config", "user.name", who])?;
    g.run(
        &loc,
        false,
        &["config", "user.email", "fixture@local.invalid"],
    )?;
    g.run(&loc, false, &["add", "-A"])?;
    g.run(&loc, false, &["commit", "-q", "--no-verify", "-m", message])?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_commits_on_main() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("TORYO_DATA_DIR", d.path().join("data"));
        let repo = d.path().join("calc");
        let head = init_repo(&repo).unwrap();
        assert_eq!(head.len(), 40);
        assert!(repo.join("scripts/test.ps1").exists());
        for (_, s) in SCHEMAS {
            let _: serde_json::Value = serde_json::from_str(s).unwrap();
        }
    }
}
