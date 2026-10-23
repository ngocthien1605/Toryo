//! The Codex `-c` overrides a profile may not set (R23-33, moved as it was): the sandbox, the
//! provider, what the launch line already says.

/// The fields of `codex.provider` that toryo passes as `model_providers.<id>.<field>`.
pub(super) const PROVIDER_FIELDS: &[&str] = &["name", "base_url", "env_key", "wire_api"];

/// Why a `codex.overrides` key is refused (ARCH-05), or None. The overrides are passed after
/// toryo's own `-c` settings and Codex applies the last one, so exactly these are refused:
/// what toryo sets itself or what switches the sandbox, approval or environment policy —
/// `sandbox_mode`, the legacy `sandbox_permissions`, `windows` / `windows.*`,
/// `approval_policy` and `shell_environment_policy*` (with their sub-keys); and, only when
/// the profile has a `codex.provider` block (`provider_id`), the selector `model_provider`
/// and `model_providers.<id>.{name, base_url, env_key, wire_api}` or a table holding them.
/// Everything else is allowed: `sandbox_workspace_write.*`, the provider's other keys
/// (`query_params`, `request_max_retries` …), and `model_provider*` without a provider
/// block, which is how `toryo spike init-config` selects a translated profile's provider.
/// The key is taken as Codex reads it: up to the first `=`, without whitespace or quotes,
/// and compared in lower case.
pub fn denied_override(key: &str, provider_id: Option<&str>) -> Option<&'static str> {
    let k: String = key
        .split('=')
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '"' && *c != '\'')
        .collect::<String>()
        .to_ascii_lowercase();
    if ["sandbox_mode", "sandbox_permissions", "windows"]
        .iter()
        .any(|p| is_or_under(&k, p))
    {
        Some("re-sets the sandbox that toryo sets (codex.sandbox_mode / codex.windows_sandbox, R18/R19)")
    } else if is_or_under(&k, "approval_policy") {
        Some("re-sets the approval policy of the unattended `codex exec` turn")
    } else if k.starts_with("shell_environment_policy") {
        Some("re-sets the environment of Codex's commands, which toryo builds (R23)")
    } else if k == "profile" || is_or_under(&k, "profiles") {
        Some("selects or defines a Codex profile, whose settings would re-set the sandbox and provider that toryo sets")
    } else if provider_id.is_some_and(|id| resets_provider(&k, id)) {
        Some("re-sets the provider of codex.provider (change it there)")
    } else {
        None
    }
}

/// `key` is `prefix` or one of its sub-keys (`prefix.…`).
pub(super) fn is_or_under(key: &str, prefix: &str) -> bool {
    key.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

/// `key` (normalised) re-sets what toryo passes for the provider `id`: the selector, a
/// field of [`PROVIDER_FIELDS`], or a table holding them.
pub(super) fn resets_provider(key: &str, id: &str) -> bool {
    let table = format!("model_providers.{}", id.to_ascii_lowercase());
    key == "model_providers"
        || key == table
        || is_or_under(key, "model_provider")
        || PROVIDER_FIELDS
            .iter()
            .any(|f| is_or_under(key, &format!("{table}.{f}")))
}
