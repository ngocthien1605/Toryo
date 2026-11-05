# M17 — Red-green proof gate and best-of-N implementer candidates

Milestone M17 (repository numbering, after M16). Task ids in commits: `M17-A1` … (Part A, red-green),
`M17-B1` … (Part B, best-of-N), `M17-Z1` … (closing). Not to be confused with **review M17** of
`docs/REVIEW-2026-10-01.md` (manifest paths), which is always written "review M17".

This file is the full specification. The master prompt (`docs/tasks/M17/MASTER-PROMPT.md`) tells the session
how to work; this file says **what** to build and when it is done. The acceptance-criteria section below is in
Toryo's own `TASK.md` format (it parses with `checks::parse_acceptance_ids`), so the same file can later be fed to
`toryo task create` for dogfooding.

## Goal

1. **Red-green proof gate.** A check gate can prove that the task's tests are real. For each acceptance criterion
   (`AC-n`) the implementer names the test(s) that prove it. Toryo runs every named test against the **base commit
   with the task's test files laid over it**, where it must **fail** (red). It then runs the same tests against the
   **checkpoint commit**, where they must **pass** (green). "Done" is then proven by code, not claimed by an agent.
2. **Best-of-N implementer candidates.** The first Implementing stage of a task can fan out to 2–4 candidate
   implementers (e.g. Codex and Claude). Each works in its own worktree and branch, and their agent turns run at the
   same time. Every candidate is checkpointed and verified by the check gates (including red-green). The winner is
   chosen by the gates first, then by one read-only compare turn of the reviewer seat. The winner becomes the task's
   checkpoint, and the normal flow continues from there (verify, environment gates, review, final gate).

Analogy for the reader: red-green is the inspector who checks the beam **fails** the load test without the new
bracket and **passes** with it. Best-of-N is 相見積もり (*aimitsumori*): two or more craftsmen build the same joint
and the master keeps the best one.

## Owner decisions (2026-11-05, asked before writing this file)

These are settled. Do not ask again. Record them in `docs/DECISIONS.md` as X47 onward (one line each), with the
code location once it exists.

| # | Decision |
|---|---|
| D-1 | Red-green uses a **test overlay**, not a new test-first agent stage: red tree = base tree + the task's changed test files (by globs). |
| D-2 | The AC link is **per AC and required**: every AC needs ≥ 1 proving test, unless `TASK.md` lists it under `## Proof exempt` with a reason. |
| D-3 | Best-of-N selection: **check gates first, then one read-only reviewer compare turn** when ≥ 2 pass. One pass: it wins. None pass: best by score goes into a normal fix cycle. |
| D-4 | Best-of-N applies to the **first Implementing stage only** (fix cycle 0). Candidates' agent turns run **in parallel**. Gates stay serial under the machine verify lock. Fix cycles continue on the winner only. |
| D-5 | At most **4** candidates (2..=4). |
| D-6 | The session runs in **Claude Code on the web** (Linux container). Windows behaviour is proven by GitHub CI (`windows-latest`). |
| D-7 | Delivery: commit per task, push, open **one pull request** to `main`, and get CI green. **Never merge.** |

Decisions the session takes alone (where this file is silent) follow the rules in the master prompt: choose the
conservative option, write it down as an X-decision, and list it in `docs/M17-NOTES.md` under
"Decided without the owner".

## Background: where things are today (read before coding)

- Flow and states: `src/engine/state.rs` (pure state machine; `EscalationClass`, `MACHINE_CAUSED`).
- Stages: `src/engine/runner/implement.rs` (implementer turn, fallback chain, kept work), `checkpoint.rs`,
  `stage_verify.rs` (preflights, verify lock, verify checkout, check gates, environment gates, `VERIFY_RESULT.json`),
  `review.rs`, `read_only_turn.rs` + `read_only.rs` (read-only turns and putting their writes back), `anchor.rs`
  (X24 anchor, X31 HEAD guard), `budget.rs`, `machine_locks.rs`, `gate_decisions.rs`.
