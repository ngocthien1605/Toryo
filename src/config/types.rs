//! The configuration's types (R23-33, moved out of `config.rs` as they were): the repository file,
//! the machine config, profiles, gates, toolchains, where each entry came from.

use super::*;

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

impl Billing {
    /// The name in a usage row and in the configuration (`rename_all = "lowercase"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Billing::Subscription => "subscription",
            Billing::Foundry => "foundry",
            Billing::Api => "api",
        }
    }
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

pub(super) fn serialize_base_branch<S: serde::Serializer>(
    v: &Option<String>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(v.as_deref().unwrap_or("main"))
}

pub(super) fn default_runs_root() -> String {
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

pub(super) fn default_wire_api() -> String {
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

pub(super) fn default_sandbox_mode() -> String {
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
    /// M17-B1 (AC-12): implementer only — 2..=4 machine profiles (repeats allowed) that implement
    /// the approved plan side by side in the first Implementing stage; the best one continues
    /// (best-of-N). Serialised only when set, so a configuration without it keeps its hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<String>>,
}

/// M17-B1: the fewest and the most implementer candidates.
pub const MIN_CANDIDATES: usize = 2;
pub const MAX_CANDIDATES: usize = 4;

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

pub(super) fn default_checkout() -> String {
    "clean".into()
}

pub(super) fn default_boundary() -> String {
    "low_integrity".into()
}

pub(super) fn default_verify_root() -> String {
    "%LOCALAPPDATA%\\toryo-verify".into()
}

pub(super) fn default_true() -> bool {
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
    /// M17-A1: the red-green proof of a `program` check gate ([`RedGreen`]). Serialised only
    /// when set, so a configuration without it keeps its hash (AC-13).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub red_green: Option<RedGreen>,
}

/// M17-A1 (X47): the `red_green` block of a `kind: check` gate or a `commands.verify` entry.
/// Every acceptance criterion names the tests that prove it (`IMPLEMENTATION.json` `proof`);
/// each runs once per side with [`TEST_PLACEHOLDER`] replaced by its id — on the checkpoint it
/// must pass (green), on the base commit with the task's test files laid over it it must fail
/// (red). Checked at load ([`super::validate`]): only on a `program` check gate, `{test}`
/// exactly once in `args` and never in `program`, `tests` not empty and every glob valid, a
/// valid regex, `max_tests` 1..=64.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedGreen {
    /// Globs (repository-relative, the write fence's rules and case) of the test files: what
    /// the red commit takes from the checkpoint.
    pub tests: Vec<String>,
    /// The arguments of one test's run (the gate's `program`); `{test}` exactly once, as a whole
    /// element or inside one (`--filter=Name~{test}`).
    pub args: Vec<String>,
    /// A green run's output must match this regex (catches "0 tests ran").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub green_output_must_match: Option<String>,
    /// The most distinct test ids a checkpoint may declare (1..=64, default 32).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tests: Option<u32>,
}

/// M17-A1: what `red_green.args` holds where the test id goes.
pub const TEST_PLACEHOLDER: &str = "{test}";

/// M17-A1: `red_green.max_tests` when unset, and its bounds.
pub const DEFAULT_MAX_TESTS: u32 = 32;
pub const MAX_MAX_TESTS: u32 = 64;

impl RedGreen {
    /// The distinct test ids a checkpoint may declare.
    pub fn max_tests(&self) -> usize {
        self.max_tests.unwrap_or(DEFAULT_MAX_TESTS) as usize
    }

    /// The arguments of the run of test `id`: every `{test}` replaced by it (load-time
    /// validation makes it exactly one). The id is passed as argv — never through a shell
    /// Toryo builds.
    pub fn args_for(&self, id: &str) -> Vec<String> {
        self.args
            .iter()
            .map(|a| a.replace(TEST_PLACEHOLDER, id))
            .collect()
    }

    /// The block in one line (`gate list`, `doctor`, `config show`): the per-test command of the
    /// gate's `program`, the test globs, the cap and the output rule.
    pub fn words(&self, program: Option<&str>) -> String {
        let argv: Vec<&str> = std::iter::once(program.unwrap_or_default())
            .chain(self.args.iter().map(String::as_str))
            .collect();
        format!(
            "red-green (inside a task only): per test `{}` · tests {} · at most {} test ids{}",
            argv.join(" "),
            self.tests.join(", "),
            self.max_tests(),
            match &self.green_output_must_match {
                Some(r) => format!(" · a green run must match `{r}`"),
                None =>
                    " · no green output rule (a filter that matches no test passes green)".into(),
            }
        )
    }
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
            red_green: None,
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
            red_green: None,
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
pub(super) struct RepoFile {
    pub(super) version: u32,
    pub(super) project: Project,
    #[serde(default)]
    pub(super) roles: BTreeMap<String, Role>,
    #[serde(default)]
    pub(super) pipeline: Pipeline,
    #[serde(default)]
    pub(super) policy: Policy,
    #[serde(default)]
    pub(super) verify: Verify,
    #[serde(default)]
    pub(super) commands: Commands,
    #[serde(default)]
    pub(super) timeouts: Timeouts,
    #[serde(default)]
    pub(super) context: ContextCfg,
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
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_bytes(path, &bytes)
    }

    /// [`Self::load`] of bytes already read (R23-19: `Effective::load` parses the bytes it
    /// hashed, not a second read of the file).
    pub fn from_bytes(path: &Path, bytes: &[u8]) -> Result<Self> {
        let text = crate::textfile::decode(path, bytes)?;
        // R23-19: the rules of a repository file — a duplicate key is refused (the typed parse
        // would keep the last one silently), and so is a YAML tag
        let doc: serde_yaml::Value =
            serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let tags = tag_problems(&doc);
        if !tags.is_empty() {
            bail!(
                "{}: YAML tags are not allowed in the machine config — remove them:\n  - {}",
                path.display(),
                tags.join("\n  - ")
            );
        }
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

#[cfg(test)]
mod m17_red_green_tests {
    use super::*;

    /// Edge case 15: `{test}` is replaced inside an element and as a whole element, nowhere else.
    #[test]
    fn m17_ec15_the_test_id_is_substituted_inside_an_element_or_as_one() {
        let rg = |args: &[&str]| RedGreen {
            tests: vec!["tests/**".into()],
            args: args.iter().map(|a| a.to_string()).collect(),
            green_output_must_match: None,
            max_tests: None,
        };
        assert_eq!(
            rg(&["test", "--filter=FullyQualifiedName~{test}", "--no-build"])
                .args_for("Calc.Divides"),
            vec![
                "test",
                "--filter=FullyQualifiedName~Calc.Divides",
                "--no-build"
            ]
        );
        assert_eq!(
            rg(&["--exact", "{test}"]).args_for("divide::by_zero"),
            vec!["--exact", "divide::by_zero"]
        );
    }
}
