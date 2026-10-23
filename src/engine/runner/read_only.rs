//! A read-only role's write (R23-27, moved out of `runner.rs` as it was): the worktree around
//! a Planner or Reviewer turn (EC-31, F5), what the role wrote, and how it is put back on the
//! task branch (M13-06, X24, X32) — also by recovery.

use super::*;

/// The task worktree around a read-only turn (EC-31, F5): HEAD, and every changed path —
/// tracked or untracked, not ignored — with its status and a hash of its content. Saved in the
/// run store before every read-only turn (M13-06) so that recovery has it too.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::engine) struct TreeState {
    /// `None`: HEAD does not resolve.
    pub(super) head: Option<String>,
    /// path → (`git status` code, content hash).
    pub(super) entries: BTreeMap<String, (String, String)>,
}

/// M13-06: the artifact that holds the [`TreeState`] before the latest turn of the read-only
/// attempt `a`.
pub(super) fn tree_before_name(a: &AttemptRow) -> String {
    format!("tree-before/{}-{}", a.stage, a.attempt)
}

/// M13-06: the worktree as the latest turn of the read-only attempt `a` found it — `None` for
/// an attempt from before M13, or one that died before its first turn.
pub(in crate::engine) fn tree_before(
    store: &Store,
    t: &TaskRow,
    a: &AttemptRow,
) -> Result<Option<TreeState>> {
    Ok(
        match crate::engine::artifacts::read_head(store, t, &tree_before_name(a))? {
            Some((_, bytes)) => serde_json::from_slice(&bytes).ok(),
            None => None,
        },
    )
}

/// The largest file a copy of a read-only role's write keeps (M13-06).
pub(super) const READ_ONLY_COPY_MAX: u64 = 8 * 1024 * 1024;

/// A copy of what a read-only role wrote at `from`, for the human to look at (M13-06): never
/// through a link (the link itself is neither copied nor followed), never more than
/// [`READ_ONLY_COPY_MAX`]. `Some(why)`: not copied.
pub(super) fn keep_copy(from: &Path, to: &Path) -> Option<String> {
    let md = std::fs::symlink_metadata(from).ok()?; // gone: nothing to copy
    if md.file_type().is_symlink() {
        return Some("a link: not followed".into());
    }
    if !md.is_file() {
        return Some("not a regular file".into());
    }
    if md.len() > READ_ONLY_COPY_MAX {
        return Some(format!("{} bytes, over the 8 MB a copy keeps", md.len()));
    }
    if let Some(dir) = to.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::copy(from, to)
        .err()
        .map(|e| format!("the copy failed: {e}"))
}

/// The first folder on the way to `rel` (below `root`) that is a link or a junction, as a
/// `/`-separated relative path — the path itself is not asked (`git status` lists a link as a
/// path of its own, and `restore` removes that as a link).
pub(super) fn link_on_the_way(root: &Path, rel: &str) -> Option<String> {
    let parts: Vec<&str> = rel.split('/').collect();
    let mut at = root.to_path_buf();
    for (i, part) in parts.iter().enumerate().take(parts.len().saturating_sub(1)) {
        at.push(part);
        if std::fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink()) {
            return Some(parts[..=i].join("/"));
        }
    }
    None
}

