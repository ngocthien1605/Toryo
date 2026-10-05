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
- [x] M13-07 `task cleanup` that cannot look fails closed; `doctor` never loops on a link
- [x] M13-08 The gate resolver is one resolver; the timeout stop names every gate that re-runs
- [x] M13-09 `environment_gate_confirm` says what to type, and how to say no
- [x] M13-10 `doctor` looks at long paths where the first run will hit them
- [x] M13-11 `task create` is all or nothing (X28)
- [x] M13-12 Ctrl+C on `toryo run` is a stop request (X29)
- [x] M13-13 A launch error before the agent starts does not reserve a budget
- [x] M13-14 Tests the review asked for
- [x] M13-15 Notes, guide, `STATUS.md`
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


### M13-07 — `task cleanup` that cannot look fails closed; `doctor` never loops on a link

- **Failing tests first:** `m12_cli::m13_07_cleanup_that_cannot_look_refuses_without_force` (AC-1 — before: the cleanup went on, cancelled the task and
  only stopped at `git worktree remove`, which cannot remove a worktree with a broken `.git` file either);
  `m12_config::m13_07_doctor_does_not_follow_a_link_under_the_toryo_folder` (AC-2 — before: "64 text file(s)" for one file: the walk went through the
  junction until the path was too long for Windows; it did end, but only there);
  `m12_config::m13_07_doctor_warns_about_a_parenthesised_variable_in_a_profile_env` (AC-3 — before: no line).
- **(a)** `cleanup_would_lose` returns `Result`: when Git cannot say what the worktree holds, `task cleanup` stops with "cannot tell what the worktree
  holds (<error>) — pass `--force` to delete it anyway" before anything is changed. **Added so that the message is true:** with `--force`, when
  `git worktree remove --force` fails, the folder is removed directly and `git worktree prune` forgets it (the test asserts both). Without `--force`
  a failing `git worktree remove` is still the error it was.
- **(b)** The walk of `encoding_checks` asks `DirEntry::file_type()` (what the entry is, not what it points at): a folder is walked, a link or junction
  to a folder gets one `info` line ("is a link to a folder: not followed") and is skipped. A link to a file is read as before.
- **(c)** `config::VAR_PATTERN` — one pattern for `expand_vars`, `residual_var` and `doctor`'s profile-`env:` warning.
- **Windows-only parts:** the junction (`mklink /J`) in AC-2; on Unix the same test uses a symlink.


### M13-08 — The gate resolver is one resolver; the timeout stop names every gate that re-runs

- **`gates::env_gate_problems` is deleted** (no caller). `runner::env_gate_program` — the one resolver `doctor` and the Verify preflight use for an
  environment gate's program — now asks `gates::startable`, as a check gate's `program_problem` does: a file `is_file()` cannot see through is not
  "not found". **Failing test first** (Windows-only, skipped when the account may not create a file link):
  `team::tests::a_gate_program_is_file_cannot_see_through_passes_the_preflight` — a file link with a missing target stands in for the App Execution
  Alias (before: `Err("program `alias-r8` not found on PATH")`). The folders searched are unchanged (PATH only, as `checks::which_in`).
- **AC-1** `m11_env_gates::m13_08_an_environment_gate_program_that_cannot_start_is_a_machine_stop` — a text file as `program`: the preflight passes and
  the start stops with `environment_gate_unavailable` ("could not be started"), no implementer turn, deployment row `ok = false`. **It passed before
  the change** (a text file *is* a file): kept as the test of the spawn-error branch (item 2 of M13-14).
- **AC-2** `gates::tests::only_the_first_file_argument_is_the_script` — `script_problems` stops after the value of the first `-File` / `-f`: what
  follows are the script's own parameters. (The list's example `-f missing.txt` is relative and was never asked; the test uses an absolute path,
  which was refused.)
- **AC-3** `m13_08_a_gate_killed_by_its_timeout_after_writing_into_the_export_is_a_timeout` — `timed_out` is asked before `export_changed` (before:
  `verify_mutated_tree`).
