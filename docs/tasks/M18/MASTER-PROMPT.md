# Master prompt — M18: the command-line reader upgrade and the trust-boundary fixes

Paste everything below the line into a new **Claude Code on the web** session on the repository
`ngocthien1605/Toryo`.

---

You are the lead engineer for **Toryo** (棟梁), a Rust CLI that orchestrates the Claude Code and Codex CLIs through
a gated, verified workflow. Your **final goal** is to implement, test and deliver milestone **M18** with both parts
working and verified:

1. **The command-line reader upgrade.** The M18 reader (`src/engine/cmdline.rs` and the entry-script code of
   `src/engine/checks.rs`) finds the entry script behind every spelling `docs/tasks/M18/TASK.md` Part A lists:
   - `sh` keywords and builtins (`if`, `while`, `!`, `command`, …);
   - the wrappers `env`, `timeout`, `nice` and `nohup`;
   - `cmd`'s `start` and `if`;
   - `pwsh -CommandWithArgs` and `-EncodedCommand`;
   - interpreters and makefiles inside a command string;
   - `+` options, versioned interpreters, `cmd /r`.

   Where it still stops, it says so. Property tests prove it.
2. **The trust-boundary fixes** of `docs/REVIEW-2026-11-05.md` (T1–T7):
   - T7, the verify record, **first**;
   - T4, the checkpoint;
   - T2, the worktree's `.git` pointer;
   - T1, packed replace refs;
   - T3, nested `.git`.

   Each fix covers the paths M17 added (candidate worktrees, candidate checkpoints, candidate and red verify records).

The complete specification is **`docs/tasks/M18/TASK.md`**. It holds the proposed owner decisions D-1 … D-8, the
design, the 24 acceptance criteria (AC-1 … AC-24) and the 31 edge cases. TASK.md is the contract: follow it exactly.
Where it is silent, follow the rules below.

## 0. Set up (first minutes)

