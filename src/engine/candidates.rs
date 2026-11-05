//! M17 Part B — best-of-N implementer candidates (X55–X62): the pure parts. The first
//! Implementing stage of a task whose implementer seat lists `candidates` fans out: each
//! candidate `c<k>` implements the approved plan in its own worktree (`<worktrees_root>/<ID>.c<k>`,
//! branch `toryo-candidate/<ID>/c<k>`, both at the base commit), the turns run at the same time,
//! each is checkpointed and verified by the check gates; the winner is chosen by the gates, then by
//! one read-only compare turn of the reviewer seat, then by the deterministic score below, and
//! becomes the task's checkpoint. The engine half is `runner/fanout.rs`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::config::{Effective, MIN_CANDIDATES};

/// The stage of candidate `k`'s turns: `implement#c<k>` (one attempt numbering per candidate,
/// `UNIQUE(task_id, stage, attempt)` holds; `toryo usage` shows it as its own stage).
pub fn stage(k: u32) -> String {
    format!("implement#c{k}")
}

/// The stage of candidate `k`'s check gates: `verify#c<k>`.
pub fn verify_stage(k: u32) -> String {
    format!("verify#c{k}")
}

/// The read-only compare turn of the reviewer seat.
pub const SELECT_STAGE: &str = "select";

/// Is `stage` a candidate's (`implement#c<k>`, `verify#c<k>`)? Such an attempt is never the task's
/// anchor and never the task's writing attempt.
pub fn is_candidate_stage(stage: &str) -> bool {
    stage.contains("#c")
}

/// `c<k>`, the id the compare turn and the views use.
pub fn id(k: u32) -> String {
    format!("c{k}")
}

/// The `k` of an id `c<k>`.
pub fn k_of(id: &str) -> Option<u32> {
    id.strip_prefix('c')?.parse().ok().filter(|k| *k > 0)
}

/// `<worktrees_root>/<ID>.c<k>` — a `.` can occur in no task id, so no task's folder is named so.
pub fn worktree_path(worktrees_root: &Path, task: &str, k: u32) -> PathBuf {
    worktrees_root.join(format!("{task}.c{k}"))
}

/// `toryo-candidate/<ID>/c<k>` — `toryo/<ID>/…` would clash with the task's ref `toryo/<ID>`.
pub fn branch(task: &str, k: u32) -> String {
    format!("toryo-candidate/{task}/c{k}")
}

/// The candidates of the frozen configuration: the implementer seat's `candidates`.
pub fn configured(cfg: &Effective) -> Option<Vec<String>> {
    cfg.repo
        .roles
        .get("implementer")
        .and_then(|r| r.candidates.clone())
        .filter(|c| c.len() >= MIN_CANDIDATES)
}

/// M17-B11 (AC-24, X57): how many of `n` candidate turns the budget admits — the k-th starts only
/// while `used + (k-1) × estimate < budget` (each earlier one counted at the estimate: the
/// reservation of a turn with no telemetry). No budget: all of them. Nothing is reserved by it.
pub fn admitted(n: usize, budget: Option<i64>, used: i64, estimate: i64) -> usize {
    let Some(b) = budget else {
        return n;
    };
    (1..=n)
        .take_while(|k| used.saturating_add((*k as i64 - 1).saturating_mul(estimate.max(1))) < b)
        .count()
}

// ── the deterministic score (AC-20) ───────────────────────────────────────────────────────

/// What the score compares of one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scored {
    pub k: u32,
    /// Every check gate passed.
    pub pass: bool,
    /// Acceptance criteria proven red-green.
    pub proven: u32,
    /// Added + deleted lines, base..checkpoint (`git diff --numstat`).
    pub changed: i64,
    pub tokens: i64,
}

/// The rungs of the score, in order (X59).
pub const RUNGS: [&str; 5] = [
    "all check gates passed",
    "more acceptance criteria proven red-green",
    "fewer changed lines",
    "fewer tokens",
    "lower candidate index",
];

/// The candidates best first: (1) all check gates passed, (2) more acceptance criteria proven
/// red-green, (3) fewer changed lines (added + deleted), (4) fewer tokens, (5) the lower index —
/// so there is never a tie.
pub fn rank(mut v: Vec<Scored>) -> Vec<Scored> {
    v.sort_by(|a, b| {
        b.pass
            .cmp(&a.pass)
            .then(b.proven.cmp(&a.proven))
            .then(a.changed.cmp(&b.changed))
            .then(a.tokens.cmp(&b.tokens))
            .then(a.k.cmp(&b.k))
    });
    v
}

