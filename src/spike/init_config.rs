//! `toryo spike init-config` — write a `toryo.yaml` for this machine, and translate
//! the user's Codex profiles (`codex --profile foundry|openai`) into explicit `-c`
//! overrides.
//!
//! Why translate: toryo runs Codex with `--ignore-user-config` (R19) so a run never
//! depends on `~/.codex/config.toml`. That also switches off `--profile`, so the
//! profile's settings (model, provider, base URL, env_key, query params …) are copied
//! into the toryo profile once, visibly, and are then hashed with every stage attempt.
//! Secrets are never copied: `env_key` names a variable that is passed through via
//! `secret_env: { VAR: env:VAR }`; header/token values are skipped with a warning.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use toml::Value;

use crate::config::{Billing, CodexSettings, Profile, ProfileKind};

/// Keys of a Codex profile that toryo owns or that must not travel.
const SKIP_PROFILE_KEYS: &[&str] = &[
    "sandbox_mode",
    "approval_policy",
    "sandbox_workspace_write",
    "windows",
    "shell_environment_policy",
    "mcp_servers",
    "notify",
    "projects",
    "history",
    "tui",
    "file_opener",
    "profiles",
    "model_providers",
];

fn looks_secret(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "api_key",
        "apikey",
        "api-key",
        "authorization",
        "bearer",
        "http_headers",
    ]
    .iter()
    .any(|w| k.contains(w))
        && k != "env_key"
        && k != "env_http_headers"
}

