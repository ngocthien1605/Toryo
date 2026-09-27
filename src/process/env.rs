//! Child environment construction (plan R23).
//!
//! Every child starts from an empty environment. A short allowlist is copied
//! from the parent, then the profile's `env` and resolved `secret_env` are
//! added. Provider-selector conflicts are rejected before spawn so a stray
//! variable cannot move a subscription run onto a paid API.

use std::collections::BTreeMap;

use crate::config::{Billing, ProfileKind};

/// Variables copied from the parent environment (plan §13).
pub const ALLOWLIST: &[&str] = &[
    // plan §13
    "SystemRoot",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PATH",
    "LANG",
    // M0 extension (finding F4): standard Windows variables that Git Bash, Node and .NET
    // commonly expect. None selects a provider or carries a secret. v0.7 decides.
    "SystemDrive",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "CommonProgramFiles",
    "USERNAME",
    "USERDOMAIN",
    "HOMEDRIVE",
    "HOMEPATH",
    "PROCESSOR_ARCHITECTURE",
    "NUMBER_OF_PROCESSORS",
    "OS",
    // Unix equivalents so the spike and the tests also run on Linux/macOS.
    "HOME",
    "TMPDIR",
];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EnvError {
    #[error("profile `{profile}` ({billing:?}) must not carry `{var}`: it selects a different provider/billing path")]
    ProviderConflict {
        profile: String,
        billing: Billing,
        var: String,
    },
    #[error("profile `{profile}` ({billing:?}) is missing required variable `{var}`")]
    Missing {
        profile: String,
        billing: Billing,
        var: String,
    },
}

/// Build the child environment. Keys are compared case-insensitively on Windows
/// semantics (the map keeps the last spelling seen).
pub fn build_env(
    parent: impl IntoIterator<Item = (String, String)>,
    profile_env: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in parent {
        if ALLOWLIST.iter().any(|a| a.eq_ignore_ascii_case(&k)) {
            insert_ci(&mut out, k, v);
        }
    }
    for (k, v) in profile_env {
        insert_ci(&mut out, k.clone(), v.clone());
    }
    for (k, v) in secrets {
        insert_ci(&mut out, k.clone(), v.clone());
    }
    out
}

fn insert_ci(map: &mut BTreeMap<String, String>, k: String, v: String) {
    if let Some(existing) = map.keys().find(|e| e.eq_ignore_ascii_case(&k)).cloned() {
        map.remove(&existing);
    }
    map.insert(k, v);
}

fn has(env: &BTreeMap<String, String>, var: &str) -> bool {
    env.iter()
        .any(|(k, v)| k.eq_ignore_ascii_case(var) && !v.is_empty())
}

/// zeroshot `validate_provider_selection`, extended per plan §13.
pub fn validate_provider_selection(
    profile: &str,
    kind: ProfileKind,
    billing: Billing,
    env: &BTreeMap<String, String>,
) -> Result<(), EnvError> {
    if kind != ProfileKind::Claude {
        return Ok(());
    }
    let forbidden: &[&str] = match billing {
        Billing::Subscription => &[
            "CLAUDE_CODE_USE_FOUNDRY",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_FOUNDRY_API_KEY",
        ],
        Billing::Foundry => &[
            "CLAUDE_CODE_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
        ],
        Billing::Api => &[
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_USE_FOUNDRY",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "ANTHROPIC_FOUNDRY_API_KEY",
        ],
    };
    for var in forbidden {
        if has(env, var) {
            return Err(EnvError::ProviderConflict {
                profile: profile.into(),
                billing,
                var: (*var).into(),
            });
        }
    }
    let required: &[&str] = match billing {
        Billing::Subscription => &[], // interactive /login of the Windows user is allowed
        Billing::Foundry => &[
            "CLAUDE_CODE_USE_FOUNDRY",
            "ANTHROPIC_FOUNDRY_RESOURCE",
            "ANTHROPIC_FOUNDRY_API_KEY",
        ],
        Billing::Api => &["ANTHROPIC_API_KEY"],
    };
    for var in required {
        if !has(env, var) {
            return Err(EnvError::Missing {
                profile: profile.into(),
                billing,
                var: (*var).into(),
            });
        }
    }
    Ok(())
}

/// Environment printed or hashed for reproducibility: secret values removed.
pub fn non_secret_view(
    env: &BTreeMap<String, String>,
    secret_keys: &[String],
) -> BTreeMap<String, String> {
    env.iter()
        .map(|(k, v)| {
            if secret_keys.iter().any(|s| s.eq_ignore_ascii_case(k)) {
                (k.clone(), "<secret>".to_string())
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn only_allowlist_is_inherited() {
        let parent = vec![
            ("PATH".to_string(), "/bin".to_string()),
            ("ANTHROPIC_API_KEY".to_string(), "sk-stray".to_string()),
            ("CLAUDE_CODE_USE_FOUNDRY".to_string(), "1".to_string()),
            ("SystemRoot".to_string(), "C:\\Windows".to_string()),
        ];
        let env = build_env(parent, &BTreeMap::new(), &BTreeMap::new());
        assert!(env.contains_key("PATH"));
        assert!(env.contains_key("SystemRoot"));
        assert!(!env.contains_key("ANTHROPIC_API_KEY"));
        assert!(!env.contains_key("CLAUDE_CODE_USE_FOUNDRY"));
    }

    #[test]
    fn profile_overrides_parent_case_insensitively() {
        let env = build_env(
            vec![("Path".into(), "a".into())],
            &m(&[("PATH", "b")]),
            &BTreeMap::new(),
        );
        assert_eq!(env.len(), 1);
        assert_eq!(env.get("PATH").map(String::as_str), Some("b"));
    }

    #[test]
    fn subscription_rejects_foundry_and_api_selectors() {
        for var in [
            "CLAUDE_CODE_USE_FOUNDRY",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_BASE_URL",
        ] {
            let env = m(&[(var, "x")]);
            let e = validate_provider_selection(
                "claude-max",
                ProfileKind::Claude,
                Billing::Subscription,
                &env,
            );
            assert!(matches!(e, Err(EnvError::ProviderConflict { .. })), "{var}");
        }
        assert!(validate_provider_selection(
            "claude-max",
            ProfileKind::Claude,
            Billing::Subscription,
            &m(&[])
        )
        .is_ok());
    }

    #[test]
    fn foundry_requires_its_variables_and_rejects_oauth() {
        let ok = m(&[
            ("CLAUDE_CODE_USE_FOUNDRY", "1"),
            ("ANTHROPIC_FOUNDRY_RESOURCE", "r"),
            ("ANTHROPIC_FOUNDRY_API_KEY", "k"),
        ]);
        assert!(
            validate_provider_selection("f", ProfileKind::Claude, Billing::Foundry, &ok).is_ok()
        );
        let mut bad = ok.clone();
        bad.insert("CLAUDE_CODE_OAUTH_TOKEN".into(), "t".into());
        assert!(
            validate_provider_selection("f", ProfileKind::Claude, Billing::Foundry, &bad).is_err()
        );
        let missing = m(&[("CLAUDE_CODE_USE_FOUNDRY", "1")]);
        assert!(matches!(
            validate_provider_selection("f", ProfileKind::Claude, Billing::Foundry, &missing),
            Err(EnvError::Missing { .. })
        ));
    }

    #[test]
    fn non_secret_view_hides_values() {
        let env = m(&[("A", "1"), ("KEY", "s3cret")]);
        let v = non_secret_view(&env, &["key".into()]);
        assert_eq!(v["KEY"], "<secret>");
        assert_eq!(v["A"], "1");
    }
}
