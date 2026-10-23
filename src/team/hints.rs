//! The words of a refusal (M11-B3; R23-38, moved out of `team.rs` as they were): "did you mean",
//! and serde's messages in the form of a key path.

/// The edit distance between `a` and `b` (Levenshtein, on chars).
pub fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != *cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The one candidate `name` was probably meant to be: a case-insensitive match, else the
/// single candidate within edit distance 2. `None` when nothing is close or two candidates
/// are equally close.
pub fn did_you_mean<'a>(name: &str, candidates: &[&'a str]) -> Option<&'a str> {
    let lower = name.to_lowercase();
    let ci: Vec<&str> = candidates
        .iter()
        .copied()
        .filter(|c| c.to_lowercase() == lower)
        .collect();
    if ci.len() == 1 {
        return Some(ci[0]);
    }
    let mut best: Option<(usize, &str)> = None;
    let mut tie = false;
    for c in candidates {
        let d = edit_distance(&lower, &c.to_lowercase());
        if d > 2 {
            continue;
        }
        match best {
            Some((bd, _)) if d > bd => {}
            Some((bd, _)) if d == bd => tie = true,
            _ => {
                best = Some((d, c));
                tie = false;
            }
        }
    }
    best.filter(|_| !tie).map(|(_, c)| c)
}

/// ` Did you mean 'x'?` when [`did_you_mean`] finds one, else empty.
pub fn hint(name: &str, candidates: &[&str]) -> String {
    did_you_mean(name, candidates)
        .map(|c| format!(" Did you mean '{c}'?"))
        .unwrap_or_default()
}

/// serde's `unknown field` / `missing field` errors in the friendly form, with the key path
/// and a "did you mean" hint: `seats: unknown field `reviwer`, expected one of …` →
/// `seats.reviwer: seat 'reviwer' not found. Did you mean 'reviewer'? (the seats are …)`.
/// `None` for any other error.
pub fn friendly_serde(msg: &str) -> Option<String> {
    static UNKNOWN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^(?:(?P<path>[^ ]+): )?unknown field `(?P<f>[^`]+)`, expected (?P<list>.*?)(?: at line (?P<line>\d+) column \d+)?$").expect("regex")
    });
    static MISSING: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^(?:(?P<path>[^ ]+): )?missing field `(?P<f>[^`]+)`(?: at line (?P<line>\d+) column \d+)?$").expect("regex")
    });
    let line = |c: &regex::Captures| {
        c.name("line")
            .map(|l| format!(" (line {})", l.as_str()))
            .unwrap_or_default()
    };
    if let Some(c) = UNKNOWN.captures(msg) {
        let f = &c["f"];
        let list: Vec<&str> = c["list"].split('`').skip(1).step_by(2).collect();
        let path = c.name("path").map(|p| p.as_str());
        let key = match path {
            Some(p) => format!("{p}.{f}"),
            None => f.to_string(),
        };
        let valid = list.join(", ");
        let what = if path == Some("seats") {
            format!("seat '{f}' not found.{} (the seats are {valid}; the flow is fixed: plan → build → review)", hint(f, &list))
        } else {
            format!("unknown key '{f}'.{} (valid: {valid})", hint(f, &list))
        };
        return Some(format!("{key}: {what}{}", line(&c)));
    }
    if let Some(c) = MISSING.captures(msg) {
        let f = &c["f"];
        let path = c.name("path").map(|p| p.as_str());
        if path == Some("seats") {
            return Some(format!(
                "seats: seat '{f}' is missing — exactly three seats: planner, implementer, reviewer{}",
                line(&c)
            ));
        }
        let key = path
            .map(|p| format!("{p}.{f}"))
            .unwrap_or_else(|| f.to_string());
        return Some(format!("{key}: required{}", line(&c)));
    }
    None
}
