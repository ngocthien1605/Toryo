//! The machine/repository boundary of the files (R60, review C1, X45; R23-33, moved as it was): the
//! machine-only keys and the YAML tags a repository file may not carry.

use super::*;

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

/// C1: every YAML tag of a repository file (`!x profiles:`, `roles: !x {…}`; R23-19: at every
/// depth, `profile: !x evil` inside a role too), refused in one error that names the file and
/// each key. A tag makes a key invisible to a lookup by name while a typed parse still reads it,
/// so repository files —
/// `toryo.yaml`, the org chart, the role files — carry none.
pub fn refuse_tags(doc: &serde_yaml::Value, file: &Path) -> Result<()> {
    let out = tag_problems(doc);
    if out.is_empty() {
        return Ok(());
    }
    bail!(
        "{}: YAML tags are not allowed in a repository file (review C1: a tag hides a key from Toryo's checks) — remove them:\n  - {}",
        file.display(),
        out.join("\n  - ")
    )
}

/// Every YAML tag in `doc`, as a line naming where it is. R23-19: at every depth — the typed
/// parse drops a nested tag silently (`roles: { planner: { profile: !x evil } }` is `evil`),
/// and a later lookup by name on the untyped document would miss the tagged key (review C1).
pub fn tag_problems(doc: &serde_yaml::Value) -> Vec<String> {
    let mut out = Vec::new();
    if let serde_yaml::Value::Tagged(t) = doc {
        out.push(format!("the document has the tag `{}`", t.tag));
    }
    tags_in(untagged(doc), "", &mut out);
    out
}

pub(super) fn tags_in(v: &serde_yaml::Value, at: &str, out: &mut Vec<String>) {
    let join = |name: &str| {
        if at.is_empty() {
            name.to_string()
        } else {
            format!("{at}.{name}")
        }
    };
    match v {
        serde_yaml::Value::Mapping(map) => {
            for (k, v) in map {
                let name = join(&match untagged(k) {
                    serde_yaml::Value::String(s) => s.clone(),
                    other => serde_yaml::to_string(other)
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                });
                if let serde_yaml::Value::Tagged(t) = k {
                    out.push(format!("key `{name}` has the tag `{}`", t.tag));
                }
                if let serde_yaml::Value::Tagged(t) = v {
                    out.push(format!("the value of `{name}` has the tag `{}`", t.tag));
                }
                tags_in(untagged(v), &name, out);
            }
        }
        serde_yaml::Value::Sequence(items) => {
            for (i, item) in items.iter().enumerate() {
                let name = join(&format!("[{i}]"));
                if let serde_yaml::Value::Tagged(t) = item {
                    out.push(format!("the value of `{name}` has the tag `{}`", t.tag));
                }
                tags_in(untagged(item), &name, out);
            }
        }
        _ => {}
    }
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
