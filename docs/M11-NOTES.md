# M11 notes — decisions and deviations while building M11 (`M11_task.md`)

Every entry: what was decided, why, and where. The owner folds these into plan v0.9.

## Progress

Branch `m11/orgchart` (from `main` @ `aa0be03`). One commit per task.

- [x] A1 — repository files parsed without machine-only keys; tagged keys refused (C1)
- [x] A2 — machine floor for `verify.boundary` and `verify_root` (H2)
- [x] B1 — types for the org chart, role files and the new machine keys
- [x] B2 — loader: discover `.toryo/`, merge, freeze and hash; `toryo config show`
- [x] B3 — validation pass: cross-references, kinds, order, "did you mean"
- [x] C1 — layered instructions and `culture` in the context loader
- [x] C2 — the implementer's write fence (`writes` / `never`)
- [ ] C3 — `checks.reviewer-differs-from-builder`: off / warn / error
- [ ] C4 — split limits: `max_gate_retries`, `max_review_rounds`
- [ ] E1 — check gates: mapping, gate variables, M18
- [ ] E2 — verify output redacted with the task's secrets (M2)
- [ ] E3 — environment gates: run, lock, record what is deployed
- [ ] D1 — skill lookup (repo, then machine) and the menu
- [ ] D2 — the plan's `skills` field and its checks
- [ ] D3 — skills delivered through the context loader
- [ ] F1 — templates
- [ ] F2 — docs, migration guide, morning summary

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

## Open

## Not started / out of scope

- **M13** (cp932 output on Japanese Windows) will also garble DB-sync output on the owner's Tokyo
  machine — the next thing to fix after M11.
