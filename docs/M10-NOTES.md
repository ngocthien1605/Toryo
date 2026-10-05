# M10 notes — decisions and deviations while building M10 (`M10_task.md`)

Every entry: what was decided, why, and where. The owner folds these into plan v0.9.

> Open items of this milestone are kept in one list, with priorities, in [`STATUS.md`](STATUS.md) (§4); the decisions in
> [`DECISIONS.md`](DECISIONS.md). The "Open" list below is the record as it stood at the time.

## Morning summary (2026-09-30, after three rounds of review fixes)

**M10, review fixes R1–R6, round 2 (R7, R8) and round 3 (R9) are DONE; nothing PARTIAL.** Branch
`m10/flight-recorder` on `main` @ `5adf204`, not pushed.

| Commit | Task | State |
|---|---|---|
| `ca35047` | docs: owner verification E1, E7 (M9) — your notes | — |
| `e17875c` … `de85374` | M10 A1, A2, B1, B2, C1, C2, D1, E1 (first night) | DONE |
| `d75beea` | R1 — `run_ended` on every exit of a drive; `watch` always terminates | DONE |
| `59cefb7` | R2 — `turn_finished.usage` is what the store recorded | DONE |
| `f9e1971` | R3 — `toryo usage` says when an estimate is incomplete | DONE |
| `6ae1624` | R4 — toast hardening (clean text, `--yes`, no inherited handles, exit wait, `-Command`) | DONE |
| `dcd9611` | R5 — every escalation and pause prints its reason once | DONE |
| `e27b1f5` | R6 — typed classes, fail-closed redaction, `seq`, recovery events, no lock probe, read-only store | DONE |
| `047a12d` | docs: review fixes in README, HUONG-DAN §5.8 and this summary | DONE |
| `52101e0` | R7 — round 2, events and engine (recovery duplicate, redact-then-cut, reject reason, `watch` rule, panic guard, `seq`, coverage test, `is_alive`) | DONE |
| `45c60b1` | R8 — round 2, usage and notify | DONE |
| (last) | R9 — round 3: a stale owner is not alive, `no price table` total, exhaustive coverage test, no panic while unwinding, `notify-test` cause, ordinal env order | DONE |

**Checks** (§0.2) green before every commit on this machine: `cargo fmt --check`, both `clippy -D warnings`,
`cargo test --locked`. Tests: **441 passed, 0 failed, 6 ignored** — 437 / 6 at `45c60b1` (before round 3),
426 / 6 at `047a12d` (before round 2),
401 / 6 at `de85374` (before the fixes), 355 / 5 before M10. Each fix has a test that fails before it, except where the commit says why not
(new API that does not compile before, and the lock-probe race of R6, which is not practical to reproduce).
No real agent was run; your configs and data dirs were not read. One real toast was shown here to check
the new `-Command` path (exit 0).

**Owner decisions applied:** Q1 `finish_and` transitions stay quiet on stderr; Q2 no gate toasts under
`--yes` (R4); Q3 `vendor_reported_cost_usd` later — the column and the display stay, nothing fills it;
Q4 reservations priced as uncached input, now said in the report header (R3); Q5 the CI toast test stays;
review item 5 — the new `!! reason` lines stay, as a uniform rule (R5).

**Deviations from `M10_task.md`** (details below):
1. The module is `src/engine/flight.rs`, not `events.rs` (`tests/architecture.rs`, R52, forbids the text
   `events::` in engine files).
