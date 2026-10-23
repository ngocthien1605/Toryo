//! The `usage` table and the token budget of a task (R41; R23-32, moved as it was).

use super::*;

/// Reservation held for a turn with no telemetry (R41) when no exact turn of the stage is known.
pub const DEFAULT_UNKNOWN_RESERVATION: i64 = 200_000;

pub fn has_usage(tx: &Transaction, attempt: &str) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT COUNT(*) FROM usage WHERE stage_attempt_id = ?1",
        [attempt],
        |r| r.get::<_, i64>(0),
    )? > 0)
}

/// What one turn's usage row is recorded from (R41).
pub struct UsageIn<'a> {
    pub task: &'a str,
    pub attempt: &'a str,
    pub stage: &'a str,
    pub profile: &'a str,
    /// The model the turn ran with (C1): the role's, else the profile's; `None` when neither
    /// names one (the CLI's default).
    pub model: Option<&'a str>,
    pub billing: &'a str,
    /// The external session (Codex thread / Claude session) the counters belong to.
    pub session: Option<&'a str>,
    /// The adapter reports cumulative totals for a resumed session (Codex).
    pub cumulative: bool,
    /// Proven: a killed turn's tokens are inside the next cumulative total of its session.
    pub killed_included: bool,
    pub usage: Option<TokenUsage>,
    pub weights: Weights,
    /// `pipeline.unknown_turn_reservation_tokens`, if set.
    pub reservation: Option<u64>,
}

pub(super) fn counters(b: &Buckets) -> Value {
    serde_json::json!({"uncached_input": b.uncached_input, "cache_read": b.cache_read, "cache_write": b.cache_write, "output": b.output, "reasoning": b.reasoning})
}

pub(super) fn from_counters(v: &Value) -> Buckets {
    let g = |k: &str| v[k].as_u64().unwrap_or(0);
    Buckets {
        uncached_input: g("uncached_input"),
        cache_read: g("cache_read"),
        cache_write: g("cache_write"),
        output: g("output"),
        reasoning: v["reasoning"].as_u64(),
    }
}

/// What [`record_usage_in`] stored for a turn (R2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recorded {
    /// The `usage` row.
    pub id: i64,
    /// The buckets of the row — for a cumulative adapter the delta against the session's
    /// baseline, not the adapter's counters; `None` for a reservation.
    pub buckets: Option<Buckets>,
    pub accounting: Accounting,
    /// The tokens reserved for a turn with no telemetry.
    pub reservation: Option<i64>,
}