- Engine core and unit tests: `src/engine/runner.rs`. Store: `src/engine/store/` (`schema.rs` is version 3 → this
  milestone makes it **4**; `attempts.rs`, `tasks.rs`, `usage.rs`). Flight recorder: `src/engine/flight.rs`.
  Views: `views.rs`, `dashboard.rs`, `render.rs`. Recovery: `recovery.rs`. Task create/cleanup: `task.rs`,
  `worktree.rs`, `commands.rs`. Verify checkout and mutation checks: `verify.rs` (**the export comparison is
  untouchable without a review round**, X15). Gate preflight: `gates.rs`. M18 entry-script rules: `checks.rs` and
  `cmdline.rs` (**do not change the rules**, only call them).
- Config: `src/config/types.rs` (`CommandSpec`, `Role`, `Pipeline`), `src/config/validate.rs`, `effective.rs`.
  Org chart: `src/team.rs` (`GateDecl`, `Seat`, `gate_command`). TASK.md parsing: `checks::parse_acceptance_ids`.
- Agents: `src/agents/claude.rs`, `codex.rs`, `hub.rs`, `fake.rs` (scripted agents keyed by `role` or
  `role@profile`). Contracts: `src/engine/contracts.rs` + `schemas/*.schema.json`.
  `schemas/implementation.schema.json` lists every property under `required` with `additionalProperties: false`.
  Keep new schemas in that style, which is safe for Codex structured output.
- `toryo goal split` (`src/engine/goal.rs`) is the model for a read-only turn with its own schema, a throwaway
  checkout and Toryo deciding what is written.
- Rules of the house: `docs/STATUS.md` §3. One task = one commit `area: what (M17-A3)`. Every fix starts with a
  test that fails before it. Never weaken or delete a test. Never run real `claude`/`codex` (fake agents only).

## Part A — Red-green proof gate (design)

### Configuration

A `kind: check` gate (org chart `gates:`) and a `commands.verify` entry (toryo.yaml-only mode, via `CommandSpec`)
accept an optional block:

```yaml
gates:
  unit:
    kind: check
    program: cargo
    args: [test, --locked]                # the normal run, as today (green, whole suite)
    timeout: 20m
    red_green:
      tests: ["tests/**", "src/**/tests.rs"]   # globs (repository-relative, case rules as the fence): which paths are test files
      args: [test, --locked, "{test}"]          # one run per declared test id; `{test}` exactly once, whole element or inside one ("--filter=Name~{test}")
      green_output_must_match: '(?m)^test result: ok\. [1-9]'   # optional regex; a green run must match it (catches "0 tests ran")
      max_tests: 32                              # optional, 1..=64, default 32: total distinct test ids per checkpoint
```

- It is not allowed on `kind: environment` or with `tool: xpp`. `{test}` must not appear in `program`. Unknown keys
  are refused. Each refusal names the key and the gate.
- `red_green.args` gets the same M18 entry-script protection as `args` (call the existing `checks.rs` functions;
  `{test}` is a word that names no file).
- `toryo config show`, `toryo gate list` and `toryo doctor` show the block.
- `toryo gate run NAME` runs only the gate's own `args` and prints one line: red-green runs only inside a task.

### The proof map (implementer output)

`IMPLEMENTATION.json` gains `proof`: `[{ "ac": "AC-1", "tests": ["calc::adds_two", "calc::adds_negative"] }, …]`.
It is added to `required` in `schemas/implementation.schema.json` (an empty array is fine), so the schema stays
strict-mode safe. The engine accepts an output **without** `proof` (treated as `[]`) when no red-green gate is in
the implementer's `before_handoff`, so every existing fake script and test keeps passing unchanged. The implementer
prompt explains the proof rule **only** when such a gate exists. It names the gate, the test globs and the ACs that
are exempt.

Test id rules (checked by Toryo before any run). The ids come from an agent and will be passed as arguments,
sometimes to a `.cmd` wrapper, so they must be inert:

- 1..=200 bytes, ASCII only, charset `[A-Za-z0-9_:./\-\[\]=,@+#]`, first char not `-` or `/`. No whitespace,
  quotes, `% ! ^ & | < > ; $ ( ) { } \`` or backtick.
- They are passed to the program as argv, never through a shell Toryo builds.

### `## Proof exempt` in TASK.md

