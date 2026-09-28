# toryo

> **Toryo** (棟梁, *tōryō*) is the master carpenter who leads a building crew: he does not cut the wood himself,
> he hands out the work, checks every joint and signs off each stage. That is this tool's job — Claude Code and
> Codex are the craftsmen; `toryo` plans the order of work, enforces the rules, checkpoints and verifies.
> (Renamed from `thien` on 2026-09-28, plan v0.7 R55.)

Rust implementation of the Toryo plan (**v0.7**). **M0 is done** (the spike kit and its evidence);
**M1** adds the vendor-neutral core the engine (M2–M8) will build on:

| M1 module | Plan | What it does |
|---|---|---|
| `domain` | §5.1, §6.1 | `Role`, `AgentEvent`, `OpenSession`/`TurnInput`/`TurnLimits`, `StageOutcome`, `TokenUsage`, `UsageCapabilities` |
| `agents::adapter` | §6.1 | `AgentAdapter` / `AgentSession` traits; `run_to_completion` (stall/max limit → interrupt → `Timeout`) |
| `agents::cli` | §6.2, §6.3, R48, R49 | Claude one-shot and Codex exec adapters on the kit's launch lines; live events per stream line; usage capabilities (Claude per-run, Codex cumulative) |
| `agents::fake` | §6.4 | `FakeAgentAdapter`: scripted turns per `role` / `role@profile` (text, tools, file writes, usage, denials, failures, hang, crash) and replay of recorded real streams (`fixtures/captured/`) through the real parsers |
| `agents::hub` | — | sync facade (owns the tokio runtime) that picks the real or fake adapter per profile |
| `runner` | §8 | `CommandRunner` / `LocalRunner` for verify commands (ANSI stripped) |
| `evidence` | R51 | evidence records carry `cli_versions`; a profile whose CLI version differs from its last canary pass is *unverified* |
| `engine::state` (M2) | §5.2, R5, R29, R32, R40, R44a, R56 | the pipeline state machine: one `transition(state, event)`, gate occurrences, fix-cycle bound, `Escalated`/`Paused` as waiting states; serializable for M3 |
| `engine::store` (M3) | §5.3, R24 | SQLite (WAL) store actor: every state change and its event in one transaction; tasks, stage attempts, gate occurrences, usage, control requests |
| `engine::lock` (M3) | R24, R38 | per-task OS file lock (lock-first), lock generation, `toryo stop` protocol |
| `engine::runner` (M3) | §5.2, R48, R49 | the engine: stages as persisted attempts, gates as approvals, fallback chain, stop polling |
| `engine::recovery`, `engine::snapshot` (M3) | §10, R31, R39, R40 | crash recovery: trailer-matched checkpoints, recovery snapshots + completion check, partial checkpoints, `--discard-partial` / `--keep-partial`, `partial export` |

M0 kit modules (ported to the core where M1 needs them, R52):

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
| `config` | §14, R35 | `toryo.yaml` + machine config, canonical hash, Credential Manager secrets |
| `doctor` | §15, R51 | environment / profile / repo checks + version-bound evidence; `--canaries` re-runs them |
| `spike::*` | §17 | the M0 commands (see `docs/M0-RUNBOOK.md`) — cargo feature `m0-kit`, on by default |
| `spike::pipeline` | §5.2, R28, R40 | prototype workflow on a real repo: plan gate, guards, checkpoints, verify, exact-AC review, fix cycles |

```
toryo task create --file TASK.md [--id ID] [--repo R] [--base B]   # snapshot + frozen config + worktree toryo/<ID>
toryo run ID                                   # 🔒 runs until a gate / escalation / stop / completion
toryo approve ID --gate plan|final [--expect DIGEST] [--continue]   # 🔒
toryo reject  ID --gate plan|final --reason "…"                     # 🔒
toryo resume [ID] [--note "…" | --discard-partial | --keep-partial]  # 🔒 recover after a crash/stop, continue
toryo stop ID                                  # ask the running owner to stop (R38)
toryo status [ID] · toryo task list|show ID|cleanup ID · toryo partial export ID --attempt N --into DIR
toryo doctor [--canaries [--profile P …] [--claude-mode restricted]]   # R51: re-verify after a CLI upgrade
toryo spike job-test | git-canary | lowil-canary
toryo spike claude-isolation --profile claude-max [--mode restricted|setting-sources|cwd-run-store]
toryo spike codex-canary --profile codex-foundry
toryo spike codex-project-config --profile codex-foundry
toryo spike killed-resume --profile <claude-max|codex-foundry>
toryo spike smoke                 # end-to-end: plan → implement → checkpoint → verify → review
toryo spike pipeline --repo R --task T.md   # one real task: plan → HUMAN GATE → implement → verify → review → fix cycles
                                            # (implementer out of quota → fallback, e.g. codex-openai → claude-max;
                                            #  the reviewer stays roles.reviewer; unverified profiles refused
                                            #  unless --accept-unverified)
toryo spike claude|codex …        # single turns for the manual dry-run
toryo spike init-fixture | render | diff | report
```

