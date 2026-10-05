# M13 notes — the review of M12 (`M13_tasks.md`)

Branch `m13/review-fixes` from `main` @ `685aa45` (the merge of M12, pull request #5). One commit per task. Baseline:
**683 passed, 0 failed, 6 ignored** on Windows; 705 passed on Linux (the review lead's container build of `e28a866`). The task list is
`C:\.thien\AgentOrchestrator\M13_tasks.md` (outside the repository). Every "Problem" in it is *read*: the failing test comes first.

## Progress

- [x] M13-01 The anchor: a writing attempt starts at the last commit Toryo recorded (X24)
- [x] M13-02 A fresh checkpoint runs the full path policy; a refused found commit does not loop (X25)
- [x] M13-03 The redactor: a line break may only be the last byte of a secret
- [x] M13-04 Gate output is cut on line boundaries; the log path the vendor sees is relative (X27)
- [x] M13-05 The deploy confirmation is a database row bound to gate, commit and digest (X26)
- [x] M13-06 Read-only turns: the checks also run when the turn errors; the copy is safe
- [x] Review of M13-01…06 (acceptance criteria against code and tests)
- [ ] M13-07 `task cleanup` that cannot look fails closed; `doctor` never loops on a link
- [ ] M13-08 The gate resolver is one resolver; the timeout stop names every gate that re-runs
- [ ] M13-09 `environment_gate_confirm` says what to type, and how to say no
- [ ] M13-10 `doctor` looks at long paths where the first run will hit them
- [ ] M13-11 `task create` is all or nothing (X28)
- [ ] M13-12 Ctrl+C on `toryo run` is a stop request (X29)
- [ ] M13-13 A launch error before the agent starts does not reserve a budget
- [ ] M13-14 Tests the review asked for
- [ ] M13-15 Notes, guide, `STATUS.md`
- [ ] Finish, push, pull request, CI

## Tasks

### M13-01 — The anchor: a writing attempt starts at the last commit Toryo recorded (X24)

- **Failing tests first** (`tests/regress_recovery.rs`, all three failed at `685aa45`): `m13_01_a_commit_left_at_the_branch_tip_is_undone_before_the_next_attempt`
  (AC-1: the task completed with the agent's commit and its denied file on the branch), `m13_01_a_commit_by_the_reviewer_is_undone_and_its_file_put_back`
  (AC-2: HEAD stayed at the reviewer's commit), `m13_01_a_head_that_does_not_descend_from_the_anchor_escalates` (AC-3: the next attempt checkpointed on
  the orphan).
- **`anchor_of(task, attempts)`** (`runner.rs`): the newest attempt's `git_head_after`, else its `partial_commit`, else `base_commit`. No HEAD, no author,
  no committer is read. Unit test `the_anchor_is_the_last_commit_toryo_recorded`.
- **`Engine::start_at_anchor`**, called in `run_implement_turn` (before `begin`) and at the top of `checkpoint()`'s fresh path: HEAD == anchor → go on.
  Otherwise, when HEAD is on the task branch, descends from the anchor and no Git operation is unfinished: `soft_reset(anchor)`, one `note:` ("commits
  <anchor>..<head> on top of the anchor were undone; their changes stay in the tree and are checked like any turn's") and a `policy_violation` event; the
  attempt then begins at the anchor. Else `Escalated` (`policy_violation`): both commits, why, "nothing was reset or committed", and the way out by hand
  (`git checkout <branch>`, `git reset --soft <anchor>`, `toryo resume <id> --note "…"`).
- **Read-only roles** (`read_only_checks`): when the turn moved HEAD and HEAD is on the task branch, HEAD is soft-reset to where the turn began first; the
  tree is read again and the existing put-back runs on what changed since `before` — so a file the role *committed* is put back like one it only wrote.
- **The HEAD guard's text** (`HeadGuard::of`, both the in-turn guard and recovery's `git_violation`): "— check out <branch> again first" became "— check
  out <branch> again; `toryo resume <id> --note "…"` then undoes the commits made since <anchor> into the tree and checks them".
- **Does not match the list (recorded in Open):** the list says the owner runs "a plain `resume`" after the HEAD guard's escalation. A plain `resume`
  does not leave a `policy_violation` escalation (only machine-caused classes and a restored protected file go back into the stage): the task stays
  `Escalated`. The hole is reached with `resume --note`, which is what the tests use and what the new text names.
- **Earlier assertions changed** (the text of Do-4): `regress_recovery::f4_recovery_never_resets_or_commits_on_the_branch_the_agent_switched_to`,
  `::f4_recovery_escalates_an_unrepairable_git_state_with_nothing_reset` (the "detached" case) and the unit test
  `the_head_guard_resets_only_an_agent_commit_on_the_task_branch` asserted the exact old sentence. **AC-5:** no `regress_engine::ec31_*` test asserted
  "HEAD moved … not reset" (they cover writes; the unit tests of `read_only_write` only pin the reason text, unchanged), so none changed.
  **AC-4 (added assertion, not a changed one):** `engine_e2e::gates_persist_and_the_task_completes` now also asserts that `fix-1/1` began at the checkpoint
  of `implement/1`; the F8 and `checkpoint:after_commit` tests pass unchanged.
- **Consequence to know:** a commit the *owner* makes on the task branch between two stages is treated the same way — undone into the tree and checked.


### M13-02 — A fresh checkpoint runs the full path policy; a refused found commit does not loop (X25)

- **Fresh path of `checkpoint()`:** after the anchor test of M13-01 (first, so that commits undone into the tree are part of what is checked — the list
  has the two in the other order; this one is the stricter), every changed path is asked `Protected::violates` (protected, denied, the write fence) —
  the function `foreign_commit` uses on a found commit — not `is_denied` alone. A violation: `Escalated` (`policy_violation`), "… would be committed —
  the path policy forbids it; nothing was committed", and the way out (`resume --note`: the next turn's checks put the paths back).
- **"It is not HEAD":** when a found commit is refused, the branch is soft-reset to the attempt's `git_head_before` whenever HEAD is on the task branch,
  contains that commit and no operation is unfinished — also when the found commit lies under further commits ("it and the commits on top of it … were
  undone"). Otherwise nothing is reset and the reason gives HEAD and the two commands to put the branch back by hand.
- **Failing tests first** (`tests/regress_recovery.rs`): `m13_02_a_fresh_checkpoint_refuses_what_the_fence_forbids` (a role `never: ZZZ` fence; `Checkpointing`
  entered again after a crash with the fenced file staged — **failed before: the file was committed**) and
  `m13_02_a_found_commit_under_another_commit_is_undone_not_refused_forever` (AC-2 — **failed before: nothing was reset**).
- **AC-1 does not fail at `685aa45` the way the list says (Open):** `m13_02_a_forged_commit_from_the_worktree_index_never_reaches_a_checkpoint` builds the
  list's scenario (a forged commit from the worktree's own index, a `never` fence) and **passed before the fix**: the refusal escalates `policy_violation`,
  and a plain `resume` does not leave that class — it never "enters Checkpointing" again; the only way on is `resume --note`, whose turn checks restore
  the staged file. The test stays as a guard (it also asserts that a plain `resume` leaves the task `Escalated` and commits nothing). The fresh-path hole
  is real through another door — `Checkpointing` entered again after a crash (the first test) or after `protected_config_changed` is restored.
- **AC-3:** the M12-01 tests pass unchanged. No earlier assertion changed (no test pinned the old "denied paths … (policy.deny_paths)" sentence).


### M13-03 — The redactor: a line break may only be the last byte of a secret

- **Failing test first:** `process::redact::tests::a_secret_ending_in_two_line_breaks_is_not_released_in_halves` — secret `ab\n\n`, chunks `zab\n` then `\n`:
  the output was `zab\n\n`, the secret whole and in clear (AC-1).
- **Fix (`Redactions::new`):** `multiline = p[..p.len() - 1].contains(&b'\n')` — exactly one trailing byte is set aside, so `ab\n\n` is multi-line and keeps
  the `max_len − 1` hold-back; `ab\n` is still a line end (`retained() == 0` after a full line). A secret that holds only line breaks is dropped from the
  patterns with one `note:` (as a pattern it would redact every line end).
- **AC-2:** the M12-04 tests, the split tests and the fuzz test pass unchanged.


### M13-04 — Gate output is cut on line boundaries; the log path the vendor sees is relative (X27)

- **Failing test first:** `m12_gates::m13_04_a_secret_at_the_cut_leaves_no_fragment` (AC-1: 1 MB − 10 bytes of `x`, the secret and a line break, 9 MB of `y`,
  through `LocalRunner` with the secret in its redactions) — before: `SECRET-V…` was left in `output` and in the log.
- **(a) The cut** (`BoundedOutput::into_bytes_cut(hold)`): when bytes were dropped, the head ends at its last line break and the tail starts after its first;
  what that drops counts as omitted; the marker line is unchanged (preceded and followed by a line break). **A side with no line break at all** — one line
  longer than the bound, which is exactly the case of AC-1's head and of the M12-16 bound test on Windows (a first line of 1 MB) — cannot be cut on a
  line: it loses `hold` bytes at the cut instead, `hold` = the longest secret − 1 (`Redactions::max_len()`, new accessor), the most a fragment can be.
  Without secrets nothing extra is dropped, so the M12-16 bound test and `bounded_output_keeps_the_head_and_the_tail` pass unchanged (AC-2). In that
  no-line-break case a multi-byte character can still be torn at the cut (recorded in Open). Unit test `bounded_output_is_cut_on_line_boundaries`.
  **Replaced by the review (see "Review of M13-01…06"):** the raw `hold` cut could split a whole secret; the cut is now `into_sides` + `bounded_text`.
- **(b) `"log"` in `VERIFY_RESULT.json`** is `gate-logs/<file>` — relative to the run store (`runner::gate_log_rel`, used for check gates, environment gates
  and `tool: xpp` entries). The console line `full output:` keeps the absolute path.
- **Earlier assertions changed (the M12-16 tests read `"log"` as an absolute path):** `m12_gates::m12_16_a_silent_gate_gets_a_heartbeat_line` and
  `m11_env_gates::m12_16_the_gate_log_is_redacted` now join it to the run store and assert the `gate-logs/` prefix.
- **AC-3:** the heartbeat test also asserts that `VERIFY_RESULT.json` names nothing under the data directory. Not asserted on the *rendered context*: the
  reviewer's context names its pack files by their run-store path on purpose (it reads them there), so an absolute run-store path is in every context.


### M13-05 — The deploy confirmation is a database row bound to gate, commit and digest (X26)

- **Failing tests first** (`tests/m11_env_gates.rs`): `m13_05_a_confirmation_file_in_the_run_store_confirms_nothing` (AC-1 — before: a hand-written
  `confirmations.json` naming the gate let the deploy run unasked) and `m13_05_a_reload_that_changes_the_configuration_asks_again` (AC-2 — before: after a
  reload that changed the gate's `args` the next checkpoint was deployed without a question).
- **The table** `deploy_confirmations (task, gate, commit_sha, config_sha256, mode, asked_at, confirmed_at)`, primary key the first four; `confirmed_at`
  empty = asked, no yes yet (this is the "pending": no file, no second table). `store::ask_deploys` (the stop: replaces what an earlier stop asked and got
  no yes for), `store::confirm_deploys` (the yes), `store::deploy_confirmed` (`first`: a yes for this task + gate + **the current frozen digest**;
  `always`: also this commit), `store::deploy_confirmed_under_another_config` (the stop then says "asked again: the task's configuration changed since your
  last yes").
- **The yes is one transaction with the resume:** `engine_cli::resume_before_drive` calls `confirm_deploys` and the `ResumeStage` transition in the same
  `store.call`.
- **Schema version 2 — the first raise ever.** `SCHEMA` gained one `CREATE TABLE IF NOT EXISTS`; `Store::open` accepts a database of version 1 and writes
  2 into `meta` (nothing is altered or dropped); a higher version is refused ("a newer toryo wrote it"). **Consequence:** once this binary has opened the
  owner's database, a pre-M13 `toryo.exe` refuses it ("has schema version 2; this toryo expects 1") — keep one binary.
- **`confirmations.json` handling is deleted** (`gates::Confirmations`, `is_confirmed`, `set_pending`, `record_confirmation`). **No migration:** M12 was merged
  on 2026-10-05 and no owner task has asked for a confirmation yet (the first real D365FO task has not run); a task that did would simply be asked once more.
- **Do-3** (a failing `set_pending` left the task `Verifying`): there is no file write left; the question is a database write like every state change.
  A failing database stops every command the same way and the verify attempt is re-run by the next `resume` — no separate machine class was added.
- **Earlier assertion expected to change:** none had to. The M12-07 tests (`resume` is the yes, `--yes` never confirms, `first` once per task, `always` once
  per checkpoint) pass unchanged (AC-3); no test pinned the schema version or read the file.
- **Unit test (AC-4):** `store::tests::an_older_database_opens_and_is_raised` (version 1 → 2 with its task kept; the functions above; version 3 refused).
- **Threat model (goes to STATUS N-11):** an agent started with the owner's token can write the database as well as the run store; the row is the same
  trust level as every other record there, not a new boundary.


### M13-06 — Read-only turns: the checks also run when the turn errors; the copy is safe

- **Failing tests first** (`tests/regress_engine.rs`; the new crash point `read_only:after_turn` lets the engine die right after a Planner/Reviewer turn; both
  tests were also run against the old `recovery.rs` and failed there): `m13_06_recovery_puts_back_only_what_the_read_only_role_wrote` (AC-1, audit O-11 —
  before: the reason named the owner's `owner-notes.txt` next to the reviewer's file and nothing was put back) and
  `m13_06_a_link_left_by_the_read_only_role_is_removed_not_followed` (AC-3 — before: Git listed `peek/precious.txt` behind the junction as a changed path).
- **The `before` state is kept:** `read_only_turn` stores the `TreeState` it read before the turn as an artifact of the attempt (`tree-before/<stage>-<attempt>`,
  kind `tree_before`, hashed and registered like the context file; one version per turn). `TreeState` is `Serialize`/`Deserialize` and `pub(super)`.
- **One function for both ways:** `runner::undo_read_only_write(wt, task, attempt, before)` — the soft reset of M13-01, the put-back, the copies, the reason.
  `read_only_checks` calls it after a turn; recovery's `Planning | Reviewing` branch calls it with the saved state (`runner::tree_before`), so it escalates
  only on what the role changed and leaves the owner's files alone. **No saved state** (an attempt from before M13, or one that died before its first turn):
  today's behaviour — any dirty file escalates.
- **A turn that ends in an error** (`hub.turn_with` → `Err`, or `usage()` → `Err`): the undo runs before the error goes up; when the role wrote something the
  attempt ends `Failed` (`policy_violation`) and the task escalates, then the error is returned as before. **Not tested** — no fake step makes `turn_with`
  return `Err` (Open).
- **Do-2:** a path that was already changed before the turn and is changed again is copied and **not** put back; the reason ends "; not put back: [...] —
  changed already before the turn, so Toryo cannot tell your edit from the role's (what is there now is copied to <dir>)".
- **Do-3 — the copy** (`keep_copy`): `symlink_metadata`; a link is never followed or copied; only regular files; at most 8 MB per file; what was not copied is
  named in the reason ("no copy kept of: <path> (<why>)").
- **Links (AC-3), found while writing the test:** Git lists the files *behind* a junction like any folder's, so the put-back would have deleted files outside
  the worktree through it. A path with a link or junction on the way is never opened: a link that was not there before the turn is removed as a link
  ("removed as links, not followed: [...]"), one that was is left alone and named. (`Worktree::restore`, used for the implementer's path-policy
  violations, has the same exposure for a junction an implementer makes — not in this task's scope: Open.)
- **Risk to know (as the list asks for it):** what the owner changes in the worktree *between* the crash and the `resume` cannot be told from the role's
  write either — it is put back like one, after a copy to `<run store>/read-only-writes/<attempt>/`.
- **Earlier assertion changed:** `regress_engine::ec31_a_reviewer_that_changes_a_file_left_before_its_turn_escalates` compared the whole reason; it now asserts
  the old sentence as the prefix, the "not put back" part and the copy (this test is AC-2).
- **Unit test:** `the_copy_of_a_read_only_write_is_capped_and_never_follows_a_link`.
- **Three things the review of the diff found, fixed before this commit:**
  (1) a role that deletes a tracked folder and leaves a link of that name: the tracked files stayed deleted once the link was gone — the tree is now
  read again after a link is removed (`m13_06_a_tracked_folder_replaced_by_a_link_is_put_back`, failed first);
  (2) `Worktree::restore` does not say when Git refuses (a stale `index.lock`): console and reason said "put back" while the edit was still there —
  the tree is looked at after the put-back and the reason ends "; could NOT be put back: [...]"
  (`m13_06_a_put_back_that_git_refuses_is_named_in_the_reason`, failed first);
  (3) recovery closed the read-only attempt before it ran the undo — an error in the undo would have left nothing `Running` for the next `resume` to
  check. The undo now runs first. **No test:** nothing was found that makes the undo return an error (a broken `.git` fails earlier in recovery; an
  `index.lock` is case 2).


### Review of M13-01…06 (after M13-06, as the list asks)

Every acceptance criterion was checked against the code and the tests, and one subagent read the diff in place (no worktree, no build). The criteria
are met with the deviations already written under each task. What the review found inside the scope of the list was fixed, each with a test that
failed first; the rest is under "Open".

- **M13-01 — `resume --keep-partial` committed on an unchecked commit.** When the engine dies in an implementer turn that committed and left a merge
  unfinished, recovery does not reset (no reset during an unfinished operation) and escalates with HEAD at the agent's commit. `keep_partial` then
  checked the policy on tree-against-HEAD only, committed the recovery checkpoint **on** that commit, and the checkpoint became the anchor.
  Now `keep_partial` starts at the commit the attempt began at: a commit on top of it on the task branch is soft-reset into the tree first (its
  changes meet the policy check that follows); a HEAD anywhere else is refused. Test: `regress_recovery::m13_01_keep_partial_never_commits_on_a_commit_the_attempt_left`
  (before: "the denied file is in a commit").
- **M13-04 — the cut made a fragment out of a whole secret.** The first version dropped "longest secret − 1" **raw** bytes at the cut of a side
  without a line break: a whole secret lying there was cut in two; and a secret of several lines that straddled a line-boundary cut kept its first
  lines. Now `BoundedOutput::into_sides` only cuts on line boundaries (a side with no line break is kept whole, less a torn UTF-8 character), and
  `runner::bounded_text` decodes, strips and **redacts each side first**, then takes "longest secret + 2" bytes at the cut edge of each side (and
  the rest of that line): a whole secret is already `[REDACTED:…]` by then, a straddling one has less than its length there. With no secret
  configured nothing more is dropped. Unit test `runner::tests::a_secret_next_to_the_cut_is_whole_or_gone` (three positions; before: "a fragment is
  left: SECRET-V"). `bounded_output_is_cut_on_line_boundaries` (this milestone's own test) now covers the torn character instead of the `hold` bytes.
- **M13-05 — "the task's configuration changed since your last yes" was said for good.** After one reload, every later stop of an `always` gate said
  it. `store::deploy_confirmed_under_another_config` now also needs "no yes under the configuration in force". Unit test
  `store::tests::the_changed_configuration_note_ends_with_a_yes_under_the_new_one`.
- **M13-01/-02 — the two "by hand" texts:** the path after `git -C` is quoted (a worktree path with a space), and both say that an unfinished Git
  operation is finished or aborted first (`reset --soft` fails in the middle of a merge). No assertion pinned these texts.
- **M13-06:** three findings, fixed inside the M13-06 commit (see there).


## Open

What does not match the list, was not fixed, or is left for a later milestone. (Collected from the task paragraphs above; M13-15 carries the
items that are plans into `docs/STATUS.md`.)

**Where the code or the test differs from the list**

1. **M13-01 AC-1, M13-02 AC-2 use `resume --note`, not a plain `resume`.** A plain `resume` does not leave a `policy_violation` escalation (only the
   machine-caused classes and a restored protected change do): the HEAD-guard text now names `--note`, and the tests follow the text.
2. **M13-02 AC-1 does not fail before the fix.** A commit forged from the worktree index never reached a checkpoint (a plain `resume` does not re-enter
   Checkpointing). The test is kept as a guard; the fresh-path hole has its own failing test (`m13_02_a_fresh_checkpoint_refuses_what_the_fence_forbids`).
3. **M13-02:** the fresh checkpoint runs the anchor test before `Protected::violates` (stricter order than the list writes; both run).
4. **M13-04 AC-3** is asserted on `VERIFY_RESULT.json`, not on the rendered context (which names its pack files by their run-store path on purpose).
5. **M13-05:** no earlier assertion had to change (the list expected one). Do-3: no separate machine class for a failing write — the question is a
   database write like every state change. A pre-M13 `toryo.exe` refuses a database this binary has opened (schema version 2).
6. **M13-06 Do-1:** the before-state is saved before every read-only **turn** as an artifact (`tree-before/<stage>-<attempt>`), not as
   `attempts/<id>/tree-before.json`; an attempt that died before its first turn has none and keeps the old "any dirty file escalates".

**Not tested (no way found to provoke it in a test)**

7. M13-06: the `Err` from `hub.turn_with` / `usage()` path of `read_only_turn`; recovery's "undo before the attempt is closed" order.
8. M13-01/-02: the branches of `start_at_anchor` for "another branch" and "an unfinished operation", and the found-commit branch "cannot be reset —
   do it by hand" (the "does not descend" branch and the reset branches are tested).

**Known holes, not in this list's scope (→ `docs/STATUS.md`)**

9. **`VERIFY_RESULT.json` still carries `verify_checkout` as an absolute path** under `verify_root` (default `%LOCALAPPDATA%\toryo-verify`: the
   Windows user name). M13-04 asked for `"log"` only.
10. **HEAD equal to the anchor but detached** (or in an unfinished operation) is not looked at by `start_at_anchor`: an implementer turn is stopped by
    the HEAD guard after the turn, but a fresh checkpoint would commit off the task branch. Needs the owner to detach HEAD by hand between stages.
11. **Recovery's early returns** (a protected change, or a missing/tampered artifact, found first) escalate without the read-only put-back.
12. **"Was the link there before the turn"** is judged by "was a path below it already changed before the turn". An owner's link with only clean
    content behind it, into which the role writes, is removed (the link only — named in the reason) and the role's file stays in the target.
    `TreeState::read` hashes (reads) the files Git lists behind a junction.
13. **`Worktree::restore` passes paths to Git as pathspecs** (a file name with `[`…`]` is a pattern), and for an **implementer's** junction it has the
    exposure M13-06 closed for read-only roles. `restore` is also used for the path policy of writing turns: not touched here.
14. **What the owner changes in the worktree between a crash and the `resume`** is taken for the read-only role's write (put back, a copy kept).
15. **The anchor uses the order of `started_at`** (as every "last attempt" query does): a clock set back between attempts would pick an older commit.
16. **A cp932 character torn by the 1 MB bound in a line with no line break** is still decoded as a wrong character at the cut (UTF-8 is handled).

**Tests that depend on the speed of the machine**

17. `m11_env_gates::a_timed_out_environment_gate_is_recorded_as_interrupted` (a 6 s gate timeout that must hold two `pwsh` start-ups) and
    `m12_06_an_environment_gate_that_times_out_stops_for_the_owner` (3 s) failed in single runs on this machine while it was slow (the whole file took
    120–137 s instead of 70 s) — the first one also at the commit of M13-05, without the change under test. Both pass when run again. Not changed.

