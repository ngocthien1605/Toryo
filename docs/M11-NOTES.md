# M11 notes — decisions and deviations while building M11 (`M11_task.md`)

Every entry: what was decided, why, and where. The owner folds these into plan v0.9.

## Morning summary (2026-10-02)

**All 17 tasks of `M11_task.md` are DONE; nothing PARTIAL or NOT STARTED.** Branch `m11/orgchart` from
`main` @ `aa0be03`, 21 commits, **not pushed, not merged**. After each group (A, B, C, E, D) an independent
review subagent read the group's diff against the task list; its findings were fixed in one follow-up
commit per group (the "Group X review" sections below say what was fixed and what was only recorded).

| Commit | Task | State |
|---|---|---|
| `cd78b22` | A1 — repository files without machine-only keys; tagged keys refused (C1) | DONE |
| `22cb580` | A2 — machine floor for `verify.boundary`, machine `verify_root` (H2) | DONE |
| `6b89f2a` | A2 — group A review: `doctor` and the Low-IL canary use the root in force | DONE |
| `37fe444` | B1 — types: org chart, role files, machine `seats`/`skills_dir`/`environment_gates` | DONE |
| `8f760f6` | B2 — loader, merge, freeze and hash; `toryo config show` | DONE |
| `c9f2655` | B3 — validation pass, key paths, "did you mean" | DONE |
| `b11dad5` | B3 — group B review: machine-seat fallback, reach check, ≥ 1 check gate | DONE |
| `995697d` | C1 — layered role instructions and culture | DONE |
| `fc11289` | C2 — the implementer's write fence | DONE |
| `8665b24` | C3 — `reviewer-differs-from-builder` off / warn / error | DONE |
| `3a7aa61` | C4 — split limits | DONE |
| `ea32fa6` | E1 — gate variables, M18 | DONE |
| `d82a86d` | E2 — verify output redacted (M2) | DONE |
| `8ecc409` | E3 — environment gates, lock, deployment record (+ group C review nit) | DONE |
| `af3743f` | D1 — skill lookup | DONE |
| `9ffd27c` | E3 — group E review: M18 per program, cwd lookups, lock folder, output order | DONE |
| `a7f7d36` | D2 — the plan's `skills`, the planner's menu, the checks | DONE |
| `8a846e9` | D3 — skills through the context loader | DONE |
| `4deb12e` | F1 — templates | DONE |
| `a99e23e` | D3 — group D review: exact names, legacy plans, skipped skills | DONE |
| (last) | F2 — README, HUONG-DAN §5.9, migration guide, this summary | DONE |

**Checks** (§0.2) green before every commit **on this Windows machine only** — the Linux test build did not compile (review blocker, fixed in R1), so F2 AC-2 was not met on Linux: `cargo fmt --check`, both `clippy -D
warnings`, `cargo test --locked`. Tests: **519 passed, 0 failed, 6 ignored** (441 / 6 at `aa0be03`). New test
files: `tests/m11_{trust,orgchart,roles,gates,env_gates,skills}.rs`; unit tests in `team.rs`, `config.rs`,
`context.rs`, `checks.rs`, `state_tests.rs`, `views.rs`, `lock.rs`, `render.rs`, `runner.rs`. No real agent was run,
nothing ran against a D365FO environment (environment-gate tests use `pwsh`/`sh` scripts in temp folders), and
your configs and data dirs were not read. No new crate.

**Backward compatibility:** a repository without `.toryo/toryo-orgchart.yaml` keeps its configuration hash
(`templates_keep_their_m10_hash` pins the three shipped templates to the `aa0be03` values) and its behaviour,
except what the task list applies to everyone (A1, A2, E1/M18, E2) — and these error **texts**: an unknown
key in any machine config now gets the friendly form with a hint, `roles.<x>: unknown role …` gets a
`Did you mean` suffix (B3), and a repository with `verify.boundary: none` now needs `verify: { min_boundary:
none }` in the machine config (H2, the point of the fix). Existing tests changed only where a task says so: the
fixture's machine config allows `boundary: none` (A2, 4 tests append `MACHINE_FLOOR`), the EC-07 unit test
expects a not-yet-existing program to be protected (M18), the flight tests' exhaustive lists include the new
events, the state tests construct `PlanPolicyViolation` with `reason: None`.

**Deviations from the task list** (details in the sections below):
1. A2: the floor is also checked at `task config --reload`; `task create --trusted-code` was not added.
2. B3: added — the implementer's `before_handoff` needs at least one `check` gate; every declared environment
   gate is checked against the machine, also an unused one.
3. C2: an empty `writes` means "no allowlist" (only `never` applies).
4. C4: `PathPolicyViolation` counts against `max_gate_retries`, `FinalRejected` against `max_review_rounds`; a
   missing one of the two split keys is 2; `--max-fix-cycles` with split limits is refused; the `--json` keys
   appear only for a split-limit task.
5. E1 (group E review): the M18 entry point is each program's *script* (PowerShell `-File`, the first argument of
   `sh`/`bash`/`python`/`node`/…, `cmd /c <file>`), not every `-f`.
6. E2: the whole output is redacted at the end (a per-stream redactor could reorder stdout and stderr).
7. E3: `status` names the lock holder from `environment_deployments` (Windows cannot read a locked file); the
   table has two extra columns (`gate`, `machine`); lock files are `locks/env/<lock>.lock`; the gate environment
   gets `NoDefaultCurrentDirectoryInExePath=1`; `environments[]` appears only when there is a row.
8. D2: `plan.schema.json` gains the optional `skills` for every agent (legacy plans drop it before storing).
9. F1: two skill stubs and `CULTURE.md` added so the template set loads; a second `toryo.yaml` example
   (`toryo.orgchart.yaml.example`) instead of changing the M10 one.

**Deviations from the drafts:** the lock holder's label is written into the lock file but `status` reads the
holder from the deployment table (above); `reviewer-differs-from-builder: error` escalates before the review turn
(the draft says "refuse to start the stage" — the same thing, with `resume --note` still possible); the
`skills_dir` in the org chart and the `flow`/`reads`/`agent` keys get their own error messages as the task list
says.

**Open (your decision; the conservative choice is in place):**
- A task created **before** M11 with `verify.boundary: none` keeps verifying that way (its config is frozen).
- After a `same_vendor_review` escalation, `resume --note` costs one more implementer turn before the review is
  refused again (the fallback is sticky); a "skip the check once" switch was not added.
- Unix only: a killed toryo leaves an environment gate's process group running while the lock is free (review
  M7); Windows kills it with the Job.
- An environment gate's script can still load a DLL from the verify checkout (Windows DLL search) — the script
  contract says to call tools by absolute path.
- `task create --repo X` reads `.toryo/` from `project.root`, not from X.
- `instructions: ../planner/x.md` (outside the role folder, inside the repository) is refused.
- **Next after M11: review M13** — cp932 output on Japanese Windows will also garble DB-sync output on the Tokyo
  machine.

