//! The environment-gate rows (R23-32, moved as they were): deploy confirmations (X26), the last
//! deployment per lock (M11-E3) and the tasks that wait for a lock.

use super::*;

/// M13-05 (X26): a deploy the owner is asked about — the gate, the checkpoint commit and the
/// gate's `confirm` mode. Evidence like a checkpoint: a row bound to the task's frozen
/// configuration digest, never a file in the run store. `confirmed_at` empty: asked, no yes yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployAsk {
    pub gate: String,
    pub commit: String,
    pub mode: String,
}

/// M13-05: has the owner said yes to a deploy by `gate` of this task under the frozen
/// configuration `config_sha256` — of `commit` (`always`), or of any commit (`first`: `None`)?
pub fn deploy_confirmed(
    tx: &Transaction,
    task: &str,
    gate: &str,
    config_sha256: &str,
    commit: Option<&str>,
) -> Result<bool> {
    let n: i64 = tx.query_row(
        "SELECT COUNT(*) FROM deploy_confirmations WHERE task = ?1 AND gate = ?2 AND config_sha256 = ?3 AND confirmed_at IS NOT NULL AND (?4 IS NULL OR commit_sha = ?4)",
        params![task, gate, config_sha256, commit],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// M13-05: a yes to `gate` exists for this task under **another** configuration digest and
/// none under this one (the stop then says why it asks again).
pub fn deploy_confirmed_under_another_config(
    tx: &Transaction,
    task: &str,
    gate: &str,
    config_sha256: &str,
) -> Result<bool> {
    let n: i64 = tx.query_row(
        "SELECT COUNT(*) FROM deploy_confirmations WHERE task = ?1 AND gate = ?2 AND config_sha256 <> ?3 AND confirmed_at IS NOT NULL \
         AND NOT EXISTS (SELECT 1 FROM deploy_confirmations d WHERE d.task = ?1 AND d.gate = ?2 AND d.config_sha256 = ?3 AND d.confirmed_at IS NOT NULL)",
        params![task, gate, config_sha256],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// M13-05: the stop asks about `asks` (what an earlier stop asked and got no yes for is dropped).
pub fn ask_deploys(
    tx: &Transaction,
    task: &str,
    config_sha256: &str,
    asks: &[DeployAsk],
) -> Result<()> {
    tx.execute(
        "DELETE FROM deploy_confirmations WHERE task = ?1 AND confirmed_at IS NULL",
        [task],
    )?;
    for a in asks {
        tx.execute(
            "INSERT OR IGNORE INTO deploy_confirmations(task, gate, commit_sha, config_sha256, mode, asked_at, confirmed_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            params![task, a.gate, a.commit, config_sha256, a.mode, now()],
        )?;
    }
    Ok(())
}

/// M13-05: the owner's yes (a plain `toryo resume` after the stop, in the same transaction as
/// its transition): what the stop asked about is confirmed. Returns `(gate, commit)` of each.
pub fn confirm_deploys(tx: &Transaction, task: &str) -> Result<Vec<(String, String)>> {
    let asked: Vec<(String, String)> = {
        let mut st = tx.prepare(
            "SELECT gate, commit_sha FROM deploy_confirmations WHERE task = ?1 AND confirmed_at IS NULL ORDER BY gate",
        )?;
        let rows = st.query_map([task], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    tx.execute(
        "UPDATE deploy_confirmations SET confirmed_at = ?2 WHERE task = ?1 AND confirmed_at IS NULL",
        params![task, now()],
    )?;
    Ok(asked)
}

/// M11-E3: what an environment lock's environment last received — written **before** the gate
/// starts (a failed or killed sync may already have changed it) and updated at its end. Never
/// deleted: cancelling a task or `task cleanup` does not change what the environment runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Deployment {
    pub lock: String,
    pub task: String,
    pub sha: String,
    pub gate: String,
    pub machine: String,
    pub started_at: String,
    /// `None`: the gate is running, or it was interrupted (crash, stop).
    pub finished_at: Option<String>,
    /// `None` until it finished; then whether the gate passed.
    pub ok: Option<bool>,
}

/// M11-E3: record that `task` starts deploying `sha` through `gate` (one row per lock).
pub fn deployment_start(
    tx: &Transaction,
    lock: &str,
    task: &str,
    sha: &str,
    gate: &str,
    machine: &str,
) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO environment_deployments(lock, task, sha, gate, machine, started_at, finished_at, ok) VALUES(?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL)",
        params![lock, task, sha, gate, machine, now()],
    )?;
    Ok(())
}

/// M11-E3: the gate of `task` on `lock` ended; `ok: None` for an interrupted (stopped) run.
pub fn deployment_end(tx: &Transaction, lock: &str, task: &str, ok: Option<bool>) -> Result<()> {
    tx.execute(
        "UPDATE environment_deployments SET finished_at = ?3, ok = ?4 WHERE lock = ?1 AND task = ?2",
        params![lock, task, now(), ok],
    )?;
    Ok(())
}

/// M11-E3: every lock's last deployment, by lock name.
pub fn deployments(tx: &Transaction) -> Result<Vec<Deployment>> {
    let mut st = tx.prepare(
        "SELECT lock, task, sha, gate, machine, started_at, finished_at, ok FROM environment_deployments ORDER BY lock",
    )?;
    let rows = st.query_map([], |r| {
        Ok(Deployment {
            lock: r.get(0)?,
            task: r.get(1)?,
            sha: r.get(2)?,
            gate: r.get(3)?,
            machine: r.get(4)?,
            started_at: r.get(5)?,
            finished_at: r.get(6)?,
            ok: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// M11-E3: `task` waits for `lock` (shown by `status`), or no longer waits (`lock: None`).
pub fn set_env_wait(tx: &Transaction, task: &str, lock: Option<&str>) -> Result<()> {
    match lock {
        Some(l) => tx.execute(
            "INSERT OR REPLACE INTO environment_waits(task, lock, since) VALUES(?1, ?2, ?3)",
            params![task, l, now()],
        )?,
        None => tx.execute("DELETE FROM environment_waits WHERE task = ?1", [task])?,
    };
    Ok(())
}

/// M11-E3: the lock `task` waits for, if any.
pub fn env_wait(tx: &Transaction, task: &str) -> Result<Option<String>> {
    Ok(tx
        .query_row(
            "SELECT lock FROM environment_waits WHERE task = ?1",
            [task],
            |r| r.get(0),
        )
        .optional()?)
}
