# M12 notes — ready for the first real task (`M12_task.md`)

Branch `m12/first-run` from `main` @ `4ad97c3`. One commit per task. Baseline on Windows: **623 passed, 0 failed,
6 ignored**. The task list is `C:\.thien\AgentOrchestrator\M12_task.md` (outside the repository); the audit it rests on is
`toryo-audit-2026-10-05.md`.

## Progress

- [x] M12-01 Recovery never accepts a commit Toryo did not make (H1)
- [x] M12-02 The protected scan fails closed (H4)
- [x] M12-03 Read-only role checks run on every way a turn ends (H6)
- [x] M12-04 The redactor releases complete lines (H5)
- [x] Review of M12-01…04 (subagent, in place)
- [x] M12-05 A gate that cannot start is a machine stop, and `doctor` finds it first
- [x] M12-06 An environment gate that times out stops for the owner (X18)
- [x] M12-07 Confirm before deploy (X17)
- [x] Review of M12-05…07 (subagent, in place)
- [x] M12-08 `base_branch` is honoured; keys without effect say so (X19, X20)
- [x] M12-09 `./toryo.yaml` before `$TORYO_CONFIG` (X21)
- [x] M12-10 Text files: BOM ignored, non-UTF-8 refused by name (X22)
- [x] M12-11 Acceptance criteria are never dropped silently
- [x] M12-12 Three CLI traps
- [x] M12-13 Long paths on Windows
- [x] M12-14 An agent CLI that dies without a result shows its error text
- [x] M12-15 Gate output: OEM code page; one UI language for every gate (X23)
- [x] M12-16 Gate output for the owner: full log, heartbeat, bounded memory
- [x] M12-17 `doctor` checks what a first run needs
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

### M12-06 — an environment gate that times out stops for the owner (X18)

