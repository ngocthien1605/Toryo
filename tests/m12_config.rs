//! M12 configuration and CLI tests: `project.base_branch` (M12-08) and the keys that do
//! nothing, and the other config behaviour of the milestone as it comes.
#![cfg(feature = "m0-kit")]

mod common;
use common::*;

use serde_json::Value;

/// The fixture repository with `feature/wip` checked out, one commit ahead of `main`.
fn on_a_feature_branch(env: &Env) -> (String, String) {
    let main = git(&env.repo, &["rev-parse", "main"]).trim().to_string();
    git(&env.repo, &["checkout", "-q", "-b", "feature/wip"]);
    commit_repo(env, &[("wip.txt", "work in progress\n")], "wip");
    let feature = git(&env.repo, &["rev-parse", "HEAD"]).trim().to_string();
    assert_ne!(main, feature);
    (main, feature)
}

fn set_project_key(env: &Env, line: &str) {
    let y = std::fs::read_to_string(&env.config).unwrap();
    assert!(y.contains("  worktrees_root:"), "{y}");
    std::fs::write(
        &env.config,
        y.replacen(
            "  worktrees_root:",
            &format!("  {line}\n  worktrees_root:"),
            1,
        )
        .replace("    base_branch", "  base_branch")
        .replace("    runs_root", "  runs_root"),
    )
    .unwrap();
}

/// AC-1: `project.base_branch: main` with `feature/wip` checked out — the task branches from
/// `main`, says so, and notes that another branch is checked out. (Was: it branched from the
/// feature commit; the key was read nowhere.)
#[test]
fn m12_08_base_branch_is_the_base_of_task_create() {
    let env = setup("codex-openai");
    let (main, _feature) = on_a_feature_branch(&env);
    set_project_key(&env, "base_branch: main");
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "B-1"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "B-1").base_commit, main);
    assert!(
        err.contains(&format!("base: main @ {}", &main[..12])),
        "{err}"
    );
    assert!(
        err.contains("note: feature/wip is checked out") && err.contains("project.base_branch"),
        "{err}"
    );
    // `--base` still wins
    let (code, err) = toryo(
        &env,
        &[
            "task",
            "create",
            "--file",
            "TASK.md",
            "--id",
            "B-1b",
            "--base",
            "feature/wip",
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        task(&env, "B-1b").base_commit,
        git(&env.repo, &["rev-parse", "feature/wip"]).trim()
    );
    // a branch that does not exist is an error that names the key
    let y = std::fs::read_to_string(&env.config).unwrap();
    assert!(y.contains("  base_branch: main\n"), "{y}");
    std::fs::write(
        &env.config,
        y.replacen("  base_branch: main\n", "  base_branch: nope\n", 1),
    )
    .unwrap();
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "B-1c"],
        &[],
    );
    assert_ne!(code, 0);
    assert!(
        err.contains("project.base_branch: the branch `nope` does not exist"),
        "{err}"
    );
}

/// AC-2: no key — today's behaviour: the checked-out branch, and no note about another one.
#[test]
fn m12_08_without_the_key_the_checked_out_branch_is_the_base() {
    let env = setup("codex-openai");
    let (_main, feature) = on_a_feature_branch(&env);
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "B-2"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "B-2").base_commit, feature);
    assert!(err.contains("base: feature/wip @"), "{err}");
    assert!(!err.contains("is checked out"), "{err}");
}

