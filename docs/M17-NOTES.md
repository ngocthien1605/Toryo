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
- X48 — one red-green gate per configuration (two are refused at load): a proof map names the tests of one test
  runner.
- X49 — all green runs before any red run, each side reported whole (not only its first failure); the per-test
  timeout is the gate's `timeout`, per run.
- X50 — the private index of the red commit is `<verify folder>/red.index`; the red checkout is
  `<verify folder>/red/<task>.red` with its own `build`/`cache`/`tmp`/`home`; the message names the checkpoint.
- X51 — the proof map is the report of the attempt that made the checkpoint (a fan-out winner: its candidate's).
- X52 — a mutated red checkout is `verify_mutated_tree` and wins over a red failure.
- X53 — `verify_command.side`; a red run that fails as required is ✔ on the console.
- X54 — the reviewer's red-green evidence is wrapped as untrusted data; `red_green` is `null` in `--json` without a
  gate.
- X59 — best-of-N names (`implement#c<k>`, `verify#c<k>`, `select`, `c<k>`), the score's five rungs, the compare
  answer's checks, one correction, and the compare turn's own reason never shown by the views (X35).
- X60 — a promotion refused by the task worktree (dirty, off the branch, not at the base, an unfinished operation) is
  the machine stop `candidate_unavailable`: a plain `resume` retries it (a `--note` would start a fix cycle on the
  base and lose the winner).
- X61 — the candidates' turns are threads of the owner process under the one task lock; each turn's end goes through
  the single post-turn path in its own worktree, one at a time; a stop or a sibling's escalation cancels them all.
- X62 — candidate check gates only (never an environment gate), serial under the verify lock, in
  `<verify_root>/<task>/c<k>` (kept until `task cleanup`), each with `verify-result#c<k>`; the winner is verified again;
  `task config --reload` may change `candidates` only before the fan-out.

## Progress

| Task | Commit | Tests added | Next |
|---|---|---|---|
| Z0 task docs | `42a09d6` | — | A1 |
| A1 `red_green` config, refusals, display, M18 | `2f092f7` | `m17_ac01_an_org_chart_gate_takes_red_green_and_shows_it`, `m17_ec18_a_toryo_yaml_verify_command_takes_red_green`, `m17_ac02_load_refuses_a_bad_red_green_block_naming_gate_and_key`, `m17_ac02_red_green_args_get_the_entry_script_protection` — 886 passed | A2 |
| A2 `## Proof exempt`; the pure red-green module (`engine/redgreen.rs`: test ids, proof map, red commit, result) | `678a8a3` | `m17_ac05_task_create_refuses_a_bad_proof_exempt_section`; unit `m17_ac05_proof_exempt_is_parsed_and_refused_with_line_numbers`, `m17_ec03_test_ids_are_inert_or_refused`, `m17_ec04_every_problem_of_a_proof_map_is_named_at_once`, `m17_ec05_one_test_for_two_criteria_and_exempt_criteria` — 891 passed | A3 |
| A3 `proof` in the schema (strict), read as `[]` without a red-green gate; the proof rule in the implementer's prompt; one red-green gate (X48) | `ca13715` | `m17_ac03_a_report_without_proof_passes_without_a_red_green_gate`, `m17_ac03_the_prompt_explains_the_proof_rule_with_a_red_green_gate`, `m17_x48_two_red_green_gates_are_refused`; unit `m17_ac03_a_report_without_proof_passes_only_without_a_red_green_gate` — 895 passed | A4–A7 |
| A4–A7 the red-green run: map check before any process, green per test, red commit + checkout, red per test, mutation check, feedback, `VERIFY_RESULT.json` `red_green`, reviewer evidence, `status`/`show`/final gate, `red_green_finished` | `497c29a` | `m17_ac06_ac07_ac11_a_red_green_gate_proves_every_criterion`, `m17_ec01_ec14_…`, `m17_ac04_ec03_…`, `m17_ec02_…`, `m17_ec07_…` (2), `m17_ec08_…`, `m17_ec09_…`, `m17_ec10_…`, `m17_ec11_…`, `m17_ec12_…`, `m17_ec13_…`, `m17_ec16_…`, `m17_ec17_…`, `m17_ec18_…_end_to_end`, `m17_ec19_…` — 911 passed | B1 |
| B1–B3 `candidates` on the implementer seat (both modes, refusals, frozen, `config show`/`task config`/`doctor`, R51, reload before the fan-out); store schema 4 (`fanouts`, `candidates`); the fake agent's `role@profile#c<k>` / `role#c<k>` scripts and `wait_for` step | `2170dcb` | `m17_ac12_candidates_on_the_implementer_seat_are_frozen_and_shown`, `m17_ac12_candidates_are_refused_off_the_implementer_with_a_bad_count_or_an_unknown_profile`, `m17_ac12_candidates_in_toryo_yaml_mode`, `m17_ac13_a_configuration_without_candidates_keeps_its_shape`, `m17_ac12_the_r51_evidence_check_covers_every_candidate_profile`, `m17_ec35_reload_changes_candidates_before_the_fan_out`, `m17_ec40_a_version_3_database_with_a_task_is_raised_and_the_task_finishes`, `m17_b3_the_fake_agent_addresses_a_candidate_turn_by_its_label`; unit `m17_ec40_a_version_3_database_is_raised_to_4` — 920 passed | B4–B10 |
| B4–B10 the fan-out engine (`runner/fanout.rs`, one state machine: creation → turns → verify → select → promotion, each phase re-entrant; one commit because no phase can be tested end to end without the next); `task cleanup` of candidate worktrees; the fan-out views (`status`/`show` text and `candidates` JSON, dashboard `now:`, final gate) and the budget admission are in it, their own tests follow in B11–B14 | `bd6599c` | `m17_ec21_…`, `m17_ec22_…` (3), `m17_ec23_…`, `m17_ec24_…`, `m17_ec25_…`, `m17_ac19_…`, `m17_ec26_…` (4), `m17_ec27_…`, `m17_ec28_…`, `m17_ec29_…`, `m17_ac15_…`, `m17_ac18_…`, `m17_ec30_…`, `m17_ec33_…`, `m17_ac14_…`, `m17_ec37_…`, `m17_ec31_…` (4), `m17_ec38_…`, `m17_ec36_…`, `m17_ac23_…`, `m17_ec32_…` (3), `m17_ac25_…`, `m17_ec43_…`; unit `m17_candidate_and_compare_stages_have_their_roles_and_packs` and the `engine/candidates.rs` unit tests — 958 passed | B11–B14 |
| B11–B14 the tests of the budget admission and the shared live interrupt, the usage per candidate, the fan-out events / `watch` / `status` / final gate / JSON shapes / dashboard `now:`, `run A B --jobs 2`, the builder the review judges, a forged candidate commit; fix: a red-green per-test run checks its program first (Windows: a missing `.cmd` only exits 1 through `cmd.exe`, which red counted as failing) | (this commit) | `m17_ec34_…` (3), `m17_ec42_…`, `m17_ac27_…` (2), `m17_ec41_…`, `m17_ec39_…`, `m17_ac22_…`, `m17_ac16_…` — 968 passed | Z1–Z4 |