- **AC-4** `m13_08_a_timeout_names_every_environment_gate_that_runs_again` — with gates that finished earlier in the same Verify run, the reason says
  "`toryo resume <id>` runs Verify and every environment gate again: `seed` (finished — the deployment record says so — and deploys again), then
  `dbsync`"; with one gate the text is as before. **"the deployment rows say so" was read as:** the reason says what the rows say (the earlier
  gate's row says it passed, the timed-out gate's says interrupted). The rows themselves and `status` are unchanged — Open.
- **The xpp text:** a project of a `tool: xpp` command that runs out of time says the limit it was given — "timed out after 734 s (what the projects
  before it left of the command's 30 min)", or "was not started: the projects before it used up the command's 30 min"; the first project still says
  "timed out after 30 min". Unit test `a_timeout_says_what_the_run_was_given_and_what_runs_again` (no MSBuild on this machine: not run end to end).


### M13-09 — `environment_gate_confirm` says what to type, and how to say no

- **`next_action`** has its own arm for the class (before the generic machine-caused text, which said "fix the cause" where there is none):
  "→ look at the diff (`git -C "<worktree>" show --stat <commit>`); yes: `toryo resume <id>`; no: `toryo resume <id> --note "…"` (one fix cycle) or
  `toryo task cleanup <id>`". `<commit>` is the checkpoint the question is about (the short SHA).
- **Earlier assertion changed (the one the list names):** `runner::tests::next_action_of_the_five_machine_caused_classes` looped over every class of
  `MACHINE_CAUSED` with the generic text; `environment_gate_confirm` is taken out of the loop and gets its own `assert_eq!` on the new text (AC-1).
  It failed first with the old text on the left.
- **`doctor`** prints `(confirm: …)` on every environment-gate line, also on a FAIL, and says what happens: "`task create` refuses it (the script is not
  there), and a run would stop …" only when a script is missing; for a missing program "`task create` admits the task, and a run would stop with
  `environment_gate_unavailable` before any gate ran". Test `m11_env_gates::m13_09_doctor_says_confirm_on_every_environment_gate_line` (AC-2; before:
  no `confirm:` on the FAIL line, and "refuses it" for a missing program).
- **Texts (AC-3, `m13_09_the_readme_and_the_guide_say_which_class_and_how_to_say_no` greps them):** README "Cannot start" names `gate_unavailable` for a
  check gate's program and `environment_gate_unavailable` for an environment gate's program, script or secret (and says that only an *environment*
  gate's timeout stops the task); README and HUONG-DAN (§9.1 row and the `confirm: first` paragraph) say how to say no; the HUONG-DAN row of
  `gate_unavailable` no longer claims that a check gate's `-File` script is checked (it is the repository's: never asked).


### M13-10 — `doctor` looks at long paths where the first run will hit them

- **Two new lines in the `paths` area, on Windows only** (`doctor::long_path_checks`; nothing on other systems):
  `core.longpaths` as a plain Git sees it in `project.root` (`git -C <root> config --type=bool --get core.longpaths`: system, global and repository
  configuration — what the agents' own Git reads; the task worktrees share the repository's configuration) — OK, or WARN with
  `git -C "<root>" config core.longpaths true`; and the machine's `LongPathsEnabled`
  (`reg query HKLM\SYSTEM\CurrentControlSet\Control\FileSystem /v LongPathsEnabled`, `reg.exe` taken from `%SystemRoot%\System32`) — OK when 1, else
  WARN with the `reg add` line and "needs an administrator once; Toryo itself does not need it".
- **Read-only:** two child processes that read; the test asserts that `core.longpaths` is the same before and after `doctor`.
- **No new dependency and no new `windows-sys` feature** (the registry is read through `reg.exe`, as the instructions allow).
- **Windows-only tests (each failed first: "no line with …"):** `m12_config::m13_10_doctor_reads_core_longpaths_of_the_repository` (sets the key in a
  temporary repository: `false` → WARN, `true` → OK) and `m12_config::m13_10_doctor_reads_long_paths_enabled` (the line exists and agrees with what
  `reg query` returns on the machine — here 0: WARN). On every system: unit `doctor::tests::the_long_path_lines_say_what_was_read_and_what_to_type`.
- **To record (the list asks for it):** the agents' own Git and the build tools (MSBuild, PowerShell) are **not** long-path safe by anything Toryo
  does — M12-13 covers Toryo's own Git calls only, and this task only warns. With `worktrees_root` and `verify_root` kept short (the existing line
  of M12-13) D365FO metadata paths stay under 260 characters; the two new lines say what to set when they cannot.
