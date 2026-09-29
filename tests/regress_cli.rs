//! Regression tests for the configuration fixes of the 2026-09-29 review (the review's
//! probes, inverted):
//!
//! - EC-23 / DES-05: `fallback: []` means no fallback; typo'd role keys and enum-like values
//!   are refused at load
//! - EC-28: `%ProgramFiles(x86)%` is expanded (or refused), never left in place
//!
//! EC-27 (`doctor` version pin) is a unit test next to the private code (`doctor.rs`).

#[cfg(feature = "m0-kit")]
mod common;

use toryo::config::{expand_vars, Effective};

fn load(yaml: &str) -> anyhow::Result<Effective> {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("toryo.yaml");
    std::fs::write(&p, yaml).unwrap();
    Effective::load(&p, None)
}

const TWO_PROFILES: &str = "version: 1\nproject: { root: r, worktrees_root: w }\nprofiles:\n  claude-max: { kind: claude, billing: subscription, exe: c }\n  codex-openai: { kind: codex, billing: subscription, exe: x }\nroles:\n  planner: { profile: claude-max }\n";

/// EC-23 (probe `implicit_implementer_fallback`, inverted): `fallback: []` is no fallback; an
/// absent `fallback` keeps the default (the planner's profile); an absent reviewer is the planner.
#[test]
fn ec23_empty_fallback_means_no_fallback() {
    let chain = |implementer: &str| {
        let cfg = load(&format!("{TWO_PROFILES}  implementer: {implementer}\n")).unwrap();
        toryo::engine::task::roles(&cfg).unwrap()
    };
    let (_, c, reviewer) = chain("{ profile: codex-openai, fallback: [] }");
    assert_eq!(c, vec!["codex-openai"], "`fallback: []` adds nothing");
    assert_eq!(reviewer, "claude-max");
    let (_, c, _) = chain("{ profile: codex-openai }");
    assert_eq!(c, vec!["codex-openai", "claude-max"], "absent: the planner");
    let (_, c, _) = chain("{ profile: codex-openai, fallback: [claude-max] }");
    assert_eq!(c, vec!["codex-openai", "claude-max"]);
}

/// EC-23 / DES-05 (probe `unknown_role_key_is_accepted`, inverted).
#[test]
fn ec23_unknown_role_keys_and_enum_values_are_refused() {
    let e = load(
        "version: 1\nproject: { root: r, worktrees_root: w, runs_root: 'D:\\toryo-runs' }\nprofiles:\n  claude-max: { kind: claude, billing: subscription, exe: c }\nroles:\n  planner: { profile: claude-max }\n  implementor: { profile: claude-max }\n  reviwer: { profile: claude-max }\nverify: { boundary: lowintegrity, checkout: tracked, final_clean: true }\n",
    )
    .expect_err("typos in role names / enum-like strings are refused");
    let e = format!("{e:#}");
    for want in [
        "roles.implementor: unknown role (valid: planner, implementer, reviewer)",
        "roles.reviwer: unknown role",
        "verify.boundary: `lowintegrity` is not a valid value (valid: low_integrity, none)",
    ] {
        assert!(e.contains(want), "{want:?} missing in:\n{e}");
    }
}

/// EC-28 (probe `expand_vars_parenthesised_names`, inverted): never left unexpanded.
#[test]
fn ec28_parenthesised_variable_names_are_expanded_or_refused() {
    match expand_vars("%ProgramFiles(x86)%\\Git\\bin\\git.exe") {
        Ok(s) => assert!(!s.contains('%'), "expanded: {s}"),
        Err(e) => assert!(
            format!("{e:#}").contains("%ProgramFiles(x86)% is not set"),
            "{e:#}"
        ),
    }
}

#[cfg(feature = "m0-kit")]
mod e2e {
    use super::common::*;

    /// (exit code, stdout, stderr)
    fn toryo_out(env: &Env, args: &[&str]) -> (i32, String, String) {
        let o = command(env, args, &[]).output().unwrap();
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).to_string(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        )
    }

    /// EC-23 end to end: the frozen chain of a task follows `fallback: []`; a typo'd role key
    /// is refused by `task create` and reported by `doctor`.
    #[test]
    fn ec23_task_create_honours_empty_fallback_and_refuses_typos() {
        let env = setup("codex-openai");
        create(&env, "F-0");
        assert_eq!(
            task(&env, "F-0").implementers,
            vec!["codex-openai", "claude-max"],
            "absent fallback: the planner's profile, as before"
        );
        let text = std::fs::read_to_string(&env.config).unwrap();
        let empty = text.replace(
            "implementer: { profile: codex-openai }",
            "implementer: { profile: codex-openai, fallback: [] }",
        );
        assert_ne!(empty, text);
        std::fs::write(&env.config, &empty).unwrap();
        create(&env, "F-1");
        assert_eq!(task(&env, "F-1").implementers, vec!["codex-openai"]);
        std::fs::write(
            &env.config,
            empty.replace(
                "  planner: { profile: claude-max }\n",
                "  planner: { profile: claude-max }\n  reviwer: { profile: codex-openai }\n",
            ),
        )
        .unwrap();
        let (code, err) = toryo(
            &env,
            &["task", "create", "--file", "TASK.md", "--id", "F-2"],
            &[],
        );
        assert_eq!(code, 2, "{err}");
        assert!(err.contains("roles.reviwer: unknown role"), "{err}");
        let (code, out, _) = toryo_out(&env, &["doctor"]);
        assert_eq!(code, 1, "{out}");
        assert!(
            out.lines()
                .any(|l| l.starts_with("[FAIL] config") && l.contains("invalid configuration")),
            "{out}"
        );
    }
}
