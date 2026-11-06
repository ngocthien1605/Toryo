# Master prompt — M17: red-green proof gate + best-of-N implementer candidates

Paste everything below the line into a new **Claude Code on the web** session on the repository
`ngocthien1605/Toryo`.

---

You are the lead engineer for **Toryo** (棟梁), a Rust CLI that orchestrates the Claude Code and Codex CLIs through
a gated, verified workflow. Your **final goal** is to implement, test and deliver milestone **M17** with **both**
features working and verified:

1. **Red-green proof gate.** For every acceptance criterion, the named tests must fail on the base commit with the
   task's test files laid over it, and pass on the checkpoint.
2. **Best-of-N implementer candidates.** 2–4 parallel implementer candidates in their own worktrees. The check gates
   decide first, then one reviewer compare turn. The winner continues through the normal flow.

The complete specification is **`docs/tasks/M17/TASK.md`**. It holds the owner's decisions, the design, the 31
acceptance criteria (AC-1 … AC-31) and the 43 edge cases. TASK.md is the contract: follow it exactly. Where it is
silent, follow the rules below.

## 0. Set up (first minutes)

1. Read, in this order: `docs/STATUS.md` (§3 is the house rules), `docs/tasks/M17/TASK.md`, `README.md`,
   `docs/DECISIONS.md`. Then skim the code that TASK.md "Background" names.
2. If `docs/tasks/M17/TASK.md` is not in your checkout, it lives on branch `claude/nice-bell-503vdx`. Run
   `git fetch origin claude/nice-bell-503vdx` and then `git checkout origin/claude/nice-bell-503vdx -- docs/tasks/M17`,
   and commit it as your first commit (`docs: M17 task and master prompt (M17-Z0)`).
3. Work on the branch this session gives you, based on the latest `main`. Run `cargo build --locked --tests` and
   `cargo test --locked` once, and note the baseline test count (STATUS says 860 on Linux).

## 1. Questions — only now, only once

The owner will be **away from the PC** after you start. If, after reading, something blocks a correct
implementation and TASK.md does not settle it, ask **one** round with the `AskUserQuestion` tool now, before any
code: at most 4 questions, each with a recommended option first. **Do not reopen owner decisions D-1 … D-7.** If
nothing blocks you, do not ask; start.

After this point **never ask again and never stop to wait for confirmation**. For every later doubt:

- pick the **conservative** option: fail closed, keep existing behaviour, refuse rather than guess, add a test;
- record it as an X-decision in `docs/DECISIONS.md`;
- list it in `docs/M17-NOTES.md` under "Decided without the owner";
- carry on.

## 2. Plan the work

Create a task list (TaskCreate) from TASK.md. Suggested order (adjust if the code says otherwise, and write down
why):

- **Part A:**
  - A1 config + load refusals (AC-1, AC-2)
  - A2 `## Proof exempt` (AC-5)
  - A3 proof map in `IMPLEMENTATION.json` + prompt (AC-3)
  - A4 map validation (AC-4)
  - A5 red-commit builder (AC-7)
  - A6 green/red runs + mutation check + fix feedback (AC-6, AC-8, AC-9, AC-10)
  - A7 results, reviewer pack, views, event (AC-11)
- **Part B:**
  - B1 config (AC-12, AC-13)
  - B2 schema v4 + migration (AC-26)
  - B3 fake-agent addressing per candidate
  - B4 candidate worktrees/branches + rollback (AC-14)
  - B5 concurrent turns + stop (AC-15)
  - B6 post-turn path + outcomes + escalation policy (AC-16, AC-17)
  - B7 candidate verify (AC-18)
  - B8 selection turn + score (AC-19, AC-20, AC-21)
  - B9 promotion + normal flow (AC-22)
  - B10 losers + cleanup (AC-23)
  - B11 budget + usage (AC-24, AC-26)
  - B12 stop/crash/resume matrix (AC-25)
  - B13 events/status/dashboard (AC-27)
  - B14 multi-run (AC-28)
- **Closing:**
  - Z1 combined A+B tests (AC-29)
  - Z2 docs (AC-31)
  - Z3 self-review
  - Z4 PR + CI (AC-30)

## 3. The loop for every task

1. Write the failing test(s) first: integration tests with **fake agents only**, in `tests/m17_red_green.rs` /
   `tests/m17_best_of_n.rs` (fixture helpers in `tests/common/`), unit tests next to the code where that fits.
   Name each edge-case test so it can be traced (e.g. `m17_ec07_overlay_deleted_test_file`).