- **A mistake of mine while checking this task, for the owner to know:** I ran the built `toryo doctor` once from the repository folder without
  `--config`; it used `$TORYO_CONFIG` (`C:\.thien\m0\toryo.yaml`) and so probed the owner's data folder and verify root (a probe file and a probe
  folder, created and removed — `doctor` does that on every run; `toryo.db` was not opened or changed). Nothing else was touched.


### M13-11 — `task create` is all or nothing (X28)

- **Failing tests first** (`tests/m12_cli.rs`): `m13_11_a_create_that_fails_at_the_worktree_leaves_nothing` (AC-1 — a **file** where
  `project.worktrees_root` should be: before, the run store stayed and the retry was refused) and
  `m13_11_a_create_that_fails_after_the_worktree_removes_branch_and_worktree` (the step after `git worktree add` fails — Git's `info/exclude` is a
  folder: before, branch, worktree and run store stayed). Both then create the same `TASK.md` again.
  **AC-1 as the list writes it** ("a file where the worktree folder should be", that is `<worktrees_root>\<id>`) is refused before anything is made
  ("worktree … already exists"), today and before: the test puts the file one level up, where the failure comes after the run store exists.
- **How:** `task::Made` — what the creation has made so far (run store, worktree, branch, database row). It is dropped at every early return; unless
  `keep()` was called (the last line of `create`), its `Drop` removes them in reverse order, best effort, one `removed: …` line each (a `WARNING:`
  line for what could not be removed). The error that ended the creation is the one printed. This shape leaves the body of `create` where it is.
  - the row: `store::remove_created_task` (the task's row, its first artifacts and its events — nothing else refers to a task that never ran);
  - the worktree: `git worktree remove --force`, else the folder itself, then `git worktree prune`;
  - the branch `toryo/<id>`: it did not exist before this `task create` (that is checked first), so it is this creation's, at the base commit;
  - the run store: the folder.
  The line `/.toryo-tmp/` in Git's `info/exclude` and an empty `worktrees_root` are left (shared, harmless).
- **The order of the refusals** was checked, nothing had to move: `TASK.md` (encoding, acceptance ids, the id), roles, the verify floor and probe, the
  environment gates, the X++ toolchain, the base branch, the entry scripts, "already exists" (row, run store, worktree, branch) all come before the
  first thing is made.
- **Two errors got their place named** (they were bare OS errors): "creating project.worktrees_root <path>" and "writing <…/info/exclude>".
- **Not tested end to end:** a failure after the row exists (the first artifacts, the protected baseline) — nothing cheap makes those fail. The
  removal of the row has the unit test `store::tests::a_task_that_was_never_finished_is_taken_back`.
- **AC-2:** every existing create test passes unchanged (full run).


### M13-12 — Ctrl+C on `toryo run` is a stop request (X29)

- **No new crate.** `engine::lock::catch_interrupt()` (called at the start of every drive: `run`, `resume`) installs a console handler
  (`SetConsoleCtrlHandler`) on Windows and a `SIGINT` handler elsewhere — the pattern `watch` already had. The **first** Ctrl+C (or Ctrl+Break) is
  counted and prints "stop requested — the running turn or gate is ended at the next boundary; press Ctrl+C again to end now". The **second** is not
  handled: on Windows the handler returns "not handled" and the system's default ends the process; on Unix the first press already put the default
  action back.
- **The same flag as `toryo stop`:** the stop watcher of the running stage (every 100 ms) and the check at every stage boundary turn a counted Ctrl+C
  into the row `toryo stop` writes (`store::insert_stop`, requested by "<owner> (Ctrl+C)") — once per owner — and from there nothing is new: the turn
  or gate is cancelled, the attempt ends `Interrupted`, the state stays (R38), `run_ended` is written, the toast is sent. A `resume` without an id
  that drives several tasks stops each of them at its first boundary.
- **Children:** agents and gates are started without a console (`CREATE_NO_WINDOW`) inside their Job — they never saw the key and still do not; they
  are ended by the stop, as after `toryo stop`.
- **Tests — with a real console event, also on Windows** (`tests/m12_cli.rs`; before the change both hung until the test killed the run):
  `m13_12_ctrl_c_on_run_is_a_stop_request` (AC-1: the note, "stop request handled", attempt `Interrupted`, state `Planning`, last event `run_ended`,
  then `resume` completes) and `m13_12_a_second_ctrl_c_ends_the_process_at_once` (AC-2: two presses → the process is gone, the attempt is still
  `Running`, no `run_ended`; the next `resume` recovers and completes).
  **How the key is pressed:** the test starts `toryo run` with a console of its own (`CREATE_NO_WINDOW`: the key goes to every process of a console,
  and the test runner must not get it) and with **`TORYO_TEST_INTERRUPT_AFTER_MS`** (tests only: `6000`, or `6000,6000` for two presses), which makes
  the process send itself the event after that time — `GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0)` on Windows, `raise(SIGINT)` on Unix. So the
  system delivers the event and the real handler runs. On Windows the hook first clears an inherited "ignore Ctrl+C" (a process started by one
  that ignores the key inherits that — `cargo test` under this session did; without it no event arrived).
- **Not done by hand:** this session has no interactive console; the two tests above are the evidence. Worth one manual press by the owner on the
  first real run.
- **Open:** a `toryo run` that was itself started with Ctrl+C ignored (`start /b`, some CI shells) does not see the key, as before — only the test
  hook clears that. The Unix halves (handler, `raise`) are written by reading (the `watch` handler next to it compiles on Linux and has the same
  shape); CI is their first build.


### M13-13 — A launch error before the agent starts does not reserve a budget

- **Failing test first:** `m12_cli::m13_13_a_launch_error_reserves_nothing_and_says_what_is_wrong` — real adapters (no `--fake-agents`, `--accept-unverified`), the
  fixture's planner profile (`exe: claude-not-installed`), `pipeline.budget_tokens: 100000`. Before: "planner turn failed: ProcessCrashed (starting
  claude-not-installed: … program not found) — the same turn again", then "admission cap reached: 200000 tokens used (0 spent + ~200000 reserved) of
  100000" → `Paused`. Now: `Escalated`, the reason holds "ProcessCrashed" and the program's name, `reserved_tokens = 0`, no usage row with a reservation.
  No program is started by this test: the profile names one that exists on no machine.
