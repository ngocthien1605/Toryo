//! `toryo.yaml` + machine config (`%USERPROFILE%\.toryo\toryo.local.yaml`), plan §14 / R35 / R37.
//!
//! The effective configuration of a task is the repo `toryo.yaml` merged with the
//! machine config, serialised canonically (sorted keys) and hashed. From M3 on it
//! is frozen per task (`config.json` artifact); in the M0 spike it is loaded per run
//! and its hash is printed so results can be tied to a configuration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileKind {
    Claude,
    Codex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Billing {
    Subscription,
    Foundry,
    Api,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub root: String,
    pub worktrees_root: String,
    #[serde(default = "default_runs_root")]
    pub runs_root: String,
    #[serde(default = "default_base_branch")]
    pub base_branch: String,
}

fn default_runs_root() -> String {
    "%LOCALAPPDATA%\\toryo\\runs".into()
}
fn default_base_branch() -> String {
    "main".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CodexProvider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub env_key: String,
    #[serde(default = "default_wire_api")]
    pub wire_api: String,
}
fn default_wire_api() -> String {
    "responses".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CodexSettings {
    #[serde(default = "default_sandbox_mode")]
    pub sandbox_mode: String,
    /// R18: `unelevated` or `elevated`. Without it the Windows sandbox is off.
    pub windows_sandbox: Option<String>,
    pub provider: Option<CodexProvider>,
    pub project_doc_max_bytes: Option<u64>,
    pub model_reasoning_effort: Option<String>,
    /// Extra `-c key=value` overrides (value is a TOML literal), e.g. imported from a
    /// `[profiles.<name>]` of the user's ~/.codex/config.toml by `toryo spike init-config`.
    /// Passed on every exec AND exec resume, like everything else (R19).
    #[serde(default)]
    pub overrides: BTreeMap<String, String>,
}
fn default_sandbox_mode() -> String {
    "workspace-write".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub kind: ProfileKind,
    pub billing: Billing,
    pub exe: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// value = `credman:<target>` (Windows Credential Manager) or `env:<VAR>` (spike convenience).
    #[serde(default)]
    pub secret_env: BTreeMap<String, String>,
    #[serde(default)]
    pub codex: Option<CodexSettings>,
    /// Default model for this profile (roles may override).
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Role {
    pub profile: String,
    #[serde(default)]
    pub model: Option<String>,
    /// Profiles to switch to, in order, when this role's profile is out of quota
    /// (implementer only in the prototype).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Pipeline {
    #[serde(default)]
    pub max_fix_cycles: Option<u32>,
    #[serde(default)]
    pub budget_tokens: Option<u64>,
    #[serde(default)]
    pub budget_usd_est: Option<f64>,
    #[serde(default)]
    pub budget_weights: Option<BTreeMap<String, f64>>,
    #[serde(default)]
    pub unknown_turn_reservation_tokens: Option<serde_yaml::Value>,
    #[serde(default)]
    pub budget_live_interrupt: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default)]
    pub deny_paths: Vec<String>,
    #[serde(default)]
    pub protected_paths: Vec<String>,
    #[serde(default)]
    pub neutralize_untrusted_filters: bool,
    #[serde(default)]
    pub allow_submodules: bool,
    #[serde(default)]
    pub allow_codex_project_config: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verify {
    #[serde(default = "default_checkout")]
    pub checkout: String,
    #[serde(default = "default_boundary")]
    pub boundary: String,
    #[serde(default = "default_verify_root")]
    pub verify_root: String,
    #[serde(default = "default_true")]
    pub final_clean: bool,
}
impl Default for Verify {
    fn default() -> Self {
        Self {
            checkout: default_checkout(),
            boundary: default_boundary(),
            verify_root: default_verify_root(),
            final_clean: true,
        }
    }
}
fn default_checkout() -> String {
    "clean".into()
}
fn default_boundary() -> String {
    "low_integrity".into()
}
fn default_verify_root() -> String {
    "%LOCALAPPDATA%\\toryo-verify".into()
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub name: String,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub timeout_min: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Commands {
    #[serde(default)]
    pub verify: Vec<CommandSpec>,
}

/// ProjectContextLoader settings (plan §7).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ContextCfg {
    /// Size budget per role in KB (`planner`, `implementer`, `reviewer`); default 40.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub budget_kb: BTreeMap<String, u64>,
}

impl ContextCfg {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Timeouts {
    #[serde(default)]
    pub agent_stall_min: Option<u64>,
    #[serde(default)]
    pub agent_stage_max_min: Option<u64>,
}

/// Repo-level `toryo.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    pub version: u32,
    pub project: Project,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    #[serde(default)]
    pub roles: BTreeMap<String, Role>,
    #[serde(default)]
    pub pipeline: Pipeline,
    #[serde(default)]
    pub policy: Policy,
    #[serde(default)]
    pub verify: Verify,
    #[serde(default)]
    pub commands: Commands,
    #[serde(default)]
    pub timeouts: Timeouts,
    #[serde(default, skip_serializing_if = "ContextCfg::is_default")]
    pub context: ContextCfg,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TrustedFilter {
    pub process: Option<String>,
    pub clean: Option<String>,
    pub smudge: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub exe_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct MachineGit {
    #[serde(default)]
    pub trusted_filters: BTreeMap<String, TrustedFilter>,
}

/// Machine config, outside every repository (R35/R37).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct MachineConfig {
    #[serde(default)]
    pub roles: BTreeMap<String, Role>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    #[serde(default)]
    pub git: MachineGit,
}

/// Effective configuration = repo config with machine overrides applied.
#[derive(Debug, Clone, Serialize)]
pub struct Effective {
    pub repo: RepoConfig,
    pub trusted_filters: BTreeMap<String, TrustedFilter>,
    #[serde(skip)]
    pub repo_path: PathBuf,
    #[serde(skip)]
    pub machine_path: Option<PathBuf>,
}

impl Effective {
    pub fn load(repo_path: &Path, machine_path: Option<&Path>) -> Result<Self> {
        let text = std::fs::read_to_string(repo_path)
            .with_context(|| format!("reading {}", repo_path.display()))?;
        let mut repo: RepoConfig = serde_yaml::from_str(&text)
            .with_context(|| format!("parsing {}", repo_path.display()))?;
        if repo.version != 1 {
            bail!("unsupported toryo.yaml version {}", repo.version);
        }
        let mut trusted_filters = BTreeMap::new();
        let mut used_machine = None;
        if let Some(mp) = machine_path {
            if mp.exists() {
                let mt = std::fs::read_to_string(mp)
                    .with_context(|| format!("reading {}", mp.display()))?;
                let machine: MachineConfig = serde_yaml::from_str(&mt)
                    .with_context(|| format!("parsing {}", mp.display()))?;
                for (k, v) in machine.roles {
                    repo.roles.insert(k, v);
                }
                for (k, v) in machine.profiles {
                    repo.profiles.insert(k, v);
                }
                trusted_filters = machine.git.trusted_filters;
                used_machine = Some(mp.to_path_buf());
            }
        }
        for (name, role) in &repo.roles {
            for p in std::iter::once(&role.profile).chain(&role.fallback) {
                if !repo.profiles.contains_key(p) {
                    bail!("role `{name}` refers to unknown profile `{p}`");
                }
            }
        }
        Ok(Self {
            repo,
            trusted_filters,
            repo_path: repo_path.to_path_buf(),
            machine_path: used_machine,
        })
    }

    /// Rebuild the configuration frozen by `toryo task create` (R35): a task never re-reads
    /// `toryo.yaml`; the frozen copy hashes to the same `config_sha256`.
    pub fn from_frozen(canonical_json: &str, origin: &Path) -> Result<Self> {
        #[derive(Deserialize)]
        struct Frozen {
            repo: RepoConfig,
            #[serde(default)]
            trusted_filters: BTreeMap<String, TrustedFilter>,
        }
        let f: Frozen = serde_json::from_str(canonical_json).context("parsing the frozen config.json")?;
        Ok(Self {
            repo: f.repo,
            trusted_filters: f.trusted_filters,
            repo_path: origin.to_path_buf(),
            machine_path: None,
        })
    }

    /// Canonical JSON (sorted keys, no whitespace) — what gets frozen and hashed.
    pub fn canonical_json(&self) -> String {
        let v = serde_json::to_value(self).expect("config serialises");
        // serde_json::Map is a BTreeMap without the preserve_order feature: keys are sorted.
        serde_json::to_string(&v).expect("json")
    }

    pub fn sha256(&self) -> String {
        hex::encode(Sha256::digest(self.canonical_json().as_bytes()))
    }

    pub fn profile(&self, name: &str) -> Result<&Profile> {
        self.repo
            .profiles
            .get(name)
            .with_context(|| format!("unknown profile `{name}`"))
    }
}

/// `%USERPROFILE%\.toryo\toryo.local.yaml` (or `~/.toryo/toryo.local.yaml`).
pub fn default_machine_config_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".toryo").join("toryo.local.yaml"))
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// `%LOCALAPPDATA%\toryo` (or `$XDG_DATA_HOME/toryo`, `~/.local/share/toryo`).
pub fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("TORYO_DATA_DIR") {
        return PathBuf::from(p);
    }
    if let Some(p) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(p).join("toryo");
    }
    if let Some(p) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(p).join("toryo");
    }
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local")
        .join("share")
        .join("toryo")
}

