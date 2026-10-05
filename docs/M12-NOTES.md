# M12 notes — ready for the first real task (`M12_task.md`)

Branch `m12/first-run` from `main` @ `4ad97c3`. One commit per task. Baseline on Windows: **623 passed, 0 failed,
6 ignored**. The task list is `C:\.thien\AgentOrchestrator\M12_task.md` (outside the repository); the audit it rests on is
`toryo-audit-2026-10-05.md`.

## Progress

- [x] M12-01 Recovery never accepts a commit Toryo did not make (H1)
- [x] M12-02 The protected scan fails closed (H4)
- [x] M12-03 Read-only role checks run on every way a turn ends (H6)
- [x] M12-04 The redactor releases complete lines (H5)
- [ ] Review of M12-01…04 (subagent, in place)
- [x] M12-05 A gate that cannot start is a machine stop, and `doctor` finds it first
- [ ] M12-06 An environment gate that times out stops for the owner (X18)
- [ ] M12-07 Confirm before deploy (X17)
- [ ] Review of M12-05…07 (subagent, in place)
- [ ] M12-08 `base_branch` is honoured; keys without effect say so (X19, X20)
- [ ] M12-09 `./toryo.yaml` before `$TORYO_CONFIG` (X21)
- [ ] M12-10 Text files: BOM ignored, non-UTF-8 refused by name (X22)
- [ ] M12-11 Acceptance criteria are never dropped silently
- [ ] M12-12 Three CLI traps
- [ ] M12-13 Long paths on Windows
- [ ] M12-14 An agent CLI that dies without a result shows its error text
- [ ] M12-15 Gate output: OEM code page; one UI language for every gate (X23)
- [ ] M12-16 Gate output for the owner: full log, heartbeat, bounded memory
- [ ] M12-17 `doctor` checks what a first run needs
- [ ] M12-18 The D365FO template loads as shipped; example gate scripts
- [ ] M12-19 Guide: "First real task on D365FO", one escalation table, stale lines
- [ ] M12-20 Notes: `STATUS.md`, `DECISIONS.md`, archive, the review in the repo
- [ ] M12-21 Housekeeping
- [ ] Finish, push, pull request, CI

## Tasks

### M12-01 — recovery never accepts a commit Toryo did not make (H1)

- **Premise confirmed** (Do 1): Toryo makes a *normal* checkpoint only in `Checkpointing` (`runner::checkpoint`),
  after the implement attempt ended `Succeeded` (`ImplementOutcome::Done` ends it and moves the state in one
  transaction). `recover_writing` runs only for a *running* (or interrupted, dirty) attempt in `Implementing` /
  `Fixing`. So there a normal trailer match is never Toryo's.
- **`recover_writing`**: only a **partial** trailer match is considered, and only when `recovery::foreign_commit`
  says no: its first parent must be a commit Toryo recorded for the task (this attempt's `git_head_before`, or the
  `git_head_after` / `partial_commit` of an attempt) **and** its diff against that parent passes the path policy
  (`Protected::violates`: protected, denied, fence). Everything else falls through to the HEAD guard's soft reset
  and the turn's normal checks (a note says so). The "found by trailer — succeeded" branch is gone.
- **`checkpoint()`**: a found normal commit is taken only when it is `HEAD`, `foreign_commit` accepts it; else the
  task escalates (`policy_violation`, "… is not Toryo's (why)"), nothing is accepted. No check asks who made a commit
  (no author or committer comparison anywhere).
- **Tests** (`tests/regress_recovery.rs`): `m12_01_a_forged_checkpoint_commit_is_never_accepted` (AC-1 normal
  trailers, AC-2 `Toryo-Partial` trailers; a denied file; engine stopped after the turn; the commit is undone, the
  file is in no checkpoint, the task completes without it — fails before) and
  `m12_01_a_forged_commit_in_checkpointing_is_refused` (Toryo's own checkpoint replaced by a forged one between
  commit and database update — fails before). AC-3: the existing F4/F8 tests and
  `engine_e2e::a_crash_in_any_stage_resumes_to_completion_with_a_clean_tree` (`checkpoint:after_commit`) pass
  unchanged. The role's `never` fence uses the same `violates` as `deny_paths` (the tests use `deny_paths`).

### M12-02 — the protected scan fails closed (H4)

