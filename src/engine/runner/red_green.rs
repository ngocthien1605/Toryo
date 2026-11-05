//! M17 Part A — the red-green proof inside a check gate (X47–X54). After the gate's own args
//! passed: every distinct test id of the proof map once on the checkpoint's verify checkout
//! (green: exit 0 within the gate's timeout, and `green_output_must_match` when set), then the red
//! commit (the base tree with the task's test files of the checkpoint, on no ref), its clean checkout
//! `<verify slot>/red` at the same boundary and under the same verify lock, every id once there (red:
//! a non-zero exit — a timeout counts, flagged), and the red checkout must still be the red commit
//! after the runs (`verify_mutated_tree`). The proof map was checked before the gate started.
//! The pure parts are `engine/redgreen.rs`.

use super::*;
use crate::engine::redgreen::{self, ProofPlan, RedGreenResult, Side};

/// What the red-green part of a verify run needs besides the checkout (M17-A4).
pub(in crate::engine) struct ProofInput {
    /// `proof` of the implementation report of the attempt that made the checkpoint, as returned
    /// (`None`: no report, or one without the field).
    pub(in crate::engine) proof: Option<Value>,
    pub(in crate::engine) exempt: Vec<redgreen::Exempt>,
    /// The verify slot of the checkpoint's checkout; the red checkout is its `red` folder.
    pub(in crate::engine) slot: verify::Slot,
}

/// What a run of the check gates produced (M17: with the red-green result).
pub(in crate::engine) struct VerifyRun {
    pub(in crate::engine) results: Vec<Value>,
    /// The fix feedback of the failing gate (wrapped as data, R61).
    pub(in crate::engine) failure: Option<String>,
    /// M17-A7: the result of the red-green gate, when it ran (or refused its map).
    pub(in crate::engine) red_green: Option<RedGreenResult>,
    /// M17-A6: a red run changed the red checkout — `verify_mutated_tree`.
    pub(in crate::engine) red_mutated: Option<String>,
}

/// The red-green part of one gate.
pub(super) struct RgRun {
    pub(super) result: RedGreenResult,
    /// The fix feedback (not wrapped yet).
    pub(super) failure: Option<String>,
    pub(super) mutated: Option<String>,
}

/// One failed run of one side, for the feedback.
struct Failed {
    id: String,
    how: String,
    tail: String,
}

/// The failures that get their output tail in the feedback (the others are named).
const FAILURES_SHOWN: usize = 5;
/// Lines of output per failure in the feedback.
const TAIL_LINES: usize = 20;

impl<'a> Engine<'a> {
    /// M17-A4: `proof` of the implementation report of the writing attempt whose checkpoint is
    /// `head` (its `git_head_after`) — hash-checked. `None` when that attempt returned no report or
    /// the report has no `proof` (the map check then names every criterion).
    pub(in crate::engine) fn proof_of(&self, t: &TaskRow, head: &str) -> Result<Option<Value>> {
        let i = self.id.clone();
        let atts = self.store.call(move |tx| store::attempts(tx, &i))?;
        let Some(a) = atts
            .iter()
            .rev()
            .find(|a| a.git_head_after.as_deref() == Some(head))
        else {
            return Ok(None);
        };
        self.proof_of_attempt(t, &a.id, "implementation")
    }

    /// M17-A4: `proof` of the newest `logical` report (`implementation`, a candidate's
    /// `implementation#c<k>`) of attempt `attempt`, hash-checked.
    pub(in crate::engine) fn proof_of_attempt(
        &self,
        t: &TaskRow,
        attempt: &str,
        logical: &str,
    ) -> Result<Option<Value>> {
        let Some(v) = crate::engine::artifacts::for_attempt(self.store, t, logical, attempt)?
        else {
            return Ok(None);
        };
        let bytes = crate::engine::artifacts::read_version(&v)?;
        let report: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Ok(report.get("proof").cloned())
    }