A new optional section, parsed at `task create` and frozen with the task. Each item is `- AC-n: reason`. An unknown
id, a duplicate, or a missing reason is refused at `task create` with the line number. `parse_acceptance_ids`
keeps its behaviour: the new section is outside `## Acceptance criteria`.

### Run order inside the gate (it takes its place in `before_handoff` order)

1. **Validate the proof map** (no process is started when it is invalid; the result is a failing gate with fix
   feedback naming every problem):
   - every non-exempt AC has ≥ 1 test;
   - no unknown `AC-n`;
   - no AC listed twice;
   - no empty `tests`;
   - every id is valid;
   - distinct ids ≤ `max_tests`.
   An exempt AC may still be mapped; its tests then run. One test may prove several ACs: it runs once and the
   result counts for each.
2. **Green, whole suite:** the gate's own `args` on the checkpoint verify checkout, exactly as today.
3. **Green, per test:** each distinct id with `{test}` substituted, in the same checkout. It must exit 0 within
   the timeout and match `green_output_must_match` when set.
4. **Build the red commit.** Start from the base commit's tree (`tasks.base_commit`) and replace every path that
   matches `red_green.tests` by its checkpoint state (added, modified or deleted; blobs byte-exact). Parent = base.
   It goes on **no branch and no ref**. Use Git plumbing with a private index file, under
   `GIT_NO_REPLACE_OBJECTS=1` (X30). An empty overlay is allowed: a bug-fix test may already exist and fail on base.
5. **Red, per test:** check the red commit out clean in `<verify_root>/<task>/red`, at the same boundary as the
   verify checkout (Low integrity on Windows), still under the verify lock. Each id must end with a **non-zero
   exit**. A timeout counts as red but is recorded `timed_out: true` and flagged to the reviewer. A run that exits
   0 fails the gate: "test `X` (AC-2) passes without the change — it proves nothing". A program that cannot start
   is the existing machine stop `gate_unavailable` (no fix cycle).
6. **Mutation check of the red checkout** after its runs. It must equal the red commit, else `verify_mutated_tree`,
   exactly as for the verify checkout.

A failure at any step is a normal failing check gate: it is one gate retry (`max_gate_retries`), and the feedback
names the AC, the test, the side (red/green), the exit code and the output tail. Stop, Ctrl+C and crash behave as
during any check gate: the attempt is interrupted, and the verify runs again on resume. After a fix cycle the red
commit is rebuilt from the new checkpoint.

### Results and evidence

- `VERIFY_RESULT.json` gains `red_green` (absent when the task has no red-green gate; existing keys unchanged). It
  holds:
  - `gate`, `base`, `red_commit`;
  - `overlay` (≤ 50 relative paths) and `overlay_count`;
  - `acs: [{ ac, exempt, tests: [{ id, green: {exit, timed_out, ms}, red: {exit, timed_out, ms} }] }]`;
  - `proven`, `total`, `pass`.
  Paths are relative: no absolute user paths go to the vendor (cf. N-12).
- The reviewer's pack and prompt present the red-green evidence per AC. The reviewer must still answer every AC;
  the evidence is input, not a replacement.
- `toryo status` / `task show` (text and `--json`: `red_green: { proven, total, exempt, pass }` or `null`) and the
  final-approval screen show it.
- Flight recorder: one printed event `red_green_finished { gate, proven, total, pass }`. Each run is also a
  `verify_command` entry, as today.

## Part B — Best-of-N implementer candidates (design)

### Configuration

- Org chart: `seats.implementer.candidates: [codex, claude]`. Toryo.yaml mode: `roles.implementer.candidates`.
- 2..=4 entries. Each must be a machine profile. Repeats are allowed (two independent sessions of one profile).
- It is refused on `planner` / `reviewer`, with fewer than 2 or more than 4 entries, or with an unknown profile.
- `fallback` stays allowed and is used only on the single path.
- Frozen at `task create`; shown by `config show` / `task config`. `doctor` and the R51 evidence check cover every
  candidate profile.
- `task config --reload` may change it only while the fan-out has not started (afterwards: refused with a message).

A task whose frozen configuration has no `candidates` behaves **exactly** as before.

### Flow

