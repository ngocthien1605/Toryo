//! The environment gates of an org chart, checked against the machine (M11-E3, X3, X12; R23-38,
//! moved out of `team.rs` as they were): `%VAR%` expanded once, what a gate's program and arguments
//! reach, whether it stays out of the roots agent code can write.

use super::*;

/// An argument that names a file: not a flag, and with a path separator or a script or
/// program extension.
pub(super) fn named_path(a: &str) -> Option<&str> {
    // the value of a `--name=value` / `-Name:value` flag; a flag alone names no file
    let v = if a.starts_with('-') {
        a.split_once(['=', ':']).map(|(_, v)| v)?
    } else {
        a
    };
    let lower = v.to_ascii_lowercase();
    // a Windows switch (`/c`, `/q`) is not a path
    let switch = v.starts_with('/') && v.len() <= 3 && !v[1..].contains(['/', '\\']);
    let file = v.contains('/')
        || v.contains('\\')
        || [".ps1", ".psm1", ".cmd", ".bat", ".sh", ".exe", ".py", ".js"]
            .iter()
            .any(|x| lower.ends_with(x));
    (!v.is_empty() && !switch && file).then_some(v)
}

/// The lexical form the reach check compares (group B review): `/` separators, no `\\?\`
/// prefix, `.` dropped, `..` resolved, trailing dots and spaces of a segment dropped (Windows
/// ignores them), lower case. The longest existing ancestor is canonicalised first (junctions,
/// 8.3 names) and the rest appended: a path that does not exist yet is compared in the same
/// spelling as an existing root (M11-ci: `C:\Users\RUNNER~1\…` against `C:\Users\runneradmin\…`).
pub(super) fn lexical(p: &str) -> String {
    let path = Path::new(p);
    let canon = path
        .ancestors()
        .find_map(|a| {
            let c = std::fs::canonicalize(a).ok()?;
            let rest = path.strip_prefix(a).ok()?;
            Some(if rest.as_os_str().is_empty() {
                c
            } else {
                c.join(rest)
            })
        })
        .map(|c| c.display().to_string())
        .unwrap_or_else(|| p.to_string());
    let s = canon.replace('\\', "/");
    let s = s
        .strip_prefix("//?/")
        .or_else(|| s.strip_prefix("//./"))
        .unwrap_or(&s);
    let mut parts: Vec<&str> = Vec::new();
    for (i, c) in s.split('/').enumerate() {
        match c {
            "" if i == 0 => parts.push(""),
            "" | "." => {}
            ".." => {
                if parts.len() > 1 {
                    parts.pop();
                }
            }
            c => parts.push(c.trim_end_matches(['.', ' '])),
        }
    }
    parts.join("/").to_lowercase()
}

/// `p` is `root` or inside it, compared in [`lexical`] form — by where it **leads** (its
/// canonical form), or by where it is **written** (M11-R17): a folder on the way to it is the
/// root or inside it. `<repo>\link\new.ps1` is inside the repository although `link` is a
/// junction pointing out of it: agent code can write the repository, and so replace the link.
pub(super) fn inside(p: &str, root: &Path) -> bool {
    let r = lexical(&root.display().to_string());
    if r.is_empty() {
        return false;
    }
    let under = |s: &str| s == r || s.starts_with(&format!("{r}/"));
    Path::new(p)
        .ancestors()
        .filter(|a| !a.as_os_str().is_empty())
        .any(|a| under(&lexical(&a.display().to_string())))
}

/// Which agent-reachable folder `p` is inside, if any: the repository (`project.root`), the
/// task worktrees or the verify root — `(what, root)`. Used at load (B3) and, for a bare
/// program resolved through PATH, before an environment gate runs (M11-R3).
pub fn reach(cfg: &Effective, p: &str) -> Option<(&'static str, PathBuf)> {
    let expand = |s: &str| crate::config::expand_vars(s).map(PathBuf::from).ok();
    [
        ("the repository", expand(&cfg.repo.project.root)),
        (
            "the task worktrees",
            expand(&cfg.repo.project.worktrees_root),
        ),
        (
            "the verify root",
            Some(crate::engine::verify::verify_root(cfg)),
        ),
    ]
    .into_iter()
    .filter_map(|(w, r)| r.map(|r| (w, r)))
    .find(|(_, r)| inside(p, r))
}

