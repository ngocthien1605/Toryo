//! `DECISIONS.md` in the run store: every human decision about a task, in order — gate
//! approvals and rejections with the subject digest, notes, accepted protected changes,
//! configuration reloads, and the non-blocking review issues left as follow-ups (R53).
//! Written only by the orchestrator; agents get it in their task pack (§7 source 5).

use std::io::Write;

use anyhow::Result;

use super::store::TaskRow;

pub fn path(t: &TaskRow) -> std::path::PathBuf {
    t.artifacts().join("DECISIONS.md")
}

/// Append one decision: `## <time> — <kind>` + body.
pub fn append(t: &TaskRow, kind: &str, body: &str) -> Result<()> {
    let p = path(t);
    let new = !p.exists();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p)?;
    if new {
        writeln!(f, "# Decisions — {}\n\nHuman decisions for this task, oldest first (written by toryo).", t.id)?;
    }
    writeln!(f, "\n## {} — {kind}\n\n{}", crate::evidence::now_rfc3339(), body.trim_end())?;
    Ok(())
}