- **How:** `agents::exec::NotStarted` marks an error of `run_turn_with` that happened before a process existed — the profile's secrets
  (`profile_env`: a missing Credential Manager entry), the `exe` (`expand_vars`), the spawn itself. The CLI adapter then reports a **provisional zero**
  usage before the `TurnFailed` (as a preflight refusal already did, `CliSession::refuse`): a turn that never started holds no reservation. The class
  stays `ProcessCrashed` with the OS error in the detail; the retries of plan §11 still happen (each without a reservation), then the task escalates.
- **Not changed:** an error *after* the process started (waiting, the capture files) still has unknown usage and still reserves — that is R41.


### M13-14 — Tests the review asked for

Nine items, each its own test. "ok at once" = the test passed without a production change.

1. **Class constants ↔ `MACHINE_CAUSED`** — `state::tests::every_machine_caused_class_is_one_of_the_engine_constants`: every constant the engine escalates
   with is machine-caused, the list names nothing else, and five classes that must cost a fix cycle are not on it. Ok at once.
2. **The environment-gate spawn-error branch** — `m11_env_gates::m13_08_an_environment_gate_program_that_cannot_start_is_a_machine_stop` (written with
   M13-08, a text file as `program`). Ok at once.
3. **`task config --reload` refuses a missing environment-gate script** — `m11_env_gates::m13_14_a_reload_refuses_a_missing_environment_gate_script`. Ok at once.
4. **The frozen digest of a configuration without `base_branch`** — `config::tests::a_configuration_without_base_branch_keeps_its_pre_m12_digest`: the
   root template with its `base_branch: main` line removed hashes to `7e2e5219…`. Ok at once.
   **How the value was obtained (not as the list says):** it was not recomputed by running the code of `4ad97c3` — exporting that commit into a scratch
   folder to build it was refused by this session's permission layer, and no worktree may be made. The value is the one the source of `4ad97c3` itself
   pins for this template (`templates_keep_their_m10_hash`, computed with `main` @ `aa0be03`; that test was green at `4ad97c3`). Before M12 the key
   defaulted to `main`, so the template with the line and without it were one configuration with that digest. Open: the owner can confirm with one
   run of the old code if wanted.
