//! `toryo.yaml` + machine config (`%USERPROFILE%\.toryo\toryo.local.yaml`), plan §14 / R35 / R37.
//!
//! The effective configuration of a task is the repo `toryo.yaml` merged with the
//! machine config, serialised canonically (sorted keys) and hashed. Agent profiles come
//! only from the machine config (R60): a repository file that defines `profiles:` or
//! `toolchains:` does not load. From M3 on it
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
    /// M12-08 (X19): the branch `task create` starts from when the file sets it; unset: the
    /// checked-out branch. Written as `main` into the frozen configuration when unset, as
    /// before (the digest of an existing configuration stays; the engine never reads it there).
    #[serde(default, serialize_with = "serialize_base_branch")]
    pub base_branch: Option<String>,
}

fn serialize_base_branch<S: serde::Serializer>(
    v: &Option<String>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(v.as_deref().unwrap_or("main"))
}

fn default_runs_root() -> String {
    "%LOCALAPPDATA%\\toryo\\runs".into()
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// The same defaults as a `codex:` block that sets nothing: a Codex profile without a
/// `codex:` block runs with `sandbox_mode = workspace-write`, not an empty value.
impl Default for CodexSettings {
    fn default() -> Self {
        Self {
            sandbox_mode: default_sandbox_mode(),
            windows_sandbox: None,
            provider: None,
            project_doc_max_bytes: None,
            model_reasoning_effort: None,
            overrides: BTreeMap::new(),
        }
    }
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
    /// (implementer only in the prototype). Absent: the engine falls back to the planner's
    /// profile (R49); `[]`: no fallback (EC-23).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<Vec<String>>,
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

/// A `commands.verify[]` item: a repository program (`program` + `args`) or, since R59, a
/// built-in kind (`tool: xpp` + `metadata` + `projects`) whose command line toryo writes from
/// the machine's toolchain. [`problems`] makes sure an item is exactly one of the two. The
/// new keys are serialised only when set, so a configuration without them keeps its hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub timeout_min: Option<u64>,
    /// M11: a gate's `timeout` (`30s`, `20m`, `1h`) in seconds; wins over `timeout_min`.
    /// Serialised only when set, so a configuration without gates keeps its hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_sec: Option<u64>,
    /// R59: a built-in verify kind; `xpp` is the only one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// `tool: xpp`: the X++ source folder, relative to the repository (→ `MetadataDirectory`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,
    /// `tool: xpp`: one X++ project per package, relative to the repository, in build order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<String>,
    /// A `program` command: how to read its output. `msbuild` parses MSBuild diagnostic lines
    /// (`file(line,col): error CODE: message`) so the fix feedback names file, line and code
    /// (`dotnet build`, a script wrapping MSBuild); `none` (the default) sends only the tail.
    /// Serialised only when set, so a configuration without it keeps its hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<String>,
}

/// The built-in verify kinds (`commands.verify[].tool`, R59).
pub const VERIFY_TOOLS: &[&str] = &["xpp"];

/// The output readers of a `program` command (`commands.verify[].diagnostics`).
pub const DIAGNOSTICS_FORMATS: &[&str] = &["msbuild", "none"];

/// What a verify command runs, as [`CommandSpec::kind`] reads a validated item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind<'a> {
    /// A program (a PATH name, or a path in or outside the repository) with its arguments.
    Program {
        program: &'a str,
        args: &'a [String],
    },
    /// `tool: xpp` (R59): MSBuild on each project, with the machine's `toolchains.xpp`.
    Xpp {
        metadata: &'a str,
        projects: &'a [String],
    },
}

impl CommandSpec {
    /// A `program` command.
    pub fn program(name: &str, program: &str, args: &[&str], timeout_min: Option<u64>) -> Self {
        Self {
            name: name.into(),
            program: Some(program.into()),
            args: args.iter().map(|a| a.to_string()).collect(),
            timeout_min,
            timeout_sec: None,
            tool: None,
            metadata: None,
            projects: Vec::new(),
            diagnostics: None,
        }
    }

    /// A `tool: xpp` command (R59).
    pub fn xpp(name: &str, metadata: &str, projects: &[&str], timeout_min: Option<u64>) -> Self {
        Self {
            name: name.into(),
            program: None,
            args: Vec::new(),
            timeout_min,
            timeout_sec: None,
            tool: Some("xpp".into()),
            metadata: Some(metadata.into()),
            projects: projects.iter().map(|p| p.to_string()).collect(),
            diagnostics: None,
        }
    }

    /// True when a `program` command's output is read as MSBuild diagnostics.
    pub fn reads_msbuild(&self) -> bool {
        self.diagnostics.as_deref() == Some("msbuild")
    }

    /// What this command runs. Load-time validation guarantees exactly one of `program` and
    /// `tool`; a frozen configuration from before R59 only has `program`.
    pub fn kind(&self) -> CommandKind<'_> {
        match &self.tool {
            Some(_) => CommandKind::Xpp {
                metadata: self.metadata.as_deref().unwrap_or_default(),
                projects: &self.projects,
            },
            None => CommandKind::Program {
                program: self.program.as_deref().unwrap_or_default(),
                args: &self.args,
            },
        }
    }

    /// True for a built-in `tool: xpp` command.
    pub fn is_xpp(&self) -> bool {
        matches!(self.kind(), CommandKind::Xpp { .. })
    }
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

/// The repository `toryo.yaml` as written (review C1): exactly the keys a repository may set.
/// It has no `profiles` / `toolchains` fields, so a key that slips past the raw-text check of
/// [`machine_only_part`] (a YAML tag, an alias) is still an unknown field, never a profile.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepoFile {
    version: u32,
    project: Project,
    #[serde(default)]
    roles: BTreeMap<String, Role>,
    #[serde(default)]
    pipeline: Pipeline,
    #[serde(default)]
    policy: Policy,
    #[serde(default)]
    verify: Verify,
    #[serde(default)]
    commands: Commands,
    #[serde(default)]
    timeouts: Timeouts,
    #[serde(default)]
    context: ContextCfg,
}

impl From<RepoFile> for RepoConfig {
    fn from(f: RepoFile) -> Self {
        Self {
            version: f.version,
            project: f.project,
            profiles: BTreeMap::new(),
            roles: f.roles,
            pipeline: f.pipeline,
            policy: f.policy,
            verify: f.verify,
            commands: f.commands,
            timeouts: f.timeouts,
            context: f.context,
        }
    }
}

/// Repo-level `toryo.yaml`, and — once merged — the effective configuration of a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    pub version: u32,
    pub project: Project,
    /// The merged view only: the repository file may not define profiles (R60, checked on
    /// its raw text by [`Effective::load`]); they come from the machine config and are
    /// frozen with the task, so `from_frozen` reads them here.
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

/// `toolchains.xpp` (R59): the X++ build toolchain of this machine — MSBuild and the five
/// CI/CD NuGet packages, extracted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct XppToolchain {
    /// MSBuild.exe (`%VAR%` expanded); `doctor` proposes the path `vswhere` finds.
    pub msbuild: String,
    /// The folder holding the five CI/CD packages, one folder each (`<id>` or `<id>.<version>`).
    pub nugets: String,
    /// Folders of binary packages the models reference (e.g. ISV), in this order.
    #[serde(default)]
    pub extra_references: Vec<String>,
    /// `xppbp.exe` after the compile; refused until V08-VERIFY 4 (M9 B-later).
    #[serde(default)]
    pub bp_check: bool,
    /// The UDE's `PackagesLocalDirectory`, for `doctor`'s version check only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ude_metadata: Option<String>,
}

/// `toolchains:` of the machine config (R59/R60): machine facts that `tool:` verify commands
/// use. Frozen with the task like the profiles.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Toolchains {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xpp: Option<XppToolchain>,
}

impl Toolchains {
    pub fn is_empty(&self) -> bool {
        self.xpp.is_none()
    }

    /// The names of the defined toolchains.
    pub fn names(&self) -> Vec<&'static str> {
        self.xpp.iter().map(|_| "xpp").collect()
    }
}

/// Machine config, outside every repository (R35/R37).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct MachineConfig {
    #[serde(default)]
    pub roles: BTreeMap<String, Role>,
    /// M11: the new spelling of `roles:` — this machine's seat overrides (same fields). Both
    /// in one file is an error ([`MachineConfig::load`]).
    #[serde(default)]
    pub seats: BTreeMap<String, Role>,
    /// M11-R8: the file spelled its seat overrides `seats:` (they are moved to `roles`) — for
    /// the messages.
    #[serde(skip)]
    pub seats_spelling: bool,
    /// M11-D1: this machine's shared skills folder (`<skills_dir>/<name>/SKILL.md`), looked up
    /// after the repository's `.toryo/skills/`.
    #[serde(default)]
    pub skills_dir: Option<String>,
    /// M11-E3: the commands of the org chart's `kind: environment` gates. They run with the
    /// owner's normal rights, so they are defined only here, out of every agent's reach.
    #[serde(default)]
    pub environment_gates: BTreeMap<String, EnvGate>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    /// R59/R60: only here, never in the repository file.
    #[serde(default)]
    pub toolchains: Toolchains,
    #[serde(default)]
    pub git: MachineGit,
    /// M10-D1: how the owner is told that a task needs them. A machine setting like the
    /// profiles' origins: not frozen with a task and not hashed.
    #[serde(default)]
    pub notify: Notify,
    /// Review H2 (M11-A2): the machine's floor for the verify boundary and its verify root.
    #[serde(default)]
    pub verify: MachineVerify,
}

/// `environment_gates.<name>.confirm` (M12-07, X17): does the gate ask the owner before it
/// deploys? `never` (the default, as before), `first` (once per task) or `always` (once per
/// checkpoint commit). The question is a machine stop before any gate runs; a plain `toryo
/// resume` is the "yes".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confirm {
    #[default]
    Never,
    First,
    Always,
}

impl Confirm {
    pub fn is_never(&self) -> bool {
        *self == Confirm::Never
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Confirm::Never => "never",
            Confirm::First => "first",
            Confirm::Always => "always",
        }
    }
}

/// `environment_gates.<name>` in the machine config (M11-E3): the command an org chart's
/// `kind: environment` gate runs, with the owner's normal token, in an empty working directory
/// (M11-R2); it deploys from a fresh export of the checkpoint commit (M11-R9, X6).
/// Frozen and hashed with each task that uses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvGate {
    /// An absolute path or a bare PATH name.
    pub program: String,
    /// Every path-like argument absolute and outside the repository, the worktrees and the
    /// verify root.
    #[serde(default)]
    pub args: Vec<String>,
    /// The machine-wide lock: one environment gate per lock name runs at a time.
    pub lock: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// `credman:<target>` or `env:<VAR>` references, resolved when the gate runs.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_env: BTreeMap<String, String>,
    /// M12-07 (X17): ask the owner before it deploys. Left out of the frozen configuration when
    /// `never`, so an existing configuration keeps its hash.
    #[serde(default, skip_serializing_if = "Confirm::is_never")]
    pub confirm: Confirm,
}

/// `verify:` in the machine config (review H2, M11-A2): the repository chooses the sandbox its
/// own code runs in only above this floor. Frozen and hashed with a task only when set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineVerify {
    /// The lowest `verify.boundary` a repository may ask for: `low_integrity` (the default
    /// when unset) or `none` (this machine trusts the repositories' verify code).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_boundary: Option<String>,
    /// Replaces the repository's `verify.verify_root`: where the Low-labelled checkouts live
    /// and what toryo empties and removes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_root: Option<String>,
}

