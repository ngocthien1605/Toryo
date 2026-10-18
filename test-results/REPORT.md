# M16 acceptance tests — report

Branch `m16/acceptance-tests` (from `docs/status-after-m16` @ `1c5c46b`, which is `main` @ `5d0a4e1` + one STATUS
commit). Final run: `pwsh -NoProfile -File scripts/test-e2e.ps1 -Repeat 3` on 2026-10-19, **exit code 0** — every step
and every AT passed in all three repetitions; no temp folder was kept. `cargo test` in each repetition: **809 passed,
0 failed, 10 ignored** (the 4 pseudo-console tests are among the ignored; the script runs them itself). `cargo clippy
--all-targets -- -D warnings` (with and without `--no-default-features`) and `cargo fmt --check` are clean on every
commit.

## Results

| AT | Where | Result (3 runs) | Evidence |
|----|-------|-----------------|----------|
| AT-1 | Pester | PASS PASS PASS | exit 0; the `══════════ 3 tasks ══════════` block lists `RUN-A/B/C  Completed  (exit 0)`; `runs/<id>/runstore/SUMMARY.md` exists for each; `refs/heads/toryo/RUN-A/B/C` exist, none is an ancestor of `main`, `main` did not move. |
| AT-2 | Pester | PASS PASS PASS | `▶ run RUN-A RUN-B RUN-C — 2 at a time (…)` printed; every line between it and the end block matches `^RUN-[ABC] │( \|$)` with exactly one prefix; ≥ 2 switches between tasks (interleaved); stdout empty. |
| AT-3 | Pester | PASS PASS PASS | sweep over `run_started`/`run_ended` of the three `events.jsonl`: at most 2 open at any `ts`; RUN-C's `run_started` is after the first `run_ended` of RUN-A/RUN-B. |
| AT-4 | Pester | PASS PASS PASS | the `machine:verify` holds (`lock_acquired`…`lock_released`) of the three tasks never overlap; each waiter's `lock_waiting.holder` is the task holding the lock at that `ts`, before that holder's `lock_released`; the waiter's `lock_acquired` is after it; the console shows `RUN-x │    waiting for lock machine:verify (held by RUN-y)`. |
| AT-5 | Rust / ConPTY | PASS PASS PASS | `run INT-A INT-B INT-C --jobs 2 --yes --fake-agents fake-slow.json` in a pseudo-console; one `0x03` once INT-A and INT-B have `agent_spawned` for `implement`; the process ends by itself in < 15 s (the test bounds it; about 1 s in the spike runs), **exit code 1**; end block `INT-A  Implementing  (exit 0)`, `INT-B  Implementing  (exit 0)`, `INT-C  New  (not started)`; `■ Ctrl+C — the running tasks stop; not started: INT-C`; INT-A/B: `cancel_requested {source: ctrl_c}`, `agent_killed {stage: implement, reason: stop}`, last event `run_ended {outcome: stopped, exit_code: 0}`; INT-C: no `run_started`, no `INT-C │` line. |
| AT-6 | Rust / ConPTY | PASS PASS PASS | after the AT-5 scenario: the processes of both children (`run_started.pid`) and of every agent with a pid are gone (fake agents have none); `locks/INT-A.lock`, `locks/INT-B.lock` and `locks/machine/verify.lock` can be taken; no `index.lock` in the worktrees' Git folders; `toryo status` gives the next command for each. |
| AT-7 | Rust / ConPTY | PASS PASS PASS | after the AT-5 scenario, `toryo resume INT-A --yes --fake-agents fake-full.json`: exit 0, `INT-A: Completed`; the resume's events have no `stage_started {stage: plan}`, an `implement` attempt 2, and end `run_ended {state: Completed, outcome: done, exit_code: 0}`. |
| AT-8 | Pester | PASS PASS PASS | the AT-1 command again on the same workspace: exit 0, all three `Completed (exit 0)`; no new `agent_spawned` and no new `stage_started` in any `events.jsonl`. |
| AT-9 | Rust / ConPTY | PASS PASS PASS | as AT-5 with `fake-slow-plan.json`, the key during `plan`: ends by itself in < 15 s with exit code 1, `INT-A/B  Planning  (exit 0)`, `cancel_requested {source: ctrl_c}`, `agent_killed {stage: plan}`, INT-C never started, nothing left (AT-6's checks), and no `stage_started {stage: implement}`. |

Durations are in `test-results/summary.md` (a Pester row is the time of its `It` only; the shared `toryo run` of
AT-1…AT-4 runs in the `BeforeAll`, about 40 s).

## Bugs found and fixed

| Commit | Bug | Found by |
|--------|-----|----------|
| `7cbd092` | **A toryo started with Ctrl+C ignored never saw Ctrl+C** (STATUS N-16). Windows passes "ignore Ctrl+C" to children (`start /b`, CREATE_NEW_PROCESS_GROUP, a runner or a shell that ignores it); toryo added its handler without taking the key back, so one Ctrl+C on `toryo run A B C` neither stopped the tasks nor ended the parent. Both handlers now call `SetConsoleCtrlHandler(NULL, FALSE)` first. | AT-5/6/7/9 failed under this session's runner ("toryo did not end within 15s of one Ctrl+C"). |
| `e646ead` | **A waiter could name the verify lock's holder before the holder's own log said it took the lock**: the holder file was written right after the OS lock, `lock_acquired` only afterwards (seen: RUN-B `lock_waiting (holder RUN-A)` at 16:46:09.198, RUN-A `lock_acquired` at 09.253). The holder file is now written after `lock_acquired` is recorded; a waiter that finds no name yet asks again (≤ 1 s). | AT-4 (holder must hold the lock at the waiter's `ts`). |

Also (test infrastructure, not the product): `conpty` — used by `expectrl` — sets no `STARTF_USESTDHANDLES`, so a
child it starts writes to the parent's redirected stdout/stderr instead of the pseudo-console; the tests clear their
own standard handles for the moment of the spawn. `portable-pty` (the document's fallback) does set the flag, but its
child failed to start here (0xC0000142), so `expectrl` was kept.

## Differences between the document and the code

1. **Branch.** There is no "current M16 branch": `m16/multi-task` was merged as pull request #9. The work is on the new
   branch `m16/acceptance-tests`, cut from `docs/status-after-m16` (its STATUS pull request is not merged yet).
2. **Event log (2.1).** It existed already (the M10 flight recorder) at `runs/<ID>/events.jsonl` — not
   `runs/<RUN-ID>/runstore/events.jsonl` — and was extended, not replaced. Envelope: `v, seq, ts, task, generation,
   type` (not `run` / `event`). Names: `state` → `state_changed {from, to, event}`; `lock_wait` → `lock_waiting {lock,
   holder}`; `run_finished` → `run_ended {state, next_action, …, outcome, exit_code}`. New (X39): `lock_acquired`,
   `lock_released`, `cancel_requested {source: ctrl_c|stop}`, `agent_spawned {stage, attempt, profile, pid}`,
   `agent_killed {…, reason: stop|limit}`, `run_started.pid`. They are never printed, and stay out of the rendered
   `watch` and the dashboard's `last:` (2.2). `ts` had 100-ns precision; it now also strictly increases per process.
3. **The lock of step 1** is the machine's verify lock, `machine:verify` (X36), contended by the tasks' check gates
   (the fixture has a 3-second gate). Environment locks get the same events but are not exercised here.
4. **No `Stopped` state.** A stop leaves the task in its state (`Implementing`, `Planning`) with the stage attempt
   `interrupted`; the end table shows `INT-A  Implementing  (exit 0)`. "Stopped" is `run_ended.outcome = stopped`.
5. **Exit code after one Ctrl+C:** `toryo run A B C` exits **1**, because a queued task was not started (each stopped
   child exits 0). If every task had started and been stopped, the parent would exit 0.
6. **Fake agents are not processes** — they run inside toryo (`--fake-agents`). `agent_spawned.pid` is `null` for
   them; AT-6 checks the toryo children's `run_started.pid` (and any real agent pid).
7. **Resume:** `toryo resume <ID>` (a plain `toryo run <ID>` continues too; that is the `next:` the end block shows).
8. **Ctrl+C path:** `lock::catch_interrupt` (X29) turns the first Ctrl+C into the stop row `… (Ctrl+C)`, which the
   engine answers at the next boundary (`Engine::stopped`, now `cancel_requested`); the parent of `run A B` only stops
   starting queued tasks (`engine_cli::catch_ctrl_c`). A second Ctrl+C ends a process at once.
9. **Fixtures:** `fake-slow.json` (implement 60 s) does not let AT-9 catch the plan stage, so `fake-slow-plan.json`
   (plan 60 s) was added. The workspace is the BatchRename sample (`toryo spike init-fixture --app batch-rename`) with
   RUN-A/B/C and INT-A/B/C only (the manual DASH-*/PRE-* tasks are not needed).
10. **Pester:** 6.2.0 installed (≥ 5 required; Windows ships 3.4).
11. **Dev-dependencies:** only `expectrl` was added (Windows only); `tempfile`, `regex` and `serde_json` were already
    there.

## What is not automated, and why

- **The look on a real terminal** (colours, the end frame, the `│` column) — every automated run writes into a pipe or
  a pseudo-console with `--color never`, where toryo prints plain text by design (X34). Section 5's manual check.
- **A Ctrl+C pressed by a hand** on a real console: the tests press it through ConPTY, which delivers the same console
  event, but no person did (STATUS N-16).
- **Real agents** (Claude Code, Codex): never run in tests (repository rule); `agent_spawned.pid` of a real CLI is
  covered by the code path only.
- **CI:** the script needs Windows, PowerShell 7 and Pester 5; `ci.yml` runs neither the ignored tests nor Pester
  (STATUS N-21).
