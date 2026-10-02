# M11 notes — decisions and deviations while building M11 (`M11_task.md`)

Every entry: what was decided, why, and where. The owner folds these into plan v0.9.

## Morning summary — review fixes (2026-10-02, rewritten after `M11-review-fixes.md`)

**R1–R8 of `M11-review-fixes.md` are DONE; nothing PARTIAL or NOT STARTED.** Branch `m11/orgchart`, from
`4bfa291`: 10 commits in the table + this summary (`docs: …`), **not pushed, not merged, main untouched**. After R3 and after R8 an independent review
subagent read the diffs against the list; its findings were fixed in one follow-up commit each (sections "Review of
R1–R3" and "Review of R4–R8" below; what was only recorded is under Open).

| Commit | Task | Tests (passed) |
|---|---|---|
| `3b273a9` | R1 — the Unix-only LocalRunner test builds again | 519 (Windows compiles it out) |
| `1253ccb` | R2 — environment gates see exactly the checkpoint, in an empty folder | 524 |
| `2dc01ab` | R3 — environment gate program/args: validate what runs | 527 |
| `10e8269` | R4 — fence patterns that match nothing; the culture file is protected | 532 |
| `d97a897` | R5 — a skill that cannot be delivered stops the turn; budget for the whole chain | 535 |
| `3f7e447` | review of R1–R3: what Git cannot see, links, the lock wait | 537 |
| `af07042` | R6 — M18 script detection (applies to everyone) | 539 |
| `759483c` | R7 — the same-vendor check counts every turn that ran (applies to everyone) | 541 |
| `e7e755c` | R8 — cheap Low fixes and test gaps | 553 |
| `056e98d` | review of R4–R8: the M18 net, fence forms, the skills budget | 557 |

**Checks** (§0.2) green before every commit — **on this Windows machine only**: `cargo fmt --check`, both `clippy
--all-targets -D warnings` (default and `--no-default-features`), `cargo test --locked`. Tests: **557 passed, 0
failed, 6 ignored** (before: 519 / 6 at `4bfa291`; 441 / 6 at `aa0be03`). **Not compiled on Linux here** (WSL has
no cargo): every Unix-only block touched, and every Unix-only use of a changed type or signature, was read by hand
(§0.3; the R4–R8 reviewer did the same), but **the first push / PR is the real Linux check** (CI `ubuntu-latest`).
No real agent was run, nothing ran against a D365FO environment, your configs and data dirs were not touched.

**Windows-only tests, by name, on this machine:**
- `tests/m11_env_gates.rs::an_environment_gate_has_the_normal_token_a_check_gate_low_integrity` (E3 AC-2) — ok
- `tests/m11_env_gates.rs::a_low_integrity_check_gate_cannot_write_the_envgate_folder` (R2 AC-4) — ok
- `engine::verify::tests::emptying_a_folder_never_follows_a_junction` (review of R1–R3, `mklink /J`) — ok
- toasts: `notify::tests::a_toast_process_is_killed_at_its_deadline` — ok; `tests/m10_notify.rs::notify_test_builds_
  the_toast` — ok; `notify_test_shows_a_real_toast` — ignored (it shows a real toast: `cargo test --test m10_notify
  -- --ignored`)
- older Windows-only tests, unchanged: `the_real_script_takes_hostile_text_as_data`, `notify::tests::a_non_unicode_
  parent_variable_is_kept`, `review_tests::a_rooted_or_drive_relative_program_is_not_anchored`,
  `verify_boundary_writes` — ok
- the other 5 ignored tests are Unix-only (`sh` scripts, Unix symlinks: `item10_…`, `ec07_…`, `ec08_…`, `ec20_…`,
  `r35_…`) — not run here; they run on CI.

**What changed for a repository without an org chart** (the list: "applies to everyone"; the config hash does not
change):
- R6: `task create` / `task config --reload` now **admit** `bash -o pipefail x.sh`, `python -W error x.py`, `node -r
  …`, `ruby -I lib …`, `bash -euo pipefail …` (refused before); they **refuse** a script missing at the base commit
  when it is a positional `pwsh x.ps1`, `pwsh -Command ./x.ps1`, `cmd /c "x.cmd arg"`, or a literal repository path
  with a separator and a script extension (also inside a `-c` command line); those scripts are now protected.
- R7: the same-vendor note at the final gate also counts an implementer turn that failed or was interrupted after
  writing, so it appears in more cases; its text is `(Claude both)` (main printed `("Claude" both)`).
- Also visible without an org chart: R8 — `LocalRunner` strips colour codes before redacting; a misspelt seat in the
  machine config names that file; `doctor` warns about a `deny_paths` / `protected_paths` pattern that can never
  match (R4).

**Decisions X1–X5** are in force (section below, with the answers to the old Open list). **Deviations** worth
knowing: `verify.checkout: clean` is required with an environment gate (X2); a fence pattern without a wildcard in
its last segment covers what is under it and a wildcard pattern without `/` matches at any depth (gitignore-like,
from the second review); the skills text is not frozen (draft deviation, R8); R7 "turn started" is read from the
stored context (the `pid` column is never filled).

**Open (your decision or a later milestone; the conservative choice is in place):**
- `tool: xpp` + an environment gate: MSBuild's own `obj\` may land in the checkout and stop every deploy with
  `verify_checkout_dirty` — check on the first real UDE run; the fix would be `/p:BaseIntermediateOutputPath`.
- `envgate` relies on `<verify_root>/<task>` not being Low-writable — a `verify_root` under `AppData\LocalLow` is not
  refused. `verify.boundary: none` with an environment gate gets no warning.
- Tasks created before R3 with `%VAR%` in an environment gate's `program` escalate `environment_gate_unavailable` —
  `toryo task config <id> --reload`.
- A fence pattern with `!` anywhere is refused (a file name with `!` cannot be fenced).
- M18 limits: a script without an extension, in the repository root, run by a program outside the table;
  `pwsh -EncodedCommand`, `--flag=path`, `pwsh -WorkingDirectory dir x.ps1`.
- `skills_dir` that *contains* an agent root is not refused; R7 counts an attempt whose agent process could not
  start; the post-lock `verify_checkout_dirty` wording blames a check gate even when a first environment gate wrote.
- `which_in` ignores PATHEXT; `named_path` reads `a;b` as one path; a secret split across two live chunks (no engine
  caller) — all predate M11.
- `toryo spike pipeline` (M0 kit) runs verify commands with `Boundary::None` (the list: record, do not fix).
- **Next after M11: review M13** — cp932 output on Japanese Windows will also garble DB-sync output.

## Morning summary of the M11 run (2026-10-02, before the review fixes)

**All 17 tasks of `M11_task.md` are DONE; nothing PARTIAL or NOT STARTED.** Branch `m11/orgchart` from
`main` @ `aa0be03`, 21 commits, **not pushed, not merged**. After each group (A, B, C, E, D) an independent
review subagent read the group's diff against the task list; its findings were fixed in one follow-up
commit per group (the "Group X review" sections below say what was fixed and what was only recorded).

| Commit | Task | State |
|---|---|---|
| `cd78b22` | A1 — repository files without machine-only keys; tagged keys refused (C1) | DONE |
| `22cb580` | A2 — machine floor for `verify.boundary`, machine `verify_root` (H2) | DONE |
| `6b89f2a` | A2 — group A review: `doctor` and the Low-IL canary use the root in force | DONE |
| `37fe444` | B1 — types: org chart, role files, machine `seats`/`skills_dir`/`environment_gates` | DONE |
| `8f760f6` | B2 — loader, merge, freeze and hash; `toryo config show` | DONE |
| `c9f2655` | B3 — validation pass, key paths, "did you mean" | DONE |
| `b11dad5` | B3 — group B review: machine-seat fallback, reach check, ≥ 1 check gate | DONE |
| `995697d` | C1 — layered role instructions and culture | DONE |
| `fc11289` | C2 — the implementer's write fence | DONE |
| `8665b24` | C3 — `reviewer-differs-from-builder` off / warn / error | DONE |
| `3a7aa61` | C4 — split limits | DONE |
| `ea32fa6` | E1 — gate variables, M18 | DONE |
| `d82a86d` | E2 — verify output redacted (M2) | DONE |
| `8ecc409` | E3 — environment gates, lock, deployment record (+ group C review nit) | DONE |
| `af3743f` | D1 — skill lookup | DONE |
| `9ffd27c` | E3 — group E review: M18 per program, cwd lookups, lock folder, output order | DONE |
| `a7f7d36` | D2 — the plan's `skills`, the planner's menu, the checks | DONE |
| `8a846e9` | D3 — skills through the context loader | DONE |
| `4deb12e` | F1 — templates | DONE |
| `a99e23e` | D3 — group D review: exact names, legacy plans, skipped skills | DONE |
| (last) | F2 — README, HUONG-DAN §5.9, migration guide, this summary | DONE |

**Checks** (§0.2) green before every commit **on this Windows machine only** — the Linux test build did not compile (review blocker, fixed in R1), so F2 AC-2 was not met on Linux: `cargo fmt --check`, both `clippy -D
warnings`, `cargo test --locked`. Tests: **519 passed, 0 failed, 6 ignored** (441 / 6 at `aa0be03`). New test
files: `tests/m11_{trust,orgchart,roles,gates,env_gates,skills}.rs`; unit tests in `team.rs`, `config.rs`,
`context.rs`, `checks.rs`, `state_tests.rs`, `views.rs`, `lock.rs`, `render.rs`, `runner.rs`. No real agent was run,
nothing ran against a D365FO environment (environment-gate tests use `pwsh`/`sh` scripts in temp folders), and
your configs and data dirs were not read. No new crate.

**Backward compatibility:** a repository without `.toryo/toryo-orgchart.yaml` keeps its configuration hash
(`templates_keep_their_m10_hash` pins the three shipped templates to the `aa0be03` values) and its behaviour,
except what the task list applies to everyone (A1, A2, E1/M18, E2) — and these error **texts**: an unknown
key in any machine config now gets the friendly form with a hint, `roles.<x>: unknown role …` gets a
`Did you mean` suffix (B3), and a repository with `verify.boundary: none` now needs `verify: { min_boundary:
none }` in the machine config (H2, the point of the fix). Existing tests changed only where a task says so: the
fixture's machine config allows `boundary: none` (A2, 4 tests append `MACHINE_FLOOR`), the EC-07 unit test
expects a not-yet-existing program to be protected (M18), the flight tests' exhaustive lists include the new
events, the state tests construct `PlanPolicyViolation` with `reason: None`.

**Deviations from the task list** (details in the sections below):
1. A2: the floor is also checked at `task config --reload`; `task create --trusted-code` was not added.
2. B3: added — the implementer's `before_handoff` needs at least one `check` gate; every declared environment
   gate is checked against the machine, also an unused one.
3. C2: an empty `writes` means "no allowlist" (only `never` applies).
4. C4: `PathPolicyViolation` counts against `max_gate_retries`, `FinalRejected` against `max_review_rounds`; a
   missing one of the two split keys is 2; `--max-fix-cycles` with split limits is refused; the `--json` keys
   appear only for a split-limit task.
5. E1 (group E review): the M18 entry point is each program's *script* (PowerShell `-File`, the first argument of
   `sh`/`bash`/`python`/`node`/…, `cmd /c <file>`), not every `-f`.
6. E2: the whole output is redacted at the end (a per-stream redactor could reorder stdout and stderr).
7. E3: `status` names the lock holder from `environment_deployments` (Windows cannot read a locked file); the
   table has two extra columns (`gate`, `machine`); lock files are `locks/env/<lock>.lock`; the gate environment
   gets `NoDefaultCurrentDirectoryInExePath=1`; `environments[]` appears only when there is a row.
8. D2: `plan.schema.json` gains the optional `skills` for every agent (legacy plans drop it before storing).
9. F1: two skill stubs and `CULTURE.md` added so the template set loads; a second `toryo.yaml` example
   (`toryo.orgchart.yaml.example`) instead of changing the M10 one.

**Deviations from the drafts:** the lock holder's label is written into the lock file but `status` reads the
holder from the deployment table (above); `reviewer-differs-from-builder: error` escalates before the review turn
(the draft says "refuse to start the stage" — the same thing, with `resume --note` still possible); the
`skills_dir` in the org chart and the `flow`/`reads`/`agent` keys get their own error messages as the task list
says.

**Open (your decision; the conservative choice is in place):**
- A task created **before** M11 with `verify.boundary: none` keeps verifying that way (its config is frozen).
- After a `same_vendor_review` escalation, `resume --note` costs one more implementer turn before the review is
  refused again (the fallback is sticky); a "skip the check once" switch was not added.
- Unix only: a killed toryo leaves an environment gate's process group running while the lock is free (review
  M7); Windows kills it with the Job.
- An environment gate's script can still load a DLL from the verify checkout (Windows DLL search) — the script
  contract says to call tools by absolute path.