fn bare_key_ok(k: &str) -> bool {
    !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Flatten a TOML value into (dotted.key, TOML literal) pairs.
fn flatten(
    prefix: &str,
    v: &Value,
    out: &mut BTreeMap<String, String>,
    warnings: &mut Vec<String>,
) {
    match v {
        Value::Table(t) => {
            for (k, v) in t {
                if !bare_key_ok(k) {
                    warnings.push(format!("skipped `{prefix}.{k}`: key needs quoting"));
                    continue;
                }
                if looks_secret(k) {
                    warnings.push(format!("skipped `{prefix}.{k}`: looks like a secret — pass it through an env var instead"));
                    continue;
                }
                flatten(&format!("{prefix}.{k}"), v, out, warnings);
            }
        }
        other => {
            out.insert(prefix.to_string(), other.to_string());
        }
    }
}

#[derive(Debug)]
pub struct ImportedCodex {
    pub profile: Profile,
    pub warnings: Vec<String>,
}

/// Translate `[profiles.<name>]` (+ top-level defaults + the referenced provider) into a toryo profile.
pub fn import_codex_profile(
    config_toml: &str,
    name: &str,
    exe: &str,
    windows_sandbox: &str,
) -> Result<ImportedCodex> {
    let doc: toml::Table = config_toml.parse().context("parsing Codex config.toml")?;
    let profiles = doc.get("profiles").and_then(Value::as_table);
    let Some(prof) = profiles.and_then(|p| p.get(name)).and_then(Value::as_table) else {
        let known: Vec<String> = profiles
            .map(|p| p.keys().cloned().collect())
            .unwrap_or_default();
        bail!("Codex config has no [profiles.{name}] (found: {known:?})");
    };
    let mut warnings = Vec::new();
    // top-level defaults first, the profile overrides them
    let mut merged = toml::Table::new();
    for k in [
        "model",
        "model_provider",
        "model_reasoning_effort",
        "model_reasoning_summary",
        "model_verbosity",
    ] {
        if let Some(v) = doc.get(k) {
            merged.insert(k.into(), v.clone());
        }
    }
    for (k, v) in prof {
        if SKIP_PROFILE_KEYS.contains(&k.as_str()) {
            warnings.push(format!(
                "not imported from [profiles.{name}]: `{k}` (toryo sets its own policy)"
            ));
            continue;
        }
        merged.insert(k.clone(), v.clone());
    }
    let model = merged
        .remove("model")
        .and_then(|v| v.as_str().map(str::to_string));
    let provider_id = merged
        .get("model_provider")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut overrides = BTreeMap::new();
    for (k, v) in &merged {
        if !bare_key_ok(k) || looks_secret(k) {
            warnings.push(format!("skipped `{k}`"));
            continue;
        }
        flatten(k, v, &mut overrides, &mut warnings);
    }
    let mut secret_env = BTreeMap::new();
    let mut billing = Billing::Subscription;
    if let Some(pid) = &provider_id {
        let prov = doc
            .get("model_providers")
            .and_then(Value::as_table)
            .and_then(|t| t.get(pid))
            .and_then(Value::as_table);
        match prov {
            Some(prov) => {
                flatten(&format!("model_providers.{pid}"), &Value::Table(prov.clone()), &mut overrides, &mut warnings);
                if let Some(ek) = prov.get("env_key").and_then(Value::as_str) {
                    secret_env.insert(ek.to_string(), format!("env:{ek}"));
                    billing = if pid.contains("azure") || pid.contains("foundry") || prov.get("base_url").and_then(Value::as_str).is_some_and(|u| u.contains("azure")) {
                        Billing::Foundry
                    } else {
                        Billing::Api
                    };
                }
                if let Some(h) = prov.get("env_http_headers").and_then(Value::as_table) {
                    for var in h.values().filter_map(Value::as_str) {
                        secret_env.insert(var.to_string(), format!("env:{var}"));
                    }
                }
                if prov.get("requires_openai_auth").and_then(Value::as_bool) == Some(true) {
                    billing = Billing::Subscription;
                }
            }
            None if pid == "openai" || pid == "oss" => {}
            None => warnings.push(format!("model_provider `{pid}` is not defined under [model_providers] — built-in provider assumed")),
        }
    }
    let codex = CodexSettings {
        sandbox_mode: "workspace-write".into(),
        windows_sandbox: Some(windows_sandbox.into()),
        provider: None,
        project_doc_max_bytes: None,
        model_reasoning_effort: None,
        overrides,
    };
    Ok(ImportedCodex {
        profile: Profile {
            kind: ProfileKind::Codex,
            billing,
            exe: exe.into(),
            version: None,
            env: BTreeMap::new(),
            secret_env,
            codex: Some(codex),
            model,
        },
        warnings,
    })
}

pub struct InitArgs {
    pub out: PathBuf,
    pub project_root: String,
    pub worktrees_root: String,
    pub claude_exe: String,
    pub git_bash: Option<String>,
    pub codex_exe: String,
    pub codex_config: Option<PathBuf>,
    pub codex_profiles: Vec<String>,
    pub windows_sandbox: String,
    /// CODEX_HOME to pass to Codex (when the user has one set).
    pub codex_home: Option<String>,
    /// Missing `[profiles.<name>]`: fall back (openai → default login) or skip, instead of failing.
    pub lenient: bool,
}

/// Build the YAML text (also used by the tests).
pub fn render_config(a: &InitArgs) -> Result<(String, Vec<String>)> {
    let mut profiles: BTreeMap<String, Profile> = BTreeMap::new();
    let mut env = BTreeMap::new();
    if let Some(b) = &a.git_bash {
        env.insert("CLAUDE_CODE_GIT_BASH_PATH".to_string(), b.clone());
    }
    profiles.insert(
        "claude-max".into(),
        Profile {
            kind: ProfileKind::Claude,
            billing: Billing::Subscription,
            exe: a.claude_exe.clone(),
            version: None,
            env,
            secret_env: BTreeMap::new(),
            codex: None,
            model: None,
        },
    );
    let mut warnings = Vec::new();
    let text = match &a.codex_config {
        Some(p) if p.exists() => {
            Some(std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?)
        }
        Some(p) => {
            warnings.push(format!(
                "{} not found: plain Codex profile with its default login",
                p.display()
            ));
            None
        }
        None => None,
    };
    let names = if a.codex_profiles.is_empty() {
        vec![String::new()]
    } else {
        a.codex_profiles.clone()
    };
    for n in names {
        let toryo_name = if n.is_empty() {
            "codex".to_string()
        } else {
            format!("codex-{n}")
        };
        let plain = || Profile {
            kind: ProfileKind::Codex,
            billing: Billing::Subscription,
            exe: a.codex_exe.clone(),
            version: None,
            env: BTreeMap::new(),
            secret_env: BTreeMap::new(),
            codex: Some(CodexSettings {
                sandbox_mode: "workspace-write".into(),
                windows_sandbox: Some(a.windows_sandbox.clone()),
                ..Default::default()
            }),
            model: None,
        };
        let mut prof = match (&text, n.is_empty()) {
            (Some(t), false) => {
                match import_codex_profile(t, &n, &a.codex_exe, &a.windows_sandbox) {
                    Ok(imp) => {
                        warnings.extend(
                            imp.warnings
                                .into_iter()
                                .map(|w| format!("[{toryo_name}] {w}")),
                        );
                        imp.profile
                    }
                    Err(e) if a.lenient => {
                        if n == "openai" || n == "chatgpt" {
                            warnings.push(format!("[{toryo_name}] {e:#} — using Codex's default login (ChatGPT) instead"));
                            plain()
                        } else {
                            warnings.push(format!("[{toryo_name}] {e:#} — profile SKIPPED"));
                            continue;
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
            _ => plain(),
        };
        if let Some(h) = &a.codex_home {
            // Codex keeps auth + sessions in CODEX_HOME; not in the env allowlist, so pass it explicitly.
            prof.env.insert("CODEX_HOME".into(), h.clone());
        }
        profiles.insert(toryo_name, prof);
    }
    let first_codex = profiles
        .keys()
        .find(|k| k.starts_with("codex"))
        .cloned()
        .unwrap_or_default();
    let doc = serde_json::json!({
        "version": 1,
        "project": { "root": a.project_root, "worktrees_root": a.worktrees_root },
        "profiles": profiles,
        "roles": {
            "planner": { "profile": "claude-max" },
            "implementer": { "profile": first_codex, "fallback": ["claude-max"] },
            "reviewer": { "profile": "claude-max" },
        },
    });
    let yaml = format!(
        "# generated by `toryo spike init-config` — Codex profiles translated from ~/.codex/config.toml\n# (toryo runs Codex with --ignore-user-config, so `--profile` is replaced by these -c overrides)\n{}",
        serde_yaml::to_string(&doc)?
    );
    Ok((yaml, warnings))
}

pub fn init_config(a: &InitArgs) -> Result<()> {
    let (yaml, warnings) = render_config(a)?;
    std::fs::write(&a.out, &yaml)?;
    // prove it loads
    crate::config::Effective::load(&a.out, None).context("generated config does not load")?;
    for w in warnings {
        eprintln!("WARNING: {w}");
    }
    print!("{yaml}");
    eprintln!("written to {}", a.out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: &str = r#"
model = "gpt-5"
model_reasoning_effort = "medium"
approval_policy = "on-request"

[profiles.foundry]
model = "gpt-5-codex"
model_provider = "azure"
model_reasoning_effort = "high"
approval_policy = "never"

[profiles.openai]
model = "gpt-5.1-codex"
model_provider = "openai"

[model_providers.azure]
name = "Azure"
base_url = "https://my-res.openai.azure.com/openai/v1"
env_key = "AZURE_OPENAI_API_KEY"
wire_api = "responses"
query_params = { api-version = "2025-04-01-preview" }
http_headers = { "X-Secret" = "abc" }
experimental_bearer_token = "sk-should-not-travel"
"#;

    #[test]
    fn foundry_profile_becomes_explicit_overrides_without_secrets() {
        let imp = import_codex_profile(CFG, "foundry", "codex.cmd", "unelevated").unwrap();
        let p = imp.profile;
        assert_eq!(p.model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(p.billing, Billing::Foundry);
        assert_eq!(
            p.secret_env.get("AZURE_OPENAI_API_KEY").map(String::as_str),
            Some("env:AZURE_OPENAI_API_KEY")
        );
        let o = &p.codex.as_ref().unwrap().overrides;
        assert_eq!(o["model_provider"], "\"azure\"");
        assert_eq!(o["model_reasoning_effort"], "\"high\"");
        assert_eq!(
            o["model_providers.azure.base_url"],
            "\"https://my-res.openai.azure.com/openai/v1\""
        );
        assert_eq!(
            o["model_providers.azure.query_params.api-version"],
            "\"2025-04-01-preview\""
        );
        assert!(!o
            .keys()
            .any(|k| k.contains("bearer") || k.contains("http_headers") || k.contains("approval")));
        assert!(!o
            .values()
            .any(|v| v.contains("sk-should-not-travel") || v.contains("abc")));
        assert!(imp.warnings.iter().any(|w| w.contains("approval_policy")));
    }

    #[test]
    fn openai_profile_uses_chatgpt_login() {
        let imp = import_codex_profile(CFG, "openai", "codex.cmd", "unelevated").unwrap();
        assert_eq!(imp.profile.billing, Billing::Subscription);
        assert!(imp.profile.secret_env.is_empty());
        assert_eq!(imp.profile.model.as_deref(), Some("gpt-5.1-codex"));
        assert_eq!(
            imp.profile.codex.unwrap().overrides["model_provider"],
            "\"openai\""
        );
    }

    #[test]
    fn lenient_mode_falls_back_for_openai_and_skips_others() {
        let d = tempfile::tempdir().unwrap();
        let cfgp = d.path().join("config.toml");
        std::fs::write(&cfgp, "model = \"gpt-5\"\n").unwrap();
        let a = InitArgs {
            out: d.path().join("t.yaml"),
            project_root: "r".into(),
            worktrees_root: "w".into(),
            claude_exe: "c".into(),
            git_bash: None,
            codex_exe: "x".into(),
            codex_config: Some(cfgp),
            codex_profiles: vec!["openai".into(), "foundry".into()],
            windows_sandbox: "unelevated".into(),
            codex_home: Some("D:\\codex-home".into()),
            lenient: true,
        };
        let (yaml, warnings) = render_config(&a).unwrap();
        assert!(yaml.contains("codex-openai:"));
        assert!(!yaml.contains("codex-foundry:"));
        assert!(yaml.contains("CODEX_HOME"));
        assert!(warnings.iter().any(|w| w.contains("SKIPPED")));
    }

    #[test]
    fn unknown_profile_lists_known_ones() {
        let e = import_codex_profile(CFG, "nope", "c", "unelevated")
            .unwrap_err()
            .to_string();
        assert!(e.contains("foundry") && e.contains("openai"), "{e}");
    }

    #[test]
    fn generated_yaml_loads_and_codex_argv_carries_overrides() {
        let d = tempfile::tempdir().unwrap();
        let cfgp = d.path().join("config.toml");
        std::fs::write(&cfgp, CFG).unwrap();
        let out = d.path().join("toryo.yaml");
        let a = InitArgs {
            out: out.clone(),
            project_root: "C:\\.thien\\m0\\calc".into(),
            worktrees_root: "C:\\.thien\\m0\\worktrees".into(),
            claude_exe: "C:\\Users\\x\\.local\\bin\\claude.exe".into(),
            git_bash: Some("C:\\Program Files\\Git\\bin\\bash.exe".into()),
            codex_exe: "C:\\Users\\x\\AppData\\Roaming\\npm\\codex.cmd".into(),
            codex_config: Some(cfgp),
            codex_profiles: vec!["openai".into(), "foundry".into()],
            windows_sandbox: "unelevated".into(),
            codex_home: None,
            lenient: false,
        };
        let (yaml, _) = render_config(&a).unwrap();
        std::fs::write(&out, yaml).unwrap();
        let eff = crate::config::Effective::load(&out, None).unwrap();
        let p = eff.profile("codex-foundry").unwrap();
        let l = crate::agents::codex::CodexLaunch {
            exe: p.exe.clone().into(),
            cwd: ".".into(),
            model: p.model.clone(),
            settings: p.codex.clone().unwrap(),
            output_schema: None,
            resume_thread: None,
            extra_args: vec![],
        };
        let argv = l.argv().join(" ");
        assert!(argv.contains("-m gpt-5-codex"));
        assert!(argv.contains(
            "-c model_providers.azure.base_url=\"https://my-res.openai.azure.com/openai/v1\""
        ));
        assert!(argv.contains("-c windows.sandbox=\"unelevated\""));
        assert!(argv.contains("--ignore-user-config"));
        assert_eq!(eff.repo.roles["implementer"].profile, "codex-foundry");
    }
}
