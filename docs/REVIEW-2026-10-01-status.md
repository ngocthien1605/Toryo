# REVIEW-2026-10-01 — status of every item at the end of M12

The review is [`REVIEW-2026-10-01.md`](REVIEW-2026-10-01.md). This is the status column for its 62 items: the audit of
`main` @ `4ad97c3` (2026-10-05; the evidence below is from that audit, with file and line at that commit), **updated for
what M12 fixed** (branch `m12/first-run`): H1, H4, H5, H6, M1, M8, M11, M12, M13 and L1 are FIXED, each with the task and
commit that did it. Everything else is as audited; the open items are listed once, with a priority, in
[`STATUS.md`](STATUS.md). A row keeps its old evidence after "Was:" so that the defect stays findable.

Line numbers in rows that say "Was:" are from `4ad97c3` and are no longer true at HEAD.

### 1.1 Counts

| Group | Items | FIXED | PARTIAL | OPEN | OBSOLETE |
|---|---:|---:|---:|---:|---:|
| Critical | 1 | 1 | 0 | 0 | 0 |
| High (H1–H6) | 6 | 5 | 0 | 1 | 0 |
| Medium (M1–M23) | 23 | 7 | 1 | 15 | 0 |
| Low (L1–L23) | 23 | 1 | 0 | 22 | 0 |
| Tests and CI (T1–T5) | 5 | 0 | 1 | 4 | 0 |
| Maintainability (4 bullets) | 4 | 0 | 0 | 4 | 0 |
| **Total** | **62** | **14** | **2** | **46** | **0** |

Why so few: M11 took exactly four review items into scope (C1, H2, M2, M18 — `M11_task.md` "Scope" and "Out of scope"), and the five review rounds that followed reviewed M11's own code, not the old findings. One more item (M14) was mostly closed by the way in round 2. The files behind most of the other findings are byte-identical to `aa0be03`: `git diff aa0be03 4ad97c3` is empty for `src/engine/protect.rs`, `src/engine/worktree.rs`, `src/engine/snapshot.rs`, `src/engine/untrusted.rs`, `src/process/redact.rs`, all of `src/agents/`, `src/spike/` (except `pipeline.rs`), `Cargo.toml`, `.github/`, `.gitignore`; `src/engine/recovery.rs` changed by 8 lines (a message).

### 1.2 Critical and High

