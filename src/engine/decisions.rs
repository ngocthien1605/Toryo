//! `DECISIONS.md` in the run store: every human decision about a task, in order — gate
//! approvals and rejections with the subject digest, notes, accepted protected changes,
//! configuration reloads, and the non-blocking review issues left as follow-ups (R53).
//! Written only by the orchestrator, as immutable versions (R34, logical `decisions`);
//! agents get it in their task pack (§7 source 5).

use anyhow::Result;

use super::store::{Store, TaskRow};

/// Append one decision: `## <time> — <kind>` + body.
pub fn append(store: &Store, t: &TaskRow, kind: &str, body: &str) -> Result<()> {
    let head = super::artifacts::head(store, t, "decisions")?;
    let mut text = String::new();
    if head.is_none() {
        text.push_str(&format!("# Decisions — {}\n\nHuman decisions for this task, oldest first (written by toryo).\n", t.id));
    }
    text.push_str(&format!("\n## {} — {kind}\n\n{}\n", crate::evidence::now_rfc3339(), body.trim_end()));
    super::artifacts::append(store, t, "decisions", "md", "decisions", &text, "DECISIONS.md")?;
    Ok(())
}
