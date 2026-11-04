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

#[cfg(test)]
mod tests {
    //! What a `secret_env` reference resolves to, and every way it is refused — always naming
    //! the reference, never showing the value. Each test uses environment variables of its own
    //! (tests run in parallel in one process). A variable that is not Unicode is not tested here:
    //! set in this process, it would make every other test that lists the environment panic.
    use super::*;

    /// A profile whose `secret_env` is `refs` (`NAME → reference`).
    fn profile(refs: &[(&str, &str)]) -> Profile {
        let mut p: Profile =
            serde_yaml::from_str("{ kind: codex, billing: foundry, exe: codex }").unwrap();
        p.secret_env = refs
            .iter()
            .map(|(k, r)| (k.to_string(), r.to_string()))
            .collect();
        p
    }

    /// An `env:` reference to a variable that is not set names the variable; it is never an
    /// empty secret.
    #[test]
    fn an_unset_variable_is_named_never_an_empty_secret() {
        std::env::remove_var("TORYO_TEST_SECRETS_UNSET");
        let e = format!(
            "{:#}",
            resolve_secret("env:TORYO_TEST_SECRETS_UNSET").unwrap_err()
        );
        assert!(
            e.contains("secret env var `TORYO_TEST_SECRETS_UNSET` is not set"),
            "{e}"
        );
    }

    /// Only the two documented forms resolve; anything else (another scheme, a bare value, the
    /// prefix in another case) is refused, naming both forms.
    #[test]
    fn a_reference_of_another_form_names_the_two_that_exist() {
        std::env::set_var("TORYO_TEST_SECRETS_CASE", "a-long-enough-value");
        for r in [
            "file:C:/secret.txt",
            "a-long-enough-value",
            "ENV:TORYO_TEST_SECRETS_CASE",
            "",
        ] {
            let e = format!("{:#}", resolve_secret(r).unwrap_err());
            assert!(
                e.contains(&format!("unsupported secret reference `{r}`"))
                    && e.contains("use credman:<target> or env:<VAR>"),
                "{r}: {e}"
            );
        }
    }

    /// The limit of X42 counts bytes, not characters: eight bytes of UTF-8 are enough even when
    /// they are four letters, six bytes are not.
    #[test]
    fn the_shortest_secret_is_counted_in_bytes() {
        std::env::set_var("TORYO_TEST_SECRETS_BYTES", "éééé");
        assert_eq!(
            resolve_secret("env:TORYO_TEST_SECRETS_BYTES").unwrap(),
            "éééé"
        );
        std::env::set_var("TORYO_TEST_SECRETS_BYTES", "ééé");
        let e = format!(
            "{:#}",
            resolve_secret("env:TORYO_TEST_SECRETS_BYTES").unwrap_err()
        );
        assert!(e.contains("is 6 byte(s)"), "{e}");
        // a value of several line breaks is still no secret (M13-03), whatever its length
        std::env::set_var("TORYO_TEST_SECRETS_BYTES", "\n\n\n");
        assert_eq!(
            resolve_secret("env:TORYO_TEST_SECRETS_BYTES").unwrap(),
            "\n\n\n"
        );
    }

    /// Off Windows there is no Credential Manager: a `credman:` reference says so and points at
    /// `env:`, and storing a secret is refused — never a silent empty value.
    #[cfg(not(windows))]
    #[test]
    fn off_windows_credential_manager_is_refused_with_the_way_out() {
        let e = format!(
            "{:#}",
            resolve_secret("credman:toryo/foundry-openai").unwrap_err()
        );
        assert!(
            e.contains("credman:toryo/foundry-openai is only available on Windows")
                && e.contains("use env:<VAR> on this host"),
            "{e}"
        );
        let e = format!(
            "{:#}",
            store_secret("toryo/foundry-openai", "a-long-enough-value").unwrap_err()
        );
        assert!(e.contains("only available on Windows"), "{e}");
    }

    /// A profile's secrets resolve to `NAME → value`; the first one that cannot resolve fails
    /// them all, its `secret_env.NAME` in the message (so the owner knows which entry), its
    /// value nowhere.
    #[test]
    fn a_profile_resolves_every_secret_or_names_the_one_that_fails() {
        std::env::set_var("TORYO_TEST_SECRETS_P_KEY", "key-0123456789");
        std::env::set_var("TORYO_TEST_SECRETS_P_TOKEN", "token-0123456789");
        let ok = resolve_profile_secrets(&profile(&[
            ("API_KEY", "env:TORYO_TEST_SECRETS_P_KEY"),
            ("TOKEN", "env:TORYO_TEST_SECRETS_P_TOKEN"),
        ]))
        .unwrap();
        assert_eq!(
            ok.into_iter().collect::<Vec<_>>(),
            [
                ("API_KEY".to_string(), "key-0123456789".to_string()),
                ("TOKEN".to_string(), "token-0123456789".to_string()),
            ]
        );
        std::env::set_var("TORYO_TEST_SECRETS_P_SHORT", "pin42");
        let e = format!(
            "{:#}",
            resolve_profile_secrets(&profile(&[
                ("API_KEY", "env:TORYO_TEST_SECRETS_P_KEY"),
                ("PIN", "env:TORYO_TEST_SECRETS_P_SHORT"),
            ]))
            .unwrap_err()
        );
        assert!(
            e.starts_with("secret_env.PIN: ") && e.contains("is 5 byte(s)"),
            "{e}"
        );
        assert!(
            !e.contains("pin42") && !e.contains("key-0123456789"),
            "no value is shown: {e}"
        );
        // no secrets: an empty map, nothing resolved
        assert!(resolve_profile_secrets(&profile(&[])).unwrap().is_empty());
    }
}