2. `RunStarted { owner }`, not `{ generation }` (the envelope has `generation`).
3. `stage_started` for verify is emitted when its header is printed (after the checkout exists).
4. The CLI runs on a thread with an 8 MB stack (the Windows debug build overflowed in clap's builder).
5. The stderr engine lines go through the task's secret redaction.
6. **stderr is not identical to `main`'s** (review item 5, kept by the owner): every escalation and pause
   prints its reason exactly once before its `[state]` line; before M10 several escalation paths printed
   none (a blocked review, the fix-cycle bound, the plan-policy bound, an implementer failure, a rejected
   review, a final-gate rejection at the bound). **Remaining exceptions** (the reason is still said once,
   by another line): the budget pauses (`!! budget reached during the turn …` / `!! admission cap reached
   …`), the pause after the last implementer switch (`!! implementer X: … → no fallback left`), the
   withdrawn final gate (the refused `approve` prints the error), and transitions inside `finish_and`,
   which print no `[state]` line at all (owner decision Q1). The first night's claim "identical on the
   same scenarios" held only because those paths were not in the scenarios.
7. Changed M10 behaviour on the owner's or the fix list's request: `watch` also stops for `New` and any
   state nobody drives (R1); `usage[].budget_tokens` → `counted_tokens` (R6); `compile_only` may be
   `null` (R6); `reason_class` is snake_case and typed (R6); toasts use `-Command` (R4); `watch` stops only
   when nobody drives the task, whatever the state (R7); `totals.est_complete` is `false` when any billed
   group is unpriced, also without an estimate (R8).

**Open** (moved to [`STATUS.md`](STATUS.md) — O-15, O-60 … O-63; kept here with the procedure of the elevated-`status` check):
- **Elevated-`status` check (R9.1), not done unattended:** this session is not elevated and UAC prompts for
  consent. To check by hand: in an **elevated** PowerShell, with a temp `TORYO_DATA_DIR` and temp
  `--config`/`--machine-config` (e.g. the ones `tests/common` writes), `toryo task create …` and `toryo run
  <ID> --fake-agents <script with a {"kind":"delay","ms":120000} planner step>`; meanwhile, in a normal
  PowerShell with the same `TORYO_DATA_DIR`, `toryo status <ID>` must show ` · ACTIVE (…)` and
  `toryo status <ID> --json` a non-null `active_owner`. Expected: the elevated process grants
  `PROCESS_QUERY_LIMITED_INFORMATION` to its own user, so the creation-time path decides.
- **M11 — Ctrl+C on `toryo run`** kills the process with the default console handler: no `run_ended`, no
  toast, the owner row stays (pre-existing). Route Ctrl+C on drive commands to the stop path (R38) later.
- **M11 — R7.1's microsecond window:** a hard kill between `record_usage_in` and its `turn_finished`
  leaves no event for that usage row (recovery skips rows that existed before it ran).
- `vendor_reported_cost_usd` is still never filled (Q3: later).
- The Unix paths (`watch`'s SIGINT handler, the non-Windows notify stubs) compile
  and run only on Linux CI — no Linux target on this machine.
- **Q5 answered (PR #2 CI, run #15):** `notify_test_builds_the_toast` and
  `the_real_script_takes_hostile_text_as_data` load WinRT types in Windows PowerShell on
  `windows-latest` and both pass there — no `#[ignore]` needed.
- Not tested (not practical): the error printed before the toast wait (R8.5 — only the timing changes) and
  the `notify-test` failure text (R8.7 — needs a failing script).
- Transitions made by CLI commands outside the engine (recovery, resume) still get their escalation class
  from the reason-prefix fallback; `task cleanup`'s `Cancel` is not in `events.jsonl` (the file goes with
  the run store).

**Try first** (build first: `cargo build --release` — `bin\toryo.exe` is not rebuilt):
1. `toryo notify-test` — a toast "toryo — notify-test"; if none, allow notifications for "Windows
   PowerShell" (under Constrained Language Mode it fails and says so).
2. `templates\prices.toml.example` → `%USERPROFILE%\.toryo\prices.toml` with your Foundry deployment names
   and prices (give `claude-foundry` a `model:`), then `toryo usage <an existing task>` — a total that
   misses a price now says `+ no price (N groups)`.
3. The next real task: `toryo run ID --notify` in one window, `toryo watch ID` in another; `toryo status
   --json` / `task show ID --json` while it runs no longer touch its lock.
4. `scripts\m9-check.ps1 -Canary` again: no `+~400000 reserved` (C2).

## Progress

Branch `m10/flight-recorder` (from `main` @ `5adf204`). One commit per task.

- [x] docs: owner verification E1, E7 (M9) — the owner's uncommitted notes, committed alone
- [x] A1 — `EngineEvent`, sinks, `events.jsonl` flight recorder
- [x] A2 — the engine's CLI lines rendered from events
- [x] B1 — `status --json`, `task show --json`
- [x] B2 — `toryo watch <ID>`
- [x] C1 — `prices.toml` + `toryo usage <ID>`
- [x] C2 — scripted turns in mixed runs report zero usage
- [x] D1 — Windows toast
- [x] E1 — docs, morning summary

Review fixes (`M10-review-fixes.md`, 2026-09-30):

- [x] R1 — `run_ended` on every exit of a drive; `watch` always terminates
- [x] R2 — `turn_finished.usage` is what the store recorded
- [x] R3 — `toryo usage` does not look complete when it is not
- [x] R4 — toast hardening
- [x] R5 — escalation reasons on stderr: keep, pin, document
- [x] R6 — cheap Low fixes
- [x] docs — README, HUONG-DAN §5.8, the rewritten morning summary

Review fixes, round 2 (`M10-review-fixes-2.md`):

- [x] R7 — events and engine
- [x] R8 — usage and notify

Review fixes, round 3 (`M10-review-fixes-3.md`):

- [x] R9 — stale owner vs "access denied", docs behind R8, cheap Lows

## A — Event backbone (ARCH-02)

### A1 — `EngineEvent`, sinks, the flight recorder

- **Module** `src/engine/flight.rs` (deviation: not `events.rs` — `tests/architecture.rs` (R52) forbids the text `events::` in engine files, where it means the vendor stream module `agents::events`; the test is not weakened, the module is named after the flight recorder): `EngineEvent` (serde `tag = "type"`, snake_case), `Envelope`
  (`v`, `seq`, `ts`, `task`, `generation`, the event flattened), `EventSink` (`name`, `emit → Result`),
  `FileSink`, `Recorder` (the sinks + the per-task sequence + redaction). No new crate; the `time`
  crate gets its `parsing` feature (no new dependency) to compute a stage's `duration_ms` from
  `stage_attempts.started_at`, which also works for an attempt begun by an earlier process.
- **Deviation: `RunStarted { owner }`, not `{ generation }`.** The envelope already carries
  `generation`; the same key inside the flattened event would be written twice. `owner` is the lock
  owner label (`user pid N`).
- **Where events come from.** The engine emits through `Engine.rec` (a `&Recorder`, new argument of
  `Engine::new`). Transitions go through one pure function, `flight::transition_events(from, event,
  machine)`: the escalation or pause the transition led to (`escalated` with `reason_class`, `paused`),
  the `state_changed`, and the `gate_opened` of a new gate — in the order the CLI prints them (the
  `!! reason` line comes before the `[state]` line). So no escalation path can forget its event.
  `stage_finished` is emitted when a *running* attempt ends (`end_and`, `finish_and`, the failed-turn
  and switch paths); the checkpoint's second `finish_attempt` on the writing attempt is not a stage end.
  `stage_started` is emitted where the stage header is printed (so the verify header's boundary and
  checkout are known): a verify stage that escalates before its checkout exists has a `stage_finished`
  without a `stage_started`.
- **Outside the engine** the gate commands record too: `approve_now`/`reject_now`/`withdraw_final`
  take the recorder; `drive` emits `run_started`/`run_ended` and a `state_changed` with event
  `recovery` when §10 recovery changed the state; `resume` records the transition it makes before
  driving (`resume_stage`, `resume_with_note`, `unpause`, `recovery` for `--discard/--keep-partial`);
  `approve --force` records `force_final`, `task config --reload` its `reverify`. Not recorded:
  `task cleanup`'s `Cancel` (the file goes with the run store).
- **`quiet`** (`Envelope.quiet`, `#[serde(skip)]`, never written): the CLI printed no line for that
  event at that point before M10 (e.g. `finish_and` transitions have never printed `[state]`).
  A2's stderr renderer skips quiet events; the file and `watch` see everything. (Corrected by R5:
  `ImplementOutcome::Failed` and a rejected review were quiet here at first; since R5 every
  escalation and pause prints its reason once — see R5 and deviation 6.)
- **Sequence** (`seq`): the recorder starts at the highest `seq` in the file + 1 (complete lines only),
  so it is monotonic across runs. Only the lock holder writes the file; gaps are possible (an event a
  sink could not write still takes its number).
- **Crash safety.** The file is opened in append mode for every event and the line is written with one
  `write_all` and flushed. A partial last line (a crash mid-write) is skipped by readers
  (`flight::read_lines`) and closed with a newline before the next event, so it never damages a good
  line.
- **Redaction (R26/R42).** `Recorder::for_task` builds the redaction set from the secrets of the task's
  profiles (`resolve_profile_secrets` of planner, reviewer, implementers — a profile whose secret cannot
  be resolved contributes nothing — plus `CLAUDE_CODE_OAUTH_TOKEN`). Every string of every event is
  redacted before any sink sees it (also the renderer and the notifier). Test:
  `a_profile_secret_never_reaches_the_flight_recorder` (literal and JSON-escaped forms).
- **Sink errors** are reported once per sink and recorder (`!! flight recorder: cannot write … — the run
  continues without it`), the run continues. Test: a directory in place of `events.jsonl`.
- **Tests.** Unit (`engine::flight::tests`): every variant round-trips, transitions → events, reason
  classes, numbering + redaction + a broken sink, partial last line, unwritable file, elapsed time.
  Integration (`tests/m10_events.rs`): AC-1 to AC-5, plus `task cleanup` removes the file.


### A2 — the engine's CLI lines come from events

- **`flight::render(&Envelope)`** gives every event a text; **`StderrSink`** prints it for every event
  that is not `quiet`. `Recorder::for_task` has both sinks (file, then stderr). The engine's
  `eprintln!` for these lines are gone from `engine/runner.rs`: `[state]` (engine, `approve_now`,
  `reject_now`, `withdraw_final`), the four stage headers, `✔ checkpoint …` (both lines of a trailer
  match: one event with `found`), the verify command lines, `!! budget: …`, and the `!! <reason>` line
  printed before an escalation or a reason-carrying pause (it is the `escalated`/`paused` event).
- **Same text.** `reject_now` has always printed `--rejected-->`: the renderer maps `plan_rejected` /
  `final_rejected` to that. Unit test `events_render_the_text_the_engine_printed` compares each
  variant with the old format expressions. End to end, `main`'s binary and this one were run on the
  same fake scenarios (fix cycles with a failing and a passing verify, review changes, a final
  rejection that escalates at the fix-cycle bound, a protected-config escalation, an implementer
  failure without changes, resume with a note, a budget warning and the admission pause): after
  normalising paths, hashes and durations the 335 stderr lines are identical. **Not identical in
  general** (found by the review): transitions the state machine turns into an escalation inside
  `end_and` — a blocked review, the fix-cycle bound after changes requested, the plan-policy bound —
  print `!! <reason>` now, which `main` did not; those paths were not in the scenarios. The owner kept
  the new lines (review item 5); R5 made the rule uniform (deviation 6).
- **One difference**, only when a secret is involved: the stderr lines now go through the task's
  redaction (R42) like the file — a reason that carried a profile secret was printed as is before.
- **Lines that are not events** (unchanged `eprintln!`, `engine/runner.rs`): `TORYO_TEST_CRASH_AT`,
  `!! admission cap reached …` (the pause itself is the quiet `paused` event), `!! budget reached
  during the turn …`, the stop notes (`■ stop request handled …`, `■ stopped at a stage boundary`,
  `■ stop requested during the backoff`), `(Plan gate approved automatically: --yes)`, the retry notes
  (`retry_note`, `!! <profile>: <class> — retry n/2 in Ns`), the rendered PLAN.md and REVIEW.md, the
  plan-time policy rejection, the path policy restore, the schema-violation note of
  implementation.json, the HEAD-guard soft reset, the failed-turn-kept note (the stage runs again),
  `!! implementer X: … → switching to Y`, `protected baseline renewed`, the stale-verify re-verify
  note, `!! invalid review (correction n/2)`, `review_inconsistent`, the context warnings and
  `context: N bytes, …`, the final-approval uncommitted-changes note. Outside the engine: the turn
  streaming and `print_summary` (adapters), `↺ recovery: …`, and `drive`'s closing lines
  (`══════════ …`, `final gate — …`, `next:`, `summary:`; the `run_ended` event renders the first and
  `next:` for `watch`, emitted quiet).

## B — Machine-readable and live views (DES-10)

### B1 — `status --json`, `task show --json`

- **Module** `src/engine/views.rs` (pure builders, unit-tested): `task_json(row, TaskFacts)` and
  `show_json(task, row, attempts, usage, gates, events_file)`. The CLI computes `TaskFacts` (lock probe,
  frozen config → `compile_only`, next action) once per task in `engine_cli::facts`, which the text
  `status` now uses too (same output).
- **Every key is always present**; an absent value is `null` (`budget_tokens`, `active_owner`, an
  attempt's `profile`/`error_class`/`head_after`/`partial_commit`). The spec's `?` fields were read as
  "may be empty", and a stable set of keys is easier for a consumer than optional keys.
- **Additions** beyond the listed fields: `title` in the task object; `attempt` (the number) in each
  attempt; `requested_at`/`resolved_at` in each gate. `task list --json` prints the `status --json`
  document (`task list` is `status` without an id).
- `events_file` is the path of the flight recorder, whether or not it exists yet.
- Tests: `tests/m10_views.rs` (both documents after a fake run to the final gate, `{ "tasks": [] }`
  without tasks, a cleaned-up task keeps state and next action), `tests/m9_xpp.rs::
  json_views_say_compile_only_for_an_xpp_verify`. Schema: README "Following a task (M10)".

### B2 — `toryo watch <ID>`

- `toryo watch ID [--json] [--from-start] [--no-follow]` prints on **stdout**: the history (the last 20
  events, or all with `--from-start`), then new events every ~500 ms — rendered with A2's renderer
  (`flight::render`: the CLI's own lines for the events it prints, a line for the others, e.g.
  `   turn implement/1 (codex-openai): completed · 125 tokens, exact (0.1s)`) or, with `--json`, the
  file's lines unchanged.
- **Reader** `flight::Tail`: remembers the offset, returns complete lines only (a partial last line
  waits for its newline), starts over when the file shrank. A line that is not a JSON object is skipped;
  with the renderer, so is an event type this toryo does not know (it is kept with `--json`).
- **No task lock, not even a probe.** `status` checks activity with `lock::probe_free`, which takes the
  OS lock for an instant — a `toryo run` starting at that instant would be refused with `task_active`.
  `watch` polls every 500 ms, so it decides "somebody drives the task" from the recorded lock owner in
  the store (its PID and creation time alive, `store::identity_alive`), never from the lock.
- **When it stops** (`flight::watch_done`): 2 s without a new event and the task waits for a human (a
  gate, `Escalated`, `Paused`), is finished, or is in a running state that no live process drives (a
  crashed run: `toryo resume`). A `New` task is followed until it starts. `--no-follow` exits after the
  history. Ctrl+C exits 0: a console control handler on Windows (`SetConsoleCtrlHandler`,
  `windows-sys` feature `Win32_System_Console`, no new crate), SIGINT on Unix (`libc::signal`); the loop
  checks the flag every 50 ms.
- **Errors.** An unknown task is the store's error (exit 2). No `events.jsonl` is an error
  ("no flight recorder … the task has not run since M10, or `task cleanup` removed its run store"),
  except while a live process drives the task (the file appears with its first event).
- **Stack size (found on the way).** Adding the `watch` subcommand made the debug binary overflow the
  1 MB main-thread stack on Windows already in `--help` (clap's derived builder for the whole command
  tree; `main` was just under the limit). `main` now runs the CLI on a thread with an 8 MB stack
  (`CLI_STACK`). No behaviour change otherwise (exit codes, the task-active code, panics → 101).
- Tests: `tests/m10_views.rs` — the whole history after a finished run (rendered and `--json` equal to
  the file, the last 20, follow mode stops by itself, a partial last line), errors, a concurrent `run`
  with a 2.5 s `delay` followed to the plan gate (AC-2), and Ctrl+C → exit 0 (Windows: CTRL_BREAK to the
  child's own process group, since Ctrl+C cannot be sent to another group and `watch` treats both alike;
  a mutation without the handler fails it with `STATUS_CONTROL_C_EXIT`). Unit: the tail, the history,
  the line filter, the stop rule.
- **Not verified here:** the Unix signal path compiles and runs only on Linux CI (no Linux target on
  this machine; installing one would be a download).

## C — Usage (plan §12)

### C1 — `prices.toml` and `toryo usage <ID>`

- **Module** `src/engine/usage_report.rs` (pure, unit-tested): `parse_prices`/`load_prices`, `report`,
  `render_text`, `to_json`, and the model resolution (`effective_model`, `configured_model`,
  `stage_role`). `store::usage_detail` reads the rows (joined with the attempt's stage). `toryo usage ID
  [--json] [--prices FILE]` needs no lock.
- **The model is recorded.** `UsageIn.model`; both INSERTs fill `usage.model`. The engine passes the
  model it asked the adapter for (the role's; for a Codex implementer the role's, else the profile's;
  none for a Claude implementer — as before M10), and the row records `effective_model`: that, else the
  profile's `model`, else a `model` in `codex.overrides`. That is the `-m` value, i.e. the Foundry
  deployment name for a profile with a `codex.provider` block. A turn killed before it reported and
  recorded by §10 recovery gets the configured model. A row with no model anywhere stays NULL.
- **Rows written before M10** (`model` NULL): the task's frozen configuration names it —
  `configured_model(cfg, stage, profile)`, the same rule — else `unknown model` (also for a cleaned-up
  task whose frozen configuration is gone).
- **Price table.** Default `%USERPROFILE%\.toryo\prices.toml` (`config::home_dir`), override `--prices`.
  `version` is required (it is printed with every estimate), `safety_factor` defaults to 1.0 and must be
  > 0, every model needs all four buckets (a missing price would silently mean $0). Parsed from a
  `toml::Table` by hand so every error names its key (`models."gpt-6-astra".output`). **No file → no
  estimates**, also for a `--prices` path that does not exist: the report says `prices: no price table
  (<path>)`, so a typo is visible. Tests always pass `--prices`: the owner's file is never read.
- **Report.** One group per stage × profile × model, in order of first appearance. Buckets are the sum of
  the counted `exact` and `provisional` rows; the three totals stay apart (`exact`, `provis.`, `~unknown`
  = reservations). Rows with `reconciled_by` are not counted (a later exact row absorbed them, R41); the
  report says how many. Cost: `—` for `subscription`, `no price` for a model missing from the table
  (listed again at the end), `no price table` without a table, else `est. $x` = Σ bucket × price / 1e6 ×
  `safety_factor`. **Reservations are priced as uncached input** and make the label `~est.` — a
  reservation has no buckets; input is where most tokens go, and output prices would inflate it ×8.
- **`vendor_reported_cost_usd`** is summed and shown next to the estimate when present — but nothing
  fills that column today (the Claude adapter parses `total_cost_usd`, `StageOutcome` does not carry it).
  Open: carry it through `StageOutcome` if the comparison is wanted.
- **`est_cost_usd` / `price_table_version` stay unused.** Filling them at insert would freeze an estimate
  made with the prices of that day into the database; computing at report time lets a corrected
  `prices.toml` fix every past estimate, and the version is printed with it. Worth filling later only if
  `budget_usd_est` enforcement (out of scope) needs a running total without the table.
- Tests: unit (bucket sums and the three totals, subscription vs foundry, `no price`, the safety factor,
  `~est.` with reservations, reconciled rows not counted twice, the vendor cost, the parser's key names,
  no file); `tests/m10_usage.rs` (a fake run with `usage` steps and a Foundry-billed fake profile: text and
  `--json` numbers, `no price`, the model on new rows, the NULL fallback, a bad table, no table).

### C2 — scripted turns in mixed runs report zero usage

- **Rule.** When the fake script lists `only_profiles` (a mixed fake/real run, like the E7 canary), a
  scripted turn that ends — `complete` or `fail` (also the "no turn left" failure and an interrupt) —
  without having reported usage first reports **exact zero** usage
  (all four buckets 0) just before its end: the row is `exact`, 0 tokens, no reservation. A scripted
  turn with a `usage` step keeps its numbers. A pure fake run (no `only_profiles`) is unchanged: a turn
  without usage is `unknown` and R41 reserves (the R41 tests rely on that and pass unchanged).
- **Where.** `agents::fake` — `Shared.zero_usage` (from the script), `FakeSession.usage_seen` (per
  turn), `FakeSession::zero_usage_before(step)`. A turn that ends without a terminal step (`crash`, the
  steps running out) still reports nothing: it models a crash, and a crash's usage is unknown.
  **Replayed streams are not affected** (corrected in R6; this note first said they were): the real
  parsers' `terminal_events` always emit a `Usage` event before the terminal one — `Unknown` when the
  stream had none — so a replayed turn has reported usage and keeps it (R41 reserves for `Unknown`).
- **Why exact zero, not "no row".** The engine records a row for every turn (R41: never silently zero);
  an exact zero row keeps that invariant and says what is true — a scripted turn costs nothing.
- Tests: `tests/m10_usage.rs::scripted_turns_of_a_mixed_run_report_exact_zero` (both profiles listed, so
  nothing real runs; exact zero rows, `status` without `reserved`; the same script without the list
  reserves), `tests/regress_agents.rs::fake_turns_of_a_mixed_run_report_zero_usage` (complete, fail,
  with a usage step, pure fake).

## D — Notifications

### D1 — Windows toast when the owner is needed

- **Module** `src/notify.rs`: `toast_for(envelope)` (pure) decides; `Notifier` (an `EventSink`) passes
  the toast to a `Toaster`; `SystemToaster` shows it (Windows) or does nothing (elsewhere).
- **Which events:** `gate_opened` (plan and final), `escalated`, `paused`, `run_ended` in `Completed` /
  `Failed` — one toast per stop (`run_ended` at a gate, in `Escalated` or `Paused` raises nothing more).
  Title `toryo — <id>`, body `<state> — <next action>`, shortened to 200 characters (an ellipsis marks
  the cut; NUL characters are dropped). The gate, escalation and pause events carry no `next_action`, so
  the body names the command from the event itself (`toryo approve ID --gate plan --continue` / `toryo
  reject …`, `toryo status ID` for an escalation — put before the reason so the cut never hides it —,
  `toryo resume ID`, `toryo budget set …` for a budget pause); `run_ended` has the engine's next action.
  The text is already redacted by the recorder (R42).
- **Settings.** `notify: { toast: false }` in the machine config (`MachineConfig`, `deny_unknown_fields`
  also inside `notify`); `Effective.notify` is `#[serde(skip)]` — not frozen, not hashed, like `origins`.
  A drive command reads the machine config in use (`--machine-config`, else the default) for it:
  `DriveArgs.machine_config` (`#[arg(skip)]`, filled by `main::drive_with`). `--notify` / `--no-notify` on
  `run`, `resume`, `approve`; `--no-notify` wins. `approve` without `--continue` never toasts (the owner
  is at the keyboard). A machine config that cannot be read: a warning, no toasts (the run uses the
  frozen configuration anyway).
- **The toast process.** `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe -NoProfile
  -NonInteractive -EncodedCommand <SCRIPT>` — Windows PowerShell 5.1, which loads WinRT types
  (`pwsh` 7 cannot). The script is a constant (`notify::SCRIPT`, base64 of UTF-16LE, a 20-line encoder
  instead of a crate); it reads `TORYO_TOAST_TITLE` / `TORYO_TOAST_BODY` and puts them into the toast XML
  with `CreateTextNode` — never parsed as XML or code. AppUserModelID: Windows PowerShell's own
  (`{1AC14E77-…}\WindowsPowerShell\v1.0\powershell.exe`), so no registration is needed. `CREATE_NO_WINDOW`,
  no stdin/stdout/stderr, not assigned to any Job (toryo's stage Jobs hold agent processes only); a
  watcher thread kills it after 10 s while toryo lives. A process that cannot start is a sink error:
  reported once on stderr, the run goes on. The script's own failure is not reported (fire and forget) —
  `notify-test` is the check.
- **`toryo notify-test [--dry-run]`** (hidden): shows a sample toast and waits for the script (exit 0 /
  1; 124 → stopped after 10 s). `--dry-run` builds the toast (WinRT types, XML, `ToastNotification`)
  and exits before `Show`. Run once here on 2026-09-30: both exit 0.
- **Test hook** `TORYO_TEST_TOAST_LOG=<file>`: the system toaster appends each toast there as a JSON line
  instead of showing it (end-to-end tests without toasts on the desktop).
- Tests: unit (`notify::tests`: which events notify and the text, the length cap, on/off rules, hostile
  text only in the environment with a constant program + arguments, base64 vectors),
  `config::tests::notify_is_a_machine_setting_outside_the_hash` (loads with and without, same hash
  `7ae81a43…` as `configs_without_toolchains_keep_their_hash`), `tests/m10_notify.rs` (default off,
  `--notify`, machine setting, `--no-notify` wins, no toast for a plain `approve`, the gates and the
  completed run with `--yes`, an escalation's body; `notify-test --dry-run` exits 0 on Windows — runs in
  CI too, no desktop needed; the real toast is `#[ignore]`d: `cargo test --test m10_notify -- --ignored`).
- **Resolved (Q5):** `notify_test_builds_the_toast` runs Windows PowerShell and loads WinRT types in CI
  (`windows-latest`). It passed here and on PR #2's CI, so it stays un-ignored — it is the only M10
  test that needs more than the Rust toolchain and Git.

## Review fixes (2026-09-30)

From `M10-review-fixes.md` (review of `de85374`). Owner decisions taken as final: Q1 `finish_and`
transitions stay quiet on stderr; Q2 no gate toasts under `--yes` (R4); Q3 `vendor_reported_cost_usd`
later, not in M10; Q4 reservations priced as uncached input, said in the report header (R3); Q5 the CI
toast test stays; review item 5 — the new `!! reason` lines stay (R5).

### R1 — `run_ended` on every exit of a drive; `watch` always terminates

- `drive` is now a wrapper around `drive_inner`: it records the state at the start, runs the drive, and
  emits **one** `run_ended` whatever happened — state and next action re-read from the store (the last
  known state if the store cannot be read), `from` = the state at the start, and on an error `error` =
  `{e:#}`, redacted like every event and shortened to 400 characters (`flight::ERROR_MAX`). Both new
  fields are optional (`skip_serializing_if`), so `v` stays 1 and older lines still parse.
- **Toasts:** a `run_ended` with an `error` toasts whatever the state (`<state> — stopped with an error:
  …`); `Completed`/`Failed` toast only when `from` differs (no second "Completed" for `run` on a finished
  task or for `approve --gate final --continue`, whose approval completed the task before the drive). An
  old line without `from` still toasts. `approve --force [--continue]` records the forced final gate with
  a recorder without the notifier: the owner just opened it.
- **`watch` stop rule** (`flight::watch_done`): 2 s without a new event and — whatever the state — no
  live process drives the task, or the task waits or is finished. So `New` (never run, or a drive that
  failed before its first stage) and a crashed running state end the follow. This changes one assertion
  of B2's unit test (`New` without a driver was "follow on"), as the fix list asks. "Done" must hold on
  **two consecutive polls** (the engine commits a transition before it writes its events) and the file is
  read once more before returning.
- **Closed stdout:** `watch` writes through a locked stdout and ends with exit 0 on `BrokenPipe`
  (`show_lines`); before, `println!` panicked (exit 101). The test runs on both platforms (the reader is
  dropped before `watch` writes; Windows reports `ERROR_NO_DATA` as `BrokenPipe`).
- Tests (all three failed before the fix, checked with the source stashed): `tests/m10_views.rs::
  a_failed_drive_ends_its_record_and_watch_terminates`, `watch_into_a_closed_pipe_exits_0`,
  `tests/m10_notify.rs::no_second_completed_toast_and_an_error_end_toasts`; unit
  `notify::tests::a_run_end_toasts_on_an_error_or_a_new_end_state`.

### R2 — `turn_finished.usage` is what the store recorded

- `store::record_usage_in` returns `store::Recorded { id, buckets, accounting, reservation }` — the row
  as stored: for a cumulative adapter (Codex on a resumed session) the delta against the session
  baseline, the accounting after the engine turned a cut-short `exact` into `provisional`, and the
  reservation of a turn with no telemetry. `Engine::usage` emits exactly that in `turn_finished`; a
  reservation is `usage` absent + the new optional `reserved_tokens` (rendered `no usage reported,
  ~N reserved`). Recovery's reservations are emitted in R6.
- Test `tests/m10_events.rs::turn_usage_in_events_is_what_the_store_recorded`: a Codex reviewer with a
  correction turn (cumulative 3300 → 4400): events 3300 and 1100 = the rows; the planner turn without
  usage shows `reserved_tokens: 200000`. Before the fix: `[3300, 4400]`.

### R3 — `toryo usage` does not look complete when it is not

- **Incomplete total:** `Report.est_complete` — `false` when a group billed per token has no price
  (the text total then reads `est. $x + no price (N groups)`, JSON `totals.est_complete: false`,
  `totals.unpriced_groups: N`), `true` when every such group is priced, `null` when nothing was
  estimated.
- **Group key** is stage × profile × model × **billing kind**: a reload that changes a profile's
  `billing:` never mixes a subscription row into a priced group.
- `no_price` lists each (model, profile) once, in order of first appearance (`dedup()` only removed
  neighbours).
- `includes_unreported` is read by `store::usage_detail`, carried per group (JSON
  `rows[].includes_unreported`), and makes that group's estimate `~est.` (it holds killed turns' tokens
  whose split into buckets is unknown).
- Header: `exact`/`provisional` are raw token counts; `~unknown` reservations are budget tokens
  (weighted by `pipeline.budget_weights`), priced as uncached input (owner decision Q4).
- Templates (`toryo.local.yaml.example`, the d365fo one) and README: a `claude-foundry` profile needs
  `model:` to be priced — a Claude implementer runs with, and records, the profile's model only.
- Unit tests (`engine::usage_report::tests::r3_*`): priced + unpriced api groups → `est_complete:
  false` and the marker (and `true`/`null`); two billing kinds → two groups; four unpriced groups of
  one model → listed once; an `includes_unreported` row → `~est.`. Before the fix the grouping and
  listing tests fail and the others do not compile (the fields are new).

### R4 — toast hardening

(Supersedes parts of D1: `-EncodedCommand`/base64, the watcher thread, "`--no-notify` wins".)

- **Clean text** (`notify::clean`, used by `Toast::new` for the title and the body before the 200-char
  cut): ANSI CSI/OSC sequences removed (`process::strip_ansi`), every character outside the XML 1.0
  `Char` set (control characters, U+FFFE, U+FFFF) and tab/CR/LF turned into a space, runs of spaces
  collapsed, trimmed. A body with such characters made the toast XML invalid and the toast was likely
  dropped silently.
- **`--yes`** (owner decision Q2): `Notifier::without_gates()` — no toast when a gate opens; escalations,
  pauses and the end of the run still toast. `tests/m10_notify.rs::toasts_at_the_gates_…` expected the
  final-gate toast of a `--yes` run: **the one existing assertion changed**, as the fix list allows.
- **No inherited handles:** `process::windows::spawn_detached` — `CreateProcessW` with `bInheritHandles =
  FALSE`, `CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT`, toryo's environment plus the three toast
  variables (names compared case-insensitively, `notify::child_env`). `std::process::Command` passed
  `TRUE`, so the toast held e.g. toryo's stdout pipe under `Tee-Object` and the reader waited for it.
  Straightforward with the existing `env_block`/`quote_windows_arg`/`Owned` helpers.
- **Lifetime:** no watcher thread (it died with toryo, right after most toasts). The started processes are
  kept in `notify::PENDING`; `notify::wait_pending()` runs at the end of `main::cli_main` and waits for each
  until its 10 s deadline, then kills it (usually well under a second of extra wait). A toast that is
  already done is dropped from the list when the next one starts. `run_script` (used by `notify-test`)
  waits the same way.
- **`-Command` instead of `-EncodedCommand`:** `SCRIPT` holds no double quote and does not end in a
  backslash, so `quote_windows_arg` only wraps it (unit test `the_script_holds_no_double_quote`); the AC-2
  test still checks that the program and arguments are constant and the text is only in the environment.
  The base64 encoder is gone. README/HUONG-DAN name Constrained Language Mode as a known limit, and
  `notify-test` says so when the script fails. Checked here: the real toast still shows (exit 0).
- **Docs:** `--notify` and `--no-notify` are mutually exclusive (clap refuses the pair); either overrides
  the machine setting.
- Tests: unit `toast_text_is_cleaned_for_xml`, `without_gates_the_gates_are_silent`,
  `the_script_holds_no_double_quote`, `the_toast_variables_replace_inherited_ones`,
  `a_toast_process_is_killed_at_its_deadline` (Windows), `process::windows::detached_tests::
  a_detached_process_inherits_no_handle` (Windows; a mutation with `bInheritHandles = 1` fails it);
  `tests/m10_notify.rs::the_real_script_takes_hostile_text_as_data` (Windows, dry run: quotes, `$(...)`,
  backticks, ANSI, BEL, U+FFFE, Japanese, Vietnamese → exit 0). The new unit tests use new API (they do not
  compile before the fix); the `--yes` assertion fails before it.

### R5 — escalation reasons on stderr: keep, pin, document

- **The rule** (documented on `Engine::end_with`/`apply_with`): every transition into `Escalated` or
  `Paused` prints its reason exactly once, as `!! <reason>`, right before its `[state]` line; no caller
  prints `!! reason` itself (checked: the only `eprintln!("!! {…}")` left in `engine/runner.rs` are the
  context warnings). The reason is the `escalated`/`paused` event itself.
- **Changed to print** (they were quiet before R5, and before M10): an implementer failure without
  changes (`!! implementer failed: …`, after the turn summary), a review rejected three times (`!!
  InvalidOutput: …`), and the "no implementer left" pause of `implement()` (`!! implementer_quota: …`).
- **Kept quiet, because another line already says the reason:** the budget pauses (`!! budget reached
  during the turn …`, `!! admission cap reached …` — their event reason is just `budget`), and the pause
  after the last implementer switch (`!! implementer X: … → no fallback left`). `withdraw_final` stays
  quiet too: the refused approval's error is the reason line. Transitions inside `finish_and` still print
  no `[state]` line (owner decision Q1): the fix-cycle bound after a failed verify prints its reason once
  and no `[state]` line.
- Tests (`tests/m10_events.rs`): a blocked review prints `!! the reviewer blocked the task` once, right
  before `[state] Reviewing --review_blocked--> Escalated`; `--max-fix-cycles 1` with a failing verify
  prints `!! not accepted after 1 fix cycle(s) (last: verify failed)` once (no `[state]` line, Q1); the
  plan-policy bound once before `[state] Planning --plan_policy_violation--> Escalated`; the implementer
  failure and the rejected review once before their `[state]` lines (this last test fails before R5).

### R6 — cheap Low fixes

- **Typed escalation class** (`flight::escalation_class`, snake_case): the state machine's own
  escalations are classed by the event that led to them (`verify_failed`, `changes_requested`,
  `final_rejected`, `path_policy_violation` at the bound → `max_fix_cycles`; `review_blocked`;
  `plan_policy_violation` → `plan_policy`); else the engine passes the class (`Recorder::
  transition_classed`): the attempt's error class in `end_with`/`finish_and` (`escalate_attempt`'s
  class, `protected_config_changed`, `policy_violation`, `InvalidOutput` → `invalid_output`, a failed
  read-only turn's `ErrorClass`), `implementer_failed` for a failed implementer turn (its attempt keeps the
  turn's error class), `protected_config_changed` / `policy_violation` for `Engine::escalate`,
  `approval_mismatch` for a withdrawn final gate. The reason-prefix heuristic (`reason_class`, now also
  snake_case) is only the fallback — CLI-level transitions such as recovery's. Tests: unit
  `escalation_classes_are_typed_and_snake_case`; the blocked review and the fix-cycle bound give
  `review_blocked` and `max_fix_cycles` in `events.jsonl` (both were `other`).
- **Redaction fails closed:** `redact_event` redacts every string field in place, per variant; enum
  fields (states, statuses, accounting) stay typed. No serialise/deserialise round trip, so no fallback to
  the unredacted event. Test: a secret equal to `Completed` (the state stays, the texts are redacted).
- **`seq`:** `flight::scan` returns the highest `seq` and the line count, and counts a complete JSON
  object on an unterminated last line; `flight::next_seq` restarts above the line count with a warning
  when the last `seq` is implausible (> 2^53 − 1, e.g. `u64::MAX`); the in-process counter saturates
  instead of wrapping (`fetch_update` + `saturating_add`). Test `seq_is_checked_and_an_unterminated_
  object_counts`.
- **Recovery:** `drive` lists the running attempts before `recover()`; those it ended get a quiet
  `stage_finished`, and a killed turn's reservation a quiet `turn_finished` (`usage` absent,
  `reserved_tokens`). Test `attempts_ended_by_recovery_are_recorded` (a crash right after the implement
  attempt began, then `resume`).
- **No lock probe in `status` / `task show`:** `engine_cli::facts` takes the owner from
  `driven_owner` — the recorded owner whose PID and creation time are alive — like `watch`; the text
  `status` keeps its format. `active_owner` is `null`, never `""` (an owner row without a label shows
  `pid N`). Test `the_active_owner_comes_from_the_recorded_owner` (a hanging run, then killed).
- **`compile_only`:** `TaskFacts.compile_only` is `Option<bool>` — `null` when the frozen configuration
  cannot be read (after `task cleanup`); the text `status` prints the compile-only line only for
  `Some(true)`.
- **`task show --json`:** `usage[].budget_tokens` is renamed `counted_tokens` (the task-level
  `budget_tokens` keeps meaning the admission cap); README schema and the B1 test updated (a rename the fix
  list asks for).
- **Read-only store calls:** `Store::read` runs the closure in a `DEFERRED` transaction that is rolled
  back — it never takes the write lock. Used by `status`, `task show` (task, attempts, usage, gates),
  `usage`, `watch` and the owner lookup. Test `a_read_does_not_wait_for_a_writer` (another connection
  holds the write lock for 3 s; the read answers at once; a write inside a read is not kept).
- **C2 notes corrected** (replayed streams always carry a `Usage` event); no behaviour change.

### Round 2

From `M10-review-fixes-2.md` (re-review of R1–R6 at `047a12d`).

#### R7 — events and engine

1. **No duplicate `turn_finished` after recovery:** `drive_inner` records the usage row ids before
   `recover()`; `recovered_events` emits a reservation's `turn_finished` only for a row recovery
   inserted. The crashed process had already emitted its own rows. Test
   `recovery_does_not_repeat_a_turn_the_crashed_process_recorded` (`TORYO_TEST_CRASH_AT=
   implement:before_partial_commit` after a failed turn with changes; two `turn_finished` before the fix).
2. **Redact, then shorten:** `drive` passes the whole error text; the recorder redacts every event and
   then applies the length limits (`flight::limit`: `run_ended.error` to 400 characters). Before, the cut
   came first and a secret crossing it left its prefix in the file, `watch` and the toast. Unit test
   `a_long_error_is_redacted_before_it_is_shortened` (the secret starts 5 characters before the cut).
3. **A final-gate rejection at the bound prints its reason:** `reject_now` passes `reason_line = true`
   when the transition goes into `Escalated` (`!! not accepted after 0 fix cycle(s) (last: final gate
   rejected: …)` right before `[state] WaitingFinalApproval --rejected--> Escalated`). The stale doc of
   `escalate_attempt` ("the caller prints the reason") is fixed. Test
   `a_rejection_at_the_bound_prints_its_reason_once`. **Deviation 6 now has no exception left in the
   engine** except the budget pauses and the last implementer switch, whose own lines say the reason.
4. **`watch` stop rule:** done = 2 s idle **and** no live driver, whatever the state — a drive holds the
   lock until after its `run_ended`, so under `--yes` `watch` no longer exits at `WaitingFinalApproval`
   while `approve_now` is still at work. The two-poll rule is the small `flight::DonePolls` struct with a
   unit test (`done_must_hold_on_two_polls_in_a_row`); `watch_stops_on_a_task_that_does_not_move_on_
   its_own` now expects "go on" for a waiting state that is still driven.
5. **Panic → `run_ended { error: "panicked" }`:** a drop guard (`RunEndGuard`) in `drive` emits it when
   a panic unwinds through the drive (`std::thread::panicking()`); the exit code stays 101. New test hook
   `TORYO_TEST_PANIC_AT=<point>` (next to `TORYO_TEST_CRASH_AT`, same points). Test
   `a_panic_still_ends_the_record`.
6. **`seq` restart:** `flight::next_seq(&seqs)` restarts at `max(highest plausible seq, line count) + 1`
   (`[500, 501, u64::MAX]` → 502, not 4); `flight::seqs(path)` lists the file's `seq`s (`scan` keeps its
   shape).
7. **Redaction coverage:** `every_string_field_of_every_variant_is_redacted` puts a secret into each
   string of each variant's JSON in turn (a value that no longer parses is an enum, not text) and checks no
   sink sees it — 18 text fields today; a variant must be in the test's list (it checks all 12 kinds).
   This test passes before the fix too (the per-variant redaction of R6 was complete): it guards the future.
8. **`is_alive` on access denied:** `OpenProcess` refused with `ERROR_ACCESS_DENIED` (an elevated or
   other-user process) now counts as alive: it retries with `PROCESS_QUERY_LIMITED_INFORMATION` alone to
   check the creation time and `STILL_ACTIVE`, and if even that is refused, the process exists → alive.
   Windows test `a_process_that_refuses_access_is_alive` on the System process (PID 4), which a
   non-elevated run cannot open with both rights; an elevated run says "nothing to test" and passes.
9. **Missing test** `a_forced_final_gate_raises_no_toast` (`approve --force --continue --notify` from a
   blocked review): no toast. It passes before this commit too — the fix was in R1.

#### R8 — usage and notify

1. **All billed groups unpriced:** `est_complete` is `Some(false)` whenever a group billed per token has no
   price — also when nothing could be estimated (it was `null`, and the total read `—`, the subscription
   label). The text total then reads `no price (N group[s])`; with an estimate, `est. $x + no price (N
   groups)` as before. Subscription-only reports keep `null`. Test `r8_a_lone_unpriced_billed_group_is_
   not_a_complete_total` (one `api` group with `unknown model`); the R3 test's `null` expectation for
   two unpriced groups is now `false` (the fix itself).
2. **The dedup test tests dedup:** `r3_an_unpriced_model_is_listed_once` interleaves A, B, A and expects
   `[A, B]` (`dedup()` kept A twice).
3. **Reservations and killed turns apart:** `totals.includes_reservations` is true only for real
   reservations; the new `totals.includes_unreported` for rows that include killed turns' tokens. Both
   make the total `~est.`; the footer says "`~est.` = includes reservations (priced as uncached input) or
   killed turns' tokens".
4. **Restored assertion** `assert!(!enabled(true, true, on));` ("clap refuses the pair; still off").
   **Forced test changes of R4, declared here:** the AC-2 test `hostile_text_is_passed_as_data` was
   rewritten for `-Command` (the constant arguments are now `[-NoProfile, -NonInteractive, -Command,
   SCRIPT]`, before `-EncodedCommand <base64>`), and `base64_matches_rfc_4648` was deleted with the
   base64 encoder it tested; this assertion was dropped by mistake in the same commit.
5. **The error is printed before the toast wait:** `cli_main` builds the exit code and prints `error:` /
   `task_active:` first, then `notify::wait_pending()`. No test: the difference is only when the text
   appears (up to 10 s earlier), not what it says — not practical to assert.
6. **`spawn_detached` input checks:** `process::windows::check_spawn_input` refuses a NUL in an argument,
   a variable name or value, an empty name, and `=` in a name after its first character (`=C:`-style names
   stay allowed) — what `std::process::Command` refused. The environment is now `OsString` pairs
   (`env_block_os`), so a parent variable that is not valid Unicode is passed on (it was dropped).
   `Toast`'s fields are private (`title()`/`body()` read them), so every toast goes through `Toast::new`
   → `clean`. Tests `detached_spawns_refuse_what_command_refuses`, `a_non_unicode_parent_variable_is_kept`
   (Windows). Checked here: `notify-test` and `--dry-run` still exit 0.
7. **`notify-test` failure text:** "the toast script failed (exit N) — e.g. Constrained Language Mode
   (AppLocker/WDAC) or notifications disabled for Windows PowerShell". No test (it needs a failing
   script).