5. **AC-2 of M12-01 with a role `never:` fence** — `regress_recovery::m13_14_a_forged_partial_commit_with_a_fenced_file_is_never_accepted`. Ok at once.
6. **`keys_without_effect`, the other direction** — `config::tests::every_key_of_the_repository_file_is_read_or_announced`: the last name of every path of
   a loaded repository configuration (`profiles` left out: the machine's) appears as a field access in a source file other than `config.rs`, or is on
   the no-effect list. Ok at once. (`RepoConfig` has no `Default`: the test loads the shipped template, which fills every default.)
7. **`TORYO_OEM_CODE_PAGE`** (tests only; read in `process::decode::oem_code_page` on Windows — **the production change the item asks for**) and
   `m12_gates::m13_14_a_cp932_line_of_a_gate_reaches_the_fix_prompt_as_text` (Windows-only; skipped when code page 932 is not installed): a check gate
   writes "エラー: 機能ソース" in cp932 and fails; the implementer's fix prompt and the gate log hold the text, no `�`. Failed before the variable was read
   (this machine's code page is not 932).
8. **Ids** — `m12_cli::m13_14_every_command_refuses_what_is_not_a_task_id`: `status`, `task show`, `task cleanup`, `approve` refuse an invalid id and a
   title id of 67 characters is refused — ok at once. **Finding:** `valid_task_id` took Windows device names (`CON`, `nul`, `COM1`, `con.task`, …) and an
   id ending in a dot. **Fixed** (the item names the behaviour): `valid_task_id` refuses both; the device-name test of the lock names is now
   `lock::is_device_name`, used by both. An existing task with such an id (none can have been created on Windows) would be refused by every command.
9. **M12-14, the stderr tail without a detail** — `exec::tests::the_stderr_tail_is_the_detail_when_there_is_none`: the tail was attached, but after a
   stray "; " when the turn had no detail of its own; `exec::with_stderr` makes the tail the detail then (the one line the review called free).


### M13-15 — Notes, guide, `STATUS.md`

- **This file:** a paragraph per task, the review after M13-06, the "Open" list and the summary at the end.
- **`docs/STATUS.md`:** the test counts (683 at the end of M12 — the file said 681 —, 732 now, 705 on Linux at the end of M12); audit rows O-11,
  O-14, O-15, O-29 are closed and deleted in place; N-04, N-05, N-08 lost the parts M13 fixed; new rows N-11 (the threat model of the run store
  and the database, as the list words it), N-12 … N-16 (what the review and the later tasks left open); O-35, O-39 (the new test variables), O-56
  (two tests that depend on the machine's speed) updated; §2 says what has still never run.
- **`docs/DECISIONS.md`:** X24–X29.
- **README / HUONG-DAN:** `./toryo.yaml` is the folder you run `toryo` in; the gate log is the bounded text, not the whole output; what `first` and
  `always` mean, and that a reload that changes the configuration asks again; Ctrl+C; the two new `paths` lines of `doctor`; a prose line that
  starts with an id is refused (guide §7.3). How to say no to a deploy and the corrected `gate_unavailable` texts came with M13-09.
- **One text in code** (the list puts it here): the refusal of M12-11 said "a table row or a sentence that only mentions an id does not count" — but
  a table row and a prose line that *starts* with an id are refused. It now says so. The existing tests of that refusal pass unchanged.


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

**From M13-07 … M13-14**

18. **M13-08, "the deployment rows say so":** read as "the reason says what the rows say". `status` still shows the first gate's row as passed, with
    no word that a `resume` deploys it again (STATUS N-05). **AC-1** (a text file as an environment gate's `program`) passed before the change; the
    failing test for the resolver is the Windows-only file-link test.
19. **M13-11 AC-1** as written (a file at `<worktrees_root>\<id>`) is refused before anything is made, before and after; the test puts the file at
    `worktrees_root`. A failure **after** the database row exists is covered by a unit test of the removal only.
20. **M13-12:** tested with a console event the process sends itself (`TORYO_TEST_INTERRUPT_AFTER_MS`), not with a key pressed by a hand; a
    `toryo run` started with Ctrl+C ignored does not see the key (as before); the Unix halves were written by reading (STATUS N-16).
21. **M13-14 item 4:** the pinned digest is the value the source of `4ad97c3` pins for the template, not one recomputed by running that code
    (exporting the commit was refused by the session's permission layer; no worktree may be made).
22. **M13-14 item 8 changed production code** (`valid_task_id` refuses Windows device names and a trailing dot): the item names that behaviour and the
    test failed without it.
23. **M13-10, my mistake:** one run of the built `toryo doctor` without `--config` used the owner's `$TORYO_CONFIG` and probed the owner's data folder
    and verify root (a probe file and folder, made and removed; `toryo.db` was not opened). Nothing else of the owner's was touched in this milestone.
24. **M13-15:** the refusal text of M12-11 (`checks::parse_acceptance_ids`, not M18 code) was reworded in this documentation commit, as the list asks;
    the escalation table already named a task-pack file under `context_unreadable` (nothing to add).

## Summary

- **Tests on Windows:** 683 passed / 0 failed / 6 ignored before M13 → **732 passed / 0 failed / 6 ignored** after (49 new tests; the full suite ran
  green before every commit: 687, 690, 691, 693, 696, 701 (M13-06), 704 (review), 707, 713, 715, 718, 721, 723, 724, 732).
  **Linux:** 705 at the end of M12; M13's count is the one `test (ubuntu-latest)` prints on the pull request (the session cannot build for Linux).
- **Assertions from before M13 that changed** (each named in its commit): the text of two F4 tests and
  `the_head_guard_resets_only_an_agent_commit_on_the_task_branch` (M13-01: the reason now names `resume --note`);
  `m12_16_a_silent_gate_gets_a_heartbeat_line` and `m12_16_the_gate_log_is_redacted` (M13-04: `"log"` is relative);
  `ec31_a_reviewer_that_changes_a_file_left_before_its_turn_escalates` (M13-06: the reason goes on with "not put back");
  `next_action_of_the_five_machine_caused_classes` (M13-09: the class has its own text). None was deleted or skipped.
- **Windows-only tests (all pass here; not compiled on Linux):**
  `team::tests::a_gate_program_is_file_cannot_see_through_passes_the_preflight` (M13-08; skips itself when the account may not create a file link),
  `m12_config::m13_10_doctor_reads_core_longpaths_of_the_repository`, `m12_config::m13_10_doctor_reads_long_paths_enabled` (M13-10),
  `m12_gates::m13_14_a_cp932_line_of_a_gate_reaches_the_fix_prompt_as_text` (M13-14; skips itself without code page 932).
  **Tests with a Windows half and a Unix half** (junction / symlink, console event / signal): `regress_engine::m13_06_a_link_left_by_the_read_only_role_is_removed_not_followed`,
  `regress_engine::m13_06_a_tracked_folder_replaced_by_a_link_is_put_back`, `runner::review_tests::the_copy_of_a_read_only_write_is_capped_and_never_follows_a_link`,
  `m12_config::m13_07_doctor_does_not_follow_a_link_under_the_toryo_folder`, `m12_cli::m13_12_*` (two). Their Unix halves, and the `SIGINT` handler
  of M13-12, are first built by CI.
- **New test-only variables:** `TORYO_OEM_CODE_PAGE`, `TORYO_TEST_INTERRUPT_AFTER_MS`; new crash point `read_only:after_turn` (STATUS O-39).
- **No new crate, no new `windows-sys` feature.** `Cargo.lock` is unchanged.
- **What changes for an existing configuration:** (1) M13-05 — the deploy confirmation is a table of the database (schema version 2: an older
  `toryo.exe` refuses the database afterwards); a `confirmations.json` left by M12 confirms nothing, the owner is simply asked once more.
  (2) M13-11 — a failed `task create` removes what it made. (3) M13-12 — Ctrl+C on `toryo run` stops the task cleanly; twice ends the process.
  Also: `doctor` has two more `paths` lines on Windows (WARN until `core.longpaths` / `LongPathsEnabled` are set), and a task id that is a Windows
  device name is refused.