impl MachineVerify {
    pub fn is_empty(&self) -> bool {
        self.min_boundary.is_none() && self.verify_root.is_none()
    }
}

/// `notify:` in the machine config (M10-D1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notify {
    /// A Windows toast at a gate, an escalation, a pause and the end of a run. Default off.
    #[serde(default)]
    pub toast: bool,
}

impl MachineConfig {
    /// Read and parse a machine config file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = crate::textfile::read_text(path)?;
        // M11-B3: an unknown key gets the friendly form with a "did you mean" hint
        let mut m: Self =
            serde_yaml::from_str(&text).map_err(|e| {
                match crate::team::friendly_serde(&e.to_string()) {
                    Some(f) => anyhow::anyhow!("{}: {f}", path.display()),
                    None => anyhow::Error::new(e).context(format!("parsing {}", path.display())),
                }
            })?;
        // M11: `seats:` is the new spelling of `roles:` — one of the two
        if !m.seats.is_empty() {
            if !m.roles.is_empty() {
                bail!(
                    "{}: both `roles:` and `seats:` are set — `seats:` is the new spelling of `roles:` (same fields); keep one of them",
                    path.display()
                );
            }
            m.roles = std::mem::take(&mut m.seats);
            m.seats_spelling = true;
        }
        Ok(m)
    }
}

/// Where an entry of the effective configuration came from (R60, shown by `doctor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// The repository `toryo.yaml`.
    Repo,
    /// The machine config.
    Machine,
    /// Set in the repository file and replaced by the machine config (roles).
    RepoOverriddenByMachine,
}

impl Origin {
    pub fn describe(self) -> &'static str {
        match self {
            Origin::Repo => "repo",
            Origin::Machine => "machine",
            Origin::RepoOverriddenByMachine => "repo, overridden by machine",
        }
    }
}

/// The [`Origin`] of each profile and role of an effective configuration. Not part of the
/// canonical JSON (so not of the config hash); empty for a frozen configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Origins {
    pub profiles: BTreeMap<String, Origin>,
    pub roles: BTreeMap<String, Origin>,
    pub toolchains: BTreeMap<String, Origin>,
    /// M11-A2: `boundary`, `min_boundary` and `verify_root` (the machine's replaces the
    /// repository's).
    pub verify: BTreeMap<String, Origin>,
    /// M11-B2: each gate of the org chart — `Repo` (a check gate, or an environment gate this
    /// machine does not define), `Machine` (an environment gate whose command is the machine's).
    pub gates: BTreeMap<String, Origin>,
    /// M11-B2: the machine's `skills_dir`, in org chart mode.
    pub skills_dir: Option<Origin>,
    /// M11-R8: how the machine config spelled its role overrides (`seats` or `roles`).
    pub machine_roles_key: Option<&'static str>,
}

/// M11-R14 (X9): an entry script of a gate (a verify command) that is build output — missing at
/// the task's base commit and ignored by the repository's ignore rules, so in the clean verify
/// checkout only a gate can create it. It may exist in the task worktree (the implementer builds
/// there); it may never be committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildEntry {
    /// The gate (verify command) that runs it.
    pub gate: String,
    /// Repository-relative, `/`.
    pub path: String,
}

/// Effective configuration = repo config with machine overrides applied.
#[derive(Debug, Clone, Serialize)]
pub struct Effective {
    pub repo: RepoConfig,
    pub trusted_filters: BTreeMap<String, TrustedFilter>,
    /// R59: the machine's toolchains, frozen and hashed with the task; left out of the
    /// canonical JSON when empty, so a configuration without them keeps its hash.
    #[serde(skip_serializing_if = "Toolchains::is_empty")]
    pub toolchains: Toolchains,
    /// M11-A2: the machine's `verify:` floor and root, frozen and hashed with the task; left
    /// out of the canonical JSON when unset, so a configuration without it keeps its hash.
    #[serde(skip_serializing_if = "MachineVerify::is_empty")]
    pub machine_verify: MachineVerify,
    /// M11-B2: org chart mode — the parsed `.toryo/` (gates, limits, checks, role files,
    /// skill roots), frozen and hashed with the task; `None` (left out) in `toryo.yaml`-only
    /// mode, so an M10 configuration keeps its hash.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<crate::team::Team>,
    /// M11-R14 (X9): the entry scripts admitted as build output at `task create` (missing at
    /// the base commit and ignored by the repository) — set at admission, frozen and hashed
    /// with the task; left out of the canonical JSON when empty, so every other configuration
    /// keeps its hash. Never read from a file.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub build_entries: Vec<BuildEntry>,
    #[serde(skip)]
    pub repo_path: PathBuf,
    #[serde(skip)]
    pub machine_path: Option<PathBuf>,
    /// Where each profile and role came from (R60); not hashed.
    #[serde(skip)]
    pub origins: Origins,
    /// M10-D1: the machine's `notify:`; not frozen, not hashed.
    #[serde(skip)]
    pub notify: Notify,
    /// M11-B2: the files read, with their SHA-256 (`config show`); empty when frozen.
    #[serde(skip)]
    pub files: Vec<crate::team::FileRead>,
}

impl Effective {
    pub fn load(repo_path: &Path, machine_path: Option<&Path>) -> Result<Self> {
        let bytes =
            std::fs::read(repo_path).with_context(|| format!("reading {}", repo_path.display()))?;
        let text = crate::textfile::decode(repo_path, &bytes)?;
        // R60 (ARCH-05): the repository file never defines an agent's program, environment
        // or secrets — checked on the raw text, before anything is merged
        if let Some(part) =
            machine_only_part(&text).with_context(|| format!("parsing {}", repo_path.display()))?
        {
            bail!(
                "{}",
                part.refusal(repo_path, &machine_config_hint(machine_path))
            );
        }
        // C1: a YAML tag hides a key from lookups by name — refused before the typed parse
        let doc: serde_yaml::Value = serde_yaml::from_str(&text)
            .with_context(|| format!("parsing {}", repo_path.display()))?;
        refuse_tags(&doc, repo_path)?;
        let file: RepoFile = serde_yaml::from_str(&text)
            .with_context(|| format!("parsing {}", repo_path.display()))?;
        let mut repo = RepoConfig::from(file);
        if repo.version != 1 {
            bail!("unsupported toryo.yaml version {}", repo.version);
        }
        let mut files = vec![crate::team::FileRead::of("toryo.yaml", repo_path, &bytes)];
        let machine = match machine_path {
            Some(mp) if mp.exists() => {
                let b = std::fs::read(mp).with_context(|| format!("reading {}", mp.display()))?;
                files.push(crate::team::FileRead::of("machine config", mp, &b));
                Some((mp.to_path_buf(), MachineConfig::load(mp)?))
            }
            _ => None,
        };
        // M11-B2: org chart mode when `<project.root>/.toryo/toryo-orgchart.yaml` exists
        let mut team = None;
        let mut team_problems = Vec::new();
        if let Some(root) = crate::team::org_chart_root(&repo.project.root) {
            let loaded = crate::team::load(
                &root,
                repo_path,
                &doc,
                machine.as_ref().map(|(_, m)| m),
                &machine_config_hint(machine_path),
            )?;
            repo.roles = loaded.roles;
            repo.commands.verify = loaded.verify;
            repo.pipeline.max_fix_cycles = loaded.team.limits.max_fix_cycles;
            files.extend(loaded.files);
            team_problems = loaded.problems;
            team = Some(loaded.team);
        }
        let mut trusted_filters = BTreeMap::new();
        let mut toolchains = Toolchains::default();
        let mut used_machine = None;
        let mut origins = Origins::default();
        let mut notify = Notify::default();
        let mut machine_verify = MachineVerify::default();
        for k in repo.roles.keys() {
            origins.roles.insert(k.clone(), Origin::Repo);
        }
        for k in ["boundary", "verify_root"] {
            origins.verify.insert(k.into(), Origin::Repo);
        }
        if let Some((mp, machine)) = machine {
            origins.machine_roles_key = Some(if machine.seats_spelling {
                "seats"
            } else {
                "roles"
            });
            for (k, mut v) in machine.roles {
                // org chart mode: a machine seat has the org chart's fields — an omitted
                // fallback is none (D7), never the planner's profile (group B review)
                if team.is_some() && v.fallback.is_none() {
                    v.fallback = Some(Vec::new());
                }
                let o = if repo.roles.contains_key(&k) {
                    Origin::RepoOverriddenByMachine
                } else {
                    Origin::Machine
                };
                origins.roles.insert(k.clone(), o);
                repo.roles.insert(k, v);
            }
            for (k, v) in machine.profiles {
                origins.profiles.insert(k.clone(), Origin::Machine);
                repo.profiles.insert(k, v);
            }
            trusted_filters = machine.git.trusted_filters;
            toolchains = machine.toolchains;
            notify = machine.notify;
            machine_verify = machine.verify;
            if machine_verify.min_boundary.is_some() {
                origins
                    .verify
                    .insert("min_boundary".into(), Origin::Machine);
            }
            if machine_verify.verify_root.is_some() {
                origins
                    .verify
                    .insert("verify_root".into(), Origin::RepoOverriddenByMachine);
            }
            for name in toolchains.names() {
                origins.toolchains.insert(name.into(), Origin::Machine);
            }
            if team.is_some() && machine.skills_dir.is_some() {
                origins.skills_dir = Some(Origin::Machine);
            }
            used_machine = Some(mp);
        }
        if let Some(t) = &team {
            for (name, g) in &t.gates {
                let o = match g.decl.kind {
                    crate::team::GateType::Check => Origin::Repo,
                    // declared in the repository, its command from the machine config
                    crate::team::GateType::Environment if g.machine.is_some() => Origin::Machine,
                    crate::team::GateType::Environment => Origin::Repo,
                };
                origins.gates.insert(name.clone(), o);
            }
        }
        // in org chart mode the validation pass reports unknown seat profiles with the others
        for (name, role) in repo.roles.iter().filter(|_| team.is_none()) {
            for p in std::iter::once(&role.profile).chain(role.fallback.iter().flatten()) {
                if !repo.profiles.contains_key(p) {
                    bail!(
                        "role `{name}` refers to unknown profile `{p}` — define it under `profiles:` in the machine config ({}) (R60)",
                        machine_config_hint(machine_path)
                    );
                }
            }
        }
        let eff = Self {
            repo,
            trusted_filters,
            toolchains,
            machine_verify,
            team,
            build_entries: Vec::new(),
            repo_path: repo_path.to_path_buf(),
            machine_path: used_machine,
            origins,
            notify,
            files,
        };
        eff.validate_with(team_problems)
            .with_context(|| match &eff.machine_path {
                Some(m) => format!("{} (with {})", repo_path.display(), m.display()),
                None => repo_path.display().to_string(),
            })?;
        Ok(eff)
    }

    /// The semantic checks serde cannot express (DES-05, EC-23, ARCH-05), all problems at
    /// once: unknown role keys, enum-like strings outside their documented set, and
    /// `codex.overrides` that would re-set what toryo sets itself. A frozen configuration
    /// ([`Effective::from_frozen`]) is not re-checked: it is what its task was created with.
    pub fn validate(&self) -> Result<()> {
        self.validate_with(Vec::new())
    }