**Quick start on Windows (no Rust needed):** `bin\toryo.exe` is a prebuilt binary (cross-compiled,
x86_64-pc-windows-gnu, stripped). One command installs Claude Code if missing, checks the Claude/Codex
logins, writes `C:\.thien\m0\toryo.yaml`, and runs `doctor`, `job-test`, `git-canary` and the
end-to-end `smoke` (Claude plans → Codex implements → toryo checkpoints + verifies → Claude reviews):

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File C:\.thien\AgentOrchestrator\toryo\scripts\m0-smoke.ps1
```

Results land in `C:\.thien\AgentOrchestrator\thien-m0-data\`.

**Hướng dẫn tiếng Việt (flow, cài đặt, cấu hình, thực hành với app mẫu BatchRename): `docs/HUONG-DAN.md`.**
`spike pipeline` is a single-process prototype of M2–M8 (no SQLite state/resume, no Low-IL verify boundary yet).

Start with **`docs/M0-RUNBOOK.md`**. Results go to `docs/M0-RESULTS.md`; findings from building the kit
are in `docs/M0-FINDINGS.md`.

## Build and test

```powershell
cargo build --release
cargo test --release
```

| Test | Runs on | What it covers |
|---|---|---|
| `tests/pipeline_fake.rs` | Windows + Linux | the pipeline scenarios on `FakeAgentAdapter`, with the state trail checked: gate reject → planner resumed, fix cycle, final-gate rejection, max cycles → `Escalated`, path-policy cycle, bounded automatic plan rejections, unlimited human ones, blocked review, sticky fallback with the configured reviewer kept, rate-limit retry, verify failure → fix, review correction, all implementers out of quota, quit at the gate |
| `tests/adapters.rs` | Windows + Linux | recorded real Claude/Codex streams through the adapters; fake failure classes, stall timeout, crash |
| `tests/spike_e2e.rs` | Linux | the kit against fake `claude`/`codex` sh scripts, incl. `version_bound_evidence` (R51) |
| `tests/engine_e2e.rs` | Windows + Linux | M3: gates persisted (occurrences, `--expect`), a crash at every stage → `resume` → clean tree, trailer-matched normal / partial / no-change checkpoints, R48 resume policy (Claude resumes, Codex fresh + hand-over), policy-violating partial → `--discard-partial` / `--keep-partial`, `partial_snapshot_roundtrip`, `lock_first_recovery` |
| `tests/redaction.rs` | Windows + Linux | `redaction_split_boundaries` (C9/R42) |
| `tests/architecture.rs` | Windows + Linux | no vendor types outside the adapters |
| `tests/git_canary.rs` | Windows + Linux | Git hook/filter/textconv canaries on the real `git` |

CI (`.github/workflows/ci.yml`) runs `cargo test`, `clippy -D warnings` and the core without the kit
(`--no-default-features`) on `windows-latest` and `ubuntu-latest`, then builds a release `toryo.exe`.

## Status

M0, M1 (adapters), M2 (state machine) and M3 (SQLite store, task lock, crash recovery) are done. The
engine commands (`task create`, `run`, `approve`, `reject`, `resume`, `stop`, `status`) run a task with its
state in `%LOCALAPPDATA%\toryo\toryo.db`; a killed `toryo` loses nothing — `toryo resume` continues from
the last committed step. Still to come: ProjectContextLoader (M4), the protected-state snapshot (M5),
immutable run-store artifacts and the Low-IL verify boundary in the engine (M6), budget admission (M7).
`spike pipeline` stays the behavioural reference (R52).