/// The rung on which `a` comes before `b` (index into [`RUNGS`]).
pub fn rung(a: &Scored, b: &Scored) -> usize {
    if a.pass != b.pass {
        0
    } else if a.proven != b.proven {
        1
    } else if a.changed != b.changed {
        2
    } else if a.tokens != b.tokens {
        3
    } else {
        4
    }
}

/// The score's verdict in words, for `status` and the final gate: `score: c2 before c1 — fewer
/// changed lines (12 vs 40)`; one candidate alone: `score: c1 (the only one)`.
pub fn score_reason(ranked: &[Scored]) -> String {
    match ranked {
        [] => "score: no candidate".into(),
        [one] => format!("score: {} (the only one)", id(one.k)),
        [a, b, ..] => {
            let r = rung(a, b);
            let values = match r {
                0 => format!(
                    "{} vs {}",
                    if a.pass { "passed" } else { "failed" },
                    if b.pass { "passed" } else { "failed" }
                ),
                1 => format!("{} vs {}", a.proven, b.proven),
                2 => format!("{} vs {}", a.changed, b.changed),
                3 => format!("{} vs {}", a.tokens, b.tokens),
                _ => format!("{} vs {}", id(a.k), id(b.k)),
            };
            format!(
                "score: {} before {} — {} ({values})",
                id(a.k),
                id(b.k),
                RUNGS[r]
            )
        }
    }
}

// ── the compare turn (AC-19) ──────────────────────────────────────────────────────────────

/// A valid answer of the compare turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub winner: u32,
    pub reason: String,
    pub ranking: Vec<u32>,
}