| # | Status | Evidence at HEAD |
|---|---|---|
| **C1** — a YAML tag hides `profiles:` from R60 | **FIXED** | Commit `cd78b22` (M11-A1). `src/config.rs:331-332` — the repository file is parsed into `RepoFile` (`deny_unknown_fields`, no `profiles` / `toolchains` field); `:1201` `untagged`, `:1212` `refuse_tags`, `:1248` `machine_only_part` looks through tags. Tests: `config.rs:1561` `a_tagged_profiles_key_is_refused` (the review's reproduction), `:1606` `a_tagged_document_is_refused`, `:1617` `the_repository_file_has_no_profile_fields`. **Re-run:** the review's file (`!x profiles: { evil: … }` + `roles.implementer.profile: evil`) is refused by `toryo doctor` (`[FAIL] config … profiles: may not be defined in the repository config`) and by `config show`; `policy: !y {…}` is refused with "YAML tags are not allowed in a repository file". |
| **H1** — recovery accepts an agent-forged checkpoint commit | **FIXED (M12)** | Fixed by M12-01, `54a7d3c`; review fix `857b291`. Was: `src/engine/recovery.rs:296` still runs the trailer search before the soft reset at `:341-342`. `src/engine/worktree.rs:163-186` (`find_checkpoint`) still matches any commit in `base..HEAD` on the three trailers alone — no `sha == HEAD`, no parent check, no write-ahead tree id. `src/engine/runner.rs:1328-1338`: a found commit still skips the deny-path re-check of `:1340-1351`. No test forges a trailer. |
| **H2** — the repository chooses the verify sandbox level | **FIXED** | Commits `22cb580` (M11-A2), `6b89f2a`; hardened by `59fded3` (R13). `src/config.rs:517-527` `MachineVerify { min_boundary, verify_root }`; `src/engine/verify.rs:78` `below_floor` (asked at `task create` and at `task config --reload`); `:126` `probe` + `:189` `not_low_writable` (a verify root a Low process can write is refused). Tests: `tests/m11_trust.rs:32` `a_boundary_below_the_machine_floor_is_refused_at_task_create`, `:73` `the_machine_verify_root_wins_and_doctor_shows_its_origin`, `config::tests::the_machine_verify_floor_and_root`, `tests/m11_env_gates.rs:1376` (Windows). **Re-run:** repository `verify.boundary: none` with no machine `verify:` → `task create` fails with `verify_boundary_below_floor` naming both files; no worktree is created. *Not covered by the fix (recorded, not counted as open here):* `policy.allow_submodules` is still a repository-only key (`config.rs:159`, `engine/task.rs:255`), and `verify_root` is the repository's unless the machine pins it — both belong to plan Open decision 18. |
| **H3** — R51 evidence is not bound to the launch mode or the profile | **OPEN** | `src/evidence.rs:249-278` (`evidence_status`) still matches on spike, profile name and CLI version only; the recorded `mode` is never compared; `src/engine_cli.rs:227` always launches `LaunchMode::Restricted`. No fingerprint, no exe hash, no integrity protection of `results.jsonl`. (Same as the plan's known gap EC-24.) |
| **H4** — the protected-state scan treats an unreadable directory as empty | **FIXED (M12)** | Fixed by M12-02, `6233a04`; review fix `857b291`. Was: `src/engine/protect.rs:127-130`: `let Ok(rd) = std::fs::read_dir(dir) else { return Ok(()) };` and `rd.flatten()` — unchanged (the file is identical to `aa0be03`). No "unreadable protected directory" test. |
| **H5** — with any secret configured the redactor holds back the line-ending newline; long silent commands are killed as stalled | **FIXED (M12)** | Fixed by M12-04, `2d17495`. Was: `src/process/redact.rs:180-184`: `final_before = len − (max_len − 1)`, no "everything up to the last `\n` is final" rule — unchanged (file identical). The EC-06 tests still use profiles without secrets. Affects a run whose profile has `secret_env` (`codex-foundry`, `claude-foundry`) or where `CLAUDE_CODE_OAUTH_TOKEN` is set in the parent environment. |
| **H6** — EC-31 and R35 checks are skipped when a Planner/Reviewer turn ends by quota, stop or budget | **FIXED (M12)** | Fixed by M12-03, `69c211b`; review fix `857b291`. Was: `src/engine/runner.rs:2309`: `cut_short` (budget, stop) still returns before `after_read_only_turn` (`:2312`). Inside it, `:2416-2430` the quota branch returns before `protected_violation` (`:2431`) and `read_only_write` (`:2446`). Only the loop-top R35 check (`:484`) would catch a *protected* file on the next drive; an ordinary source edit by the reviewer is still taken for the human's file. No test. |

### 1.3 Medium

| # | Status | Evidence at HEAD |
|---|---|---|
| **M1** verify output buffered without limit | **FIXED (M12)** | Fixed by M12-16, `ff7413e`. Was: `src/runner.rs:138` `out.extend(b)`; `:155-158` a second full copy for `strip_ansi` + redaction; `tail` (`:81-84`) limits lines, not their length. |
| **M2** verify commands spawned with `Redactions::empty()` | **FIXED** | Commits `d82a86d` (M11-E2), `e7e755c` (R8: colour codes stripped before redacting). `src/runner.rs:155-158` (whole output redacted with `spec.redact`), `src/engine/runner.rs:1699` (`task_redactions`), `:1887-1904` (an environment gate adds its `secret_env` values), `src/evidence.rs:130-157` (the R51 `--version` run). Tests: `tests/m11_gates.rs:169` `verify_output_is_redacted_with_the_task_secrets`, `:257` `output_without_secrets_is_unchanged_by_redaction`, `tests/m11_env_gates.rs:515`, `tests/adapters.rs:342`. *Not adopted:* the review's "also scan `DIFF.patch` for known secret values" (`docs/M11-NOTES.md` E2: "Not done"); `toryo xpp build` and `spike pipeline` keep an empty set (`src/xpp_cli.rs:235`, `src/spike/pipeline.rs:86`). |
| **M3** an error before spawn becomes `ProcessCrashed` with no usage → ~600k tokens reserved | **OPEN** | `src/agents/cli.rs:317-323` unchanged (file identical). |
| **M4** Git calls on the task worktree use `Loc::Discover` | **OPEN** | `src/engine/worktree.rs:37`; `src/engine/protect.rs:82, 179, 295` — unchanged. |
| **M5** Low child inherits every inheritable handle | **OPEN** | `src/process/windows.rs:531-533` (the "M6 should switch to PROC_THREAD_ATTRIBUTE_HANDLE_LIST" comment is still there), `:556` `CreateProcessAsUserW`; no handle list anywhere in `src/`. |
| **M6** Medium-IL housekeeping inside the Low checkout follows reparse points | **OPEN** | `src/engine/verify.rs:425-427` still `std::fs::write(co.dir.join(".git"), …)`; `:429-435` `checkout --force` / `clean -ffdx` still run with their working directory in the Low checkout (`src/git.rs:135-138`). Hardening *around* it since the review: `empty_dir` / `remove_entry` never follow a link (`3f7e447`, `919f034`), a Low-writable verify root is refused (`59fded3`), and an environment gate no longer reads the Low checkout at all (`29226ec`). The two cited call sites are as they were. |
| **M7** `ExitInfo.tree_gone` computed, never enforced | **OPEN** | Only `src/process/mod.rs:267-275` and the spike read it; neither the engine nor `LocalRunner` does. Recorded as open in `docs/M11-NOTES.md` (R12 "Open"). |
| **M8** `toryo resume` without an id ignores its flags, starts `New` tasks, stops at the first error | **FIXED (M12)** | Fixed by M12-12, `8fe4f1e`. Was: `src/main.rs:104-116` — no `requires = "id"`; `src/engine_cli.rs:1119-1138` — `ResumeArgs::default()`, every non-terminal task, `?` inside the loop. **Re-run:** `toryo resume --note "for nobody"` (no id) is accepted, prints `▶ resume T-1` for a task in state `New` and tries to start it (stopped here only by the R51 evidence gate, exit 2). |
| **M9** agent/vendor text printed to the terminal unchanged | **FIXED (M15)** | Fixed by M15-01 (X33): `src/term.rs` `text`; `render()` and every human-readable line of the engine and the CLI go through it (`errln!` / `outln!`). Was: `src/engine/flight.rs:373` `format!("!! {reason}")`; `notify::clean` used by the toast only. |
| **M10** a gate decision commits the transition, then appends `DECISIONS.md` | **OPEN** | `src/engine/runner.rs:4451-4455` (transition, then `decisions::append`); same order at `src/engine_cli.rs:823-824`. |
| **M11** `task cleanup` on an `Escalated` task needs no `--force` and deletes the partial snapshots | **FIXED (M12)** | Fixed by M12-12, `8fe4f1e`. Was: `src/engine_cli.rs:554` (`Escalated` is exempt from `--force`), `:578-586` (`worktree remove --force`, `remove_dir_all(run_store)`). |
| **M12** AC lines without `: . ) ：` after the id are skipped silently | **FIXED (M12)** | Fixed by M12-11, `2c9835f`. Was: `src/engine/checks.rs:18-22` — regex unchanged. **Re-run:** a TASK.md with `- AC-1: …`, `- AC-2 — …`, `- AC-3 - …`, `・AC-4：…`, `- AC-５: …`, `\| AC-6 \| … \|` gives `created T-2: 1 acceptance criteria ["AC-1"]`; five criteria vanish without a word. |
| **M13** output decoded as UTF-8 only (cp932 on a Japanese UDE) | **FIXED (M12)** | Fixed by M12-15, `29f6c0e`. Was: `src/runner.rs:139, 158` `String::from_utf8_lossy`; `src/engine/xpp.rs:47-48` (the `VSLANG` comment the review calls wrong); no MSBuild-version warning in `doctor`. Named "next after M11" in `README.md:532`, `docs/M11-NOTES.md:2322`, `docs/HUONG-DAN.md:837`. |
| **M14** infrastructure escalations can only be left with `resume --note` (costs a fix cycle) | **PARTIAL** | Commits `341673a` (R11), `36e28d4` (R16). `src/engine/state.rs:45-51` `MACHINE_CAUSED`; `src/engine_cli.rs:981-987` `machine_caused_stop` → plain `resume` applies `ResumeStage`. Tests: `m11_skills::plain_resume_after_a_missing_skill_costs_no_fix_cycle`, `m11_env_gates::plain_resume_after_an_unavailable_gate_costs_no_fix_cycle`, `::plain_resume_after_an_unavailable_boundary_costs_no_fix_cycle` (Windows), `m9_xpp::a_broken_xpp_toolchain_refuses_admission_or_escalates`. **Left:** `artifact_tampered` / `artifact_missing` — the third class the review names — is not in the list (`src/engine/runner.rs:2508`), so it still costs a fix cycle and an implementer turn. |
| **M15** `events.jsonl` never fsync'd, never reconciled with SQLite | **OPEN** | `src/engine/flight.rs:233-239` (`write_all` + `flush`, no `sync_data`); no reconcile at drive start; `seq` still comes from the file (`:537`). |
| **M16** `--discard-partial` / `--keep-partial` do not apply the HEAD guard; snapshot heads are unpinned | **OPEN** | `src/engine/recovery.rs:588` `discard_partial`, `:641` `keep_partial` — unchanged; no `refs/toryo/snapshots/…` anywhere. |
| **M17** manifest hash discarded; manifest paths not validated (`partial export` can write outside `--into`) | **OPEN** | `src/engine/snapshot.rs:367-371`, `:468` `into.join(&e.path)` — unchanged (file identical). |
| **M18** a verify program is protected only if it exists when the engine starts | **FIXED** | Commits `ea32fa6` (M11-E1), then `af07042`, `5192b56`, `8da7d61` (R14: protection from the base commit), `21e83c9`, `a878e68`, `6696686`. `src/engine/checks.rs:769` `verify_files_at_base`; `src/engine/task.rs:86` `check_entry_points`, `:131` `base_files`. Tests: `tests/m11_gates.rs:111` `a_gate_script_missing_from_the_base_commit_is_refused_at_admission`, `checks.rs:2031` `m18_entry_points_are_protected_before_they_exist`, and the R6–R22 tables. **Re-run:** `commands.verify: sh scripts/verify-new.sh` with the script missing at the base commit → `task create` fails with `verify_script_missing … not in the base commit`. The limits of the detection that the notes record are in the open list (O-41, O-42). |
| **M19** recovery of a crashed Planning/Reviewing attempt escalates on any dirty path | **OPEN** | `src/engine/recovery.rs:230-241` — any changed path, no "before the turn" comparison, no IDE-folder exception. |
| **M20** the `m0-kit` "prototype" feature is on by default and the product depends on it | **OPEN** | `Cargo.toml:14-17`; `tests/common/mod.rs:94-104` (the fixture comes from `toryo spike init-fixture`); 25 files under `tests/` name `m0-kit`; CI's "Core without the M0 kit" step is clippy only (`.github/workflows/ci.yml:62-63`). |
| **M21** every test that runs the real CLI adapters against a fake subprocess is Unix-only | **OPEN** | `tests/regress_agents.rs:805`, `tests/spike_e2e.rs:5`, `tests/redaction.rs:112`, `tests/adapters.rs:310` — the same `cfg(unix)` gates. |
| **M22** `spike pipeline` never calls `unverified_implementers` | **OPEN** | `src/spike_cli.rs:638` — `unverified_profiles` only. |
| **M23** `init-config` overwrites `--out` with no check; m0-smoke regenerates `toryo.yaml` on every run | **OPEN** | `src/spike/init_config.rs:378-379`; `scripts/m0-smoke.ps1:156-167` — unchanged. |

### 1.4 Low

All 23 are **OPEN**. The files behind L11, L14–L18, L22, L23 are identical to `aa0be03`.

| # | Evidence at HEAD |
|---|---|
| **L1** task id joined into the lock path before lookup | **FIXED in M12.** Fixed by M12-12, `8fe4f1e`. Was: `src/engine/lock.rs:31-33`. **Re-run:** `toryo run '../../repo/.git/index'` prints `no task …` and leaves `repo/.git/index.lock`; `git add` in that repository then fails ("Unable to create … index.lock: File exists"). |
| **L2** token cap wraps; NaN/negative weights; `timeout_min: 0` | `src/engine_cli.rs:1427` `n as i64`; `src/engine/task.rs:439` `b as i64`; `src/config.rs:983` checks weight *names* only. **Re-run:** `toryo budget set T-1 --tokens 18446744073709551615` → `admission cap -1`; `status --json` shows `"budget_tokens": -1`. |
| **L3** `BudgetHit` wins over a successful turn | `src/engine/runner.rs:3005-3008` (called at `:932`), `:2309`. |
| **L4** rate-limit Retry and quota Switch return before the path-policy and protected checks | `src/engine/runner.rs:969-975` (quota before `protected_violation`). |
| **L5** `ImplementationDone` and `set_handoff(None)` are separate transactions | `src/engine/runner.rs:1120`. |
| **L6** a missing/edited TASK.md snapshot bails before recovery | `src/engine/runner.rs:124-130`. |
| **L7** `keep_partial` records its commit in a later transaction | `src/engine/recovery.rs:641-680`. |
| **L8** `task create` makes the run store, worktree and branch before the DB row | `src/engine/task.rs:390-402` before the insert at `:442`; only the worktree is removed when the insert fails (`:445-451`). |
| **L9** `TaskLock::drop` leaves the OS unlock to the handle close | `src/engine/lock.rs:177-182`. (The *new* `EnvLock` does unlock explicitly and cites "review L9", `lock.rs:300-305`; the task lock was not touched.) |
| **L10** schema migration in a DEFERRED transaction | `src/engine/store.rs:282` `conn.transaction()`. |
| **L11** snapshot files read whole into memory twice; stale `.staging-*` | `src/engine/snapshot.rs:205, 272, 392, 400`. |
| **L12** `.git/info/exclude` rewritten with read + truncate + write | `src/engine/task.rs:412-420`. |
| **L13** `--accept-protected-change` accepts by path | `src/engine_cli.rs:885, 1028`. |
| **L14** `hash_file` reads whole files; `git diff --no-index` on a FIFO | `src/engine/protect.rs:159-170, 459`. |
| **L15** `spawn_low_integrity` skips `check_spawn_input`, sorts with `to_uppercase` | `src/process/windows.rs:515-517, 533-545`. |
| **L16** `"usage": null` recorded as exact zero | `src/agents/claude.rs:420`, `src/agents/codex.rs:307`. |
| **L17** exec backstop hard-coded to 4 h | `src/agents/cli.rs:283-285`. |
| **L18** `CliSession` has no `Drop` | no `impl Drop` in `src/agents/`; no `shutdown_timeout` in `src/`. |
| **L19** `next:` line and `next_action` unredacted | `src/engine_cli.rs:478, 780, 838, 859, 875`; `src/engine/views.rs:109`. |
| **L20** `println!` in `--json` panics on a closed pipe | `src/engine_cli.rs:1185`. **Re-run:** `toryo status --json` into a closed pipe → `panicked … failed printing to stdout: Broken pipe` at `engine_cli.rs:1185`, exit 101. |
| **L21** `status` / `task show` / `usage --json` have no schema version | `src/engine/views.rs`, `src/engine/usage_report.rs` — no `"schema"` key (confirmed in the `status --json` output). Only the new `config show --json` has one (`src/config_show.rs:121`). |
| **L22** `spike pipeline --id` not validated; `spike codex` without `--worktree` sandboxes the cwd | `src/spike/pipeline.rs:48, 266`; `src/spike/agents.rs:942-946`. |
| **L23** agent-chosen file names reach the next prompt outside `<untrusted_data>`; `neutralise` catches only the exact closing tag | `src/engine/untrusted.rs:48-59`; `src/engine/runner.rs:1065, 2917, 2940` (M11 added two more such `POLICY:` lines — the fence and build-output notes). |

### 1.5 Tests and CI

| # | Status | Evidence at HEAD |
|---|---|---|
| **T1** test Git calls inherit the developer's global config | **OPEN** | `tests/common/mod.rs:211-216, 313` — plain `Command::new("git")`, no `GIT_CONFIG_GLOBAL` / `GIT_CONFIG_NOSYSTEM`; CI still writes a global identity (`ci.yml` step "Git identity for the test repositories"). |
| **T2** `stop took < 6s` with zero margin | **OPEN** | `tests/regress_implement.rs:915` still `Duration::from_secs(6)`. (`tests/engine_e2e.rs:685` allows 8 s — as it already did at `aa0be03`.) |
| **T3** actions on mutable tags, floating `stable`, MSRV never built, no `timeout-minutes` | **OPEN** | `ci.yml:24-25, 70-71`: `actions/checkout@v4`, `dtolnay/rust-toolchain@stable`; no `timeout-minutes`; no `rust-toolchain.toml`. It already cost one CI round: `f5ad5e1` (Rust 1.99 deprecated `AtomicU64::fetch_update` and broke clippy on the pull request). |
| **T4** the quick start uses `bin\toryo.exe`, which is gitignored | **OPEN** | `README.md:86-87`, `.gitignore:4`, `bin/README.md`; `docs/HUONG-DAN.md:117, 123, 852`. |
| **T5** missing regression tests for the high-risk paths | **PARTIAL** | Two of the eight exist: C1 (`config.rs:1561`) and M2 (`tests/m11_gates.rs:169`). **Left:** forged-trailer recovery (H1), unreadable protected directory (H4), redaction with a secret during a long command (H5), write-then-quota in review (H6), rewritten worktree `.git` pointer (M4), Low child with only three handles (M5). |

### 1.6 Maintainability

| Bullet | Status | Evidence at HEAD |
|---|---|---|
| **Mt1** `src/engine/runner.rs` is 4,859 lines; coupling with `recovery.rs` | **OPEN — worse** | 6,008 lines now; the inline `mod tests` starts at `:4712`; `src/engine/recovery.rs:20` still imports `runner::{partial_trailers, writing_stage, FailedTurn, HeadGuard}`. None of the suggested seams exists. M11 put its new logic in new files (`team.rs` 2,717, `checks.rs` 3,230, `cmdline.rs` 1,641, `verify.rs` 1,792 lines). |
| **Mt2** duplicates to merge | **OPEN** | `end_with` `runner.rs:2711` / `finish_and` `:2756` — and a third sibling since R16, `finish_classed` `:2769`; `billing()` `runner.rs:338` and `recovery.rs:66`; `gate_name` `runner.rs:4584` and `store.rs:658`; the review JSON is still parsed untyped (`runner.rs:2862, 3894, 4151, 4332`). |
| **Mt3** `src/engine_cli.rs` (1,616 lines): repeated closures, positional tuple, no split | **OPEN** | 1,746 lines; `u.1 == a.id && u.3 == "unknown" … u.5` at `:302-303`; no `cli/` folder. |
| **Mt4** test hooks honoured in release builds | **OPEN — more of them** | `TORYO_TEST_CRASH_AT` / `TORYO_TEST_PANIC_AT` `runner.rs:87-99`, `TORYO_TEST_TOAST_LOG` `notify.rs:327-329`; new in M11: `TORYO_TEST_LABEL_ERROR` `verify.rs:116` (makes every Low-label call fail), `TORYO_TEST_ENV_LOCK_POLL_MS` `runner.rs:4137`. No `cfg(debug_assertions)` anywhere in `src/`. |

### 1.7 Every High or Medium item that is still OPEN or PARTIAL

High, OPEN: **H3**.
Medium, OPEN: **M3, M4, M5, M6, M7, M10, M15, M16, M17, M19, M20, M21, M22, M23**.
Medium, PARTIAL: **M14** (left: `artifact_tampered`).
