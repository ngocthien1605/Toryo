# thien — M0 spike kit

Rust implementation of **milestone M0** of the Thien Orchestrator plan (v0.6, §17): the pieces needed
to answer the twelve `M0-VERIFY` questions on the real Windows machine, before any pipeline code
(M1–M8) is written — the plan's stop rule.

| Module | Plan | What it does |
|---|---|---|
| `process::redact` | R42 (C9) | streaming secret redaction with carry-over across chunks, UTF-8 safe |
| `process::env` | R23 | `env_clear()` + allowlist, provider-selector conflict rejection |
| `process::windows` | R23, R36 | per-stage Job Object (suspended create → assign → resume), `ActiveProcesses` drain, Low-integrity `CreateProcessAsUserW`, Low labels |
| `process::unix` | — | process-group stand-in so the kit and its tests also run on Linux |
| `agents::claude` | R16, R17, R30, R44b | exact launch line (+ the two fallbacks), stream-json parser, provisional usage |
| `agents::codex` | R18, R19, R21, R22 | canonical `-c` argv (identical on resume), JSONL parser, message classifier |
| `usage` | R21, R41 | disjoint token buckets, cumulative deltas with reset |
| `git` | R33, R37 | `GitCmd` hardening prefix, `<CANON_DIFF>`, trusted/neutralised filter plan, canaries, pkt-line filter |
| `config` | §14, R35 | `thien.yaml` + machine config, canonical hash, Credential Manager secrets |
| `doctor` | §15 | environment / profile / repo checks + recorded M0 evidence |
| `spike::*` | §17 | the M0 commands (see `docs/M0-RUNBOOK.md`) |

```
thien doctor
thien spike job-test | git-canary | lowil-canary
thien spike claude-isolation --profile claude-max [--mode restricted|setting-sources|cwd-run-store]
thien spike codex-canary --profile codex-foundry
thien spike codex-project-config --profile codex-foundry
thien spike killed-resume --profile <claude-max|codex-foundry>
thien spike claude|codex …        # single turns for the manual dry-run
thien spike init-fixture | render | diff | report
```

Start with **`docs/M0-RUNBOOK.md`**. Results go to `docs/M0-RESULTS.md`; findings from building the kit
are in `docs/M0-FINDINGS.md`.

## Build and test

```powershell
cargo build --release
cargo test --release
```

On Linux the tests run the whole spike harness against fake `claude`/`codex` scripts
(`tests/spike_e2e.rs`) and the Git canaries against the real `git` (`tests/git_canary.rs`). The
Windows-only paths (Job Object, Low integrity) compile on Linux via
`cargo check --target x86_64-pc-windows-gnu` but only *run* on Windows — `cargo test` and
`thien spike job-test` / `lowil-canary` there are part of M0.

## Status

M0 kit — not the orchestrator. No `thien task`, `run`, `approve` yet: they start in M1 once M0's
evidence is in plan v0.7.