**Try first** (build first: `cargo build --release`):
1. `toryo config show` on a copy of the drafts: copy `templates\d365fo\.toryo\` and `templates\d365fo\CULTURE.md`
   into a scratch Git repo, use `templates\d365fo\toryo.orgchart.yaml.example` as its `toryo.yaml` (set
   `project.root`), and a copy of your machine config with `environment_gates.dbsync` — run
   `toryo --config <scratch>\toryo.yaml --machine-config <copy> config show` (and `--json`). Every seat, gate and
   setting says where it came from; a typo shows the "did you mean" message.
2. A fake run in org chart mode: `cargo test --release --test m11_orgchart -- --nocapture`, or by hand with a
   temp `TORYO_DATA_DIR` and `--fake-agents` (the scripts in `tests/m11_*.rs` show the shape).
3. Write `dbsync.ps1` against the contract in `templates\toryo.local.yaml.example` (exit code; deploy only
   `$env:TORYO_VERIFY_CHECKOUT` / `$env:TORYO_VERIFY_BUILD`; idempotent; error lines at the end; never write into
   the checkout; never prompt; absolute tool paths). Start with a script that prints `$env:TORYO_SHA` and exits 0,
   then `toryo status` shows `d365-env holds <task> @ <sha>`.
4. If a repository uses `verify.boundary: none`, add `verify: { min_boundary: none }` to the machine config.

## Review fixes (2026-10-02)

Task list: `M11-review-fixes.md` (review of 2026-10-02, built and run on Linux by the reviewer). Same branch
`m11/orgchart`, from `4bfa291`. One commit per task.

### Progress

- [x] R1 — the test build compiles on Linux again
- [ ] R2 — environment gates see exactly the checkpoint; neutral working directory
- [ ] R3 — environment gate `program`/`args`: validate what runs
- [ ] R4 — fence patterns that match nothing; the culture file is protected
- [ ] R5 — a skill that cannot be delivered stops the turn; the budget check covers the whole chain
- [ ] R6 — M18 script detection: no false refusals, no missed scripts
- [ ] R7 — the same-vendor check counts every turn that ran
- [ ] R8 — cheap Low fixes and test gaps

### Decisions of the list (X1–X5) and the answers to "Open"

- **X1** an environment gate's working directory is an empty folder agent code cannot write (R2); **X2** before
  the first environment gate the checkout must be exactly the checkpoint, so an org chart with an environment
  gate needs `verify.checkout: clean` (R2); **X3** `%VAR%` in an environment gate's `program`/`args` is expanded
  once, at load (R3); **X4** `reviewer-differs-from-builder` counts every implementer turn that ran (R7); **X5**
  the same-vendor note keeps `(Claude both)` (R7).
- Answers to the M11 "Open" list: a pre-M11 task with `boundary: none` keeps it — accepted; the extra turn after
  `same_vendor_review` — accepted; Unix process group of a killed toryo — accepted, note kept; a DLL from the
  checkout — closed by X1; `task create --repo X` — refused in org chart mode unless X is `project.root` (R8);
  `instructions:` outside the role folder — stays refused.

### R1 — the test build compiles on Linux again

- `tests/adapters.rs::local_runner_captures_plain_text_and_exit` (`#[cfg(unix)]`) built a `RunSpec` without the
  `redact` field E2 added: `E0063` on Linux, invisible here because Windows compiles the test out.
- §0.3 applied to every type and signature M11 changed (`RunSpec`, `PipelineEvent::PlanPolicyViolation`,
  `context::Request`/`TeamContext`, `EngineEvent` + `VerifyCommand.kind`, `Machine`, `MachineConfig`,
  `Checkout`, `Effective`, `CommandSpec`, `Origins`, `evidence::run_version`): read every `cfg(unix)` /
  `cfg(not(windows))` / `cfg(all(unix, …))` block of `src/config.rs`, `src/engine/{artifacts,protect,snapshot,
  runner_review_tests}.rs`, `src/engine_cli.rs`, `src/git/canary.rs`, `src/notify.rs`, `src/process/{mod,unix}.rs`,
  `tests/{adapters,m10_notify,m10_views,m9_xpp,redaction,regress_agents,regress_engine,regress_policy,
  spike_e2e}.rs`. That literal was the only one.
- **Not compiled on Linux here**: WSL exists on this machine but has no cargo (nothing installed for this). The
  reviewer's Linux run with this one line was clean (fmt, both clippy, 551 tests passed) — the first CI run
  (`ubuntu-latest`) is the real check.

## Progress

Branch `m11/orgchart` (from `main` @ `aa0be03`). One commit per task.

- [x] A1 — repository files parsed without machine-only keys; tagged keys refused (C1)
- [x] A2 — machine floor for `verify.boundary` and `verify_root` (H2)
- [x] B1 — types for the org chart, role files and the new machine keys
- [x] B2 — loader: discover `.toryo/`, merge, freeze and hash; `toryo config show`
- [x] B3 — validation pass: cross-references, kinds, order, "did you mean"
- [x] C1 — layered instructions and `culture` in the context loader
- [x] C2 — the implementer's write fence (`writes` / `never`)
- [x] C3 — `checks.reviewer-differs-from-builder`: off / warn / error
- [x] C4 — split limits: `max_gate_retries`, `max_review_rounds`
- [x] E1 — check gates: mapping, gate variables, M18
- [x] E2 — verify output redacted with the task's secrets (M2)
- [x] E3 — environment gates: run, lock, record what is deployed
- [x] D1 — skill lookup (repo, then machine) and the menu
- [x] D2 — the plan's `skills` field and its checks
- [x] D3 — skills delivered through the context loader
- [x] F1 — templates
- [x] F2 — docs, migration guide, morning summary

## A — Trust floor

### A1 — repository files without machine-only keys; tagged keys refused (review C1)

- **`RepoFile`** (`src/config.rs`, private): the repository `toryo.yaml` as written —
  `deny_unknown_fields`, **no** `profiles` / `toolchains` fields. `Effective::load` parses the
  text into it and builds `RepoConfig` from it (`profiles` empty until the machine config is
  merged). `RepoConfig` keeps its shape, so the canonical JSON and every hash are unchanged
  (`templates_keep_their_m10_hash` pins the three shipped templates to the values computed on
  `main` @ `aa0be03`).
- **Tags.** `config::untagged` takes the tags off a YAML value; `machine_only_part` looks the
  machine-only keys up through it, so `!x profiles:` gets the usual R60 refusal (names
  `profiles:`, the machine config and the block to move). `config::refuse_tags(doc, file)` is
  the one shared walk: every tagged top-level key or value of a repository file, all in one
  error naming the file and each key. The org chart and the role files (B1) use the same two
  functions.
- Regression tests next to the R60 tests: `a_tagged_profiles_key_is_refused` (the review's
  reproduction, plus `!x roles` / `policy: !y` / `!t toolchains`) and
  `the_repository_file_has_no_profile_fields`.

### A2 — a machine floor for the verify boundary (review H2)

- **Machine key** `verify: { min_boundary: low_integrity | none, verify_root: <path> }`
  (`config::MachineVerify`, `deny_unknown_fields`, both optional). Merged into
  `Effective.machine_verify`, serialised into the canonical JSON as `machine_verify` **only when
  one of the two is set** — a configuration without it keeps its hash (the pinned hashes of
  `configs_without_toolchains_keep_their_hash` and `templates_keep_their_m10_hash` hold).
- **Floor.** `engine::verify::below_floor(cfg)`: repository `verify.boundary: none` with a floor of
  `low_integrity` (the default) is refused at `task create` — before the worktree, the run store
  or anything else is created — and at `task config --reload` (a reload must not go below the
  floor either; not in the task list, the conservative choice). The message
  (`verify_boundary_below_floor: …`) names the repository file and the machine config, gives the
  two ways out (keep `low_integrity` in the repository, or the owner sets `min_boundary: none` on
  this machine) and says that environment gates are the only sanctioned way to run something with
  the owner's full rights. With `min_boundary: none` the run is unchanged: every
  `VERIFY_RESULT.json` still says `none (trusted-code mode …)`.
- **`task create --trusted-code` was not added**: the machine key is enough for M11 (the task list
  says so); a per-task switch can come later.
