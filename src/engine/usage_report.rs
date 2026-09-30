//! `toryo usage` (plan §12, M10-C1): the tokens of a task per stage × profile × model in the
//! four buckets, with the `exact`, `provisional` and `~unknown` (reservation) totals kept
//! apart, and an estimated cost from the machine's `prices.toml` for the rows billed `foundry`
//! or `api` (subscription rows have no marginal cost). Everything here is pure; the CLI reads
//! the rows and the price table.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::config::{Effective, Profile, ProfileKind};
use crate::usage::Buckets;

/// USD per million tokens, per bucket.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub uncached_input: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub output: f64,
}

/// `prices.toml`: facts about the owner's contracts (a machine file, like R60).
#[derive(Debug, Clone, PartialEq)]
pub struct Prices {
    /// Recorded with every estimate.
    pub version: String,
    pub safety_factor: f64,
    /// By model or Foundry deployment name, as the usage rows record it.
    pub models: BTreeMap<String, Price>,
}

const BUCKETS: [&str; 4] = ["uncached_input", "cache_read", "cache_write", "output"];

/// A non-negative finite number (integer or float) at `key`.
fn number(v: &toml::Value, key: &str) -> Result<f64> {
    let x = match v {
        toml::Value::Integer(i) => *i as f64,
        toml::Value::Float(f) => *f,
        _ => bail!("`{key}` must be a number"),
    };
    if !x.is_finite() {
        bail!("`{key}` must be a finite number");
    }
    if x < 0.0 {
        bail!("`{key}` is negative ({x}): prices and factors are never negative");
    }
    Ok(x)
}

/// Parse `prices.toml`. Unknown keys and negative prices are errors naming the key.
pub fn parse_prices(text: &str) -> Result<Prices> {
    let top: toml::Table = text.parse().map_err(|e| anyhow!("{e}"))?;
    for k in top.keys() {
        if !["version", "safety_factor", "models"].contains(&k.as_str()) {
            bail!("unknown key `{k}` (expected `version`, `safety_factor`, `models`)");
        }
    }
    let version = match top.get("version") {
        Some(toml::Value::String(s)) if !s.trim().is_empty() => s.clone(),
        Some(_) => bail!("`version` must be a non-empty string"),
        None => bail!("`version` is missing: it is recorded with every estimate"),
    };
    let safety_factor = match top.get("safety_factor") {
        Some(v) => number(v, "safety_factor")?,
        None => 1.0,
    };
    if safety_factor == 0.0 {
        bail!("`safety_factor` is 0: every estimate would be $0");
    }
    let mut models = BTreeMap::new();
    if let Some(m) = top.get("models") {
        let m = m.as_table().context("`models` must be a table")?;
        for (name, p) in m {
            let at = |k: &str| format!("models.\"{name}\".{k}");
            let p = p
                .as_table()
                .with_context(|| format!("`models.\"{name}\"` must be a table"))?;
            for k in p.keys() {
                if !BUCKETS.contains(&k.as_str()) {
                    bail!(
                        "unknown key `{}` (the buckets are {})",
                        at(k),
                        BUCKETS.join(", ")
                    );
                }
            }
            let get = |k: &str| -> Result<f64> {
                let v = p
                    .get(k)
                    .with_context(|| format!("`{}` is missing (USD per million tokens)", at(k)))?;
                number(v, &at(k))
            };
            models.insert(
                name.clone(),
                Price {
                    uncached_input: get("uncached_input")?,
                    cache_read: get("cache_read")?,
                    cache_write: get("cache_write")?,
                    output: get("output")?,
                },
            );
        }
    }
    Ok(Prices {
        version,
        safety_factor,
        models,
    })
}

