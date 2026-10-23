//! `toryo.yaml` + machine config (`%USERPROFILE%\.toryo\toryo.local.yaml`), plan §14 / R35 / R37.
//!
//! The effective configuration of a task is the repo `toryo.yaml` merged with the
//! machine config, serialised canonically (sorted keys) and hashed. Agent profiles come
//! only from the machine config (R60): a repository file that defines `profiles:` or
//! `toolchains:` does not load. It is frozen per task (`config.json` artifact, R35).
//!
//! R23-33: the types, the effective configuration, its checks, the machine/repository guard,
//! the Codex override policy, paths and variables, and secrets are modules of their own.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod types;
pub use types::*;
mod effective;
pub use effective::*;
mod validate;
pub use validate::*;
mod codex_policy;
pub use codex_policy::*;
mod guard;
pub use guard::*;
mod env;
pub use env::*;
mod secrets;
pub use secrets::*;

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

    /// R23-19 (review 2026-10-23): a tag below the top level is refused too, with where it is.
    /// Was: only the top level was looked at; the typed parse dropped a nested tag silently.
    #[test]
    fn a_nested_tag_is_refused() {
        let doc: serde_yaml::Value = serde_yaml::from_str(
            "version: 1\nroles:\n  planner: { profile: !x evil, skills: [ !y s1, s2 ] }\n",
        )
        .unwrap();
        let e = format!("{:#}", refuse_tags(&doc, Path::new("t.yaml")).unwrap_err());
        assert!(
            e.contains("the value of `roles.planner.profile` has the tag `!x`")
                && e.contains("the value of `roles.planner.skills.[0]` has the tag `!y`"),
            "{e}"
        );
        let plain: serde_yaml::Value =
            serde_yaml::from_str("version: 1\nroles:\n  planner: { profile: p, skills: [s1] }\n")
                .unwrap();
        refuse_tags(&plain, Path::new("t.yaml")).unwrap();
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

    /// R23-11: numbers that feed time and budget arithmetic are bounded at load. Was: a
    /// `timeouts.agent_stage_max_min` of 2^64-1 wrapped `60 * min` and the turn's deadline
    /// panicked (`Instant + Duration`) after the agent process had started; `context.budget_kb`
    /// near 2^54 wrapped `kb * 1024` to a few bytes; a gate `timeout_min` the same.
    #[test]
    fn numbers_that_feed_arithmetic_are_bounded() {
        let big = u64::MAX;
        let e = load_err(&format!(
            "{SAMPLE}timeouts: {{ agent_stall_min: {big}, agent_stage_max_min: 10081 }}\ncontext: {{ budget_kb: {{ implementer: 18014398509481985 }} }}\npipeline: {{ budget_tokens: {big} }}\n"
        ));
        for want in [
            "timeouts.agent_stall_min: `18446744073709551615` is too large (at most 10080",
            "timeouts.agent_stage_max_min: `10081` is too large",
            "context.budget_kb.implementer: `18014398509481985` is too large (at most 1048576 KB)",
            "pipeline.budget_tokens: `18446744073709551615` is too large",
        ] {
            assert!(e.contains(want), "{want:?} missing in:\n{e}");
        }
        load_str(&format!(
            "{SAMPLE}timeouts: {{ agent_stall_min: 10080, agent_stage_max_min: 10080 }}\ncontext: {{ budget_kb: {{ implementer: 1048576 }} }}\npipeline: {{ budget_tokens: {MAX_BUDGET_TOKENS} }}\n"
        ))
        .unwrap();
        // a gate's `timeout` (org chart) and `timeout_min`
        assert!(crate::team::parse_duration("168h").is_ok());
        let e = format!("{:#}", crate::team::parse_duration("169h").unwrap_err());
        assert!(e.contains("longer than a week"), "{e}");
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
            // the list itself lives in config/effective.rs (R23-33: it was config.rs)
            if f.file_name().is_some_and(|n| n == "config.rs") || f.ends_with("config/effective.rs")
            {
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