    /// [`Self::validate`] with the problems the org chart loader found (M11-B2), first.
    fn validate_with(&self, mut problems: Vec<String>) -> Result<()> {
        let hint = machine_config_hint(self.machine_path.as_deref());
        problems.extend(self::problems(
            &self.repo,
            &self.toolchains,
            &hint,
            self.team.is_some(),
            &self.origins,
        ));
        if let Some(b) = &self.machine_verify.min_boundary {
            let key = format!("verify.min_boundary (machine config {hint})");
            one_of(&mut problems, &key, b, VERIFY_BOUNDARIES);
        }
        // M11-B3: the org chart's cross-references, kinds and order
        if let Some(root) = crate::team::org_chart_root(&self.repo.project.root) {
            problems.extend(crate::team::problems(self, &root, &hint));
        }
        if self.team.is_some() {
            // M11-B2: a machine seat override cannot give the planner or reviewer a fallback
            for seat in ["planner", "reviewer"] {
                let fb = self.repo.roles.get(seat).and_then(|r| r.fallback.as_ref());
                if fb.is_some_and(|f| !f.is_empty())
                    && self.origins.roles.get(seat) != Some(&Origin::Repo)
                {
                    problems.push(format!(
                        "seats.{seat}.fallback (machine config {hint}): a fallback on the {seat} is not supported yet — only the implementer may switch profiles (R49); remove it"
                    ));
                }
            }
        }
        if problems.is_empty() {
            return Ok(());
        }
        bail!("invalid configuration:\n  - {}", problems.join("\n  - "))
    }

    /// Rebuild the configuration frozen by `toryo task create` (R35): a task never re-reads
    /// `toryo.yaml`; the frozen copy hashes to the same `config_sha256`. Org chart mode comes
    /// back from the frozen `team` section, without reading `.toryo/` again (M11-B2).
    pub fn from_frozen(canonical_json: &str, origin: &Path) -> Result<Self> {
        #[derive(Deserialize)]
        struct Frozen {
            repo: RepoConfig,
            #[serde(default)]
            trusted_filters: BTreeMap<String, TrustedFilter>,
            #[serde(default)]
            toolchains: Toolchains,
            #[serde(default)]
            machine_verify: MachineVerify,
            #[serde(default)]
            team: Option<crate::team::Team>,
            #[serde(default)]
            build_entries: Vec<BuildEntry>,
        }
        let f: Frozen =
            serde_json::from_str(canonical_json).context("parsing the frozen config.json")?;
        Ok(Self {
            repo: f.repo,
            trusted_filters: f.trusted_filters,
            toolchains: f.toolchains,
            machine_verify: f.machine_verify,
            team: f.team,
            build_entries: f.build_entries,
            repo_path: origin.to_path_buf(),
            machine_path: None,
            origins: Origins::default(),
            notify: Notify::default(),
            files: Vec::new(),
        })
    }

    /// Canonical JSON (sorted keys, no whitespace) — what gets frozen and hashed.
    pub fn canonical_json(&self) -> String {
        let v = serde_json::to_value(self).expect("config serialises");
        // serde_json::Map is a BTreeMap without the preserve_order feature: keys are sorted.
        serde_json::to_string(&v).expect("json")
    }

    /// [`Self::canonical_json`] for a configuration that is about to be frozen (`task create`,
    /// `task config --reload`): an error unless the text reads back as this configuration
    /// (R23-02). JSON has no place for some values (a non-finite number becomes `null`): a frozen
    /// text that cannot be read back would leave a task that no command but `cleanup` can open.
    pub fn frozen_json(&self) -> Result<String> {
        let text = self.canonical_json();
        let back = Self::from_frozen(&text, &self.repo_path)
            .context("the configuration cannot be frozen: it does not read back")?;
        if back.canonical_json() != text {
            bail!("the configuration cannot be frozen: it does not read back as itself");
        }
        Ok(text)
    }

    /// M12-08 (X20): the keys of the repository file that are accepted and that no engine code
    /// reads — set to something other than their default. Each is announced by `config show`,
    /// `doctor` and `task create` as "accepted but has no effect in this version". The test
    /// `keys_without_effect_are_read_nowhere` fails when one of them starts being used.
    pub fn keys_without_effect(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.repo.project.runs_root != default_runs_root() {
            out.push("project.runs_root");
        }
        if !self.repo.verify.final_clean {
            out.push("verify.final_clean");
        }
        if self.repo.pipeline.budget_usd_est.is_some() {
            out.push("pipeline.budget_usd_est");
        }
        out
    }

