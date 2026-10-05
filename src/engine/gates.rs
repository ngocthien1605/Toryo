//! M12-05: can a gate start on this machine? One resolver for `doctor`, admission and the start
//! of the Verify stage — so that a program that is not there (PowerShell 7 on a machine
//! without it) or a wrong `-File` path in the machine config is found before an implementer
//! turn is spent on it, and is a **machine** stop, not a failing gate.
//!
//! Only what the machine provides is asked: a bare program name on the PATH the gate gets
//! (`PATHEXT` on Windows), an absolute path that exists; for an environment gate also the
//! script after `-File` / `-f` and every absolute argument with a script extension. A
//! repository path (`scripts/build.ps1`, `./gradlew`) is never asked here: it is the business
//! of the M18 rules (`checks.rs`, `task.rs`).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::config::{CommandKind, CommandSpec, EnvGate};

/// The escalation class of a check gate that cannot be started (machine-caused).
pub const GATE_UNAVAILABLE: &str = "gate_unavailable";

/// Script extensions of an absolute argument that must exist (environment gates).
const SCRIPT_EXTENSIONS: [&str; 8] = [".ps1", ".psm1", ".cmd", ".bat", ".sh", ".py", ".js", ".exe"];

/// A file the system would let a process start — or, on Windows, anything that is not a folder
/// where `metadata` cannot see through it (an App Execution Alias of the Store `pwsh.exe` is a
/// reparse point `is_file()` calls false). Lenient on purpose: if the start then fails, the
/// typed spawn error stops the gate the same way.
fn startable(p: &Path) -> bool {
    p.is_file()
        || (cfg!(windows) && std::fs::symlink_metadata(p).is_ok_and(|m| !m.file_type().is_dir()))
}

/// `name` the way the engine starts a bare program: a folder of `path` with nothing, `.exe` or
/// `.cmd` appended (`checks::which_in`) — not `PATHEXT`: a `.bat` or `.com` alone does not start
/// (M12-05 review) — then, as `CreateProcess` would, the folder of this program and the system
/// folder on Windows.
fn on_path(name: &str, path: &OsStr) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::split_paths(path).collect();
    if cfg!(windows) {
        dirs.extend(
            std::env::current_exe()
                .ok()
                .and_then(|e| e.parent().map(Path::to_path_buf)),
        );
        dirs.extend(std::env::var_os("SystemRoot").map(|r| PathBuf::from(r).join("System32")));
    }
    for dir in dirs {
        for ext in ["", ".exe", ".cmd"] {
            let p = dir.join(format!("{name}{ext}"));
            if startable(&p) {
                return Some(p);
            }
        }
    }
    None
}

/// Why the program `program` (as it will be started: variables already expanded) cannot be
/// started here, or `None`. A repository-relative path (a separator, not absolute) is not asked.
pub fn program_problem(program: &str, path: Option<&OsStr>) -> Option<String> {
    let has_sep = program.contains('/') || program.contains('\\');
    let p = Path::new(program);
    if p.is_absolute() || p.has_root() {
        // an absolute path without an extension starts `<path>.exe` on Windows
        let found = startable(p)
            || (cfg!(windows)
                && p.extension().is_none()
                && startable(&PathBuf::from(format!("{program}.exe"))));
        return (!found).then(|| format!("program `{program}` does not exist"));
    }
    if has_sep {
        return None;
    }
    let found = path.and_then(|path| on_path(program, path));
    found.is_none().then(|| {
        format!("program `{program}` is not on the PATH the gate gets (PATH of this process)")
    })
}