- `task create --repo X` reads `.toryo/` from `project.root`, not from X.
- `instructions: ../planner/x.md` (outside the role folder, inside the repository) is refused.
- **Next after M11: review M13** — cp932 output on Japanese Windows will also garble DB-sync output on the Tokyo
  machine.

**Try first** (build first: `cargo build --release`):
1. `toryo config show` on a copy of the drafts: copy `templates\d365fo\.toryo\` and `templates\d365fo\CULTURE.md`
   into a scratch Git repo, use `templates\d365fo\toryo.orgchart.yaml.example` as its `toryo.yaml` (set
   `project.root`), and a copy of your machine config with `environment_gates.dbsync` — run
   `toryo --config <scratch>\toryo.yaml --machine-config <copy> config show` (and `--json`). Every seat, gate and
   setting says where it came from; a typo shows the "did you mean" message.
2. A fake run in org chart mode: `cargo test --release --test m11_orgchart -- --nocapture`, or by hand with a
   temp `TORYO_DATA_DIR` and `--fake-agents` (the scripts in `tests/m11_*.rs` show the shape).
3. Write `dbsync.ps1` against the contract in `templates\toryo.local.yaml.example` (exit code; deploy only
   `$env:TORYO_VERIFY_CHECKOUT` / `$env:TORYO_VERIFY_BUILD`; idempotent; error lines at the end; never write into
   the checkout; never prompt; absolute tool paths). Start with a script that prints `$env:TORYO_SHA` and exits 0,
   then `toryo status` shows `d365-env holds <task> @ <sha>`.
4. If a repository uses `verify.boundary: none`, add `verify: { min_boundary: none }` to the machine config.

## Review fixes (2026-10-02)

Task list: `M11-review-fixes.md` (review of 2026-10-02, built and run on Linux by the reviewer). Same branch
`m11/orgchart`, from `4bfa291`. One commit per task.

### Progress

- [x] R1 — the test build compiles on Linux again
- [x] R2 — environment gates see exactly the checkpoint; neutral working directory
- [x] R3 — environment gate `program`/`args`: validate what runs
- [x] R4 — fence patterns that match nothing; the culture file is protected
- [x] R5 — a skill that cannot be delivered stops the turn; the budget check covers the whole chain
- [x] R6 — M18 script detection: no false refusals, no missed scripts
- [x] R7 — the same-vendor check counts every turn that ran
- [x] R8 — cheap Low fixes and test gaps

### Decisions of the list (X1–X5) and the answers to "Open"

- **X1** an environment gate's working directory is an empty folder agent code cannot write (R2); **X2** (replaced by
  X6 in round 2) before the first environment gate the checkout must be exactly the checkpoint, so an org chart
  with an environment gate needs `verify.checkout: clean` (R2); **X3** `%VAR%` in an environment gate's `program`/`args` is expanded
  once, at load (R3); **X4** `reviewer-differs-from-builder` counts every implementer turn that ran (R7); **X5**
  the same-vendor note keeps `(Claude both)` (R7).
- Answers to the M11 "Open" list: a pre-M11 task with `boundary: none` keeps it — accepted; the extra turn after
  `same_vendor_review` — accepted; Unix process group of a killed toryo — accepted, note kept; a DLL from the
  checkout — closed by X1; `task create --repo X` — refused in org chart mode unless X is `project.root` (R8);
  `instructions:` outside the role folder — stays refused.

### R1 — the test build compiles on Linux again

- `tests/adapters.rs::local_runner_captures_plain_text_and_exit` (`#[cfg(unix)]`) built a `RunSpec` without the
  `redact` field E2 added: `E0063` on Linux, invisible here because Windows compiles the test out.
- §0.3 applied to every type and signature M11 changed (`RunSpec`, `PipelineEvent::PlanPolicyViolation`,
  `context::Request`/`TeamContext`, `EngineEvent` + `VerifyCommand.kind`, `Machine`, `MachineConfig`,
  `Checkout`, `Effective`, `CommandSpec`, `Origins`, `evidence::run_version`): read every `cfg(unix)` /
  `cfg(not(windows))` / `cfg(all(unix, …))` block of `src/config.rs`, `src/engine/{artifacts,protect,snapshot,
  runner_review_tests}.rs`, `src/engine_cli.rs`, `src/git/canary.rs`, `src/notify.rs`, `src/process/{mod,unix}.rs`,
  `tests/{adapters,m10_notify,m10_views,m9_xpp,redaction,regress_agents,regress_engine,regress_policy,
  spike_e2e}.rs`. That literal was the only one.
- **Not compiled on Linux here**: WSL exists on this machine but has no cargo (nothing installed for this). The
  reviewer's Linux run with this one line was clean (fmt, both clippy, 551 tests passed) — the first CI run
  (`ubuntu-latest`) is the real check.

### R2 — environment gates see exactly the checkpoint; neutral working directory

- **Before the first environment gate** of a verify run (`Engine::deploys()`: the implementer's `before_handoff`
  has an environment gate this machine defines), `verify::mutated` and the new `verify::untracked`
  (`status --porcelain=v1 -z --untracked-files=all --ignored` through the recorded admin dir, R47) look at the
  checkout. Either one found → no environment gate starts, no `environment_deployments` row, `VERIFY_RESULT.json`
  is stored with the check-gate results, `pass: false` and the reason in `mutated`, and the task **escalates**
  (not a fix cycle): `verify_mutated_tree`, or the new class `verify_checkout_dirty` with up to 10 file names and
  the hint to write output under `TORYO_VERIFY_BUILD`. The check after the gates stays (E3 AC-8).
- **Load rule (B3 pass):** the implementer's `before_handoff` with an environment gate needs `verify.checkout:
  clean`; `tracked` is a problem naming `verify.checkout` in `toryo.yaml` and `gates.<name>` of the org chart.
- **Working directory (X1):** `verify::envgate_dir` — `<verify_root>/<task>/envgate`, created and emptied before
  each gate. It is not among the Low-labelled folders (`dir`, `build`, `cache`, `tmp`, `home`), so Low code
  cannot plant a file there. `NoDefaultCurrentDirectoryInExePath=1` stays; the gate variables are unchanged.
- Wording updated: `templates/toryo.local.yaml.example` (the contract: empty working directory, no `git`
  against the checkout, use `TORYO_SHA`), `tests/fixtures/m11/toryo.local.yaml` (the fixture's comment), README,
  HUONG-DAN §5.9.
- No M11 E3 test relied on the working directory being the checkout (they use `TORYO_VERIFY_CHECKOUT` or
  absolute paths); all pass unchanged.
- Tests: `team::tests::an_environment_gate_needs_a_clean_checkout` (AC-5), `tests/m11_env_gates.rs` —
  `a_check_gate_that_adds_a_file_stops_the_deploy` (AC-1), `a_check_gate_that_edits_a_tracked_file_stops_the_
  deploy` (AC-2), `an_environment_gate_runs_in_a_neutral_folder` (AC-3), `a_low_integrity_check_gate_cannot_
  write_the_envgate_folder` (AC-4, Windows only).
- **Open (owner, on the UDE machine):** the built-in `tool: xpp` check gate passes `OutputDirectory` /
  `CompilerMetadata` under `TORYO_VERIFY_BUILD`, but MSBuild's own intermediate folder (`obj\` next to the
  `.rnrproj`) is not redirected. If MSBuild writes it into the checkout, an org chart with `tool: xpp` + an
  environment gate would always stop with `verify_checkout_dirty`. Not changed here (the X++ command line is M9's);
  the template's default `build` gate is the owner's script, which must write under `TORYO_VERIFY_BUILD`. Check
  with the first real xpp + dbsync run; the fix would be `/p:BaseIntermediateOutputPath=<build>\obj\`.

### R3 — environment gate `program`/`args`: validate what runs

- **Expanded once, at load (X3):** `team::load` (new parameter: the machine config's name for messages) runs
  `expand_env_gate` on every machine `environment_gates.<name>`: `%VAR%` in `program` and in each `args[i]` is
  replaced; an unset variable is a load problem `"<machine config>: environment_gates.<name>.args[3]: environment
  variable %X% is not set (in `…`) — set it, or write the path out"`. The **expanded** text is what the B3 reach check
  sees (`env_gate_problems` no longer expands on its own), what is frozen into `config.json` and hashed, and what
  runs: `runner::env_gate_program` no longer calls `expand_vars`. So a variable that differs between `task create`
  and `run` cannot change the command after it was checked.
- **PATH at run time:** a bare `program` is resolved through PATH (`checks::which_in`, the PATH value passed in so
  the unit test does not touch the process PATH); the resolved file — and an absolute program too — goes through
  the shared `team::reach` (project.root, worktrees_root, verify root). Inside → `environment_gate_unavailable:
  … program `x` resolves to <file>, inside the repository (<root>) where agent code can write …` (escalate, nothing
  deployed).
- **`config show`:** the JSON gate entry gained `command` (`[program, args…]` as expanded, `null` for a check gate or
  an undefined environment gate); the text form prints `      runs: <program> <args…>` under the gate line.
- Config hash: only org chart mode has environment gates, so a repository without an org chart keeps its hash. In
  org chart mode a machine config that used `%VAR%` in a gate now freezes the expanded text (a different hash than
  at 4bfa291 — intended: the hash must cover what runs).
- Tests: `team::tests::environment_gate_variables_are_expanded_at_load` (AC-2 unset, AC-3 into the repository, and
  the stored text is expanded), `team::tests::a_bare_gate_program_found_inside_the_repository_is_refused` (AC-4),
  `tests/m11_env_gates.rs::an_environment_gate_variable_is_expanded_once_at_create` (AC-1: `%TORYO_R3_GATES%` set
  only for `task create`; `config.json` holds the expanded path; `run` without the variable deploys). The B2 test
  `the_draft_set_loads_freezes_and_reloads` gained the `runs:` line (added assertions only).
- Docs: the template's `environment_gates` paragraph, README, HUONG-DAN §5.9.

### Review of R1–R3 (independent subagent) — follow-up commit

A subagent reviewed `4bfa291..2dc01ab` (read-only, from a `git archive` export; it ran `m11_env_gates` 15/15). Every
AC of R1–R3 was confirmed met (R2 AC-4 is a property test: `base` was never Low-writable). Fixed:

- **Must fix — what `git status` cannot see:** a gitlink (mode 160000, no `.gitmodules`; `git add -A` turns a nested
  repository the implementer creates into one) gives an empty folder in the checkout that a Low check gate can fill;
  Git never looks inside it, `git clean -ffdx` leaves it, and an entry named `.git` below the root is skipped too. So
  both pre-deploy checks passed while the deploy would read agent files. New `verify::hidden`: `git ls-files -s -z`
  (a 160000 entry is reported) and a walk of the checkout **without following links** — any `.git` entry below the
  root, any untracked link or junction, any untracked file → `verify_checkout_dirty`. `verify::deployable` combines
  `mutated`, `untracked` and `hidden`. Test: `tests/m11_env_gates.rs::files_git_cannot_see_stop_the_deploy` (a
  committed gitlink filled by the probe gate, plus `src/.git/payload`; no marker, no deployment row).
- **Should fix — `envgate` as a junction:** `empty_dir` (used for `envgate` and the Low folders) now removes a link
  or junction put in place of the folder, or inside it, as a link — never emptying its target; a junction entry no
  longer makes it fail. Test (Windows only, `mklink /J`): `verify::tests::emptying_a_folder_never_follows_a_junction`.
- **Nice to have, done:** the checkout is checked again once each environment gate holds its lock (the wait may be
  long; new `EnvGates::Dirty`, handled like the pre-deploy check); stale "runs in the verify checkout" comments
  (runner, config.rs); the `environment_gate_unavailable` hint names the machine config in use
  (`--machine-config`), not the default; an unset variable is reported once (no second "must be an absolute path"
  problem); `draft_repo` normalises CRLF in the machine fixture too (a Windows clone with `autocrlf`); the script
  contract says TORYO_VERIFY_BUILD is Low-written and not covered by the check, and that what the script interprets
  from either folder runs agent logic with full rights; HUONG-DAN's troubleshooting table has rows for
  `verify_checkout_dirty`, `environment_gate_unavailable`, `skill_unavailable` / `context_over_budget`.
