//! Secrets (R23-33, moved as they were): `secret_env` references, Credential Manager, the shortest
//! secret (X42).

use super::*;

/// R23-04: the fewest bytes a secret value may have. Every occurrence of a secret's bytes is
/// redacted from what the agents and the gates print; a value of a few bytes (`1`) would be
/// found inside every line — the agent's JSON stream would no longer parse.
pub const MIN_SECRET_BYTES: usize = 8;

/// Resolve a `secret_env` reference. Never logs the value.
pub fn resolve_secret(reference: &str) -> Result<String> {
    let v = resolve_secret_value(reference)?;
    // M13-03: a value of line breaks only is no secret (not redacted, noted by the redactor)
    if v.len() < MIN_SECRET_BYTES && !v.bytes().all(|b| b == b'\n') {
        bail!(
            "the value of `{reference}` is {} byte(s); a secret needs at least {MIN_SECRET_BYTES} — every occurrence of its bytes would be redacted from the agents' and the gates' output (a value that is no secret belongs under `env:`, not `secret_env:`)",
            v.len()
        );
    }
    Ok(v)
}

pub(super) fn resolve_secret_value(reference: &str) -> Result<String> {
    if let Some(var) = reference.strip_prefix("env:") {
        // (R16–R17 review) never through `VarError`: its `NotUnicode` form prints the value
        return match std::env::var(var) {
            Ok(v) => Ok(v),
            Err(std::env::VarError::NotPresent) => bail!("secret env var `{var}` is not set"),
            Err(std::env::VarError::NotUnicode(_)) => {
                bail!("secret env var `{var}` is not valid Unicode")
            }
        };
    }
    if let Some(target) = reference.strip_prefix("credman:") {
        return resolve_credman(target);
    }
    bail!("unsupported secret reference `{reference}` (use credman:<target> or env:<VAR>)")
}

#[cfg(windows)]
pub(super) fn resolve_credman(target: &str) -> Result<String> {
    // `toryo/claude-max-oauth` → service "toryo", user "claude-max-oauth"
    let (service, user) = target.split_once('/').unwrap_or(("toryo", target));
    let entry = keyring::Entry::new(service, user).context("opening Credential Manager entry")?;
    entry
        .get_password()
        .with_context(|| format!("reading Credential Manager entry `{target}`"))
}

#[cfg(not(windows))]
pub(super) fn resolve_credman(target: &str) -> Result<String> {
    bail!("credman:{target} is only available on Windows; use env:<VAR> on this host")
}

#[cfg(windows)]
pub fn store_secret(target: &str, value: &str) -> Result<()> {
    let (service, user) = target.split_once('/').unwrap_or(("toryo", target));
    keyring::Entry::new(service, user)?.set_password(value)?;
    Ok(())
}

#[cfg(not(windows))]
pub fn store_secret(_target: &str, _value: &str) -> Result<()> {
    bail!("Credential Manager is only available on Windows")
}

/// Resolve all secrets of a profile: returns (env name → value).
pub fn resolve_profile_secrets(p: &Profile) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for (k, r) in &p.secret_env {
        out.insert(
            k.clone(),
            resolve_secret(r).with_context(|| format!("secret_env.{k}"))?,
        );
    }
    Ok(out)
}