/// What a read-only role (Planner, Reviewer) did to the task worktree since `before`, undone —
/// after its turn, however that ended, and in recovery when the engine died after it (M12-03,
/// M13-01, M13-06). `None`: it changed nothing. Otherwise the escalation reason, after:
/// - a commit it made on the task branch is undone (soft reset to where the turn began, X24);
/// - every path that was clean or absent before the turn is put back (tracked → HEAD, new →
///   removed; a link is removed as a link), after a copy for the human;
/// - a path that was **already changed before** the turn and changed again is copied and left
///   as it is — Toryo cannot tell the owner's edit from the role's — and the reason says so.
///
/// What the owner had in the tree before the turn and the role did not touch is not looked at.
pub(in crate::engine) fn undo_read_only_write(
    wt: &Worktree,
    t: &TaskRow,
    attempt: &str,
    before: &TreeState,
) -> Result<Option<Undone>> {
    let mut after = TreeState::read(wt)?;
    let Some(mut reason) = read_only_write(before, &after) else {
        return Ok(None);
    };
    if let (Some(began), true) = (&before.head, before.head != after.head) {
        let on_branch = wt.head_ref().ok().flatten() == Some(format!("refs/heads/{}", t.branch));
        if on_branch && wt.unfinished_operation()?.is_none() {
            wt.soft_reset(began)?;
            crate::errln!(
                "   the read-only role had moved HEAD — soft reset to {}",
                short(began)
            );
            after = TreeState::read(wt)?;
        }
    }
    if before.head != after.head {
        // (no commit to go back to, ever: nothing is pending)
        let pending = before.head.is_some();
        return Ok(Some(Undone {
            reason: could_not_undo(wt, t, attempt, before, &after, reason),
            pending,
        }));
    }
    // (again, written): changed before the turn and changed again / clean or absent before it
    let split = |after: &TreeState| -> (Vec<String>, Vec<String>) {
        after
            .changed_since(before)
            .into_iter()
            .filter(|p| !ide_state(p))
            .partition(|p| before.entries.contains_key(p))
    };
    let (again, written) = split(&after);
    if written.is_empty() && again.is_empty() {
        return Ok(Some(Undone::done(reason)));
    }
    // AC-3: nothing is copied, put back or deleted through a link or junction — Git lists the
    // files behind a junction like any folder's. A link the role made is removed as a link; one
    // that was there before the turn (a path below it was already changed then) is left alone,
    // and the reason says so.
    let mut links: std::collections::BTreeSet<String> = Default::default();
    for p in written.iter().chain(&again) {
        if let Some(link) = link_on_the_way(&t.worktree, p) {
            let under = format!("{link}/");
            if !before.entries.keys().any(|k| k.starts_with(&under)) {
                links.insert(link);
            }
        }
    }
    for link in &links {
        let p = t.worktree.join(link);
        if std::fs::remove_file(&p).is_err() {
            let _ = std::fs::remove_dir(&p); // a directory link or junction: the link only
        }
    }
    // what a removed link stood in place of — a tracked folder the role deleted first — shows
    // only now: read the tree again
    let (again, written) = if links.is_empty() {
        (again, written)
    } else {
        split(&TreeState::read(wt)?)
    };
    let mut behind: Vec<String> = Vec::new();
    let mut direct = |paths: Vec<String>| -> Vec<String> {
        paths
            .into_iter()
            .filter(|p| {
                let link = link_on_the_way(&t.worktree, p).is_some();
                if link {
                    behind.push(p.clone());
                }
                !link
            })
            .collect()
    };
    let (written, again) = (direct(written), direct(again));
    if !links.is_empty() {
        reason.push_str(&format!(
            "; removed as links, not followed: {:?}",
            links.iter().collect::<Vec<_>>()
        ));
    }
    if !behind.is_empty() {
        reason.push_str(&format!(
            "; not touched (behind a link Toryo left in place: it was there before the turn, or could not be removed): {behind:?}"
        ));
    }
    // a copy first: the human may want to see what it wrote
    let keep = t
        .run_store
        .join("read-only-writes")
        .join(attempt.replace('/', "-"));
    let not_copied: Vec<String> = written
        .iter()
        .chain(&again)
        .filter_map(|p| {
            keep_copy(&t.worktree.join(p), &keep.join(p)).map(|why| format!("{p} ({why})"))
        })
        .collect();
    if !written.is_empty() {
        wt.restore(&written)?;
        // `restore` does not say when Git refused (a stale `index.lock`, a file in use): look
        let now = split(&TreeState::read(wt)?).1;
        let (left, back): (Vec<String>, Vec<String>) =
            written.into_iter().partition(|p| now.contains(p));
        if !back.is_empty() {
            crate::errln!(
                "   put back (written by the read-only role): {} — a copy is in {}",
                paths_words(&back),
                keep.display()
            );
        }
        if !left.is_empty() {
            reason.push_str(&format!(
                "; could NOT be put back: {} — Git refused (look at `git -C \"{}\" status`); put them back by hand, or they go into the next checkpoint",
                paths_words(&left),
                t.worktree.display()
            ));
        }
    }
    // (M13-R7: a file the role *deleted* has nothing to copy — the reason must not say it was)
    let (again_gone, again): (Vec<String>, Vec<String>) = again
        .into_iter()
        .partition(|p| std::fs::symlink_metadata(t.worktree.join(p)).is_err());
    if !again.is_empty() {
        reason.push_str(&format!(
            "; not put back: {} — changed already before the turn, so Toryo cannot tell your edit from the role's (what is there now is copied to {})",
            paths_words(&again),
            keep.display()
        ));
    }
    if !again_gone.is_empty() {
        reason.push_str(&format!(
            "; not put back: {} — changed already before the turn and gone now, so Toryo cannot tell your edit from the role's; gone, nothing to copy",
            paths_words(&again_gone),
        ));
    }
    if !not_copied.is_empty() {
        reason.push_str(&format!("; no copy kept of: {}", not_copied.join(", ")));
    }
    Ok(Some(Undone::done(reason)))
}