/// Record one turn's usage (R41) and update the task's spent / reserved tokens. Returns what
/// was stored (R2): the row, its buckets after the delta, the accounting, the reservation.
/// - nothing known → `unknown` with a reservation (config, else the largest exact turn of the
///   same stage in this task, else 200 000) — never zero;
/// - `provisional` (a killed Claude turn's streamed counts) → charged as they are;
/// - `exact` on a cumulative adapter → the delta against the session's durable baseline
///   (a counter going down is a reset); killed turns of the same session not yet absorbed make
///   it `includes_unreported`, and when the adapter is proven to include them they are
///   `reconciled_by` this row and stop counting.
pub fn record_usage_in(tx: &Transaction, u: &UsageIn) -> Result<Recorded> {
    let acc = u.usage.and_then(|x| x.buckets.map(|b| (b, x.accounting)));
    let at = now();
    match acc {
        None | Some((_, Accounting::Unknown)) => {
            let res = match u.reservation {
                Some(r) => r as i64,
                None => tx
                    .query_row(
                        "SELECT MAX(u.budget_tokens) FROM usage u JOIN stage_attempts a ON a.id = u.stage_attempt_id WHERE u.task_id = ?1 AND a.stage = ?2 AND u.accounting = 'exact'",
                        params![u.task, u.stage],
                        |r| r.get::<_, Option<i64>>(0),
                    )?
                    .filter(|m| *m > 0)
                    .unwrap_or(DEFAULT_UNKNOWN_RESERVATION),
            };
            tx.execute(
                "INSERT INTO usage (task_id, stage_attempt_id, profile, model, billing_kind, accounting, reservation_tokens, at) VALUES (?1, ?2, ?3, ?4, ?5, 'unknown', ?6, ?7)",
                params![u.task, u.attempt, u.profile, u.model, u.billing, res, at],
            )?;
            tx.execute(
                "UPDATE tasks SET reserved_tokens = reserved_tokens + ?2 WHERE id = ?1",
                params![u.task, res],
            )?;
            Ok(Recorded {
                id: tx.last_insert_rowid(),
                buckets: None,
                accounting: Accounting::Unknown,
                reservation: Some(res),
            })
        }
        Some((b, acc)) => {
            let exact = acc == Accounting::Exact;
            let mut delta = b;
            let mut includes_unreported = false;
            let mut absorb: Vec<(i64, String, i64, i64)> = Vec::new();
            if let (true, true, Some(sess)) = (exact, u.cumulative, u.session) {
                let base: Option<String> = tx
                    .query_row("SELECT counters FROM usage_baselines WHERE profile = ?1 AND external_session_id = ?2", params![u.profile, sess], |r| r.get(0))
                    .optional()?;
                if let Some(base) = base {
                    let bb = from_counters(&serde_json::from_str(&base)?);
                    let reset = b.uncached_input < bb.uncached_input
                        || b.cache_read < bb.cache_read
                        || b.cache_write < bb.cache_write
                        || b.output < bb.output;
                    if !reset {
                        delta = Buckets {
                            uncached_input: b.uncached_input - bb.uncached_input,
                            cache_read: b.cache_read - bb.cache_read,
                            cache_write: b.cache_write - bb.cache_write,
                            output: b.output - bb.output,
                            reasoning: match (b.reasoning, bb.reasoning) {
                                (Some(x), Some(y)) if x >= y => Some(x - y),
                                (x, _) => x,
                            },
                        };
                    }
                }
                tx.execute(
                    "INSERT INTO usage_baselines (profile, external_session_id, counters, updated_at) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(profile, external_session_id) DO UPDATE SET counters = excluded.counters, updated_at = excluded.updated_at",
                    params![u.profile, sess, counters(&b).to_string(), at],
                )?;
                // killed turns of this session since its last exact total (none has absorbed them)
                let mut q = tx.prepare(
                    "SELECT u.id, u.accounting, COALESCE(u.reservation_tokens, 0), COALESCE(u.budget_tokens, 0) FROM usage u JOIN stage_attempts a ON a.id = u.stage_attempt_id
                     WHERE u.task_id = ?1 AND u.profile = ?2 AND a.external_session_id = ?3 AND u.accounting IN ('unknown', 'provisional') AND u.reconciled_by IS NULL AND u.stage_attempt_id != ?4
                       AND u.id > COALESCE((SELECT MAX(u2.id) FROM usage u2 JOIN stage_attempts a2 ON a2.id = u2.stage_attempt_id
                                            WHERE u2.task_id = ?1 AND u2.profile = ?2 AND a2.external_session_id = ?3 AND u2.accounting = 'exact'), 0)",
                )?;
                absorb = q
                    .query_map(params![u.task, u.profile, sess, u.attempt], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                includes_unreported = !absorb.is_empty();
            }
            let budget = delta.budget_tokens(&u.weights) as i64;
            tx.execute(
                "INSERT INTO usage (task_id, stage_attempt_id, profile, model, billing_kind, uncached_input_tokens, cache_read_tokens, cache_write_tokens, output_tokens, reasoning_tokens, budget_tokens, accounting, includes_unreported, at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    u.task,
                    u.attempt,
                    u.profile,
                    u.model,
                    u.billing,
                    delta.uncached_input as i64,
                    delta.cache_read as i64,
                    delta.cache_write as i64,
                    delta.output as i64,
                    delta.reasoning.map(|r| r as i64),
                    budget,
                    if exact { "exact" } else { "provisional" },
                    includes_unreported as i64,
                    at
                ],
            )?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "UPDATE tasks SET spent_tokens = spent_tokens + ?2 WHERE id = ?1",
                params![u.task, budget],
            )?;
            if u.killed_included {
                for (rid, acc, res, spent) in absorb {
                    tx.execute(
                        "UPDATE usage SET reconciled_by = ?2 WHERE id = ?1",
                        params![rid, id],
                    )?;
                    let (ds, dr) = if acc == "unknown" {
                        (0, res)
                    } else {
                        (spent, 0)
                    };
                    tx.execute("UPDATE tasks SET spent_tokens = spent_tokens - ?2, reserved_tokens = reserved_tokens - ?3 WHERE id = ?1", params![u.task, ds, dr])?;
                }
            }
            Ok(Recorded {
                id,
                buckets: Some(delta),
                accounting: if exact {
                    Accounting::Exact
                } else {
                    Accounting::Provisional
                },
                reservation: None,
            })
        }
    }
}