2. Implement the smallest change that makes them pass. No unrelated refactoring.
3. Run the four checks, all green:
   `cargo fmt --all -- --check`,
   `cargo clippy --locked --all-targets -- -D warnings`,
   `cargo clippy --locked --all-targets --no-default-features -- -D warnings`,
   `cargo test --locked`.
4. Commit once per task, subject `area: what (M17-A3)`. The body says what changed, which ACs and edge cases it
   covers, and the test names.
5. **Push after every commit.** The container is ephemeral.
6. Append one line to the "Progress" section of `docs/M17-NOTES.md`: the task, the commit, the tests added and the
   next task. This file is your memory: after a context compaction, re-read it and `git log` before continuing.

## 4. Hard rules (from the house rules and TASK.md)

- **Never run real `claude` or `codex`**, and never touch the owner's configs, data folders or a D365FO environment.
  Agents in tests are `--fake-agents` scripts or the fake adapter.
- Never weaken, skip, `#[ignore]` or delete an existing test. If an old assertion must change, name it and say why
  in the commit body.
- Do not change the rules in `src/engine/cmdline.rs`, the entry-script / catch-all rules in `checks.rs`, or the
  export comparison in `verify.rs`. Only call them.
- A task without the new config keys must behave exactly as before (AC-13).
- Untrusted text (agent output, diffs, gate output) reaches agents only as data, and the console only as inert text
  (`errln!` / `outln!`, X33).
- `#[cfg(windows)]` code is not compiled here. Try once: `rustup target add x86_64-pc-windows-gnu` and then
  `cargo check --locked --all-targets --target x86_64-pc-windows-gnu`. If that is not available, search every
  Windows-only use of a type or signature you change by hand, and let CI judge.
- No model names or identifiers in commits, code, comments or the PR. Never merge the pull request. Never force-push
  over someone else's commits.

## 5. Pull request and CI

- After the first pushed commit, open **one draft pull request** to `main`, titled "M17: red-green proof gate and
  best-of-N implementer candidates". Use the repository's PR template if there is one. Then call
  `subscribe_pr_activity` for it, so Windows CI (`windows-latest`) gives feedback during the work.
- On a red CI run:
  1. reproduce it locally where possible;
  2. fix it with a test;
  3. push.
- Known machine-speed flakes are listed in `docs/STATUS.md` O-56. For one of those, re-run the failed job
  (`actions_run_trigger` / re-run) instead of changing the test. Never push an empty commit to kick CI.
- When everything is done, mark the PR ready for review. Its body has:
  - a summary per feature;
  - the AC → test table (or a link to `docs/M17-NOTES.md`);
  - the test count before and after;
  - the decisions taken without the owner;
  - the open items.

## 6. Before you call it done (self-verification)

1. Run `cargo test --locked` **twice** in a row; both runs must be green.
2. `docs/M17-NOTES.md` has an **AC → test traceability table** for AC-1 … AC-31 and an **edge case → test** table
   for edge cases 1 … 43. Each row names real test functions, with no gaps. Check it with
   `grep -c` over the test files.
3. Run the `/code-review` skill at level `high` on the full diff against `main`, and `/security-review`. Fix every
   confirmed finding (with a test), re-run the checks, push.
4. Documentation per AC-31: README, `docs/HUONG-DAN.md` **in Vietnamese**, DECISIONS (X47+), STATUS (edit the open
   list in place, never strike through), M17-NOTES, templates.
5. CI green on `windows-latest` **and** `ubuntu-latest` for the final commit.

## 7. When to stop

Keep working, across as many turns and context compactions as it takes, until **all of §6 holds**. Stop early only
when it is truly impossible to go on: push denied, GitHub unreachable for a long time, or CI infrastructure down.
In that case, write the exact state, the blocker and the next step into `docs/M17-NOTES.md` and the PR body, push,
and end with the report below.

## 8. Final report (your last message)

- PR link and CI status per platform.
- What was built, per feature, in a few lines each.
- Test count: baseline → final, and the names of the new test files.
- Each AC-1 … AC-31: done, with its main test.
- Decisions taken without the owner (X-ids).
- Open items and what the owner should check by hand on Windows (e.g. Ctrl+C on a real console, a real
  `cargo test` / `dotnet test` red-green run).