/// What the undo of a read-only role's write found: the reason for the escalation, and whether
/// the undo is still `pending` (R23-36: a typed flag the attempt keeps, `undo_pending`; was the
/// sentence [`UNDO_PENDING`] searched for in its `error_detail`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::engine) struct Undone {
    pub(in crate::engine) reason: String,
    pub(in crate::engine) pending: bool,
}

impl Undone {
    fn done(reason: String) -> Self {
        Self {
            reason,
            pending: false,
        }
    }
}

impl std::fmt::Display for Undone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

/// M13-R4 (X32): the newest attempt is a read-only one that ended with "could not be put back
/// yet" (its `undo_pending`, R23-36), and HEAD is on the task branch again with no Git
/// operation unfinished: run the undo with the state that attempt saved before its turn.
/// `None`: nothing was pending (or it cannot be done yet).
pub(in crate::engine) fn finish_pending_undo(
    store: &Store,
    t: &TaskRow,
    attempts: &[AttemptRow],
    wt: &Worktree,
) -> Result<Option<Undone>> {
    let Some(a) = attempts.last() else {
        return Ok(None);
    };
    if a.status != AttemptStatus::Failed || !a.undo_pending {
        return Ok(None);
    }
    let on_branch = wt.head_ref().ok().flatten() == Some(format!("refs/heads/{}", t.branch));
    if !on_branch || wt.unfinished_operation()?.is_some() {
        return Ok(None);
    }
    match tree_before(store, t, a)? {
        Some(before) => undo_read_only_write(wt, t, &a.id, &before),
        None => Ok(None),
    }
}

/// The tail of a reason that says a read-only role's write could not be undone yet (M13-R4,
/// X32), for the owner; the attempt's `undo_pending` is what the next writing attempt and
/// recovery read (R23-36). The words live with the store, which migrates a database of schema
/// version 2 by them.
pub(in crate::engine) const UNDO_PENDING: &str = store::UNDO_PENDING_WORDS;

/// M13-R4 (X32): the role moved HEAD and it cannot be reset now (HEAD is off the task branch, or
/// a Git operation is unfinished). Nothing is reset or put back — a reset would move another
/// branch — but the reason says so, what is in the tree is copied for the owner, and the
/// attempt's record ([`UNDO_PENDING`]) makes the next resume undo it once the branch is back.
pub(super) fn could_not_undo(
    wt: &Worktree,
    t: &TaskRow,
    attempt: &str,
    before: &TreeState,
    after: &TreeState,
    mut reason: String,
) -> String {
    if before.head.is_none() {
        return reason; // no commit to go back to, ever
    }
    let on_branch = wt.head_ref().ok().flatten() == Some(format!("refs/heads/{}", t.branch));
    let (why, way_out) = if on_branch {
        (
            "a Git operation is unfinished in the worktree".to_string(),
            "after that operation is finished or aborted".to_string(),
        )
    } else {
        (
            format!("HEAD is not on {}", t.branch),
            format!("after `git checkout {}`", t.branch),
        )
    };
    // never through a link: a path behind one is not copied
    let paths: Vec<String> = after
        .changed_since(before)
        .into_iter()
        .filter(|p| !ide_state(p) && link_on_the_way(&t.worktree, p).is_none())
        .collect();
    let keep = t
        .run_store
        .join("read-only-writes")
        .join(attempt.replace('/', "-"));
    let not_copied: Vec<String> = paths
        .iter()
        .filter_map(|p| {
            keep_copy(&t.worktree.join(p), &keep.join(p)).map(|why| format!("{p} ({why})"))
        })
        .collect();
    let in_tree = if paths.is_empty() {
        String::new()
    } else {
        format!(" — in the tree: {paths:?}, copied to {}", keep.display())
    };
    reason.push_str(&format!(
        "; could NOT be put back ({why}){in_tree}: {way_out} {UNDO_PENDING}"
    ));
    if !not_copied.is_empty() {
        reason.push_str(&format!("; no copy kept of: {}", not_copied.join(", ")));
    }
    reason
}

