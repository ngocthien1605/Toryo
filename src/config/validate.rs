//! The semantic checks of a configuration (R23-33, moved as they were): known keys and values,
//! bounds (X43), verify commands.

use super::*;

/// `verify.boundary` values v0.1 implements (R36; `separate_user` is not in v0.1).
pub const VERIFY_BOUNDARIES: &[&str] = &["low_integrity", "none"];

/// `verify.checkout` (R29).
pub const VERIFY_CHECKOUTS: &[&str] = &["clean", "tracked"];

/// `codex.sandbox_mode`: the plan runs Codex in `workspace-write` (R18/R19); `read-only` is
/// stricter and runs with a warning. Anything else (`danger-full-access`) turns off the
/// sandbox that R18 requires.
pub const CODEX_SANDBOX_MODES: &[&str] = &["workspace-write", "read-only"];

/// `codex.windows_sandbox` (R18).
pub const CODEX_WINDOWS_SANDBOXES: &[&str] = &["unelevated", "elevated"];

/// `pipeline.budget_weights` keys: the four token buckets (R41).
pub const BUDGET_WEIGHT_KEYS: &[&str] = &["uncached_input", "cache_read", "cache_write", "output"];

/// R23-11: the longest time limit (minutes) a configuration may give an agent turn or a gate:
/// a week. A larger one overflows the deadline arithmetic (a panic after the agent started).
pub const MAX_TIMEOUT_MIN: u64 = 7 * 24 * 60;

/// R23-11: the largest `context.budget_kb` (1 GB): `kb * 1024` must not wrap.
pub const MAX_BUDGET_KB: u64 = 1024 * 1024;

/// R23-11: the largest token admission cap (`pipeline.budget_tokens`, `toryo budget set`): the
/// budget arithmetic multiplies it by 10 in an i64.
pub const MAX_BUDGET_TOKENS: u64 = (i64::MAX / 10) as u64;

/// A problem for `key` when `value` is above `max`.
pub(super) fn at_most(out: &mut Vec<String>, key: &str, value: Option<u64>, max: u64, what: &str) {
    if let Some(v) = value.filter(|v| *v > max) {
        out.push(format!("{key}: `{v}` is too large (at most {max} {what})"));
    }
}

/// The pipeline's roles: the keys of `roles` and `context.budget_kb`.
pub(super) fn role_names() -> [&'static str; 3] {
    use crate::domain::Role as R;
    [R::Planner, R::Implementer, R::Reviewer].map(R::as_str)
}

/// A problem for `key` unless `value` is one of `valid` (the message lists them).
pub(super) fn one_of(out: &mut Vec<String>, key: &str, value: &str, valid: &[&str]) {
    if !valid.contains(&value) {
        let valid = valid.join(", ");
        out.push(format!(
            "{key}: `{value}` is not a valid value (valid: {valid})"
        ));
    }
}

/// A problem for the key `section.name` unless `name` is one of `valid`.
pub(super) fn known_key(
    out: &mut Vec<String>,
    section: &str,
    name: &str,
    what: &str,
    valid: &[&str],
) {
    if !valid.contains(&name) {
        // M11-B3: a "did you mean" hint when one valid key is close
        let hint = crate::team::hint(name, valid);
        out.push(format!(
            "{section}.{name}: unknown {what} (valid: {}){hint}",
            valid.join(", ")
        ));
    }
}

