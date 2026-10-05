//! M12-21: the short commit this binary is built from, for `toryo --version` and `doctor`
//! (`TORYO_COMMIT`; "unknown" where there is no Git or no repository).

use std::path::PathBuf;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let o = Command::new("git").args(args).output().ok()?;
    if !o.status.success() {
        return None;
    }
    let s = String::from_utf8(o.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn main() {
    let commit = git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=TORYO_COMMIT={commit}");
    println!("cargo:rerun-if-changed=build.rs");
    // rebuild when the commit moves: HEAD itself, and the branch it names
    if let Some(dir) = git(&["rev-parse", "--git-dir"]).map(PathBuf::from) {
        let head = dir.join("HEAD");
        println!("cargo:rerun-if-changed={}", head.display());
        if let Some(r) = std::fs::read_to_string(&head)
            .ok()
            .and_then(|t| t.trim().strip_prefix("ref: ").map(str::to_string))
        {
            println!("cargo:rerun-if-changed={}", dir.join(r).display());
        }
    }
}