- **Not fixed — recorded under Open:** (a) `envgate` relies on `<verify_root>/<task>` not being Low-writable; a
  `verify_root` the owner puts under a Low-labelled tree (e.g. `AppData\LocalLow`) is not refused. (b) `verify.boundary:
  none` (machine `min_boundary: none`) with an environment gate makes the check gates run with full rights — no B3
  warning (Linux tests need `none`). (c) A task frozen before R3 with `%VAR%` in `program` now escalates with
  `environment_gate_unavailable … not found` — `toryo task config <id> --reload` (troubleshooting row added).
  (d) `named_path` reads `a.ps1;b.ps1` or space-joined paths as one path (owner mistakes only). (e) `which_in` ignores
  PATHEXT and tries the bare name first (predates M11). (f) The xpp `obj\` question of R2 stays open.

### R4 — fence patterns that match nothing; the culture file is protected

- **Normalised at load:** `checks::fence_pattern` — `\` → `/`; a leading `./` and empty or `.` segments go (`a//b`
  is `a/b`; the list said "a leading `./`", the middle ones are the same mistake); a trailing `/`, or a pattern
  without `*` / `?`, becomes `x/**` (the path itself and everything under it — `glob_ci` already matched `dir` for
  `dir/**`). `team::load` rewrites the role's `writes` / `never` in place, so the frozen config holds the normalised
  form. Messages quote what was written (`<role file>: never: `/secrets/**` is absolute …`).
- **Refused (B3, in `load`):** empty / names no path (`./`), absolute (`/x`, `C:\x`, `\\server\x`), a `..` segment,
  and `{ } [ ] !` — "only `*` (within one folder), `**` (any depth) and `?` (one character)". `!` is refused
  anywhere in the pattern (a file name with `!` cannot be fenced; conservative — recorded under Open).
- **Legacy (`policy.deny_paths` / `protected_paths`):** unchanged and still compared as written; `toryo doctor`
  (`doctor::policy_checks`) adds a WARN `policy.<key>: `<p>` never matches … write `<fix>`` for `./` / `//` /
  trailing `/` and for the forms `fence_pattern` refuses. `@verify-scripts` is not a pattern. The hash of such a
  configuration is unchanged (the test checks the canonical JSON keeps `./x/**`).
- **Culture:** `task_policy` adds `team.culture` (repository-relative, normalised at load) to the protected exact
  files in org chart mode; `## Allow protected` opens it like any protected path. A repository without an org chart
  has no culture, so nothing changes there.
- Config hash in org chart mode: a role file with `src` / `./x` / `x/` now freezes `x/**` (different hash than at
  4bfa291 for such a file; the drafts and the template use the normal form, their hash is unchanged).
- Tests: `checks::tests::fence_patterns_are_normalised_or_refused` (AC-1: the five forms, `\`, `//`, the refusals),
  `checks::tests::legacy_patterns_that_never_match_are_named`, `team::tests::fence_patterns_are_normalised_at_load`
  (frozen form; problems name file, key, pattern), `doctor::tests::a_legacy_pattern_that_never_matches_is_a_warning`
  (AC-3), `tests/m11_roles.rs::the_culture_file_is_protected_unless_the_task_allows_it` (AC-2: restored + fix note;
  with `## Allow protected` committed). No existing test changed.
- Docs: README, HUONG-DAN §5.9, the d365fo template's implementer role file (pattern rules) and org chart
  (`culture` is protected).

### R5 — a skill that cannot be delivered stops the turn; the budget check covers the whole chain

- **Before a turn** (`Engine::turn_blocked`, called after admission and before `begin` in `plan`, `review` and
  `run_implement_turn` — so implement and every fix stage): the stage's `context::request` is built; any
  `missing_skills` (a skill that does not resolve now, or an approved-plan skill no longer on the menu) → escalate
  `skill_unavailable: the <stage> turn (<profile>) was not started — skill 'x' not found: neither <repo path> nor
  <machine path>; put the skill back, then `toryo resume <id> --note …``. No attempt row, no tokens. `resume`
  re-enters the stage, which checks again. A pack hash error is left to the attempt (R34 escalation as before).
  Without an org chart nothing is checked.
- **The menu** (`context::skill_menu`, and the plan check's menu via the new `context::available_skills`) lists only
  skills that resolve now; a plan naming one that does not is sent back as "not on the implementer's menu".
- **Plan-time budget:** `plan_skills_problem` assembles the implement context for every distinct profile of
  `t.implementers` (or the stage profile when the chain is empty). Over budget → the same "choose fewer skills"
  re-plan, now naming the profile (`the fixed instructions of the implementer `claude-max` (… bytes, with TASK.md
  and PLAN.md inline)`).
- **Attempt time:** in `context::assemble`, when skill parts are present and the agent has no run store, TASK.md and
  PLAN.md get no trim rank (they are counted in `fixed_bytes`). With skills, a context still over budget after
  trimming → `context::over_budget` → escalate `context_over_budget: the implementer context is N bytes, over its
  budget of K KB (B bytes); its skills (S bytes), TASK.md and PLAN.md are never trimmed — raise
  `context.budget_kb.implementer` or choose fewer skills` — before the turn (`turn_blocked`, no attempt) and, as a
  guard, in `context_and_data` (attempt `Failed` with that class). Without skills the M10 rule stays (trim, warning).
  With the run store (Codex) TASK.md / PLAN.md may still become `path (budget)` — listed, not truncated.
- M11 test changed (mandated by "the planner's menu lists only skills that resolve"):
  `tests/m11_skills.rs::machine_skills_are_delivered_and_missing_ones_skipped` asserted the menu line
  `- xpp-batch — (not found on this machine)`; it now asserts that `xpp-batch` is absent and `xpp-axclass` listed.
  The `context show` assertions of that test (skipped, not found) are unchanged — the preview still reports it.
- Tests: `tests/m11_skills.rs::a_skill_that_is_gone_stops_the_turn_until_it_is_back` (AC-1: both paths, no
  implementer attempt, resume with the skill in the context), `::the_budget_check_covers_every_implementer_of_the_
  chain` (AC-2: `[codex-openai, claude-max]`, the feedback names `claude-max` only),
  `context::tests::with_skills_the_inlined_task_pack_is_never_trimmed` (attempt time).

### R6 — M18 script detection: no false refusals, no missed scripts (applies to everyone)

- New `checks::script_of(program, args) -> Option<String>`: `pwsh`/`powershell` — `-File x` as before, else the first
  token of `-Command` / `-c` (after an optional `&` / `.`, quotes and a trailing `;` removed) when it has a script
  extension, else the first positional argument ending in `.ps1`; `cmd` — the first token after `/c` (`/k`);
  interpreters — `INTERPRETERS` gained a third list, the flags that take a value (`sh`/`bash`/`zsh`/`dash`/`ksh`
  `-o -O`, `python`/`python3`/`py` `-W -X`, `node` `-r --require --import --loader`, `ruby` `-I -r`, `perl` `-I`),
  whose value is skipped. **A candidate is a script only with a path separator or a script extension** — otherwise
  `None`, never a refusal (an existing file is still protected, as any existing argument; this is how "or exists at
  the base commit" is honoured). `dash` got `-o -O` too (same shell family).
- `script_arg` keeps its signature (`Option<&String>`; the M18 tests call it) and returns the argument when
  `script_of` names a whole argument. `verify_files` and `verify_entry_points` use `script_of`.
- **The net:** `checks::script_like_args` — every argument (or its first token) that is a repository path with
  extension `.ps1 .psm1 .cmd .bat .sh .py .js .ts .rb .pl .exe`. `task::check_entry_points` refuses one missing at
  the base commit: `verify_script_missing: <gate|verify command> `<name>` passes `<path>` (a script or program file),
  which is not in the base commit …`. (Narrowed by the R4–R8 review, see there.)
- **Remaining limit (recorded):** a script without an extension, in the repository root, run by a program outside the
  table (e.g. `make` with a root `Makefile`, `dotnet tool run x later`) is neither protected before it exists nor
  refused when missing. Also not parsed: `pwsh -EncodedCommand`, `--flag=path` forms.
- What changed without an org chart: `task create` / `task config --reload` admit `bash -o pipefail x.sh` (and the
  other value flags) that were refused; they refuse a positional `pwsh x.ps1`, a `-Command ./x.ps1`, `cmd /c "x.cmd
  arg"` and any script-extension argument missing at base that were admitted; those scripts are now protected. The
  config hash does not change (nothing new is stored).
- Tests: `checks::tests::script_detection_skips_flag_values_and_finds_more_scripts` (AC-1 table, incl. `dotnet test -f
  net8.0`, and the net), `tests/m11_gates.rs::script_detection_admits_flag_values_and_refuses_a_positional_script`
  (AC-2 in a toryo.yaml-only repository, plus the net). AC-3: `m18_script_arguments_per_program`,
  `m18_entry_points_are_protected_before_they_exist`, `a_gate_script_missing_from_the_base_commit_is_refused_at_
  admission` and the EC-07 tests pass unchanged.

### R7 — the same-vendor check counts every turn that ran

- `runner::implementer_profiles` (shared by the final-gate note and `error`) counts every `implement` / `fix-N` agent
  attempt whose turn started, whatever its end — `Succeeded`, `Failed`, `Interrupted` (R49 keeps the partial work).
  "Started" is read from the row: `context_sha256` is stored right before the agent starts, so an attempt without it
  ended before (a pack hash error, R5's over-budget guard) and is left out; the `pid` column is never filled, so it
  cannot tell. A `Succeeded` attempt always counts (rows from before context hashing).
- **Deviation (applies to everyone):** legacy mode, where the note is always `warn`, now shows `same vendor: …` in
  more cases — whenever a turn of the reviewer's vendor wrote into the checkpoint, also one that then failed.
- X5: the note keeps `(Claude both)`; the C3 section's "byte-identical" sentence is corrected.
- Tests: `tests/m11_roles.rs::the_same_vendor_check_counts_a_turn_that_failed_after_writing` (AC-1: Codex quota →
  Claude writes and hits its quota → pause → `resume` → Codex succeeds → Claude reviews; `warn` names `claude-max`,
  `error` escalates `same_vendor_review` with no review attempt), `::the_legacy_same_vendor_note_is_exact` (the exact
  line in a toryo.yaml-only repository). AC-2: the C3 tests pass unchanged.

### R8 — cheap Low fixes and test gaps (one commit)

- **Timed-out / cancelled environment gate:** `run_environment_gates` runs the gate itself (not through
  `run_gate_command`): `ok` is recorded empty when the gate timed out or was cancelled (`status`: "interrupted — the
  environment may hold a partial deploy"); a stop that arrives after the gate finished keeps its real result (then the
  run stops). Test `tests/m11_env_gates.rs::a_timed_out_environment_gate_is_recorded_as_interrupted` (a 3 s timeout,
  the gate's child writes a heartbeat file: it stops growing — the whole tree is gone).
- **"approved" means this commit:** `views::deployment_status(d, holder, approved_head, active)` — `approved` only when
  the holder is `Completed` **and** the row's `sha` is the head its final gate approved (the completed task's
  checkpoint). `environments()` passes it; the final-gate note passes none. M11 test changed:
  `views::tests::deployment_status_in_words` passes the row's own sha as the approved head for its existing cases
  (expectations unchanged) and gained two cases (another commit, none known → `not approved yet`).
- **Lock names:** `lock::lock_name_problem` refuses Windows device names (`con prn aux nul com1–9 lpt1–9`, any case,
  also before a dot — `con.lock` is a device); `lock::lock_key` lower-cases the name, and the runner uses that form
  for the lock, the deployment row, the wait and `VERIFY_RESULT.json` (the frozen config keeps the text as written,
  so its hash does not change). Tests: `lock::tests::lock_names_refuse_device_names_and_ignore_case`,
  `tests/m11_env_gates.rs::a_lock_name_is_stored_in_lower_case`.
- **`if_missing: error` end to end:** `tests/m11_env_gates.rs::a_missing_environment_gate_refuses_the_task`.
- **Redaction:** `LocalRunner` strips ANSI, then redacts (the whole output and the live chunks). Test
  `tests/adapters.rs::local_runner_redacts_a_secret_split_by_a_colour_code` (`key=ab\x1b[0mcd!` → `key=[REDACTED:S]!`;
  Windows and Linux).
- **`skills_dir`:** a B3 problem (`<machine config>: skills_dir: …`) when, after `%VAR%`, it is not absolute, an
  unset variable, or inside the repository / worktrees / verify root; `doctor::skills_dir_checks` — OK when the folder
  exists, WARN when it does not. Test `team::tests::the_machine_skills_dir_is_absolute_and_out_of_reach`.
- **Tests the review found missing:** `tests/m11_skills.rs::a_plan_without_skills_gives_the_implementer_none`,
  `::fix_and_fallback_turns_get_the_approved_skills` (`fix-1` and the Claude fallback both get `xpp-batch` only).
- **Messages:** a seat only the machine config names is reported as `<machine config>: seats.<x>: unknown role …`
  (`roles.<x>` when the file says `roles:`; new `MachineConfig::seats_spelling`, `Origins::machine_roles_key`, both
  not serialised) — test `config::tests::a_misspelt_machine_seat_names_the_machine_config`. Fence violations name
  their rule: `Protected::violation_words` — recovery ("the interrupted attempt … left paths the implementer may not
  change: `docs/x.md` (outside the implementer's write fence (`writes: [src/**]`))") and the plan re-plan bound (the
  plan-policy event now carries that reason when a path breaks the fence) — test
  `tests/m11_roles.rs::a_fence_violation_names_its_rule_after_the_bound_and_in_recovery`.
- **Loader:** `task create --repo X` in org chart mode is refused unless X is `project.root` (compared canonicalised):
  "change project.root in <toryo.yaml> or leave --repo out" — test `tests/m11_orgchart.rs::task_create_repo_must_be_
  the_project_root_in_org_chart_mode`. The template test `config::tests::the_commented_m11_machine_block_parses`
  uncomments the M11 block of `templates/toryo.local.yaml.example` (`seats`, `skills_dir`, `environment_gates`,
  `verify`; the active `roles:` is dropped — one of the two per file) and parses it as a `MachineConfig`.
- **Docs:** README and HUONG-DAN §5.9 — an org chart without `limits:` has the single counter (2 fix cycles), not
  split defaults; `writes: []` (or none) means no allowlist, only `never`. The machine template notes the lock-name
  and `skills_dir` rules.
- **Deviation from the drafts (recorded as the list asks):** the draft `toryo-local.yaml` says skills "ARE frozen and
  hashed with each task" — only the skill **names** (role files, the plan) and the lookup **roots** are frozen; the
  `SKILL.md` text is read for each attempt and hashed into that attempt's `context_sha256` (D3 AC-2).
- **Open, not fixed (the list says so):** `toryo spike pipeline` (the M0 kit, `src/spike/pipeline.rs`) runs repository
  verify commands with `Boundary::None` whatever the machine floor says.

### Review of R4–R8 (independent subagent) — follow-up commit

A second subagent reviewed `2dc01ab..e7e755c` (the R2/R3 follow-up and R4–R8; read-only, from a `git archive`
export, all affected suites passing there). It confirmed Linux compilation by reading, the named test changes, and
M10 behaviour/hash without an org chart. Fixed:

- **Must fix — R6's net refused legitimate configurations, also without an org chart:** it took any argument ending
  in a script extension, so `bash -c "cd src && ./test.sh"`, `pwsh -Command "Invoke-Pester tests/x.Tests.ps1"`,
  `npx eslint src/**/*.js`, `tsc --outFile build/x.js`, `$HOME/bin/x.sh` were refused at `task create`. Now
  `script_like_args` looks at a whole argument, or — after `-c` of a shell, `-Command`/`-c` of PowerShell, `/c` of
  cmd — at the first token of the command line; skips the value of `-o`, `--out…`, `-OutFile`; and keeps a
  candidate only when it is a **literal path** (no whitespace, `* ? [ ] $ % ~`) **with a separator** and a script
  extension. What it finds is now also **protected** (`verify_files`), e.g. `bash -c "./ci/x.sh --fast"`. Admission
  lists folders too (`ls-tree -r -t`). `script_of`: a shell's short-flag cluster ending in `o`/`O` takes a value
  (`bash -euo pipefail ci/run` → `ci/run`), one containing `c` runs inline code; a folder or a glob in the script
  position (`node --test test/`) is no script. Tests: the R6 table and net cases extended, an admitted lint/output/
  command-line configuration in `script_detection_admits_flag_values_and_refuses_a_positional_script`.
- **R4, still failing open:** a last segment without a wildcard now always covers what is under it (`**/secrets` →
  `**/secrets/**`), and a wildcard pattern without `/` matches at any depth (`*.pfx` → `**/*.pfx`, gitignore-like).
  `with_fence` normalises too (a task frozen before R4); `## Allow protected` drops a leading `./`; `doctor` warns
  about a legacy wildcard-free pattern that names a folder of the repository (`deny_paths: [secrets]`), and about
  `x/**/` with the right suggestion. Tests: `fence_patterns_are_normalised_or_refused` (more forms, deeper matches,
  pre-R4 patterns, `./` in the allow list), `legacy_patterns_that_never_match_are_named`,
  `a_legacy_pattern_that_never_matches_is_a_warning` (the folder case; assertions added only).
- **R5, plan time weaker than attempt time:** the plan-time check now also applies `context::over_budget` (the
  rendered context after trimming) on the budget minus `PLAN_BUDGET_MARGIN` (512 bytes: room for a fix stage's pack
  sentence and data note). Test `the_plan_budget_check_uses_the_rendered_context` (a Claude implementer with nothing
  trimmable, the rendered context 100 bytes under 8 KB: re-planned; fails with the old check — verified).
- **R5, a skill gone between the check and the attempt:** `context_and_data` escalates `skill_unavailable` too (one
  reason helper for both places). `context show` prints "the turn would not start: skill_unavailable / context_over_
  budget …" when the real turn would not start.
- **Test gaps:** `a_missing_planner_or_reviewer_skill_stops_their_turn`, `a_skill_that_grew_past_the_budget_stops_
  the_turn` (no attempt, the numbers, the `context show` line), `runner::tests::the_deployment_record_of_a_gate_
  that_ran` (the new `deployment_ok`: a finished gate keeps its result whatever the stop flag says — the end-to-end
  race of a stop arriving just after the gate is not reproducible reliably, so the decision is a pure function and
  tested as one). The timeout test now allows 6 s (two `pwsh` start-ups).
- **Recorded under Open, not fixed:** (a) `pwsh -WorkingDirectory scripts x.ps1` is checked as `x.ps1` at the root;
  (b) a `skills_dir` that *contains* an agent root (e.g. the parent of the worktrees) is not refused; (c) R7 counts an
  attempt whose context was stored but whose agent process could not start (no "process started" marker; the list
  allows counting them); (d) `EnvGates::Dirty` after the lock wait uses the pre-deploy wording ("a check gate must
  write …") although, with two environment gates, the first gate's script may be the writer; (e) a secret split
  across two live chunks of `LocalRunner` (no engine caller; predates M11).

### Round 2

Task list: `M11-review-fixes-2.md` (re-review of R1–R8 on 2026-10-02; the reviewer built `f77fc49` on Linux: fmt
and both clippy clean, 587 tests passed). Same branch, from `f77fc49`. One commit per task; this round ends with
the push and the pull request.

#### Progress (round 2)

- [x] R9 — environment gates deploy from a fresh export of the commit
- [x] R10 — M18 script detection, second pass
- [ ] R11 — `resume` after a machine-caused escalation costs no fix cycle
- [ ] R12 — cheap Low fixes
- [ ] Finish — notes, README, HUONG-DAN, templates
- [ ] Push and pull request

#### Decisions X6–X8 (binding)

- **X6 — replaces X2.** An environment gate no longer reads the Low-integrity verify checkout at all: it gets a
  fresh export of the checkpoint commit in a folder agent code cannot write (R9). `verify_checkout_dirty` is no
  longer an escalation (extra files are a warning, not deployed); `verify.checkout: clean` is no longer required;
  the MSBuild `obj\` Open item of R2 is closed by it. **X1 stays** (the empty `envgate` working directory).
- **X7** M18 refuses only a gate's entry script, and only when the implementer could write it; other arguments are
  protected when they exist, never refused (R10).
- **X8** leaving an escalation the machine caused (a missing skill, a context over budget, a gate the machine cannot
  run) costs no fix cycle: plain `toryo resume <id>` goes back into the stage (R11).

#### R9 — environment gates deploy from a fresh export of the commit

- **The export:** `verify::deploy_source(g, co, head) -> Export { dir, index, tree }` — `<base>/deploy`, emptied with
  the link-safe `empty_dir`; a scratch index `<base>/deploy.index` (`GIT_INDEX_FILE`; a link there is removed as a
  link); `read-tree <head>`, `checkout-index -a -f -u`, `write-tree` must equal `<head>^{tree}`. Git runs with
  `Loc::Explicit { git_dir: co.admin (recorded at creation, R47), work_tree: deploy }` and the hardened prefix of
  the task's `GitCmd` (hooks, fsmonitor off, the R37 filter plan) and the repository's own config, so the bytes are
  those of a clean checkout. Nothing is read from `co.dir`. Neither `deploy` nor the index is in the Low label list.
  An export that fails → the gate does not start, `environment_gate_unavailable` ("the checkpoint … could not be
  exported").
- **When:** in `run_environment_gates`, once the gate holds its lock, for every environment gate; then the
  deployment row, then the gate. `set_gate_vars` gained the `checkout` path: check gates keep `co.dir`, environment
  gates get the export. The working directory stays `envgate` (X1). The export has no `.git`.
- **After each environment gate:** `verify::export_changed` — `update-index -q --refresh` on the scratch index, then
  `diff-files` (content after a stat refresh) and `ls-files --others` → `EnvGates::Mutated` → `verify_mutated_tree`
  (E3 AC-8, now against the export; `an_environment_gate_that_writes_into_the_checkout_mutates_the_tree` unchanged).
- **X2 withdrawn:** before the first environment gate only `verify::mutated` decides (it still escalates, R29); the
  call after the lock wait is gone with `deployable` and `EnvGates::Dirty`. `untracked` + `hidden` became
  `verify::checkout_extra` (the walk without following links, plus gitlinks from `ls-files -s`): one stderr line
  `!! a check gate left files in the verify checkout — they are not deployed: […]` and `checkout_extra` (first 10)
  in `VERIFY_RESULT.json`. `verify_checkout_dirty` (`verify::DIRTY`) is gone. The B3 rule "an environment gate
  needs `verify.checkout: clean`" is removed.
- **Record:** each environment gate's `VERIFY_RESULT.json` entry has `source: "export"` and `source_tree`.
- **Round-1 tests changed** (named in the commit body): `a_check_gate_that_adds_a_file_stops_the_deploy` →
  `a_check_gate_that_adds_a_file_is_not_deployed` (AC-2); `files_git_cannot_see_stop_the_deploy` →
  `files_git_cannot_see_are_not_deployed` (the gitlink and `.git` entry are listed, not deployed);
  `an_environment_gate_needs_a_clean_checkout` → `a_tracked_checkout_with_an_environment_gate_loads` (AC-7). The
  test helper `probe_then_dbsync`'s environment gate also writes what it sees (`gates/seen.txt`); its other output is
  unchanged.
- **Tests:** `verify::tests::the_export_holds_the_commit_not_the_checkout` (AC-1, also `export_changed` and
  `checkout_extra`), `a_check_gate_that_adds_a_file_is_not_deployed` (AC-2: under `deploy`, not the check gates'
  checkout, exactly the commit's files, no `.git`, the warning, `checkout_extra`, `source_tree`),
  `a_same_length_forgery_in_the_checkout_is_never_deployed` (AC-3, Windows), `a_low_integrity_check_gate_cannot_
  write_the_export` (AC-4, Windows, on the second verify run when `deploy` and `deploy.index` exist),
  `an_environment_gate_that_writes_into_the_checkout_mutates_the_tree` (AC-5), `a_check_gate_that_edits_a_tracked_
  file_stops_the_deploy` (AC-6, unchanged), `a_tracked_checkout_with_an_environment_gate_loads` and
  `two_tasks_share_one_environment_lock` (AC-7).
- **AC-3 on this machine:** Git for Windows **does** see the same-length forgery with restored timestamps (the
  task escalates `verify_mutated_tree` before any deploy) — so on Windows the test takes the "no deploy" branch; the
  other branch (Git fooled, as the reviewer showed on Linux with `core.trustctime=false`) is covered by AC-1: the
  export is made from the object store and never holds the checkout's bytes.
- Docs: the machine template (the paragraph and the script contract: the two variables), the fixture's comment,
  README, HUONG-DAN §5.9 and its troubleshooting row.
- **Open (not fixed, the list says so):** the Low verify checkouts of different tasks share one integrity label, so
  one task's check gates can write another task's checkout and build folder (a separate user or AppContainer per
  task would be needed). The export closes this for what an environment gate deploys from
  `TORYO_VERIFY_CHECKOUT`, not for `TORYO_VERIFY_BUILD`.

#### R10 — M18 script detection, second pass (applies to everyone)

- **Entry scripts (X7):** `checks::entry_scripts(_with)` → `Vec<Entry { path, refusable }>`. PowerShell: `-File x`;
  else every literal path with a separator and a script extension in the `-Command` / `-c` line (the rest of the
  arguments, joined; tokens split at whitespace and `; & | ( ) { }`); else the first positional `.ps1`. `cmd /c`
  (`/k`): the same in its command line, plus a bare first name (after an optional `call`) with a script extension
  when no program of that name is on PATH (`which`, injectable for the test). Table interpreters: the first argument
  that is not a flag or a flag's value — protected **also without a separator or an extension** (`sh check`),
  refusable only with one; a shell's `-c`, also inside a short-flag cluster (`-ec`, `-lc`, `-euxc`), gives the
  entries of its command line; new value flags `--env-file` (node), `--rcfile`, `--init-file` (bash). Folders, globs
  and variables are never scripts. `script_of` / `script_arg` stay (the first entry), so the M18 tests keep calling
  them.
- **Refusal** (`task::entry_point_notes`, called by `check_entry_points`): a refusable entry script missing at the
  base commit **and not ignored** (`git check-ignore --no-index -q`, run in the repository itself) → `verify_script_
  missing`. An ignored one (build output such as `dist/cli.js`) is admitted and stays protected (`verify_files`
  protects every entry script, whether or not it exists).
- **The net** (`script_like_args`): every other argument, the **value** of `KEY=value` / `--flag=value` /
  `-Flag:value` / `/p:Name=value` (not the whole argument), and the tokens of a command line, when it is a literal
  repository path with a separator and a script or program extension (output-flag values skipped). Never a
  refusal: protected when it exists; missing → `note: <gate|verify command> `x` passes `p`, which is not in the base
  commit — fine for build output; if the … runs it as its script, commit it first` at `task create` and as an Info
  line in `doctor` (repo checks, at `HEAD`; a refusal there is a WARN).
- **Decision (not in the list, conservative):** for `cmd`, a bare first name counts only with a script extension —
  `cmd /c echo hi` stays no script (the round-1/M11 tables pin it), `cmd /c x.cmd arg` is one.
- **Round-1 tests changed** (named in the commit body): `script_detection_skips_flag_values_and_finds_more_scripts` —
  `sh --posix later` now yields `later` (protected, not refusable; was `None`), and its net case: `bash -c
  "./ci/x.sh --fast"` is an entry script now (in `verify_entry_points`), no longer a net candidate;
  `script_detection_admits_flag_values_and_refuses_a_positional_script` — `dotnet tool run runner scripts/gen.ps1`
  missing at base is admitted with a note (it was refused by the net).
- **Deviation from AC-3:** an implementer that creates the missing `check` is stopped by the R35 protected-state
  snapshot — the task escalates `protected_config_changed: added check` and nothing is committed — not by the
  path-policy restore with a fix note the list describes (a new untracked protected file has always gone that way).
  The protection AC-3 is about holds; the test asserts what happens.
- **Tests:** `checks::tests::entry_scripts_refusals_and_protection` (AC-1: all 20 command lines — entries, refusable,
  net, protected; plus `cmd` with the name on PATH), `tests/m11_gates.rs::admission_refuses_only_entry_scripts_the_
  implementer_could_write` (AC-2, incl. the `doctor` note), `::an_extensionless_entry_script_is_admitted_and_
  protected` (AC-3). AC-4: the M11 M18 tests (`m18_script_arguments_per_program`, `m18_entry_points_are_protected_
  before_they_exist`, `a_gate_script_missing_from_the_base_commit_is_refused_at_admission`, EC-07) pass unchanged.
- Docs: README (the M18 paragraph rewritten with these rules and the remaining limits) and HUONG-DAN §5.9.

## Progress

Branch `m11/orgchart` (from `main` @ `aa0be03`). One commit per task.

- [x] A1 — repository files parsed without machine-only keys; tagged keys refused (C1)
- [x] A2 — machine floor for `verify.boundary` and `verify_root` (H2)
- [x] B1 — types for the org chart, role files and the new machine keys
- [x] B2 — loader: discover `.toryo/`, merge, freeze and hash; `toryo config show`
- [x] B3 — validation pass: cross-references, kinds, order, "did you mean"
- [x] C1 — layered instructions and `culture` in the context loader
- [x] C2 — the implementer's write fence (`writes` / `never`)
- [x] C3 — `checks.reviewer-differs-from-builder`: off / warn / error
- [x] C4 — split limits: `max_gate_retries`, `max_review_rounds`
- [x] E1 — check gates: mapping, gate variables, M18
- [x] E2 — verify output redacted with the task's secrets (M2)
- [x] E3 — environment gates: run, lock, record what is deployed
- [x] D1 — skill lookup (repo, then machine) and the menu
- [x] D2 — the plan's `skills` field and its checks
- [x] D3 — skills delivered through the context loader
- [x] F1 — templates
- [x] F2 — docs, migration guide, morning summary

## A — Trust floor

### A1 — repository files without machine-only keys; tagged keys refused (review C1)

- **`RepoFile`** (`src/config.rs`, private): the repository `toryo.yaml` as written —
  `deny_unknown_fields`, **no** `profiles` / `toolchains` fields. `Effective::load` parses the
  text into it and builds `RepoConfig` from it (`profiles` empty until the machine config is
  merged). `RepoConfig` keeps its shape, so the canonical JSON and every hash are unchanged
  (`templates_keep_their_m10_hash` pins the three shipped templates to the values computed on
  `main` @ `aa0be03`).
- **Tags.** `config::untagged` takes the tags off a YAML value; `machine_only_part` looks the
  machine-only keys up through it, so `!x profiles:` gets the usual R60 refusal (names
  `profiles:`, the machine config and the block to move). `config::refuse_tags(doc, file)` is
  the one shared walk: every tagged top-level key or value of a repository file, all in one
  error naming the file and each key. The org chart and the role files (B1) use the same two
  functions.
- Regression tests next to the R60 tests: `a_tagged_profiles_key_is_refused` (the review's
  reproduction, plus `!x roles` / `policy: !y` / `!t toolchains`) and
  `the_repository_file_has_no_profile_fields`.

### A2 — a machine floor for the verify boundary (review H2)

- **Machine key** `verify: { min_boundary: low_integrity | none, verify_root: <path> }`
  (`config::MachineVerify`, `deny_unknown_fields`, both optional). Merged into
  `Effective.machine_verify`, serialised into the canonical JSON as `machine_verify` **only when
  one of the two is set** — a configuration without it keeps its hash (the pinned hashes of
  `configs_without_toolchains_keep_their_hash` and `templates_keep_their_m10_hash` hold).
- **Floor.** `engine::verify::below_floor(cfg)`: repository `verify.boundary: none` with a floor of
  `low_integrity` (the default) is refused at `task create` — before the worktree, the run store
  or anything else is created — and at `task config --reload` (a reload must not go below the
  floor either; not in the task list, the conservative choice). The message
  (`verify_boundary_below_floor: …`) names the repository file and the machine config, gives the
  two ways out (keep `low_integrity` in the repository, or the owner sets `min_boundary: none` on
  this machine) and says that environment gates are the only sanctioned way to run something with
  the owner's full rights. With `min_boundary: none` the run is unchanged: every
  `VERIFY_RESULT.json` still says `none (trusted-code mode …)`.
- **`task create --trusted-code` was not added**: the machine key is enough for M11 (the task list
  says so); a per-task switch can come later.
- **`verify_root`.** The machine's replaces the repository's (`verify::verify_root`). Origins:
  `Origins.verify` (`boundary`, `verify_root`, `min_boundary`); `doctor` prints one `info` line per
  verify setting with its source (`verify verify_root … — source: the repository config,
  overridden by the machine config`), also in `--json` (`source`). `config show` (B2) reuses it.
  A reload that changes the machine `verify:` counts as a verify-configuration change (the verify
  result is invalidated, like a change of `verify.*`).
- **Who is affected.** Every repository with `verify.boundary: none` — on Linux, where Low IL does
  not exist, that is every repository — now needs `verify: { min_boundary: none }` in the machine
  config. This is the point of H2, and the message says what to do.
- **Test fixture.** `tests/common` writes `MACHINE_FLOOR` (`verify: { min_boundary: none }`) into the
  fixture's machine config, because the fixture repository verifies with `boundary: none`; the four
  tests that rewrite the machine config (`engine_policy`, `m10_events`, `m10_notify`, `m10_usage`)
  append it too. No assertion changed. New: `config::tests::the_machine_verify_floor_and_root`,
  `tests/m11_trust.rs` (AC-1, AC-2).

### Group A review (independent subagent) — follow-up commit

- Fixed: `doctor`'s verify-root check and `spike lowil-canary` used the repository's
  `verify_root`; both now use the root in force (`verify::verify_root_setting`: the machine's, else
  the repository's), so the Low-label evidence is gathered where the tasks will run. `doctor`'s
  `boundary: none` warning says when `task create` will refuse it. Tests:
  `doctor::tests::the_verify_root_check_uses_the_machine_root`,
  `config::tests::a_tagged_document_is_refused`.
- Not changed (recorded): a task created **before** M11 with `boundary: none` keeps verifying that
  way — its configuration is frozen (R35) and `toryo run` never re-reads the machine floor. The
  floor applies at `task create` and `task config --reload`. `verify_root` is shown as "repo,
  overridden by machine" also when the repository only had the default.

## B — The `.toryo/` loader

### B1 — types for the org chart, role files and the new machine keys

- **New module `src/team.rs`** (`config.rs` keeps `Effective`): `OrgChart`, `Seats` (a struct with
  exactly `planner`, `implementer`, `reviewer` — a fourth or misspelt seat is an unknown field, a
  missing one a missing field), `Seat`, `GateDecl` (`kind: check | environment` = `GateType`, the
  name `GateKind` is taken by the approval gates of the state machine), `IfMissing`, `Limits`,
  `Checks` / `Level` (`off | warn | error`, default `warn`; YAML key
  `reviewer-differs-from-builder`), `RoleFile`. All `deny_unknown_fields`, mirroring the drafts.
- **Parsing** (`team::parse_repo_file`): YAML → `refuse_tags` (C1, the A1 function) → the removed
  keys → the typed parse. **Removed keys** get their own message, all of a file at once, each
  with its key path: `flow` (the flow is fixed; move the retry knobs to `limits:`), `skills_dir`
  in the org chart (belongs in the machine config), `seats.<seat>.agent` (name a Toryo profile:
  `profile: codex-openai`), `reads` in a role file (Toryo decides what each stage sees).
- **Durations** (`team::parse_duration`): `<n>s | <n>m | <n>h`, n ≥ 1, ASCII digits only;
  anything else (`0s`, `20`, `20min`, `1.5h`, `1d`, an overflow) is an error naming the value.
- **`CommandSpec.timeout_sec`** (the choice the task list asks to record): a new optional field in
  seconds, serialised only when set, winning over `timeout_min` in `xpp::command_timeout`. Exact
  (`30s` stays 30 s, no rounding to minutes) and every existing hash is unchanged.
- **Machine config**: `seats:` (the new spelling of `roles:`, same `Role` fields; both in one file
  is an error naming the file; `seats:` is then read exactly like `roles:`, so it hashes the
  same), `skills_dir: Option<String>`, `environment_gates: { <name>: EnvGate }` with
  `EnvGate { program, args, lock, env, secret_env }` (`secret_env` values are the existing
  `credman:` / `env:` references). None of them is frozen or hashed in `toryo.yaml`-only mode.
- **Fixtures** `tests/fixtures/m11/`: the three owner drafts, copied with their comments; in the
  machine draft the owner paths (`C:\Users\<you>\…`, `C:/Temp/AgentShare/skills`,
  `C:\.thien\toryo-gates\dbsync.ps1`) are `'@TMP@/…'`, which the B2 tests replace with a temp
  folder. Tests: `team::tests::{the_owner_drafts_parse, removed_keys_have_their_own_message,
  seats_are_exactly_three, durations}`, `config::tests::machine_seats_are_the_new_spelling_of_roles`.

### B2 — the loader: discover `.toryo/`, merge, freeze and hash; `toryo config show`

- **Discovery** (`team::org_chart_root`): after `toryo.yaml` is parsed, `<project.root>/.toryo/
  toryo-orgchart.yaml` (with `project.root` expanded; a root that does not expand is `toryo.yaml`-only
  mode, as before). Read from the repository checkout, never a task worktree. Note: `task create --repo
  X` does not change where the org chart is read from (it is a property of the configuration, like
  `toryo.yaml` itself).
- **Merge** (`team::load` → `team::Loaded`, applied in `Effective::load`): the seats become
  `repo.roles` (an omitted `fallback` is `Some([])` — a chain of one); the implementer's `before_handoff`
  check gates become `repo.commands.verify`, in that order (`team::gate_command`, `timeout_sec` from
  the gate's `timeout`); a lone `limits.max_fix_cycles` becomes `repo.pipeline.max_fix_cycles`. So the
  engine reads the same fields as in M10. Machine `seats:` / `roles:` replace repository seats
  afterwards (`RepoOverriddenByMachine`). `verify_commands` never falls back to `scripts/build.ps1` in
  org chart mode.
- **Freeze and hash**: `Effective.team: Option<team::Team>` — team name, culture path, every gate
  (declaration + the machine's `EnvGate` for an environment gate, so the command that will run with
  full rights is the one the task was created with), limits, checks, each role file's parsed content
  (purpose, instructions path relative to the repo root, skills, writes, never, before_handoff) and
  the skill roots (`.toryo/skills`, the machine's `skills_dir`). Serialised as `team` only in org
  chart mode — M10 configurations keep their hash (`templates_keep_their_m10_hash`). `from_frozen`
  reads `team` back without touching `.toryo/` (tested by deleting `.toryo/` before the reload).
- **Errors** (all at once, with the other validation problems): `roles:`, `commands:` or
  `pipeline.max_fix_cycles` in `toryo.yaml` (each names both files and where the setting lives now);
  a missing role file; a role file that does not parse; a leftover `.toryo/roles/<role>.md` ("move it
  to `.toryo/roles/<role>/instructions.md`"); a non-empty `fallback` on the planner or reviewer, in the
  org chart or from a machine seat override (an explicit `fallback: []` is accepted — it says
  nothing); `max_fix_cycles` together with a split limit; a bad `timeout`; `toryo:` other than 1. The
  org chart itself must parse (YAML, tags, removed keys, serde) before anything else is reported.
- **Origins**: `Origins.gates` (`repo` = a check gate or an environment gate this machine does not
  define; `machine` = an environment gate whose command is the machine's) and `Origins.skills_dir`.
  `Effective.files`: every file read (toryo.yaml, machine config, org chart, role files) with its
  SHA-256 — not hashed, empty for a frozen configuration.
- **`toryo config show [--json]`** (`src/config_show.rs`, new top-level `config` command): the text
  form and the JSON document (schema 1, in README "The effective configuration (M11)"). In
  `toryo.yaml`-only mode the verify commands are listed as `kind: check` gates.
- Tests: `team::tests::{the_draft_set_loads_freezes_and_reloads, a_setting_in_both_places_is_refused,
  fallbacks_overrides_and_leftover_role_files}` (AC-1, AC-3) and `tests/m11_orgchart.rs` (`config show`
  through the CLI, a task frozen and reloaded with the same digest, a fake run to the final gate —
  AC-1, AC-4; `toryo.yaml only` without an org chart). `tests/common::setup_orgchart` builds the
  fixture repository in org chart mode for the later groups.

### B3 — validation pass: cross-references, kinds, order, "did you mean"

- **`team::problems(cfg, root, machine)`** — a pure pass over the loaded configuration, called from
  `Effective::validate_with` in org chart mode, so its problems are reported **with** the loader's
  (B2) and `config::problems`, all in one `invalid configuration:` error. Every message starts with
  the file and the key path (`…\toryo-orgchart.yaml: seats.implementer.profile: …`,
  `…\roles\implementer\toryo-role.yaml: before_handoff: …`, `<machine config>:
  environment_gates.dbsync.args: …`). Rules, as listed in the task: instructions files exist (and
  stay inside the repository); seat `profile`/`fallback` names exist in the machine profiles (the
  message points at the machine config, R60 — in org chart mode this replaces the M10 one-at-a-time
  `role … refers to unknown profile` bail); `before_handoff` names a declared gate; only the
  implementer has `before_handoff`, `writes`, `never`; a check gate is exactly one of `program` and
  `tool` (the same `verify_command_problems` as `commands.verify[]`, now `pub(crate)` with a full
  label, so `tool: xpp` needs `metadata`, `projects` and the toolchain as today); an environment gate
  has none of `program`/`args`/`tool`/`metadata`/`projects`; `if_missing` only on environment gates;
  every check gate before every environment gate; an environment gate the machine does not define is
  refused unless `if_missing: skip` (naming `environment_gates.<name>`); a machine `EnvGate` has a
  `lock`, a program that is absolute or a bare PATH name, and every path-like argument absolute and
  not inside `project.root`, `project.worktrees_root` or the verify root (also an absolute program
  inside them is refused); the culture file exists. Skills are added in D1.
- **Added (conservative, not in the list):** the implementer's `before_handoff` may not be empty —
  M10 refused a configuration without verify commands, and org chart mode would otherwise review
  unverified work. Every declared environment gate is checked against the machine, also one that no
  `before_handoff` names.
- In org chart mode `config::problems` skips `commands.verify[]` (they are the check gates, checked
  as `gates.<name>`).
- **"Did you mean"** (`team::did_you_mean`, ~20 lines of Levenshtein, no new crate): a single
  case-insensitive match, else the single candidate within edit distance 2; a tie or nothing close
  gives no hint. Used for unknown seats, gates, profiles (and skills in D1), for the unknown role
  keys of `toryo.yaml` (`roles.reviwer: unknown role (valid: …) Did you mean 'reviewer'?` — the old
  text is kept, the hint appended), and in **`team::friendly_serde`**, which rewrites serde's
  `unknown field `x`, expected one of …` / `missing field` into `<path>.<key>: unknown key 'x'. Did
  you mean 'y'? (valid: …)` (`seat 'reviwer' not found. Did you mean 'reviewer'?` under `seats`) for
  the org chart, the role files and the machine config. The repository files are now typed-parsed
  from their text (not from the YAML value) so serde reports the key path and line.
- Tests (`team::tests`): `did_you_mean_suggests_only_one_close_candidate` (AC-3),
  `unknown_keys_name_the_path_and_the_hint`, `a_seat_profile_must_exist_on_the_machine`,
  `before_handoff_names_order_and_owner`, `gate_kinds_and_their_fields`,
  `environment_gates_on_this_machine`, `culture_and_instructions_must_exist` (AC-1, one per rule),
  `three_problems_are_reported_at_once` (AC-2, four problems from three files in one error).

### Group B review (independent subagent) — follow-up commit

- **Fixed:** a machine seat override without `fallback` fell back to the planner's profile in org
  chart mode (`task::roles` maps `None` to the planner) — now `Some([])` like an org chart seat (D7).
- **Fixed:** the "out of the agents' reach" check compared strings only. It now compares a
  lexical form (`\\?\` prefix dropped, `.`/`..` resolved, Windows' trailing dots and spaces
  dropped, case folded) of the canonicalised path when it exists (junctions, 8.3 names); a `..`
  segment in a machine gate's program or argument is refused outright; the path in a
  `--name=value` / `-Name:value` argument is checked too; a Windows switch (`/c`) is not a path.
  **Limit (documented in the script contract, F1/F2):** a bare-name argument (`cmd /c build`,
  `python -m deploy`) is resolved by the program against its working directory — the verify
  checkout — so the script contract says to name scripts by absolute path. The program itself is
  resolved to an absolute path through PATH before it starts (E3), never from the checkout.
- **Fixed:** `before_handoff` now needs at least one **check** gate (environment gates alone, or
  skipped on a laptop, would hand unverified work to review).
- **Fixed:** an absolute `culture:` / `instructions:` path is refused as written ("must be
  relative") instead of being normalised into a relative one.
- Tests added for every loader problem the review listed (missing role file, version ≠ 1,
  `max_fix_cycles` + split, bad timeout, a check gate with no command, absolute paths, the
  friendly missing-seat message, the machine override fallback, the reach cases).
- **Deviations from "byte for byte as in M10" (error texts only, valid configurations and hashes
  unchanged):** an unknown or missing key in **any** machine config now gets the friendly form
  (`<file>: <path>.<key>: unknown key 'x'. Did you mean …? (valid: …) (line N)` — serde's column is
  dropped); `roles.<x>: unknown role (valid: …)` gets a `Did you mean` suffix when one role is
  close. Both are B3 behaviour; existing tests assert with `contains` and pass unchanged.
- Not changed (recorded): `templates_keep_their_m10_hash` reads the dogfood template, whose
  `project.root` is this repository — a `.toryo/toryo-orgchart.yaml` at the root of the toryo
  repository would switch it to org chart mode. `instructions: ../planner/x.md` (inside the
  repository but outside the role folder) is refused — conservative. A misspelt machine seat
  (`seats: { reviwer: … }`) is reported as `roles.reviwer: unknown role … Did you mean 'reviewer'?`
  without naming the machine file. Unknown enum values (`kind: chek`) keep serde's message.

## C — Roles

### C1 — layered instructions and `culture` in the context loader

- `context::Request.team: Option<TeamContext>` (org chart mode only, built by `context::request`
  from the frozen `team` section): the culture file and the role's `instructions.md`, both in the
  **repository checkout** (`tasks.repo`), like sources 2–4.
- **Source 3** ends with the culture (`culture (CULTURE.md)`, heading `Team culture (…)`), never
  trimmed. **Source 4** is two parts, both never trimmed and hashed: the embedded role template
  (`contracts::role`) **always**, then `instructions.md` under `Role instructions (team)` (label
  `.toryo/roles/<role>/instructions.md`). A repository adds to the built-in rules (output contract,
  no Git, untrusted data) and can no longer remove them: in org chart mode a
  `.toryo/roles/<role>.md` is never read (B2 refuses it at load; the loader ignores it at attempt
  time too). **This closes the M4 gap** where a repository file replaced the embedded template —
  legacy mode keeps that behaviour unchanged (`repo_role_template_overrides_the_embedded_one`
  passes unchanged).
- A culture or instructions file that is gone at attempt time is skipped and reported (`skipped`),
  like the other sources; the task was validated at `task create`.
- Org chart paths in messages use the platform separators (`team::org_chart_path`).
- Tests: `context::tests::org_chart_mode_layers_the_role_instructions_after_the_embedded_rules`
  (each role; the legacy file present and ignored; never trimmed under a 1 KB budget),
  `instructions_and_culture_are_hashed` (AC-2), `tests/m11_orgchart.rs::
  context_show_layers_the_team_instructions_after_the_embedded_template` (AC-1 through the CLI).

### C2 — the implementer's write fence (`writes` / `never`)

- `checks::Protected` gains an optional fence (`with_fence(writes, never)`, built by
  `runner::task_policy` from the frozen implementer role in org chart mode). `fence_rule(path)`:
  `Never` when a `never` glob matches (never wins), `Outside` when `writes` lists anything and no
  glob matches, `None` when the task's `## Allow protected` lists the path (it opens fence entries
  like protected paths; `deny_paths` still cannot be opened). Same matcher as the other rules
  (`glob_ci`: case-insensitive, `/` and `\` alike, `dir/**` also matches `dir`). `violates()` adds
  the fence — it only **adds** violations — while `matches()` (the R35 snapshot) is unchanged.
- **Decision:** an empty `writes` means "no allowlist" (only `never` applies), not "nothing may be
  written" — a role file without `writes` must not fence everything.
- **Plan time:** a `files_likely_involved` entry outside the fence joins the automatic re-plan
  (`Ev::PlanPolicyViolation`, bounded by `MAX_PLAN_AUTO_REJECTIONS`); the feedback names each path
  and its rule: ``"`docs/x.md` — outside the implementer's write fence (`writes: [src/**]`)"`` /
  ``"… — a `never` path (`never: [**/Descriptor/*.xml]`)"``, then any protected or denied paths.
- **After each build turn:** a changed path outside the fence is restored and sent back like a
  denied path (one fix cycle, as M10); the fix note names the rule (``"`…/Model.xml` (touched a never
  path (`never: [**/Descriptor/*.xml]`))"``). Crash recovery uses the same `violates()`.
- When no fence rule is broken, the M10 texts (stderr and feedback) are used unchanged.
- No fence for planner and reviewer (read-only already) and none in legacy mode.
- Tests: `checks::tests::the_write_fence` (AC-3 case and `\`, AC-4 protected/denied inside
  `writes`, allow-protected), `runner::tests::fence_feedback_names_the_rule`,
  `tests/m11_roles.rs` — `a_plan_outside_the_write_fence_is_replanned` (AC-1),
  `a_never_path_is_restored_unless_the_task_allows_it` (AC-2, also with `## Allow protected`).

### C3 — `checks.reviewer-differs-from-builder`: off / warn / error

- `runner::implementer_profiles(store, t)` — the profiles of the **succeeded** implement/fix
  attempts (the profile that actually ran, after any fallback), factored out of
  `final_gate_notes`; `same_vendor` compares their profile `kind` with the reviewer's.
- `warn` (the default, and the only behaviour in legacy mode): today's "same vendor" note at the
  final gate — **not** byte-identical, corrected in M11-R7: `main` printed `("Claude" both)`, M11 prints `(Claude
  both)` (X5 keeps it; `the_legacy_same_vendor_note_is_exact` pins the line), and since R7 a failed or interrupted
  implementer turn counts too. `off`: no note. `error`: before a **review** turn starts (after the
  EC-10 re-verify check, before admission and before the attempt begins), the task escalates with
  the typed class `same_vendor_review` and a message naming both profiles and the ways out
  (`approve --gate final --force` to review it yourself, or a reviewer of another kind via machine
  `seats:` + `task config --reload`, then `resume --note`). `resume --note` works as for any
  escalation (R5).
- **Note:** after `resume --note` the implementer index is sticky (R49), so the next review is
  refused again unless the reviewer seat changed — the message says so implicitly by offering the
  two ways out. A "skip the check once" switch was not added.
- Tests: `tests/m11_roles.rs::reviewer_differs_from_builder_off_warn_error` (AC-1: the
  quota fallback codex-openai → claude-max with a claude-max reviewer; `events.jsonl` has the
  `escalated` with `reason_class: same_vendor_review`; no review attempt) and
  `different_vendors_pass_at_every_level` (AC-2).

### C4 — split limits: `max_gate_retries`, `max_review_rounds`

- `state::Machine` gains `split: Option<SplitLimits { max_gate_retries, max_review_rounds,
  gate_retries, review_rounds }>`, `#[serde(default, skip_serializing_if = "Option::is_none")]` —
  a machine stored by M10 reads back unchanged and serialises to the same JSON
  (`an_m10_machine_reads_back_and_behaves_the_same`). `Machine::new(max)` is unchanged;
  `Machine::with_limits(gate, review)` sets the split bounds. `fix_cycle` still counts every entry
  into `Fixing` (it names the `fix-N` stages).
- Which counter (the task names two; decided for the others): `VerifyFailed` → gate retries;
  **`PathPolicyViolation`** (the implementer's own fence/policy violation) → gate retries too (a
  build-stage bounce); `ChangesRequested` → review rounds; **`FinalRejected`** (the human's final
  rejection) → review rounds too (a review-stage bounce; M10 also bounded it). `resume --note`
  still grants one more cycle without touching the counters, as in M10.
- The escalation names the limit: `2 gate retries used (limits.max_gate_retries) — not accepted
  (last: verify failed)`; the typed class (`escalated.reason_class`) is `max_gate_retries` /
  `max_review_rounds` instead of `max_fix_cycles`.
- `task::new_machine`: split limits when the org chart has either key — a missing one of the two
  is **2** (today's default); `--max-fix-cycles` together with split limits is **refused** (it sets
  the single counter; the conservative choice instead of silently overriding both). A lone
  `limits.max_fix_cycles` keeps the single counter.
- `status` prints `fix 1 (gate retries 1/1, review rounds 0/3)`; SUMMARY.md likewise;
  `status --json` / `task show --json` get `gate_retries`, `max_gate_retries`, `review_rounds`,
  `max_review_rounds` **only for a split-limit task** (a single-bound task's document is unchanged
  — so the README rule "a key is always present" does not hold for these four); `max_fix_cycles`
  is then their sum, for display.
- The pure state machine stays pure; `split_limits_keep_the_transition_table` runs the exhaustive
  table again with split limits. Tests: `gate_retries_are_bounded_on_their_own` (AC-1),
  `review_rounds_are_bounded_on_their_own` (AC-2), `an_m10_machine_reads_back_and_behaves_the_same`
  (AC-3), `tests/m11_roles.rs::split_limits_end_to_end` (escalation, `status`, `--json`, the class in
  `events.jsonl`, `--max-fix-cycles` refused, a single-bound task without the new keys).

## E — Gates

### E1 — check gates: mapping, gate variables, M18

- **Mapping** (`team::gate_command`, B2): a check gate is a `CommandSpec` — `program`/`args` or
  `tool: xpp` + `metadata`/`projects`, `timeout_sec` from `timeout` — in `commands.verify`, so it
  runs exactly like an M10 verify command (clean checkout of the checkpoint, the configured
  boundary, no secrets, `@verify-scripts` protection, the same `VERIFY_RESULT.json` entry).
- **Gate variables** (`verify::set_gate_vars`, called for every verify run, legacy commands too —
  new variables only): `TORYO_TASK`, `TORYO_SHA` (the checkpoint commit), `TORYO_VERIFY_CHECKOUT`
  (the clean checkout), next to `TORYO_VERIFY_BUILD`. `toryo xpp build` (outside a task) does not
  set them.
- **M18 (applies to everyone):** `checks::verify_files` protects a repository-relative program and
  the script after `-File` (PowerShell's `-File`/`-f`/`-fi`/`-fil`, case-insensitive) whether or
  not they exist yet. **Existing test changed, as M18 requires:** `verify_files_include_a_repo_program`
  (EC-07) now lists `scripts/missing.sh` (a program that does not exist yet) — before, it asserted
  it was not protected. **Admission:** `task create` refuses a task whose base commit lacks one of
  those entry points (`checks::verify_entry_points`), naming the gate (`verify command` in
  legacy mode): `verify_script_missing: gate `later` runs `scripts/later.ps1`, which is not in the
  base commit … — commit it before creating the task`. It runs before the worktree, the branch or
  the run store exists. Other arguments that name files are protected only when they exist, as
  before.
- Tests: `checks::tests::m18_entry_points_are_protected_before_they_exist`,
  `tests/m11_gates.rs` — `a_check_gate_gets_the_gate_variables` (AC-1: the script prints the four
  variables; `VERIFY_RESULT.json` shows the task id, `head`, `verify_checkout` and its `build`
  folder), `a_gate_script_missing_from_the_base_commit_is_refused_at_admission` (AC-2). The
  protected-path violation for an edit of an existing gate script is the M10 EC-07 path
  (`regress_policy.rs`), unchanged.

### E2 — verify output is redacted with the task's secrets (review M2)

- `runner::RunSpec` gains `redact: Redactions`; `LocalRunner` spawns with it instead of
  `Redactions::empty()`. The engine's verify runs (`VerifyAt`) use `flight::task_redactions(cfg, t)`
  — every secret of the task's profiles, plus `CLAUDE_CODE_OAUTH_TOKEN` from the parent environment;
  E3 adds an environment gate's own `secret_env` values. So `VERIFY_RESULT.json`, the fix prompt,
  the reviewer's pack and anything derived from the output only ever see `[REDACTED:<name>]`.
- The output is collected whole (the stream redactor flushes its held-back tail at EOF), so the
  H5 newline pitfall does not apply here: verify output is not parsed line by line while the
  command runs. Output without a secret is byte-identical (an empty set is the identity; a set
  whose values do not occur changes nothing — tested).
- **The evidence run** (`evidence::run_version`, the R51 `--version` probe of `doctor` and the run
  gate) takes a redaction set too: the profile's own resolved secrets
  (`evidence::profile_redactions`; one that cannot be resolved is left out). Its environment holds
  no secrets, so this is belt and braces.
- `toryo xpp build` (outside a task) and `spike pipeline` keep an empty set (no task, no secrets).
- Not done (not in the task list): scanning `DIFF.patch` for known secret values (review M2's last
  sentence).
- Tests (`tests/m11_gates.rs`): `verify_output_is_redacted_with_the_task_secrets` (AC-1: a verify
  command prints the implementer profile's secret; it fails once, so there is a fix prompt, then
  passes, so there is a reviewer pack; every file of the run store and stderr are free of it, the
  tail shows `[REDACTED:TORYO_M11_TOKEN]`), `output_without_secrets_is_unchanged_by_redaction`
  (AC-2).

### E3 — environment gates: run, lock, record what is deployed

- **When** (`runner::run_environment_gates`): inside `Verifying`, after **every** check gate of the
  same run passed, in the implementer's `before_handoff` order, stopping at the first failure. A
  failing check gate means no environment gate runs.
- **How**: the **frozen** machine command (`team.gates.<name>.machine`, B2 — the command that runs
  with full rights is the one the task was created with), `Boundary::None` (the owner's normal
  token; the check gates' boundary is unchanged), working directory = the verify checkout, the
  Job Object and timeout of `LocalRunner` (`timeout:` of the gate), environment from scratch (R23,
  `process::env::build_env`) + the gate variables (E1) + `env` + resolved `secret_env`
  (`config::resolve_secret`). The output is redacted with the task's secrets **plus** the gate's
  own (`flight::task_secrets` is new, `task_redactions` builds on it). **The program is resolved
  before it starts** (`env_gate_program`): an absolute path as written, a bare name through PATH —
  never looked up in the verify checkout. A secret that cannot be resolved, or a program not on
  PATH, escalates with the class `environment_gate_unavailable` (not something a fix cycle can
  repair). A failure goes back to the implementer as untrusted data (R61, the same
  `untrusted::wrap("verify", …)` as check gates) and counts against `max_gate_retries` (C4).
- **Lock** (`lock::EnvLock`): `<data_dir>\locks\env-<lock>.lock`, an exclusive OS file lock like
  `TaskLock`, the holder's label written inside, released explicitly on drop (review L9). While
  another task holds it: one `lock_waiting { lock, holder }` event, a row in the new table
  `environment_waits` so `status` shows `waiting for lock d365-env (held by T-12)`, a try about
  every 2 s (`TORYO_TEST_ENV_LOCK_POLL_MS` in tests), and `toryo stop` ends the wait at once (the
  verify attempt is `interrupted`, as for any stop). **Deviation:** Windows does not let another
  process read a locked file, so the holder is named from `environment_deployments` (the row with
  no `finished_at` for that lock), not from the lock file. Lock names are letters, digits, `-`,
  `_`, `.` (B3 refuses others).
- **Record** (new table `environment_deployments(lock PRIMARY KEY, task, sha, gate, machine,
  started_at, finished_at, ok)` — `gate` and `machine` added to the listed columns): written
  **before** the gate starts, updated at its end (`ok` empty for a stopped gate). Never deleted
  (cancel, `task cleanup`). Shown in `toryo status` (`environment    d365-env holds BR-001 @
  1a2b3c4d5e6f (not approved yet)` — all locks; `status ID` / `task show ID`: the locks that task
  deployed to), in both `--json` documents (`environments[]`, **only when there is a row**, so an
  M10 document is unchanged) and in the final gate notes. Status words (`views::deployment_status`):
  `deploying now` (the holder's run is alive), `interrupted — the environment may hold a partial
  deploy`, `the deploy failed`, `approved` (the holder is `Completed`), `not approved — the task
  was cancelled`, `not approved yet`.
- **Result**: one `VERIFY_RESULT.json` entry per environment gate: `name`, `kind: environment`,
  `lock`, `machine` (`COMPUTERNAME`, else `HOSTNAME`), `argv` (the frozen program and args — the
  secrets are environment variables, never in argv), `exit`, `timed_out`, `ms`, `output_tail`.
  `if_missing: skip` and not defined here: `{name, kind: environment, skipped: true, reason: "not
  defined on this machine"}` and the final-gate note `dbsync did not run on this machine …`. The
  R47 mutation check runs after all gates (`verify_mutated_tree` when a gate changed a tracked file
  of the checkout).
- **Final gate wording**: with an X++ compile and environment gates that passed, `verify: compile +
  dbsync on <machine>` replaces `COMPILE_ONLY`; without X++, `verify: dbsync on <machine>
  (environment gate)`; `COMPILE_ONLY` stays when none ran. Plus `environment: <the deployment
  line>` for each lock this task deployed to.
- **Events**: `lock_waiting`, `environment_deployed { lock, sha, ok }`; `verify_command` gets an
  optional `kind` serialised only for environment gates (M10 event shapes unchanged — tested). The
  flight tests' exhaustive variant list is extended (14 variants).
- **Recovery** needs nothing special: a killed toryo leaves the attempt `running` and the row
  without `finished_at`; `resume` re-runs `Verifying` from the check gates. Crash test: the real
  kill of a `toryo run` during the gate (Windows: the Job Object kills the script with it).
- The script contract (owner draft `toryo-local.yaml`) is documented in F1/F2. Added to it:
  name scripts by absolute path (a bare-name argument is resolved by the program against its
  working directory, the verify checkout).
- Tests (`tests/m11_env_gates.rs`, scripts `pwsh` on Windows / `sh` elsewhere in `<root>/gates/`):
  `an_environment_gate_runs_after_the_checks_and_its_exit_code_decides` (AC-1, AC-4 with the Claude
  implementer's `<untrusted_data>` block and `gate_retries`, AC-6 in every view, after `task
  cleanup`), `cancelling_keeps_the_deployment_record` (AC-6), `an_environment_gate_has_the_normal_
  token_a_check_gate_low_integrity` (AC-2, `#[cfg(windows)]`), `two_tasks_share_one_environment_lock`
  (AC-3, with `toryo stop` while waiting and a later `resume`), `an_environment_gate_secret_is_
  redacted_everywhere` (AC-5), `a_skipped_environment_gate_is_recorded_and_noted` (AC-7; the
  `error` case is B3's), `an_environment_gate_that_writes_into_the_checkout_mutates_the_tree`
  (AC-8), `a_crash_during_the_environment_gate_is_recovered_by_resume` (AC-9);
  `views::tests::deployment_status_in_words`, the extended flight tests.
- Also in this commit: the group C review nit (a doc comment had moved off `ImplementTurn`).

### Group E review (independent subagent) — follow-up commit

- **Fixed (M18):** the entry point of a verify command is now its **script**, per program
  (`checks::script_arg`): the argument after the first `-File` of `pwsh`/`powershell` only (a
  script's own `-f x` after it is not), the first non-flag argument of an interpreter (`sh`,
  `bash`, `zsh`, `dash`, `ksh`, `python`, `python3`, `py`, `node`, `ruby`, `perl`; none when `-c`,
  `-m`, `-e`… runs inline code or a module), the file after `cmd /c`. Before, `-f` counted for
  every program (`dotnet test -f net8.0` would have been refused) and `sh scripts/x.sh` was not
  protected before it existed. The admission check also runs at **`task config --reload`**
  (`task::check_entry_points`, shared).
- **Fixed (E3):** an environment gate's environment gets `NoDefaultCurrentDirectoryInExePath=1`, so
  cmd.exe and `CreateProcess` in the owner's script never run a bare-named program from the verify
  checkout. **Not fixable here (script contract):** DLL loading still searches the working
  directory — the contract says to call tools by absolute path.
- **Fixed:** environment lock files live in `locks/env/<lock>.lock` (lower case), so a task id like
  `env-d365-env` can never take a gate's lock; a gate program that does not exist (or a bare name
  not on PATH) escalates with `environment_gate_unavailable` before any record is written; a gate
  that fails to start marks its record `ok: false` instead of "interrupted"; `which` results are
  made absolute; `status` shows "waiting for lock" only while the task's run is alive (a crash while
  waiting leaves the `environment_waits` row behind).
- **Fixed (E2 AC-2):** `LocalRunner` collects the raw streams and redacts the whole output at the
  end — a per-stream redactor held bytes back and could move a short stdout line after later
  stderr output whenever any secret was configured. Live chunks (no engine caller) are redacted
  chunk by chunk.
- **Fixed:** `task config --reload` treats a change of the `team` section (gates, their machine
  commands, the role files) as a verify-configuration change (the verify result is invalidated).
- **Known limit (not fixed, Unix only):** on Unix a killed toryo leaves the gate's process group
  running while the lock is free (review M7: the process group is not a containment boundary);
  Windows kills the Job with toryo.
- Lock names that differ only in case share one lock file but get separate `environment_deployments`
  rows.
- Tests added: `checks::tests::m18_script_arguments_per_program`, `lock::tests::environment_locks_
  have_their_own_folder`, `runner::tests::the_verify_scope_at_the_final_gate`,
  `m11_env_gates::{an_environment_gate_that_cannot_run_escalates,
  a_stop_during_an_environment_gate_interrupts_it}`,
  `m11_gates::a_configured_secret_does_not_reorder_stdout_and_stderr`.

### Group C review (independent subagent)

- Nothing serious. Fixed: the misplaced doc comment. Recorded, not changed: after a
  `same_vendor_review` escalation `resume --note` costs one more implementer turn before the
  review is refused again (see C3); fence violations found by crash recovery are worded "protected
  paths" and escalate (as M10 does for protected paths); ignored files outside `writes` are not seen
  (never committed; same as `deny_paths` in M10); after `resume --note` with split limits the
  escalation text counts the limit, not the cycles run (`2 gate retries used` after 3 cycles), and
  says `1 review rounds`.

## D — Skills

### D1 — skill lookup and the menu

- `team::resolve_skill(name, repo, machine_dir)`: first match wins —
  `<project.root>/.toryo/skills/<name>/SKILL.md`, then `<skills_dir>/<name>/SKILL.md` (the frozen
  `team.skill_roots.machine`, `%VAR%` expanded). Not found: a message naming **both** paths (or
  "no `skills_dir` in the machine config"). Each resolved skill is `{ name, origin: repo | machine,
  path, sha256, bytes }` plus its `description` and text (CRLF folded to LF, like the other context
  sources). A name must be a folder name — no `/`, `\`, `:`, `..` (a path in a name could otherwise
  escape both folders).
- **Front matter** (`team::front_matter` / `skill_description`): the YAML mapping between the first
  two `---` lines; a UTF-8 BOM and CRLF are accepted; no opening `---` on the first line, an
  unclosed block or a non-mapping is "no front matter".
- **Validation (B3 pass):** every skill of every role file resolves (with a "did you mean" over the
  skill folders found in both places); a skill on the **implementer's** menu needs a
  `description` (the planner chooses by it). Planner and reviewer skills need none.
- **Only `SKILL.md` is delivered** (D3). Files next to it (scripts, examples) are readable by the
  agents **only for repository skills** — they are in the worktree; a machine skill's other files
  are outside every worktree (and Codex's sandbox). README says so (F2).
- The draft fixture gets the two skills of the draft role file: `xpp-axclass` in the repository,
  `xpp-batch` in the machine folder.
- Tests: `team::tests::skills_resolve_repository_first` (AC-1), `skill_front_matter` (AC-2),
  `skills_are_validated_at_load`.

### D2 — the plan's `skills` field and its checks

- `schemas/plan.schema.json`: optional `"skills": { "type": "array", "items": { "type": "string" } }`
  (not in `required`; inside `contracts::KEYWORDS`). **Applies to every plan schema the agents get**,
  also in legacy mode — an optional property; plans without it stay valid and render as before.
- **Menu** (`context::skill_menu`): when the implementer's role has skills, the **planner's**
  context gets a never-trimmed part `implementer skill menu` (heading `Skills available to the
  implementer`): an instruction to list the needed ones in `skills`, then one `- name — description`
  line per skill, read per attempt (a skill gone since `task create` shows `(not found on this
  machine)`). None in legacy mode or with an empty menu.
- **Checks after a valid plan** (`Engine::plan_skills_problem`, after the path/fence check): every
  name must be on the implementer's menu — else the bounded automatic re-plan with feedback listing
  the menu and a "did you mean" — and the chosen SKILL.md files plus the implementer's never-trimmed
  parts (`context::Assembled.fixed_bytes`, new: policy, culture, role template, instructions, menu,
  skills) must fit `context.budget_kb.implementer` — else "Choose fewer skills". 
- **Event:** `Ev::PlanPolicyViolation` gains `reason: Option<String>` (serialised only when set, so
  the stored M10 shape is unchanged); `paths` is empty for a skills problem; at the bound the
  escalation says `plan_skills: … — still, after 2 automatic re-plans` (class `plan_policy`, as for
  paths). The exhaustive state-machine tests construct it with `reason: None` (extended, not
  weakened).
- `PLAN.md` shows `Skills for the implementer: a, b` under "Files likely involved" when the plan
  has skills (the plan digest already covers `plan.json`).
- Tests: `render::tests::a_plan_names_the_implementers_skills` (rendering and schema, AC-1/AC-4),
  `tests/m11_skills.rs` — `a_plan_chooses_skills_from_the_menu` (AC-1, the menu in the planner's
  context), `a_skill_off_the_menu_is_replanned_then_escalates` (AC-2), `skills_over_the_budget_are_
  replanned` (AC-3), `a_plan_without_skills_is_valid` (AC-4).

### D3 — skills delivered through the context loader

- `context::TeamContext.skills` (resolved by `context::request` **per attempt**, from the repository
  checkout and the machine folder): one part per skill right after the role part (C1) and the
  menu (D2) — label `skill <name> (<origin>)`, heading `Skill: <name> (<origin>)`, the SKILL.md text,
  **never trimmed**, hashed (into `context_sha256`, and each source's own `sha256` in the
  `context_assembled` event). `context show` prints each skill line with its `sha256` (only skill
  lines get the suffix — legacy output is unchanged).
- Which skills: the implementer (implement and every fix stage) → the **approved plan's** `skills`
  (the head of `plan`, filtered to the menu); planner and reviewer → every skill of their role file.
  A skill that no longer resolves at attempt time is reported under `skipped`, not an error.
- Skills are trusted content (protected `.toryo/` of the repository checkout, or the machine): for
  the Claude implementer they are in the context file (its system prompt), never in the
  `<untrusted_data>` user prompt (tested).
- **Over budget at attempt time** (found out, kept): exactly what the loader does today when the
  policy and the role template alone exceed the budget — the trimmable sources are trimmed first,
  then a warning `context budget: N bytes still exceed K KB — policy files and the role template are
  never trimmed` (wording unchanged; it now also covers culture, instructions, menu and skills), and
  the attempt runs with the full context. The plan-time check (D2) is what keeps a plan's choice
  inside the budget; it now measures the implementer's context **with** the chosen skills
  (`fixed_bytes`).
- The planner's menu label is `implementer skill menu` (so only skill parts start with `skill `).
- Tests (`tests/m11_skills.rs`): `context_show_lists_the_skills_of_each_role` (AC-1: the planned
  skill only, with origin and SHA-256; the reviewer's own; none in legacy mode),
  `a_changed_skill_changes_the_context_hash` (AC-2, through the `context show` preview — exactly
  what the next attempt gets), `the_claude_implementer_gets_skills_in_its_context_file` (AC-3, and
  the reviewer's skill reached the reviewer).

### Group D review (independent subagent) — follow-up commit

- **Fixed:** the plan check trimmed skill names but the delivery compared them exactly, so
  `" xpp-batch"` passed the check and was then silently dropped (also skipping the budget check).
  One shared `context::plan_skills` (names exactly as written, deduplicated) is used by both; a name
  with spaces is simply off the menu.
- **Fixed:** in `toryo.yaml`-only mode a plan's `skills` is removed before `plan.json` / `PLAN.md` are
  written — it means nothing there (a planner might list its native skills), so the plan looks as in
  M10.
- **Fixed:** an approved skill that is no longer on the implementer's menu at attempt time (e.g. a
  `task config --reload` at the plan gate changed the menu) is reported under `skipped` instead of
  disappearing silently. A lower budget after the plan was approved only gives the usual over-budget
  warning (not re-checked).
- **Fixed:** planner-written skill names are escaped in the stderr line, the escalation reason and
  the feedback.
- Not changed (recorded): `fixed_bytes` leaves out the render scaffolding (the preamble, the task-pack
  listing), so a plan just under the budget can still exceed it slightly at attempt time — a warning
  only. Skill files are read through symlinks/junctions without a containment check: both roots
  (the repository's protected `.toryo/` and the machine's folder) are owner-controlled.
- Tests added: `m11_skills::{a_skill_name_is_matched_exactly, legacy_plans_ignore_skills,
  machine_skills_are_delivered_and_missing_ones_skipped}` (origin `machine`, `skipped`, the menu's
  "(not found on this machine)").

## F — Wrap-up

### F1 — templates

- `templates/d365fo/.toryo/toryo-orgchart.yaml` and `…/roles/implementer/toryo-role.yaml`: the
  owner's drafts **verbatim** (all comments kept; the drafts have no personal paths), each with a
  `TODO(owner)` header in front. `…/roles/{planner,reviewer}/toryo-role.yaml` (purpose,
  `instructions`, empty `skills`), `…/roles/*/instructions.md` (short, `TODO(owner)` — the D365 notes
  per role stay in `.toryo/context/50-xpp-roles.md`), `templates/d365fo/CULTURE.md` (the draft names
  `culture: ./CULTURE.md`), and stubs of the draft implementer's two skills in
  `.toryo/skills/{xpp-axclass,xpp-batch}/SKILL.md` (with a `description`, as the menu needs) — added
  so the set loads as shipped; the draft also allows them in the machine's `skills_dir`.
- **A second example** `templates/d365fo/toryo.orgchart.yaml.example` is the `toryo.yaml` of org
  chart mode (no `roles:`, `commands:` or `pipeline.max_fix_cycles`); its header says the pair loads
  together and that `toryo.yaml.example` (the M9/M10 layout, unchanged, same hash) does not load next
  to an org chart.
- The draft's `build` gate (`pwsh -File scripts/build-model.ps1`) is kept; the template's
  `TODO(owner)` says the script must be committed before a task (M18) or to switch to the
  `tool: xpp` alternative the draft shows.
- `templates/toryo.local.yaml.example`: commented `seats:`, `skills_dir:`, `environment_gates:` with
  the six-point script contract of the owner's draft (plus: call tools by absolute path — the
  working directory is the verify checkout) and `verify:` (A2). `templates/d365fo/
  toryo.local.yaml.example`: a short commented pointer. Comments only: both keep their hash.
- Test: `team::tests::the_d365fo_org_chart_templates_load` (the set loads with fake profiles and a
  machine `environment_gates.dbsync`; every new file starts with its `TODO(owner)` items; the old
  example next to the org chart is refused, one place per setting). The existing template tests
  pass unchanged (AC-1).

### F2 — docs, migration guide, morning summary

- README: "Org chart mode (M11)" (the `.toryo/` layout, the two modes, seats and profiles, limits, checks,
  instructions and culture, skills — lookup order, the planner's choice, only `SKILL.md` delivered —, the write
  fence, check vs environment gates and the script contract, the deployment record, the machine `verify:` floor),
  "Migrating an M10 `toryo.yaml` to `.toryo/`" (eight steps), "The effective configuration (M11)" (`config show
  --json`, schema 1, from B2), the new event and `--json` keys, the new test files, an M11 status paragraph.
- `docs/HUONG-DAN.md` §5.9 (Vietnamese): the workshop picture, the layout, what to remember, the two gate kinds,
  a worked D365FO example (a `tool: xpp` build check gate + a `dbsync` environment gate, the role file, the machine
  config), the script contract, the M13 note for the Tokyo machine, the migration in short.
- This summary, at the top.

## Open

The list is in the morning summary at the top ("Open"); each item is explained in its task's section.

## Not started / out of scope

- **M13** (cp932 output on Japanese Windows) will also garble DB-sync output on the owner's Tokyo
  machine — the next thing to fix after M11.

