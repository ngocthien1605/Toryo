//! The artifact registry (R34; R23-32, moved as it was).

use super::*;

#[allow(clippy::too_many_arguments)]
pub fn register_artifact(
    tx: &Transaction,
    task: &str,
    attempt: Option<&str>,
    logical: &str,
    kind: &str,
    path: &str,
    sha256: &str,
    size: u64,
) -> Result<String> {
    let v: i64 = tx.query_row(
        "SELECT COALESCE(MAX(version), 0) + 1 FROM artifacts WHERE task_id = ?1 AND logical_name = ?2",
        params![task, logical],
        |r| r.get(0),
    )?;
    let id = format!("{task}/{logical}/{v}");
    tx.execute(
        "INSERT INTO artifacts (id, task_id, stage_attempt_id, logical_name, version, kind, path, sha256, size_bytes, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![id, task, attempt, logical, v, kind, path, sha256, size as i64, now()],
    )?;
    let prev: Option<String> = tx
        .query_row(
            "SELECT artifact_id FROM artifact_heads WHERE task_id = ?1 AND logical_name = ?2",
            params![task, logical],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(p) = prev {
        tx.execute(
            "UPDATE artifacts SET superseded_by = ?2 WHERE id = ?1",
            params![p, id],
        )?;
    }
    tx.execute(
        "INSERT INTO artifact_heads (task_id, logical_name, artifact_id) VALUES (?1, ?2, ?3)
         ON CONFLICT(task_id, logical_name) DO UPDATE SET artifact_id = excluded.artifact_id",
        params![task, logical, id],
    )?;
    Ok(id)
}

/// (id, path, sha256) of an artifact version.
pub fn artifact(tx: &Transaction, id: &str) -> Result<(String, String, String)> {
    Ok(tx.query_row(
        "SELECT id, path, sha256 FROM artifacts WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}
