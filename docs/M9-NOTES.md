# M9 notes — decisions and deviations while building M9 (`M9_task.md`)

Every entry: what was decided, why, and where. The owner folds these into plan v0.9.

## A — Trust boundaries (R60, R61)

### A1 — profiles only in the machine config (R60)

- **Checked on the raw text.** `Effective::load` parses the repository file to a YAML value and
  refuses a top-level `profiles` or `toolchains` key before anything is merged
  (`config::machine_only_part`, `MACHINE_ONLY_KEYS`). A key of that name inside another block does not
  count. The error names the file, the machine config to move the block to (the `--machine-config`
  path, else `%USERPROFILE%\.toryo\toryo.local.yaml`), and prints the block as YAML. Nothing is rewritten.
- **`RepoConfig.profiles` stays** as the merged view: `Effective` is frozen into `config.json` with the
  profiles in it, and `from_frozen` must load tasks created before R60. The repository file itself can
  no longer fill it. Moving the same profiles from the repository file to the machine config gives the
  same canonical JSON and hash (test `profiles_from_the_machine_config_hash_like_the_old_layout`).
- **`toolchains` is refused already in A1**, before B1 teaches the machine config to read it. Between
  the two commits a `toolchains:` block loads nowhere; nothing used it yet.
- **Unknown profile message** now says where to define it ("define it under `profiles:` in the machine
  config (…) (R60)").
- **`toryo spike init-config`** writes two files: `--out` (repository part: project + roles) and
  `--machine-out` (default `toryo.local.yaml` next to `--out`: the profiles). It refuses to overwrite an
  existing machine-part file and then writes nothing.
- **`scripts/m0-smoke.ps1`** writes its profiles to a machine config of its own,
  `C:\.thien\m0\toryo.local.yaml` (an earlier one is moved aside with a timestamp, never deleted), and
  passes `--machine-config` to every call. It never touches `%USERPROFILE%\.toryo\`. Consequence: the
  other scripts (`m3-stop-test`, `m4-m7-check`, `m8-check`) use the default machine config — after a
  fresh `m0-smoke` run, merge `C:\.thien\m0\toryo.local.yaml` into `%USERPROFILE%\.toryo\toryo.local.yaml`
  (owner checklist E1).
- **Tests.** The harness (`tests/common`, `pipeline_fake`, `spike_e2e`, `regress_agents`, `regress_cli`)
  writes the fixture profiles into a machine config and passes it with `--machine-config` (it used to
  pass a missing `none.yaml`). `common::MACHINE_PROFILES` holds the fixture's profiles; tests that add a
  profile edit `env.machine`, tests that change roles or commands still edit `env.config`.

### A2 — `doctor` shows where each profile and role came from (R60)

- `Effective.origins` (`#[serde(skip)]`, so not in the canonical JSON or the hash) records, while
  merging, `repo`, `machine` or `repo_overridden_by_machine` for every role and `machine` for every
  profile. A frozen configuration has no origins; `doctor` then says "frozen configuration".
- `doctor` prints one `info` line per profile and role (`doctor::origin_checks`); in `--json` those
  checks carry an extra `source` field. Other checks keep their JSON shape (the field is omitted).
- Toolchains get their origin line in B1, when the machine config can hold them.