/// Read the price table at `path`; `None` when there is no file (no estimates, not an error).
pub fn load_prices(path: &Path) -> Result<Option<Prices>> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_prices(&text)
            .map(Some)
            .with_context(|| format!("{}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The default price table: `%USERPROFILE%\.toryo\prices.toml`, next to the machine config.
pub fn default_prices_path() -> Option<std::path::PathBuf> {
    crate::config::home_dir().map(|h| h.join(".toryo").join("prices.toml"))
}

/// The role that runs a stage: `plan` → planner, `implement`/`fix-N` → implementer, `review` →
/// reviewer.
pub fn stage_role(stage: &str) -> Option<&'static str> {
    match stage {
        "plan" => Some("planner"),
        "review" => Some("reviewer"),
        s if s == "implement" || s.starts_with("fix-") => Some("implementer"),
        _ => None,
    }
}

/// The model a turn runs with: the one the engine asks for (the role's), else the profile's,
/// else — for Codex — a `model` in `codex.overrides`. For a Codex profile with a
/// `codex.provider` block this is the Foundry deployment name (`-m`).
pub fn effective_model(requested: Option<&str>, p: &Profile) -> Option<String> {
    requested
        .map(str::to_string)
        .or_else(|| p.model.clone())
        .or_else(|| {
            p.codex
                .as_ref()
                .and_then(|c| c.overrides.get("model"))
                .map(|m| m.trim().trim_matches('"').to_string())
        })
        .filter(|m| !m.is_empty())
}

/// The model the engine asks for in `stage` with `profile` (the role's model; never for a Claude
/// implementer, which runs with its profile's model), resolved like [`effective_model`]. Used for
/// rows written before M10 (`usage.model` NULL) and for a killed turn's reservation.
pub fn configured_model(cfg: &Effective, stage: &str, profile: &str) -> Option<String> {
    let p = cfg.profile(profile).ok()?;
    let role = stage_role(stage);
    let requested = match role {
        Some("implementer") if p.kind == ProfileKind::Claude => None,
        Some(r) => cfg.repo.roles.get(r).and_then(|x| x.model.clone()),
        None => None,
    };
    effective_model(requested.as_deref(), p)
}

/// The name shown for a model nobody recorded or configured.
pub const UNKNOWN_MODEL: &str = "unknown model";

/// One usage row, as `toryo usage` reads it (the store's `usage` joined with the attempt's
/// stage).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageRow {
    pub id: i64,
    pub stage: String,
    pub profile: String,
    /// Resolved by the caller (stored, else configured, else [`UNKNOWN_MODEL`]).
    pub model: Option<String>,
    pub billing_kind: String,
    pub buckets: Buckets,
    /// `exact`, `provisional` or `unknown`.
    pub accounting: String,
    pub reservation_tokens: Option<i64>,
    /// A later exact row absorbed this one (R41): it no longer counts.
    pub reconciled_by: Option<i64>,
    pub vendor_reported_cost_usd: Option<f64>,
    /// The exact row also holds killed turns' tokens of its session (R41).
    pub includes_unreported: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cost {
    /// Subscription: no marginal cost (`—`).
    Subscription,
    /// Billed per token, but the model is not in the price table.
    NoPrice,
    /// No price table: no estimate.
    NoTable,
    /// Estimated, × the safety factor; `with_reservations`: includes reserved tokens (priced as
    /// uncached input) — `~est.`.
    Est { usd: f64, with_reservations: bool },
}

impl Cost {
    pub fn label(&self) -> String {
        match self {
            Cost::Subscription => "—".into(),
            Cost::NoPrice => "no price".into(),
            Cost::NoTable => "no price table".into(),
            Cost::Est {
                usd,
                with_reservations,
            } => format!(
                "{}est. ${usd:.4}",
                if *with_reservations { "~" } else { "" }
            ),
        }
    }
    fn usd(&self) -> Option<f64> {
        match self {
            Cost::Est { usd, .. } => Some(*usd),
            _ => None,
        }
    }
}

/// The tokens and cost of one stage × profile × model.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub stage: String,
    pub profile: String,
    pub model: String,
    pub billing_kind: String,
    /// The counted rows' buckets (exact + provisional).
    pub buckets: Buckets,
    pub exact: u64,
    pub provisional: u64,
    /// Reservations of turns with no telemetry (`~`).
    pub reserved: u64,
    /// Usage rows (turns) counted.
    pub turns: u32,
    /// Rows absorbed by a later exact row: shown, not counted.
    pub reconciled: u32,
    /// A counted row includes killed turns' tokens (`includes_unreported`): the estimate is
    /// approximate (`~est.`).
    pub includes_unreported: bool,
    pub vendor_cost_usd: Option<f64>,
    pub cost: Cost,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub groups: Vec<Group>,
    pub exact: u64,
    pub provisional: u64,
    pub reserved: u64,
    /// The sum of the estimates (`None`: nothing was estimated).
    pub est_usd: Option<f64>,
    pub est_with_reservations: bool,
    /// `false`: some groups billed per token have no price, so `est_usd` leaves them out;
    /// `None` when nothing was estimated (R3).
    pub est_complete: Option<bool>,
    /// How many groups billed per token have no price.
    pub unpriced_groups: usize,
    pub vendor_cost_usd: Option<f64>,
    /// (model, profile) of the rows billed per token whose model has no price, each once.
    pub no_price: Vec<(String, String)>,
    pub price_version: Option<String>,
    pub safety_factor: Option<f64>,
}