/// Record a turn's usage with default settings (no session, weights 1.0).
pub fn record_usage(
    tx: &Transaction,
    task: &str,
    attempt: &str,
    profile: &str,
    billing: &str,
    u: Option<TokenUsage>,
) -> Result<()> {
    let stage: String = tx
        .query_row(
            "SELECT stage FROM stage_attempts WHERE id = ?1",
            [attempt],
            |r| r.get(0),
        )
        .unwrap_or_default();
    record_usage_in(
        tx,
        &UsageIn {
            task,
            attempt,
            stage: &stage,
            profile,
            model: None,
            billing,
            session: None,
            cumulative: false,
            killed_included: false,
            usage: u,
            weights: Weights::default(),
            reservation: None,
        },
    )?;
    Ok(())
}

/// `toryo budget set` (R41).
pub fn set_budget(tx: &Transaction, id: &str, tokens: Option<i64>) -> Result<()> {
    tx.execute(
        "UPDATE tasks SET budget_tokens = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, tokens, now()],
    )?;
    Ok(())
}

/// Usage rows of a task: (id, attempt, profile, accounting, budget_tokens, reservation, reconciled_by, includes_unreported).
#[allow(clippy::type_complexity)]
pub fn usage_rows(
    tx: &Transaction,
    task: &str,
) -> Result<
    Vec<(
        i64,
        String,
        String,
        String,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        bool,
    )>,
> {
    let mut q = tx.prepare("SELECT id, stage_attempt_id, profile, accounting, budget_tokens, reservation_tokens, reconciled_by, includes_unreported FROM usage WHERE task_id = ?1 ORDER BY id")?;
    let v = q
        .query_map([task], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get::<_, i64>(7)? != 0,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

/// The usage rows of a task for `toryo usage` (C1): with the attempt's stage, the recorded model
/// (NULL before M10: `None`), the four buckets and the vendor-reported cost.
pub fn usage_detail(
    tx: &Transaction,
    task: &str,
) -> Result<Vec<crate::engine::usage_report::UsageRow>> {
    let mut q = tx.prepare(
        "SELECT u.id, a.stage, u.profile, u.model, u.billing_kind, COALESCE(u.uncached_input_tokens, 0), COALESCE(u.cache_read_tokens, 0), COALESCE(u.cache_write_tokens, 0), COALESCE(u.output_tokens, 0), u.accounting, u.reservation_tokens, u.reconciled_by, u.vendor_reported_cost_usd, u.includes_unreported
         FROM usage u JOIN stage_attempts a ON a.id = u.stage_attempt_id WHERE u.task_id = ?1 ORDER BY u.id",
    )?;
    let v = q
        .query_map([task], |r| {
            let n = |i: usize| r.get::<_, i64>(i).map(|x| x.max(0) as u64);
            Ok(crate::engine::usage_report::UsageRow {
                id: r.get(0)?,
                stage: r.get(1)?,
                profile: r.get(2)?,
                model: r.get(3)?,
                billing_kind: r.get(4)?,
                buckets: Buckets {
                    uncached_input: n(5)?,
                    cache_read: n(6)?,
                    cache_write: n(7)?,
                    output: n(8)?,
                    reasoning: None,
                },
                accounting: r.get(9)?,
                reservation_tokens: r.get(10)?,
                reconciled_by: r.get(11)?,
                vendor_reported_cost_usd: r.get(12)?,
                includes_unreported: r.get::<_, i64>(13)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}