- `run_environment_gates`: after the export check, `res.timed_out` returns `EnvGates::TimedOut` → the attempt ends with
  the new class **`environment_gate_timeout`** (in `MACHINE_CAUSED`, now seven): `Escalated`, no implementer turn, no
  fix cycle, plain `resume` runs Verify again. The deployment record already said "interrupted"
  (`deployment_ok` → `None` for a timeout). The reason names the limit, "the environment may be half-changed
  (the deployment record says interrupted): check it, then `toryo resume <id>` runs Verify and the gate again", and
  that a longer `timeout:` needs `task config --reload` (frozen with the task). `VERIFY_RESULT.json` is not written
  for this stop (like the other machine stops; the gate's full log comes with M12-16).
- Check gates: still a gate failure; the feedback now says `timed out after 30 min` (`verify_feedback_in`, the limit the
  command was given; `verify_feedback` is unchanged for every other case, so its unit test is untouched).
- **Tests** (`m11_env_gates`): `m12_06_an_environment_gate_that_times_out_stops_for_the_owner` (AC-1, AC-2: a 3 s
  limit, the script sleeps the first time only; the deployment row — one per lock — ends `Some(true)` after the
  resume), `m12_06_a_check_gate_timeout_is_still_a_gate_failure_and_says_so` (AC-3). Two helper variants take a
  timeout (`env_gate_setup_timeout`, `probe_then_dbsync_timed`); the originals delegate with `2m`.

### M12-07 — confirm before deploy (X17)

- **Key:** machine config `environment_gates.<name>.confirm: never | first | always` (`config::Confirm`, default `never`).
  `skip_serializing_if = Confirm::is_never` — **an existing configuration keeps its frozen digest**; a gate with
  `first`/`always` has `"confirm"` in its frozen part (unit test `config::tests::the_confirm_key_of_an_environment_gate`,
  also: an unknown word is a load error naming the allowed ones). `config show` (text and `--json`: `"confirm"`) and
  `doctor` (`environment gate … can start here (confirm: first)`) print it.
- **The stop** (`Runner::confirm_preflight`, in `verify()` right after the gate preflights, before the checkout and any
  gate): for each environment gate of the implementer's `before_handoff` that this machine defines and whose
  `confirm` is not satisfied — `first`: any confirmation of the gate in this task; `always`: this checkpoint commit —
  the attempt ends `environment_gate_confirm` (new, **machine-caused**, now eight classes). The reason has the commit,
  each gate with lock, mode and command, the first 20 lines of `git diff --stat <base>..<commit>`, and "look at it
  (`git -C <worktree> show --stat <commit>`), then `toryo resume <id>` allows the deploy". All unconfirmed gates are
  asked at once. `next:` uses the generic machine-caused text (its existing unit test enumerates `MACHINE_CAUSED`, so
  a special text would change an earlier assertion; the reason itself says what to do).
- **The yes:** a plain `resume` from this class (`engine_cli`, the `machine_caused_stop` branch) calls
  `gates::record_confirmation` — it moves what the stop asked (`pending`) into `confirmed`, prints "deploy confirmed
  (<gate> @ <commit>)", then `ResumeStage`. `--yes` confirms nothing (`run --yes` finds the task still waiting).
  **Storage choice (no migration):** `<run store>/confirmations.json` (`{confirmed, pending}`), not a database column.
- **Tests** (`m11_env_gates`): `m12_07_confirm_first_stops_before_any_gate_and_resume_is_the_yes` (AC-2, AC-4: no check
  gate ran — the probe gate logs its runs —, nothing deployed, one implement attempt, `run --yes` does not skip, resume
  confirms, `fix_cycle == 0`), `m12_07_first_asks_once_per_task_and_always_once_per_checkpoint` (AC-2 second half, AC-3:
  `first` stops once over two checkpoints, `always` twice). AC-1: the whole existing suite passes unchanged with the
  default. AC-5: README and HUONG-DAN gate paragraphs; M12-18 puts `confirm: first` into the machine example, M12-19
  the class into the escalation table.

### M12-08 — `base_branch` is honoured; keys without effect say so (X19, X20)

- **`project.base_branch`** is `Option<String>` now. `task create` without `--base`: the branch's tip when the file sets
  it (a branch that does not exist is an error naming the key and the file), else the checked-out branch as before.
  `task create` prints `base: <branch> @ <commit>` and, when another branch is checked out, one `note:` line (the
  checked-out branch, and why: `--base` or `project.base_branch`). **Digest:** the frozen configuration writes
  `"base_branch": "main"` when the key is unset (custom `serialize_with`) — exactly as before, so no existing task's
  `config_sha256` changes (the engine never reads the frozen value).
- **Keys without effect** (`Effective::keys_without_effect`): `project.runs_root`, `verify.final_clean`,
  `pipeline.budget_usd_est` — none is read by any engine code (`budget_usd_est` only exists as a database column that
  nothing writes from the key). Announced as "`<key>` is accepted but has no effect in this version" by `task create`
  (`note:`), `doctor` (WARN, area `config`), `config show` (text `note:` and `--json` `"no_effect"`). **Deviation:**
  a key is announced when its value differs from the default (the loader does not keep whether the file wrote it);
  `final_clean: true` — the default — is silent. The four templates no longer set `runs_root` / `final_clean`;
  HUONG-DAN says `final_clean` does nothing (the dogfood line "the final gate still needs one passing clean run" is
  gone). `final_clean` is **not** implemented (STATUS.md, M12-20).
- **Guard (AC-3, as far as cheap):** `config::tests::keys_without_effect_are_read_nowhere` fails when any source file
  other than `config.rs` reads one of the three fields — then the list and the message must change.
- **Tests:** `tests/m12_config.rs` — `m12_08_base_branch_is_the_base_of_task_create` (AC-1: `feature/wip` checked out,
  the task branches from `main`, the note; `--base` wins; a missing branch names the key),
  `m12_08_without_the_key_the_checked_out_branch_is_the_base` (AC-2), `m12_08_keys_without_effect_say_so` (AC-3).

### Review of M12-05…07 (independent subagent, in place, no build) — follow-up commit

No Critical or High; three Medium, fixed here.
- **Medium — `toryo resume` without an id confirmed every deploy stop**, and **`resume <id> --yes`** confirmed too
  (contradicting "`--yes` never confirms a deploy"). Now: `resume <id> --yes` on an `environment_gate_confirm` stop is
  refused ("--yes never confirms a deploy … `toryo resume <id>` without --yes"); a resume of everything skips such a
  task and says it waits for the owner's yes (exit code 1). The confirm tests now resume without `--yes` and approve
  the final gate by hand.
- **Medium — the resolver did not look programs up like the spawn does** (`checks::which_in`: nothing, `.exe`, `.cmd`;
  Rust adds `.exe` to an absolute path without an extension; `CreateProcess` also looks in the folder of the
  program and in System32). `gates::program_problem` follows that, **not `PATHEXT`** (deviation from the list's "with
  `PATHEXT`": a `.bat` or `.com` alone does not start, so it is a stop at the preflight, not after it). It is lenient
  for a Windows reparse point (an App Execution Alias of Store `pwsh.exe`; `is_file()` is false for it) — if the start
  then fails, the typed spawn error stops the gate the same way.
- **Low–Medium — the timeout stop lost what the gate had printed:** the reason now carries the last 40 lines
  (`untrusted::wrap`ped). The spawn-failure hint also says "or the working directory and the rights it needs".
- **Docs:** `first` is once per **task**, not per commit — README and HUONG-DAN say so.
- **Recorded, not changed (Open):** a gate that exits 0 while a descendant keeps the pipe open past the limit is
  reported as timed out; a stop request that lands after the timeout wins and the timeout warning is lost (the next
  resume re-runs the gate); `doctor` shows `confirm:` only on OK lines; the `git -C <worktree> show` hint does not
  quote a path with spaces; if the pending file is lost, "deploy confirmed ()" is printed and Verify asks once more;
  `script_problems` would refuse an absolute non-existent output path ending in `.js` / `.py` / `.exe`; UNC `is_file`
  calls can block `doctor` / `task create`; the environment-gate spawn-error branch, a `--note` after a confirm stop
  and a reload mid-task have no test; whether `Path::is_file()` is true for a Store App Execution Alias is
  **unverified** here (the preflight is lenient for it).

### M12-09 — `./toryo.yaml` before `$TORYO_CONFIG` (X21)

- **Reproduced by reading:** `load_config` took `$TORYO_CONFIG` before `./toryo.yaml`; `--help` said the opposite.
- **Fix:** `config_path(cli)` in `src/main.rs`: `--config`, then `./toryo.yaml` when it is a file, then `$TORYO_CONFIG`.
  When both exist and are not the same file (canonicalised), one stderr note names both (printed once per process).
  `$TORYO_CONFIG` pointing at a missing file with no local file: as before (the load error names it).
- **Guide:** HUONG-DAN §3 states the order and what a variable in `$PROFILE` now means.
- **Test:** `m12_09_the_local_config_comes_before_the_variable` (both → the local file and the note; the same file
  twice → no note; only the variable → it, no note; only the file → it).
- **Scripts:** `scripts/m3-stop-test.ps1`, `m4-m7-check.ps1`, `m8-check.ps1` set `$env:TORYO_CONFIG` and relied on the
  old order when run from a folder that has its own `toryo.yaml` — see Open. No test relied on the old order (all use
  `--config` or the environment without a local file).


### M12-10 — Text files: BOM ignored, non-UTF-8 refused by name (X22)

- **New `src/textfile.rs`:** `decode(path, bytes)` / `read_text(path)` — strips a leading U+FEFF; a UTF-16 mark or NUL
  bytes → "`<file>` is not UTF-8 (it looks like UTF-16): save it as UTF-8"; other invalid UTF-8 → "… (it looks like a
  code-page file, such as Shift-JIS or Windows-1252): save it as UTF-8". Unit test `a_utf8_mark_is_ignored_and_…`.
- **Used for:** `toryo.yaml` and the machine config (`config.rs`), the org chart and every role file (`team.rs`; a role
  file that is not UTF-8 is one of the load problems, named), `SKILL.md` (`resolve_skill` → `Err` naming the file), `TASK.md`
  at `task create` (the id and title come from the decoded text) and its snapshot at turn time (`runner.rs`), and every
  context file (`context::read`: global, `CLAUDE.md`, `AGENTS.md`, `.toryo/context/*.md`, culture, role instructions,
  the task pack). The raw bytes still feed the SHA-256 of `config show`'s file list; the frozen configuration and its digest
  do not change (test: the digest of the draft set with a mark on four files equals the one without).
- **New escalation class `context_unreadable`** (the ninth in `MACHINE_CAUSED`, as the list says): a context file that
  exists and is not UTF-8 stops the turn before it starts (`turn_blocked` in org chart mode, the attempt-time check in all
  modes) with the file's name; no turn, no fix cycle; a plain `resume` after the file is saved as UTF-8. For the
  M12-19 table: reason `context_unreadable: the <stage> turn (<profile>) was not started — <file> is not UTF-8 …`,
  way out: save the file as UTF-8 (the file is read from the repository checkout — nothing is frozen), `toryo resume <id>`.
- **Deviation:** a `SKILL.md` that is not UTF-8 stops a turn as `skill_unavailable` (the reason names the file), not as
  `context_unreadable`: skills are resolved before the context is assembled and `resolve_skill` reports every problem as a
  missing skill. At `task create`, `doctor` and `config show` it is a refusal, as the list asks.
- **Not done here:** `doctor`'s per-file encoding check of everything under `.toryo/` is M12-17.
- **Tests:** `team::tests::a_byte_order_mark_is_ignored_and_other_encodings_are_refused_by_name` (AC-1, AC-3: the four
  YAML kinds with a mark load with the same digest; Shift-JIS `SKILL.md` and a UTF-16 org chart are refused by name),
  `m12_10_task_md_with_a_bom_has_the_right_id_and_utf16_is_refused` (AC-2, AC-3),
  `m12_10_a_context_file_that_is_not_utf8_stops_the_turn_by_name` (the stop, no turn, no cycle, resume). AC-4: no
  `from_utf8_lossy` is left on these paths (checked with grep).


### M12-11 — Acceptance criteria are never dropped silently (review M12)

- **Fix (`src/engine/checks.rs`, `AC_ITEM`/`AC_LIKE`/`parse_acceptance_ids` only — not the M18 rules):** after the id also
  ` — `, ` – `, ` - ` (a dash with a space on both sides); bullets `•` and `・`. A new `AC_LIKE` (after an optional bullet,
  number, box or table bar: `AC`, an optional `-`/`_`, a digit — ASCII or full-width) marks lines under the acceptance
  heading that `AC_ITEM` did not accept; `task create` refuses them all at once: "TASK.md: 2 lines under `## Acceptance
  criteria` look like acceptance criteria but would be dropped — line 6: `…`; line 7: `…`. Write each as `- AC-1: text`
  …". Fenced blocks are still skipped; a sentence that only mentions an id, or "Accept 5 items", is still text.
- **Decision:** the id stays ASCII (`AC-５` is refused, not normalised): the reviewer must return exactly these ids and
  the review check compares strings.
- **Tests:** `engine::checks::tests::ac_parser_sections_numbered_fenced_and_malformed` extended (the existing assertions
  are unchanged); `m12_11_an_acceptance_line_that_would_be_dropped_is_refused` (AC-1: the review's six lines through the
  binary, 1–4 accepted, 5 and 6 refused with their line numbers). AC-2: every existing TASK fixture loads unchanged (the
  whole suite, including the template `TASK.example.md` test). HUONG-DAN troubleshooting row updated.


### M12-12 — Three CLI traps (review M11, M8, L1)

- **(a) `task cleanup`** (`src/engine_cli.rs`): the frozen configuration and Git are opened first; unless `--force`,
  `cleanup_would_lose` lists the uncommitted changes of the worktree (`Worktree::changed`, untracked included, ignored not)
  and the partial snapshots under `<run store>/artifacts/partial` — "…would delete 1 uncommitted change(s) in the worktree …
  and the partial snapshot(s) of 1 attempt(s) in … — look at it first (`git -C … status`); `toryo task cleanup <id> --force`
  deletes it anyway" — and nothing is cancelled or deleted. **Applies to every state**, not only `Escalated` (a completed
  task whose run store still holds a recovery snapshot also asks). A clean worktree and no snapshot: as before, an
  `Escalated` task needs no `--force`.
- **(b) `resume` without an id:** `--note`, `--discard-partial`, `--keep-partial`, `--accept-protected-change` are refused
  ("--note belongs to one task — `toryo resume <id> --note …`"; before, the all-tasks branch took them and dropped them
  silently); tasks in `New` are listed as "not started — `toryo run <id>`" and skipped; one task whose resume fails
  prints "<id>: error: …" and the loop goes on; the exit code is 2 if any failed (else the largest code, as before).
- **(c) task ids:** `engine::task::valid_task_id` (`[A-Za-z0-9][A-Za-z0-9._-]{0,63}`, never `..`) is checked in
  `open_lock_file` (so in every `TaskLock::acquire`), in `engine_cli::get` / `read_task` (every command that takes an
  id) and in `task create` for `--id` and for the id of a `TASK.md` title (`create` keeps its older rule too: no `.`).
  The title regex of `task_id` can only produce valid ids; `--id` is where a path can come from.
- **Two earlier assertions changed (the milestone text contradicts them):** `engine_e2e::lock_first_recovery` expected
  `resume` without an id to plan the `New` task B-1 — now it expects "B-1: not started", B-1 still `New`, and then starts
  B-1 with `run`; `m10_events::an_escalation_carries_its_typed_reason_class` ran `task cleanup EV-3` on an escalated task
  whose worktree holds the uncommitted protected file — now `--force`.
- **Tests (`tests/m12_cli.rs`):** `m12_12_cleanup_asks_for_force_when_it_would_delete_unsaved_work`,
  `m12_12_cleanup_of_a_clean_escalated_task_needs_no_force`, `m12_12_resume_without_an_id` (the failing task is one whose
  worktree was deleted), `m12_12_a_task_id_is_never_a_path` (`../victim-x`, `a/b`, `..`, `a..b`, `_x`, `x y`; no lock file
  outside `locks/`).


### M12-13 — Long paths on Windows (read → reproduced here)

- **Reproduced first** (`tests/m12_paths.rs::m12_13_a_path_over_260_characters_goes_through_create_and_verify`, run
  before the fix) on this machine (Git for Windows 2.55, `core.longpaths` unset, a fixture repository whose commit —
  made with `-c core.longpaths=true` — holds `deep-folder-of-thirty-chars-00/…-08/Model.xml`, 9 × 31 characters):
  ```
  warning: unable to access 'deep-folder-of-thirty-chars-00/…/deep-folder-of-thirty-chars-06/.gitattributes': Filename too long
  base: main @ f6aceabe21d9
  error: git worktree add -q -b toryo/L-1 C:/Users/…/wts/L-1 f6aceabe… failed (exit code: 128):
  fatal: cannot create directory at 'deep-folder-of-thirty-chars-00/…/deep-folder-of-thirty-chars-06': Filename too long
  ```
  `task create` exits 2.
- **Fix:** `GitCmd::prefix` (`src/git.rs`) adds `-c core.longpaths=true` on Windows — for every Git call Toryo makes (the
  worktree, checkpoints, the export into the verify root). After it the same test goes through `task create` and a whole
  run to `Completed` (the verify gate runs on a checkout with the long path).
- **Doctor:** a new `paths` line prints the lengths of `worktrees_root` and `verify_root`; more than 70 characters is a WARN
  on Windows with the way out ("keep them short, for example D:\w and D:\v"). The build tools and the agents' tools may
  still stop at 260 — that is outside Toryo's Git. (M12-17 lists this line among its doctor lines: done here.)
- **Tests:** the reproduction above (now green; it is not Windows-only, a Linux Git has no such limit) and
  `m12_13_doctor_prints_the_root_lengths`.


### M12-14 — An agent CLI that dies without a result shows its error text

- **Fix (`src/agents/exec.rs`, one place for Claude and Codex):** `run_turn_with` keeps the last 16 KiB of the
  (already redacted) stderr chunks; when the parser's outcome is `ProcessCrashed` and not a success, `error_detail`
  gets "; the CLI's stderr (last lines):" and the last 10 non-empty lines (`stderr_last_lines`: colour codes stripped with
  `process::strip_ansi`, each line cut at 300 characters, prefixed `  | `). Then the existing derived-text redaction
  runs over it. The engine's escalation reason and the console already carry `error_detail`, so both show it.
  `claude.rs` / `codex.rs` are unchanged: they do not see stderr. The full stderr was always in `<label>.stderr.txt`.
- **Not done:** a turn that fails for another class (a result event with `is_error`, a timeout) is not given the stderr —
  the list asks for the no-result case only.
- **Tests:** `m12_14_a_cli_that_dies_without_a_result_shows_its_error_text` (AC-1: the `toryo` binary stands in for the
  agent CLI — it is an executable on every platform, a `.cmd` fake is refused by Rust's batch-file argument check — and
  rejects the Claude flags: the escalation reason and the console hold `error: unexpected argument …`, no escape
  codes), and the unit test `agents::exec::tests::the_stderr_tail_is_the_last_ten_lines_without_colour_codes`.


### M12-15 — Gate output: OEM code page; one UI language for every gate (X23, review M13)

- **`process::decode_output(bytes)`** (`src/process/decode.rs`, new): line by line, a line that is valid UTF-8 stays; any
  other line is decoded on Windows with `MultiByteToWideChar` and the OEM code page (`GetOEMCP`), elsewhere lossily as
  before. `decode_output_with(bytes, code_page)` takes the code page (for the tests); a UTF-8 buffer as a whole returns
  unchanged without a per-line pass. Order at the gate sink: decode → `strip_ansi` → redact (`runner.rs`, final and live
  chunks).
- **Used for:** `LocalRunner` output (every check gate, environment gate and `tool: xpp` run, and with it the
  diagnostics parser and the fix prompt), `evidence::run_version`, `evidence::git_version`, the agent `health` probe
  (`agents/cli.rs`), `doctor`'s `git --version` and `vswhere` output, and the stderr tail of M12-14. Git's own output
  (`core.quotePath=false`, UTF-8) is left as it was.
- **UI language:** `xpp::ENV` (`DOTNET_CLI_UI_LANGUAGE=en-US`, `VSLANG=1033`) now goes to every check gate
  (`run_verify_commands`: replaces what the process inherited) and every environment gate (replaces the inherited value
  except a name the gate's own `env:` in the machine config sets — the owner's explicit value wins). The comment in
  `xpp.rs` is corrected. Children still get a hidden console, so the owner's `chcp 65001` never reached them: this is
  why the decode exists.
- **Tests:** `process::decode::tests::cp932_gives_the_text_back` (AC-1, Windows; the test prints "skipped" and checks nothing
  if code page 932 is not installed — here it is), `the_xpp_parser_finds_the_file_in_a_decoded_cp932_line` (AC-1: the file
  `ソース/能力表.xml`, 12:5, `XPP0001`), `utf8_and_cp932_lines_mix` (AC-2), `utf8_is_unchanged` (AC-3, every
  platform; the existing redaction tests pass unchanged), `m11_env_gates::m12_15_every_gate_gets_the_ui_language` (a check
  gate and an environment gate under an inherited Japanese `VSLANG`).
- **Not tested end to end:** a gate that really prints cp932 through the pipeline — this machine's OEM code page is not 932,
  and `decode_output` reads the machine's, so only the inner function is pinned.


### M12-16 — Gate output for the owner: full log, heartbeat, bounded memory (review M1)

- **(c) Bounded memory (`src/runner.rs`):** `BoundedOutput` keeps the first 1 MB and the last 8 MB of the raw output (a
  `VecDeque` for the tail), counts what it drops and puts one marker line between ("[... N bytes of output omitted: Toryo
  keeps the first 1 MB and the last 8 MB of a gate's output ...]"). The whole kept text is decoded (M12-15), stripped and
  redacted once at the end, as before — so `CommandResult.output` is at most ~9 MB (was unbounded, held twice).
- **(a) Log:** `LocalRunner::run_with(spec, sink, GateIo { log, heartbeat })` (`CommandRunner::run` is `run_with` with
  nothing). The engine passes `<run store>/gate-logs/<unix ms>-<gate>.log` for every check gate, `tool: xpp` run and
  environment gate (`Engine::gate_io`); the file holds exactly `output` (redacted, bounded). `VERIFY_RESULT.json` entries
  get `"log"`; a failing gate prints `    full output: <path>` under its ✘ line. A log that cannot be written is a WARNING, not
  a failing gate. `task cleanup` removes it with the run store. **Note on the spec:** (a) says "the full output" and (c) says
  the log holds "the same text" as the bounded memory — I followed (c): the log is the bounded text, not 50 MB.
- **(b) Heartbeat:** `WaitOptions.heartbeat` (called from `Running::wait`'s loop, so also while the gate prints nothing):
  `  ... <gate> still running: M:SS elapsed, N bytes of output so far` on stderr every 60 s. `TORYO_GATE_HEARTBEAT_SECS`
  shortens it — a knob for the test, not documented for the owner.
- **Struct changes:** `CommandResult.log` (serialised only when set) — the seven struct literals in the unit tests got
  `log: None`; `WaitOptions.heartbeat` (the one literal outside `Default` got `heartbeat: None`).
- **Tests:** `runner::tests::bounded_output_keeps_the_head_and_the_tail`; `m12_gates::m12_16_a_gate_that_prints_50_mb_is_bounded`
  (AC-2: 50 MB through `pwsh` / `sh`: exit 0, head and tail kept, marker, `output.len() <= 9 MB + 300`, the log equals the
  output); `m12_16_a_silent_gate_gets_a_heartbeat_line` (AC-3, 1 s interval, and the log named in `VERIFY_RESULT.json`);
  `m12_16_a_failing_gate_names_its_log`; `m11_env_gates::m12_16_the_gate_log_is_redacted` (AC-1: the secret is `[REDACTED:…]`
  in the log; the check gate has a log too). `an_environment_gate_secret_is_redacted_everywhere` (EG-5) already walks the
  whole run store for the secret and still passes with the logs in it.
- **Not done:** live streaming of the output (the list says it is not part of this task).


### M12-17 — `doctor` checks what a first run needs

`doctor::run` gained two parameters (`config_source`, `config_variable`; its one caller is `main.rs`). Each line below is a
function of `src/doctor.rs` with its own test in `tests/m12_config.rs` (`m12_17_*`):

| line (area) | level | what | test |
|---|---|---|---|
| gates | OK / FAIL | check gate / environment gate can start (M12-05) and `(confirm: <mode>)` of each environment gate (M12-07) | M12-05 / M12-07 tests (earlier commits) |
| config | INFO + WARN | "`<file>` is in use — chosen by `--config` / the toryo.yaml of the folder you are in / $TORYO_CONFIG / the default"; WARN when `$TORYO_CONFIG` names another existing file that this folder's file shadows (not when `--config` was given) | `m12_17_doctor_says_which_config_is_used_and_why` |
| base branch | OK / FAIL / INFO | `project.base_branch` exists / does not (names the key, the way out, "task create refuses it") / unset: "starts from the checked-out branch (`main`)" | `m12_17_doctor_checks_the_base_branch` |
| encoding | OK / FAIL | every `.md .yaml .yml .txt .json .toml` under `<root>/.toryo/` is UTF-8 (a mark passes) — FAIL names each file that is not and says `context_unreadable`; the machine config too | `m12_17_doctor_checks_the_encoding_of_the_toryo_folder` |
| paths | INFO / WARN | the lengths of `worktrees_root` / `verify_root` (M12-13) | `m12_13_doctor_prints_the_root_lengths` |
| token | OK / WARN | Windows only: WARN when the token is elevated ("agents and the Low-integrity gates get an administrator token: start Toryo from a normal shell and elevate inside the environment gate script"); OK otherwise. `is_elevated()` reads `TokenElevation` | `m12_17_doctor_says_whether_toryo_runs_elevated` (the texts on every platform; on Windows that `doctor`'s line agrees with the token — the elevated branch itself cannot be run from a test) |
| profile `<name>` | WARN | a profile `env:` value with `%NAME%` — "not expanded here: the value is passed as written; write the absolute path in the machine config"; a lone `%` is not a variable | `m12_17_doctor_warns_about_a_percent_variable_in_a_profile_env` |

- Only `env:` is looked at: `exe` is expanded by Toryo and `secret_env` values are references, not paths.
- The doctor's own tests ran with the owner's real `$TORYO_CONFIG` set in the shell (`C:\.thien\m0\toryo.yaml`): the new
  WARN looked at it (`is_file`, `canonicalize`; the file is never read). That is why the variable is not reported when
  `--config` is given.


## Open

- M12-09: `scripts/m3-stop-test.ps1`, `m4-m7-check.ps1`, `m8-check.ps1` and the other M-check scripts set `$env:TORYO_CONFIG` and then run `toryo` in whatever folder they were started from. Run from a folder that has its own `toryo.yaml`, they now use that file, not the variable. Not changed (scripts are the owner's; the fix is `--config` in each call). Conservative choice: left as is, recorded here.
- Observed once while running the four checks (M12-11), not reproduced in two reruns: `m11_env_gates::an_environment_gate_runs_when_the_blobs_keep_crlf_under_eol_crlf` failed at its `marker.exists()` or state assertion; its own comment names racy-git timing. Not touched.
(filled as the work goes)
