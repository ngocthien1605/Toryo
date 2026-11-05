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
- X63 — (the review) a candidate folder with uncommitted work is kept and named, a folder that cannot be removed only
  warns; a candidate turn that ended on its own after a sibling hit the shared budget is kept.
- X62 — candidate check gates only (never an environment gate), serial under the verify lock, in
  `<verify_root>/<task>/c<k>` (kept until `task cleanup`), each with `verify-result#c<k>`; the winner is verified again;
  `task config --reload` may change `candidates` only before the fan-out.

## Progress

| Task | Commit | Tests added | Next |
|---|---|---|---|
| Z0 task docs | `414758c` | — | A1 |
| A1 `red_green` config, refusals, display, M18 | `f7513bb` | `m17_ac01_an_org_chart_gate_takes_red_green_and_shows_it`, `m17_ec18_a_toryo_yaml_verify_command_takes_red_green`, `m17_ac02_load_refuses_a_bad_red_green_block_naming_gate_and_key`, `m17_ac02_red_green_args_get_the_entry_script_protection` — 886 passed | A2 |
| A2 `## Proof exempt`; the pure red-green module (`engine/redgreen.rs`: test ids, proof map, red commit, result) | `e33bc46` | `m17_ac05_task_create_refuses_a_bad_proof_exempt_section`; unit `m17_ac05_proof_exempt_is_parsed_and_refused_with_line_numbers`, `m17_ec03_test_ids_are_inert_or_refused`, `m17_ec04_every_problem_of_a_proof_map_is_named_at_once`, `m17_ec05_one_test_for_two_criteria_and_exempt_criteria` — 891 passed | A3 |
| A3 `proof` in the schema (strict), read as `[]` without a red-green gate; the proof rule in the implementer's prompt; one red-green gate (X48) | `a2581b2` | `m17_ac03_a_report_without_proof_passes_without_a_red_green_gate`, `m17_ac03_the_prompt_explains_the_proof_rule_with_a_red_green_gate`, `m17_x48_two_red_green_gates_are_refused`; unit `m17_ac03_a_report_without_proof_passes_only_without_a_red_green_gate` — 895 passed | A4–A7 |
| A4–A7 the red-green run: map check before any process, green per test, red commit + checkout, red per test, mutation check, feedback, `VERIFY_RESULT.json` `red_green`, reviewer evidence, `status`/`show`/final gate, `red_green_finished` | `62ffc9b` | `m17_ac06_ac07_ac11_a_red_green_gate_proves_every_criterion`, `m17_ec01_ec14_…`, `m17_ac04_ec03_…`, `m17_ec02_…`, `m17_ec07_…` (2), `m17_ec08_…`, `m17_ec09_…`, `m17_ec10_…`, `m17_ec11_…`, `m17_ec12_…`, `m17_ec13_…`, `m17_ec16_…`, `m17_ec17_…`, `m17_ec18_…_end_to_end`, `m17_ec19_…` — 911 passed | B1 |
| B1–B3 `candidates` on the implementer seat (both modes, refusals, frozen, `config show`/`task config`/`doctor`, R51, reload before the fan-out); store schema 4 (`fanouts`, `candidates`); the fake agent's `role@profile#c<k>` / `role#c<k>` scripts and `wait_for` step | `b735bff` | `m17_ac12_candidates_on_the_implementer_seat_are_frozen_and_shown`, `m17_ac12_candidates_are_refused_off_the_implementer_with_a_bad_count_or_an_unknown_profile`, `m17_ac12_candidates_in_toryo_yaml_mode`, `m17_ac13_a_configuration_without_candidates_keeps_its_shape`, `m17_ac12_the_r51_evidence_check_covers_every_candidate_profile`, `m17_ec35_reload_changes_candidates_before_the_fan_out`, `m17_ec40_a_version_3_database_with_a_task_is_raised_and_the_task_finishes`, `m17_b3_the_fake_agent_addresses_a_candidate_turn_by_its_label`; unit `m17_ec40_a_version_3_database_is_raised_to_4` — 920 passed | B4–B10 |
| B4–B10 the fan-out engine (`runner/fanout.rs`, one state machine: creation → turns → verify → select → promotion, each phase re-entrant; one commit because no phase can be tested end to end without the next); `task cleanup` of candidate worktrees; the fan-out views (`status`/`show` text and `candidates` JSON, dashboard `now:`, final gate) and the budget admission are in it, their own tests follow in B11–B14 | `98efed3` | `m17_ec21_…`, `m17_ec22_…` (3), `m17_ec23_…`, `m17_ec24_…`, `m17_ec25_…`, `m17_ac19_…`, `m17_ec26_…` (4), `m17_ec27_…`, `m17_ec28_…`, `m17_ec29_…`, `m17_ac15_…`, `m17_ac18_…`, `m17_ec30_…`, `m17_ec33_…`, `m17_ac14_…`, `m17_ec37_…`, `m17_ec31_…` (4), `m17_ec38_…`, `m17_ec36_…`, `m17_ac23_…`, `m17_ec32_…` (3), `m17_ac25_…`, `m17_ec43_…`; unit `m17_candidate_and_compare_stages_have_their_roles_and_packs` and the `engine/candidates.rs` unit tests — 958 passed | B11–B14 |
| B11–B14 the tests of the budget admission and the shared live interrupt, the usage per candidate, the fan-out events / `watch` / `status` / final gate / JSON shapes / dashboard `now:`, `run A B --jobs 2`, the builder the review judges, a forged candidate commit; fix: a red-green per-test run checks its program first (Windows: a missing `.cmd` only exits 1 through `cmd.exe`, which red counted as failing) | `ac979e4` | `m17_ec34_…` (3), `m17_ec42_…`, `m17_ac27_…` (2), `m17_ec41_…`, `m17_ec39_…`, `m17_ac22_…`, `m17_ac16_…` — 968 passed | Z1–Z4 |
| Z1–Z2 docs: README (red-green, best-of-N, the boundary, stop classes, events and JSON), HUONG-DAN §5.10, §5.11 and the §9.1 row (Vietnamese), STATUS (state, tests, N-24, N-25, O-39 in place), the templates' commented examples, these notes' tables; tests for edge cases 15 and 35 (after the fan-out), the commented examples parse; a Windows path in an assertion | `c736fd3` | `m17_ec15_test_inside_an_element_is_substituted_there`, unit `m17_ec15_the_test_id_is_substituted_inside_an_element_or_as_one`, `m17_ec35_reload_after_the_fan_out_is_refused`, unit `the_commented_m17_examples_parse` — 972 passed | Z3 reviews |
| Z3 `/code-review` (high) of the whole diff: 8 findings — fixed: a loser's uncommitted work was deleted with its folder (now kept, X63), a removal failure could stop the promotion (now a warning), a turn that ended after a sibling hit the budget was run again, `task cleanup` cancelled before it removed the candidate worktrees, the reload refusal's words for a single path, reads in write transactions, an unused parameter and a duplicated field; skipped: one shared worktree-removal helper (three call sites with different rules) | `5148fab` | `m17_x63_a_candidate_folder_with_uncommitted_work_is_kept`, `m17_review_a_turn_that_ended_after_the_budget_was_hit_is_kept` | security review |
| Z4 Windows CI: the test runner scripts (`.cmd`) passed `/`-spelled paths to cmd's `move` and `del` (`%~f0` keeps the spelling the runner was started with), so `m17_ec11_…` never moved the runner away and `m17_ec39_…`'s busy marker was never deleted (a serial second gate saw it); both now spell their paths with `\`, and `m17_ec11_…` says in its first line whether the runner moved | `9eef16a` | — 974 passed | CI |
| Z4 commit messages: the `Co-Authored-By` line of the first six commits named a model, which the master prompt forbids; with the owner's go-ahead the branch's messages were reworded and force-pushed with a lease (the trees are byte-identical; every commit id above is the new one) | `b723a28` | — | CI |
| Z4 Windows CI, `m17_ec11_…` again: after its self-move the `.cmd` runner returned with `exit /b 0`, and cmd then reported "The batch file cannot be found." with exit code 1, so the green run failed (a fix cycle, not `gate_unavailable`); it now ends with `exit 0`, which ends cmd at once. `m17_ec39_…` passed on Windows with the `\\` paths. The class assertion of `m17_ec11_…` now names the class, the fix cycle and the reason first | `8364fba` | — 974 passed | CI |
| Z4 Windows CI on `8364fba`: every test passed on `windows-latest`; Clippy there found `sh` unused in the `#[cfg(windows)]` branch of the test helper `gate_program` (code this container cannot compile) — discarded as the other branch already discards `cmd`. Also `docs/tasks/M18/TASK.md` (`52dc7dd`) | (this commit) | — 974 passed | CI |

