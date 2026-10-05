//! (R59) The built-in verify kind `tool: xpp`: the headless X++ build of Microsoft's hosted
//! pipeline, on the owner's UDE machine.
//!
//! - **Toolchain** — `toolchains.xpp` of the machine config: MSBuild and a folder holding the
//!   five CI/CD NuGet packages, one folder each (`<id>` or `<id>.<version>`), found by
//!   [`resolve`].
//! - **Command line** — [`argv`]: one MSBuild run per project, with Microsoft's documented
//!   properties, `-noAutoResponse` (no `Directory.Build.rsp` from the checkout) and
//!   `-nodeReuse:false` (no MSBuild node outlives the verify Job). The compiler output goes to
//!   `<verify_root>\<task>\build\xpp`, outside the checkout.
//! - **Result** — [`entry`]: one `VERIFY_RESULT.json` entry per project, with the argv, the
//!   resolved toolchain and `scope: compile` — local Verify proves compile level only (no DB
//!   sync, no unit tests).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};

use crate::config::{expand_vars, CommandKind, CommandSpec, Effective, XppToolchain};
use crate::runner::CommandResult;

/// The package with `xppc.exe` and the X++ build tasks.
pub const COMPILER: &str = "Microsoft.Dynamics.AX.Platform.CompilerPackage";

/// The five CI/CD packages, in the order their folders appear in `ReferenceFolder` (the
/// compiler package first: it is not a reference folder but the build's framework).
pub const PACKAGES: [&str; 5] = [
    COMPILER,
    "Microsoft.Dynamics.AX.Platform.DevALM.BuildXpp",
    "Microsoft.Dynamics.AX.Application1.DevALM.BuildXpp",
    "Microsoft.Dynamics.AX.Application2.DevALM.BuildXpp",
    "Microsoft.Dynamics.AX.ApplicationSuite.DevALM.BuildXpp",
];

/// `timeout_min` of a `tool: xpp` command without one: a package build takes long.
pub const DEFAULT_TIMEOUT_MIN: u64 = 90;

/// The error class of a toolchain that cannot be used (a missing MSBuild or package).
pub const UNAVAILABLE: &str = "xpp_toolchain_unavailable";

/// Added to the environment of every check gate and environment gate (M12-15, X23; it was an
/// MSBuild run only): English messages whatever language pack Visual Studio has — the
/// diagnostics parser and the fixer read them. `DOTNET_CLI_UI_LANGUAGE` is the variable
/// msbuild.exe documents (17.7+; it also switches the console output to UTF-8); `VSLANG` covers
/// older builds. A tool that still prints in the OEM code page is decoded as such
/// (`process::decode_output`).
pub const ENV: [(&str, &str); 2] = [("DOTNET_CLI_UI_LANGUAGE", "en-US"), ("VSLANG", "1033")];

/// Set [`ENV`] in `env`, replacing a value the process inherited — except a name the owner set
/// on purpose in `keep` (an environment gate's `env:` in the machine config).
pub fn set_ui_language(
    env: &mut std::collections::BTreeMap<String, String>,
    keep: &std::collections::BTreeMap<String, String>,
) {
    for (k, v) in ENV {
        if keep.keys().any(|e| e.eq_ignore_ascii_case(k)) {
            continue;
        }
        env.retain(|e, _| !e.eq_ignore_ascii_case(k));
        env.insert(k.to_string(), v.to_string());
    }
}

/// What `status`, `task show` and the final gate say about a compile-only verify (R59).
pub const COMPILE_ONLY: &str = "verify: compile only (X++: no DB sync, no unit tests)";

/// One CI/CD package as found under `nugets`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Package {
    pub id: String,
    /// From the package's `.nuspec`, else from the folder name.
    pub version: Option<String>,
    pub dir: PathBuf,
}

/// `toolchains.xpp`, resolved on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Toolchain {
    pub msbuild: PathBuf,
    /// In [`PACKAGES`] order.
    pub packages: Vec<Package>,
    pub extra_references: Vec<PathBuf>,
}

impl Toolchain {
    pub fn compiler(&self) -> &Package {
        &self.packages[0]
    }
}

/// `<id>` → `Some("")`, `<id>.<version>` (the version starting with a digit) →
/// `Some(version)`, anything else → `None`. Case-insensitive, like the Windows file system.
fn package_suffix<'a>(name: &'a str, id: &str) -> Option<&'a str> {
    if name.len() < id.len() || !name.is_char_boundary(id.len()) {
        return None;
    }
    let (head, rest) = name.split_at(id.len());
    if !head.eq_ignore_ascii_case(id) {
        return None;
    }
    if rest.is_empty() {
        return Some("");
    }
    rest.strip_prefix('.')
        .filter(|v| v.starts_with(|c: char| c.is_ascii_digit()))
}

/// The `<version>` of the package's `.nuspec` (`<id>.nuspec`, else any `*.nuspec` in `dir`).
fn nuspec_version(dir: &Path, id: &str) -> Option<String> {
    let named = dir.join(format!("{id}.nuspec"));
    let file = if named.is_file() {
        named
    } else {
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("nuspec"))
            })?
    };
    let text = std::fs::read_to_string(file).ok()?;
    let re = regex::Regex::new(r"<version>\s*([^<\s]+)\s*</version>").ok()?;
    re.captures(&text).map(|c| c[1].to_string())
}

/// What the build reads in a package: `DevAlm` in the compiler package, `ref\net40` in the
/// others ([`argv`]).
pub fn marker(id: &str) -> PathBuf {
    if id == COMPILER {
        PathBuf::from("DevAlm")
    } else {
        Path::new("ref").join("net40")
    }
}