/// AC-3: a key that is accepted and read nowhere says so in `config show`, `doctor` and
/// `task create`; a configuration without them says nothing.
#[test]
fn m12_08_keys_without_effect_say_so() {
    let env = setup("codex-openai");
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "K-0"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains("has no effect"), "{err}");
    set_project_key(&env, "runs_root: 'C:/somewhere/else'");
    let y = std::fs::read_to_string(&env.config).unwrap();
    std::fs::write(
        &env.config,
        format!("{y}pipeline:\n  budget_usd_est: 5.0\n").replace(
            "verify: { boundary: none,",
            "verify: { final_clean: false, boundary: none,",
        ),
    )
    .unwrap();
    let lines = [
        "`project.runs_root` is accepted but has no effect in this version",
        "`verify.final_clean` is accepted but has no effect in this version",
        "`pipeline.budget_usd_est` is accepted but has no effect in this version",
    ];
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", "TASK.md", "--id", "K-1"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    for l in lines {
        assert!(err.contains(l), "create: {l}\n{err}");
    }
    let o = command(&env, &["doctor"], &[]).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    for l in lines {
        assert!(out.contains(l), "doctor: {l}\n{out}");
    }
    let o = command(&env, &["config", "show"], &[]).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    for l in lines {
        assert!(out.contains(l), "config show: {l}\n{out}");
    }
    let o = command(&env, &["config", "show", "--json"], &[])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        v["no_effect"],
        serde_json::json!([
            "project.runs_root",
            "verify.final_clean",
            "pipeline.budget_usd_est"
        ])
    );
}

// ── M12-09 (X21): `./toryo.yaml` before `$TORYO_CONFIG` ───────────────────────────────────

/// `toryo config show --json` run from `cwd` without `--config`, with `$TORYO_CONFIG` set to
/// `var` (when given): the path of the repository config it read, and its stderr.
fn config_in_use(
    env: &Env,
    cwd: &std::path::Path,
    var: Option<&std::path::Path>,
) -> (String, String) {
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_toryo"));
    c.arg("--machine-config")
        .arg(&env.machine)
        .args(["config", "show", "--json"])
        .env("TORYO_DATA_DIR", &env.data)
        .env_remove("TORYO_CONFIG")
        .current_dir(cwd)
        .stdin(std::process::Stdio::null());
    if let Some(v) = var {
        c.env("TORYO_CONFIG", v);
    }
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    let path = v["files"][0]["path"].as_str().unwrap().to_string();
    (path, String::from_utf8_lossy(&o.stderr).to_string())
}

/// AC-1/AC-2: both present and different — the file of the folder you stand in is used and
/// one note names both; only the variable — as before; neither flag nor file nor variable is
/// an error that names `toryo.yaml`.
#[test]
fn m12_09_the_local_config_comes_before_the_variable() {
    let env = setup("codex-openai");
    let here = env.root.join("here");
    let elsewhere = env.root.join("elsewhere");
    std::fs::create_dir_all(&here).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    let text = std::fs::read_to_string(&env.config).unwrap();
    std::fs::write(here.join("toryo.yaml"), &text).unwrap();
    let other = elsewhere.join("other.yaml");
    std::fs::write(&other, &text).unwrap();
    let norm = |p: &str| {
        p.replace(std::path::MAIN_SEPARATOR, "/")
            .trim_start_matches("//?/")
            .to_lowercase()
    };
    // both present: the local file, and the note
    let (used, err) = config_in_use(&env, &here, Some(&other));
    // the local file is named as `toryo.yaml` (relative to the folder the command ran in)
    assert_eq!(used, "toryo.yaml", "{used}");
    assert!(
        err.contains("note: using")
            && err.contains("other.yaml")
            && err.contains("is ignored here"),
        "{err}"
    );
    // the same file twice: no note
    let (_, err) = config_in_use(&env, &here, Some(&here.join("toryo.yaml")));
    assert!(!err.contains("note: using"), "{err}");
    // only the variable (nothing in the folder): as today
    let (used, err) = config_in_use(&env, &elsewhere, Some(&other));
    assert!(norm(&used).ends_with("/elsewhere/other.yaml"), "{used}");
    assert!(!err.contains("note: using"), "{err}");
    // only the file: it
    let (used, _) = config_in_use(&env, &here, None);
    assert!(norm(&used).ends_with("toryo.yaml"), "{used}");
}