/// Why the script an environment gate hands to its program cannot be there: the argument after
/// `-File` / `-f` (case-insensitive), and every absolute argument with a script extension. One
/// line per problem.
pub fn script_problems(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let is_file_flag = a.eq_ignore_ascii_case("-file") || a.eq_ignore_ascii_case("-f");
        let candidate = if is_file_flag { it.next() } else { Some(a) };
        let Some(c) = candidate else { continue };
        let lower = c.to_ascii_lowercase();
        let absolute = Path::new(c).is_absolute() || Path::new(c).has_root();
        let script = SCRIPT_EXTENSIONS.iter().any(|x| lower.ends_with(x));
        // an argument after `-File` is the script whatever it is called; any other argument
        // only when it is an absolute path to a script
        if (is_file_flag || (absolute && script)) && !Path::new(c).is_file() {
            // a relative `-File` is the repository's (M18) — a path that is not asked here
            if is_file_flag && !absolute {
                continue;
            }
            out.push(format!("script `{c}` does not exist"));
        }
    }
    out
}

/// Why the check gate `c` cannot start here (its program), or `None`. A `tool: xpp` command is
/// the toolchain preflight's business.
pub fn check_gate_problem(c: &CommandSpec, path: Option<&OsStr>) -> Option<String> {
    let CommandKind::Program { program, .. } = c.kind() else {
        return None;
    };
    let program = crate::config::expand_vars(program).unwrap_or_else(|_| program.to_string());
    program_problem(&program, path)
}

/// Why the environment gate `m` cannot start here — program and script — one line per problem.
pub fn env_gate_problems(m: &EnvGate, path: Option<&OsStr>) -> Vec<String> {
    let mut out: Vec<String> = program_problem(&m.program, path).into_iter().collect();
    out.extend(script_problems(&m.args));
    out
}

/// M12-05: the refusal of `task create` and `task config --reload` — the first environment gate
/// of the implementer's `before_handoff` that this machine defines but whose **script** is not
/// there (`-File`, an absolute script path), or `None`. Deliberately not the program: a task
/// whose gate program is missing has always been admitted and stopped at Verify
/// (`environment_gate_unavailable`; existing tests rely on it — recorded in the notes). A check
/// gate is never refused here: its program may legitimately be missing on a machine that only
/// creates tasks.
pub fn env_gates_refusal(cfg: &crate::config::Effective) -> Option<String> {
    let team = cfg.team.as_ref()?;
    team.handoff_gates().into_iter().find_map(|(name, g)| {
        if g.decl.kind != crate::team::GateType::Environment {
            return None;
        }
        let m = g.machine.as_ref()?;
        let problems = script_problems(&m.args);
        (!problems.is_empty()).then(|| {
            format!(
                "{}: environment gate `{name}` cannot start on this machine: {} — fix `environment_gates.{name}` in the machine config",
                super::runner::ENV_UNAVAILABLE,
                problems.join("; ")
            )
        })
    })
}

/// The escalation class of an environment gate that waits for the owner's "yes" (M12-07, X17).
pub const ENV_CONFIRM: &str = "environment_gate_confirm";

/// What the owner has confirmed so far, in the task's run store (`confirmations.json`): one
/// entry per gate and checkpoint commit. A small file, not a database column: no migration.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Confirmations {
    #[serde(default)]
    pub confirmed: Vec<Asked>,
    /// What the last stop asked and a plain `resume` will confirm.
    #[serde(default)]
    pub pending: Vec<Asked>,
}

/// A deploy the owner is asked about: the gate and the checkpoint commit.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Asked {
    pub gate: String,
    pub commit: String,
}

fn confirmations_path(run_store: &Path) -> PathBuf {
    run_store.join("confirmations.json")
}

