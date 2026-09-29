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

/// The five packages under `nugets`: exactly one folder per id, named `<id>` or
/// `<id>.<version>`. Every problem is reported at once: a missing package, one found twice,
/// and one that is only a `.nupkg` (with how to extract it).
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
            [(name, dir, _)] => {
                let suffix = package_suffix(name, id).filter(|s| !s.is_empty());
                found.push(Package {
                    id: id.to_string(),
                    version: nuspec_version(dir, id).or(suffix.map(str::to_string)),
                    dir: dir.clone(),
                });
            }
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

/// `rel` (with `/` or `\`) below `base`.
pub fn join_rel(base: &Path, rel: &str) -> PathBuf {
    rel.split(['/', '\\'])
        .filter(|c| !c.is_empty() && *c != ".")
        .fold(base.to_path_buf(), |p, c| p.join(c))
}

/// A path as an MSBuild property value: without trailing separators (a root such as `C:\`
/// or `/` keeps its own).
fn value(p: &Path) -> String {
    let s = p.display().to_string();
    let trimmed = s.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() || trimmed.ends_with(':') {
        s
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
/// (`<command>/<project stem>`), the project and the arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub name: String,
    pub project: String,
    pub args: Vec<String>,
}

/// The runs of `c`, in project order; `None` for a program command.
pub fn runs(c: &CommandSpec, tc: &Toolchain, checkout: &Path, out: &Path) -> Option<Vec<Run>> {
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
                Run {
                    name: format!("{}/{stem}", c.name),
                    project: p.clone(),
                    args: argv(tc, checkout, metadata, p, out),
                }
            })
            .collect(),
    )
}

/// The time a command's `timeout_min` gives it (`tool: xpp`: [`DEFAULT_TIMEOUT_MIN`] when unset;
/// a program: 30 minutes, as before).
pub fn command_timeout(c: &CommandSpec) -> Duration {
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

/// A project's `VERIFY_RESULT.json` entry.
pub fn entry(run: &Run, tc: &Toolchain, res: &CommandResult) -> Value {
    let argv: Vec<String> = std::iter::once(tc.msbuild.display().to_string())
        .chain(run.args.iter().cloned())
        .collect();
    json!({
        "name": run.name,
        "kind": "xpp",
        "scope": "compile",
        "project": run.project,
        "argv": argv,
        "exit": res.exit,
        "ms": res.duration_ms,
        "output_tail": res.tail(40),
        "toolchain": toolchain_json(tc),
    })
}

/// True when a configuration's verify is compile level only (it has a `tool: xpp` command).
pub fn compile_only(cfg: &Effective) -> bool {
    cfg.repo.commands.verify.iter().any(CommandSpec::is_xpp)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `nugets` folder with the five packages (version-suffixed where `versioned`).
    pub(crate) fn nugets(root: &Path, versioned: bool) -> PathBuf {
        let n = root.join("nugets");
        for id in PACKAGES {
            let name = if versioned {
                format!("{id}.7.0.7198.40")
            } else {
                id.to_string()
            };
            std::fs::create_dir_all(n.join(&name).join("ref").join("net40")).unwrap();
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
        assert!(!e.contains(COMPILER), "the compiler package is fine: {e}");
        // not a folder at all
        let e = format!(
            "{:#}",
            resolve_packages(&d.path().join("nope")).unwrap_err()
        );
        assert!(e.contains("is not a readable folder"), "{e}");
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
        let r = runs(&c, &tc, &co, &out).unwrap();
        assert_eq!(
            r.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            ["build/Core.Build", "build/Ext"]
        );
        assert_eq!(command_timeout(&c), Duration::from_secs(90 * 60));
        let e = entry(&r[0], &tc, &not_started("build/Core.Build"));
        assert_eq!(e["kind"], "xpp");
        assert_eq!(e["scope"], "compile");
        assert_eq!(e["argv"][0], s(d.path().join("MSBuild.exe")));
        assert_eq!(e["toolchain"]["packages"][0]["version"], "7.0.7198.40");
        assert!(runs(&CommandSpec::program("t", "git", &[], None), &tc, &co, &out).is_none());
    }

    #[test]
    fn a_property_value_keeps_a_root_but_not_a_trailing_separator() {
        assert_eq!(value(Path::new("/")), "/");
        assert_eq!(value(Path::new("/a/b/")), "/a/b");
        assert_eq!(value(Path::new("C:\\")), "C:\\");
        assert_eq!(value(Path::new("C:\\x\\\\")), "C:\\x");
    }
}
