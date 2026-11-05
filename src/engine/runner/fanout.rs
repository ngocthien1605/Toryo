//! M17 Part B — best-of-N implementer candidates (X55–X62): the fan-out of a task's first
//! Implementing stage. This part: the phases of a fan-out and the candidates' profiles that may
//! still run a turn (for the R51 evidence check of a drive).

use super::*;

/// The phases of a fan-out (`fanouts.phase`).
pub mod phase {
    pub const CREATING: &str = "creating";
    pub const TURNS: &str = "turns";
    pub const VERIFY: &str = "verify";
    pub const SELECT: &str = "select";
    pub const SELECTED: &str = "selected";
    pub const PROMOTED: &str = "promoted";
    pub const SINGLE: &str = "single";
}

/// M17-B1 (AC-12): the candidates' profiles that may still run a turn — while the task's first
/// Implementing stage has not fanned out yet or its candidates' turns are not over. For the R51
/// evidence check of a drive.
pub fn pending_candidate_profiles(
    store: &Store,
    t: &TaskRow,
    cfg: &Effective,
) -> Result<Vec<String>> {
    let Some(profiles) = cfg
        .repo
        .roles
        .get("implementer")
        .and_then(|r| r.candidates.clone())
    else {
        return Ok(Vec::new());
    };
    let early = matches!(
        t.state,
        St::New
            | St::Planning
            | St::WaitingPlanApproval
            | St::Implementing
            | St::Escalated
            | St::Paused
    );
    if t.fix_cycle != 0 || !early {
        return Ok(Vec::new());
    }
    let i = t.id.clone();
    let fan = store.read(move |tx| store::fanout(tx, &i))?;
    Ok(match fan {
        None => profiles,
        Some(f) if f.phase == phase::CREATING || f.phase == phase::TURNS => profiles,
        Some(_) => Vec::new(),
    })
}