// ── M12-10 (X22): text files are UTF-8, a byte-order mark is ignored ──────────────────────

/// The fixture TASK.md with its heading replaced by `# BOM-7 …` and `lead` bytes in front.
fn task_file_with(env: &Env, name: &str, lead: &[u8]) -> std::path::PathBuf {
    let text = std::fs::read_to_string(env.repo.join("TASK.md")).unwrap();
    let body: Vec<&str> = text
        .lines()
        .filter(|l| !l.starts_with("# "))
        .collect::<Vec<_>>();
    let mut bytes = lead.to_vec();
    bytes.extend(format!("# BOM-7 a title\n{}\n", body.join("\n")).bytes());
    let p = env.root.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

/// AC-2 and AC-3: a `TASK.md` with a byte-order mark gives the id from its heading (was: the
/// file name, because `^#` did not match behind the mark); a UTF-16 one is refused with its
/// name.
#[test]
fn m12_10_task_md_with_a_bom_has_the_right_id_and_utf16_is_refused() {
    let env = setup("codex-openai");
    let p = task_file_with(&env, "with-bom.md", &[0xEF, 0xBB, 0xBF]);
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", &p.display().to_string()],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(task(&env, "BOM-7").id, "BOM-7");
    // UTF-16 with a mark
    let text = std::fs::read_to_string(&p).unwrap();
    let mut b = vec![0xFF, 0xFE];
    for u in text.trim_start_matches('\u{feff}').encode_utf16() {
        b.extend(u.to_le_bytes());
    }
    let u16p = env.root.join("utf16.md");
    std::fs::write(&u16p, b).unwrap();
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", &u16p.display().to_string()],
        &[],
    );
    assert_ne!(code, 0);
    assert!(
        err.contains("utf16.md") && err.contains("is not UTF-8") && err.contains("UTF-16"),
        "{err}"
    );
}

