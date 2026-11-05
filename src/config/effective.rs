//! The effective configuration (R23-33, moved as it was): the repository file merged with the
//! machine config, frozen per task (R35) and hashed.

use super::*;

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
                Some((mp.to_path_buf(), MachineConfig::from_bytes(mp, &b)?))
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
    pub(super) fn validate_with(&self, mut problems: Vec<String>) -> Result<()> {
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
        // M17-A3 (X48): one proof map per checkpoint, so one red-green gate — a test id of one
        // runner is no test id of another
        let rg: Vec<&str> = self
            .repo
            .commands
            .verify
            .iter()
            .filter(|c| c.red_green.is_some())
            .map(|c| c.name.as_str())
            .collect();
        if rg.len() > 1 {
            problems.push(format!(
                "red_green: {} gates have a `red_green` block ({}) — the implementer's proof map names the tests of one test runner: keep `red_green` on one gate",
                rg.len(),
                rg.join(", ")
            ));
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

    /// The billing kind of `profile` as a usage row and `toryo usage` name it; `subscription`
    /// for a profile this configuration does not know (R23-31: one place, was two copies).
    pub fn billing_of(&self, profile: &str) -> &'static str {
        self.profile(profile)
            .map(|p| p.billing.as_str())
            .unwrap_or("subscription")
    }
}