- **`verify_root`.** The machine's replaces the repository's (`verify::verify_root`). Origins:
  `Origins.verify` (`boundary`, `verify_root`, `min_boundary`); `doctor` prints one `info` line per
  verify setting with its source (`verify verify_root … — source: the repository config,
  overridden by the machine config`), also in `--json` (`source`). `config show` (B2) reuses it.
  A reload that changes the machine `verify:` counts as a verify-configuration change (the verify
  result is invalidated, like a change of `verify.*`).
- **Who is affected.** Every repository with `verify.boundary: none` — on Linux, where Low IL does
  not exist, that is every repository — now needs `verify: { min_boundary: none }` in the machine
  config. This is the point of H2, and the message says what to do.
- **Test fixture.** `tests/common` writes `MACHINE_FLOOR` (`verify: { min_boundary: none }`) into the
  fixture's machine config, because the fixture repository verifies with `boundary: none`; the four
  tests that rewrite the machine config (`engine_policy`, `m10_events`, `m10_notify`, `m10_usage`)
  append it too. No assertion changed. New: `config::tests::the_machine_verify_floor_and_root`,
  `tests/m11_trust.rs` (AC-1, AC-2).

### Group A review (independent subagent) — follow-up commit

- Fixed: `doctor`'s verify-root check and `spike lowil-canary` used the repository's
  `verify_root`; both now use the root in force (`verify::verify_root_setting`: the machine's, else
  the repository's), so the Low-label evidence is gathered where the tasks will run. `doctor`'s
  `boundary: none` warning says when `task create` will refuse it. Tests:
  `doctor::tests::the_verify_root_check_uses_the_machine_root`,
  `config::tests::a_tagged_document_is_refused`.
- Not changed (recorded): a task created **before** M11 with `boundary: none` keeps verifying that
  way — its configuration is frozen (R35) and `toryo run` never re-reads the machine floor. The
  floor applies at `task create` and `task config --reload`. `verify_root` is shown as "repo,
  overridden by machine" also when the repository only had the default.

## B — The `.toryo/` loader

### B1 — types for the org chart, role files and the new machine keys

- **New module `src/team.rs`** (`config.rs` keeps `Effective`): `OrgChart`, `Seats` (a struct with
  exactly `planner`, `implementer`, `reviewer` — a fourth or misspelt seat is an unknown field, a
  missing one a missing field), `Seat`, `GateDecl` (`kind: check | environment` = `GateType`, the
  name `GateKind` is taken by the approval gates of the state machine), `IfMissing`, `Limits`,
  `Checks` / `Level` (`off | warn | error`, default `warn`; YAML key
  `reviewer-differs-from-builder`), `RoleFile`. All `deny_unknown_fields`, mirroring the drafts.
- **Parsing** (`team::parse_repo_file`): YAML → `refuse_tags` (C1, the A1 function) → the removed
  keys → the typed parse. **Removed keys** get their own message, all of a file at once, each
  with its key path: `flow` (the flow is fixed; move the retry knobs to `limits:`), `skills_dir`
  in the org chart (belongs in the machine config), `seats.<seat>.agent` (name a Toryo profile:
  `profile: codex-openai`), `reads` in a role file (Toryo decides what each stage sees).
- **Durations** (`team::parse_duration`): `<n>s | <n>m | <n>h`, n ≥ 1, ASCII digits only;
  anything else (`0s`, `20`, `20min`, `1.5h`, `1d`, an overflow) is an error naming the value.
- **`CommandSpec.timeout_sec`** (the choice the task list asks to record): a new optional field in
  seconds, serialised only when set, winning over `timeout_min` in `xpp::command_timeout`. Exact
  (`30s` stays 30 s, no rounding to minutes) and every existing hash is unchanged.
- **Machine config**: `seats:` (the new spelling of `roles:`, same `Role` fields; both in one file
  is an error naming the file; `seats:` is then read exactly like `roles:`, so it hashes the
  same), `skills_dir: Option<String>`, `environment_gates: { <name>: EnvGate }` with
  `EnvGate { program, args, lock, env, secret_env }` (`secret_env` values are the existing
  `credman:` / `env:` references). None of them is frozen or hashed in `toryo.yaml`-only mode.
- **Fixtures** `tests/fixtures/m11/`: the three owner drafts, copied with their comments; in the
  machine draft the owner paths (`C:\Users\<you>\…`, `C:/Temp/AgentShare/skills`,
  `C:\.thien\toryo-gates\dbsync.ps1`) are `'@TMP@/…'`, which the B2 tests replace with a temp
  folder. Tests: `team::tests::{the_owner_drafts_parse, removed_keys_have_their_own_message,
  seats_are_exactly_three, durations}`, `config::tests::machine_seats_are_the_new_spelling_of_roles`.

### B2 — the loader: discover `.toryo/`, merge, freeze and hash; `toryo config show`

- **Discovery** (`team::org_chart_root`): after `toryo.yaml` is parsed, `<project.root>/.toryo/
  toryo-orgchart.yaml` (with `project.root` expanded; a root that does not expand is `toryo.yaml`-only
  mode, as before). Read from the repository checkout, never a task worktree. Note: `task create --repo
  X` does not change where the org chart is read from (it is a property of the configuration, like
  `toryo.yaml` itself).
- **Merge** (`team::load` → `team::Loaded`, applied in `Effective::load`): the seats become
  `repo.roles` (an omitted `fallback` is `Some([])` — a chain of one); the implementer's `before_handoff`
  check gates become `repo.commands.verify`, in that order (`team::gate_command`, `timeout_sec` from
  the gate's `timeout`); a lone `limits.max_fix_cycles` becomes `repo.pipeline.max_fix_cycles`. So the
  engine reads the same fields as in M10. Machine `seats:` / `roles:` replace repository seats
  afterwards (`RepoOverriddenByMachine`). `verify_commands` never falls back to `scripts/build.ps1` in
  org chart mode.
- **Freeze and hash**: `Effective.team: Option<team::Team>` — team name, culture path, every gate
  (declaration + the machine's `EnvGate` for an environment gate, so the command that will run with
  full rights is the one the task was created with), limits, checks, each role file's parsed content
  (purpose, instructions path relative to the repo root, skills, writes, never, before_handoff) and
  the skill roots (`.toryo/skills`, the machine's `skills_dir`). Serialised as `team` only in org
  chart mode — M10 configurations keep their hash (`templates_keep_their_m10_hash`). `from_frozen`
  reads `team` back without touching `.toryo/` (tested by deleting `.toryo/` before the reload).
- **Errors** (all at once, with the other validation problems): `roles:`, `commands:` or
  `pipeline.max_fix_cycles` in `toryo.yaml` (each names both files and where the setting lives now);
  a missing role file; a role file that does not parse; a leftover `.toryo/roles/<role>.md` ("move it
  to `.toryo/roles/<role>/instructions.md`"); a non-empty `fallback` on the planner or reviewer, in the
  org chart or from a machine seat override (an explicit `fallback: []` is accepted — it says
  nothing); `max_fix_cycles` together with a split limit; a bad `timeout`; `toryo:` other than 1. The
  org chart itself must parse (YAML, tags, removed keys, serde) before anything else is reported.
- **Origins**: `Origins.gates` (`repo` = a check gate or an environment gate this machine does not
  define; `machine` = an environment gate whose command is the machine's) and `Origins.skills_dir`.
  `Effective.files`: every file read (toryo.yaml, machine config, org chart, role files) with its
  SHA-256 — not hashed, empty for a frozen configuration.
- **`toryo config show [--json]`** (`src/config_show.rs`, new top-level `config` command): the text
  form and the JSON document (schema 1, in README "The effective configuration (M11)"). In
  `toryo.yaml`-only mode the verify commands are listed as `kind: check` gates.
- Tests: `team::tests::{the_draft_set_loads_freezes_and_reloads, a_setting_in_both_places_is_refused,
  fallbacks_overrides_and_leftover_role_files}` (AC-1, AC-3) and `tests/m11_orgchart.rs` (`config show`
  through the CLI, a task frozen and reloaded with the same digest, a fake run to the final gate —
  AC-1, AC-4; `toryo.yaml only` without an org chart). `tests/common::setup_orgchart` builds the
  fixture repository in org chart mode for the later groups.

### B3 — validation pass: cross-references, kinds, order, "did you mean"

- **`team::problems(cfg, root, machine)`** — a pure pass over the loaded configuration, called from
  `Effective::validate_with` in org chart mode, so its problems are reported **with** the loader's
  (B2) and `config::problems`, all in one `invalid configuration:` error. Every message starts with
  the file and the key path (`…\toryo-orgchart.yaml: seats.implementer.profile: …`,
  `…\roles\implementer\toryo-role.yaml: before_handoff: …`, `<machine config>:
  environment_gates.dbsync.args: …`). Rules, as listed in the task: instructions files exist (and
  stay inside the repository); seat `profile`/`fallback` names exist in the machine profiles (the
  message points at the machine config, R60 — in org chart mode this replaces the M10 one-at-a-time
  `role … refers to unknown profile` bail); `before_handoff` names a declared gate; only the
  implementer has `before_handoff`, `writes`, `never`; a check gate is exactly one of `program` and
  `tool` (the same `verify_command_problems` as `commands.verify[]`, now `pub(crate)` with a full
  label, so `tool: xpp` needs `metadata`, `projects` and the toolchain as today); an environment gate
  has none of `program`/`args`/`tool`/`metadata`/`projects`; `if_missing` only on environment gates;
  every check gate before every environment gate; an environment gate the machine does not define is
  refused unless `if_missing: skip` (naming `environment_gates.<name>`); a machine `EnvGate` has a
  `lock`, a program that is absolute or a bare PATH name, and every path-like argument absolute and
  not inside `project.root`, `project.worktrees_root` or the verify root (also an absolute program
  inside them is refused); the culture file exists. Skills are added in D1.
- **Added (conservative, not in the list):** the implementer's `before_handoff` may not be empty —
  M10 refused a configuration without verify commands, and org chart mode would otherwise review
  unverified work. Every declared environment gate is checked against the machine, also one that no
  `before_handoff` names.
- In org chart mode `config::problems` skips `commands.verify[]` (they are the check gates, checked
  as `gates.<name>`).
- **"Did you mean"** (`team::did_you_mean`, ~20 lines of Levenshtein, no new crate): a single
  case-insensitive match, else the single candidate within edit distance 2; a tie or nothing close
  gives no hint. Used for unknown seats, gates, profiles (and skills in D1), for the unknown role
  keys of `toryo.yaml` (`roles.reviwer: unknown role (valid: …) Did you mean 'reviewer'?` — the old
  text is kept, the hint appended), and in **`team::friendly_serde`**, which rewrites serde's
  `unknown field `x`, expected one of …` / `missing field` into `<path>.<key>: unknown key 'x'. Did
  you mean 'y'? (valid: …)` (`seat 'reviwer' not found. Did you mean 'reviewer'?` under `seats`) for
  the org chart, the role files and the machine config. The repository files are now typed-parsed
  from their text (not from the YAML value) so serde reports the key path and line.
- Tests (`team::tests`): `did_you_mean_suggests_only_one_close_candidate` (AC-3),
  `unknown_keys_name_the_path_and_the_hint`, `a_seat_profile_must_exist_on_the_machine`,
  `before_handoff_names_order_and_owner`, `gate_kinds_and_their_fields`,
  `environment_gates_on_this_machine`, `culture_and_instructions_must_exist` (AC-1, one per rule),
  `three_problems_are_reported_at_once` (AC-2, four problems from three files in one error).

### Group B review (independent subagent) — follow-up commit

- **Fixed:** a machine seat override without `fallback` fell back to the planner's profile in org
  chart mode (`task::roles` maps `None` to the planner) — now `Some([])` like an org chart seat (D7).
- **Fixed:** the "out of the agents' reach" check compared strings only. It now compares a
  lexical form (`\\?\` prefix dropped, `.`/`..` resolved, Windows' trailing dots and spaces
  dropped, case folded) of the canonicalised path when it exists (junctions, 8.3 names); a `..`
  segment in a machine gate's program or argument is refused outright; the path in a
  `--name=value` / `-Name:value` argument is checked too; a Windows switch (`/c`) is not a path.
  **Limit (documented in the script contract, F1/F2):** a bare-name argument (`cmd /c build`,
  `python -m deploy`) is resolved by the program against its working directory — the verify
  checkout — so the script contract says to name scripts by absolute path. The program itself is
  resolved to an absolute path through PATH before it starts (E3), never from the checkout.
- **Fixed:** `before_handoff` now needs at least one **check** gate (environment gates alone, or
  skipped on a laptop, would hand unverified work to review).
- **Fixed:** an absolute `culture:` / `instructions:` path is refused as written ("must be
  relative") instead of being normalised into a relative one.
- Tests added for every loader problem the review listed (missing role file, version ≠ 1,
  `max_fix_cycles` + split, bad timeout, a check gate with no command, absolute paths, the
  friendly missing-seat message, the machine override fallback, the reach cases).
- **Deviations from "byte for byte as in M10" (error texts only, valid configurations and hashes
  unchanged):** an unknown or missing key in **any** machine config now gets the friendly form
  (`<file>: <path>.<key>: unknown key 'x'. Did you mean …? (valid: …) (line N)` — serde's column is
  dropped); `roles.<x>: unknown role (valid: …)` gets a `Did you mean` suffix when one role is
  close. Both are B3 behaviour; existing tests assert with `contains` and pass unchanged.
- Not changed (recorded): `templates_keep_their_m10_hash` reads the dogfood template, whose
  `project.root` is this repository — a `.toryo/toryo-orgchart.yaml` at the root of the toryo
  repository would switch it to org chart mode. `instructions: ../planner/x.md` (inside the
  repository but outside the role folder) is refused — conservative. A misspelt machine seat
  (`seats: { reviwer: … }`) is reported as `roles.reviwer: unknown role … Did you mean 'reviewer'?`
  without naming the machine file. Unknown enum values (`kind: chek`) keep serde's message.

## C — Roles

### C1 — layered instructions and `culture` in the context loader

- `context::Request.team: Option<TeamContext>` (org chart mode only, built by `context::request`
  from the frozen `team` section): the culture file and the role's `instructions.md`, both in the
  **repository checkout** (`tasks.repo`), like sources 2–4.
- **Source 3** ends with the culture (`culture (CULTURE.md)`, heading `Team culture (…)`), never
  trimmed. **Source 4** is two parts, both never trimmed and hashed: the embedded role template
  (`contracts::role`) **always**, then `instructions.md` under `Role instructions (team)` (label
  `.toryo/roles/<role>/instructions.md`). A repository adds to the built-in rules (output contract,
  no Git, untrusted data) and can no longer remove them: in org chart mode a
  `.toryo/roles/<role>.md` is never read (B2 refuses it at load; the loader ignores it at attempt
  time too). **This closes the M4 gap** where a repository file replaced the embedded template —
  legacy mode keeps that behaviour unchanged (`repo_role_template_overrides_the_embedded_one`
  passes unchanged).
- A culture or instructions file that is gone at attempt time is skipped and reported (`skipped`),
  like the other sources; the task was validated at `task create`.
- Org chart paths in messages use the platform separators (`team::org_chart_path`).
- Tests: `context::tests::org_chart_mode_layers_the_role_instructions_after_the_embedded_rules`
  (each role; the legacy file present and ignored; never trimmed under a 1 KB budget),
  `instructions_and_culture_are_hashed` (AC-2), `tests/m11_orgchart.rs::
  context_show_layers_the_team_instructions_after_the_embedded_template` (AC-1 through the CLI).

### C2 — the implementer's write fence (`writes` / `never`)

- `checks::Protected` gains an optional fence (`with_fence(writes, never)`, built by
  `runner::task_policy` from the frozen implementer role in org chart mode). `fence_rule(path)`:
  `Never` when a `never` glob matches (never wins), `Outside` when `writes` lists anything and no
  glob matches, `None` when the task's `## Allow protected` lists the path (it opens fence entries
  like protected paths; `deny_paths` still cannot be opened). Same matcher as the other rules
  (`glob_ci`: case-insensitive, `/` and `\` alike, `dir/**` also matches `dir`). `violates()` adds
  the fence — it only **adds** violations — while `matches()` (the R35 snapshot) is unchanged.
- **Decision:** an empty `writes` means "no allowlist" (only `never` applies), not "nothing may be
  written" — a role file without `writes` must not fence everything.
- **Plan time:** a `files_likely_involved` entry outside the fence joins the automatic re-plan
  (`Ev::PlanPolicyViolation`, bounded by `MAX_PLAN_AUTO_REJECTIONS`); the feedback names each path
  and its rule: ``"`docs/x.md` — outside the implementer's write fence (`writes: [src/**]`)"`` /
  ``"… — a `never` path (`never: [**/Descriptor/*.xml]`)"``, then any protected or denied paths.
- **After each build turn:** a changed path outside the fence is restored and sent back like a
  denied path (one fix cycle, as M10); the fix note names the rule (``"`…/Model.xml` (touched a never
  path (`never: [**/Descriptor/*.xml]`))"``). Crash recovery uses the same `violates()`.
- When no fence rule is broken, the M10 texts (stderr and feedback) are used unchanged.
- No fence for planner and reviewer (read-only already) and none in legacy mode.
- Tests: `checks::tests::the_write_fence` (AC-3 case and `\`, AC-4 protected/denied inside
  `writes`, allow-protected), `runner::tests::fence_feedback_names_the_rule`,
  `tests/m11_roles.rs` — `a_plan_outside_the_write_fence_is_replanned` (AC-1),
  `a_never_path_is_restored_unless_the_task_allows_it` (AC-2, also with `## Allow protected`).

### C3 — `checks.reviewer-differs-from-builder`: off / warn / error

- `runner::implementer_profiles(store, t)` — the profiles of the **succeeded** implement/fix
  attempts (the profile that actually ran, after any fallback), factored out of
  `final_gate_notes`; `same_vendor` compares their profile `kind` with the reviewer's.
- `warn` (the default, and the only behaviour in legacy mode): today's "same vendor" note at the
  final gate, byte-identical. `off`: no note. `error`: before a **review** turn starts (after the
  EC-10 re-verify check, before admission and before the attempt begins), the task escalates with
  the typed class `same_vendor_review` and a message naming both profiles and the ways out
  (`approve --gate final --force` to review it yourself, or a reviewer of another kind via machine
  `seats:` + `task config --reload`, then `resume --note`). `resume --note` works as for any
  escalation (R5).
- **Note:** after `resume --note` the implementer index is sticky (R49), so the next review is
  refused again unless the reviewer seat changed — the message says so implicitly by offering the
  two ways out. A "skip the check once" switch was not added.
- Tests: `tests/m11_roles.rs::reviewer_differs_from_builder_off_warn_error` (AC-1: the
  quota fallback codex-openai → claude-max with a claude-max reviewer; `events.jsonl` has the
  `escalated` with `reason_class: same_vendor_review`; no review attempt) and
  `different_vendors_pass_at_every_level` (AC-2).

### C4 — split limits: `max_gate_retries`, `max_review_rounds`

- `state::Machine` gains `split: Option<SplitLimits { max_gate_retries, max_review_rounds,
  gate_retries, review_rounds }>`, `#[serde(default, skip_serializing_if = "Option::is_none")]` —
  a machine stored by M10 reads back unchanged and serialises to the same JSON
  (`an_m10_machine_reads_back_and_behaves_the_same`). `Machine::new(max)` is unchanged;
  `Machine::with_limits(gate, review)` sets the split bounds. `fix_cycle` still counts every entry
  into `Fixing` (it names the `fix-N` stages).
- Which counter (the task names two; decided for the others): `VerifyFailed` → gate retries;
  **`PathPolicyViolation`** (the implementer's own fence/policy violation) → gate retries too (a
  build-stage bounce); `ChangesRequested` → review rounds; **`FinalRejected`** (the human's final
  rejection) → review rounds too (a review-stage bounce; M10 also bounded it). `resume --note`
  still grants one more cycle without touching the counters, as in M10.
- The escalation names the limit: `2 gate retries used (limits.max_gate_retries) — not accepted
  (last: verify failed)`; the typed class (`escalated.reason_class`) is `max_gate_retries` /
  `max_review_rounds` instead of `max_fix_cycles`.
- `task::new_machine`: split limits when the org chart has either key — a missing one of the two
  is **2** (today's default); `--max-fix-cycles` together with split limits is **refused** (it sets
  the single counter; the conservative choice instead of silently overriding both). A lone
  `limits.max_fix_cycles` keeps the single counter.
- `status` prints `fix 1 (gate retries 1/1, review rounds 0/3)`; SUMMARY.md likewise;
  `status --json` / `task show --json` get `gate_retries`, `max_gate_retries`, `review_rounds`,
  `max_review_rounds` **only for a split-limit task** (a single-bound task's document is unchanged
  — so the README rule "a key is always present" does not hold for these four); `max_fix_cycles`
  is then their sum, for display.
- The pure state machine stays pure; `split_limits_keep_the_transition_table` runs the exhaustive
  table again with split limits. Tests: `gate_retries_are_bounded_on_their_own` (AC-1),
  `review_rounds_are_bounded_on_their_own` (AC-2), `an_m10_machine_reads_back_and_behaves_the_same`
  (AC-3), `tests/m11_roles.rs::split_limits_end_to_end` (escalation, `status`, `--json`, the class in
  `events.jsonl`, `--max-fix-cycles` refused, a single-bound task without the new keys).

## E — Gates

### E1 — check gates: mapping, gate variables, M18

- **Mapping** (`team::gate_command`, B2): a check gate is a `CommandSpec` — `program`/`args` or
  `tool: xpp` + `metadata`/`projects`, `timeout_sec` from `timeout` — in `commands.verify`, so it
  runs exactly like an M10 verify command (clean checkout of the checkpoint, the configured
  boundary, no secrets, `@verify-scripts` protection, the same `VERIFY_RESULT.json` entry).
- **Gate variables** (`verify::set_gate_vars`, called for every verify run, legacy commands too —
  new variables only): `TORYO_TASK`, `TORYO_SHA` (the checkpoint commit), `TORYO_VERIFY_CHECKOUT`
  (the clean checkout), next to `TORYO_VERIFY_BUILD`. `toryo xpp build` (outside a task) does not
  set them.
- **M18 (applies to everyone):** `checks::verify_files` protects a repository-relative program and
  the script after `-File` (PowerShell's `-File`/`-f`/`-fi`/`-fil`, case-insensitive) whether or
  not they exist yet. **Existing test changed, as M18 requires:** `verify_files_include_a_repo_program`
  (EC-07) now lists `scripts/missing.sh` (a program that does not exist yet) — before, it asserted
  it was not protected. **Admission:** `task create` refuses a task whose base commit lacks one of
  those entry points (`checks::verify_entry_points`), naming the gate (`verify command` in
  legacy mode): `verify_script_missing: gate `later` runs `scripts/later.ps1`, which is not in the
  base commit … — commit it before creating the task`. It runs before the worktree, the branch or
  the run store exists. Other arguments that name files are protected only when they exist, as
  before.
- Tests: `checks::tests::m18_entry_points_are_protected_before_they_exist`,
  `tests/m11_gates.rs` — `a_check_gate_gets_the_gate_variables` (AC-1: the script prints the four
  variables; `VERIFY_RESULT.json` shows the task id, `head`, `verify_checkout` and its `build`
  folder), `a_gate_script_missing_from_the_base_commit_is_refused_at_admission` (AC-2). The
  protected-path violation for an edit of an existing gate script is the M10 EC-07 path
  (`regress_policy.rs`), unchanged.

### E2 — verify output is redacted with the task's secrets (review M2)

- `runner::RunSpec` gains `redact: Redactions`; `LocalRunner` spawns with it instead of
  `Redactions::empty()`. The engine's verify runs (`VerifyAt`) use `flight::task_redactions(cfg, t)`
  — every secret of the task's profiles, plus `CLAUDE_CODE_OAUTH_TOKEN` from the parent environment;
  E3 adds an environment gate's own `secret_env` values. So `VERIFY_RESULT.json`, the fix prompt,
  the reviewer's pack and anything derived from the output only ever see `[REDACTED:<name>]`.
- The output is collected whole (the stream redactor flushes its held-back tail at EOF), so the
  H5 newline pitfall does not apply here: verify output is not parsed line by line while the
  command runs. Output without a secret is byte-identical (an empty set is the identity; a set
  whose values do not occur changes nothing — tested).
- **The evidence run** (`evidence::run_version`, the R51 `--version` probe of `doctor` and the run
  gate) takes a redaction set too: the profile's own resolved secrets
  (`evidence::profile_redactions`; one that cannot be resolved is left out). Its environment holds
  no secrets, so this is belt and braces.
- `toryo xpp build` (outside a task) and `spike pipeline` keep an empty set (no task, no secrets).
- Not done (not in the task list): scanning `DIFF.patch` for known secret values (review M2's last
  sentence).
- Tests (`tests/m11_gates.rs`): `verify_output_is_redacted_with_the_task_secrets` (AC-1: a verify
  command prints the implementer profile's secret; it fails once, so there is a fix prompt, then
  passes, so there is a reviewer pack; every file of the run store and stderr are free of it, the
  tail shows `[REDACTED:TORYO_M11_TOKEN]`), `output_without_secrets_is_unchanged_by_redaction`
  (AC-2).

### E3 — environment gates: run, lock, record what is deployed

- **When** (`runner::run_environment_gates`): inside `Verifying`, after **every** check gate of the
  same run passed, in the implementer's `before_handoff` order, stopping at the first failure. A
  failing check gate means no environment gate runs.
- **How**: the **frozen** machine command (`team.gates.<name>.machine`, B2 — the command that runs
  with full rights is the one the task was created with), `Boundary::None` (the owner's normal
  token; the check gates' boundary is unchanged), working directory = the verify checkout, the
  Job Object and timeout of `LocalRunner` (`timeout:` of the gate), environment from scratch (R23,
  `process::env::build_env`) + the gate variables (E1) + `env` + resolved `secret_env`
  (`config::resolve_secret`). The output is redacted with the task's secrets **plus** the gate's
  own (`flight::task_secrets` is new, `task_redactions` builds on it). **The program is resolved
  before it starts** (`env_gate_program`): an absolute path as written, a bare name through PATH —
  never looked up in the verify checkout. A secret that cannot be resolved, or a program not on
  PATH, escalates with the class `environment_gate_unavailable` (not something a fix cycle can
  repair). A failure goes back to the implementer as untrusted data (R61, the same
  `untrusted::wrap("verify", …)` as check gates) and counts against `max_gate_retries` (C4).
- **Lock** (`lock::EnvLock`): `<data_dir>\locks\env-<lock>.lock`, an exclusive OS file lock like
  `TaskLock`, the holder's label written inside, released explicitly on drop (review L9). While
  another task holds it: one `lock_waiting { lock, holder }` event, a row in the new table
  `environment_waits` so `status` shows `waiting for lock d365-env (held by T-12)`, a try about
  every 2 s (`TORYO_TEST_ENV_LOCK_POLL_MS` in tests), and `toryo stop` ends the wait at once (the
  verify attempt is `interrupted`, as for any stop). **Deviation:** Windows does not let another
  process read a locked file, so the holder is named from `environment_deployments` (the row with
  no `finished_at` for that lock), not from the lock file. Lock names are letters, digits, `-`,
  `_`, `.` (B3 refuses others).
- **Record** (new table `environment_deployments(lock PRIMARY KEY, task, sha, gate, machine,
  started_at, finished_at, ok)` — `gate` and `machine` added to the listed columns): written
  **before** the gate starts, updated at its end (`ok` empty for a stopped gate). Never deleted
  (cancel, `task cleanup`). Shown in `toryo status` (`environment    d365-env holds BR-001 @
  1a2b3c4d5e6f (not approved yet)` — all locks; `status ID` / `task show ID`: the locks that task
  deployed to), in both `--json` documents (`environments[]`, **only when there is a row**, so an
  M10 document is unchanged) and in the final gate notes. Status words (`views::deployment_status`):
  `deploying now` (the holder's run is alive), `interrupted — the environment may hold a partial
  deploy`, `the deploy failed`, `approved` (the holder is `Completed`), `not approved — the task
  was cancelled`, `not approved yet`.
- **Result**: one `VERIFY_RESULT.json` entry per environment gate: `name`, `kind: environment`,
  `lock`, `machine` (`COMPUTERNAME`, else `HOSTNAME`), `argv` (the frozen program and args — the
  secrets are environment variables, never in argv), `exit`, `timed_out`, `ms`, `output_tail`.
  `if_missing: skip` and not defined here: `{name, kind: environment, skipped: true, reason: "not
  defined on this machine"}` and the final-gate note `dbsync did not run on this machine …`. The
  R47 mutation check runs after all gates (`verify_mutated_tree` when a gate changed a tracked file
  of the checkout).
- **Final gate wording**: with an X++ compile and environment gates that passed, `verify: compile +
  dbsync on <machine>` replaces `COMPILE_ONLY`; without X++, `verify: dbsync on <machine>
  (environment gate)`; `COMPILE_ONLY` stays when none ran. Plus `environment: <the deployment
  line>` for each lock this task deployed to.
- **Events**: `lock_waiting`, `environment_deployed { lock, sha, ok }`; `verify_command` gets an
  optional `kind` serialised only for environment gates (M10 event shapes unchanged — tested). The
  flight tests' exhaustive variant list is extended (14 variants).
- **Recovery** needs nothing special: a killed toryo leaves the attempt `running` and the row
  without `finished_at`; `resume` re-runs `Verifying` from the check gates. Crash test: the real
  kill of a `toryo run` during the gate (Windows: the Job Object kills the script with it).
- The script contract (owner draft `toryo-local.yaml`) is documented in F1/F2. Added to it:
  name scripts by absolute path (a bare-name argument is resolved by the program against its
  working directory, the verify checkout).
- Tests (`tests/m11_env_gates.rs`, scripts `pwsh` on Windows / `sh` elsewhere in `<root>/gates/`):
  `an_environment_gate_runs_after_the_checks_and_its_exit_code_decides` (AC-1, AC-4 with the Claude
  implementer's `<untrusted_data>` block and `gate_retries`, AC-6 in every view, after `task
  cleanup`), `cancelling_keeps_the_deployment_record` (AC-6), `an_environment_gate_has_the_normal_
  token_a_check_gate_low_integrity` (AC-2, `#[cfg(windows)]`), `two_tasks_share_one_environment_lock`
  (AC-3, with `toryo stop` while waiting and a later `resume`), `an_environment_gate_secret_is_
  redacted_everywhere` (AC-5), `a_skipped_environment_gate_is_recorded_and_noted` (AC-7; the
  `error` case is B3's), `an_environment_gate_that_writes_into_the_checkout_mutates_the_tree`
  (AC-8), `a_crash_during_the_environment_gate_is_recovered_by_resume` (AC-9);
  `views::tests::deployment_status_in_words`, the extended flight tests.
- Also in this commit: the group C review nit (a doc comment had moved off `ImplementTurn`).

### Group E review (independent subagent) — follow-up commit

- **Fixed (M18):** the entry point of a verify command is now its **script**, per program
  (`checks::script_arg`): the argument after the first `-File` of `pwsh`/`powershell` only (a
  script's own `-f x` after it is not), the first non-flag argument of an interpreter (`sh`,
  `bash`, `zsh`, `dash`, `ksh`, `python`, `python3`, `py`, `node`, `ruby`, `perl`; none when `-c`,
  `-m`, `-e`… runs inline code or a module), the file after `cmd /c`. Before, `-f` counted for
  every program (`dotnet test -f net8.0` would have been refused) and `sh scripts/x.sh` was not
  protected before it existed. The admission check also runs at **`task config --reload`**
  (`task::check_entry_points`, shared).
- **Fixed (E3):** an environment gate's environment gets `NoDefaultCurrentDirectoryInExePath=1`, so
  cmd.exe and `CreateProcess` in the owner's script never run a bare-named program from the verify
  checkout. **Not fixable here (script contract):** DLL loading still searches the working
  directory — the contract says to call tools by absolute path.
- **Fixed:** environment lock files live in `locks/env/<lock>.lock` (lower case), so a task id like
  `env-d365-env` can never take a gate's lock; a gate program that does not exist (or a bare name
  not on PATH) escalates with `environment_gate_unavailable` before any record is written; a gate
  that fails to start marks its record `ok: false` instead of "interrupted"; `which` results are
  made absolute; `status` shows "waiting for lock" only while the task's run is alive (a crash while
  waiting leaves the `environment_waits` row behind).
- **Fixed (E2 AC-2):** `LocalRunner` collects the raw streams and redacts the whole output at the
  end — a per-stream redactor held bytes back and could move a short stdout line after later
  stderr output whenever any secret was configured. Live chunks (no engine caller) are redacted
  chunk by chunk.
- **Fixed:** `task config --reload` treats a change of the `team` section (gates, their machine
  commands, the role files) as a verify-configuration change (the verify result is invalidated).
- **Known limit (not fixed, Unix only):** on Unix a killed toryo leaves the gate's process group
  running while the lock is free (review M7: the process group is not a containment boundary);
  Windows kills the Job with toryo.
- Lock names that differ only in case share one lock file but get separate `environment_deployments`
  rows.
- Tests added: `checks::tests::m18_script_arguments_per_program`, `lock::tests::environment_locks_
  have_their_own_folder`, `runner::tests::the_verify_scope_at_the_final_gate`,
  `m11_env_gates::{an_environment_gate_that_cannot_run_escalates,
  a_stop_during_an_environment_gate_interrupts_it}`,
  `m11_gates::a_configured_secret_does_not_reorder_stdout_and_stderr`.

### Group C review (independent subagent)

- Nothing serious. Fixed: the misplaced doc comment. Recorded, not changed: after a
  `same_vendor_review` escalation `resume --note` costs one more implementer turn before the
  review is refused again (see C3); fence violations found by crash recovery are worded "protected
  paths" and escalate (as M10 does for protected paths); ignored files outside `writes` are not seen
  (never committed; same as `deny_paths` in M10); after `resume --note` with split limits the
  escalation text counts the limit, not the cycles run (`2 gate retries used` after 3 cycles), and
  says `1 review rounds`.

## D — Skills

### D1 — skill lookup and the menu

- `team::resolve_skill(name, repo, machine_dir)`: first match wins —
  `<project.root>/.toryo/skills/<name>/SKILL.md`, then `<skills_dir>/<name>/SKILL.md` (the frozen
  `team.skill_roots.machine`, `%VAR%` expanded). Not found: a message naming **both** paths (or
  "no `skills_dir` in the machine config"). Each resolved skill is `{ name, origin: repo | machine,
  path, sha256, bytes }` plus its `description` and text (CRLF folded to LF, like the other context
  sources). A name must be a folder name — no `/`, `\`, `:`, `..` (a path in a name could otherwise
  escape both folders).
- **Front matter** (`team::front_matter` / `skill_description`): the YAML mapping between the first
  two `---` lines; a UTF-8 BOM and CRLF are accepted; no opening `---` on the first line, an
  unclosed block or a non-mapping is "no front matter".
- **Validation (B3 pass):** every skill of every role file resolves (with a "did you mean" over the
  skill folders found in both places); a skill on the **implementer's** menu needs a
  `description` (the planner chooses by it). Planner and reviewer skills need none.
- **Only `SKILL.md` is delivered** (D3). Files next to it (scripts, examples) are readable by the
  agents **only for repository skills** — they are in the worktree; a machine skill's other files
  are outside every worktree (and Codex's sandbox). README says so (F2).
- The draft fixture gets the two skills of the draft role file: `xpp-axclass` in the repository,
  `xpp-batch` in the machine folder.
- Tests: `team::tests::skills_resolve_repository_first` (AC-1), `skill_front_matter` (AC-2),
  `skills_are_validated_at_load`.

### D2 — the plan's `skills` field and its checks

- `schemas/plan.schema.json`: optional `"skills": { "type": "array", "items": { "type": "string" } }`
  (not in `required`; inside `contracts::KEYWORDS`). **Applies to every plan schema the agents get**,
  also in legacy mode — an optional property; plans without it stay valid and render as before.
- **Menu** (`context::skill_menu`): when the implementer's role has skills, the **planner's**
  context gets a never-trimmed part `implementer skill menu` (heading `Skills available to the
  implementer`): an instruction to list the needed ones in `skills`, then one `- name — description`
  line per skill, read per attempt (a skill gone since `task create` shows `(not found on this
  machine)`). None in legacy mode or with an empty menu.
- **Checks after a valid plan** (`Engine::plan_skills_problem`, after the path/fence check): every
  name must be on the implementer's menu — else the bounded automatic re-plan with feedback listing
  the menu and a "did you mean" — and the chosen SKILL.md files plus the implementer's never-trimmed
  parts (`context::Assembled.fixed_bytes`, new: policy, culture, role template, instructions, menu,
  skills) must fit `context.budget_kb.implementer` — else "Choose fewer skills". 
- **Event:** `Ev::PlanPolicyViolation` gains `reason: Option<String>` (serialised only when set, so
  the stored M10 shape is unchanged); `paths` is empty for a skills problem; at the bound the
  escalation says `plan_skills: … — still, after 2 automatic re-plans` (class `plan_policy`, as for
  paths). The exhaustive state-machine tests construct it with `reason: None` (extended, not
  weakened).
- `PLAN.md` shows `Skills for the implementer: a, b` under "Files likely involved" when the plan
  has skills (the plan digest already covers `plan.json`).
- Tests: `render::tests::a_plan_names_the_implementers_skills` (rendering and schema, AC-1/AC-4),
  `tests/m11_skills.rs` — `a_plan_chooses_skills_from_the_menu` (AC-1, the menu in the planner's
  context), `a_skill_off_the_menu_is_replanned_then_escalates` (AC-2), `skills_over_the_budget_are_
  replanned` (AC-3), `a_plan_without_skills_is_valid` (AC-4).

### D3 — skills delivered through the context loader

- `context::TeamContext.skills` (resolved by `context::request` **per attempt**, from the repository
  checkout and the machine folder): one part per skill right after the role part (C1) and the
  menu (D2) — label `skill <name> (<origin>)`, heading `Skill: <name> (<origin>)`, the SKILL.md text,
  **never trimmed**, hashed (into `context_sha256`, and each source's own `sha256` in the
  `context_assembled` event). `context show` prints each skill line with its `sha256` (only skill
  lines get the suffix — legacy output is unchanged).
- Which skills: the implementer (implement and every fix stage) → the **approved plan's** `skills`
  (the head of `plan`, filtered to the menu); planner and reviewer → every skill of their role file.
  A skill that no longer resolves at attempt time is reported under `skipped`, not an error.
- Skills are trusted content (protected `.toryo/` of the repository checkout, or the machine): for
  the Claude implementer they are in the context file (its system prompt), never in the
  `<untrusted_data>` user prompt (tested).
- **Over budget at attempt time** (found out, kept): exactly what the loader does today when the
  policy and the role template alone exceed the budget — the trimmable sources are trimmed first,
  then a warning `context budget: N bytes still exceed K KB — policy files and the role template are
  never trimmed` (wording unchanged; it now also covers culture, instructions, menu and skills), and
  the attempt runs with the full context. The plan-time check (D2) is what keeps a plan's choice
  inside the budget; it now measures the implementer's context **with** the chosen skills
  (`fixed_bytes`).
- The planner's menu label is `implementer skill menu` (so only skill parts start with `skill `).
- Tests (`tests/m11_skills.rs`): `context_show_lists_the_skills_of_each_role` (AC-1: the planned
  skill only, with origin and SHA-256; the reviewer's own; none in legacy mode),
  `a_changed_skill_changes_the_context_hash` (AC-2, through the `context show` preview — exactly
  what the next attempt gets), `the_claude_implementer_gets_skills_in_its_context_file` (AC-3, and
  the reviewer's skill reached the reviewer).

### Group D review (independent subagent) — follow-up commit

- **Fixed:** the plan check trimmed skill names but the delivery compared them exactly, so
  `" xpp-batch"` passed the check and was then silently dropped (also skipping the budget check).
  One shared `context::plan_skills` (names exactly as written, deduplicated) is used by both; a name
  with spaces is simply off the menu.
- **Fixed:** in `toryo.yaml`-only mode a plan's `skills` is removed before `plan.json` / `PLAN.md` are
  written — it means nothing there (a planner might list its native skills), so the plan looks as in
  M10.
- **Fixed:** an approved skill that is no longer on the implementer's menu at attempt time (e.g. a
  `task config --reload` at the plan gate changed the menu) is reported under `skipped` instead of
  disappearing silently. A lower budget after the plan was approved only gives the usual over-budget
  warning (not re-checked).
- **Fixed:** planner-written skill names are escaped in the stderr line, the escalation reason and
  the feedback.
- Not changed (recorded): `fixed_bytes` leaves out the render scaffolding (the preamble, the task-pack
  listing), so a plan just under the budget can still exceed it slightly at attempt time — a warning
  only. Skill files are read through symlinks/junctions without a containment check: both roots
  (the repository's protected `.toryo/` and the machine's folder) are owner-controlled.
- Tests added: `m11_skills::{a_skill_name_is_matched_exactly, legacy_plans_ignore_skills,
  machine_skills_are_delivered_and_missing_ones_skipped}` (origin `machine`, `skipped`, the menu's
  "(not found on this machine)").

## F — Wrap-up

### F1 — templates

- `templates/d365fo/.toryo/toryo-orgchart.yaml` and `…/roles/implementer/toryo-role.yaml`: the
  owner's drafts **verbatim** (all comments kept; the drafts have no personal paths), each with a
  `TODO(owner)` header in front. `…/roles/{planner,reviewer}/toryo-role.yaml` (purpose,
  `instructions`, empty `skills`), `…/roles/*/instructions.md` (short, `TODO(owner)` — the D365 notes
  per role stay in `.toryo/context/50-xpp-roles.md`), `templates/d365fo/CULTURE.md` (the draft names
  `culture: ./CULTURE.md`), and stubs of the draft implementer's two skills in
  `.toryo/skills/{xpp-axclass,xpp-batch}/SKILL.md` (with a `description`, as the menu needs) — added
  so the set loads as shipped; the draft also allows them in the machine's `skills_dir`.
- **A second example** `templates/d365fo/toryo.orgchart.yaml.example` is the `toryo.yaml` of org
  chart mode (no `roles:`, `commands:` or `pipeline.max_fix_cycles`); its header says the pair loads
  together and that `toryo.yaml.example` (the M9/M10 layout, unchanged, same hash) does not load next
  to an org chart.
- The draft's `build` gate (`pwsh -File scripts/build-model.ps1`) is kept; the template's
  `TODO(owner)` says the script must be committed before a task (M18) or to switch to the
  `tool: xpp` alternative the draft shows.
- `templates/toryo.local.yaml.example`: commented `seats:`, `skills_dir:`, `environment_gates:` with
  the six-point script contract of the owner's draft (plus: call tools by absolute path — the
  working directory is the verify checkout) and `verify:` (A2). `templates/d365fo/
  toryo.local.yaml.example`: a short commented pointer. Comments only: both keep their hash.
- Test: `team::tests::the_d365fo_org_chart_templates_load` (the set loads with fake profiles and a
  machine `environment_gates.dbsync`; every new file starts with its `TODO(owner)` items; the old
  example next to the org chart is refused, one place per setting). The existing template tests
  pass unchanged (AC-1).

### F2 — docs, migration guide, morning summary

- README: "Org chart mode (M11)" (the `.toryo/` layout, the two modes, seats and profiles, limits, checks,
  instructions and culture, skills — lookup order, the planner's choice, only `SKILL.md` delivered —, the write
  fence, check vs environment gates and the script contract, the deployment record, the machine `verify:` floor),
  "Migrating an M10 `toryo.yaml` to `.toryo/`" (eight steps), "The effective configuration (M11)" (`config show
  --json`, schema 1, from B2), the new event and `--json` keys, the new test files, an M11 status paragraph.
- `docs/HUONG-DAN.md` §5.9 (Vietnamese): the workshop picture, the layout, what to remember, the two gate kinds,
  a worked D365FO example (a `tool: xpp` build check gate + a `dbsync` environment gate, the role file, the machine
  config), the script contract, the M13 note for the Tokyo machine, the migration in short.
- This summary, at the top.

## Open

The list is in the morning summary at the top ("Open"); each item is explained in its task's section.

## Not started / out of scope

- **M13** (cp932 output on Japanese Windows) will also garble DB-sync output on the owner's Tokyo
  machine — the next thing to fix after M11.