1. Read, in this order:
   - `docs/STATUS.md` (§3 is the house rules);
   - `docs/tasks/M18/TASK.md`;
   - `docs/REVIEW-2026-11-05.md` (each finding's reproduction and proposed fix);
   - `README.md` ("What the boundary does and does not buy");
   - `docs/DECISIONS.md`, at least X7, X9, X10, X13, X15, X30, X44 and X47–X63;
   - `docs/M17-NOTES.md`.

   Then read the module comment of `src/engine/cmdline.rs` (it is the rule book) and the code that TASK.md
   "Background" names.
2. If `docs/tasks/M18/TASK.md` is not in your checkout, it lives on branch `claude/m17-task-implementation-2lun7d`.
   Run `git fetch origin claude/m17-task-implementation-2lun7d` and then
   `git checkout origin/claude/m17-task-implementation-2lun7d -- docs/tasks/M18`. Commit it as your first commit
   (`docs: M18 task and master prompt (M18-Z0)`).
3. M18 builds on M17: candidate worktrees, `verify::Slot` and the candidate and red records. Work on the branch this
   session gives you, based on the latest `main`. If `main` has no `src/engine/runner/fanout.rs`, M17 is not merged
   yet. Raise that in your one question round (base on the M17 branch, or wait).
4. Run `cargo build --locked --tests` and `cargo test --locked` once, and note the baseline test count (STATUS says
   974 passed, 8 ignored on Linux after M17). Also run
   `cargo test --locked --test review_2026_11_05 -- --ignored`: it must show **0 passed, 8 failed**. Each of those
   eight tests is the proof that its finding exists.

## 1. Questions — only now, only once

The owner will be **away from the PC** after you start. Unlike M17, the owner decisions in TASK.md are
**proposed**, not settled. So the one question round has a fixed first job: confirm D-1 … D-8.

- Ask **one** round with the `AskUserQuestion` tool now, before any code. Ask at most 4 questions, each with the
  recommended option first.
- Group the proposed decisions so that the owner can accept them in as few answers as possible (for example, one
  question "accept D-1 … D-8 as written?" plus questions only where you see a real choice).
- If the owner changes one, TASK.md changes with it. Write the change into TASK.md in your first commit, as M17's
  notes recorded its answers.

After this point **never ask again and never stop to wait for confirmation**. For every later doubt:

- pick the **conservative** option: fail closed, keep existing behaviour, protect rather than refuse when unsure (X13),
  refuse only what is certain (X10), add a test;
- record it as an X-decision in `docs/DECISIONS.md` (X64 onward, after the confirmed D-decisions);
- list it in `docs/M18-NOTES.md` under "Decided without the owner";
- carry on.

## 2. Plan the work

Create a task list (TaskCreate) from TASK.md. Suggested order (adjust if the code says otherwise, and write down
why):

- **B1 first**, the verify record (T7: AC-13, AC-14). It is the one finding with an irreversible loss, and the review
  says to fix it first.
- **Part A, the reader:**
  - A8a, the property-test generator (AC-10), **before** the reader changes. It is the net under every later step:
    run it on the unchanged reader and record which properties fail today (they are the findings).
  - A1, `sh` keywords and builtins (AC-1);
  - A2, the wrappers (AC-2);
  - A3, `cmd` `start` and `if` (AC-3);
  - A4, `pwsh -CommandWithArgs` and `-EncodedCommand` (AC-4);
  - A5, interpreters and makefiles inside a command string (AC-5);
  - A6, option and interpreter spellings (AC-6);
  - A7, the notes where the reader stops (AC-7);
  - A8b, all four properties green, at 10 000 cases per shell (AC-10);
  - then AC-8, AC-9 (nothing unprotected, no refusal mid-task), AC-11 (red-green args) and AC-12 (T5 and T6 un-ignored).
- **Part B, the rest of the trust edges:**
  - B2, the checkpoint (T4: AC-15);
  - B3, the Git directories from the main repository and store schema 5 (T2: AC-16, AC-21);
  - B4, replace refs (T1: AC-17);
  - B5, nested `.git` (T3: AC-18);
  - B6, the M17 paths (AC-19), then AC-20 (no `#[ignore]` left).
- **Closing:**
  - Z1, the cross-checks: an upgrade test from a version-4 database in the middle of a fan-out (AC-21), red-green
    forms through the new reader (AC-11);
  - Z2, docs (AC-24);
  - Z3, self-review;
  - Z4, PR and CI (AC-23).

## 3. The loop for every task

1. Write the failing test(s) first. Use integration tests with **fake agents only**, in `tests/m18_reader.rs` and
   `tests/m18_trust.rs` (fixture helpers in `tests/common/`), and unit tests next to the code where that fits — the
   reader's unit tests belong in `cmdline.rs` and `checks.rs`.
   - Name each edge-case test so it can be traced (e.g. `m18_ec07_cmd_if_conditions_pass_the_head`).
   - When a review test in `tests/review_2026_11_05.rs` is the proof of the finding, **remove its `#[ignore]` in the
     commit that fixes it**, and in no other commit.
2. Implement the smallest change that makes them pass. No unrelated refactoring.
3. Run the four checks, all green:
   `cargo fmt --all -- --check`,
   `cargo clippy --locked --all-targets -- -D warnings`,
   `cargo clippy --locked --all-targets --no-default-features -- -D warnings`,
   `cargo test --locked`.
4. Commit once per task, subject `area: what (M18-A3)`. The body says:
   - what changed, which ACs and edge cases it covers, and the test names;
   - for a reader change, **every configuration form that is now refused** that was admitted before (AC-8).
5. **Push after every commit.** The container is ephemeral.
6. Append one line to the "Progress" section of `docs/M18-NOTES.md`: the task, the commit, the tests added and the
   next task. This file is your memory: after a context compaction, re-read it and `git log` before continuing.

## 4. Hard rules (from the house rules and TASK.md)

- **Never run real `claude` or `codex`**, and never touch the owner's configs, data folders or a D365FO environment.
  Agents in tests are `--fake-agents` scripts or the fake adapter.
- Never weaken, skip, `#[ignore]` or delete an existing test. If an old assertion must change, name it and say why in
  the commit body.
  - **Every existing test of `cmdline.rs` and `checks.rs` must pass unchanged** (AC-8). A reader change that needs one
    of them changed is the wrong change.
- **This milestone is the review round for the M18 rules**, so `cmdline.rs` and the entry-script code of `checks.rs`
  may change. Their principles may not:
  - only heads are entries (X10);
  - refusing needs certainty, protecting does not (X13);
  - what is protected is decided from the base commit (X9).

  Nothing protected before may become unprotected.
- **The export comparison in `verify.rs` (X15) stays untouchable.** T7 changes how a verify record is accepted and
  what `cleanup` deletes. It never changes how an export is compared.
- A task without the new readings behaves exactly as before. A task created before M18 is never refused mid-task: only
  `task create` and `task config --reload` refuse (D-3).
- Untrusted text (agent output, diffs, gate output) reaches agents only as data, and the console only as inert text
  (`errln!` / `outln!`, X33). An `-EncodedCommand` value is decoded for **reading** only; it is never printed undecoded
  or decoded into a console line without the inert filter.
- No new crate (D-7): the property tests use a seeded generator written in the test file.
- `#[cfg(windows)]` code is not compiled here. M17 learned this the hard way.
  - `rustup target add x86_64-pc-windows-gnu` works, but `cargo check --target x86_64-pc-windows-gnu` fails because the
    bundled SQLite needs a MinGW C compiler. Do not spend time on it.
  - Search every Windows-only use of a type or signature you change by hand.
  - In a `#[cfg(windows)]` / `#[cfg(not(windows))]` pair, make each branch use (or `let _ =`) every argument: an unused
    one fails Clippy on `windows-latest` only.
  - Let CI judge.
- Windows test scripts (`.cmd`), from M17's CI:
  - spell every path you hand to `cmd`'s built-ins (`move`, `del`, `if exist`) with `\` — a `/` can be read as a
    switch, and `%~f0` keeps the spelling the script was started with;
  - a script that moves or deletes itself ends with `exit 0`, not `exit /b 0` (otherwise cmd says "The batch file
    cannot be found." and exits 1);
  - reuse `tests/common/m17.rs` `gate_program`.
- No model names or identifiers in commits, code, comments or the PR: the trailer is `Co-Authored-By: Claude
  <noreply@anthropic.com>`. Never merge the pull request. Never force-push over someone else's commits.

## 5. Pull request and CI

- After the first pushed commit, open **one draft pull request** to `main`, titled "M18: command-line reader upgrade
  and trust-boundary fixes". Use the repository's PR template if there is one.
- Then call `subscribe_pr_activity` for it, so Windows CI (`windows-latest`) gives feedback during the work. A Windows
  run takes about 20 minutes: push the fixes of independent tasks without waiting for each run, and read every failed
  run's annotations before the next push.
- On a red CI run:
  1. reproduce it locally where possible;
  2. fix it with a test;
  3. push.
- Known machine-speed flakes are listed in `docs/STATUS.md` O-56. For one of those, re-run the failed job
  (`actions_run_trigger` / re-run) instead of changing the test. Never push an empty commit to kick CI.
- When everything is done, mark the PR ready for review. Its body has:
  - a summary per part;
  - the AC → test table (or a link to `docs/M18-NOTES.md`);
  - the test count before and after;
  - each form the reader now refuses that it admitted before;
  - the decisions taken without the owner;
  - the open items.

## 6. Before you call it done (self-verification)

1. Run `cargo test --locked` **twice** in a row; both runs must be green.
2. `cargo test --locked --test review_2026_11_05` runs all eight review tests with **no `#[ignore]` left**, and they pass.
   `grep -c '#\[ignore' tests/review_2026_11_05.rs` prints 0.
3. The property tests run at least 10 000 cases per shell in the normal `cargo test` and pass. A deliberately broken
   reader (comment out one prefix of A1 locally, never committed) makes them fail with a seed. Note that you did this
   check in the notes.
4. `docs/M18-NOTES.md` has an **AC → test traceability table** for AC-1 … AC-24 and an **edge case → test** table for
   edge cases 1 … 31. Each row names real test functions, with no gaps. Check it with a script or `grep -c` over the
   test files.
5. Run the `/code-review` skill at level `high` on the full diff against `main`, and `/security-review`. This
   milestone **is** security work, so give the security review the review's findings as context. Fix every confirmed
   finding (with a test), re-run the checks, push.
6. Documentation per AC-24:
   - README;
   - `docs/HUONG-DAN.md` **in Vietnamese**;
   - DECISIONS (X64+);
   - STATUS (edit the open list in place, never strike through: close T1–T7, the done parts of O-41, O-42, O-34 and
     N-10);
   - the status table of `docs/REVIEW-2026-11-05.md`;
   - M18-NOTES.
7. CI green on `windows-latest` **and** `ubuntu-latest` for the final commit.

## 7. When to stop

Keep working, across as many turns and context compactions as it takes, until **all of §6 holds**. Stop early only
when it is truly impossible to go on: push denied, GitHub unreachable for a long time, or CI infrastructure down.
In that case, write the exact state, the blocker and the next step into `docs/M18-NOTES.md` and the PR body, push,
and end with the report below.

## 8. Final report (your last message)

- PR link and CI status per platform.
- What was built, per part, in a few lines each.
- Test count: baseline → final, and the names of the new test files; the eight review tests and their state.
- Each AC-1 … AC-24: done, with its main test.
- Every gate form that is refused now and was admitted before.
- Decisions taken without the owner (X-ids), and the owner's answers to D-1 … D-8.
- Open items and what the owner should check by hand on Windows. For example:
  - the vendors' handling of a `.git` **file** (TASK.md out of scope);
  - a real `cmd /c start /wait` gate;
  - a `pwsh -EncodedCommand` gate.
