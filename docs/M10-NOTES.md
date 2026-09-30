# M10 notes — decisions and deviations while building M10 (`M10_task.md`)

Every entry: what was decided, why, and where. The owner folds these into plan v0.9.

## Morning summary (2026-09-30)

**All eight tasks are DONE; none PARTIAL, none NOT STARTED.** Branch `m10/flight-recorder`, 9 commits on
`main` @ `5adf204`, not pushed:

| Commit | Task | State |
|---|---|---|
| `ca35047` | docs: owner verification E1, E7 (M9) — your notes, committed alone | — |
| `e17875c` | A1 — `EngineEvent`, sinks, `events.jsonl` flight recorder | DONE |
| `bb49186` | A2 — the engine's stderr lines rendered from events (same text) | DONE |
| `24679d4` | B1 — `status --json`, `task show --json` | DONE |
| `0f69359` | B2 — `toryo watch <ID>` | DONE |
| `80ed7a0` | C1 — `prices.toml` + `toryo usage <ID>` | DONE |
| `87d587d` | C2 — scripted turns of a mixed run report exact zero usage | DONE |
| `2ca9059` | D1 — Windows toast (`notify:`, `--notify`, `notify-test`) | DONE |
| (last) | E1 — README, HUONG-DAN §5.8, this summary (the templates came with C1 and D1) | DONE |

**Checks** (§0.2) green before every commit on this machine: `cargo fmt --check`, both `clippy -D warnings`,
`cargo test --locked`. Tests: **401 passed, 0 failed, 6 ignored** (before M10: 355 passed, 5 ignored; the new
ignored one shows a real toast). New test files: `tests/m10_events.rs`, `m10_views.rs`, `m10_usage.rs`,
`m10_notify.rs`, plus unit tests in `engine::flight`, `engine::views`, `engine::usage_report`, `notify`,
`config`. No real agent was run; the owner's configs and data dirs were not read.

**Deviations from `M10_task.md`** (details in the sections below):
1. The module is `src/engine/flight.rs`, not `events.rs`: `tests/architecture.rs` (R52) forbids the text
   `events::` in engine files (it means the vendor module `agents::events`); the test was not weakened.
2. `RunStarted { owner }`, not `{ generation }`: the envelope already has `generation`, and the flattened
   event would write the key twice.
3. `stage_started` for verify is emitted when its header is printed (after the checkout exists); a verify
   that escalates before that has `stage_finished` only.
4. The CLI now runs on a thread with an 8 MB stack: adding `watch` made the Windows debug build overflow the
   1 MB main-thread stack in clap's builder already on `--help` (found in B2, `main::CLI_STACK`).
5. The stderr engine lines now go through the task's secret redaction too (a reason that carried a profile
   secret was printed as is before). Otherwise the text is identical — checked against `main`'s binary on
   the same fake scenarios (335 stderr lines, A2).

**Open questions for you:**
1. Transitions the CLI never printed (inside `finish_and`, e.g. `Verifying --verify_passed--> Reviewing`)
   are in `events.jsonl` and in `watch`, still not on the run's stderr (`Envelope.quiet`). Print them too?
2. With `--yes` and toasts on, the gates still raise toasts although they are approved automatically — as
   the task says. Suppress toasts under `--yes`?
3. `vendor_reported_cost_usd` is shown next to the estimate but nothing fills it (the Claude adapter parses
   `total_cost_usd`, `StageOutcome` does not carry it). Worth carrying?
4. Reservations are priced as uncached input in the `~est.` cost (C1). Another rule?
5. CI: `notify_test_builds_the_toast` loads WinRT types in Windows PowerShell on `windows-latest`; if the
   image cannot, mark it `#[ignore]`. The Unix Ctrl+C path of `watch` compiles only on Linux CI (no Linux
   target here).

**Try first** (build first: `cargo build --release` — `bin\toryo.exe` is not rebuilt):
1. `toryo notify-test` — a toast "toryo — notify-test" should appear; if not, allow notifications for
   "Windows PowerShell" (Settings → System → Notifications). It was run once here: exit 0.
2. Copy `templates\prices.toml.example` to `%USERPROFILE%\.toryo\prices.toml`, put in your Foundry
   deployment names and contract prices, then `toryo usage <an existing task>` (older rows show the model
   from the frozen config).
3. On the next real task: `toryo run ID --notify` in one window, `toryo watch ID` in another; afterwards
   `toryo status --json` and `toryo task show ID --json`.
4. `scripts\m9-check.ps1 -Canary` again: the status line should no longer show `+~400000 reserved` (C2).

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
- [ ] R3 — `toryo usage` does not look complete when it is not
- [ ] R4 — toast hardening
- [ ] R5 — escalation reasons on stderr: keep, pin, document
- [ ] R6 — cheap Low fixes

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
  event at that point before M10 (e.g. `finish_and` transitions have never printed `[state]`,
  `ImplementOutcome::Failed` and a rejected review never printed `!! reason`). A2's stderr renderer
  skips quiet events so the text of a run does not change; the file and `watch` see everything.
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
  printed before an escalation or a reason-carrying pause (it is the `escalated`/`paused` event; the
  paths that never printed it emit it quiet).
- **Same text.** `reject_now` has always printed `--rejected-->`: the renderer maps `plan_rejected` /
  `final_rejected` to that. Unit test `events_render_the_text_the_engine_printed` compares each
  variant with the old format expressions. End to end, `main`'s binary and this one were run on the
  same fake scenarios (fix cycles with a failing and a passing verify, review changes, a final
  rejection that escalates at the fix-cycle bound, a protected-config escalation, an implementer
  failure without changes, resume with a note, a budget warning and the admission pause): after
  normalising paths, hashes and durations the 335 stderr lines are identical.
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
  scripted turn that ends — `complete`, `fail` (also the "no turn left" failure and an interrupt), or a
  replayed stream's terminal event — without having reported usage first reports **exact zero** usage
  (all four buckets 0) just before its end: the row is `exact`, 0 tokens, no reservation. A scripted
  turn with a `usage` step keeps its numbers. A pure fake run (no `only_profiles`) is unchanged: a turn
  without usage is `unknown` and R41 reserves (the R41 tests rely on that and pass unchanged).
- **Where.** `agents::fake` — `Shared.zero_usage` (from the script), `FakeSession.usage_seen` (per
  turn), `FakeSession::zero_usage_before(step)`. A turn that ends without a terminal step (`crash`, the
  steps running out) still reports nothing: it models a crash, and a crash's usage is unknown.
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
- **Open:** `notify_test_builds_the_toast` runs Windows PowerShell and loads WinRT types in CI
  (`windows-latest`). It passed here; if the CI image cannot load them, mark that test `#[ignore]`
  like the real-toast one — it is the only M10 test that needs more than the Rust toolchain and Git.

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