    /// The line for one key of [`Self::keys_without_effect`].
    pub fn no_effect_line(key: &str) -> String {
        format!("`{key}` is accepted but has no effect in this version")
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

/// The pipeline's roles: the keys of `roles` and `context.budget_kb`.
fn role_names() -> [&'static str; 3] {
    use crate::domain::Role as R;
    [R::Planner, R::Implementer, R::Reviewer].map(R::as_str)
}

/// A problem for `key` unless `value` is one of `valid` (the message lists them).
fn one_of(out: &mut Vec<String>, key: &str, value: &str, valid: &[&str]) {
    if !valid.contains(&value) {
        let valid = valid.join(", ");
        out.push(format!(
            "{key}: `{value}` is not a valid value (valid: {valid})"
        ));
    }
}

/// A problem for the key `section.name` unless `name` is one of `valid`.
fn known_key(out: &mut Vec<String>, section: &str, name: &str, what: &str, valid: &[&str]) {
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
fn problems(
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
    for name in repo.context.budget_kb.keys() {
        known_key(&mut out, "context.budget_kb", name, "role", &roles);
    }
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

/// The fields of `codex.provider` that toryo passes as `model_providers.<id>.<field>`.
const PROVIDER_FIELDS: &[&str] = &["name", "base_url", "env_key", "wire_api"];

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
fn is_or_under(key: &str, prefix: &str) -> bool {
    key.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

/// `key` (normalised) re-sets what toryo passes for the provider `id`: the selector, a
/// field of [`PROVIDER_FIELDS`], or a table holding them.
fn resets_provider(key: &str, id: &str) -> bool {
    let table = format!("model_providers.{}", id.to_ascii_lowercase());
    key == "model_providers"
        || key == table
        || is_or_under(key, "model_provider")
        || PROVIDER_FIELDS
            .iter()
            .any(|f| is_or_under(key, &format!("{table}.{f}")))
}

/// R60 (ARCH-05): the top-level keys only the machine config may hold. They name the
/// programs toryo starts, their environment and secrets, and the sandbox they run in — so
/// repository content (a teammate's commit, merged agent work) must not choose them.
pub const MACHINE_ONLY_KEYS: [&str; 2] = ["profiles", "toolchains"];

/// The machine-only keys found in a repository `toryo.yaml` (R60).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineOnlyPart {
    /// The keys found, in [`MACHINE_ONLY_KEYS`] order.
    pub keys: Vec<String>,
    /// Those keys and their values as YAML, ready to paste into the machine config.
    pub yaml: String,
}

impl MachineOnlyPart {
    /// The load error: what is wrong, where the block goes, and the block itself.
    pub fn refusal(&self, repo_path: &Path, machine_config: &str) -> String {
        let keys: Vec<String> = self.keys.iter().map(|k| format!("`{k}:`")).collect();
        format!(
            "{repo}: {keys} may not be defined in the repository config (R60: agent profiles and toolchains come only from the machine config, so repository content cannot choose the programs toryo starts, their secrets or their sandbox).\nMove this block to {machine_config} (merge it with what is there) and delete it from {repo}:\n\n{yaml}",
            repo = repo_path.display(),
            keys = keys.join(" and "),
            yaml = self.yaml.trim_end(),
        )
    }
}

/// The text of a YAML value with any tags taken off (`!x profiles` → `profiles`), for a
/// lookup by name that a tag cannot hide from (review C1).
pub fn untagged(v: &serde_yaml::Value) -> &serde_yaml::Value {
    match v {
        serde_yaml::Value::Tagged(t) => untagged(&t.value),
        other => other,
    }
}

/// C1: every YAML tag on a top-level key or value of a repository file (`!x profiles:`,
/// `roles: !x {…}`), refused in one error that names the file and each key. A tag makes a key
/// invisible to a lookup by name while a typed parse still reads it, so repository files —
/// `toryo.yaml`, the org chart, the role files — carry none.
pub fn refuse_tags(doc: &serde_yaml::Value, file: &Path) -> Result<()> {
    let Some(map) = untagged(doc).as_mapping() else {
        return Ok(());
    };
    let mut out = Vec::new();
    if let serde_yaml::Value::Tagged(t) = doc {
        out.push(format!("the document has the tag `{}`", t.tag));
    }
    for (k, v) in map {
        let name = match untagged(k) {
            serde_yaml::Value::String(s) => s.clone(),
            other => serde_yaml::to_string(other)
                .unwrap_or_default()
                .trim()
                .to_string(),
        };
        if let serde_yaml::Value::Tagged(t) = k {
            out.push(format!("key `{name}` has the tag `{}`", t.tag));
        }
        if let serde_yaml::Value::Tagged(t) = v {
            out.push(format!("the value of `{name}` has the tag `{}`", t.tag));
        }
    }
    if out.is_empty() {
        return Ok(());
    }
    bail!(
        "{}: YAML tags are not allowed in a repository file (review C1: a tag hides a key from Toryo's checks) — remove them:\n  - {}",
        file.display(),
        out.join("\n  - ")
    )
}

/// The machine-only part of a repository `toryo.yaml` text; `None` when it has none (R60).
/// Only the top level counts: a key of that name inside another block is not one. A tagged
/// key (`!x profiles:`) counts like the plain one (review C1).
pub fn machine_only_part(text: &str) -> Result<Option<MachineOnlyPart>> {
    let doc: serde_yaml::Value = serde_yaml::from_str(text)?;
    let Some(map) = untagged(&doc).as_mapping() else {
        return Ok(None);
    };
    let mut part = serde_yaml::Mapping::new();
    let mut keys = Vec::new();
    for key in MACHINE_ONLY_KEYS {
        let found = map
            .iter()
            .find(|(k, _)| untagged(k).as_str() == Some(key))
            .map(|(_, v)| v);
        if let Some(v) = found {
            part.insert(key.into(), v.clone());
            keys.push(key.to_string());
        }
    }
    if keys.is_empty() {
        return Ok(None);
    }
    Ok(Some(MachineOnlyPart {
        keys,
        yaml: serde_yaml::to_string(&serde_yaml::Value::Mapping(part))?,
    }))
}

/// How to name the machine config in a message: the path in use, else the default one.
pub fn machine_config_hint(machine_path: Option<&Path>) -> String {
    machine_path
        .map(Path::to_path_buf)
        .or_else(default_machine_config_path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "%USERPROFILE%\\.toryo\\toryo.local.yaml".into())
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

/// A `%NAME%` reference, the name in group 1: names may contain `(` `)`, as in
/// `%ProgramFiles(x86)%` (EC-28). One pattern for what [`expand_vars`] expands and for what
/// `doctor` warns about where nothing is expanded (M13-07).
pub const VAR_PATTERN: &str = r"%([A-Za-z_][A-Za-z0-9_()]*)%";

/// Expand `%VAR%` (Windows style) and `${VAR}` / `$VAR`-free paths. Unknown variables are an error.
/// Names may contain `(` `)`, as in `%ProgramFiles(x86)%` (EC-28).
/// M11-R12: the first `%NAME%` token in `s`, if any — used to catch a `%VAR%` that survived
/// [`expand_vars`] because a variable's own value contained it (it is never expanded again).
pub fn residual_var(s: &str) -> Option<String> {
    regex::Regex::new(VAR_PATTERN)
        .expect("regex")
        .captures(s)
        .map(|c| c[1].to_string())
}

pub fn expand_vars(s: &str) -> Result<String> {
    let re = regex::Regex::new(VAR_PATTERN).expect("regex");
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

fn resolve_secret_value(reference: &str) -> Result<String> {
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

    /// Test layout (R60): `yaml` written as a repository file without its machine-only
    /// keys, plus a machine config holding them. Returns (dir, repo path, machine path).
    fn write_split(yaml: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let mut doc: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        let map = doc.as_mapping_mut().unwrap();
        let mut machine = serde_yaml::Mapping::new();
        for key in MACHINE_ONLY_KEYS {
            if let Some(v) = map.remove(key) {
                machine.insert(key.into(), v);
            }
        }
        let (p, m) = (d.path().join("toryo.yaml"), d.path().join("machine.yaml"));
        std::fs::write(&p, serde_yaml::to_string(&doc).unwrap()).unwrap();
        std::fs::write(&m, serde_yaml::to_string(&machine).unwrap()).unwrap();
        (d, p, m)
    }

    #[test]
    fn loads_and_hashes_stably() {
        let (_d, p, m) = write_split(SAMPLE);
        let a = Effective::load(&p, Some(&m)).unwrap();
        let b = Effective::load(&p, Some(&m)).unwrap();
        assert_eq!(a.sha256(), b.sha256());
        assert_eq!(a.repo.verify.boundary, "low_integrity");
        // the frozen copy (R35) round-trips to the same digest
        let f = Effective::from_frozen(&a.canonical_json(), &p).unwrap();
        assert_eq!(f.sha256(), a.sha256());
    }

    #[test]
    fn machine_config_overrides_roles_and_changes_hash() {
        let (d, p, base_m) = write_split(SAMPLE);
        let m = d.path().join("machine-override.yaml");
        std::fs::write(
            &m,
            format!(
                "{}roles:\n  planner: {{ profile: codex-foundry }}\ngit:\n  trusted_filters:\n    lfs: {{ process: 'git-lfs filter-process', required: true }}\n",
                std::fs::read_to_string(&base_m).unwrap()
            ),
        )
        .unwrap();
        let base = Effective::load(&p, Some(&base_m)).unwrap();
        let eff = Effective::load(&p, Some(&m)).unwrap();
        assert_eq!(eff.repo.roles["planner"].profile, "codex-foundry");
        assert!(eff.trusted_filters.contains_key("lfs"));
        assert_ne!(base.sha256(), eff.sha256());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(load_str(&format!("{SAMPLE}\nbogus: 1\n")).is_err());
    }

    #[test]
    fn role_with_unknown_profile_is_rejected() {
        let e = load_err(&SAMPLE.replace("profile: claude-max, model: opus", "profile: nope"));
        assert!(
            e.contains("role `planner` refers to unknown profile `nope`")
                && e.contains("define it under `profiles:` in the machine config"),
            "{e}"
        );
    }

    /// R60 (ARCH-05): a repository file that defines `profiles:` or `toolchains:` does not
    /// load; the message names the file and carries the block to move, and nothing is
    /// rewritten.
    #[test]
    fn repo_file_may_not_define_profiles_or_toolchains() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("toryo.yaml");
        let m = d.path().join("machine.yaml");
        std::fs::write(&p, SAMPLE).unwrap();
        let e = format!("{:#}", Effective::load(&p, Some(&m)).unwrap_err());
        assert!(e.contains(&p.display().to_string()), "{e}");
        assert!(
            e.contains("`profiles:` may not be defined in the repository config (R60"),
            "{e}"
        );
        assert!(
            e.contains(&format!("Move this block to {}", m.display())),
            "{e}"
        );
        // the block is complete YAML: pasted into the machine config, it loads
        let block = &e[e.find("\n\nprofiles:").expect("block") + 2..];
        assert!(block.contains("codex-foundry:") && block.contains("windows_sandbox: unelevated"));
        let repo_only = {
            let mut doc: serde_yaml::Value = serde_yaml::from_str(SAMPLE).unwrap();
            doc.as_mapping_mut().unwrap().remove("profiles");
            serde_yaml::to_string(&doc).unwrap()
        };
        let q = d.path().join("repo-only.yaml");
        std::fs::write(&q, &repo_only).unwrap();
        std::fs::write(&m, block).unwrap();
        Effective::load(&q, Some(&m)).unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            SAMPLE,
            "never rewritten"
        );
        // toolchains too, each named
        std::fs::write(
            &p,
            format!("{SAMPLE}toolchains:\n  xpp: {{ msbuild: m }}\n"),
        )
        .unwrap();
        let e = format!("{:#}", Effective::load(&p, None).unwrap_err());
        assert!(
            e.contains("`profiles:` and `toolchains:` may not be defined"),
            "{e}"
        );
        assert!(e.contains("toolchains:\n  xpp:"), "{e}");
        // only the top level counts
        assert_eq!(
            machine_only_part("version: 1\nproject: { root: r, profiles: x }\n").unwrap(),
            None
        );
    }

    /// Review C1 (M11-A1 AC-1): a YAML tag on `profiles:` hid it from the raw-text check while
    /// the typed parse still read it — the reproduction is refused, naming `profiles` and the
    /// machine config, and no repository profile ever reaches the effective configuration.
    #[test]
    fn a_tagged_profiles_key_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let (p, m) = (d.path().join("toryo.yaml"), d.path().join("machine.yaml"));
        std::fs::write(
            &m,
            "profiles:\n  claude-max: { kind: claude, billing: subscription, exe: claude }\n",
        )
        .unwrap();
        std::fs::write(
            &p,
            "version: 1\nproject: { root: r, worktrees_root: w }\n!x profiles:\n  evil: { kind: codex, billing: subscription, exe: 'tools\\evil.exe', secret_env: { K: 'credman:toryo/foundry-openai' } }\nroles:\n  planner: { profile: claude-max }\n  implementer: { profile: evil }\n",
        )
        .unwrap();
        let e = format!("{:#}", Effective::load(&p, Some(&m)).unwrap_err());
        assert!(
            e.contains("`profiles:` may not be defined in the repository config (R60"),
            "{e}"
        );
        assert!(
            e.contains(&format!("Move this block to {}", m.display())),
            "{e}"
        );
        assert!(e.contains("evil:"), "the block to move: {e}");
        // a tag on any other top-level key or value is refused too, all at once
        std::fs::write(
            &p,
            "version: 1\nproject: { root: r, worktrees_root: w }\n!x roles:\n  planner: { profile: claude-max }\npolicy: !y { deny_paths: [] }\n",
        )
        .unwrap();
        let e = format!("{:#}", Effective::load(&p, Some(&m)).unwrap_err());
        assert!(e.contains(&p.display().to_string()), "{e}");
        assert!(e.contains("key `roles` has the tag `!x`"), "{e}");
        assert!(e.contains("the value of `policy` has the tag `!y`"), "{e}");
        // and `toolchains` behind a tag is a machine-only key as well
        assert_eq!(
            machine_only_part("version: 1\n!t toolchains: { xpp: { msbuild: m } }\n")
                .unwrap()
                .unwrap()
                .keys,
            ["toolchains"]
        );
    }

    /// Review C1 (group A review): a tag on the whole document is refused too.
    #[test]
    fn a_tagged_document_is_refused() {
        let doc: serde_yaml::Value =
            serde_yaml::from_str("!x\nversion: 1\nproject: { root: r, worktrees_root: w }\n")
                .unwrap();
        let e = format!("{:#}", refuse_tags(&doc, Path::new("t.yaml")).unwrap_err());
        assert!(e.contains("the document has the tag `!x`"), "{e}");
    }

    /// Review C1: the repository file is parsed without the machine-only fields, so whatever
    /// gets past the raw-text check is an unknown field, never a profile.
    #[test]
    fn the_repository_file_has_no_profile_fields() {
        let e = serde_yaml::from_str::<RepoFile>(
            "version: 1\nproject: { root: r, worktrees_root: w }\nprofiles: {}\n",
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("unknown field `profiles`"), "{e}");
    }

    /// R60 AC-2: the same profiles from the machine config give the same effective
    /// configuration — and hash — as the old layout with profiles in the repository file.
    #[test]
    fn profiles_from_the_machine_config_hash_like_the_old_layout() {
        let old = Effective {
            repo: serde_yaml::from_str(SAMPLE).unwrap(),
            trusted_filters: BTreeMap::new(),
            toolchains: Toolchains::default(),
            machine_verify: MachineVerify::default(),
            team: None,
            build_entries: Vec::new(),
            repo_path: PathBuf::new(),
            machine_path: None,
            origins: Origins::default(),
            notify: Notify::default(),
            files: Vec::new(),
        };
        let (_d, p, m) = write_split(SAMPLE);
        let new = Effective::load(&p, Some(&m)).unwrap();
        assert_eq!(new.canonical_json(), old.canonical_json());
        assert_eq!(new.sha256(), old.sha256());
        // a task frozen with profiles inside its config.json keeps loading
        let f = Effective::from_frozen(&old.canonical_json(), Path::new("config.json")).unwrap();
        assert_eq!(f.repo.profiles.len(), 2);
    }

    #[test]
    fn secret_reference_forms() {
        // (R23-04: the value was `v`, which is now too short to be a secret — see below)
        std::env::set_var("TORYO_TEST_SECRET_X", "value-of-x");
        assert_eq!(
            resolve_secret("env:TORYO_TEST_SECRET_X").unwrap(),
            "value-of-x"
        );
        assert!(resolve_secret("plain").is_err());
    }

    /// R23-04 (review 2026-10-23 C3): a secret of a few bytes (`1`, a placeholder in Credential
    /// Manager, a selector put under `secret_env` instead of `env`) was redacted wherever those
    /// bytes stood — in every JSON line of the agent's stream, which then no longer parsed, and
    /// the turn failed as `ProcessCrashed` with no word why. It is refused where it is resolved,
    /// with its name and length, never its value. A value of line breaks only stays what M13-03
    /// made it: no secret, not redacted, not refused.
    #[test]
    fn a_secret_too_short_to_redact_is_refused() {
        for v in ["1", "abc", "1234567"] {
            std::env::set_var("TORYO_TEST_SECRET_SHORT", v);
            let e = format!(
                "{:#}",
                resolve_secret("env:TORYO_TEST_SECRET_SHORT").unwrap_err()
            );
            assert!(
                e.contains(&format!("{} byte(s)", v.len())) && e.contains("at least 8"),
                "{v}: {e}"
            );
            assert!(
                !e.contains(&format!("`{v}`")),
                "the value is never shown: {e}"
            );
        }
        std::env::set_var("TORYO_TEST_SECRET_SHORT", "12345678");
        assert_eq!(
            resolve_secret("env:TORYO_TEST_SECRET_SHORT").unwrap(),
            "12345678"
        );
        std::env::set_var("TORYO_TEST_SECRET_SHORT", "\n");
        assert_eq!(resolve_secret("env:TORYO_TEST_SECRET_SHORT").unwrap(), "\n");
    }

    /// Load `yaml` in the R60 layout: its profiles in a machine config (see `write_split`).
    fn load_str(yaml: &str) -> Result<Effective> {
        let (_d, p, m) = write_split(yaml);
        Effective::load(&p, Some(&m))
    }

    /// The error text of a config that must not load.
    fn load_err(yaml: &str) -> String {
        format!(
            "{:#}",
            load_str(yaml).expect_err("the config must be refused")
        )
    }

    /// EC-23: an explicit `fallback: []` is kept (and hashed) apart from an absent one.
    #[test]
    fn empty_fallback_is_kept_apart_from_an_absent_one() {
        let absent = load_str(SAMPLE).unwrap();
        let empty = load_str(&SAMPLE.replace(
            "implementer: { profile: codex-foundry }",
            "implementer: { profile: codex-foundry, fallback: [] }",
        ))
        .unwrap();
        assert_eq!(absent.repo.roles["implementer"].fallback, None);
        assert_eq!(empty.repo.roles["implementer"].fallback, Some(vec![]));
        assert!(!absent.canonical_json().contains("fallback"));
        assert!(empty.canonical_json().contains(r#""fallback":[]"#));
        let f = Effective::from_frozen(&empty.canonical_json(), Path::new("c.json")).unwrap();
        assert_eq!(f.sha256(), empty.sha256());
        assert_eq!(f.repo.roles["implementer"].fallback, Some(vec![]));
    }

    /// EC-23 / DES-05: typos in role keys and enum-like values are refused, all at once.
    #[test]
    fn unknown_roles_and_values_are_refused() {
        let roles = |extra: &str| {
            SAMPLE.replace(
                "  implementer: { profile: codex-foundry }\n",
                &format!("  implementer: {{ profile: codex-foundry }}\n{extra}"),
            )
        };
        let yaml = format!(
            "{}verify: {{ boundary: lowintegrity, checkout: trackd }}\npipeline: {{ unknown_turn_reservation_tokens: lots, budget_weights: {{ cached_read: 0.1, output: 2.0 }} }}\ncontext: {{ budget_kb: {{ reviewer: 50, planer: 30 }} }}\n",
            roles("  reviwer: { profile: claude-max }\n  implementor: { profile: claude-max }\n")
        );
        let e = load_err(&yaml);
        for want in [
            "roles.reviwer: unknown role (valid: planner, implementer, reviewer)",
            "roles.implementor: unknown role",
            "verify.boundary: `lowintegrity` is not a valid value (valid: low_integrity, none)",
            "verify.checkout: `trackd` is not a valid value (valid: clean, tracked)",
            "pipeline.unknown_turn_reservation_tokens: `lots` is not a valid value",
            "pipeline.budget_weights.cached_read: unknown bucket",
            "context.budget_kb.planer: unknown role",
        ] {
            assert!(e.contains(want), "{want:?} missing in:\n{e}");
        }
        assert!(!e.contains("budget_weights.output") && !e.contains("budget_kb.reviewer"));
        // the valid spellings load
        let ok = format!(
            "{}verify: {{ boundary: none, checkout: tracked }}\npipeline: {{ unknown_turn_reservation_tokens: auto, budget_weights: {{ uncached_input: 1.0, cache_read: 0.1, cache_write: 1.25, output: 1.0 }} }}\ncontext: {{ budget_kb: {{ planner: 30, implementer: 60, reviewer: 50 }} }}\n",
            roles("  reviewer: { profile: claude-max }\n")
        );
        load_str(&ok).unwrap();
        load_str(&ok.replace("tokens: auto", "tokens: 150000")).unwrap();
    }

    /// R23-02 (review 2026-10-23 C1): a budget weight JSON cannot carry (`.inf`, `.nan`) was
    /// frozen as `null` at `task create`, and every later
    /// command of the task failed to read the frozen configuration back. A negative weight would
    /// give budget back for spent tokens. Both are refused at load; 0 stays a valid weight.
    #[test]
    fn budget_weights_json_cannot_carry_are_refused() {
        for bad in [".inf", "-.inf", ".nan", "-1", "-0.5"] {
            let e = load_err(&format!(
                "{SAMPLE}pipeline: {{ budget_weights: {{ output: {bad} }} }}\n"
            ));
            assert!(
                e.contains("pipeline.budget_weights.output:")
                    && e.contains("is not a valid weight"),
                "{bad}: {e}"
            );
        }
        load_str(&format!(
            "{SAMPLE}pipeline: {{ budget_weights: {{ cache_read: 0, output: 1.5 }} }}\n"
        ))
        .unwrap();
        // and a configuration that does not read back is never frozen
        let mut cfg = load_str(SAMPLE).unwrap();
        assert!(cfg.frozen_json().is_ok());
        cfg.repo.pipeline.budget_weights = Some(
            [("output".to_string(), f64::INFINITY)]
                .into_iter()
                .collect(),
        );
        let e = format!("{:#}", cfg.frozen_json().unwrap_err());
        assert!(e.contains("cannot be frozen"), "{e}");
    }

    #[test]
    fn codex_sandbox_values_are_checked() {
        let e = load_err(&SAMPLE.replace(
            "windows_sandbox: unelevated",
            "windows_sandbox: elevatd\n      sandbox_mode: danger-full-access",
        ));
        assert!(
            e.contains("profiles.codex-foundry.codex.windows_sandbox: `elevatd` is not a valid value (valid: unelevated, elevated)"),
            "{e}"
        );
        assert!(
            e.contains("profiles.codex-foundry.codex.sandbox_mode: `danger-full-access` is not a valid value (valid: workspace-write, read-only)"),
            "{e}"
        );
        load_str(&SAMPLE.replace("windows_sandbox: unelevated", "windows_sandbox: elevated"))
            .unwrap();
        // stricter than workspace-write: loads (the launch preflight warns and runs it)
        for mode in ["read-only", "workspace-write"] {
            let yaml = SAMPLE.replace(
                "windows_sandbox: unelevated",
                &format!("windows_sandbox: unelevated\n      sandbox_mode: {mode}"),
            );
            let cfg = load_str(&yaml).unwrap();
            let codex = cfg.repo.profiles["codex-foundry"].codex.as_ref().unwrap();
            assert_eq!(codex.sandbox_mode, mode);
        }
        for mode in ["danger-full-access", "workspace_write", "none"] {
            let e = load_err(&SAMPLE.replace(
                "windows_sandbox: unelevated",
                &format!("windows_sandbox: unelevated\n      sandbox_mode: {mode}"),
            ));
            assert!(
                e.contains(&format!("sandbox_mode: `{mode}` is not a valid value")),
                "{e}"
            );
        }
    }

    /// A Codex profile without a `codex:` block gets the documented defaults, not an
    /// empty `sandbox_mode` (which the launch preflight would refuse).
    #[test]
    fn default_codex_settings_use_workspace_write() {
        let d = CodexSettings::default();
        assert_eq!(d.sandbox_mode, "workspace-write");
        assert!(d.windows_sandbox.is_none() && d.overrides.is_empty() && d.provider.is_none());
    }

    /// ARCH-05: exactly the overrides that re-set what toryo sets or switch the sandbox,
    /// approval or environment policy are refused; the provider's selector and the fields
    /// toryo passes for it only with a `codex.provider` block. A key is read as Codex reads
    /// it (up to `=`, no whitespace or quotes), in any case.
    /// `-c profile="x"` with `-c profiles.x.sandbox_mode=…` would re-set the sandbox through
    /// a profile, so both are refused, like the sandbox keys themselves.
    #[test]
    fn codex_overrides_cannot_select_or_define_a_profile() {
        for key in [
            "profile",
            "Profile",
            "profiles",
            "profiles.x.sandbox_mode",
            "profiles.x",
        ] {
            assert!(denied_override(key, None).is_some(), "{key}");
        }
        for key in ["profiles_dir", "profile_name", "model_profile"] {
            assert!(denied_override(key, None).is_none(), "{key}");
        }
    }

    #[test]
    fn codex_overrides_refuse_exactly_what_toryo_sets() {
        let foundry = Some("foundry");
        let refused = [
            "sandbox_mode",
            " sandbox_mode ",
            "Sandbox_Mode",
            "sandbox_mode=\"danger-full-access\"#",
            "sandbox_permissions",
            "windows",
            "windows.sandbox",
            "approval_policy",
            "approval_policy.reject.sandbox_approval",
            "shell_environment_policy",
            "shell_environment_policy.inherit",
            "shell_environment_policy.set.PATH",
        ];
        let refused_with_provider = [
            "model_provider",
            "model_providers",
            "model_providers.foundry",
            "model_providers.foundry.name",
            "model_providers.foundry.base_url",
            "model_providers.\"foundry\".base_url",
            "model_providers.foundry.env_key",
            "model_providers.foundry.wire_api",
        ];
        let allowed = [
            "sandbox_workspace_write.network_access",
            "sandbox_workspace_write.writable_roots",
            "model_providers.foundry.query_params.api-version",
            "model_providers.foundry.request_max_retries",
            "model_providers.foundry.stream_idle_timeout_ms",
            "model_providers.azure.base_url",
            "model_reasoning_effort",
            "model_verbosity",
            "windows_x",
            "sandbox",
        ];
        for k in refused {
            assert!(denied_override(k, None).is_some(), "{k}");
            assert!(denied_override(k, foundry).is_some(), "{k}");
        }
        for k in refused_with_provider {
            assert!(denied_override(k, foundry).is_some(), "{k}");
            // without a codex.provider block, the provider comes from the overrides
            // (how `toryo spike init-config` translates a Codex profile)
            assert_eq!(denied_override(k, None), None, "{k}");
        }
        for k in allowed {
            assert_eq!(denied_override(k, foundry), None, "{k}");
            assert_eq!(denied_override(k, None), None, "{k}");
        }
        // the same at load (SAMPLE's profile has the provider block `foundry`)
        let with = |key: &str| {
            SAMPLE.replace(
                "windows_sandbox: unelevated",
                &format!("windows_sandbox: unelevated\n      overrides: {{ '{key}': '\"x\"' }}"),
            )
        };
        for k in [
            "sandbox_mode",
            "sandbox_permissions",
            "approval_policy",
            "model_provider",
            "model_providers.foundry.base_url",
        ] {
            let e = load_err(&with(k));
            let want = format!("profiles.codex-foundry.codex.overrides.{k}: re-sets");
            assert!(e.contains(&want), "{k}: {e}");
        }
        for k in allowed {
            load_str(&with(k)).unwrap();
        }
        let no_provider = with("model_provider").replace(
            "      provider: { id: foundry, name: Azure Foundry, base_url: https://x.openai.azure.com/openai/v1, env_key: AZURE_OPENAI_API_KEY }\n",
            "",
        );
        assert!(!no_provider.contains("provider: {"));
        load_str(&no_provider).unwrap();
    }

    /// The configuration shape `toryo spike init-config` writes (explicit nulls), as in use.
    #[test]
    fn an_init_config_style_config_loads() {
        let yaml = r#"version: 1
project: { root: C:\r, worktrees_root: C:\w }
profiles:
  claude-max: { kind: claude, billing: subscription, exe: c.exe, env: { CLAUDE_CODE_GIT_BASH_PATH: b.exe }, codex: null, model: null, version: null }
  codex-openai:
    kind: codex
    billing: subscription
    exe: codex.cmd
    codex: { model_reasoning_effort: null, overrides: {}, project_doc_max_bytes: null, provider: null, sandbox_mode: workspace-write, windows_sandbox: unelevated }
    env: {}
    model: null
    version: null
roles:
  implementer: { profile: codex-openai, fallback: [claude-max] }
  planner: { profile: claude-max }
  reviewer: { profile: claude-max }
"#;
        let cfg = load_str(yaml).unwrap();
        assert_eq!(
            cfg.repo.roles["implementer"].fallback,
            Some(vec!["claude-max".to_string()])
        );
    }

    /// The R59 shape: `toolchains.xpp` from the machine config, `tool: xpp` in the repository.
    const XPP: &str = r#"
version: 1
project: { root: D:\D365-Git, worktrees_root: D:\D365-worktrees }
profiles:
  claude-max: { kind: claude, billing: subscription, exe: claude }
roles:
  planner: { profile: claude-max }
toolchains:
  xpp:
    msbuild: 'C:\Program Files\Microsoft Visual Studio\2022\Professional\MSBuild\Current\Bin\MSBuild.exe'
    nugets: 'D:\D365\cicd-nugets\10.0.45'
    extra_references: ['D:\ISV\Bin']
    bp_check: false
    ude_metadata: '%LOCALAPPDATA%\Microsoft\Dynamics365\10.0.2345.12\PackagesLocalDirectory'
commands:
  verify:
    - name: build
      tool: xpp
      metadata: Metadata
      projects: [Projects/Build/ContosoCore.Build.rnrproj]
      timeout_min: 90
    - { name: labels, program: pwsh, args: [-NoProfile, -File, scripts/check-labels.ps1] }
"#;

    /// R59 (M9-B1 AC-1): the shape loads; the toolchain comes from the machine config, is
    /// frozen with the task and changes the hash.
    #[test]
    fn xpp_toolchain_and_verify_kind_load() {
        let cfg = load_str(XPP).unwrap();
        let x = cfg.toolchains.xpp.as_ref().unwrap();
        assert_eq!(x.nugets, r"D:\D365\cicd-nugets\10.0.45");
        assert_eq!(x.extra_references, [r"D:\ISV\Bin"]);
        assert_eq!(cfg.origins.toolchains["xpp"], Origin::Machine);
        let v = &cfg.repo.commands.verify;
        assert_eq!(
            v[0].kind(),
            CommandKind::Xpp {
                metadata: "Metadata",
                projects: &["Projects/Build/ContosoCore.Build.rnrproj".to_string()]
            }
        );
        assert!(v[0].is_xpp() && !v[1].is_xpp());
        let frozen = cfg.canonical_json();
        assert!(frozen.contains(r#""toolchains":{"xpp":{"bp_check":false,"extra_references""#));
        let back = Effective::from_frozen(&frozen, Path::new("config.json")).unwrap();
        assert_eq!(back.toolchains, cfg.toolchains);
        assert_eq!(back.sha256(), cfg.sha256());
        // another NuGet release is another configuration
        let other = load_str(&XPP.replace("10.0.45", "10.0.46")).unwrap();
        assert_ne!(other.sha256(), cfg.sha256());
    }

    /// M9-B1 AC-2: every invalid combination is reported at once, naming its key.
    #[test]
    fn invalid_xpp_commands_name_the_key() {
        let verify = |items: &str| {
            XPP.split("commands:").next().unwrap().to_string() + "commands:\n  verify:\n" + items
        };
        let e = load_err(&verify(
            r#"    - { name: both, program: pwsh, tool: xpp, metadata: M, projects: [p.rnrproj] }
    - { name: neither }
    - { name: other, tool: cmake }
    - { name: nometa, tool: xpp, projects: [p.rnrproj] }
    - { name: noproj, tool: xpp, metadata: M }
    - { name: paths, tool: xpp, metadata: 'C:\M', projects: ['../x.rnrproj', '/abs.rnrproj'] }
    - { name: args, tool: xpp, metadata: M, projects: [p.rnrproj], args: [-x] }
    - { name: prog, program: pwsh, metadata: M, projects: [p.rnrproj] }
"#,
        ));
        for want in [
            "commands.verify[0] (both): has both `program` and `tool` — use one",
            "commands.verify[1] (neither): needs `program` (with `args`) or `tool`",
            "commands.verify[2] (other).tool: `cmake` is not a valid value (valid: xpp)",
            "commands.verify[3] (nometa).metadata: required for `tool: xpp`",
            "commands.verify[4] (noproj).projects: `tool: xpp` needs at least one project",
            r"commands.verify[5] (paths).metadata: `C:\M` must be relative to the repository",
            "commands.verify[5] (paths).projects: `../x.rnrproj` may not contain `..`",
            "commands.verify[5] (paths).projects: `/abs.rnrproj` must be relative to the repository",
            "commands.verify[6] (args).args: only for a `program` command",
            "commands.verify[7] (prog).metadata: only for `tool: xpp`",
            "commands.verify[7] (prog).projects: only for `tool: xpp`",
        ] {
            assert!(e.contains(want), "{want}\n---\n{e}");
        }
        // no toolchain in the machine config
        let (head, rest) = XPP.split_once("toolchains:").unwrap();
        let no_tc = format!("{head}commands:{}", rest.split_once("commands:").unwrap().1);
        let e = load_err(&no_tc);
        assert!(
            e.contains("commands.verify[0] (build).tool: `tool: xpp` needs `toolchains.xpp` in the machine config"),
            "{e}"
        );
        // bp_check: true waits for V08-VERIFY 4
        let e = load_err(&XPP.replace("bp_check: false", "bp_check: true"));
        assert!(
            e.contains("toolchains.xpp.bp_check: `true` is not supported yet"),
            "{e}"
        );
        // unknown toolchain keys are refused by serde
        let e = load_err(&XPP.replace("bp_check: false", "bpcheck: false"));
        assert!(e.contains("bpcheck"), "{e}");
    }

    /// `diagnostics: msbuild | none` is for a `program` command only; a bad value, or the key on a
    /// `tool: xpp` command, names its key; a configuration that sets it is another configuration.
    #[test]
    fn diagnostics_is_for_program_commands_only() {
        let verify = |items: &str| {
            XPP.split("commands:").next().unwrap().to_string() + "commands:\n  verify:\n" + items
        };
        let ok = load_str(&verify(
            "    - { name: build, program: dotnet, args: [build], diagnostics: msbuild }\n    - { name: test, program: dotnet, args: [test], diagnostics: none }\n    - { name: lint, program: pwsh, args: [-File, l.ps1] }\n",
        ))
        .unwrap();
        let v = &ok.repo.commands.verify;
        assert!(v[0].reads_msbuild() && !v[1].reads_msbuild() && !v[2].reads_msbuild());
        let plain = load_str(&verify(
            "    - { name: build, program: dotnet, args: [build] }\n    - { name: test, program: dotnet, args: [test] }\n    - { name: lint, program: pwsh, args: [-File, l.ps1] }\n",
        ))
        .unwrap();
        assert_ne!(ok.sha256(), plain.sha256());
        assert!(!plain.canonical_json().contains("diagnostics"));
        let back = Effective::from_frozen(&ok.canonical_json(), Path::new("config.json")).unwrap();
        assert_eq!(back.sha256(), ok.sha256());
        let e = load_err(&verify(
            "    - { name: a, program: dotnet, diagnostics: junit }\n    - { name: b, tool: xpp, metadata: M, projects: [p.rnrproj], diagnostics: msbuild }\n",
        ));
        for want in [
            "commands.verify[0] (a).diagnostics: `junit` is not a valid value (valid: msbuild, none)",
            "commands.verify[1] (b).diagnostics: only for a `program` command",
        ] {
            assert!(e.contains(want), "{want}\n---\n{e}");
        }
    }

    /// M9-B1 AC-3: a configuration without the new keys keeps the hash it had before R59
    /// (value computed with the M9-A4 build), and a frozen one reads back unchanged.
    #[test]
    fn configs_without_toolchains_keep_their_hash() {
        let d = tempfile::tempdir().unwrap();
        let (p, m) = (d.path().join("toryo.yaml"), d.path().join("machine.yaml"));
        std::fs::write(&p, "version: 1\nproject: { root: D:\\\\R, worktrees_root: D:\\\\W }\nroles:\n  planner: { profile: claude-max }\ncommands:\n  verify:\n    - { name: build, program: pwsh, args: [-File, scripts/build.ps1], timeout_min: 20 }\n    - { name: test, program: git, args: [status] }\n").unwrap();
        std::fs::write(
            &m,
            "profiles:\n  claude-max: { kind: claude, billing: subscription, exe: claude }\n",
        )
        .unwrap();
        let e = Effective::load(&p, Some(&m)).unwrap();
        assert_eq!(
            e.sha256(),
            "7ae81a43f08d24ea1866b54a3cbb30bce443dc15aa02a212f01cace2f719a3b8"
        );
        let frozen = e.canonical_json();
        assert!(!frozen.contains("toolchains") && !frozen.contains("\"tool\""));
        let back = Effective::from_frozen(&frozen, Path::new("config.json")).unwrap();
        assert_eq!(back.canonical_json(), frozen);
        assert!(back.toolchains.is_empty());
    }

    /// M11-B1 AC-2: `seats:` is the machine's new spelling of `roles:` — it overrides like
    /// `roles:` and hashes the same; both in one file are refused, naming the file.
    #[test]
    fn machine_seats_are_the_new_spelling_of_roles() {
        let d = tempfile::tempdir().unwrap();
        let (p, m) = (d.path().join("toryo.yaml"), d.path().join("machine.yaml"));
        std::fs::write(&p, "version: 1\nproject: { root: r, worktrees_root: w }\nroles:\n  planner: { profile: a }\n").unwrap();
        let profiles = "profiles:\n  a: { kind: claude, billing: subscription, exe: c }\n  b: { kind: codex, billing: subscription, exe: x }\n";
        std::fs::write(
            &m,
            format!("{profiles}roles:\n  planner: {{ profile: b }}\n"),
        )
        .unwrap();
        let old = Effective::load(&p, Some(&m)).unwrap();
        std::fs::write(
            &m,
            format!("{profiles}seats:\n  planner: {{ profile: b }}\n"),
        )
        .unwrap();
        let new = Effective::load(&p, Some(&m)).unwrap();
        assert_eq!(new.repo.roles["planner"].profile, "b");
        assert_eq!(
            new.origins.roles["planner"],
            Origin::RepoOverriddenByMachine
        );
        assert_eq!(new.sha256(), old.sha256());
        std::fs::write(
            &m,
            format!("{profiles}roles:\n  planner: {{ profile: b }}\nseats:\n  reviewer: {{ profile: b }}\n"),
        )
        .unwrap();
        let e = format!("{:#}", Effective::load(&p, Some(&m)).unwrap_err());
        assert!(
            e.contains(&m.display().to_string())
                && e.contains("both `roles:` and `seats:` are set"),
            "{e}"
        );
    }

    /// M11-R8: a misspelt seat in the machine config names that file and the key as the file
    /// spells it (`seats.x` / `roles.x`); one in toryo.yaml stays `roles.x`.
    #[test]
    fn a_misspelt_machine_seat_names_the_machine_config() {
        let d = tempfile::tempdir().unwrap();
        let (p, m) = (d.path().join("toryo.yaml"), d.path().join("machine.yaml"));
        std::fs::write(&p, "version: 1\nproject: { root: r, worktrees_root: w }\nroles:\n  planner: { profile: a }\n").unwrap();
        let profiles = "profiles:\n  a: { kind: claude, billing: subscription, exe: c }\n";
        for key in ["seats", "roles"] {
            std::fs::write(
                &m,
                format!("{profiles}{key}:\n  reviwer: {{ profile: a }}\n"),
            )
            .unwrap();
            let e = format!("{:#}", Effective::load(&p, Some(&m)).unwrap_err());
            assert!(
                e.contains(&format!("{}: {key}.reviwer: unknown role", m.display())),
                "{key}: {e}"
            );
        }
    }

    /// Review H2 (M11-A2): the machine's `verify:` floor and root. Unset, nothing changes (the
    /// hash above); set, they are frozen and hashed with the task, the root replaces the
    /// repository's, and each says where it came from.
    #[test]
    fn the_machine_verify_floor_and_root() {
        let d = tempfile::tempdir().unwrap();
        let (p, m) = (d.path().join("toryo.yaml"), d.path().join("machine.yaml"));
        let profiles =
            "profiles:\n  claude-max: { kind: claude, billing: subscription, exe: claude }\n";
        std::fs::write(&p, "version: 1\nproject: { root: D:\\\\R, worktrees_root: D:\\\\W }\nroles:\n  planner: { profile: claude-max }\nverify: { boundary: none, verify_root: 'D:\\repo-verify' }\n").unwrap();
        std::fs::write(&m, profiles).unwrap();
        let without = Effective::load(&p, Some(&m)).unwrap();
        assert!(without.machine_verify.is_empty());
        assert!(!without.canonical_json().contains("machine_verify"));
        assert_eq!(without.origins.verify["boundary"], Origin::Repo);
        assert_eq!(without.origins.verify["verify_root"], Origin::Repo);
        // below the default floor: refused, naming both files and both ways out
        let why = crate::engine::verify::below_floor(&without).unwrap();
        for want in [
            "verify_boundary_below_floor",
            &p.display().to_string(),
            &m.display().to_string(),
            "keep `verify.boundary: low_integrity`",
            "min_boundary: none",
            "Environment gates",
        ] {
            assert!(why.contains(want), "{want}: {why}");
        }
        std::fs::write(
            &m,
            format!(
                "{profiles}verify: {{ min_boundary: none, verify_root: 'E:\\machine-verify' }}\n"
            ),
        )
        .unwrap();
        let with = Effective::load(&p, Some(&m)).unwrap();
        assert_eq!(crate::engine::verify::below_floor(&with), None);
        assert_ne!(with.sha256(), without.sha256());
        assert!(with.canonical_json().contains(
            r#""machine_verify":{"min_boundary":"none","verify_root":"E:\\machine-verify"}"#
        ));
        assert_eq!(
            crate::engine::verify::verify_root(&with),
            PathBuf::from("E:\\machine-verify")
        );
        assert_eq!(with.origins.verify["min_boundary"], Origin::Machine);
        assert_eq!(
            with.origins.verify["verify_root"],
            Origin::RepoOverriddenByMachine
        );
        let back = Effective::from_frozen(&with.canonical_json(), &p).unwrap();
        assert_eq!(back.sha256(), with.sha256());
        assert_eq!(
            crate::engine::verify::verify_root(&back),
            PathBuf::from("E:\\machine-verify")
        );
        // the repository's own low_integrity is never below the floor
        std::fs::write(&p, "version: 1\nproject: { root: D:\\\\R, worktrees_root: D:\\\\W }\nroles:\n  planner: { profile: claude-max }\n").unwrap();
        std::fs::write(&m, profiles).unwrap();
        assert_eq!(
            crate::engine::verify::below_floor(&Effective::load(&p, Some(&m)).unwrap()),
            None
        );
        // a misspelt value or key is refused
        std::fs::write(&m, format!("{profiles}verify: {{ min_boundary: off }}\n")).unwrap();
        let e = format!("{:#}", Effective::load(&p, Some(&m)).unwrap_err());
        assert!(
            e.contains("verify.min_boundary (machine config")
                && e.contains("`off` is not a valid value (valid: low_integrity, none)"),
            "{e}"
        );
        std::fs::write(&m, format!("{profiles}verify: {{ boundary: none }}\n")).unwrap();
        let e = format!("{:#}", Effective::load(&p, Some(&m)).unwrap_err());
        assert!(e.contains("boundary"), "{e}");
    }

    /// M10-D1 AC-3: `notify:` is read from the machine config, and it is not part of the
    /// frozen, hashed configuration — a config with it hashes like the one without.
    #[test]
    fn notify_is_a_machine_setting_outside_the_hash() {
        let d = tempfile::tempdir().unwrap();
        let (p, m) = (d.path().join("toryo.yaml"), d.path().join("machine.yaml"));
        std::fs::write(&p, "version: 1\nproject: { root: D:\\\\R, worktrees_root: D:\\\\W }\nroles:\n  planner: { profile: claude-max }\ncommands:\n  verify:\n    - { name: build, program: pwsh, args: [-File, scripts/build.ps1], timeout_min: 20 }\n    - { name: test, program: git, args: [status] }\n").unwrap();
        let profiles =
            "profiles:\n  claude-max: { kind: claude, billing: subscription, exe: claude }\n";
        std::fs::write(&m, profiles).unwrap();
        let without = Effective::load(&p, Some(&m)).unwrap();
        assert_eq!(without.notify, Notify { toast: false });
        std::fs::write(&m, format!("{profiles}notify: {{ toast: true }}\n")).unwrap();
        let with = Effective::load(&p, Some(&m)).unwrap();
        assert_eq!(with.notify, Notify { toast: true });
        assert_eq!(with.sha256(), without.sha256());
        assert_eq!(
            with.sha256(),
            "7ae81a43f08d24ea1866b54a3cbb30bce443dc15aa02a212f01cace2f719a3b8"
        );
        assert!(!with.canonical_json().contains("notify"));
        // the frozen configuration has no notify setting: a drive command reads the machine's
        let back = Effective::from_frozen(&with.canonical_json(), Path::new("c.json")).unwrap();
        assert_eq!(back.notify, Notify::default());
        // unknown keys under notify are refused (deny_unknown_fields)
        std::fs::write(
            &m,
            format!("{profiles}notify: {{ toast: true, sound: loud }}\n"),
        )
        .unwrap();
        let e = format!("{:#}", Effective::load(&p, Some(&m)).unwrap_err());
        assert!(e.contains("sound"), "{e}");
    }

    /// M11-A1 AC-2 / B2 AC-2: every shipped template hashes as it did in M10 (values computed
    /// with `main` @ aa0be03): the new loader, the repository-file struct and every M11 key
    /// leave a configuration without them byte for byte the same.
    #[test]
    fn templates_keep_their_m10_hash() {
        let t = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
        let d = tempfile::tempdir().unwrap();
        let m = d.path().join("machine.yaml");
        std::fs::write(&m, "profiles:\n  claude-max: { kind: claude, billing: subscription, exe: claude }\n  codex-openai: { kind: codex, billing: subscription, exe: codex, codex: { windows_sandbox: unelevated } }\n").unwrap();
        for (repo, machine, want) in [
            (
                t.join("toryo.yaml.example"),
                t.join("toryo.local.yaml.example"),
                "7e2e52195e51597e2b220e46c99dc6fa8aa2c6b9e08e4707da102b4bcf087e0f",
            ),
            (
                t.join("d365fo").join("toryo.yaml.example"),
                t.join("d365fo").join("toryo.local.yaml.example"),
                // M12-18: the d365fo machine example itself changed (the `codex-openai` profile
                // the org chart seats, `seats`, `verify`, the `dbsync` environment gate with
                // `confirm: first`); it was `fd5f27ff…` (M10). The loader-neutrality this test
                // pins is still held by the two templates beside it, whose files did not change.
                "919cfa7aec8e707d98772fb5be7eed6ce7abd5924f4db5ba2635778704e0deb4",
            ),
            (
                t.join("dogfood").join("toryo.yaml.example"),
                m.clone(),
                "29303e317d9e6055a8f98863a14140fca260d45300f00024827db971e743862d",
            ),
        ] {
            let cfg = Effective::load(&repo, Some(&machine)).unwrap();
            assert_eq!(cfg.sha256(), want, "{}", repo.display());
        }
    }

    /// M11-R8: the commented M11 block of the machine template (`seats`, `skills_dir`,
    /// `environment_gates`, `verify`) parses as a `MachineConfig` once uncommented — the
    /// example cannot drift from the struct. (`seats:` replaces the active `roles:`: one of
    /// the two per file.)
    #[test]
    fn the_commented_m11_machine_block_parses() {
        let t = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
        let text = std::fs::read_to_string(t.join("toryo.local.yaml.example"))
            .unwrap()
            .replace("\r\n", "\n");
        let lines: Vec<&str> = text.lines().collect();
        let start = lines
            .iter()
            .position(|l| l.starts_with("# M11 —"))
            .expect("the M11 block");
        let key = regex::Regex::new(r"^ *[A-Za-z_][A-Za-z0-9_]*:(\s|$)").unwrap();
        let top = |l: &str| key.is_match(l) && !l.starts_with(' ');
        // the active part without its `roles:` block
        let mut active = Vec::new();
        let mut in_roles = false;
        for l in &lines[..start] {
            if top(l) {
                in_roles = l.starts_with("roles:");
            }
            if !in_roles {
                active.push(l.to_string());
            }
        }
        let mut uncommented = 0;
        for l in &lines[start..] {
            match l.strip_prefix("# ") {
                Some(c) if key.is_match(c) => {
                    active.push(c.to_string());
                    uncommented += 1;
                }
                _ => active.push(l.to_string()),
            }
        }
        assert!(uncommented >= 15, "{uncommented}");
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("machine.yaml");
        std::fs::write(&p, active.join("\n")).unwrap();
        let m = MachineConfig::load(&p).unwrap();
        assert!(m.seats_spelling);
        assert_eq!(m.roles["planner"].profile, "claude-foundry");
        assert_eq!(m.skills_dir.as_deref(), Some("C:/Temp/AgentShare/skills"));
        let g = &m.environment_gates["dbsync"];
        assert_eq!(
            (g.program.as_str(), g.lock.as_str()),
            ("powershell", "d365-env")
        );
        assert!(g.secret_env.contains_key("D365_SYNC_SECRET"));
        assert_eq!(m.verify.min_boundary.as_deref(), Some("low_integrity"));
    }

    /// M12-08 (X20): the keys `keys_without_effect` announces are read by no engine code. When
    /// one starts being used this test fails — take it off the list (and out of the notes), so
    /// that the message "has no effect" stays true.
    #[test]
    fn keys_without_effect_are_read_nowhere() {
        fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    rs_files(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        rs_files(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let mut seen = 0;
        for f in files {
            if f.file_name().is_some_and(|n| n == "config.rs") {
                continue;
            }
            seen += 1;
            let text = std::fs::read_to_string(&f).unwrap();
            for field in [".runs_root", ".final_clean", ".budget_usd_est"] {
                assert!(
                    !text.contains(field),
                    "{field} is read in {} — it has an effect now: update Effective::keys_without_effect",
                    f.display()
                );
            }
        }
        assert!(seen > 20, "the scan found the sources ({seen} files)");
        // and the list itself: set to a non-default value, a key is announced
        let mut e = Effective::load(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("templates")
                .join("toryo.yaml.example"),
            Some(
                &Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("templates")
                    .join("toryo.local.yaml.example"),
            ),
        )
        .unwrap();
        assert!(e.keys_without_effect().is_empty());
        e.repo.verify.final_clean = false;
        e.repo.pipeline.budget_usd_est = Some(1.0);
        e.repo.project.runs_root = "D:/x".into();
        assert_eq!(
            e.keys_without_effect(),
            [
                "project.runs_root",
                "verify.final_clean",
                "pipeline.budget_usd_est"
            ]
        );
    }

    /// M13-14 (4): a configuration **without** `project.base_branch` (M12-08 made the key
    /// optional) freezes to the digest it had before M12. The value is the one the pre-M12
    /// source pins for this template in `templates_keep_their_m10_hash` (computed with `main` @
    /// aa0be03, unchanged at 4ad97c3): before M12 the key defaulted to `main`, so the template
    /// with its `base_branch: main` line and without it were the same configuration.
    #[test]
    fn a_configuration_without_base_branch_keeps_its_pre_m12_digest() {
        let t = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
        let text = std::fs::read_to_string(t.join("toryo.yaml.example"))
            .unwrap()
            .replace("\r\n", "\n");
        assert!(
            text.contains("\n  base_branch: main\n"),
            "the template sets the key"
        );
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("toryo.yaml");
        std::fs::write(&p, text.replace("\n  base_branch: main\n", "\n")).unwrap();
        let cfg = Effective::load(&p, Some(&t.join("toryo.local.yaml.example"))).unwrap();
        assert_eq!(cfg.repo.project.base_branch, None);
        assert_eq!(
            cfg.sha256(),
            "7e2e52195e51597e2b220e46c99dc6fa8aa2c6b9e08e4707da102b4bcf087e0f",
            "{}",
            cfg.canonical_json()
        );
    }

    /// M13-14 (6): `keys_without_effect` in the other direction — every key of the repository
    /// file (the last name of every path of a loaded configuration; `profiles` is the machine's)
    /// is read by engine code outside `config.rs`, or is on the no-effect list. A key that is
    /// accepted, does nothing and is not announced fails here.
    #[test]
    fn every_key_of_the_repository_file_is_read_or_announced() {
        fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    rs_files(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        fn leaves(at: &str, v: &serde_json::Value, out: &mut Vec<(String, String)>) {
            match v.as_object().filter(|o| !o.is_empty()) {
                Some(o) => {
                    for (k, v) in o {
                        leaves(&format!("{at}.{k}"), v, out);
                    }
                }
                None => {
                    let name = at.rsplit('.').next().unwrap_or(at).to_string();
                    out.push((at.trim_start_matches('.').to_string(), name));
                }
            }
        }
        let mut files = Vec::new();
        rs_files(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let sources: String = files
            .iter()
            .filter(|f| f.file_name().is_some_and(|n| n != "config.rs"))
            .map(|f| std::fs::read_to_string(f).unwrap())
            .collect();
        let t = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
        let e = Effective::load(
            &t.join("toryo.yaml.example"),
            Some(&t.join("toryo.local.yaml.example")),
        )
        .unwrap();
        let mut repo = serde_json::to_value(&e.repo).unwrap();
        repo.as_object_mut().unwrap().remove("profiles");
        let mut keys = Vec::new();
        leaves("", &repo, &mut keys);
        assert!(keys.len() > 20, "{keys:?}");
        let no_effect = ["runs_root", "final_clean", "budget_usd_est"];
        let unread: Vec<&str> = keys
            .iter()
            .filter(|(_, name)| !no_effect.contains(&name.as_str()))
            .filter(|(_, name)| !sources.contains(&format!(".{name}")))
            .map(|(path, _)| path.as_str())
            .collect();
        assert!(
            unread.is_empty(),
            "accepted, read by no engine code and not announced as `has no effect`: {unread:?}"
        );
    }

    /// M12-07 (X17): `confirm` is `never`, `first` or `always`; `never` is the default and is
    /// left out of the frozen configuration, so an existing configuration keeps its hash.
    #[test]
    fn the_confirm_key_of_an_environment_gate() {
        let gate = |extra: &str| {
            format!("environment_gates:\n  dbsync:\n    program: powershell\n    lock: d365-env\n{extra}")
        };
        let d = tempfile::tempdir().unwrap();
        let load = |text: &str| {
            let p = d.path().join("m.yaml");
            std::fs::write(&p, text).unwrap();
            MachineConfig::load(&p)
        };
        for (word, want) in [
            ("first", Confirm::First),
            ("always", Confirm::Always),
            ("never", Confirm::Never),
        ] {
            let m = load(&gate(&format!("    confirm: {word}\n"))).unwrap();
            assert_eq!(m.environment_gates["dbsync"].confirm, want);
        }
        let none = load(&gate("")).unwrap();
        let g = &none.environment_gates["dbsync"];
        assert_eq!(g.confirm, Confirm::Never);
        assert!(
            !serde_json::to_string(g).unwrap().contains("confirm"),
            "never is not frozen: the digest of an existing configuration stays"
        );
        let first = load(&gate("    confirm: first\n")).unwrap();
        assert!(serde_json::to_string(&first.environment_gates["dbsync"])
            .unwrap()
            .contains("\"confirm\":\"first\""));
        let err = format!("{:#}", load(&gate("    confirm: sometimes\n")).unwrap_err());
        assert!(err.contains("sometimes") && err.contains("first"), "{err}");
    }

    /// The shipped templates keep loading under the checks.
    #[test]
    fn templates_load() {
        let t = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
        let cfg = Effective::load(
            &t.join("toryo.yaml.example"),
            Some(&t.join("toryo.local.yaml.example")),
        )
        .unwrap();
        assert!(cfg.trusted_filters.contains_key("lfs"));
    }

    /// M9-C1 AC-1/AC-2: the D365FO starter kit — the repository and machine parts load together
    /// (a `tool: xpp` build with the machine's toolchain, no profiles in the repository part);
    /// every file starts with its `TODO(owner)` items; the policy files stay small (plan §7:
    /// never trimmed); the task example parses.
    #[test]
    fn d365fo_templates_load() {
        let t = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("templates")
            .join("d365fo");
        let cfg = Effective::load(
            &t.join("toryo.yaml.example"),
            Some(&t.join("toryo.local.yaml.example")),
        )
        .unwrap();
        assert!(cfg.repo.commands.verify[0].is_xpp());
        assert!(cfg.toolchains.xpp.is_some());
        // (4 since M12-18: the machine example also defines `codex-openai`, the org chart's
        // implementer; it was 3)
        assert_eq!(cfg.origins.profiles.len(), 4);
        assert!(cfg.repo.policy.deny_paths.iter().any(|d| d == "**/ZZZ/**"));
        assert_eq!(cfg.repo.verify.checkout, "clean");
        let mut files = vec![
            t.join("toryo.yaml.example"),
            t.join("toryo.local.yaml.example"),
            t.join("TASK.example.md"),
        ];
        let ctx = t.join(".toryo").join("context");
        let mut policy: Vec<PathBuf> = std::fs::read_dir(&ctx)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        policy.sort();
        let names: Vec<String> = policy
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "10-d365fo-models.md",
                "20-naming.md",
                "30-labels.md",
                "40-build.md",
                "50-xpp-roles.md"
            ]
        );
        for p in &policy {
            let size = std::fs::metadata(p).unwrap().len();
            assert!(size <= 2048, "{}: {size} bytes", p.display());
        }
        files.extend(policy);
        for f in files {
            // the leading comment: `#` lines of a YAML file, the first line (`<!-- … -->`) of
            // a Markdown file
            let text = std::fs::read_to_string(&f).unwrap();
            let head: Vec<&str> = if f.extension().is_some_and(|x| x == "md") {
                text.lines().take(1).collect()
            } else {
                text.lines().take_while(|l| l.starts_with('#')).collect()
            };
            assert!(
                head.iter().any(|l| l.contains("TODO(owner)")),
                "{}: TODO(owner) items at the top",
                f.display()
            );
        }
        let task = std::fs::read_to_string(t.join("TASK.example.md")).unwrap();
        assert_eq!(
            crate::engine::checks::parse_acceptance_ids(&task).unwrap(),
            ["AC-1", "AC-2", "AC-3", "AC-4", "AC-5"]
        );
    }

    /// M9-D1 AC-1/AC-2: the dogfood example loads with a machine config that defines its
    /// profiles, runs `scripts/verify.ps1` (protected, hashed) in a tracked checkout, and the
    /// script keeps cargo from rewriting Cargo.lock.
    #[test]
    fn dogfood_template_loads() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let d = tempfile::tempdir().unwrap();
        let m = d.path().join("machine.yaml");
        std::fs::write(&m, "profiles:\n  claude-max: { kind: claude, billing: subscription, exe: claude }\n  codex-openai: { kind: codex, billing: subscription, exe: codex, codex: { windows_sandbox: unelevated } }\n").unwrap();
        let cfg = Effective::load(
            &root
                .join("templates")
                .join("dogfood")
                .join("toryo.yaml.example"),
            Some(&m),
        )
        .unwrap();
        assert_eq!(cfg.repo.verify.checkout, "tracked");
        assert!(cfg.repo.verify.final_clean);
        let v = &cfg.repo.commands.verify;
        assert_eq!(
            v[0].kind(),
            CommandKind::Program {
                program: "pwsh",
                args: &["-NoProfile", "-File", "scripts/verify.ps1"].map(String::from)
            }
        );
        assert_eq!(
            crate::engine::checks::verify_files(v, root),
            ["scripts/verify.ps1"]
        );
        let script = std::fs::read_to_string(root.join("scripts").join("verify.ps1")).unwrap();
        for want in [
            "--locked",
            "CARGO_HOME",
            "CARGO_TARGET_DIR",
            "exit $LASTEXITCODE",
        ] {
            assert!(script.contains(want), "{want}");
        }
    }

    /// EC-28: `%ProgramFiles(x86)%`-style names expand; unknown ones stay an error.
    #[test]
    fn expand_vars_takes_parenthesised_names() {
        std::env::set_var("TORYO_TEST_DIR(x86)", "C:\\PF86");
        assert_eq!(
            expand_vars("%TORYO_TEST_DIR(x86)%\\Git\\bin\\git.exe").unwrap(),
            "C:\\PF86\\Git\\bin\\git.exe"
        );
        let e = expand_vars("%TORYO_TEST_UNSET(x86)%\\x").unwrap_err();
        assert!(
            format!("{e:#}").contains("%TORYO_TEST_UNSET(x86)% is not set"),
            "{e:#}"
        );
        // not a variable reference: left alone
        assert_eq!(expand_vars("50% (approx)").unwrap(), "50% (approx)");
    }
}
