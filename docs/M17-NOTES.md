# M17 — red-green proof gate and best-of-N implementer candidates (notes)

The specification is [`docs/tasks/M17/TASK.md`](tasks/M17/TASK.md) (owner decisions D-1 … D-7, AC-1 …
AC-31, edge cases 1 … 43); how the session works is [`docs/tasks/M17/MASTER-PROMPT.md`](tasks/M17/MASTER-PROMPT.md).
Branch `claude/m17-task-implementation-2lun7d`, from `main` @ `2c0d561`. Tests: `tests/m17_red_green.rs`,
`tests/m17_best_of_n.rs`, fixtures in `tests/common/m17.rs`, unit tests next to the code.

Baseline before M17 (Linux, this container, `cargo test --locked` on `main` @ `2c0d561`): **882 passed, 0 failed,
8 ignored** (`docs/STATUS.md` said 860 — tests were added since).

## Asked once, at the start (2026-11-05)

The four questions of the single round and the owner's answers (each the recommended option):

| # | Question | Answer |
|---|---|---|
| Q1 | Fix cycles after the fan-out: resume the winner's agent session? (Claude looks a session up by its folder; the winner ran in its candidate worktree.) | **Fresh session**, as today's fix cycles (X55). |
| Q2 | The winner's own candidate folder after promotion | **Removed after promotion**; its branch is kept (X56). |
| Q3 | "Admission reserves N candidate turns" | **Estimate only**: count the turns that fit at the reservation estimate; nothing extra in `reserved_tokens`; one shared live-interrupt counter (X57). |
| Q4 | An unrecorded leftover candidate folder or branch | **Machine stop**: a new machine-caused class `candidate_unavailable`; a plain `resume` continues after it is removed (X58). |

## Decided without the owner

(Each is an X-decision in `docs/DECISIONS.md`; the conservative option where the specification is silent.)

- X47 — `red_green.args` gets the M18 protection by passing the red-green run as one more verify command
  (`checks::with_red_green`, `{test}` an empty word), so the rules in `cmdline.rs` and `checks.rs` are only called.

## Progress

| Task | Commit | Tests added | Next |
|---|---|---|---|
| Z0 task docs | `42a09d6` | — | A1 |
| A1 `red_green` config, refusals, display, M18 | (this commit) | `m17_ac01_an_org_chart_gate_takes_red_green_and_shows_it`, `m17_ec18_a_toryo_yaml_verify_command_takes_red_green`, `m17_ac02_load_refuses_a_bad_red_green_block_naming_gate_and_key`, `m17_ac02_red_green_args_get_the_entry_script_protection` — 886 passed | A2 |
