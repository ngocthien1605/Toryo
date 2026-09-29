# M10 notes — decisions and deviations while building M10 (`M10_task.md`)

Every entry: what was decided, why, and where. The owner folds these into plan v0.9.

## Progress

Branch `m10/flight-recorder` (from `main` @ `5adf204`). One commit per task.

- [x] docs: owner verification E1, E7 (M9) — the owner's uncommitted notes, committed alone
- [x] A1 — `EngineEvent`, sinks, `events.jsonl` flight recorder
- [x] A2 — the engine's CLI lines rendered from events
- [x] B1 — `status --json`, `task show --json`
- [x] B2 — `toryo watch <ID>`
- [x] C1 — `prices.toml` + `toryo usage <ID>`
- [ ] C2 — scripted turns in mixed runs report zero usage
- [ ] D1 — Windows toast
- [ ] E1 — docs, morning summary

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