/// M11-R3 (X3): an environment gate's `program` and `args` with `%VAR%` expanded — once, at
/// load; the expanded text is what the reach check sees, what is frozen and hashed, and what
/// runs. An unset variable is a problem naming the key (the text is then kept as written).
pub(super) fn expand_env_gate(
    name: &str,
    g: &EnvGate,
    machine: &str,
    problems: &mut Vec<String>,
) -> EnvGate {
    // M11-R12: expand once; a `%NAME%` that survives (because a variable's value contained it)
    // is a load problem — it is never expanded again
    let mut one = |key: String, s: &str| match crate::config::expand_vars(s) {
        Ok(v) => match crate::config::residual_var(&v) {
            Some(var) => {
                problems.push(format!(
                    "{machine}: environment_gates.{name}.{key}: a variable's value contains `%{var}%`, which is left as is — expand it yourself or remove it"
                ));
                v
            }
            None => v,
        },
        Err(e) => {
            problems.push(format!(
                "{machine}: environment_gates.{name}.{key}: {e:#} — set it, or write the path out"
            ));
            s.to_string()
        }
    };
    let program = one("program".into(), &g.program);
    let args = g
        .args
        .iter()
        .enumerate()
        .map(|(i, a)| one(format!("args[{i}]"), a))
        .collect();
    EnvGate {
        program,
        args,
        ..g.clone()
    }
}

/// The problems of a machine `EnvGate` (B3): a lock; a program that is absolute or a PATH
/// name; every path-like argument absolute and out of the agents' reach — not inside the
/// repository, the task worktrees or the verify root.
pub(super) fn env_gate_problems(
    out: &mut Vec<String>,
    name: &str,
    g: &EnvGate,
    cfg: &Effective,
    machine: &str,
) {
    let key = format!("{machine}: environment_gates.{name}");
    if g.lock.trim().is_empty() {
        out.push(format!(
            "{key}.lock: required — the machine-wide lock name (e.g. d365-env): one environment gate per lock runs at a time"
        ));
    } else if let Some(why) = crate::engine::lock::lock_name_problem(&g.lock) {
        // M11-E3: the name is part of the lock's file name
        out.push(format!("{key}.lock: `{}` — {why}", g.lock));
    }
    // M11-R3 (X3): `program` and `args` were expanded at load — this is the text that runs.
    // M11-R12: a value that still holds `%...%` (an unset variable kept raw, or a variable's
    // value that contained it) was reported once at load; skip it here without re-expanding.
    let unset = |s: &str| crate::config::residual_var(s).is_some();
    let reach = |p: &str| reach(cfg, p);
    let program = g.program.clone();
    if (program.contains('/') || program.contains('\\')) && !unset(&program) {
        let dots = program.replace('\\', "/").split('/').any(|c| c == "..");
        if dots || !Path::new(&program).is_absolute() {
            out.push(format!(
                "{key}.program: `{}` must be an absolute path or a bare name found on PATH",
                g.program
            ));
        } else if let Some((what, r)) = reach(&program) {
            out.push(format!(
                "{key}.program: `{}` is inside {what} ({}) — an agent could change it; keep it outside the repository, the task worktrees and the verify root",
                g.program,
                r.display()
            ));
        }
    }
    for a in g.args.iter().filter(|a| !unset(a)) {
        let Some(named) = named_path(a) else {
            continue;
        };
        let x = named.to_string();
        if x.replace('\\', "/").split('/').any(|c| c == "..") {
            out.push(format!(
                "{key}.args: `{a}` may not contain `..` — write the absolute path of the script, outside the repository, the task worktrees and the verify root"
            ));
        } else if !Path::new(&x).is_absolute() {
            out.push(format!(
                "{key}.args: `{a}` must be an absolute path — the script must be out of the agents' reach (not in the repository, a task worktree or the verify root)"
            ));
        } else if let Some((what, r)) = reach(&x) {
            out.push(format!(
                "{key}.args: `{a}` is inside {what} ({}) — an agent could change it; keep the script outside the repository, the task worktrees and the verify root",
                r.display()
            ));
        }
    }
}