## AC → test traceability

Integration tests are in `tests/m17_red_green.rs` (RG) and `tests/m17_best_of_n.rs` (BN); a unit test names its module.

| AC | Main test(s) |
|---|---|
| AC-1 | RG `m17_ac01_an_org_chart_gate_takes_red_green_and_shows_it`, RG `m17_ec18_a_toryo_yaml_verify_command_takes_red_green` |
| AC-2 | RG `m17_ac02_load_refuses_a_bad_red_green_block_naming_gate_and_key`, RG `m17_ac02_red_green_args_get_the_entry_script_protection`, RG `m17_x48_two_red_green_gates_are_refused` |
| AC-3 | RG `m17_ac03_a_report_without_proof_passes_without_a_red_green_gate`, RG `m17_ac03_the_prompt_explains_the_proof_rule_with_a_red_green_gate`, unit `contracts::m17_ac03_a_report_without_proof_passes_only_without_a_red_green_gate` |
| AC-4 | RG `m17_ac04_ec03_an_invalid_proof_map_starts_no_process`, unit `redgreen::m17_ec04_every_problem_of_a_proof_map_is_named_at_once`, unit `redgreen::m17_ec03_test_ids_are_inert_or_refused` |
| AC-5 | RG `m17_ac05_task_create_refuses_a_bad_proof_exempt_section`, unit `redgreen::m17_ac05_proof_exempt_is_parsed_and_refused_with_line_numbers` |
| AC-6 | RG `m17_ac06_ac07_ac11_a_red_green_gate_proves_every_criterion`, RG `m17_ec15_test_inside_an_element_is_substituted_there`, RG `m17_ec02_a_filter_that_matches_no_test_needs_the_output_rule` |
| AC-7 | RG `m17_ac06_ac07_ac11_a_red_green_gate_proves_every_criterion`, RG `m17_ec07_the_overlay_takes_added_modified_deleted_and_renamed_test_files`, RG `m17_ec08_an_empty_overlay_passes`, RG `m17_ec09_crlf_bytes_and_the_executable_bit_are_byte_exact` |
| AC-8 | RG `m17_ec01_ec14_a_test_that_passes_on_red_proves_nothing_and_the_fix_gets_a_new_red_commit`, RG `m17_ec10_a_red_run_that_times_out_counts_as_red_and_is_flagged`, RG `m17_ec11_a_program_missing_when_red_starts_is_gate_unavailable` |
| AC-9 | RG `m17_ec12_test_code_writing_into_the_red_checkout_is_verify_mutated_tree` |
| AC-10 | RG `m17_ec01_ec14_a_test_that_passes_on_red_proves_nothing_and_the_fix_gets_a_new_red_commit`, RG `m17_ec13_a_crash_and_a_stop_during_the_red_runs` |
| AC-11 | RG `m17_ac06_ac07_ac11_a_red_green_gate_proves_every_criterion`, RG `m17_ec17_a_proof_without_a_red_green_gate_is_ignored`, RG `m17_ec10_a_red_run_that_times_out_counts_as_red_and_is_flagged` (reviewer pack) |
| AC-12 | BN `m17_ac12_candidates_on_the_implementer_seat_are_frozen_and_shown`, BN `m17_ac12_candidates_are_refused_off_the_implementer_with_a_bad_count_or_an_unknown_profile`, BN `m17_ac12_candidates_in_toryo_yaml_mode`, BN `m17_ac12_the_r51_evidence_check_covers_every_candidate_profile`, BN `m17_ec35_reload_changes_candidates_before_the_fan_out` |
| AC-13 | BN `m17_ac13_a_configuration_without_candidates_keeps_its_shape`, BN `m17_ec41_json_documents_with_and_without_candidates`, RG `m17_ec17_a_proof_without_a_red_green_gate_is_ignored`, every test that existed before M17 (unchanged; `docs/M17-NOTES.md` Progress names each changed assertion) |
| AC-14 | BN `m17_ac14_a_failure_while_the_worktrees_are_made_takes_them_back`, BN `m17_ec31_ctrl_c_during_worktree_creation_takes_back`, BN `m17_ec37_a_leftover_folder_or_branch_is_refused_by_name_and_kept`, BN `m17_ec21_two_candidates_of_one_profile_compare_and_promote` |
| AC-15 | BN `m17_ac15_candidate_turns_run_at_the_same_time`, BN `m17_ec21_two_candidates_of_one_profile_compare_and_promote`, BN `m17_ec31_ctrl_c_during_the_turns_ends_every_candidate_turn`, BN `m17_ec31_toryo_stop_from_another_process_ends_the_fan_out`, BN `m17_b3_the_fake_agent_addresses_a_candidate_turn_by_its_label` |
| AC-16 | BN `m17_ec22_a_candidate_out_of_quota_loses_without_a_fallback` (and its crash and invalid-output twins), BN `m17_ec23_no_candidate_changes_anything_the_single_path_runs_once`, BN `m17_ac16_a_forged_candidate_commit_is_never_accepted`, BN `m17_ec32_a_crash_after_the_first_checkpoint_before_the_second` |
| AC-17 | BN `m17_ec28_a_protected_write_escalates_and_stops_the_other_turn`, BN `m17_ec29_a_fence_violation_disqualifies_only_that_candidate`, BN `m17_ec30_a_gate_that_mutates_a_candidates_checkout_disqualifies_it`, BN `m17_ec33_a_machine_stop_in_a_candidate_verify_resumes_without_new_turns` |
| AC-18 | BN `m17_ac18_candidates_run_the_check_gates_only_each_in_its_own_checkout`, BN `m17_ec39_two_tasks_with_candidates_run_with_two_jobs` |
| AC-19 | BN `m17_ac19_the_compare_turn_gets_the_passing_candidates_as_data`, BN `m17_ec26_an_unknown_id_then_a_failing_candidate_fall_to_the_score`, BN `m17_ec26_malformed_answers_fall_to_the_score_a_valid_correction_is_used`, BN `m17_ec26_a_failed_compare_turn_falls_to_the_score`, BN `m17_ec26_a_compare_turn_that_writes_is_put_back`, unit `candidates::m17_ec26_the_compare_answer_is_checked` |
| AC-20 | unit `candidates::m17_ac20_each_rung_of_the_score`; BN `m17_ec24_all_candidates_fail_the_gates_the_best_by_score_enters_fixing` (rung 3), BN `m17_ec43_red_green_and_best_of_n_more_proven_wins_on_the_score` (rung 2), BN `m17_ec25_an_exact_tie_goes_to_the_lower_index` (rung 5) |
| AC-21 | BN `m17_ec24_all_candidates_fail_the_gates_the_best_by_score_enters_fixing`, BN `m17_ec23_no_candidate_changes_anything_the_single_path_runs_once` |
| AC-22 | BN `m17_ec21_two_candidates_of_one_profile_compare_and_promote`, BN `m17_ec38_a_dirty_task_worktree_or_a_moved_head_stops_the_promotion`, BN `m17_ec27_the_fix_cycle_after_the_compare_runs_on_the_winners_profile`, BN `m17_ac22_reviewer_differs_from_builder_judges_the_winner`, BN `m17_ac18_candidates_run_the_check_gates_only_each_in_its_own_checkout` (the environment gate runs on the winner) |
| AC-23 | BN `m17_ec21_two_candidates_of_one_profile_compare_and_promote` (folders removed, branches kept), BN `m17_ec36_task_cleanup_in_the_middle_of_a_fan_out`, BN `m17_ac23_task_cleanup_after_a_crash_in_the_middle_of_a_fan_out` |
| AC-24 | BN `m17_ec34_a_budget_for_one_candidate_takes_the_single_path`, BN `m17_ec34_a_budget_for_two_of_three_runs_two_candidates`, BN `m17_ec34_a_live_interrupt_in_the_turns_pauses_and_resume_runs_the_unfinished`, unit `candidates::m17_ac24_the_budget_admits_this_many_candidates` |
| AC-25 | BN `m17_ac25_a_crash_at_every_point_of_the_fan_out_resumes_to_the_same_result`, BN `m17_ec32_a_crash_after_the_first_checkpoint_before_the_second`, BN `m17_ec32_a_crash_after_the_selection_before_the_promotion`, BN `m17_ec32_a_crash_after_the_promotion_before_the_state_change`, BN `m17_ec31_a_second_ctrl_c_ends_the_process_and_resume_recovers` |
| AC-26 | BN `m17_ec40_a_version_3_database_with_a_task_is_raised_and_the_task_finishes`, unit `store::m17_ec40_a_version_3_database_is_raised_to_4`, BN `m17_ec42_usage_lists_each_candidate_and_the_total_is_the_sum` |
| AC-27 | BN `m17_ac27_the_fan_out_events_status_and_the_final_gate`, BN `m17_ec41_json_documents_with_and_without_candidates`, BN `m17_ac27_the_dashboard_now_line_shows_the_fan_out` |
| AC-28 | BN `m17_ec39_two_tasks_with_candidates_run_with_two_jobs` |
| AC-29 | every row of these two tables; red-green and best-of-N together: BN `m17_ec43_red_green_and_best_of_n_more_proven_wins_on_the_score` |
| AC-30 | the four checks before every commit (Progress); CI on `windows-latest` and `ubuntu-latest` (pull request #16) |
| AC-31 | `README.md` (red-green, best-of-N, boundary, stop classes, events), `docs/HUONG-DAN.md` §5.10, §5.11, §9.1, `docs/DECISIONS.md` X47–X62, `docs/STATUS.md`, these notes, the templates; unit `config::the_commented_m17_examples_parse` |

## Edge case → test

| # | Test(s) |
|---|---|
| 1 | RG `m17_ec01_ec14_a_test_that_passes_on_red_proves_nothing_and_the_fix_gets_a_new_red_commit` |
| 2 | RG `m17_ec02_a_filter_that_matches_no_test_needs_the_output_rule` |
| 3 | RG `m17_ac04_ec03_an_invalid_proof_map_starts_no_process`, unit `redgreen::m17_ec03_test_ids_are_inert_or_refused` |
| 4 | unit `redgreen::m17_ec04_every_problem_of_a_proof_map_is_named_at_once`, RG `m17_ac04_ec03_an_invalid_proof_map_starts_no_process` |
| 5 | RG `m17_ac06_ac07_ac11_a_red_green_gate_proves_every_criterion`, unit `redgreen::m17_ec05_one_test_for_two_criteria_and_exempt_criteria` |
| 6 | RG `m17_ac05_task_create_refuses_a_bad_proof_exempt_section`, unit `redgreen::m17_ec05_one_test_for_two_criteria_and_exempt_criteria`, BN `m17_ec43_red_green_and_best_of_n_more_proven_wins_on_the_score` (an exempt AC with and without a mapping) |
| 7 | RG `m17_ec07_the_overlay_takes_added_modified_deleted_and_renamed_test_files`, RG `m17_ec07_globs_that_are_too_wide_prove_nothing` |
| 8 | RG `m17_ec08_an_empty_overlay_passes` |
| 9 | RG `m17_ec09_crlf_bytes_and_the_executable_bit_are_byte_exact` |
| 10 | RG `m17_ec10_a_red_run_that_times_out_counts_as_red_and_is_flagged` |
| 11 | RG `m17_ec11_a_program_missing_when_red_starts_is_gate_unavailable` |
| 12 | RG `m17_ec12_test_code_writing_into_the_red_checkout_is_verify_mutated_tree` |
| 13 | RG `m17_ec13_a_crash_and_a_stop_during_the_red_runs` |
| 14 | RG `m17_ec01_ec14_a_test_that_passes_on_red_proves_nothing_and_the_fix_gets_a_new_red_commit` |
| 15 | RG `m17_ec15_test_inside_an_element_is_substituted_there`, unit `config::types::m17_red_green_tests::m17_ec15_the_test_id_is_substituted_inside_an_element_or_as_one` |
| 16 | RG `m17_ec16_a_red_green_gate_keeps_its_place_before_an_environment_gate` |
| 17 | RG `m17_ec17_a_proof_without_a_red_green_gate_is_ignored` |
| 18 | RG `m17_ec18_a_toryo_yaml_verify_command_takes_red_green`, RG `m17_ec18_a_toryo_yaml_red_green_command_runs_end_to_end` |
| 19 | RG `m17_ec19_a_huge_per_test_output_is_bounded_and_redacted` |
| 20 | RG `m17_ac06_ac07_ac11_a_red_green_gate_proves_every_criterion` (no new ref; `task cleanup` leaves no red checkout) |
| 21 | BN `m17_ec21_two_candidates_of_one_profile_compare_and_promote` |
| 22 | BN `m17_ec22_a_candidate_out_of_quota_loses_without_a_fallback`, BN `m17_ec22_a_crashed_candidate_loses_without_a_fallback`, BN `m17_ec22_a_candidate_with_invalid_output_loses_without_a_fallback` |
| 23 | BN `m17_ec23_no_candidate_changes_anything_the_single_path_runs_once` |
| 24 | BN `m17_ec24_all_candidates_fail_the_gates_the_best_by_score_enters_fixing` |
| 25 | BN `m17_ec25_an_exact_tie_goes_to_the_lower_index` |
| 26 | BN `m17_ec26_an_unknown_id_then_a_failing_candidate_fall_to_the_score`, BN `m17_ec26_malformed_answers_fall_to_the_score_a_valid_correction_is_used`, BN `m17_ec26_a_failed_compare_turn_falls_to_the_score`, BN `m17_ec26_a_compare_turn_that_writes_is_put_back` |
| 27 | BN `m17_ec27_the_fix_cycle_after_the_compare_runs_on_the_winners_profile` |
| 28 | BN `m17_ec28_a_protected_write_escalates_and_stops_the_other_turn` |
| 29 | BN `m17_ec29_a_fence_violation_disqualifies_only_that_candidate` |
| 30 | BN `m17_ec30_a_gate_that_mutates_a_candidates_checkout_disqualifies_it` |
| 31 | BN `m17_ec31_ctrl_c_during_worktree_creation_takes_back`, BN `m17_ec31_ctrl_c_during_the_turns_ends_every_candidate_turn`, BN `m17_ec31_a_second_ctrl_c_ends_the_process_and_resume_recovers`, BN `m17_ec31_toryo_stop_from_another_process_ends_the_fan_out` |
| 32 | BN `m17_ec32_a_crash_after_the_first_checkpoint_before_the_second`, BN `m17_ec32_a_crash_after_the_selection_before_the_promotion`, BN `m17_ec32_a_crash_after_the_promotion_before_the_state_change` |
| 33 | BN `m17_ec33_a_machine_stop_in_a_candidate_verify_resumes_without_new_turns` |
| 34 | BN `m17_ec34_a_budget_for_one_candidate_takes_the_single_path`, BN `m17_ec34_a_budget_for_two_of_three_runs_two_candidates`, BN `m17_ec34_a_live_interrupt_in_the_turns_pauses_and_resume_runs_the_unfinished` |
| 35 | BN `m17_ec35_reload_changes_candidates_before_the_fan_out`, BN `m17_ec35_reload_after_the_fan_out_is_refused` |
| 36 | BN `m17_ec36_task_cleanup_in_the_middle_of_a_fan_out` |
| 37 | BN `m17_ec37_a_leftover_folder_or_branch_is_refused_by_name_and_kept` |
| 38 | BN `m17_ec38_a_dirty_task_worktree_or_a_moved_head_stops_the_promotion` |
| 39 | BN `m17_ec39_two_tasks_with_candidates_run_with_two_jobs` |
| 40 | BN `m17_ec40_a_version_3_database_with_a_task_is_raised_and_the_task_finishes`, unit `store::m17_ec40_a_version_3_database_is_raised_to_4` |
| 41 | BN `m17_ec41_json_documents_with_and_without_candidates` |
| 42 | BN `m17_ec42_usage_lists_each_candidate_and_the_total_is_the_sum` |
| 43 | BN `m17_ec43_red_green_and_best_of_n_more_proven_wins_on_the_score` |

## Open items

- **Real runs** (STATUS N-24): M17 has run against fake agents and script test runners only — one red-green task with a
  real runner (`cargo test`, `dotnet test`, Pester) and one best-of-N task with the real CLIs on Windows are still to do,
  with a Ctrl+C pressed by hand during the candidates' turns.
- **By design** (STATUS N-25): a candidate that writes into a sibling candidate's worktree is not detected (the agents'
  sandboxes are the guard); no red-green inside an environment gate or with `tool: xpp`; the winner's check gates run
  again in the task's Verifying; no fan-out of fix cycles; at most 4 candidates.
- **Windows cross-check**: `rustup target add x86_64-pc-windows-gnu` worked, but `cargo check --target
  x86_64-pc-windows-gnu` needs a MinGW C compiler for the bundled SQLite, which this container does not have; the
  `#[cfg(windows)]` code was left to CI (`windows-latest`).