fn per_token(billing: &str) -> bool {
    matches!(billing, "foundry" | "api")
}

fn priced(b: &Buckets, p: &Price) -> f64 {
    (b.uncached_input as f64 * p.uncached_input
        + b.cache_read as f64 * p.cache_read
        + b.cache_write as f64 * p.cache_write
        + b.output as f64 * p.output)
        / 1_000_000.0
}

/// The report of a task's usage rows (in row order; groups in order of first appearance).
pub fn report(rows: &[UsageRow], prices: Option<&Prices>) -> Report {
    let mut groups: Vec<Group> = Vec::new();
    for r in rows {
        let model = r.model.clone().unwrap_or_else(|| UNKNOWN_MODEL.into());
        // R3: a group never mixes billing kinds (a profile's `billing:` can change on reload)
        let i = match groups.iter().position(|g| {
            g.stage == r.stage
                && g.profile == r.profile
                && g.model == model
                && g.billing_kind == r.billing_kind
        }) {
            Some(i) => i,
            None => {
                groups.push(Group {
                    stage: r.stage.clone(),
                    profile: r.profile.clone(),
                    model,
                    billing_kind: r.billing_kind.clone(),
                    buckets: Buckets::default(),
                    exact: 0,
                    provisional: 0,
                    reserved: 0,
                    turns: 0,
                    reconciled: 0,
                    includes_unreported: false,
                    vendor_cost_usd: None,
                    cost: Cost::Subscription,
                });
                groups.len() - 1
            }
        };
        let g = &mut groups[i];
        if r.reconciled_by.is_some() {
            g.reconciled += 1;
            continue;
        }
        g.turns += 1;
        g.includes_unreported |= r.includes_unreported;
        if let Some(v) = r.vendor_reported_cost_usd {
            g.vendor_cost_usd = Some(g.vendor_cost_usd.unwrap_or(0.0) + v);
        }
        match r.accounting.as_str() {
            "exact" | "provisional" => {
                g.buckets.add(&Buckets {
                    reasoning: None,
                    ..r.buckets
                });
                let n = r.buckets.total();
                if r.accounting == "exact" {
                    g.exact += n;
                } else {
                    g.provisional += n;
                }
            }
            _ => g.reserved += r.reservation_tokens.unwrap_or(0).max(0) as u64,
        }
    }
    let mut no_price = Vec::new();
    for g in &mut groups {
        g.cost = if !per_token(&g.billing_kind) {
            Cost::Subscription
        } else {
            match prices {
                None => Cost::NoTable,
                Some(p) => match p.models.get(&g.model) {
                    None => {
                        // each (model, profile) once, in order of first appearance
                        let key = (g.model.clone(), g.profile.clone());
                        if !no_price.contains(&key) {
                            no_price.push(key);
                        }
                        Cost::NoPrice
                    }
                    Some(price) => {
                        let reserved = Buckets {
                            uncached_input: g.reserved,
                            ..Buckets::default()
                        };
                        Cost::Est {
                            usd: (priced(&g.buckets, price) + priced(&reserved, price))
                                * p.safety_factor,
                            with_reservations: g.reserved > 0 || g.includes_unreported,
                        }
                    }
                },
            }
        };
    }
    let unpriced_groups = groups.iter().filter(|g| g.cost == Cost::NoPrice).count();
    let estimated: Vec<&Group> = groups.iter().filter(|g| g.cost.usd().is_some()).collect();
    let vendor: Vec<f64> = groups.iter().filter_map(|g| g.vendor_cost_usd).collect();
    Report {
        exact: groups.iter().map(|g| g.exact).sum(),
        provisional: groups.iter().map(|g| g.provisional).sum(),
        reserved: groups.iter().map(|g| g.reserved).sum(),
        est_usd: (!estimated.is_empty())
            .then(|| estimated.iter().filter_map(|g| g.cost.usd()).sum()),
        est_with_reservations: estimated.iter().any(|g| {
            matches!(
                g.cost,
                Cost::Est {
                    with_reservations: true,
                    ..
                }
            )
        }),
        est_complete: (!estimated.is_empty()).then_some(unpriced_groups == 0),
        unpriced_groups,
        vendor_cost_usd: (!vendor.is_empty()).then(|| vendor.iter().sum()),
        no_price,
        price_version: prices.map(|p| p.version.clone()),
        safety_factor: prices.map(|p| p.safety_factor),
        groups,
    }
}