```
plan approved → [fan-out]  c1 … cN worktrees + branches at base
                → N implementer turns at once (one per candidate)
                → per candidate: post-turn checks + checkpoint (own worktree)
                → per candidate: check gates incl. red-green (serial, verify lock, own verify checkout)
              → [select]   1 pass: it wins · ≥2 pass: reviewer compare turn · 0 pass: best score → Fixing
              → [promote]  toryo/<ID> := winner's checkpoint commit (same SHA)
              → normal Verifying (all check gates + environment gates) → Reviewing → final gate
```

- **Worktrees and branches:**
  - Candidate `k` gets worktree `<worktrees_root>/<ID>.c<k>` and a new branch `toryo-candidate/<ID>/c<k>` at the
    base commit. `.` cannot occur in a task id, and `toryo/<ID>/…` would clash with the ref `toryo/<ID>`.
  - Both are recorded in the store **before** they are made.
  - A failure or Ctrl+C during creation takes back what was made (X28 style).
  - A folder or branch that already exists and that Toryo did not record is refused by name, never deleted.
- **Turns:**
  - All candidate turns run concurrently inside the owner process, under the one task lock (X37 is about tasks;
    record the choice as an X-decision).
  - Each turn has its own profile, agent session, Job Object / process group, `TEMP` and context file.
  - Every candidate gets the same approved plan.
  - A stop request or Ctrl+C ends every running candidate turn.
  - The fake agent must be able to give two candidates of the **same** profile different scripts (e.g. addressing
    `implementer@profile#c2`). Extend `fake.rs` for that.
- **Post-turn, per candidate:** the same path as a single implementer turn, in that candidate's worktree:
  - protected / deny / fence checks, the HEAD guard (X31);
  - the checkpoint commit made by Toryo, with trailers naming the task and the candidate;
  - recovery never accepts a commit Toryo did not make.
- **Candidate outcomes:** `checkpointed`, `no_change`, `failed_turn` (quota, crash, invalid output — no fallback
  chain for a candidate), `disqualified(<class>)`, `interrupted`.
- **Escalation policy:**
  - `protected_config_changed`, `artifact_tampered` or `recovery_policy_violation` in **any** candidate escalates
    the **task**, and the other candidates' turns are stopped.
  - A machine-caused class escalates the task. A plain `toryo resume` then continues the fan-out without re-running
    candidates that already finished.
  - A fence/deny `policy_violation` or a `verify_mutated_tree` of a candidate's verify **disqualifies that
    candidate only**.
- **Verify per candidate:**
  - Check gates only, red-green included, in `<verify_root>/<task>/c<k>`, one after another under the machine
    verify lock (X36).
  - Environment gates **never** run for a candidate.
  - Each candidate keeps its own `VERIFY_RESULT.json` artifact (logical name per candidate).
- **Selection:**
  - Exactly one candidate passed all check gates: it wins (`by: only`).
  - Two or more passed: one read-only compare turn of the **reviewer seat** with a new
    `schemas/select.schema.json` (`winner` = a passing candidate id, `reason`, `ranking` of all passing ids; strict
    style). Its pack holds each passing candidate's `DIFF.patch`, `IMPLEMENTATION.json` and verify summary
    (red-green included) as **untrusted data**.
  - An invalid answer (unknown id, a failing candidate, a malformed answer) gets one correction turn, then the
    deterministic score decides (`by: score`). A failed turn → `by: score`.
  - Writes of the read-only turn are put back as `read_only.rs` does.
- **Deterministic score** (also the tie-break; document it and test each rung):
  1. all check gates passed;
  2. more ACs proven by red-green;
  3. fewer changed lines (added + deleted, `git diff --numstat` base..checkpoint);
  4. fewer tokens;
  5. lower candidate index.
- **No candidate passed, ≥ 1 checkpointed:** best by score wins. The task enters `Fixing` with that candidate's
  verify feedback (one gate retry).
- **No candidate checkpointed:** the task takes the normal single-implementer path (primary + fallback chain),
  with one note line. There is no second fan-out.