8. **Title length:** `Toast::new` shortens the title to 200 characters like the body (unit assertion in
   `the_body_is_capped`).

### Round 3

From `M10-review-fixes-3.md` (re-review of R7/R8 at `45c60b1`; last round before merge).

#### R9

1. **A stale owner no longer looks alive.** `is_alive`: when `OpenProcess(QUERY_LIMITED | SYNCHRONIZE)`
   is refused, it retries with `PROCESS_QUERY_LIMITED_INFORMATION` alone and checks the creation time
   (an elevated toryo of the same user grants that right); if **that** is refused too, the process is
   **not alive** — a toryo owner runs as the same user, so a process refusing even the limited right is
   someone else's (e.g. a SYSTEM process that reused a hard-killed owner's PID), which made `watch` hang and
   `status` show `ACTIVE` forever. The PID-4 test (`access_denied_is_decided_by_the_limited_right`) adapts:
   on this machine the System process refuses both rights → now "not alive" (it was "alive"); where the
   limited right is granted, the creation time decides (a wrong one → not alive).
   **Elevated-`status` check: not done.** This session runs at Medium integrity (not elevated) and UAC
   prompts for consent (`ConsentPromptBehaviorAdmin = 5`): starting an elevated `toryo run` needs a click on
   the UAC dialog, which cannot be done unattended (and toryo must not bypass UAC). Moved to Open with the
   procedure.
