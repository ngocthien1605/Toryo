//! The token budget of a task (R41; R23-27, moved as it was): billing, weights, admission, the live
//! interrupt and the usage of a turn.

use super::*;

impl<'a> Engine<'a> {
    pub(super) fn billing(&self, profile: &str) -> &'static str {
        self.cfg.billing_of(profile)
    }

    pub(super) fn weights(&self) -> Weights {
        let m = self
            .cfg
            .repo
            .pipeline
            .budget_weights
            .clone()
            .unwrap_or_default();
        let g = |k: &str| m.get(k).copied().unwrap_or(1.0);
        Weights {
            uncached_input: g("uncached_input"),
            cache_read: g("cache_read"),
            cache_write: g("cache_write"),
            output: g("output"),
        }
    }

    /// R41 admission: a turn starts only while `spent + reserved < budget`; otherwise the
    /// task pauses (`Paused(Budget)`) without starting it. Warns at 80 %.
    pub(super) fn admitted(&self) -> Result<bool> {
        let t = self.task()?;
        let Some(b) = t.budget_tokens else {
            return Ok(true);
        };
        let used = t.spent_tokens + t.reserved_tokens;
        if used >= b {
            crate::errln!(
                "!! admission cap reached: {used} tokens used ({} spent + ~{} reserved) of {b} — paused before the turn (`toryo budget set {} --tokens N`, then `toryo resume {}`)",
                t.spent_tokens, t.reserved_tokens, t.id, t.id
            );
            self.apply(Ev::BudgetExceeded)?;
            return Ok(false);
        }
        // (R23-11: saturating — a cap near i64::MAX must not wrap into a warning every turn)
        if used.saturating_mul(10) >= b.saturating_mul(8) {
            self.rec.emit(EngineEvent::BudgetWarning { used, cap: b });
        }
        Ok(true)
    }

    /// The live interrupt fired: the turn's provisional usage is recorded, the attempt is
    /// interrupted and the task pauses.
    pub(super) fn budget_interrupted(&self, a: &AttemptRow, r: &StageOutcome) -> Result<Flow> {
        crate::errln!(
            "!! budget reached during the turn — interrupted (R41 live interrupt), the task pauses"
        );
        let end = AttemptEnd {
            error_class: Some("budget".into()),
            error_detail: Some("budget reached during the turn (R41 live interrupt)".into()),
            ..Default::default()
        };
        self.close(
            a,
            Close::of_turn(AttemptStatus::Interrupted, r, end)
                .then(Some(Ev::BudgetExceeded), None)
                .quiet(),
        )?;
        Ok(Flow::Next)
    }

    /// Record the turn's usage (R41): cumulative adapters store the delta against the
    /// session baseline; a turn that did not complete counts its streamed usage as provisional.
    /// `requested`: the model the engine asked the adapter for; the row records the one the turn
    /// ran with (C1).
    pub(super) fn usage(
        &self,
        a: &AttemptRow,
        profile: &str,
        r: &StageOutcome,
        requested: Option<&str>,
    ) -> Result<()> {
        let caps = self.hub.capabilities(profile)?;
        let model = self
            .cfg
            .profile(profile)
            .ok()
            .and_then(|p| crate::engine::usage_report::effective_model(requested, p));
        let mut u = r.usage;
        if let Some(x) = &mut u {
            if !r.success && (r.cancelled || r.timed_out) && x.accounting == Accounting::Exact {
                x.accounting = Accounting::Provisional;
            }
        }
        let reservation = self
            .cfg
            .repo
            .pipeline
            .unknown_turn_reservation_tokens
            .as_ref()
            .and_then(|v| v.as_u64());
        let (i, aid, stage, p, b, sess, w) = (
            self.id.clone(),
            a.id.clone(),
            a.stage.clone(),
            profile.to_string(),
            self.billing(profile),
            r.session_id.clone(),
            self.weights(),
        );
        let stored = self.store.call(move |tx| {
            store::record_usage_in(
                tx,
                &store::UsageIn {
                    task: &i,
                    attempt: &aid,
                    stage: &stage,
                    profile: &p,
                    model: model.as_deref(),
                    billing: b,
                    session: sess.as_deref(),
                    cumulative: caps.cumulative_on_resume == Tri::Yes,
                    killed_included: caps.killed_turn_in_resume == Tri::Yes,
                    usage: u,
                    weights: w,
                    reservation,
                },
            )
        })?;
        // R2: what the store recorded (a cumulative adapter's delta, the accounting after a
        // cut-short turn), never the adapter's raw counters
        self.rec.emit_quiet(EngineEvent::TurnFinished {
            stage: a.stage.clone(),
            attempt: a.attempt,
            profile: profile.to_string(),
            success: r.success,
            usage: stored.buckets.map(|buckets| TurnUsage {
                buckets,
                accounting: stored.accounting,
            }),
            reserved_tokens: stored.reservation,
            duration_ms: r.duration_ms as u64,
        });
        Ok(())
    }
}
