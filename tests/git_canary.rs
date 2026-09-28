//! R33 hook canary + R37 filter/textconv canary against the real `git` on this machine
//! (closes M0-VERIFY 10 for this Git version; re-run `toryo spike git-canary` on Windows).

use toryo::git::canary::{run_all, Variant};

#[test]
fn git_canaries_pass() {
    let work = tempfile::tempdir().unwrap();
    let exe = env!("CARGO_BIN_EXE_toryo").replace('\\', "/");
    let trusted = format!("'{exe}' spike filter-process --marker");
    let reports = run_all(std::path::Path::new("git"), work.path(), Some(&trusted)).unwrap();
    for r in &reports {
        eprintln!(
            "{:?}: pass={} markers={:?} unexpected={:?} err={:?}\n  {}",
            r.variant, r.pass, r.markers, r.unexpected_markers, r.admission_error, r.note
        );
    }
    let control = reports
        .iter()
        .find(|r| r.variant == Variant::Unhardened)
        .unwrap();
    assert!(
        control.pass,
        "negative control must show markers with plain git"
    );
    for r in &reports {
        assert!(r.pass, "{:?} failed: {:?}", r.variant, r);
    }
}