/// Every semantic problem of a (merged) configuration, as `key: what is wrong`. `machine` names
/// the machine config for the messages that send the reader there.
pub(super) fn problems(
    repo: &RepoConfig,
    toolchains: &Toolchains,
    machine: &str,
    team: bool,
    origins: &Origins,
) -> Vec<String> {
    let mut out = Vec::new();
    let roles = role_names();
    for name in repo.roles.keys() {
        // M11-R8: a role (seat) only the machine config names: that file, as it spells the key
        let section = match origins.roles.get(name) {
            Some(Origin::Machine) => format!(
                "{machine}: {}",
                origins.machine_roles_key.unwrap_or("roles")
            ),
            _ => "roles".into(),
        };
        known_key(&mut out, &section, name, "role", &roles);
    }
    for (name, kb) in &repo.context.budget_kb {
        known_key(&mut out, "context.budget_kb", name, "role", &roles);
        let key = format!("context.budget_kb.{name}");
        at_most(&mut out, &key, Some(*kb), MAX_BUDGET_KB, "KB");
    }
    let t = &repo.timeouts;
    for (key, v) in [
        ("timeouts.agent_stall_min", t.agent_stall_min),
        ("timeouts.agent_stage_max_min", t.agent_stage_max_min),
    ] {
        at_most(&mut out, key, v, MAX_TIMEOUT_MIN, "minutes, a week");
    }
    at_most(
        &mut out,
        "pipeline.budget_tokens",
        repo.pipeline.budget_tokens,
        MAX_BUDGET_TOKENS,
        "tokens",
    );
    for (name, w) in repo.pipeline.budget_weights.iter().flatten() {
        let section = "pipeline.budget_weights";
        known_key(&mut out, section, name, "bucket", BUDGET_WEIGHT_KEYS);
        // R23-02: `.inf`, `.nan` or `1e400` would be frozen as `null`, which the task can never
        // read back; a negative weight would make spending tokens give budget back
        if !w.is_finite() || *w < 0.0 {
            out.push(format!(
                "{section}.{name}: `{w}` is not a valid weight (a finite number, 0 or more)"
            ));
        }
    }
    // plan §14 (R41): `auto` = the largest exact turn of the stage, else a token count
    if let Some(v) = &repo.pipeline.unknown_turn_reservation_tokens {
        if v.as_u64().is_none() && v.as_str() != Some("auto") {
            let shown = serde_yaml::to_string(v).unwrap_or_default();
            out.push(format!(
                "pipeline.unknown_turn_reservation_tokens: `{}` is not a valid value (valid: auto, or a number of tokens)",
                shown.trim()
            ));
        }
    }
    let v = &repo.verify;
    one_of(&mut out, "verify.boundary", &v.boundary, VERIFY_BOUNDARIES);
    one_of(&mut out, "verify.checkout", &v.checkout, VERIFY_CHECKOUTS);
    for (name, p) in &repo.profiles {
        let Some(c) = &p.codex else { continue };
        let key = format!("profiles.{name}.codex.sandbox_mode");
        one_of(&mut out, &key, &c.sandbox_mode, CODEX_SANDBOX_MODES);
        if let Some(ws) = &c.windows_sandbox {
            let key = format!("profiles.{name}.codex.windows_sandbox");
            one_of(&mut out, &key, ws, CODEX_WINDOWS_SANDBOXES);
        }
        let provider = c.provider.as_ref().map(|p| p.id.as_str());
        for k in c.overrides.keys() {
            if let Some(why) = denied_override(k, provider) {
                out.push(format!("profiles.{name}.codex.overrides.{k}: {why}"));
            }
        }
    }
    for (i, c) in repo.commands.verify.iter().enumerate() {
        if team {
            break; // M11-B3: the check gates, checked as `gates.<name>` by `team::problems`
        }
        let key = format!("commands.verify[{i}] ({})", c.name);
        verify_command_problems(&mut out, &key, c, toolchains, machine);
        let k = format!("{key}.timeout_min");
        at_most(
            &mut out,
            &k,
            c.timeout_min,
            MAX_TIMEOUT_MIN,
            "minutes, a week",
        );
    }
    if let Some(x) = &toolchains.xpp {
        for (k, v) in [("msbuild", &x.msbuild), ("nugets", &x.nugets)] {
            if v.trim().is_empty() {
                out.push(format!(
                    "toolchains.xpp.{k}: empty (machine config {machine})"
                ));
            }
        }
        if x.bp_check {
            out.push(format!("toolchains.xpp.bp_check: `true` is not supported yet — xppbp.exe waits for V08-VERIFY 4; set it to false (machine config {machine})"));
        }
    }
    out
}

/// The problems of one `commands.verify[]` item (R59): exactly one of `program` and `tool`;
/// `tool` is `xpp`; `tool: xpp` has `metadata` and projects, as relative paths inside the
/// repository, and needs `toolchains.xpp` in the machine config.
pub(crate) fn verify_command_problems(
    out: &mut Vec<String>,
    key: &str,
    c: &CommandSpec,
    toolchains: &Toolchains,
    machine: &str,
) {
    let key = key.to_string();
    let Some(tool) = &c.tool else {
        if c.program.is_none() {
            out.push(format!("{key}: needs `program` (with `args`) or `tool`"));
        }
        for (k, set) in [
            ("metadata", c.metadata.is_some()),
            ("projects", !c.projects.is_empty()),
        ] {
            if set {
                out.push(format!(
                    "{key}.{k}: only for `tool: xpp`, not for a `program` command"
                ));
            }
        }
        if let Some(d) = &c.diagnostics {
            one_of(out, &format!("{key}.diagnostics"), d, DIAGNOSTICS_FORMATS);
        }
        return;
    };
    if c.diagnostics.is_some() {
        out.push(format!(
            "{key}.diagnostics: only for a `program` command — `tool: xpp` always reads MSBuild diagnostics"
        ));
    }
    if c.program.is_some() {
        out.push(format!("{key}: has both `program` and `tool` — use one"));
    }
    if !c.args.is_empty() {
        out.push(format!("{key}.args: only for a `program` command — `tool: xpp` takes `metadata` and `projects`"));
    }
    one_of(out, &format!("{key}.tool"), tool, VERIFY_TOOLS);
    if tool != "xpp" {
        return;
    }
    match &c.metadata {
        None => out.push(format!("{key}.metadata: required for `tool: xpp` (the X++ source folder, relative to the repository)")),
        Some(m) => {
            if let Some(why) = not_a_repo_path(m) {
                out.push(format!("{key}.metadata: `{m}` {why}"));
            }
        }
    }
    if c.projects.is_empty() {
        out.push(format!("{key}.projects: `tool: xpp` needs at least one project (one X++ project per package, in build order)"));
    }
    for p in &c.projects {
        if let Some(why) = not_a_repo_path(p) {
            out.push(format!("{key}.projects: `{p}` {why}"));
        }
    }
    if toolchains.xpp.is_none() {
        out.push(format!("{key}.tool: `tool: xpp` needs `toolchains.xpp` in the machine config ({machine}): msbuild, nugets (R59)"));
    }
}

/// Why `p` is not a relative path inside the repository, or None: empty, absolute, rooted,
/// with a drive, or with a `..` segment.
pub fn not_a_repo_path(p: &str) -> Option<&'static str> {
    let t = p.trim();
    if t.is_empty() {
        return Some("is empty");
    }
    let rooted = t.starts_with(['/', '\\']) || t.chars().nth(1) == Some(':');
    if rooted || Path::new(t).is_absolute() {
        return Some("must be relative to the repository");
    }
    if t.split(['/', '\\']).any(|c| c == "..") {
        return Some("may not contain `..`");
    }
    None
}