/// Expand `%VAR%` (Windows style) and `${VAR}` / `$VAR`-free paths. Unknown variables are an error.
pub fn expand_vars(s: &str) -> Result<String> {
    let re = regex::Regex::new(r"%([A-Za-z_][A-Za-z0-9_]*)%").expect("regex");
    let mut err = None;
    let out = re.replace_all(s, |c: &regex::Captures| match std::env::var(&c[1]) {
        Ok(v) => v,
        Err(_) => {
            // Fall back for the two locations toryo uses when running on non-Windows hosts.
            match &c[1] {
                "LOCALAPPDATA" => data_dir()
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                "USERPROFILE" => home_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                other => {
                    err = Some(other.to_string());
                    String::new()
                }
            }
        }
    });
    if let Some(v) = err {
        bail!("environment variable %{v}% is not set (in `{s}`)");
    }
    Ok(out.into_owned())
}

/// Resolve a `secret_env` reference. Never logs the value.
pub fn resolve_secret(reference: &str) -> Result<String> {
    if let Some(var) = reference.strip_prefix("env:") {
        return std::env::var(var).with_context(|| format!("secret env var `{var}` is not set"));
    }
    if let Some(target) = reference.strip_prefix("credman:") {
        return resolve_credman(target);
    }
    bail!("unsupported secret reference `{reference}` (use credman:<target> or env:<VAR>)")
}