impl TreeState {
    pub(super) fn read(wt: &Worktree) -> Result<Self> {
        let entries = wt
            .changed()?
            .into_iter()
            .map(|(status, path)| {
                let hash = content_hash(&wt.path.join(&path));
                (path, (status, hash))
            })
            .collect();
        Ok(Self {
            head: wt.head().ok(),
            entries,
        })
    }

    /// The paths that differ from `before`: new, removed, or with another status or content.
    fn changed_since(&self, before: &TreeState) -> Vec<String> {
        let mut paths: Vec<String> = self
            .entries
            .iter()
            .filter(|(p, e)| before.entries.get(*p) != Some(*e))
            .map(|(p, _)| p.clone())
            .collect();
        paths.extend(
            before
                .entries
                .keys()
                .filter(|p| !self.entries.contains_key(*p))
                .cloned(),
        );
        paths.sort();
        paths
    }
}

/// A changed path's content, as a comparable string: the SHA-256 of a file's bytes, a link's
/// target (never followed), or what the path is when it has no bytes to hash (gone, a
/// directory such as a nested repository, a FIFO or device — never opened, it could block).
pub(super) fn content_hash(p: &Path) -> String {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let md = match std::fs::symlink_metadata(p) {
        Ok(md) => md,
        Err(_) => return "absent".into(),
    };
    if md.file_type().is_symlink() {
        let to = std::fs::read_link(p)
            .map(|t| t.display().to_string())
            .unwrap_or_default();
        return format!("link {to}");
    }
    if md.is_dir() {
        return "directory".into();
    }
    if !md.is_file() {
        return "special".into();
    }
    let hash = std::fs::File::open(p).and_then(|mut f| {
        let (mut h, mut buf) = (Sha256::new(), vec![0u8; 1 << 16]);
        loop {
            match f.read(&mut buf)? {
                0 => return Ok(hex::encode(h.finalize())),
                n => h.update(&buf[..n]),
            }
        }
    });
    hash.unwrap_or_else(|e| format!("unreadable ({})", e.kind()))
}

/// R23-22: paths for a reason — all of them as a list up to [`PATHS_SHOWN`], else the first
/// ones and how many there are. Was: every path: a reviewer that ran a build leaving ten
/// thousand files put megabytes into `tasks.machine`, the console and SUMMARY.md.
pub(super) fn paths_words(paths: &[String]) -> String {
    if paths.len() <= PATHS_SHOWN {
        format!("{paths:?}")
    } else {
        format!("{:?} … ({} in all)", &paths[..PATHS_SHOWN], paths.len())
    }
}

/// How many paths a reason lists.
pub(super) const PATHS_SHOWN: usize = 20;

/// EC-31: a read-only role changed the worktree during its turn (tracked or untracked, not
/// ignored: a path new, gone or different since `before`) or moved HEAD — a policy violation
/// (the wording of recovery's check). What was already there before the turn is not its
/// write (F5).
pub(super) fn read_only_write(before: &TreeState, after: &TreeState) -> Option<String> {
    let changed: Vec<String> = after
        .changed_since(before)
        .into_iter()
        .filter(|p| !ide_state(p))
        .collect();
    if !changed.is_empty() {
        return Some(format!(
            "policy_violation: the worktree changed during a read-only stage: {}",
            paths_words(&changed)
        ));
    }
    (before.head != after.head).then(|| {
        let at = |h: &Option<String>| h.as_deref().map(short).unwrap_or("?").to_string();
        format!(
            "policy_violation: HEAD moved during a read-only stage ({} → {})",
            at(&before.head),
            at(&after.head)
        )
    })
}

/// An IDE's own state folder (Visual Studio `.vs/`, JetBrains `.idea/`) at any depth — Visual
/// Studio puts `.vs/` next to the `.sln`, often under `Projects/<Model>/` in a D365FO repo: an
/// IDE opened on the task worktree creates it at any time, so it is never a read-only role's
/// write.
pub(super) fn ide_state(path: &str) -> bool {
    path.split(['/', '\\'])
        .any(|c| c.eq_ignore_ascii_case(".vs") || c.eq_ignore_ascii_case(".idea"))
}