    /// M17-A7 (AC-11): the printed `red_green_finished` event.
    pub(super) fn red_green_finished(&self, r: &RedGreenResult) {
        self.rec.emit(EngineEvent::RedGreenFinished {
            gate: r.gate.clone(),
            proven: r.proven as u32,
            total: r.total as u32,
            pass: r.pass,
        });
    }

    /// M17-A6 (AC-6..AC-10): the per-test runs of the gate `c` after its own args passed in the
    /// verify checkout of `at` — green, then the red commit, its checkout and the red runs, then the
    /// red checkout's mutation check. `None`: a stop request ended a run (R38: the attempt is
    /// interrupted, verify runs again on resume). A program that cannot start is the error
    /// `StartFailed` (`gate_unavailable`, the caller's); a red checkout that cannot be made at the
    /// boundary is a `verify::Stop`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn red_green_run(
        &self,
        c: &CommandSpec,
        rg: &crate::config::RedGreen,
        plan: ProofPlan,
        at: &VerifyAt,
        b: Boundary,
        head: &str,
        proof: &ProofInput,
    ) -> Result<Option<RgRun>> {
        let t = self.task()?;
        let mut r = RedGreenResult::planned(&c.name, &t.base_commit, &plan);
        let program_text = expand_vars(c.program.as_deref().unwrap_or_default())?;
        let limit = xpp::command_timeout(c);
        // (validated at load; a frozen configuration is not checked again — no rule then)
        let rule = rg
            .green_output_must_match
            .as_deref()
            .and_then(|re| regex::Regex::new(re).ok());
        let acs_of = |id: &str| acs_of(&plan, id);
        // ── green, per test (AC-6) ──
        let mut green_ok: std::collections::HashSet<String> = Default::default();
        let mut bad: Vec<Failed> = Vec::new();
        let program = verify_program(&program_text, &at.co.dir);
        for id in &plan.tests {
            let run = at.spec(
                &format!("{} {id}", c.name),
                program.clone(),
                rg.args_for(id),
                limit,
            );
            let Some(res) = self.run_gate_command(run, at.stop, None, Some("green"))? else {
                return Ok(None);
            };
            r.record(id, true, side(&res));
            let matched = rule.as_ref().is_none_or(|re| re.is_match(&res.output));
            if res.ok() && matched {
                green_ok.insert(id.clone());
                continue;
            }
            let how = if res.ok() {
                format!(
                    "exited 0, but its output does not match `green_output_must_match` (`{}`) — did it run the test at all?",
                    rg.green_output_must_match.as_deref().unwrap_or_default()
                )
            } else if res.timed_out {
                format!("timed out after {}", fmt_limit(limit))
            } else {
                ended(&res)
            };
            bad.push(Failed {
                id: id.clone(),
                how,
                tail: res.tail(TAIL_LINES),
            });
        }
        if !bad.is_empty() {
            r.finish(&|id| green_ok.contains(id), false);
            self.red_green_finished(&r);
            let f = side_feedback(
                &c.name,
                "green side (your checkpoint): it must pass",
                &bad,
                &acs_of,
            );
            return Ok(Some(RgRun {
                result: r,
                failure: Some(f),
                mutated: None,
            }));
        }
        crash_point("red_green:after_green");
        // ── the red commit (AC-7): the base tree + the test files of the checkpoint, no ref ──
        let globs = crate::engine::checks::test_globs(&rg.tests).with_context(|| {
            format!(
                "gate `{}`: a `red_green.tests` glob is not valid (frozen configuration)",
                c.name
            )
        })?;
        let red = redgreen::red_commit(
            &self.g,
            &crate::git::Loc::Discover(t.repo.clone()),
            &t.base_commit,
            head,
            &globs,
            &proof.slot.base.join("red.index"),
            &format!(
                "toryo({}): red commit of checkpoint {head} (red-green gate {})",
                t.id, c.name
            ),
        )?;
        r.set_red(&red);
        crash_point("red_green:after_red_commit");
        // its clean checkout at the verify boundary, under the verify lock the stage holds
        let red_slot = verify::Slot::red(&t, &proof.slot);
        let red_co = verify::prepare_in(&self.g, &t, &self.cfg, &red.sha, b, &red_slot)?;
        let mut env = verify::env(&red_co);
        verify::set_gate_vars(&mut env, &t.id, &red.sha, &red_co, &red_co.dir);
        xpp::set_ui_language(&mut env, &BTreeMap::new());
        let red_at = VerifyAt {
            co: &red_co,
            env,
            boundary: b,
            stop: at.stop,
            redact: at.redact.clone(),
        };
        // ── red, per test (AC-8) ──
        let program = verify_program(&program_text, &red_co.dir);
        let mut passed: Vec<Failed> = Vec::new();
        for (n, id) in plan.tests.iter().enumerate() {
            let run = red_at.spec(
                &format!("{} {id}", c.name),
                program.clone(),
                rg.args_for(id),
                limit,
            );
            let Some(res) = self.run_gate_command(run, at.stop, None, Some("red"))? else {
                return Ok(None);
            };
            r.record(id, false, side(&res));
            if n == 0 {
                crash_point("red_green:after_red_run");
            }
            if res.exit == Some(0) && !res.timed_out {
                passed.push(Failed {
                    id: id.clone(),
                    how: "passes without the change — it proves nothing (exit 0)".into(),
                    tail: res.tail(TAIL_LINES),
                });
            }
        }
        // ── the red checkout is still the red commit (AC-9) ──
        let mutated = verify::mutated(&self.g, &red_co, &red.sha)?
            .map(|why| format!("{why} (the red checkout of gate `{}`)", c.name));
        let pass = passed.is_empty() && mutated.is_none();
        r.finish(&|id| green_ok.contains(id), pass);
        self.red_green_finished(&r);
        let failure = (!passed.is_empty()).then(|| {
            side_feedback(
                &c.name,
                "red side (the base commit with only your test files laid over it): it must fail",
                &passed,
                &acs_of,
            )
        });
        Ok(Some(RgRun {
            result: r,
            failure,
            mutated,
        }))
    }
}