- **Promotion:**
  - The task worktree must be at base, clean, on `toryo/<ID>`, with no unfinished Git operation (X31), else it
    escalates.
  - The branch `toryo/<ID>` is moved to **exactly** the winner's checkpoint commit (same SHA, no new commit). The
    worktree ends clean at it. The anchor (X24) is the winner.
  - Then the normal flow: Verifying (every check gate again, then environment gates) on that commit. Re-running
    the check gates is deliberate and conservative; reusing candidate results is an Open item.
  - Then Reviewing (full review) and the final gate.
  - Fix cycles use the winner's profile (resuming its agent session where the adapter supports it), then the
    configured fallback chain.
  - `reviewer-differs-from-builder` judges the winner's profile.
- **Losers:** their worktree folders are removed right after selection. Their branches are kept and named in the
  output. `task cleanup` removes any candidate worktree still present, and only the folders the store recorded
  (extend X44). Branches are kept, as for the task branch.
- **Budget:**
  - Admission reserves N candidate turns.
  - A budget that admits fewer than N but ≥ 2 runs that many (one note line). Fewer than 2 → single path (note).
  - A live budget interrupt during the fan-out pauses the task. `budget set` + `resume` continues the unfinished
    candidates only.
- **Store:**
  - Schema version **4**: candidate data (e.g. a `candidates` table: task, k, profile, worktree, branch, outcome,
    head, verify pass, tokens, winner, by; and/or a `stage_attempts.candidate` column — `UNIQUE(task_id, stage,
    attempt)` must still hold).
  - A version-3 database is raised to 4 on open (as 2 → 3 was).
  - `toryo usage` shows candidate turns separately (`implement#c1`).
- **Observability:**
  - Printed events `candidates_started { n, profiles }`, `candidate_finished { k, outcome, head, gates_passed }`
    and `candidate_selected { k, by }` in `events.jsonl` and `watch`.
  - `status` / `task show` text and `--json` `candidates: [ { id, profile, outcome, head, verify, tokens, winner } ]`
    (`null` when there is no fan-out).
  - Dashboard `now:` shows fan-out progress (`candidates 1/2 verifying`). It speaks Toryo's words only (X35).
  - The final-approval screen names the winner, how it was chosen, and the losers' branches.

## Acceptance criteria

### Part A — red-green proof gate

- AC-1: A `kind: check` gate (org chart) and a `commands.verify` entry (toryo.yaml mode) accept the optional `red_green` block (`tests`, `args`, `green_output_must_match`, `max_tests`) as specified above; `toryo config show`, `toryo gate list` and `toryo doctor` show it; `toryo gate run` runs only the gate's own args and says that red-green runs only inside a task.
- AC-2: Loading refuses, naming gate and key: `red_green` on an environment gate or with `tool: xpp`; `{test}` missing from, or more than once in, `red_green.args`; `{test}` in `program`; empty `tests`; an invalid glob; an invalid regex; `max_tests` outside 1..=64; an unknown key. `red_green.args` gets the M18 entry-script protection of `args` without any change to the rules in `cmdline.rs`.
- AC-3: `IMPLEMENTATION.json` has `proof` (required in the schema, strict style); an output without `proof` is accepted when no red-green gate is in `before_handoff`, so every existing test and fake script passes unchanged; the implementer prompt explains the proof rule only when such a gate exists.
- AC-4: The proof map is validated before any red-green process starts; each problem (missing non-exempt AC, unknown AC, duplicate AC, empty tests, invalid test id per the charset and length rules, more distinct ids than `max_tests`) fails the gate with fix feedback that names it; no gate command runs for an invalid map.
- AC-5: An optional `## Proof exempt` section (`- AC-n: reason`) is parsed and frozen at `task create`; an unknown id, a duplicate or a missing reason is refused with its line number; `parse_acceptance_ids` keeps its existing behaviour and tests.
- AC-6: Green per test: each distinct id runs once with `{test}` substituted as argv (whole element or inside one, never through a shell Toryo builds) in the checkpoint verify checkout after the gate's own args passed; it must exit 0 within the timeout and match `green_output_must_match` when set.
- AC-7: The red commit is the base tree with every path matching `red_green.tests` replaced by its checkpoint state (added, modified, deleted; byte-exact blobs), parent = base, on no branch or ref, built under `GIT_NO_REPLACE_OBJECTS=1`; an empty overlay is allowed; it is checked out clean in `<verify_root>/<task>/red` at the verify boundary, under the verify lock.
- AC-8: Red per test: a non-zero exit is red; a timeout is red and flagged `timed_out`; an exit of 0 fails the gate with "passes without the change — it proves nothing" naming test and AC; a program that cannot start is `gate_unavailable` (machine-caused, no fix cycle).
- AC-9: After the red runs the red checkout must equal the red commit, else `verify_mutated_tree`; the protected-state scan after verify is unchanged.
- AC-10: A red-green failure is one gate retry with feedback naming AC, test, side, exit code and output tail; stop, Ctrl+C and crash during red-green runs leave the attempt interrupted and verify runs again on resume; after a fix cycle the red commit is rebuilt from the new checkpoint.
- AC-11: `VERIFY_RESULT.json` carries `red_green` (relative paths only) when the task has a red-green gate and is otherwise byte-for-byte as before; the reviewer pack and prompt present the evidence per AC; `status`, `task show` (text and `--json`) and the final-approval screen show proven/total/exempt; `red_green_finished` is recorded and printed by `watch`.

