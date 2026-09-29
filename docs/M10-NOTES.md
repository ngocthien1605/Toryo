# M10 notes — decisions and deviations while building M10 (`M10_task.md`)

Every entry: what was decided, why, and where. The owner folds these into plan v0.9.

## Progress

Branch `m10/flight-recorder` (from `main` @ `5adf204`). One commit per task.

- [x] docs: owner verification E1, E7 (M9) — the owner's uncommitted notes, committed alone
- [x] A1 — `EngineEvent`, sinks, `events.jsonl` flight recorder
- [ ] A2 — the engine's CLI lines rendered from events
- [ ] B1 — `status --json`, `task show --json`
- [ ] B2 — `toryo watch <ID>`
- [ ] C1 — `prices.toml` + `toryo usage <ID>`
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