fn tilde(n: u64) -> String {
    if n == 0 {
        "0".into()
    } else {
        format!("~{n}")
    }
}

fn vendor(v: Option<f64>) -> String {
    v.map(|x| format!("  (vendor ${x:.4})")).unwrap_or_default()
}

/// The text of `toryo usage`. `table`: where the price table was looked for.
pub fn render_text(r: &Report, task: &str, table: Option<&Path>) -> String {
    let mut s = format!(
        "usage of {task} (plan §12) — tokens per stage × profile × model; exact | provisional | ~unknown are kept apart\nexact and provisional are raw token counts (the four buckets); ~unknown are reservations for turns with no telemetry, in budget tokens (weighted by pipeline.budget_weights), priced as uncached input\n\n{:<11} {:<16} {:<22} {:>10} {:>10} {:>10} {:>9} {:>10} {:>9} {:>10}  cost\n",
        "stage", "profile", "model", "uncached", "cache rd", "cache wr", "output", "exact", "provis.", "~unknown"
    );
    for g in &r.groups {
        s.push_str(&format!(
            "{:<11} {:<16} {:<22} {:>10} {:>10} {:>10} {:>9} {:>10} {:>9} {:>10}  {}{}{}\n",
            g.stage,
            g.profile,
            g.model,
            g.buckets.uncached_input,
            g.buckets.cache_read,
            g.buckets.cache_write,
            g.buckets.output,
            g.exact,
            g.provisional,
            tilde(g.reserved),
            g.cost.label(),
            vendor(g.vendor_cost_usd),
            if g.reconciled > 0 {
                format!("  ({} reconciled row(s) not counted)", g.reconciled)
            } else {
                String::new()
            }
        ));
    }
    let total_cost = match r.est_usd {
        Some(usd) => Cost::Est {
            usd,
            with_reservations: r.est_with_reservations,
        }
        .label(),
        None => "—".into(),
    };
    // R3: a total that leaves out groups billed per token says so
    let total_cost = match r.est_complete {
        Some(false) => format!("{total_cost} + no price ({} groups)", r.unpriced_groups),
        _ => total_cost,
    };
    s.push_str(&format!(
        "{:<11} {:<16} {:<22} {:>10} {:>10} {:>10} {:>9} {:>10} {:>9} {:>10}  {total_cost}{}\n",
        "total",
        "",
        "",
        "",
        "",
        "",
        "",
        r.exact,
        r.provisional,
        tilde(r.reserved),
        vendor(r.vendor_cost_usd)
    ));
    for (m, p) in &r.no_price {
        s.push_str(&format!(
            "no price: {m} ({p}) — add it to prices.toml to estimate its cost\n"
        ));
    }
    let at = table
        .map(|p| format!(" ({})", p.display()))
        .unwrap_or_default();
    match (&r.price_version, r.safety_factor) {
        (Some(v), Some(f)) => s.push_str(&format!(
            "prices: version {v}, safety factor {f}{at}; est. = estimated, ~est. = includes reservations (priced as uncached input)\n"
        )),
        _ => s.push_str(&format!(
            "prices: no price table{at} — no cost estimates (templates/prices.toml.example)\n"
        )),
    }
    s
}