- `protect::walk`, the hooks folder of `git_meta` and `hash_file`: only "does not exist" stays quiet. Any other error
  is an `Unreadable { path, what: list | read, error }` (names the path); `rd.flatten()` no longer swallows entry
  errors. `protect::check` turns it into one hard `Change` (`cannot list <path> (<os error>)`), so every caller
  that escalates on hard changes escalates `protected_config_changed: cannot list …` — recovery, the loop-top check
  and the post-turn check — and nothing runs on. A baseline that cannot be taken (`baseline_or_init`) stays a plain
  error: the task cannot start.
- **Tests** (`tests/regress_recovery.rs`): `m12_02_an_unlistable_protected_folder_escalates` — the owner dies after
  the turn, the test puts `.claude/settings.local.json` into a folder it then makes unlistable, `run` recovers:
  `Escalated`, "protected_config_changed: cannot list …\.claude" (AC-1 Unix: mode 000, **never run here**; AC-2
  Windows: `icacls /deny <user>:(RD)` works in a test — run here, fails before; the ACL is removed before the
  temporary folder drops; the test says why and returns where listing cannot be taken away, e.g. root).
  `m12_02_a_missing_protected_folder_is_still_fine` (AC-3).

### M12-03 — read-only role checks run on every way a turn ends (H6)

- `read_only_turn` ran `cut_short` (live budget, stop) and then `after_read_only_turn`, whose quota branch returned
  before `protected_violation` and the "read-only role wrote" check. Now `read_only_checks` (protected state, then
  "the worktree changed during a read-only stage / HEAD moved") runs **first**, on every exit; then `cut_short`;
  then `read_only_quota` (the pause). The write is handled exactly as on the normal path: the attempt ends `Failed`
  with class `policy_violation`, the task `Escalated`; only when nothing was written do the stop, the budget pause and
  the quota pause happen as before. A stop request that arrives together with a write now escalates.
- **Tests** (new file `tests/m12_safety.rs`, all four fail before): reviewer + quota (AC-1), reviewer + live budget and
  reviewer + `toryo stop` (AC-2), planner + quota (AC-3); each asserts the state, the reason prefix
  `policy_violation: the worktree changed during a read-only stage` and that the file is in no checkpoint.

### M12-04 — the redactor releases complete lines (H5)

- `StreamRedactor::process`: when no configured secret contains a line break (`Redactions.multiline`, computed in
  `new`), everything up to and including the last `\n` in the buffer is final — the `max_len − 1` hold-back applies
  only to the unfinished last line. A secret with a line break (a PEM key) keeps the old rule.
- **Tests** (unit, `process::redact`): `a_complete_line_is_released_at_once` (AC-1: `retained() == 0` after a chunk that
  ends in `\n`; fails before), `a_secret_split_inside_a_line_is_still_redacted` (AC-2, also every split of a
  multi-line sample), `a_secret_with_a_line_break_keeps_the_old_hold_back`; the fuzz and split tests pass unchanged.
  AC-3 `regress_agents::unix::codex_stall_waits_for_a_running_command_with_a_secret_configured` — a fake Codex whose
  profile has `secret_env`, a silent command longer than the stall limit: **Unix-only, never run here**; compiled on
  Windows once by switching its `cfg(unix)` off for one `cargo check` (only the `std::os::unix` lines failed).

### Review of M12-01…04 (independent subagent, in place, no build) — follow-up commit

One High, four Medium, four Low; the High and the Mediums fixed here.
- **High (M12-01):** a forged *partial* commit under a further agent commit was accepted (it passed `foreign_commit`
  without being HEAD; recovery then stopped before the HEAD guard and the second commit's denied file reached the
  next attempt). `foreign_commit` now also requires **the commit to be HEAD**, and its first parent to be **the
  commit the attempt began at** (`git_head_before`) — every genuine checkpoint or partial commit of an attempt sits
  right on it; the "any recorded sha" parent test is gone (a forger could reset soft to an older recorded sha).
  `checkpoint()` calls the same function and, when it refuses, soft-resets to `git_head_before` (the commit is
  undone, its changes stay in the tree) before it escalates. Test: `m12_01_a_forged_partial_commit_under_another_
  commit_is_not_taken`.