/// The folder of a package whose content is `dir` itself, or — the NuGet cache layout
/// `<id>\<version>\` — its only version subfolder. `None` when neither holds [`marker`].
fn package_root(dir: &Path, id: &str) -> Option<(PathBuf, Option<String>)> {
    let m = marker(id);
    if dir.join(&m).is_dir() {
        return Some((dir.to_path_buf(), None));
    }
    let versions: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(|c: char| c.is_ascii_digit()))
                && p.join(&m).is_dir()
        })
        .collect();
    match versions.as_slice() {
        [one] => {
            let v = one.file_name().map(|n| n.to_string_lossy().to_string());
            Some((one.clone(), v))
        }
        _ => None,
    }
}

/// The five packages under `nugets`: exactly one folder per id, named `<id>` or
/// `<id>.<version>` (or the NuGet cache layout `<id>\<version>\`), holding what the build
/// reads (`DevAlm`, `ref\net40`). Every problem is reported at once: a missing package, one
/// found twice, one that is only a `.nupkg` (with how to extract it), one without its content.
pub fn resolve_packages(nugets: &Path) -> Result<Vec<Package>> {
    let entries: Vec<(String, PathBuf, bool)> = std::fs::read_dir(nugets)
        .with_context(|| {
            format!(
                "{UNAVAILABLE}: toolchains.xpp.nugets {} is not a readable folder",
                nugets.display()
            )
        })?
        .flatten()
        .map(|e| {
            let dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false) || e.path().is_dir();
            (e.file_name().to_string_lossy().to_string(), e.path(), dir)
        })
        .collect();
    let mut found = Vec::new();
    let mut problems = Vec::new();
    for id in PACKAGES {
        let mut dirs: Vec<&(String, PathBuf, bool)> = entries
            .iter()
            .filter(|(n, _, d)| *d && package_suffix(n, id).is_some())
            .collect();
        dirs.sort_by(|a, b| a.0.cmp(&b.0));
        match dirs.as_slice() {
            [(name, dir, _)] => match package_root(dir, id) {
                Some((root, sub)) => {
                    let suffix = package_suffix(name, id).filter(|s| !s.is_empty());
                    found.push(Package {
                        id: id.to_string(),
                        version: nuspec_version(&root, id)
                            .or(suffix.map(str::to_string))
                            .or(sub),
                        dir: root,
                    });
                }
                None => problems.push(format!(
                    "{id}: {name} has no {} folder — a partly extracted package? Extract the whole .nupkg again",
                    marker(id).display()
                )),
            },
            [] => {
                let nupkg = entries.iter().find(|(n, _, d)| {
                    !*d && n.to_ascii_lowercase().ends_with(".nupkg")
                        && package_suffix(&n[..n.len() - ".nupkg".len()], id).is_some()
                });
                problems.push(match nupkg {
                    Some((n, _, _)) => format!(
                        "{id}: only the package file {n} — extract it into a folder: `tar -xf \"{n}\" -C \"{id}\"` after `mkdir {id}` in {} (a .nupkg is a zip file), or `nuget install {id} -Source <download folder> -OutputDirectory \"{}\" -ExcludeVersion`",
                        nugets.display(),
                        nugets.display()
                    ),
                    None => format!(
                        "{id}: not found (a folder `{id}` or `{id}.<version>`; Visual Studio: Tools › Download Dynamics365 FnO NuGets for CI/CD)"
                    ),
                });
            }
            many => problems.push(format!(
                "{id}: found {} times ({}) — keep exactly one",
                many.len(),
                many.iter()
                    .map(|(n, _, _)| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
    if !problems.is_empty() {
        bail!(
            "{UNAVAILABLE}: the CI/CD packages under toolchains.xpp.nugets {} (R59):\n  - {}",
            nugets.display(),
            problems.join("\n  - ")
        );
    }
    Ok(found)
}

/// `toolchains.xpp` on this machine: `%VAR%` expanded, the packages found. MSBuild itself is
/// not required to exist here — [`check_msbuild`] is the admission check.
pub fn resolve(tc: &XppToolchain) -> Result<Toolchain> {
    let expand = |key: &str, v: &str| {
        expand_vars(v)
            .map(PathBuf::from)
            .with_context(|| format!("{UNAVAILABLE}: toolchains.xpp.{key}"))
    };
    Ok(Toolchain {
        msbuild: expand("msbuild", &tc.msbuild)?,
        packages: resolve_packages(&expand("nugets", &tc.nugets)?)?,
        extra_references: tc
            .extra_references
            .iter()
            .map(|r| expand("extra_references", r))
            .collect::<Result<_>>()?,
    })
}

/// MSBuild must be a file.
pub fn check_msbuild(tc: &Toolchain) -> Result<()> {
    if !tc.msbuild.is_file() {
        bail!(
            "{UNAVAILABLE}: toolchains.xpp.msbuild {} not found (`toryo doctor` proposes the path vswhere finds)",
            tc.msbuild.display()
        );
    }
    Ok(())
}

/// Admission (`task create`) and the start of each verify: the toolchain of a configuration
/// with a `tool: xpp` command, resolved and with an existing MSBuild; `None` without one.
pub fn for_config(cfg: &Effective) -> Result<Option<Toolchain>> {
    if !cfg.repo.commands.verify.iter().any(CommandSpec::is_xpp) {
        return Ok(None);
    }
    let Some(x) = &cfg.toolchains.xpp else {
        bail!("{UNAVAILABLE}: a `tool: xpp` verify command but no toolchains.xpp in the configuration (R59)");
    };
    let tc = resolve(x)?;
    check_msbuild(&tc)?;
    Ok(Some(tc))
}

/// The error class of a `tool: xpp` command whose metadata folder or project is missing.
pub const PROJECT_MISSING: &str = "xpp_project_missing";

/// Admission (`task create`): every `tool: xpp` command's metadata folder and projects exist
/// in `place` (the task's base commit). `kind_of(path)` says what a repository path is there:
/// `Some(true)` a folder, `Some(false)` a file, `None` nothing. The projects are protected (the
/// implementer cannot add them later), so a missing one would fail every verify.
pub fn check_repo(
    cfg: &Effective,
    place: &str,
    kind_of: &dyn Fn(&str) -> Option<bool>,
) -> Result<()> {
    let mut missing = Vec::new();
    for c in &cfg.repo.commands.verify {
        let CommandKind::Xpp { metadata, projects } = c.kind() else {
            continue;
        };
        if kind_of(metadata) != Some(true) {
            missing.push(format!("{}: metadata folder {metadata}", c.name));
        }
        for p in projects {
            if kind_of(p) != Some(false) {
                missing.push(format!("{}: project {p}", c.name));
            }
        }
    }
    if !missing.is_empty() {
        bail!(
            "{PROJECT_MISSING}: not in {place} — {} (create and commit the build projects first — if they exist, `git check-ignore -v <path>` shows whether .gitignore hides them; they are protected, R59)",
            missing.join(", ")
        );
    }
    Ok(())
}

/// A repository path as `git ls-tree` prints it, lower case: `/` separators, no `./`, no
/// trailing `/` — how [`check_repo`]'s lookup compares paths (Windows is case-insensitive).
pub fn tree_key(p: &str) -> String {
    p.split(['/', '\\'])
        .filter(|c| !c.is_empty() && *c != ".")
        .collect::<Vec<_>>()
        .join("/")
        .to_lowercase()
}

/// `rel` (with `/` or `\`) below `base`.
pub fn join_rel(base: &Path, rel: &str) -> PathBuf {
    rel.split(['/', '\\'])
        .filter(|c| !c.is_empty() && *c != ".")
        .fold(base.to_path_buf(), |p, c| p.join(c))
}

/// A path as an MSBuild property value: never ending in a separator, which would escape the
/// closing quote in the response file (`"D:\"`) — trailing separators are stripped and a root
/// (`C:\`, `/`) becomes `C:\.`, `/.`.
fn value(p: &Path) -> String {
    let s = p.display().to_string();
    let trimmed = s.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() || trimmed.ends_with(':') {
        format!("{s}.")
    } else {
        trimmed.to_string()
    }
}

/// The MSBuild arguments that build `project` (plan §14, R59). Each property is one argv
/// element — the process layer quotes, nothing here is shell syntax.
pub fn argv(
    tc: &Toolchain,
    checkout: &Path,
    metadata: &str,
    project: &str,
    out: &Path,
) -> Vec<String> {
    let compiler = &tc.compiler().dir;
    let meta = value(&join_rel(checkout, metadata));
    let out = value(out);
    let refs: Vec<String> = tc.packages[1..]
        .iter()
        .map(|p| value(&p.dir.join("ref").join("net40")))
        .chain(tc.extra_references.iter().map(|r| value(r)))
        .chain([meta.clone(), out.clone()])
        .collect();
    vec![
        value(&join_rel(checkout, project)),
        "-nologo".into(),
        "-noAutoResponse".into(),
        "-nodeReuse:false".into(),
        "-v:minimal".into(),
        format!("/p:BuildTasksDirectory={}", value(&compiler.join("DevAlm"))),
        format!("/p:MetadataDirectory={meta}"),
        format!("/p:FrameworkDirectory={}", value(compiler)),
        format!("/p:ReferenceFolder={}", refs.join(";")),
        format!("/p:ReferencePath={}", value(compiler)),
        format!("/p:OutputDirectory={out}"),
        format!("/p:CompilerMetadata={out}"),
    ]
}

/// One MSBuild run of a `tool: xpp` command: its `VERIFY_RESULT.json` name
/// (`<command>/<project stem>`), the project, the arguments of plan §14 ([`argv`]) and how
/// they reach MSBuild ([`invocation`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub name: String,
    pub project: String,
    /// The MSBuild arguments as one list: what is built.
    pub args: Vec<String>,
    /// The response file with the properties (written by [`Run::write_response`]).
    pub rsp: PathBuf,
    /// Its content.
    pub response: String,
    /// What is spawned after MSBuild: the project, the switches, `@<rsp>`.
    pub command: Vec<String>,
}

impl Run {
    /// Write the response file, UTF-8 with a byte-order mark (MSBuild on .NET Framework reads
    /// it with the ANSI code page otherwise).
    pub fn write_response(&self) -> Result<()> {
        if let Some(d) = self.rsp.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(self.response.as_bytes());
        std::fs::write(&self.rsp, bytes).with_context(|| format!("writing {}", self.rsp.display()))
    }
}

/// How the arguments of [`argv`] reach MSBuild. MSBuild splits a `-p:` value at `;` and `,`
/// unless the value is quoted in the command line it parses itself, and the process layer's
/// quoting cannot give it that — so the properties go to a response file, one per line with
/// the value in quotes (values carry no trailing `\`, see [`value`]); the project and the
/// switches (`-noAutoResponse` among them) stay on the command line, followed by `@<rsp>`.
/// `-noAutoResponse` skips only the automatic `MSBuild.rsp`/`Directory.Build.rsp`.
pub fn invocation(args: &[String], rsp: &Path) -> (Vec<String>, String) {
    let (props, rest): (Vec<&String>, Vec<&String>) =
        args.iter().partition(|a| a.starts_with("/p:"));
    let response: String = props
        .iter()
        .map(|p| match p.split_once('=') {
            Some((k, v)) => format!("{k}=\"{v}\"\r\n"),
            None => format!("{p}\r\n"),
        })
        .collect();
    let mut command: Vec<String> = rest.into_iter().cloned().collect();
    command.push(format!("@{}", rsp.display()));
    (command, response)
}

/// `name` as a file name on any system: letters, digits, `.`, `_` and `-`; anything else `-`.
pub fn file_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// The runs of `c`, in project order, each with its response file in `rsp_dir` (a folder the
/// build itself cannot write: the verify folder of the task, not the Low-labelled `build`);
/// `None` for a program command.
pub fn runs(
    c: &CommandSpec,
    tc: &Toolchain,
    checkout: &Path,
    out: &Path,
    rsp_dir: &Path,
) -> Option<Vec<Run>> {
    let CommandKind::Xpp { metadata, projects } = c.kind() else {
        return None;
    };
    Some(
        projects
            .iter()
            .map(|p| {
                let file = p.rsplit(['/', '\\']).next().unwrap_or(p);
                let stem = Path::new(file)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| file.to_string());
                let name = format!("{}/{stem}", c.name);
                let args = argv(tc, checkout, metadata, p, out);
                let rsp = rsp_dir.join(format!("xpp-{}.rsp", file_name(&name)));
                let (command, response) = invocation(&args, &rsp);
                Run {
                    name,
                    project: p.clone(),
                    args,
                    rsp,
                    response,
                    command,
                }
            })
            .collect(),
    )
}

/// The time a command's `timeout_min` gives it (`tool: xpp`: [`DEFAULT_TIMEOUT_MIN`] when unset;
/// a program: 30 minutes, as before).
pub fn command_timeout(c: &CommandSpec) -> Duration {
    // M11: a gate's `timeout` (seconds) wins over minutes
    if let Some(s) = c.timeout_sec {
        return Duration::from_secs(s);
    }
    let default = if c.is_xpp() { DEFAULT_TIMEOUT_MIN } else { 30 };
    Duration::from_secs(60 * c.timeout_min.unwrap_or(default))
}

/// The result of a project that was not started because the command's time was used up.
pub fn not_started(name: &str) -> CommandResult {
    CommandResult {
        name: name.to_string(),
        exit: None,
        timed_out: true,
        cancelled: false,
        duration_ms: 0,
        output: "not started: the earlier projects of this command used up its timeout_min".into(),
        signal: None,
    }
}

/// The toolchain as `VERIFY_RESULT.json` records it: MSBuild and each package's id, version
/// and folder.
pub fn toolchain_json(tc: &Toolchain) -> Value {
    json!({
        "msbuild": tc.msbuild,
        "packages": tc.packages,
        "extra_references": tc.extra_references,
    })
}

/// A project's `VERIFY_RESULT.json` entry, with its parsed diagnostics (B3).
pub fn entry(run: &Run, tc: &Toolchain, res: &CommandResult, diags: &Diagnostics) -> Value {
    let argv: Vec<String> = std::iter::once(tc.msbuild.display().to_string())
        .chain(run.command.iter().cloned())
        .collect();
    json!({
        "name": run.name,
        "kind": "xpp",
        "scope": "compile",
        "project": run.project,
        "argv": argv,
        "response_file": {
            "path": run.rsp,
            "lines": run.response.lines().collect::<Vec<_>>(),
        },
        "exit": res.exit,
        "ms": res.duration_ms,
        "diagnostics": diags.errors,
        "errors_total": diags.errors_total,
        "warnings_total": diags.warnings_total,
        "output_tail": res.tail(40),
        "toolchain": toolchain_json(tc),
    })
}

// ── diagnostics (B3, ARCH-10) ──────────────────────────────────────────────────────────

/// How many errors `VERIFY_RESULT.json` and the fix feedback carry.
pub const DIAGNOSTICS_MAX: usize = 50;

/// One MSBuild diagnostic line: `origin(line,col): [subcategory ]error|warning [CODE]: message`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Diagnostic {
    /// The origin: a file (relative to the checkout, `/` separators, when it lies inside it),
    /// or a tool such as `MSBUILD`; none for a bare `error : …`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// `error` or `warning`.
    pub severity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
}

impl Diagnostic {
    /// The line as the fixer reads it: `src/Foo.xml(12,5): error XPP0001: message`.
    pub fn render(&self) -> String {
        let pos = match (self.line, self.column) {
            (Some(l), Some(c)) => format!("({l},{c})"),
            (Some(l), None) => format!("({l})"),
            _ => String::new(),
        };
        let origin = match &self.file {
            Some(f) => format!("{f}{pos}: "),
            None => String::new(),
        };
        let code = self
            .code
            .as_ref()
            .map(|c| format!(" {c}"))
            .unwrap_or_default();
        format!("{origin}{}{code}: {}", self.severity, self.message)
    }
}

/// The errors of a build's output (the first [`DIAGNOSTICS_MAX`], each once) and the counts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagnostics {
    pub errors: Vec<Diagnostic>,
    pub errors_total: usize,
    pub warnings_total: usize,
}