/// The confirmations of a task (none when the file is not there or unreadable).
pub fn confirmations(run_store: &Path) -> Confirmations {
    std::fs::read(confirmations_path(run_store))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save(run_store: &Path, c: &Confirmations) -> std::io::Result<()> {
    std::fs::write(
        confirmations_path(run_store),
        serde_json::to_vec_pretty(c).unwrap_or_default(),
    )
}

/// Has the owner confirmed the deploy of `commit` by `gate`? `first`: any confirmation of the
/// gate in this task; `always`: this commit. `never` needs none.
pub fn is_confirmed(
    c: &Confirmations,
    mode: crate::config::Confirm,
    gate: &str,
    commit: &str,
) -> bool {
    use crate::config::Confirm;
    match mode {
        Confirm::Never => true,
        Confirm::First => c.confirmed.iter().any(|a| a.gate == gate),
        Confirm::Always => c
            .confirmed
            .iter()
            .any(|a| a.gate == gate && a.commit == commit),
    }
}

/// The stop asks about these deploys: a plain `resume` confirms them ([`record_confirmation`]).
pub fn set_pending(run_store: &Path, asked: Vec<Asked>) -> std::io::Result<()> {
    let mut c = confirmations(run_store);
    c.pending = asked;
    save(run_store, &c)
}

/// A plain `toryo resume` after the stop: what it asked about is now confirmed. Returns the
/// confirmed deploys.
pub fn record_confirmation(run_store: &Path) -> std::io::Result<Vec<Asked>> {
    let mut c = confirmations(run_store);
    let asked = std::mem::take(&mut c.pending);
    for a in &asked {
        if !c.confirmed.contains(a) {
            c.confirmed.push(a.clone());
        }
    }
    save(run_store, &c)?;
    Ok(asked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn programs_are_asked_only_where_the_machine_decides() {
        let d = tempfile::tempdir().unwrap();
        let tool = d
            .path()
            .join(if cfg!(windows) { "tool.exe" } else { "tool" });
        std::fs::write(&tool, "x").unwrap();
        let path = std::env::join_paths([d.path()]).unwrap();
        // a bare name on the PATH is found (`.exe` through PATHEXT on Windows)
        assert_eq!(program_problem("tool", Some(&path)), None);
        let why = program_problem("no-such-program", Some(&path)).unwrap();
        assert!(
            why.contains("no-such-program") && why.contains("PATH"),
            "{why}"
        );
        assert!(program_problem("tool", None).is_some());
        // an absolute path must exist
        assert_eq!(program_problem(&tool.display().to_string(), None), None);
        let gone = d.path().join("gone.exe").display().to_string();
        assert!(program_problem(&gone, None)
            .unwrap()
            .contains("does not exist"));
        // (review) the engine starts `name`, `name.exe` or `name.cmd` — not PATHEXT: a `.bat`
        // alone does not start, so it is a stop at the preflight and not after it
        std::fs::write(d.path().join("only-a-batch.bat"), "x").unwrap();
        assert!(program_problem("only-a-batch", Some(&path)).is_some());
        // an absolute path without an extension starts `<path>.exe` on Windows
        if cfg!(windows) {
            let no_ext = d.path().join("tool").display().to_string();
            assert_eq!(program_problem(&no_ext, None), None);
        }
        // a repository path is not asked here (M18)
        assert_eq!(program_problem("scripts/build.ps1", None), None);
        assert_eq!(program_problem("./gradlew", None), None);
        assert_eq!(program_problem(r"scripts\build.cmd", None), None);
    }

    #[test]
    fn the_script_of_an_environment_gate_must_exist() {
        let d = tempfile::tempdir().unwrap();
        let ok = d.path().join("dbsync.ps1");
        std::fs::write(&ok, "x").unwrap();
        let ok = ok.display().to_string();
        let gone = d.path().join("typo.ps1").display().to_string();
        assert!(script_problems(&s(&["-NoProfile", "-File", &ok])).is_empty());
        let p = script_problems(&s(&["-NoProfile", "-File", &gone]));
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("typo.ps1"), "{p:?}");
        // `-f` too, and an absolute argument with a script extension without a flag
        assert_eq!(script_problems(&s(&["-f", &gone])).len(), 1);
        assert_eq!(script_problems(&s(&["run", &gone])).len(), 1);
        // an absolute argument that is no script, and a relative `-File`, are not asked
        assert!(script_problems(&s(&[
            "--out",
            &d.path().join("x.log").display().to_string()
        ]))
        .is_empty());
        assert!(script_problems(&s(&["-File", "scripts/x.ps1"])).is_empty());
    }
}