/// The criteria a test proves: `AC-1, AC-3`.
fn acs_of(plan: &ProofPlan, id: &str) -> String {
    plan.acs
        .iter()
        .filter(|a| a.tests.iter().any(|t| t == id))
        .map(|a| a.ac.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// One side of a run, for the result.
fn side(res: &CommandResult) -> Side {
    Side {
        exit: res.exit,
        timed_out: res.timed_out,
        ms: res.duration_ms as u64,
    }
}

/// M17-A4 (AC-4): the feedback of a proof map refused before any run — every problem.
pub(super) fn map_feedback(gate: &str, problems: &[String]) -> String {
    format!(
        "- RED-GREEN FAILED: gate `{gate}` did not run — the proof map (`proof` in IMPLEMENTATION.json) is not valid:\n{}\nReturn the whole map: every acceptance criterion that is not under `## Proof exempt` with at least one test id.",
        problems
            .iter()
            .map(|p| format!("  - {p}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// M17-A6 (AC-10): the feedback of one side's failures — the gate, the side, each test with the
/// criteria it proves, how it ended and the tail of its output (the first few; the rest named).
fn side_feedback(
    gate: &str,
    side: &str,
    bad: &[Failed],
    acs_of: &dyn Fn(&str) -> String,
) -> String {
    let mut s = format!("- RED-GREEN FAILED: gate `{gate}`, {side}.\n");
    for f in bad.iter().take(FAILURES_SHOWN) {
        s.push_str(&format!(
            "  - test `{}` ({}) {}. Last output:\n```\n{}\n```\n",
            f.id,
            acs_of(&f.id),
            f.how,
            f.tail
        ));
    }
    if bad.len() > FAILURES_SHOWN {
        let rest: Vec<String> = bad[FAILURES_SHOWN..]
            .iter()
            .map(|f| format!("`{}` ({})", f.id, acs_of(&f.id)))
            .collect();
        s.push_str(&format!("  - and {}: the same\n", rest.join(", ")));
    }
    s
}