#[cfg(windows)]
fn resolve_credman(target: &str) -> Result<String> {
    // `toryo/claude-max-oauth` → service "toryo", user "claude-max-oauth"
    let (service, user) = target.split_once('/').unwrap_or(("toryo", target));
    let entry = keyring::Entry::new(service, user).context("opening Credential Manager entry")?;
    entry
        .get_password()
        .with_context(|| format!("reading Credential Manager entry `{target}`"))
}

#[cfg(not(windows))]
fn resolve_credman(target: &str) -> Result<String> {
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
    use super::*;

    const SAMPLE: &str = r#"
version: 1
project:
  root: D:\D365-Git
  worktrees_root: D:\D365-worktrees
profiles:
  claude-max:
    kind: claude
    billing: subscription
    exe: C:\Users\toryo\.local\bin\claude.exe
  codex-foundry:
    kind: codex
    billing: foundry
    exe: C:\tools\codex\codex.exe
    secret_env:
      AZURE_OPENAI_API_KEY: env:AZURE_OPENAI_API_KEY
    codex:
      windows_sandbox: unelevated
      provider: { id: foundry, name: Azure Foundry, base_url: https://x.openai.azure.com/openai/v1, env_key: AZURE_OPENAI_API_KEY }
roles:
  planner: { profile: claude-max, model: opus }
  implementer: { profile: codex-foundry }
policy:
  deny_paths: ["**/ZZZ/**"]
"#;

    #[test]
    fn loads_and_hashes_stably() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("toryo.yaml");
        std::fs::write(&p, SAMPLE).unwrap();
        let a = Effective::load(&p, None).unwrap();
        let b = Effective::load(&p, None).unwrap();
        assert_eq!(a.sha256(), b.sha256());
        assert_eq!(a.repo.verify.boundary, "low_integrity");
        // the frozen copy (R35) round-trips to the same digest
        let f = Effective::from_frozen(&a.canonical_json(), &p).unwrap();
        assert_eq!(f.sha256(), a.sha256());
    }

    #[test]
    fn machine_config_overrides_roles_and_changes_hash() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("toryo.yaml");
        std::fs::write(&p, SAMPLE).unwrap();
        let m = d.path().join("machine.yaml");
        std::fs::write(
            &m,
            "roles:\n  planner: { profile: codex-foundry }\ngit:\n  trusted_filters:\n    lfs: { process: 'git-lfs filter-process', required: true }\n",
        )
        .unwrap();
        let base = Effective::load(&p, None).unwrap();
        let eff = Effective::load(&p, Some(&m)).unwrap();
        assert_eq!(eff.repo.roles["planner"].profile, "codex-foundry");
        assert!(eff.trusted_filters.contains_key("lfs"));
        assert_ne!(base.sha256(), eff.sha256());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("toryo.yaml");
        std::fs::write(&p, format!("{SAMPLE}\nbogus: 1\n")).unwrap();
        assert!(Effective::load(&p, None).is_err());
    }

    #[test]
    fn role_with_unknown_profile_is_rejected() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("toryo.yaml");
        std::fs::write(
            &p,
            SAMPLE.replace("profile: claude-max, model: opus", "profile: nope"),
        )
        .unwrap();
        assert!(Effective::load(&p, None).is_err());
    }

    #[test]
    fn secret_reference_forms() {
        std::env::set_var("TORYO_TEST_SECRET_X", "v");
        assert_eq!(resolve_secret("env:TORYO_TEST_SECRET_X").unwrap(), "v");
        assert!(resolve_secret("plain").is_err());
    }
}