2. **Docs behind R8:** README and HUONG-DAN §5.8 list the `totals` fields of `toryo usage --json`
   (`exact_tokens`, `provisional_tokens`, `reserved_tokens`, `est_cost_usd`, `est_complete`,
   `unpriced_groups`, `includes_reservations`, `includes_unreported`, `vendor_reported_cost_usd`) and the
   total's text forms. `Report.est_complete`'s doc comment is corrected. **New:** with per-token groups and
   no price table the total reads `no price table` (it read `—`, the subscription label); unit test
   `r9_no_price_table_is_said_in_the_total`.
3. **The redaction coverage test fails for a new variant or field:** `fields_named` is an exhaustive
   `match` over `EngineEvent` that names every field (no wildcard, no `..`) — a new variant or field does
   not compile until it is added there; the samples must cover all its arms (`VARIANTS`), every
   `Option<String>` is `Some`, and the string walker descends into arrays too.
4. **No second panic while unwinding:** `Recorder::dispatch` reports a sink failure with
   `let _ = writeln!(std::io::stderr(), …)`, and `StderrSink` writes with `writeln!(…)?` (a closed stderr is
   that sink's error, reported once) — `eprintln!` panics when stderr is gone, and a panic during
   `RunEndGuard`'s unwinding would abort. Test `a_failing_sink_while_unwinding_does_not_panic_again` guards
   the path (it cannot fail first: stderr is present in tests).
5. **`notify-test` tells the real cause:** blocked notifications do not make `Show()` fail, so the script
   now reads `$notifier.Setting` after the dry-run exit and exits `5` (`notify::EXIT_DISABLED`) when it is
   not `Enabled`; `notify::test_message` maps 5 to "notifications are disabled for Windows PowerShell
   (Settings → System → Notifications)" and other codes to "the toast script failed (exit N) — e.g.
   Constrained Language Mode (AppLocker/WDAC)". The drive path stays fire-and-forget (the exit code is not
   read). The constant-command-line and no-double-quote tests stay green; new unit test
   `notify_test_tells_disabled_notifications_apart`. Checked here: `notify-test` exit 0 (notifications are
   enabled on this machine), `--dry-run` exit 0.
6. **Environment block order:** `process::windows::env_name_order` = `CompareStringOrdinal(a, b, TRUE)` on
   the UTF-16 names (std's `EnvKey`), `windows-sys` feature `Win32_Globalization` (no new crate). Test
   `environment_names_sort_ordinally_ignoring_case` (`path`/`Path` equal, `SSA` < `ßX` < emoji < `ﬀX`; the
   old Rust uppercase put `ﬀX` first as `FFX`).
7. **`check_spawn_input` also rejects a NUL in the program path** (new first parameter).
