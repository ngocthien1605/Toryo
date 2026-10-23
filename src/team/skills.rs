//! Skills (M11-D1; R23-38, moved out of `team.rs` as they were): `<name>/SKILL.md` in the
//! repository, then the machine's `skills_dir`; front matter, names, resolution.

use super::*;

/// Where a skill was found (owner decision D3: the repository first, then the machine).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillOrigin {
    Repo,
    Machine,
}

impl SkillOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Repo => "repo",
            Self::Machine => "machine",
        }
    }
}

/// A skill as resolved for one attempt (M11-D1): `{ name, origin, path, sha256, bytes }`, the
/// `description` of its front matter and its text. Only `SKILL.md` itself is delivered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skill {
    pub name: String,
    pub origin: SkillOrigin,
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: usize,
    pub description: Option<String>,
    #[serde(skip)]
    pub text: String,
}

/// The front matter of a SKILL.md: the YAML mapping between the first two `---` lines (a UTF-8
/// BOM and CRLF line ends allowed). `None` without front matter, or when it is not a mapping.
pub fn front_matter(text: &str) -> Option<serde_yaml::Mapping> {
    let t = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n");
    let mut lines = t.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let mut yaml = String::new();
    for l in lines {
        if l.trim_end() == "---" {
            let v: serde_yaml::Value = serde_yaml::from_str(&yaml).ok()?;
            return v.as_mapping().cloned();
        }
        yaml.push_str(l);
        yaml.push('\n');
    }
    None
}

/// The `description` of a SKILL.md's front matter, trimmed; `None` when missing or empty.
pub fn skill_description(text: &str) -> Option<String> {
    front_matter(text)?
        .get("description")?
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// A skill name is a folder name: not empty, no separator, no `..`, no drive.
pub fn valid_skill_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && name.trim() == name
}

/// The machine's skills folder of the frozen roots, expanded (`%VAR%`).
pub fn machine_skills_dir(roots: &SkillRoots) -> Option<PathBuf> {
    roots
        .machine
        .as_deref()
        .map(|m| PathBuf::from(crate::config::expand_vars(m).unwrap_or_else(|_| m.to_string())))
}

/// Where `name` is looked up, in order (D3): `<repo>/.toryo/skills/<name>/SKILL.md`, then
/// `<machine>/<name>/SKILL.md`.
pub fn skill_candidates(
    name: &str,
    repo: &Path,
    machine: Option<&Path>,
) -> Vec<(SkillOrigin, PathBuf)> {
    let mut out = vec![(
        SkillOrigin::Repo,
        repo.join(".toryo")
            .join("skills")
            .join(name)
            .join("SKILL.md"),
    )];
    if let Some(m) = machine {
        out.push((SkillOrigin::Machine, m.join(name).join("SKILL.md")));
    }
    out
}

/// Resolve `name`: the first place that has its SKILL.md wins. `Err`: why not, naming both
/// places.
pub fn resolve_skill(
    name: &str,
    repo: &Path,
    machine: Option<&Path>,
) -> std::result::Result<Skill, String> {
    if !valid_skill_name(name) {
        return Err(format!(
            "skill '{name}' is not a folder name — a skill is `<name>/SKILL.md`; use a name without `/`, `\\`, `:` or `..`"
        ));
    }
    let places = skill_candidates(name, repo, machine);
    for (origin, path) in &places {
        if let Ok(bytes) = std::fs::read(path) {
            // M12-10: a SKILL.md that is not UTF-8 is refused by name, not sent as `�`
            let text = crate::textfile::decode(path, &bytes)
                .map_err(|e| format!("{e:#}"))?
                .replace("\r\n", "\n");
            return Ok(Skill {
                name: name.to_string(),
                origin: *origin,
                path: path.clone(),
                sha256: crate::engine::task::sha256_hex(text.as_bytes()),
                bytes: text.len(),
                description: skill_description(&text),
                text,
            });
        }
    }
    let shown: Vec<String> = places
        .iter()
        .map(|(_, p)| p.display().to_string())
        .collect();
    let machine_hint = if machine.is_none() {
        " (no `skills_dir` in the machine config)"
    } else {
        ""
    };
    Err(format!(
        "skill '{name}' not found: neither {}{machine_hint}",
        shown.join(" nor ")
    ))
}

/// The skill names that exist under the roots (folders with a SKILL.md), for "did you mean".
pub fn skill_names(repo: &Path, machine: Option<&Path>) -> Vec<String> {
    let mut out = Vec::new();
    let dirs =
        std::iter::once(repo.join(".toryo").join("skills")).chain(machine.map(Path::to_path_buf));
    for d in dirs {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            if e.path().join("SKILL.md").is_file() {
                let n = e.file_name().to_string_lossy().to_string();
                if !out.contains(&n) {
                    out.push(n);
                }
            }
        }
    }
    out.sort();
    out
}