static LINE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"(?x)^\s*
        (?:(?P<origin>.*?)(?:\((?P<pos>\d+(?:\s*[,-]\s*\d+){0,3})\))?\s*:\s*)?   # origin[(pos)]:
        (?:(?P<sub>[A-Za-z][A-Za-z\ ]*?)\s+)?                                        # subcategory
        (?P<sev>(?i:error|warning))
        (?:\s+(?P<code>[A-Za-z][A-Za-z0-9_.-]*))?
        \s*:\s*(?P<msg>.*?)\s*$",
    )
    .expect("diagnostic regex")
});

static PROJECT_SUFFIX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\s+\[(?P<p>[^\[\]]+)\]$").expect("project suffix regex")
});

/// `/`-separated, ASCII lower case (same byte length): how two Windows paths are compared.
fn norm(p: &str) -> String {
    p.replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

/// `origin` relative to the checkout (with `/`) when it lies inside it; otherwise as written.
fn relative_origin(origin: &str, checkout: &Path) -> String {
    let co = norm(&checkout.display().to_string());
    let o = norm(origin);
    match o.strip_prefix(&co).and_then(|r| r.strip_prefix('/')) {
        Some(rest) if !co.is_empty() => {
            // the original spelling of the part below the checkout (same byte offsets)
            let orig = origin.replace('\\', "/");
            orig[o.len() - rest.len()..o.len()].to_string()
        }
        _ => origin.to_string(),
    }
}

/// One MSBuild diagnostic, or `None` for any other line (`Build FAILED.`, `2 Error(s)`, …).
pub fn parse_line(line: &str, checkout: &Path) -> Option<Diagnostic> {
    let c = LINE.captures(line)?;
    let origin = c
        .name("origin")
        .map(|m| m.as_str().trim())
        .filter(|o| !o.is_empty());
    let mut msg = c["msg"].to_string();
    if let Some(p) = PROJECT_SUFFIX.captures(&msg) {
        let inner = &p["p"];
        if inner.contains(['/', '\\']) || inner.to_ascii_lowercase().contains("proj") {
            msg.truncate(p.get(0).map(|m| m.start()).unwrap_or(msg.len()));
        }
    }
    let nums: Vec<u32> = c
        .name("pos")
        .map(|p| {
            p.as_str()
                .split([',', '-'])
                .filter_map(|n| n.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default();
    Some(Diagnostic {
        file: origin.map(|o| relative_origin(o, checkout)),
        line: nums.first().copied(),
        column: nums.get(1).copied(),
        severity: c["sev"].to_ascii_lowercase(),
        code: c.name("code").map(|m| m.as_str().to_string()),
        message: msg.trim().to_string(),
    })
}

/// The diagnostics of a build's output: every error and warning once (MSBuild repeats them in
/// its final summary), errors in order of appearance, the first [`DIAGNOSTICS_MAX`] kept.
pub fn diagnostics(output: &str, checkout: &Path) -> Diagnostics {
    let mut seen = std::collections::HashSet::new();
    let mut d = Diagnostics::default();
    for line in output.lines() {
        let Some(x) = parse_line(line, checkout) else {
            continue;
        };
        if !seen.insert(x.clone()) {
            continue;
        }
        if x.severity == "error" {
            d.errors_total += 1;
            if d.errors.len() < DIAGNOSTICS_MAX {
                d.errors.push(x);
            }
        } else {
            d.warnings_total += 1;
        }
    }
    d
}

/// The fix feedback of a failed X++ build (inside the `verify` data block): its errors first,
/// then the command's usual "last output" part.
pub fn feedback_errors(d: &Diagnostics) -> String {
    if d.errors.is_empty() {
        return String::new();
    }
    let more = d.errors_total - d.errors.len();
    format!(
        "X++ build errors ({} of {}{}):\n{}\n",
        d.errors.len(),
        d.errors_total,
        if d.warnings_total > 0 {
            format!("; {} warning(s) not listed", d.warnings_total)
        } else {
            String::new()
        },
        d.errors
            .iter()
            .map(|e| format!("  - {}", e.render()))
            .chain((more > 0).then(|| format!("  - … and {more} more")))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// `program` and `args` as one PowerShell line to paste (`& 'C:\…\MSBuild.exe' '…' …`):
/// every element single-quoted, a `'` doubled.
pub fn powershell_line(program: &Path, args: &[String]) -> String {
    let q = |s: &str| format!("'{}'", s.replace('\'', "''"));
    std::iter::once(format!("& {}", q(&program.display().to_string())))
        .chain(args.iter().map(|a| q(a)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The part of `toolchains.xpp` a verify build uses — `None` without a `tool: xpp` command, and
/// without `ude_metadata` (only `doctor` reads it). `task config --reload` verifies again when
/// this changes (another MSBuild or NuGet release), not for a toolchain the task never uses.
pub fn build_toolchain(cfg: &Effective) -> Option<XppToolchain> {
    if !compile_only(cfg) {
        return None;
    }
    cfg.toolchains.xpp.clone().map(|x| XppToolchain {
        ude_metadata: None,
        ..x
    })
}

/// True when a configuration's verify is compile level only (it has a `tool: xpp` command).
pub fn compile_only(cfg: &Effective) -> bool {
    cfg.repo.commands.verify.iter().any(CommandSpec::is_xpp)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `nugets` folder with the five packages (version-suffixed where `versioned`), each
    /// with what the build reads (`DevAlm`, `ref\net40`).
    pub(crate) fn nugets(root: &Path, versioned: bool) -> PathBuf {
        let n = root.join("nugets");
        for id in PACKAGES {
            let name = if versioned {
                format!("{id}.7.0.7198.40")
            } else {
                id.to_string()
            };
            std::fs::create_dir_all(n.join(&name).join(marker(id))).unwrap();
        }
        n
    }

    #[test]
    fn xpp_packages_resolve_plain_and_versioned_folders() {
        let d = tempfile::tempdir().unwrap();
        let n = nugets(d.path(), true);
        // a nuspec wins over the folder name
        std::fs::write(
            n.join(format!("{COMPILER}.7.0.7198.40"))
                .join(format!("{COMPILER}.nuspec")),
            "<package><metadata><id>x</id><version> 7.0.7198.41 </version></metadata></package>",
        )
        .unwrap();
        let p = resolve_packages(&n).unwrap();
        assert_eq!(p.len(), 5);
        assert_eq!(p[0].id, COMPILER);
        assert_eq!(p[0].version.as_deref(), Some("7.0.7198.41"));
        assert_eq!(p[1].version.as_deref(), Some("7.0.7198.40"));
        assert!(p[4].dir.ends_with(format!("{}.7.0.7198.40", PACKAGES[4])));
        // plain folders (nuget install -ExcludeVersion), any case, without a version
        let d = tempfile::tempdir().unwrap();
        let n = nugets(d.path(), false);
        std::fs::rename(n.join(PACKAGES[2]), n.join(PACKAGES[2].to_lowercase())).unwrap();
        let p = resolve_packages(&n).unwrap();
        assert!(p.iter().all(|x| x.version.is_none()));
        assert!(p[2].dir.ends_with(PACKAGES[2].to_lowercase()));
    }

    #[test]
    fn xpp_packages_missing_duplicated_or_only_a_nupkg_are_named() {
        let d = tempfile::tempdir().unwrap();
        let n = nugets(d.path(), true);
        // missing
        std::fs::remove_dir_all(n.join(format!("{}.7.0.7198.40", PACKAGES[3]))).unwrap();
        // duplicated
        std::fs::create_dir_all(n.join(format!("{}.7.0.7100.1", PACKAGES[1]))).unwrap();
        // only a .nupkg
        std::fs::remove_dir_all(n.join(format!("{}.7.0.7198.40", PACKAGES[4]))).unwrap();
        std::fs::write(n.join(format!("{}.10.0.2015.16.nupkg", PACKAGES[4])), "zip").unwrap();
        // a similar-looking folder is not a match
        std::fs::create_dir_all(n.join(format!("{}Extra", PACKAGES[3]))).unwrap();
        // a partly extracted package: the folder without its content
        let compiler = n.join(format!("{COMPILER}.7.0.7198.40"));
        std::fs::remove_dir_all(compiler.join("DevAlm")).unwrap();
        let e = format!("{:#}", resolve_packages(&n).unwrap_err());
        assert!(e.starts_with(UNAVAILABLE), "{e}");
        assert!(
            e.contains(&format!("{}: not found", PACKAGES[3])),
            "missing: {e}"
        );
        assert!(
            e.contains(&format!(
                "{}: found 2 times ({id}.7.0.7100.1, {id}.7.0.7198.40) — keep exactly one",
                PACKAGES[1],
                id = PACKAGES[1]
            )),
            "duplicate: {e}"
        );
        assert!(
            e.contains(&format!("{}: only the package file", PACKAGES[4]))
                && e.contains("a .nupkg is a zip file")
                && e.contains("-ExcludeVersion"),
            "nupkg: {e}"
        );
        assert!(
            e.contains(&format!(
                "{COMPILER}: {COMPILER}.7.0.7198.40 has no DevAlm folder — a partly extracted package?"
            )),
            "partly extracted: {e}"
        );
        // not a folder at all
        let e = format!(
            "{:#}",
            resolve_packages(&d.path().join("nope")).unwrap_err()
        );
        assert!(e.contains("is not a readable folder"), "{e}");
    }

    /// The NuGet cache layout `<id>\<version>\` resolves to the version folder.
    #[test]
    fn xpp_packages_in_the_nuget_cache_layout() {
        let d = tempfile::tempdir().unwrap();
        let n = d.path().join("nugets");
        for id in PACKAGES {
            std::fs::create_dir_all(
                n.join(id.to_lowercase())
                    .join("7.0.7198.40")
                    .join(marker(id)),
            )
            .unwrap();
        }
        let p = resolve_packages(&n).unwrap();
        assert!(p
            .iter()
            .all(|x| x.version.as_deref() == Some("7.0.7198.40")));
        assert!(p[0]
            .dir
            .ends_with(Path::new(&COMPILER.to_lowercase()).join("7.0.7198.40")));
    }

    /// MSBuild splits a `-p:` value at `;` unless it is quoted: the properties go to a response
    /// file with quoted values; the project and the switches stay on the command line.
    #[test]
    fn properties_go_to_a_response_file() {
        let args: Vec<String> = [
            "C:/co/P/Core.Build.rnrproj",
            "-nologo",
            "-noAutoResponse",
            "/p:ReferenceFolder=C:/a/ref/net40;C:/Program Files/isv;C:/co/Metadata",
            "/p:OutputDirectory=C:/b/xpp",
        ]
        .map(String::from)
        .to_vec();
        let (command, response) = invocation(&args, Path::new("C:/v/T-1/xpp-build-Core.Build.rsp"));
        assert_eq!(
            command,
            [
                "C:/co/P/Core.Build.rnrproj",
                "-nologo",
                "-noAutoResponse",
                "@C:/v/T-1/xpp-build-Core.Build.rsp"
            ]
        );
        assert_eq!(
            response,
            "/p:ReferenceFolder=\"C:/a/ref/net40;C:/Program Files/isv;C:/co/Metadata\"\r\n/p:OutputDirectory=\"C:/b/xpp\"\r\n"
        );
        let d = tempfile::tempdir().unwrap();
        let run = Run {
            name: "build/Core.Build".into(),
            project: "P/Core.Build.rnrproj".into(),
            args,
            rsp: d.path().join("x").join("a.rsp"),
            response: response.clone(),
            command,
        };
        run.write_response().unwrap();
        let bytes = std::fs::read(&run.rsp).unwrap();
        assert_eq!(
            &bytes[..3],
            [0xEF, 0xBB, 0xBF],
            "UTF-8 with a byte-order mark"
        );
        assert_eq!(&bytes[3..], response.as_bytes());
    }

    /// M9-B2 AC-2 (`xpp_command_line`): the exact argv, including the order of
    /// `extra_references` and the `<out>` folder; trailing separators are stripped.
    #[test]
    fn xpp_command_line() {
        let d = tempfile::tempdir().unwrap();
        let n = nugets(d.path(), true);
        let isv = d.path().join("isv");
        let tc = resolve(&XppToolchain {
            msbuild: d.path().join("MSBuild.exe").display().to_string(),
            nugets: format!("{}{}", n.display(), std::path::MAIN_SEPARATOR),
            extra_references: vec![
                format!("{}{}", isv.join("B").display(), std::path::MAIN_SEPARATOR),
                isv.join("A").display().to_string(),
            ],
            bp_check: false,
            ude_metadata: None,
        })
        .unwrap();
        let co = d.path().join("T-1").join("T-1.verify");
        let out = d.path().join("T-1").join("build").join("xpp");
        let s = |p: PathBuf| p.display().to_string();
        let pkg = |i: usize| n.join(format!("{}.7.0.7198.40", PACKAGES[i]));
        let meta = s(co.join("Metadata"));
        let refs = [
            s(pkg(1).join("ref").join("net40")),
            s(pkg(2).join("ref").join("net40")),
            s(pkg(3).join("ref").join("net40")),
            s(pkg(4).join("ref").join("net40")),
            s(isv.join("B")),
            s(isv.join("A")),
            meta.clone(),
            s(out.clone()),
        ]
        .join(";");
        assert_eq!(
            argv(
                &tc,
                &co,
                "Metadata/",
                "Projects/Build\\Core.Build.rnrproj",
                &out
            ),
            vec![
                s(co.join("Projects").join("Build").join("Core.Build.rnrproj")),
                "-nologo".into(),
                "-noAutoResponse".into(),
                "-nodeReuse:false".into(),
                "-v:minimal".into(),
                format!("/p:BuildTasksDirectory={}", s(pkg(0).join("DevAlm"))),
                format!("/p:MetadataDirectory={meta}"),
                format!("/p:FrameworkDirectory={}", s(pkg(0))),
                format!("/p:ReferenceFolder={refs}"),
                format!("/p:ReferencePath={}", s(pkg(0))),
                format!("/p:OutputDirectory={}", s(out.clone())),
                format!("/p:CompilerMetadata={}", s(out.clone())),
            ]
        );
        // one run per project, named after the command and the project stem
        let c = CommandSpec::xpp(
            "build",
            "Metadata",
            &["P/Core.Build.rnrproj", "P/Ext.rnrproj"],
            None,
        );
        let r = runs(&c, &tc, &co, &out, d.path()).unwrap();
        assert_eq!(
            r.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            ["build/Core.Build", "build/Ext"]
        );
        assert_eq!(command_timeout(&c), Duration::from_secs(90 * 60));
        let e = entry(
            &r[0],
            &tc,
            &not_started("build/Core.Build"),
            &Diagnostics::default(),
        );
        assert_eq!(e["diagnostics"], json!([]));
        assert_eq!(e["errors_total"], 0);
        assert_eq!(e["kind"], "xpp");
        assert_eq!(e["scope"], "compile");
        assert_eq!(e["argv"][0], s(d.path().join("MSBuild.exe")));
        assert_eq!(e["toolchain"]["packages"][0]["version"], "7.0.7198.40");
        assert!(runs(
            &CommandSpec::program("t", "git", &[], None),
            &tc,
            &co,
            &out,
            d.path()
        )
        .is_none());
        // what is spawned: MSBuild, the project, the switches, the response file
        assert_eq!(r[0].rsp, d.path().join("xpp-build-Core.Build.rsp"));
        assert_eq!(e["argv"].as_array().unwrap().len(), 7);
        assert_eq!(e["argv"][6], format!("@{}", r[0].rsp.display()));
        assert_eq!(e["response_file"]["lines"].as_array().unwrap().len(), 7);
    }

    /// M9-B3 AC-1: realistic MSBuild lines — with and without a column, with a project suffix,
    /// `MSB` errors without a location, warnings, the repeated summary, paths outside the
    /// checkout — and the lines that are not diagnostics.
    #[test]
    fn msbuild_lines_are_parsed() {
        let co = Path::new("C:/verify/T-1/T-1.verify");
        let p = |l: &str| parse_line(l, co);
        let d = p(r"  C:\verify\T-1\T-1.verify\Metadata\Core\Core\AxClass\CoreTax.xml(12,5): error XPP0101: 'amount' is not declared. [C:\verify\T-1\T-1.verify\Projects\Build\Core.Build.rnrproj]").unwrap();
        assert_eq!(
            d.file.as_deref(),
            Some("Metadata/Core/Core/AxClass/CoreTax.xml")
        );
        assert_eq!((d.line, d.column), (Some(12), Some(5)));
        assert_eq!(d.severity, "error");
        assert_eq!(d.code.as_deref(), Some("XPP0101"));
        assert_eq!(d.message, "'amount' is not declared.");
        assert_eq!(
            d.render(),
            "Metadata/Core/Core/AxClass/CoreTax.xml(12,5): error XPP0101: 'amount' is not declared."
        );
        // line only, the long form, no code, other case of the checkout
        let d = p(r"c:\VERIFY\t-1\T-1.verify\Metadata\Core\Descriptor\Core.xml(3): error : Model reference missing").unwrap();
        assert_eq!(d.file.as_deref(), Some("Metadata/Core/Descriptor/Core.xml"));
        assert_eq!((d.line, d.column, d.code.as_deref()), (Some(3), None, None));
        let d = p(r"C:\verify\T-1\T-1.verify\x.xml(1,2,3,4): warning BP123: long form").unwrap();
        assert_eq!(
            (d.line, d.column, d.severity.as_str()),
            (Some(1), Some(2), "warning")
        );
        // no location
        let d = p("MSBUILD : error MSB1009: Project file does not exist.").unwrap();
        assert_eq!(d.file.as_deref(), Some("MSBUILD"));
        assert_eq!((d.code.as_deref(), d.line), (Some("MSB1009"), None));
        assert_eq!(d.message, "Project file does not exist.");
        let d = p("error : The build tasks folder was not found").unwrap();
        assert_eq!((d.file.as_deref(), d.code.as_deref()), (None, None));
        // a subcategory
        let d = p(r"Main.cs(17,20): Command line warning CS0168: never used").unwrap();
        assert_eq!(
            (d.severity.as_str(), d.code.as_deref()),
            ("warning", Some("CS0168"))
        );
        // outside the checkout: kept as written
        let d = p(r"C:\Program Files\MSBuild\Microsoft.Common.targets(2302,5): warning MSB3245: Could not resolve this reference. [C:\verify\T-1\T-1.verify\Projects\Build\Core.Build.rnrproj]").unwrap();
        assert_eq!(
            d.file.as_deref(),
            Some(r"C:\Program Files\MSBuild\Microsoft.Common.targets")
        );
        assert_eq!(d.message, "Could not resolve this reference.");
        // a bracket that is not a project stays in the message
        let d = p("error X1: index [3] out of range").unwrap();
        assert_eq!(d.message, "index [3] out of range");
        for other in [
            "Build FAILED.",
            "    2 Error(s)",
            "    0 Warning(s)",
            "  Core -> C:\\out\\Core.dll",
            "Time Elapsed 00:01:02.34",
            "Compiling Foo: error count 0",
            "",
        ] {
            assert_eq!(p(other), None, "{other}");
        }
    }

    /// The summary repeats every error: each is kept once; warnings are counted, errors capped.
    #[test]
    fn diagnostics_are_deduplicated_counted_and_capped() {
        let co = Path::new("/v/T/T.verify");
        let mut out = String::new();
        let e1 = "/v/T/T.verify/src/A.xml(1,1): error XPP1: one [/v/T/T.verify/P/B.rnrproj]";
        let e2 = "/v/T/T.verify/src/B.xml(2,1): error XPP2: two [/v/T/T.verify/P/B.rnrproj]";
        let w = "/v/T/T.verify/src/C.xml(3,1): warning XPP3: three";
        for l in [e1, w, e2, "Build FAILED.", "", e1, w, e2, "    2 Error(s)"] {
            out.push_str(l);
            out.push('\n');
        }
        let d = diagnostics(&out, co);
        assert_eq!((d.errors_total, d.warnings_total), (2, 1));
        assert_eq!(
            d.errors
                .iter()
                .map(|e| e.file.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["src/A.xml", "src/B.xml"]
        );
        let f = feedback_errors(&d);
        assert!(f.starts_with("X++ build errors (2 of 2; 1 warning(s) not listed):\n  - src/A.xml(1,1): error XPP1: one\n"), "{f}");
        let many: String = (0..70)
            .map(|i| format!("/v/T/T.verify/src/X{i}.xml(1,1): error XPP9: e{i}\n"))
            .collect();
        let d = diagnostics(&many, co);
        assert_eq!((d.errors.len(), d.errors_total), (DIAGNOSTICS_MAX, 70));
        assert!(feedback_errors(&d).ends_with("  - … and 20 more\n"));
        assert_eq!(feedback_errors(&Diagnostics::default()), "");
    }

    /// `task config --reload` verifies again only for what the build uses: not for a toolchain a
    /// program-only task never uses, not for `ude_metadata` (doctor only).
    #[test]
    fn a_reload_compares_only_the_build_toolchain() {
        let d = tempfile::tempdir().unwrap();
        let load = |verify: &str, ude: &str| {
            let (p, m) = (d.path().join("t.yaml"), d.path().join("m.yaml"));
            std::fs::write(&p, format!("version: 1\nproject: {{ root: r, worktrees_root: w }}\ncommands:\n  verify:\n    - {verify}\n")).unwrap();
            std::fs::write(
                &m,
                format!("toolchains:\n  xpp: {{ msbuild: m, nugets: n, ude_metadata: {ude} }}\n"),
            )
            .unwrap();
            Effective::load(&p, Some(&m)).unwrap()
        };
        let prog = "{ name: t, program: git, args: [status] }";
        let xpp = "{ name: b, tool: xpp, metadata: M, projects: [p.rnrproj] }";
        assert_eq!(build_toolchain(&load(prog, "a")), None);
        assert_eq!(
            build_toolchain(&load(xpp, "a")),
            build_toolchain(&load(xpp, "b"))
        );
        assert!(build_toolchain(&load(xpp, "a")).is_some());
    }

    /// Admission looks the metadata folder and the projects up in the base commit (a lookup,
    /// here a set), case-insensitively; a file named like the folder does not count.
    #[test]
    fn the_repository_must_hold_the_metadata_and_the_projects() {
        let d = tempfile::tempdir().unwrap();
        let (p, m) = (d.path().join("t.yaml"), d.path().join("m.yaml"));
        std::fs::write(&p, "version: 1\nproject: { root: r, worktrees_root: w }\ncommands:\n  verify:\n    - { name: b, tool: xpp, metadata: Metadata, projects: [P/Core.Build.rnrproj, P/Missing.rnrproj] }\n").unwrap();
        std::fs::write(&m, "toolchains:\n  xpp: { msbuild: m, nugets: n }\n").unwrap();
        let cfg = Effective::load(&p, Some(&m)).unwrap();
        let tree: std::collections::BTreeMap<&str, bool> = [
            ("metadata", true),
            ("p", true),
            ("p/core.build.rnrproj", false),
        ]
        .into();
        let kind = |x: &str| tree.get(tree_key(x).as_str()).copied();
        let e = format!(
            "{:#}",
            check_repo(&cfg, "commit abc of R", &kind).unwrap_err()
        );
        assert_eq!(
            e,
            "xpp_project_missing: not in commit abc of R — b: project P/Missing.rnrproj (create and commit the build projects first — if they exist, `git check-ignore -v <path>` shows whether .gitignore hides them; they are protected, R59)"
        );
        assert_eq!(tree_key(".\\P\\Core.Build.rnrproj"), "p/core.build.rnrproj");
        assert_eq!(file_name("build:core/X.Build"), "build-core-X.Build");
    }

    #[test]
    fn a_powershell_line_quotes_every_element() {
        assert_eq!(
            powershell_line(
                Path::new("C:/VS/MSBuild.exe"),
                &["C:/r/x.rnrproj".into(), "/p:A=it's;b".into()]
            ),
            "& 'C:/VS/MSBuild.exe' 'C:/r/x.rnrproj' '/p:A=it''s;b'"
        );
    }

    #[test]
    fn a_property_value_never_ends_in_a_separator() {
        assert_eq!(value(Path::new("/")), "/.");
        assert_eq!(value(Path::new("/a/b/")), "/a/b");
        assert_eq!(value(Path::new("C:\\")), "C:\\.");
        assert_eq!(value(Path::new("C:\\x\\\\")), "C:\\x");
    }
}
