//! Token accounting (plan R21, R41).
//!
//! * Four **disjoint** buckets: uncached input, cache read, cache write, output.
//!   `reasoning` is an informational subset of output and is never added again.
//! * Cumulative vendor counters (Codex on a resumed thread; Claude if M0-VERIFY 4
//!   says so) are turned into deltas against a baseline; any counter going down
//!   is a reset and the new snapshot is stored as-is.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Buckets {
    pub uncached_input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    /// Subset of `output`; `None` when the vendor does not report it separately.
    pub reasoning: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Accounting {
    Exact,
    Provisional,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Weights {
    pub uncached_input: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub output: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            uncached_input: 1.0,
            cache_read: 1.0,
            cache_write: 1.0,
            output: 1.0,
        }
    }
}

impl Buckets {
    /// Tokens the model processed (weights 1.0) — the sum of the disjoint buckets only.
    pub fn total(&self) -> u64 {
        self.uncached_input + self.cache_read + self.cache_write + self.output
    }

    pub fn budget_tokens(&self, w: &Weights) -> u64 {
        (self.uncached_input as f64 * w.uncached_input
            + self.cache_read as f64 * w.cache_read
            + self.cache_write as f64 * w.cache_write
            + self.output as f64 * w.output)
            .round() as u64
    }

    pub fn add(&mut self, o: &Buckets) {
        self.uncached_input += o.uncached_input;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.output += o.output;
        self.reasoning = match (self.reasoning, o.reasoning) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
        };
    }
}

fn get(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

/// Anthropic usage object: `input_tokens` already EXCLUDES cache reads/writes.
pub fn claude_buckets(usage: &Value) -> Buckets {
    Buckets {
        uncached_input: get(usage, "input_tokens"),
        cache_read: get(usage, "cache_read_input_tokens"),
        cache_write: get(usage, "cache_creation_input_tokens"),
        output: get(usage, "output_tokens"),
        reasoning: None,
    }
}

/// How Codex exec reports subsets (M0-VERIFY 11 decides; the plan's default is `true`).
#[derive(Debug, Clone, Copy)]
pub struct CodexSubsets {
    pub cached_in_input: bool,
    pub reasoning_in_output: bool,
}

impl Default for CodexSubsets {
    fn default() -> Self {
        Self {
            cached_in_input: true,
            reasoning_in_output: true,
        }
    }
}

/// Codex / OpenAI usage: `cached_input_tokens` ⊂ `input_tokens`, `reasoning_output_tokens` ⊂ `output_tokens`.
pub fn codex_buckets(raw: &BTreeMap<String, u64>, s: CodexSubsets) -> Buckets {
    let g = |k: &str| raw.get(k).copied().unwrap_or(0);
    let input = g("input_tokens");
    let cached = g("cached_input_tokens");
    let output = g("output_tokens");
    let reasoning = raw.get("reasoning_output_tokens").copied();
    Buckets {
        uncached_input: if s.cached_in_input {
            input.saturating_sub(cached)
        } else {
            input
        },
        cache_read: cached,
        cache_write: 0,
        output: if s.reasoning_in_output {
            output
        } else {
            output + reasoning.unwrap_or(0)
        },
        reasoning,
    }
}

/// Numeric fields of a vendor usage object (the raw counters that baselines store).
pub fn raw_counters(v: &Value) -> BTreeMap<String, u64> {
    v.as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_u64().map(|n| (k.clone(), n)))
                .collect()
        })
        .unwrap_or_default()
}

/// Delta of cumulative counters against a baseline. Returns (delta, was_reset).
pub fn cumulative_delta(
    baseline: Option<&BTreeMap<String, u64>>,
    observed: &BTreeMap<String, u64>,
) -> (BTreeMap<String, u64>, bool) {
    let Some(base) = baseline else {
        return (observed.clone(), false);
    };
    let reset = observed
        .iter()
        .any(|(k, v)| base.get(k).is_some_and(|b| v < b));
    if reset {
        return (observed.clone(), true);
    }
    (
        observed
            .iter()
            .map(|(k, v)| (k.clone(), v - base.get(k).copied().unwrap_or(0)))
            .collect(),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn claude_buckets_are_disjoint() {
        let b = claude_buckets(&json!({
            "input_tokens": 10, "cache_read_input_tokens": 1000,
            "cache_creation_input_tokens": 200, "output_tokens": 50
        }));
        assert_eq!(b.total(), 1260);
        assert_eq!(b.reasoning, None);
    }

    #[test]
    fn codex_subsets_are_not_double_counted() {
        let r = raw(&[
            ("input_tokens", 1000),
            ("cached_input_tokens", 800),
            ("output_tokens", 300),
            ("reasoning_output_tokens", 200),
        ]);
        let b = codex_buckets(&r, CodexSubsets::default());
        assert_eq!(b.uncached_input, 200);
        assert_eq!(b.cache_read, 800);
        assert_eq!(b.output, 300);
        assert_eq!(b.reasoning, Some(200));
        assert_eq!(
            b.total(),
            1300,
            "= input_tokens + output_tokens, the vendor total"
        );
        // If M0 shows reasoning is NOT a subset, the mapping adds instead:
        let b2 = codex_buckets(
            &r,
            CodexSubsets {
                cached_in_input: true,
                reasoning_in_output: false,
            },
        );
        assert_eq!(b2.output, 500);
    }

    #[test]
    fn cumulative_delta_and_reset() {
        let t1 = raw(&[("input_tokens", 100), ("output_tokens", 10)]);
        let t2 = raw(&[("input_tokens", 250), ("output_tokens", 30)]);
        let (d, reset) = cumulative_delta(Some(&t1), &t2);
        assert!(!reset);
        assert_eq!(d, raw(&[("input_tokens", 150), ("output_tokens", 20)]));
        let t3 = raw(&[("input_tokens", 40), ("output_tokens", 5)]);
        let (d, reset) = cumulative_delta(Some(&t2), &t3);
        assert!(reset);
        assert_eq!(d, t3);
        assert_eq!(cumulative_delta(None, &t1).0, t1);
    }

    #[test]
    fn weights_apply_to_disjoint_buckets() {
        let b = Buckets {
            uncached_input: 100,
            cache_read: 1000,
            cache_write: 0,
            output: 10,
            reasoning: Some(5),
        };
        let w = Weights {
            cache_read: 0.1,
            ..Default::default()
        };
        assert_eq!(b.budget_tokens(&w), 210);
    }
}