/// M17-B8 (AC-19): check the compare turn's answer against `select.schema.json` and the passing
/// candidates: `winner` is one of them, `ranking` names each of them once, `reason` is not empty.
/// `Err`: every problem, for the one correction turn.
pub fn check_answer(v: &Value, passing: &[u32]) -> Result<Answer, Vec<String>> {
    let mut problems = match crate::engine::contracts::validate("select.schema.json", v) {
        Ok(()) => Vec::new(),
        Err(e) => e,
    };
    let ids: Vec<String> = passing.iter().map(|k| id(*k)).collect();
    let winner = v["winner"].as_str().unwrap_or_default();
    let w = k_of(winner).filter(|k| passing.contains(k));
    if !winner.is_empty() && w.is_none() {
        problems.push(format!(
            "`winner` `{}` is not one of the candidates to choose from ({})",
            crate::engine::flight::shorten(winner, 40),
            ids.join(", ")
        ));
    }
    let ranking: Vec<Option<u32>> = v["ranking"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|x| x.as_str().and_then(k_of).filter(|k| passing.contains(k)))
        .collect();
    let mut sorted: Vec<u32> = ranking.iter().flatten().copied().collect();
    sorted.sort_unstable();
    let mut want = passing.to_vec();
    want.sort_unstable();
    if v["ranking"].is_array() && (ranking.iter().any(Option::is_none) || sorted != want) {
        problems.push(format!(
            "`ranking` must name each of {} exactly once, best first",
            ids.join(", ")
        ));
    }
    if v["reason"].as_str().is_some_and(|r| r.trim().is_empty()) {
        problems.push("`reason` is empty".into());
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(Answer {
        winner: w.unwrap_or_default(),
        reason: v["reason"].as_str().unwrap_or_default().trim().to_string(),
        ranking: ranking.into_iter().flatten().collect(),
    })
}

/// The compare turn's prompt: which candidates to choose from and what to return. The candidates'
/// diffs, reports and verify summaries follow it as data blocks.
pub fn compare_prompt(ids: &[String]) -> String {
    format!(
        "Compare the implementations of the candidates {} of the approved plan (PLAN.md) for the task (TASK.md). Each of them passed every check gate. Choose the one to keep: the one that meets every acceptance criterion best, with the least risk and the least unneeded change. Do not change any file. Return `winner` (one of {}), `reason` (why, in a few sentences) and `ranking` (every candidate id, best first). Each candidate's DIFF.patch (base → its checkpoint), IMPLEMENTATION.json and verify summary follow as data.\n",
        ids.join(", "),
        ids.join(", ")
    )
}

/// R22 for the compare turn: the one correction prompt.
pub fn correction_prompt(problems: &[String]) -> String {
    format!(
        "Your previous final response was rejected and was not used. Validation error: {}. Return only a corrected JSON object matching the schema.",
        problems.join("; ")
    )
}

/// `toryo diff --numstat` of base..head as one number: added + deleted lines (a binary file
/// counts 1).
pub fn changed_lines(numstat: &str) -> i64 {
    numstat
        .lines()
        .map(|l| {
            let mut f = l.split('\t');
            let (a, d) = (f.next().unwrap_or("0"), f.next().unwrap_or("0"));
            match (a.parse::<i64>(), d.parse::<i64>()) {
                (Ok(a), Ok(d)) => a + d,
                _ => 1,
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn s(k: u32, pass: bool, proven: u32, changed: i64, tokens: i64) -> Scored {
        Scored {
            k,
            pass,
            proven,
            changed,
            tokens,
        }
    }

    /// AC-20: each rung of the score decides on its own, in order — and an exact tie on every
    /// rung goes to the lower index (edge case 25).
    #[test]
    fn m17_ac20_each_rung_of_the_score() {
        let cases = [
            // (a, b, the winner, the rung)
            (s(1, false, 9, 1, 1), s(2, true, 0, 99, 99), 2, 0),
            (s(1, true, 1, 1, 1), s(2, true, 2, 99, 99), 2, 1),
            (s(1, true, 2, 50, 1), s(2, true, 2, 10, 99), 2, 2),
            (s(1, true, 2, 10, 900), s(2, true, 2, 10, 800), 2, 3),
            (s(1, true, 2, 10, 800), s(2, true, 2, 10, 800), 1, 4),
        ];
        for (a, b, winner, r) in cases {
            let ranked = rank(vec![b.clone(), a.clone()]);
            assert_eq!(ranked[0].k, winner, "{a:?} {b:?}");
            assert_eq!(rung(&ranked[0], &ranked[1]), r, "{a:?} {b:?}");
            assert!(
                score_reason(&ranked).contains(RUNGS[r]),
                "{}",
                score_reason(&ranked)
            );
        }
        assert_eq!(
            score_reason(&rank(vec![s(2, true, 2, 10, 800), s(1, true, 2, 10, 800)])),
            "score: c1 before c2 — lower candidate index (c1 vs c2)"
        );
    }

    /// AC-24: the budget admits the turns that start while under it, each earlier one counted at
    /// the estimate.
    #[test]
    fn m17_ac24_the_budget_admits_this_many_candidates() {
        assert_eq!(admitted(3, None, 1_000_000, 10), 3);
        assert_eq!(admitted(2, Some(150), 0, 100), 2);
        assert_eq!(admitted(3, Some(150), 0, 100), 2);
        assert_eq!(admitted(2, Some(100), 0, 100), 1);
        assert_eq!(admitted(2, Some(50), 60, 10), 0);
        assert_eq!(admitted(4, Some(i64::MAX), i64::MAX - 1, i64::MAX), 1);
    }

    /// AC-19, edge case 26: an unknown id, a failing candidate, a malformed answer and a bad
    /// ranking are each named; a valid answer is read.
    #[test]
    fn m17_ec26_the_compare_answer_is_checked() {
        let passing = [1, 3];
        let ok = json!({"winner": "c3", "reason": "smaller", "ranking": ["c3", "c1"]});
        assert_eq!(
            check_answer(&ok, &passing),
            Ok(Answer {
                winner: 3,
                reason: "smaller".into(),
                ranking: vec![3, 1]
            })
        );
        let unknown = json!({"winner": "c9", "reason": "r", "ranking": ["c3", "c1"]});
        assert!(check_answer(&unknown, &passing).unwrap_err()[0]
            .contains("`winner` `c9` is not one of"));
        let failing = json!({"winner": "c2", "reason": "r", "ranking": ["c1", "c3"]});
        assert!(check_answer(&failing, &passing).unwrap_err()[0].contains("`c2`"));
        let malformed = json!("c1");
        assert!(!check_answer(&malformed, &passing).unwrap_err().is_empty());
        let ranking = json!({"winner": "c1", "reason": "r", "ranking": ["c1", "c1"]});
        assert!(check_answer(&ranking, &passing).unwrap_err()[0]
            .contains("`ranking` must name each of c1, c3"));
        let extra = json!({"winner": "c1", "reason": "r", "ranking": ["c1", "c3"], "note": 1});
        assert!(
            check_answer(&extra, &passing).is_err(),
            "strict: no other key"
        );
    }

    #[test]
    fn names_and_lines() {
        assert_eq!(stage(2), "implement#c2");
        assert!(is_candidate_stage("implement#c2") && is_candidate_stage("verify#c1"));
        assert!(!is_candidate_stage("implement") && !is_candidate_stage("fix-1"));
        assert_eq!(branch("T-1", 3), "toryo-candidate/T-1/c3");
        assert_eq!(
            worktree_path(Path::new("w"), "T-1", 2),
            Path::new("w").join("T-1.c2")
        );
        assert_eq!((k_of("c2"), k_of("c0"), k_of("x2")), (Some(2), None, None));
        assert_eq!(changed_lines("3\t1\ta\n-\t-\tbin\n10\t0\tb\n"), 15);
    }
}