/// AC-3 at turn time: a Shift-JIS `AGENTS.md` is a machine stop (`context_unreadable`) before
/// the turn — no tokens, no fix cycle; saved as UTF-8, a plain `resume` goes on.
#[test]
fn m12_10_a_context_file_that_is_not_utf8_stops_the_turn_by_name() {
    let env = setup("codex-openai");
    let agents = env.repo.join("AGENTS.md");
    let good = std::fs::read(&agents).ok();
    std::fs::write(&agents, [0x93, 0xFA, 0x96, 0x7B, 0x8C, 0xEA, b'\n']).unwrap();
    create(&env, "E-1");
    let s = script(
        &env,
        "e1.json",
        serde_json::json!({
            "planner": [turn(vec![plan()])],
            "implementer": [turn(vec![write("src/Calculator.psm1", DIVIDE), implementation()])],
            "reviewer": [turn(vec![approved()])],
        }),
    );
    let (_, err) = toryo(&env, &["run", "E-1", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "E-1");
    assert_eq!(t.state, toryo::engine::PipelineState::Escalated, "{err}");
    assert_eq!(
        t.machine.reason_class(),
        Some("context_unreadable"),
        "{err}"
    );
    let why = t.machine.reason().unwrap().to_string();
    assert!(
        why.starts_with("context_unreadable:")
            && why.contains("AGENTS.md")
            && why.contains("is not UTF-8")
            && why.contains("`toryo resume E-1`"),
        "{why}"
    );
    assert_eq!(t.fix_cycle, 0);
    let at = attempts(&env, "E-1");
    assert!(
        at.len() == 1
            && at[0].error_class.as_deref() == Some("context_unreadable")
            && at[0].context_sha256.is_none(),
        "no turn was spent: {at:?}"
    );
    // the way out: the file saved as UTF-8; a plain resume goes on
    match good {
        Some(g) => std::fs::write(&agents, g).unwrap(),
        None => std::fs::remove_file(&agents).unwrap(),
    }
    let (_, err) = toryo(&env, &["resume", "E-1", "--yes", "--fake-agents", &s], &[]);
    let t = task(&env, "E-1");
    assert_ne!(
        t.machine.reason_class(),
        Some("context_unreadable"),
        "{err}"
    );
    assert_eq!(t.fix_cycle, 0, "a machine stop costs no fix cycle");
}

// ── M12-11: acceptance criteria are never dropped silently ───────────────────────────────

/// AC-1: a TASK.md with the six lines of the review — the first four are accepted (the dash
/// forms and the `・` bullet), the last two (a full-width digit, a table row) make
/// `task create` refuse with their line numbers and the form to use. (Was: `created …: 1
/// acceptance criteria ["AC-1"]` and five criteria gone.)
#[test]
fn m12_11_an_acceptance_line_that_would_be_dropped_is_refused() {
    let env = setup("codex-openai");
    let write_task = |name: &str, tail: &str| {
        let p = env.root.join(name);
        std::fs::write(
            &p,
            format!(
                "# AC-7 six lines\n\n## Acceptance criteria\n- AC-1: one\n- AC-2 — two\n- AC-3 - three\n・AC-4：four\n{tail}"
            ),
        )
        .unwrap();
        p.display().to_string()
    };
    let ok = write_task("four.md", "");
    let (code, err) = toryo(&env, &["task", "create", "--file", &ok, "--id", "A-4"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("4 acceptance criteria"), "{err}");
    let bad = write_task("six.md", "- AC-５: five\n| AC-6 | six |\n");
    let (code, err) = toryo(
        &env,
        &["task", "create", "--file", &bad, "--id", "A-6"],
        &[],
    );
    assert_ne!(code, 0);
    assert!(
        err.contains("line 8: `- AC-５: five`")
            && err.contains("line 9: `| AC-6 | six |`")
            && err.contains("- AC-1: text"),
        "{err}"
    );
}

// ── M12-17: `doctor` checks what a first run needs ───────────────────────────────────────

fn doctor_lines(env: &Env, cwd: Option<&std::path::Path>) -> Vec<String> {
    let mut c = command(env, &["doctor"], &[]);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let o = c.output().unwrap();
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn line_with<'a>(lines: &'a [String], needle: &str) -> &'a str {
    lines
        .iter()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line with `{needle}` in:\n{}", lines.join("\n")))
}

/// The base branch line (M12-08): the key set and the branch there — OK; the key set and no
/// such branch — FAIL with the way out; no key — INFO naming the checked-out branch.
#[test]
fn m12_17_doctor_checks_the_base_branch() {
    let env = setup("codex-openai");
    let lines = doctor_lines(&env, None);
    let l = line_with(&lines, "no project.base_branch");
    assert!(l.contains("checked-out branch (`main`)"), "{l}");
    set_project_key(&env, "base_branch: main");
    let lines = doctor_lines(&env, None);
    let l = line_with(&lines, "project.base_branch `main` exists");
    assert!(l.contains("OK"), "{l}");
    let y = std::fs::read_to_string(&env.config).unwrap();
    std::fs::write(
        &env.config,
        y.replace("base_branch: main\n", "base_branch: nope\n"),
    )
    .unwrap();
    let lines = doctor_lines(&env, None);
    let l = line_with(&lines, "the branch `nope` does not exist");
    assert!(l.contains("FAIL") && l.contains("task create"), "{l}");
}

/// The encoding line: every text file under `.toryo/` and the machine config are UTF-8 (a
/// byte-order mark passes); a Shift-JIS file is a FAIL naming it.
#[test]
fn m12_17_doctor_checks_the_encoding_of_the_toryo_folder() {
    let env = setup("codex-openai");
    let ctx = env.repo.join(".toryo").join("context");
    std::fs::create_dir_all(&ctx).unwrap();
    std::fs::write(ctx.join("10-ok.md"), b"\xEF\xBB\xBFplain text\n").unwrap();
    let lines = doctor_lines(&env, None);
    let l = line_with(&lines, "text file(s) under .toryo/ are UTF-8");
    assert!(l.contains("OK") && l.contains("1 text file(s)"), "{l}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("machine config") && l.contains("is UTF-8")),
        "{lines:?}"
    );
    std::fs::write(ctx.join("20-bad.md"), [0x93, 0xFA, 0x96, 0x7B]).unwrap();
    let lines = doctor_lines(&env, None);
    let l = line_with(&lines, "20-bad.md is not UTF-8");
    assert!(
        l.contains("FAIL") && l.contains("context_unreadable"),
        "{l}"
    );
}

/// The config-file line (M12-09): which file and why; and a WARN when `$TORYO_CONFIG` names
/// another file that is ignored in this folder.
#[test]
fn m12_17_doctor_says_which_config_is_used_and_why() {
    let env = setup("codex-openai");
    let here = env.root.join("here");
    std::fs::create_dir_all(&here).unwrap();
    std::fs::copy(&env.config, here.join("toryo.yaml")).unwrap();
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_toryo"));
    c.arg("--machine-config")
        .arg(&env.machine)
        .arg("doctor")
        .env("TORYO_DATA_DIR", &env.data)
        .env("TORYO_CONFIG", &env.config)
        .current_dir(&here)
        .stdin(std::process::Stdio::null());
    let o = c.output().unwrap();
    let lines: Vec<String> = String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    let l = line_with(&lines, "is in use — chosen by");
    assert!(l.contains("the toryo.yaml of the folder you are in"), "{l}");
    let l = line_with(&lines, "$TORYO_CONFIG names");
    assert!(l.contains("WARN") && l.contains("is ignored here"), "{l}");
    // `--config` is named as the reason when it is given
    let lines = doctor_lines(&env, None);
    let l = line_with(&lines, "is in use — chosen by");
    assert!(l.contains("`--config`"), "{l}");
    assert!(!lines.iter().any(|l| l.contains("$TORYO_CONFIG names")));
}

/// A profile `env:` value with `%NAME%` is a WARN: it is passed as written.
#[test]
fn m12_17_doctor_warns_about_a_percent_variable_in_a_profile_env() {
    let env = setup("codex-openai");
    let m = std::fs::read_to_string(&env.machine).unwrap();
    assert!(m.contains("exe: codex-not-installed }"), "{m}");
    std::fs::write(
        &env.machine,
        m.replace(
            "exe: codex-not-installed }",
            "exe: codex-not-installed, env: { CODEX_HOME: '%USERPROFILE%/codex', PLAIN: 'a%b' } }",
        ),
    )
    .unwrap();
    let lines = doctor_lines(&env, None);
    let l = line_with(&lines, "env CODEX_HOME contains %USERPROFILE%");
    assert!(
        l.contains("WARN") && l.contains("profile codex-openai") && l.contains("not expanded here"),
        "{l}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("env PLAIN")),
        "a lone % is no variable"
    );
}

/// The elevation line: WARN when elevated (the way out in it), OK when not — and on Windows
/// `doctor` prints one of the two.
#[test]
fn m12_17_doctor_says_whether_toryo_runs_elevated() {
    use toryo::doctor::{elevation_check, Level};
    let w = elevation_check(true);
    assert!(matches!(w.level, Level::Warn));
    assert!(
        w.message.contains("administrator token")
            && w.message
                .contains("elevate inside the environment gate script"),
        "{}",
        w.message
    );
    assert!(matches!(elevation_check(false).level, Level::Ok));
    if cfg!(windows) {
        let env = setup("codex-openai");
        let lines = doctor_lines(&env, None);
        let l = line_with(&lines, "Toryo runs");
        assert_eq!(
            l.contains("WARN"),
            toryo::doctor::is_elevated(),
            "doctor and the token agree: {l}"
        );
    }
}