### Part B — best-of-N implementer candidates

- AC-12: `seats.implementer.candidates` (org chart) and `roles.implementer.candidates` (toryo.yaml mode) accept 2..=4 machine profiles (repeats allowed); refused on other seats, with fewer than 2 or more than 4, or with an unknown profile; frozen at `task create`; shown by `config show` and `task config`; covered by `doctor` and the R51 evidence check; `task config --reload` changes it only before the fan-out started.
- AC-13: A task without `candidates` behaves exactly as before: every existing test passes unchanged and its `events.jsonl`, `status --json` and `VERIFY_RESULT.json` keep their shape (new keys are `null` or absent).
- AC-14: Fan-out happens only at the first Implementing stage after plan approval; candidate k gets worktree `<worktrees_root>/<ID>.c<k>` on new branch `toryo-candidate/<ID>/c<k>` at the base commit, recorded before creation; a failure or Ctrl+C during creation takes back what was made; an existing unrecorded folder or branch is refused by name and never deleted.
- AC-15: The candidates' agent turns run concurrently (a fake-agent test proves overlap), each with its own profile, session, process containment, TEMP and context; a stop request or Ctrl+C ends every running candidate turn; the fake agent can script two candidates of the same profile separately.
- AC-16: Each candidate goes through the single-turn post-turn path in its own worktree (protected, deny and fence checks, HEAD guard, Toryo's checkpoint commit with task and candidate trailers); recovery never accepts a commit Toryo did not make; outcomes are `checkpointed`, `no_change`, `failed_turn`, `disqualified(<class>)` or `interrupted`, and a failed candidate turn never uses the fallback chain.
- AC-17: The escalation policy holds: a protected, tampered-artifact or recovery-policy class in any candidate escalates the task and stops the other turns; a machine-caused class escalates the task and a plain `resume` continues without re-running finished candidates; a fence or deny violation or a mutated verify tree disqualifies only that candidate.
- AC-18: Each checkpointed candidate runs the check gates only (red-green included), serially under the machine verify lock, in `<verify_root>/<task>/c<k>`, with its own verify-result artifact; no environment gate ever runs for a candidate.
- AC-19: Selection: one passing candidate wins with `by: only`; two or more get one read-only reviewer compare turn answering `schemas/select.schema.json` with the passing candidates' diffs, implementation reports and verify summaries as untrusted data; an invalid answer gets one correction turn and then the deterministic score decides (`by: score`); a failed turn falls to the score; writes of the compare turn are put back.
- AC-20: The deterministic score is, in order: all check gates passed, more ACs proven by red-green, fewer changed lines (numstat added + deleted), fewer tokens, lower index; each rung has its own test.
- AC-21: No passing candidate but at least one checkpointed: the best by score wins and the task enters Fixing with its verify feedback as one gate retry; no candidate checkpointed: the normal single-implementer path (primary + fallback chain) runs once with a note and no second fan-out.
- AC-22: Promotion checks that the task worktree is at base, clean, on `toryo/<ID>` with no unfinished Git operation (else escalation), then moves `toryo/<ID>` to exactly the winner's checkpoint SHA without a new commit; the anchor is the winner; the normal Verifying (all check gates, then environment gates), Reviewing and final gate follow; fix cycles use the winner's profile (resuming its session where supported) then the fallback chain; `reviewer-differs-from-builder` judges the winner's profile.
- AC-23: Losers' worktree folders are removed after selection and their branches kept and named; `task cleanup` removes any remaining candidate worktree, only folders the store recorded; this works after a crash in the middle of a fan-out.
- AC-24: Budget: admission reserves N candidate turns; a budget that admits 2..N-1 runs that many with a note; fewer than 2 takes the single path with a note; a live budget interrupt during fan-out pauses the task and `budget set` + `resume` continues only the unfinished candidates.
- AC-25: A stop or a crash at every point of the fan-out (worktree creation, turns, between checkpoint and verify, candidate gates, selection, promotion) is covered by a test: `resume` reaches the same result without re-running finished candidate turns, without a second promotion and without accepting a foreign commit.
- AC-26: The store schema is version 4 with the candidate data; a version-3 database (with existing tasks) is raised to 4 on open; `toryo usage` shows candidate turns separately with totals that keep their meaning.
- AC-27: Events `candidates_started`, `candidate_finished` and `candidate_selected` are recorded and printed by `watch`; `status` and `task show` (text and `--json` `candidates`, `null` without a fan-out), the dashboard `now:` line and the final-approval screen show the fan-out, the winner, how it was chosen and the losers' branches, in Toryo's own words only.
- AC-28: `toryo run A B --jobs 2` where both tasks have candidates completes without deadlock; check gates of all candidates of all tasks are serialised by the machine verify lock.

### Closing

- AC-29: Every behaviour above and every item of the "Edge cases" section below is a named test, with fake agents only, mainly in `tests/m17_red_green.rs` and `tests/m17_best_of_n.rs` (unit tests next to the code where that fits); red-green and best-of-N are also tested together (proven-AC count in the score).
- AC-30: `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, the same with `--no-default-features`, and `cargo test --locked` pass on Linux; GitHub CI is green on `windows-latest` and `ubuntu-latest`; no existing test was weakened or deleted.
- AC-31: Documentation is updated: `README.md` (configuration, gates, commands, the boundary section), `docs/HUONG-DAN.md` (in Vietnamese, new sections plus the §9.1 table), `docs/DECISIONS.md` (X47 onward), `docs/STATUS.md` (state, tests, open list edited in place), a new `docs/M17-NOTES.md` (record per task, an AC → test traceability table, "Decided without the owner", open items), and commented examples in `templates/` (org chart, `toryo.yaml.example`, dogfood).

## Edge cases (each one is a named test)

### Part A

1. A declared test already passes on the red commit (feature existed): the gate fails, "proves nothing".
2. A filter that matches no test: green whole-suite passes and per-test green exits 0. With
   `green_output_must_match` set the gate fails; without it the docs warn. Test both.
3. Test ids refused: leading `-` (`--nocapture`), leading `/`, a space, `;`, `&`, `|`, `%PATH%`, `$(x)`, a backtick,
   `"`, a newline, a non-ASCII char, 201 bytes, empty string. Each test also asserts that no process started.
4. Proof map problems: a missing AC, an unknown `AC-99`, a duplicate AC, an empty `tests`, more than `max_tests`
   distinct ids, all at once (every problem named in one feedback).
5. One test proving two ACs runs once per side, and both ACs count as proven.
6. An exempt AC with a mapping runs its tests. An exempt AC without a mapping is fine. An unknown exempt id and an
   exempt item without a reason are refused at `task create`.
7. Overlay: a test file added, modified, deleted and renamed on the checkpoint. Non-test changes are not overlaid.
   Globs that are too wide (`src/**`) make red ≈ green, and the gate fails "proves nothing".
8. An empty overlay (the test already exists and fails on base: a bug-fix task) passes.
9. CRLF test files under `eol=crlf` and an executable-bit change: blobs byte-exact in the red commit.
10. A red run that times out counts as red, `timed_out: true` in `VERIFY_RESULT.json` and in the reviewer pack.
11. The program is missing when red starts: `gate_unavailable`, no fix cycle, plain `resume` re-enters.
12. Test code that writes into the red checkout: `verify_mutated_tree`.
13. Stop / Ctrl+C during a red run, and a crash point inside the red phase: interrupted, verify runs again on resume.
14. A fix cycle after a red-green failure: the second checkpoint gets a new red commit; the old one is not reused.
15. `{test}` inside an element (`--filter=FullyQualifiedName~{test}`) and as a whole element.
16. A red-green gate listed before an environment gate: order kept. One after an environment gate: refused, as for
    every check gate.
17. A task without a red-green gate whose implementer still writes `proof`: ignored, no error, no `red_green` key.
18. A red-green gate in toryo.yaml-only mode (`commands.verify`).
19. A gate output over the 1 MB + 8 MB bound in a per-test run: bounded and redacted as today.
20. The red commit is on no ref after the run (`git for-each-ref` shows nothing new), and `task cleanup` leaves no
    red checkout behind.

### Part B

21. Two candidates with the same profile get distinct sessions and scripts; both checkpoint.
22. One candidate fails its turn (quota, then crash, then invalid output — three tests) and the other passes:
    `by: only`, with no fallback for the failed one.
23. All candidates make no change: the single path runs once with the note.
24. All candidates fail the gates: the best by score enters Fixing; the score's rung is visible in `status`.
25. An exact tie on every rung: the lower index wins.
26. The compare turn names an unknown id, then a failing candidate, then malformed JSON: one correction turn, then
    `by: score`. Also a compare turn that fails outright, and a compare turn that writes a file (put back).
27. The compare turn picks c2, and the later full review asks for changes: the fix cycle runs on c2's profile.
28. A candidate writes a protected file: the task escalates `protected_config_changed` and the other candidate's
    running turn is stopped (no orphan process: `agent_killed` recorded).
29. A candidate violates the fence: only that candidate is disqualified; the other wins.
30. A candidate's check-gate code mutates its verify checkout: that candidate is disqualified.
31. Ctrl+C during worktree creation, during turns, and a second Ctrl+C; `toryo stop` from another process during
    the fan-out.
32. Crash points:
    - after the first candidate's checkpoint, before the second's;
    - after selection, before promotion;
    - after promotion, before the state change.
    `resume` gives the same winner, one promotion, and no re-run of finished turns.
33. A machine-caused stop in a candidate's verify (`gate_unavailable`): the task escalates, a plain `resume`
    continues, and finished candidate turns are not re-run.
34. Budget admits 1 of 2 (single path), 2 of 3 (two candidates), and a live interrupt in the middle of the turns
    (Paused, then `budget set` + `resume`).
35. `task config --reload` changing `candidates` before the fan-out (applies) and after it (refused).
36. `task cleanup` in the middle of a fan-out (task not active) removes the candidate worktrees; `--force` is needed
    when a candidate worktree holds unsaved work.
37. A leftover candidate folder or branch that the store did not record: refused by name, nothing deleted.
38. The task worktree is dirty or HEAD moved before promotion: escalation, no branch move.
39. `toryo run A B --jobs 2` with candidates in both tasks: completes, and the verify lock serialises every
    candidate gate.
40. A version-3 database with an existing task opens and is raised to 4; that task finishes on the old path.
41. `status --json`, `task show --json` and `dashboard --json` shape with and without candidates (`null` vs array).
42. `toryo usage` lists `implement#c1` and `implement#c2`, and the task total equals the sum.
43. Red-green and best-of-N together: two passing candidates and the compare turn fails, so the one with more ACs
    proven wins on the score.

## Out of scope (write them under Open in `docs/STATUS.md` when they matter)

- Fan-out of fix cycles; more than 4 candidates; reusing a candidate's verify result for the winner's Verifying.
- Red-green inside an environment gate (e.g. D365FO SysTest needs a deployed environment); `tool: xpp` red-green.
- Detecting a candidate that writes into a sibling candidate's worktree. Document this as a limitation: the agents'
  own sandboxes (Claude `--add-dir`, Codex workspace-write) are the guard today.
- Any change to `cmdline.rs` rules or the export comparison in `verify.rs`.