- **Medium (M12-03):** the write was only reported, it stayed in the tree and the next checkpoint (`git add -A` of a
  fix cycle) committed it. `read_only_checks` now puts back what the role wrote (paths that were clean or absent
  before the turn: tracked → HEAD, new → removed; what a human had changed before the turn stays) when HEAD did not
  move. The quota test now goes on with `resume --note` and asserts the file is in no checkpoint (the acceptance
  wording "after resume"). A copy of what it wrote is kept first (`<run store>/read-only-writes/<attempt>/<path>`).
  **Earlier assertion changed:** `regress_engine::ec31_a_reviewer_write_is_a_policy_violation` ended with "left in
  place for the human, uncommitted"; it now says the file is put back (not in `git status`) and the copy is in the
  run store — the earlier decision was exactly the hole (the next fix cycle's checkpoint committed it). The reason
  text is unchanged (the other EC-31 tests assert it).
- **Medium (M12-02):** the printed way out (`--accept-protected-change <path>`) cannot work for a path that cannot be
  read; the reason now says "fix the permissions on it (or remove the folder from the worktree), then `toryo resume`".
  The walk descends into every folder of the worktree, not only protected ones, so any unlistable folder (a
  root-owned Docker data folder) escalates — by design of "fails closed"; recorded in Open. `git_meta` entries and
  `hash_file` no longer fail open or abort the run with a plain error; a file that vanishes between the listing and
  the stat is ignored.
- **Low (M12-04):** a secret with only a **trailing** line break (read from a file) no longer keeps the old
  hold-back (`multiline` ignores trailing `\n`).
- **Not changed (recorded in Open):** AC-2 of M12-01 is tested with `deny_paths`, not a role's `writes` / `never`
  fence (same `Protected::violates`); the Unix AC-3 test sets a process-wide variable (unique name); an error from
  `hub.turn_with` skips the read-only checks as before; after a stop that came with a write, the loop prints "stopped
  at a stage boundary" although the task escalated.

### M12-05 — a gate that cannot start is a machine stop, and `doctor` finds it first

- **New class `gate_unavailable`** (`engine::gates::GATE_UNAVAILABLE`), added to `MACHINE_CAUSED` (now six): plain
  `resume` costs no fix cycle, `task config --reload` works (the task is `Escalated`, a waiting state). The
  `next:` text is the generic machine-caused one (both ways out).
- **One resolver** (`src/engine/gates.rs`, new): `program_problem` (a bare name on the PATH the gate gets — `PATHEXT`
  on Windows; an absolute path exists; a repository path is **never** asked: M18's business), `script_problems` (the
  argument after `-File` / `-f`, and every absolute argument with a script extension; a relative `-File` is the
  repository's), `check_gate_problem`, `env_gate_problems`.
- **Where it is asked.** *Verify*, before any gate (`check_gates_preflight`, right after X12's environment preflight,
  one attempt end `gate_unavailable`); the environment gate preflight (`gate_ready`) now also asks the script.
  *Spawn error path*: `LocalRunner` returns a typed `StartFailed` (name, program, OS error) instead of a plain
  context; `verify()` turns it into `gate_unavailable` (`gate_start_failed`), the environment gate runner into
  `environment_gate_unavailable` (the deployment row says "did not start"). *`doctor`*: area `gates`, one line per
  check gate and environment gate (OK, or FAIL with the way out; `gate_checks`).
- **Deviation from the list (needed for AC-4):** `task create` / `task config --reload` refuse an environment gate
  only for a missing **script** (new), **not** for a missing program: two M11 tests
  (`a_wrong_gate_program_needs_a_reload_before_resume`, `an_environment_gate_that_cannot_run_escalates`) create a
  task with a program that does not exist and expect the Verify stop — "no existing test changes". A missing program
  is therefore admitted as before, `doctor` FAILs it and Verify stops with `environment_gate_unavailable`. A check
  gate is never refused or noted at create (AC-1/AC-4).
- **Tests:** `tests/m12_gates.rs` — `m12_05_a_check_gate_that_cannot_start_is_a_machine_stop` (AC-1: no refusal, no
  note, `doctor` FAIL, `gate_unavailable` before any gate and before another implementer turn, reload allowed, plain
  resume completes, no fix cycle), `m12_05_a_program_that_exists_but_cannot_start_is_the_same_stop` (AC-2: a text
  file as program — the spawn error), `m11_env_gates::m12_05_an_environment_gate_with_a_missing_script_is_a_
  machine_stop` (AC-3), and the unit tests of `gates.rs`. The M18 tables are untouched.

## Open

(filled as the work goes)