fn buckets_json(b: &Buckets) -> Value {
    json!({"uncached_input": b.uncached_input, "cache_read": b.cache_read, "cache_write": b.cache_write, "output": b.output})
}

/// The `--json` document of `toryo usage`.
pub fn to_json(r: &Report, task: &str, table: Option<&Path>) -> Value {
    json!({
        "task": task,
        "prices": {
            "path": table,
            "version": r.price_version,
            "safety_factor": r.safety_factor,
        },
        "rows": r.groups.iter().map(|g| json!({
            "stage": g.stage,
            "profile": g.profile,
            "model": g.model,
            "billing_kind": g.billing_kind,
            "buckets": buckets_json(&g.buckets),
            "exact_tokens": g.exact,
            "provisional_tokens": g.provisional,
            "reserved_tokens": g.reserved,
            "turns": g.turns,
            "reconciled_rows": g.reconciled,
            "includes_unreported": g.includes_unreported,
            "est_cost_usd": g.cost.usd(),
            "cost": g.cost.label(),
            "vendor_reported_cost_usd": g.vendor_cost_usd,
        })).collect::<Vec<_>>(),
        "totals": {
            "exact_tokens": r.exact,
            "provisional_tokens": r.provisional,
            "reserved_tokens": r.reserved,
            "est_cost_usd": r.est_usd,
            "est_complete": r.est_complete,
            "unpriced_groups": r.unpriced_groups,
            "includes_reservations": r.est_with_reservations,
            "vendor_reported_cost_usd": r.vendor_cost_usd,
        },
        "no_price": r.no_price.iter().map(|(m, p)| json!({"model": m, "profile": p})).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = r#"
version = "2026-09-29"
safety_factor = 1.2
[models."gpt-6-astra"]
uncached_input = 1.25
cache_read = 0.125
cache_write = 0.0
output = 10
"#;

    fn row(
        id: i64,
        stage: &str,
        profile: &str,
        model: &str,
        billing: &str,
        acc: &str,
        b: [u64; 4],
    ) -> UsageRow {
        UsageRow {
            id,
            stage: stage.into(),
            profile: profile.into(),
            model: Some(model.into()),
            billing_kind: billing.into(),
            buckets: Buckets {
                uncached_input: b[0],
                cache_read: b[1],
                cache_write: b[2],
                output: b[3],
                reasoning: None,
            },
            accounting: acc.into(),
            ..Default::default()
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn the_price_table_parses_and_names_a_bad_key() {
        let p = parse_prices(TABLE).unwrap();
        assert_eq!(p.version, "2026-09-29");
        assert!(close(p.safety_factor, 1.2));
        assert!(close(p.models["gpt-6-astra"].output, 10.0));
        for (text, key) in [
            ("version = \"v\"\ncolour = 1\n", "colour"),
            (
                "version = \"v\"\n[models.m]\nuncached_input = 1\ncache_read = 1\ncache_write = 1\noutput = 1\nreasoning = 2\n",
                "models.\"m\".reasoning",
            ),
            (
                "version = \"v\"\n[models.m]\nuncached_input = -1\ncache_read = 1\ncache_write = 1\noutput = 1\n",
                "models.\"m\".uncached_input",
            ),
            (
                "version = \"v\"\n[models.m]\nuncached_input = 1\ncache_read = 1\ncache_write = 1\n",
                "models.\"m\".output",
            ),
            ("version = \"v\"\nsafety_factor = -2\n", "safety_factor"),
            ("safety_factor = 1\n", "version"),
        ] {
            let e = format!("{:#}", parse_prices(text).unwrap_err());
            assert!(e.contains(key), "{key}: {e}");
        }
        assert!(parse_prices("version = \n").is_err(), "not TOML");
    }

    #[test]
    fn no_file_is_no_table() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_prices(&dir.path().join("prices.toml")).unwrap(), None);
        let p = dir.path().join("bad.toml");
        std::fs::write(&p, "version = \"v\"\nx = 1\n").unwrap();
        let e = format!("{:#}", load_prices(&p).unwrap_err());
        assert!(e.contains("bad.toml") && e.contains("`x`"), "{e}");
    }

    #[test]
    fn buckets_and_the_three_totals_are_kept_apart() {
        let mut rows = vec![
            row(
                1,
                "plan",
                "claude-max",
                "opus",
                "subscription",
                "exact",
                [100, 2000, 0, 50],
            ),
            row(
                2,
                "plan",
                "claude-max",
                "opus",
                "subscription",
                "exact",
                [10, 0, 0, 5],
            ),
            row(
                3,
                "implement",
                "codex-foundry",
                "gpt-6-astra",
                "foundry",
                "provisional",
                [1000, 0, 0, 100],
            ),
        ];
        let mut unknown = row(
            4,
            "implement",
            "codex-foundry",
            "gpt-6-astra",
            "foundry",
            "unknown",
            [0; 4],
        );
        unknown.reservation_tokens = Some(200_000);
        rows.push(unknown);
        let r = report(&rows, None);
        assert_eq!(r.groups.len(), 2);
        let plan = &r.groups[0];
        assert_eq!(
            (
                plan.buckets.uncached_input,
                plan.buckets.cache_read,
                plan.buckets.output
            ),
            (110, 2000, 55)
        );
        assert_eq!(
            (plan.exact, plan.provisional, plan.reserved, plan.turns),
            (2165, 0, 0, 2)
        );
        let imp = &r.groups[1];
        assert_eq!(
            (imp.exact, imp.provisional, imp.reserved),
            (0, 1100, 200_000)
        );
        assert_eq!((r.exact, r.provisional, r.reserved), (2165, 1100, 200_000));
        // no price table: subscription stays `—`, per-token rows have no estimate
        assert_eq!(plan.cost, Cost::Subscription);
        assert_eq!(imp.cost, Cost::NoTable);
        assert_eq!(r.est_usd, None);
    }

    #[test]
    fn costs_for_per_token_rows_with_the_safety_factor() {
        let prices = parse_prices(TABLE).unwrap();
        let rows = vec![
            row(
                1,
                "plan",
                "claude-max",
                "opus",
                "subscription",
                "exact",
                [1_000_000, 0, 0, 0],
            ),
            row(
                2,
                "implement",
                "codex-foundry",
                "gpt-6-astra",
                "foundry",
                "exact",
                [1_000_000, 2_000_000, 0, 100_000],
            ),
            row(
                3,
                "review",
                "codex-api",
                "gpt-x",
                "api",
                "exact",
                [5, 0, 0, 5],
            ),
        ];
        let r = report(&rows, Some(&prices));
        assert_eq!(r.groups[0].cost.label(), "—");
        // (1 × 1.25 + 2 × 0.125 + 0.1 × 10) × 1.2 = 3.0
        let Cost::Est {
            usd,
            with_reservations,
        } = r.groups[1].cost
        else {
            panic!("{:?}", r.groups[1].cost)
        };
        assert!(close(usd, 3.0) && !with_reservations, "{usd}");
        assert_eq!(r.groups[1].cost.label(), "est. $3.0000");
        assert_eq!(r.groups[2].cost, Cost::NoPrice);
        assert_eq!(
            r.no_price,
            vec![("gpt-x".to_string(), "codex-api".to_string())]
        );
        assert!(close(r.est_usd.unwrap(), 3.0));
        let text = render_text(&r, "T-1", Some(Path::new("p.toml")));
        assert!(text.contains("no price: gpt-x (codex-api)"), "{text}");
        assert!(
            text.contains("version 2026-09-29, safety factor 1.2 (p.toml)"),
            "{text}"
        );
        let last_rows: Vec<&str> = text.lines().filter(|l| l.starts_with("no price")).collect();
        assert_eq!(last_rows.len(), 1, "listed at the end");
    }

    #[test]
    fn reservations_make_an_approximate_estimate() {
        let prices = parse_prices(TABLE).unwrap();
        let mut u = row(
            1,
            "implement",
            "codex-foundry",
            "gpt-6-astra",
            "foundry",
            "unknown",
            [0; 4],
        );
        u.reservation_tokens = Some(200_000);
        let e = row(
            2,
            "implement",
            "codex-foundry",
            "gpt-6-astra",
            "foundry",
            "exact",
            [0, 0, 0, 1_000],
        );
        let r = report(&[u, e], Some(&prices));
        // (0.2 × 1.25 + 0.001 × 10) × 1.2 = 0.312
        let Cost::Est {
            usd,
            with_reservations,
        } = r.groups[0].cost
        else {
            panic!()
        };
        assert!(close(usd, 0.312) && with_reservations, "{usd}");
        assert_eq!(r.groups[0].cost.label(), "~est. $0.3120");
        assert!(r.est_with_reservations);
        let j = to_json(&r, "T-1", None);
        assert_eq!(j["totals"]["includes_reservations"], true);
        assert_eq!(j["rows"][0]["cost"], "~est. $0.3120");
    }

    #[test]
    fn reconciled_rows_are_not_counted_twice() {
        let mut killed = row(
            1,
            "implement",
            "codex-openai",
            "gpt-5-codex",
            "subscription",
            "provisional",
            [500, 0, 0, 10],
        );
        killed.reconciled_by = Some(2);
        let mut reserved = row(
            2,
            "implement",
            "codex-openai",
            "gpt-5-codex",
            "subscription",
            "unknown",
            [0; 4],
        );
        reserved.reservation_tokens = Some(200_000);
        reserved.reconciled_by = Some(3);
        let exact = row(
            3,
            "implement",
            "codex-openai",
            "gpt-5-codex",
            "subscription",
            "exact",
            [900, 0, 0, 20],
        );
        let r = report(&[killed, reserved, exact], None);
        let g = &r.groups[0];
        assert_eq!(
            (g.exact, g.provisional, g.reserved, g.turns, g.reconciled),
            (920, 0, 0, 1, 2)
        );
        assert!(render_text(&r, "T", None).contains("2 reconciled row(s) not counted"));
    }

    #[test]
    fn the_vendor_cost_is_shown_next_to_the_estimate() {
        let prices = parse_prices(TABLE).unwrap();
        let mut a = row(
            1,
            "implement",
            "codex-foundry",
            "gpt-6-astra",
            "foundry",
            "exact",
            [0, 0, 0, 100_000],
        );
        a.vendor_reported_cost_usd = Some(0.9);
        let r = report(&[a], Some(&prices));
        assert_eq!(r.vendor_cost_usd, Some(0.9));
        let text = render_text(&r, "T", None);
        assert!(text.contains("est. $1.2000  (vendor $0.9000)"), "{text}");
    }

    #[test]
    fn r3_an_estimate_that_leaves_out_unpriced_groups_says_so() {
        let prices = parse_prices(TABLE).unwrap();
        let rows = vec![
            row(
                1,
                "implement",
                "codex-foundry",
                "gpt-6-astra",
                "foundry",
                "exact",
                [1_000_000, 0, 0, 0],
            ),
            row(
                2,
                "review",
                "claude-api",
                "sonnet-x",
                "api",
                "exact",
                [10, 0, 0, 1],
            ),
            row(
                3,
                "plan",
                "claude-api",
                "sonnet-x",
                "api",
                "exact",
                [10, 0, 0, 1],
            ),
        ];
        let r = report(&rows, Some(&prices));
        assert_eq!((r.est_complete, r.unpriced_groups), (Some(false), 2));
        let total = render_text(&r, "T", None)
            .lines()
            .find(|l| l.starts_with("total"))
            .unwrap()
            .to_string();
        assert!(
            total.contains("est. $1.5000 + no price (2 groups)"),
            "{total}"
        );
        assert_eq!(to_json(&r, "T", None)["totals"]["est_complete"], false);
        // all priced: complete; nothing estimated: null
        let r = report(&rows[..1], Some(&prices));
        assert_eq!(r.est_complete, Some(true));
        assert!(!render_text(&r, "T", None).contains("+ no price"));
        let r = report(&rows[1..], Some(&prices));
        assert_eq!(r.est_complete, None);
        assert!(to_json(&r, "T", None)["totals"]["est_complete"].is_null());
    }

    #[test]
    fn r3_a_group_never_mixes_billing_kinds() {
        let rows = vec![
            row(
                1,
                "implement",
                "codex-x",
                "m",
                "subscription",
                "exact",
                [10, 0, 0, 0],
            ),
            row(
                2,
                "implement",
                "codex-x",
                "m",
                "api",
                "exact",
                [20, 0, 0, 0],
            ),
        ];
        let r = report(&rows, None);
        assert_eq!(r.groups.len(), 2);
        assert_eq!(
            (r.groups[0].billing_kind.as_str(), r.groups[0].exact),
            ("subscription", 10)
        );
        assert_eq!(
            (r.groups[1].billing_kind.as_str(), r.groups[1].exact),
            ("api", 20)
        );
    }

    #[test]
    fn r3_an_unpriced_model_is_listed_once() {
        let prices = parse_prices(TABLE).unwrap();
        let rows = vec![
            row(
                1,
                "plan",
                "codex-api",
                "gpt-x",
                "api",
                "exact",
                [1, 0, 0, 0],
            ),
            row(
                2,
                "implement",
                "codex-api",
                "gpt-x",
                "api",
                "exact",
                [1, 0, 0, 0],
            ),
            row(
                3,
                "review",
                "codex-api",
                "gpt-x",
                "api",
                "exact",
                [1, 0, 0, 0],
            ),
            row(
                4,
                "implement",
                "codex-foundry",
                "gpt-6-astra",
                "foundry",
                "exact",
                [1, 0, 0, 0],
            ),
            row(
                5,
                "fix-1",
                "codex-api",
                "gpt-x",
                "api",
                "exact",
                [1, 0, 0, 0],
            ),
        ];
        let r = report(&rows, Some(&prices));
        assert_eq!(
            r.no_price,
            vec![("gpt-x".to_string(), "codex-api".to_string())]
        );
        assert_eq!(r.unpriced_groups, 4);
    }

    #[test]
    fn r3_a_row_with_killed_turns_makes_the_estimate_approximate() {
        let prices = parse_prices(TABLE).unwrap();
        let mut a = row(
            1,
            "implement",
            "codex-foundry",
            "gpt-6-astra",
            "foundry",
            "exact",
            [0, 0, 0, 1_000],
        );
        a.includes_unreported = true;
        let r = report(&[a], Some(&prices));
        assert!(r.groups[0].includes_unreported);
        assert!(
            r.groups[0].cost.label().starts_with("~est."),
            "{:?}",
            r.groups[0].cost
        );
        let j = to_json(&r, "T", None);
        assert_eq!(j["rows"][0]["includes_unreported"], true);
        assert_eq!(j["totals"]["includes_reservations"], true);
        assert!(render_text(&r, "T", None).contains("budget tokens"));
    }

    #[test]
    fn models_come_from_the_role_then_the_profile() {
        let p: Profile = serde_yaml::from_str(
            "{ kind: codex, billing: foundry, exe: codex, codex: { overrides: { model: '\"dep-1\"' } } }",
        )
        .unwrap();
        assert_eq!(
            effective_model(Some("role-m"), &p).as_deref(),
            Some("role-m")
        );
        assert_eq!(effective_model(None, &p).as_deref(), Some("dep-1"));
        let mut q = p.clone();
        q.model = Some("prof-m".into());
        assert_eq!(effective_model(None, &q).as_deref(), Some("prof-m"));
        let none: Profile =
            serde_yaml::from_str("{ kind: claude, billing: subscription, exe: claude }").unwrap();
        assert_eq!(effective_model(None, &none), None);
        assert_eq!(stage_role("fix-2"), Some("implementer"));
        assert_eq!(stage_role("plan"), Some("planner"));
        assert_eq!(stage_role("verify"), None);
    }
}
