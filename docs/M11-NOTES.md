# M11 notes — decisions and deviations while building M11 (`M11_task.md`)

Every entry: what was decided, why, and where. The owner folds these into plan v0.9.

## Progress

Branch `m11/orgchart` (from `main` @ `aa0be03`). One commit per task.

- [x] A1 — repository files parsed without machine-only keys; tagged keys refused (C1)
- [ ] A2 — machine floor for `verify.boundary` and `verify_root` (H2)
- [ ] B1 — types for the org chart, role files and the new machine keys
- [ ] B2 — loader: discover `.toryo/`, merge, freeze and hash; `toryo config show`
- [ ] B3 — validation pass: cross-references, kinds, order, "did you mean"
- [ ] C1 — layered instructions and `culture` in the context loader
- [ ] C2 — the implementer's write fence (`writes` / `never`)
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

## Open

## Not started / out of scope

- **M13** (cp932 output on Japanese Windows) will also garble DB-sync output on the owner's Tokyo
  machine — the next thing to fix after M11.
