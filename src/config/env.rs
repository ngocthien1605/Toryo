//! Paths and variables (R23-33, moved as they were): the data folder, the machine config's default
//! path, `%VAR%` expansion.

use super::*;

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

/// How many times [`VAR_PATTERN`] was compiled in this process (tests only).
#[cfg(test)]
pub(crate) static VAR_REGEX_COMPILES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// [`VAR_PATTERN`] compiled once for the process (review 2026-10-23: it was compiled on every
/// call of [`expand_vars`] and [`residual_var`]).
fn var_regex() -> &'static regex::Regex {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        #[cfg(test)]
        VAR_REGEX_COMPILES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        regex::Regex::new(VAR_PATTERN).expect("regex")
    });
    &RE
}

/// M11-R12: the first `%NAME%` token in `s`, if any — used to catch a `%VAR%` that survived
/// [`expand_vars`] because a variable's own value contained it (it is never expanded again).
pub fn residual_var(s: &str) -> Option<String> {
    var_regex().captures(s).map(|c| c[1].to_string())
}

/// Expand `%VAR%` (Windows style). Unknown variables are an error. Names may contain `(` `)`,
/// as in `%ProgramFiles(x86)%` (EC-28).
pub fn expand_vars(s: &str) -> Result<String> {
    let re = var_regex();
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
