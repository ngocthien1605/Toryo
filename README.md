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
| `engine::context` (M4) | §7, R20, R30 | ProjectContextLoader: global.md → AGENTS.md/CLAUDE.md (Claude only) → `.toryo/context/*.md` → role template → task pack (R61: only TASK.md/PLAN.md inline; untrusted files as paths, or `<untrusted_data>` user-prompt blocks for the Claude implementer); 40 KB budget per role; stored per attempt with its hash; `context show` |
| `engine::protect` (M5) | R35 | protected state compared on the filesystem (incl. ignored files and Git metadata), versioned baselines, `--accept-protected-change`; frozen config + `task config --reload` |
| `engine::artifacts` (M6) | R34 | immutable artifact versions (staging → publish → one transaction), derived views, reconciliation (orphans, tampered, missing) |
| `engine::verify` (M6) | R29, R36, R45, R47, R50 | verify checkout of the checkpoint under `verify_root`, Low-IL boundary, explicit `--git-dir`, `verify_mutated_tree` |
| `engine::decisions` (M7) + usage in `engine::store` | R41, R53 | `DECISIONS.md` (gates, notes, follow-ups); admission budget, cumulative baselines, reservations, Claude live interrupt |
| `engine::xpp` (M9) | R59 | `tool: xpp`: the five CI/CD packages, the MSBuild argv of Microsoft's hosted X++ build, per-project results, MSBuild diagnostics, `scope: compile`; `toryo xpp build` |
| `engine::flight` (M10) | ARCH-02 | typed `EngineEvent`s, the recorder (envelope, redaction, sinks), the `events.jsonl` flight recorder, the stderr renderer, `watch`'s tail |
| `engine::views`, `engine::usage_report` (M10) | DES-10, §12 | the `--json` documents of `status` / `task show`; `toryo usage` and `prices.toml` |
| `notify` (M10) | — | the toast sink: which events notify, the fixed Windows PowerShell toast script |

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
| `config` | §14, R35, R59, R60 | `toryo.yaml` + machine config (the only place for profiles and toolchains, R60), verify kinds (`program` or `tool: xpp`), canonical hash, Credential Manager secrets |
| `doctor` | §15, R51 | environment / profile / repo checks + version-bound evidence; `--canaries` re-runs them |
| `spike::*` | §17 | the M0 commands (see `docs/M0-RUNBOOK.md`) — cargo feature `m0-kit`, on by default |
| `spike::pipeline` | §5.2, R28, R40 | prototype workflow on a real repo: plan gate, guards, checkpoints, verify, exact-AC review, fix cycles |

```
toryo task create --file TASK.md [--id ID] [--repo R] [--base B]   # snapshot + frozen config + worktree toryo/<ID>
toryo run ID [--notify | --no-notify]          # 🔒 runs until a gate / escalation / stop / completion
toryo approve ID --gate plan|final [--expect DIGEST] [--continue]   # 🔒
toryo reject  ID --gate plan|final --reason "…"                     # 🔒
toryo resume [ID] [--note "…" | --discard-partial | --keep-partial | --accept-protected-change P …]  # 🔒
toryo stop ID                                  # ask the running owner to stop (R38)
toryo watch ID [--json] [--from-start] [--no-follow]   # M10: follow events.jsonl (no lock)
toryo status [ID] [--json] · toryo task list [--json]|show ID [--json]|cleanup ID · toryo partial export ID --attempt N --into DIR
toryo task config ID [--reload [--expect DIGEST]]   # 🔒 R35: frozen config; reload only in a waiting state
toryo budget set ID --tokens N | --none             # 🔒 R41: admission cap, then `toryo resume`
toryo usage ID [--json] [--prices FILE]            # §12: tokens per stage × profile × model, est. cost
toryo notify-test [--dry-run]                      # M10: a sample Windows toast (hidden)
toryo context show ID --stage plan|implement|review [--attempt N] [--profile P]   # §7: what the agent gets
toryo spike claude-implementer-isolation --profile claude-max   # R49 / V07-VERIFY 4
toryo spike codex-temp-probe --profile codex-openai             # R46 / V07-VERIFY 2
toryo doctor [--canaries [--profile P …] [--claude-mode restricted]]   # R51: re-verify after a CLI upgrade
                                               # (R59) + X++: MSBuild (vswhere proposal), NuGets, UDE version, projects
toryo xpp build --repo R --project P [--project P …] [--metadata Metadata] [--out D] [--print-only] [--low]
                                               # R59/R72: the `tool: xpp` build outside a task
toryo spike job-test | git-canary | lowil-canary
toryo spike claude-isolation --profile claude-max [--mode restricted|setting-sources|cwd-run-store]
toryo spike codex-canary --profile codex-foundry
toryo spike codex-project-config --profile codex-foundry
toryo spike killed-resume --profile <claude-max|codex-foundry>
toryo spike smoke                 # end-to-end: plan → implement → checkpoint → verify → review
toryo spike pipeline --repo R --task T.md   # reference prototype (R52) — use `toryo run`
                                            # (implementer out of quota → fallback, e.g. codex-openai → claude-max;
                                            #  the reviewer stays roles.reviewer; unverified profiles refused
                                            #  unless --accept-unverified)
toryo spike claude|codex …        # single turns for the manual dry-run
toryo spike init-fixture | render | diff | report
```

**Quick start on Windows (no Rust needed):** `bin\toryo.exe` is a prebuilt binary (cross-compiled,
x86_64-pc-windows-gnu, stripped). One command installs Claude Code if missing, checks the Claude/Codex
logins, writes `C:\.thien\m0\toryo.yaml` plus the profiles in `C:\.thien\m0\toryo.local.yaml`, and runs `doctor`, `job-test`, `git-canary` and the
end-to-end `smoke` (Claude plans → Codex implements → toryo checkpoints + verifies → Claude reviews):

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File C:\.thien\AgentOrchestrator\toryo\scripts\m0-smoke.ps1
```

Results land in `C:\.thien\AgentOrchestrator\thien-m0-data\`.

**Configuration (R60).** Agent profiles — the executable, its environment and secrets, the Codex
sandbox settings — are defined **only** in the machine config (`%USERPROFILE%\.toryo\toryo.local.yaml`,
or `--machine-config`). A repository `toryo.yaml` that defines `profiles:` or `toolchains:` does not
load; the error prints the block to move. The repository file names profiles in `roles`. (R59) The X++
toolchain — MSBuild and the five CI/CD NuGet packages — is `toolchains.xpp` in the machine config; a
repository verify command names only what to build: `{ name, tool: xpp, metadata, projects, timeout_min }`
instead of `program`/`args`.

**D365FO starter kit (M9):** `templates/d365fo/` — the repository part (`toryo.yaml.example`: `tool: xpp`,
ZZZ deny rule, protected build projects), the machine part (`toryo.local.yaml.example`: the Foundry and Max
profiles, `toolchains.xpp`), `.toryo/context/` policy files (models, naming, labels, build, per-role X++
notes) and `TASK.example.md`. Each file lists its `TODO(owner)` items at the top; setup steps are in
`docs/HUONG-DAN.md` §7.3.

**Toryo on Toryo (M9):** `templates/dogfood/toryo.yaml.example` runs `scripts/verify.ps1` (what CI runs, with
`CARGO_HOME`/`CARGO_TARGET_DIR` in the Low-writable verify folders and `--locked`) in a `tracked` verify checkout.
Run a stable toryo installed outside the repository (`cargo install --path . --root C:\tools\toryo --locked`),
never add `.cargo` to Codex's `writable_roots`; details in `docs/HUONG-DAN.md` §7.4.

**Hướng dẫn tiếng Việt (flow, cài đặt, cấu hình, thực hành với app mẫu BatchRename): `docs/HUONG-DAN.md`.**
`spike pipeline` is the single-process reference prototype (R52); it prints a warning and gets no new features — use the engine (`toryo task create` + `toryo run`).

Start with **`docs/M0-RUNBOOK.md`**. Results go to `docs/M0-RESULTS.md`; findings from building the kit
are in `docs/M0-FINDINGS.md`.

## Following a task (M10)

**JSON output (DES-10).** `toryo status [ID] --json` (also `toryo task list --json`) and `toryo task show ID --json`
print one JSON document on stdout and nothing else there. Field names are snake_case; a key is always present
(`null` when there is no value).

```jsonc
// status --json
{ "tasks": [ {
    "id": "BR-003", "title": "…", "state": "WaitingFinalApproval",   // PipelineState
    "fix_cycle": 1, "max_fix_cycles": 3,
    // M11, only with the org chart's split limits: "gate_retries", "max_gate_retries",
    // "review_rounds", "max_review_rounds" (max_fix_cycles is then their sum)
    "spent_tokens": 61234, "reserved_tokens": 0, "budget_tokens": null,   // R41; null = no admission cap
    "active_owner": null,          // "user pid N" while a live process holds the task lock (never a lock probe)
    "compile_only": false,         // R59: an X++ verify proves compile level only; null: frozen config unreadable
    "next_action": "review the branch toryo/BR-003 …"   // what `status` prints after `next:`
} ],
  // M11, only once an environment gate has deployed (task show --json: this task's rows):
  "environments": [ { "lock": "d365-env", "task": "BR-001", "sha": "<sha>", "gate": "dbsync", "machine": "UDE-1",
                      "started_at": "…", "finished_at": "…", "ok": true, "approved": false, "status": "not approved yet" } ] }
// task show ID --json: the task object above, plus
{ "worktree": "…", "branch": "toryo/BR-003", "run_store": "…\\runs\\BR-003",
  "attempts": [ { "id": "BR-003/implement/1", "stage": "implement", "attempt": 1, "status": "succeeded",
                  "profile": "codex-openai", "error_class": null, "head_after": "<sha>", "partial_commit": null } ],
  "usage": [ { "id": 3, "attempt": "BR-003/implement/1", "profile": "codex-openai", "accounting": "exact",   // counted_tokens: what the row counts (weighted)
               "counted_tokens": 51000, "reservation_tokens": null, "reconciled_by": null, "includes_unreported": false } ],
  "gates": [ { "gate": "plan", "occurrence": 1, "status": "approved", "subject_digest": "…", "reason": null,
               "requested_at": "…", "resolved_at": "…" } ],
  "events_file": "…\\runs\\BR-003\\events.jsonl" }
```

**The flight recorder (ARCH-02).** Every command that drives a task appends the engine's events to
`<data_dir>\runs\<ID>\events.jsonl`, one JSON object per line: `{"v":1,"seq":N,"ts":"<RFC 3339>","task":"ID",
"generation":G,"type":"…", …}` with `type` one of `run_started`, `state_changed` (`from`, `to`, `event`),
`stage_started` (`stage`, `attempt`, `profile`, `detail`), `stage_finished` (`status`, `error_class`,
`duration_ms`), `turn_finished` (`profile`, `success`, `usage` = the four buckets + `accounting` as the store
recorded them — a cumulative adapter's delta —, or `reserved_tokens` for a turn with no telemetry, `duration_ms`),
`checkpoint_created` (`head`, `kind`, `found`), `verify_command` (`name`, `exit`, `timed_out`, `ok`, `duration_ms`),
`gate_opened` (`gate`, `occurrence`), `budget_warning` (`used`, `cap`), `escalated` (`reason_class` — a typed
snake_case class such as `max_fix_cycles`, `review_blocked`, `protected_config_changed` —, `reason`),
`paused` (`reason`), `run_ended` (`state`, `next_action`, `from` = the state when the drive started, `error` when
it stopped with one); M11: `lock_waiting` (`lock`, `holder`) and `environment_deployed` (`lock`, `sha`, `ok`) for
environment gates, whose `verify_command` also has `kind: "environment"` (absent for every other command). Every drive command ends its lines with one `run_ended`, also when it fails before the
first stage. `seq` grows across runs; `generation` is the task lock generation of the writing command. Reasons
are redacted like captures. `task cleanup` removes the file with the run store. The engine's own stderr lines
(`[state] …`, stage headers, `✔ checkpoint …`, verify commands, `!! budget: …`, and the reason of every
escalation and pause, once, before its `[state]` line) are rendered from these events.

```
toryo watch ID [--json] [--from-start] [--no-follow]
```

`watch` follows the file like `tail -f` — no task lock, so it runs next to `toryo run` in another window:
the last 20 events (all with `--from-start`), then new ones as they come, rendered as the run prints them, or
the raw lines with `--json`. It stops by itself 2 s after the last event once the task waits for you, is
finished, or nobody drives it; Ctrl+C also exits 0.

**Usage and cost (plan §12).** `toryo usage ID [--json] [--prices FILE]` shows the tokens per stage × profile ×
model in the four buckets, with the `exact`, `provisional` and `~unknown` (reservations for turns with no
telemetry) totals apart, and an estimated cost for rows billed `foundry` or `api` from the machine's price table
`%USERPROFILE%\.toryo\prices.toml` (start from `templates/prices.toml.example`; USD per million tokens per bucket,
`version`, `safety_factor`). Subscription rows show `—`, a model missing from the table `no price`, an estimate that
includes reservations or killed turns' tokens `~est.`. The total line says what it leaves out: `est. $x + no price
(N groups)` when some per-token groups have no price, `no price (N groups)` when none has one, `no price table`
when per-token groups exist but there is no table (`—` only when nothing is billed per token). Groups are stage ×
profile × model × billing kind.
`exact`/`provisional` are raw token counts; reservations are budget tokens (weighted by `pipeline.budget_weights`),
priced as uncached input. A `claude-foundry` profile needs `model:` to be priced: a Claude implementer runs with,
and records, the profile's model only. No table: the report without costs. Every usage row records the model the turn ran
with (the role's, else the profile's; the Foundry deployment name for a Codex provider profile); rows written
before M10 take it from the task's frozen configuration. `--json`: `{ task, prices: {path, version, safety_factor},
rows: [ {stage, profile, model, billing_kind, buckets, exact_tokens, provisional_tokens, reserved_tokens, turns, includes_unreported,
reconciled_rows, est_cost_usd, cost, vendor_reported_cost_usd} ], totals: {exact_tokens, provisional_tokens,
reserved_tokens, est_cost_usd, est_complete, unpriced_groups, includes_reservations, includes_unreported,
vendor_reported_cost_usd}, no_price: [ {model, profile} ] }`. `totals.est_cost_usd` is `null` when nothing was
estimated; `est_complete` is `false` when a per-token group has no price (also without any estimate), `true` when
all are priced, `null` when nothing is billed per token or there is no price table; `includes_reservations` /
`includes_unreported`: the estimate holds reservations / killed turns' tokens (either makes it `~est.`).

**Toasts (Windows).** With `notify: { toast: true }` in the machine config (default off; not frozen with a task,
not hashed) or `--notify` on `run`, `resume` and `approve --continue`, toryo shows a Windows toast when a task
needs you: a gate opens, the task escalates or pauses, a run reaches `Completed` or `Failed`, or a run stops with
an error — title `toryo — <ID>`, body the state and the next action (≤ 200 characters, ANSI and characters XML
forbids removed). Under `--yes` the gates raise no toast (they are approved automatically); `approve` without
`--continue` raises none. `--notify` and `--no-notify` are mutually exclusive; either overrides the machine
setting. The toast is Windows PowerShell 5.1 started with `-Command <fixed script>` (no inherited handles, no
console window) that reads the title and body from environment variables, so agent-written text is never part of
a command line or a script; it never blocks or fails the run, and toryo waits for it at exit at most until its
10 s deadline. Check it with `toryo notify-test` (hidden; `--dry-run` builds the toast without showing it).
Known limit: under PowerShell Constrained Language Mode (AppLocker/WDAC policies) the script cannot load the
WinRT types — `notify-test` then fails and says so. Elsewhere than Windows: nothing is shown.

## Org chart mode (M11)

A repository can describe its team in `.toryo/` instead of `toryo.yaml`'s `roles:` and `commands:`.
**Without `.toryo/toryo-orgchart.yaml` nothing changes**: a `toryo.yaml`-only repository behaves as in M10,
with the same configuration hash.

```
<repo>/
  toryo.yaml                          # project, policy, verify, timeouts, context, budgets — no roles/commands
  CULTURE.md                          # optional team norms (culture: in the org chart)
  .toryo/
    toryo-orgchart.yaml               # WHO: seats → Toryo profiles, gates, limits, checks
    roles/<seat>/toryo-role.yaml      # WHAT: purpose, instructions, skills, writes/never, before_handoff
    roles/<seat>/instructions.md      # added after Toryo's built-in role text, never instead of it
    skills/<name>/SKILL.md            # repository skills
    context/*.md                      # project policy (as before)
```

Starter set: `templates/d365fo/.toryo/`, `templates/d365fo/CULTURE.md` and `templates/d365fo/toryo.orgchart.yaml.example`
(the `toryo.yaml` of this mode). Every key of these files is `deny_unknown_fields` with a "did you mean"
hint (`seats.reviwer: seat 'reviwer' not found. Did you mean 'reviewer'?`); YAML tags are refused in every repository
file (review C1); all problems of a load are reported at once, each with its file and key path. `.toryo/` is read
from the repository checkout (`project.root`), never from a task worktree, and everything under it is a protected path.

- **The flow is fixed:** plan → build → review, with the plan gate and the final gate. A `flow:` key is an error.
- **Seats.** Exactly `planner`, `implementer`, `reviewer`; each names a **Toryo profile** of the machine config
  (R60) — `codex --profile openai` becomes a profile `codex-openai` with its own `CODEX_HOME`. `model` overrides the
  profile's model. `fallback` (an ordered list of profiles, implementer only; omitted = none) — a fallback on the
  planner or reviewer is an error. The machine config's `seats:` (the new spelling of `roles:`) overrides a seat
  on that machine. A setting in both `toryo.yaml` and the org chart (`roles:`, `commands:`,
  `pipeline.max_fix_cycles`) is an error.
- **Limits.** `limits: { max_gate_retries, max_review_rounds }`: a failing gate counts against the first, a rejected
  review (or a rejected final gate) against the second; a lone `max_fix_cycles` keeps today's single counter, and
  an org chart without `limits:` has that single counter too (2 fix cycles) — not the split defaults.
  `status` shows `fix 1 (gate retries 1/3, review rounds 0/3)`.
- **Checks.** `reviewer-differs-from-builder: off | warn | error` compares the `kind` of every profile whose
  implementer turn ran (also one that failed or was interrupted after writing — its work stays; M11-R7) with the
  reviewer's: `warn` (default) notes it at the final gate, `error` escalates
  (`same_vendor_review`) before the review turn.
- **Instructions and culture.** In this mode the context of every agent has the embedded role template **always**,
  then the role's `instructions.md`; the culture file closes the policy sources. A leftover `.toryo/roles/<role>.md`
  (the M4 override) is an error: move it to `.toryo/roles/<role>/instructions.md`.
- **Skills.** A role file lists skill names; each is `.toryo/skills/<name>/SKILL.md` of the repository, else
  `<skills_dir>/<name>/SKILL.md` of the machine config (first match wins; not found is an error naming both). The
  implementer's list is a **menu**: the planner sees `name — description` (from the SKILL.md front matter) and lists
  in the plan's `skills` what this task needs; `PLAN.md` shows `Skills for the implementer: …`; a name off the
  menu, or skills that do not fit `context.budget_kb.implementer`, send the plan back (bounded). Planner and reviewer
  get every skill of their role file. Skills are pasted into the context file (never trimmed, hashed, listed by
  `toryo context show` with origin and SHA-256), never into the untrusted user prompt. **Only `SKILL.md` is
  delivered**; files next to it are readable by the agents only for repository skills (they are in the worktree).
  The menu lists only the skills that resolve now. The budget check covers **every profile of the implementer
  chain** (a Claude fallback has TASK.md and PLAN.md inline, never trimmed to make room for skills). Before a turn
  starts, every skill it needs must resolve: otherwise the task escalates with `skill_unavailable` (naming both
  lookup paths; no turn, no tokens) — put it back and `toryo resume <id> --note …`. A context with skills that is
  still over budget escalates with `context_over_budget`.
- **The write fence.** The implementer's `writes:` (globs; when set, the only files it may change — `writes: []` or no
  `writes` means no allowlist: only `never` applies) and `never:`
  (never, even inside `writes`) — case-insensitive, `/` and `\` alike. A planned file outside the fence sends the
  plan back; a changed file outside it is restored and sent back to the implementer, with the rule named. The
  fence only narrows `toryo.yaml`: protected and denied paths stay so; `## Allow protected` in TASK.md opens a fence
  entry like a protected path. Patterns are normalised at load (`./src/**`, `src/` and `src` all mean `src/**`: the
  path and everything under it); an absolute pattern, `..`, an empty one or syntax the matcher lacks (`{ } [ ] !`;
  it has `*`, `**`, `?`) is a load error. (`policy.deny_paths` / `protected_paths` keep their M10 behaviour;
  `toryo doctor` warns about a pattern there that can never match.)
- **The culture file is protected** (org chart mode) like the verify files: an agent's change is restored, unless
  TASK.md lists it under `## Allow protected`.
- **Gates.** The implementer's `before_handoff` lists gates by name, every `check` gate before every `environment`
  gate, at least one check gate. They run on the checkpoint commit, in that order, and stop at the first failure;
  the exit code decides. Every gate gets `TORYO_TASK`, `TORYO_SHA`, `TORYO_VERIFY_CHECKOUT`, `TORYO_VERIFY_BUILD`
  (legacy verify commands too).
  - `kind: check` — defined in the repository (`program`/`args` or `tool: xpp` + `metadata`/`projects`, `timeout:
    30s | 20m | 1h`), runs exactly like a verify command: clean checkout, Low integrity, no secrets, its script
    protected. (M18, for everyone) a repository program or script (`-File` of PowerShell, else the first token of
    `-Command` or the first positional `.ps1`; the script of `sh`/`bash`/`python`/`node`/…, skipping the values of
    `-o`, `-W`, `-r`, `-I`, …; the first token after `cmd /c`) is protected before it exists, and `task create` refuses
    a configuration whose base commit lacks it. A candidate without a path separator or a script extension is never
    refused; any argument naming a repository `.ps1 .psm1 .cmd .bat .sh .py .js .ts .rb .pl .exe` file missing at the
    base commit is (M11-R6). Not caught: a script without an extension, in the repository root, run by a program
    outside this table.
  - `kind: environment` — the repository only declares it (`timeout`, `if_missing: error | skip`); the command is
    `environment_gates.<name>` in the machine config (`program`, `args`, `lock`, `env`, `secret_env`), frozen with the
    task. It runs only after every check gate passed **and the verify checkout is exactly the checkpoint** (a check
    gate that changed or added a file there → no environment gate starts, the task escalates with
    `verify_mutated_tree` / `verify_checkout_dirty`; so an org chart with an environment gate needs `verify.checkout:
    clean`), with **your normal rights** (no Low integrity), in an **empty working directory** that agent code cannot
    write (`<verify_root>\<task>\envgate`; the code is under `TORYO_VERIFY_CHECKOUT`), inside a Job Object, with an
    environment from scratch + the gate variables + `env` + the resolved
    `secret_env` (redacted everywhere). The program and every script path must be absolute (or a PATH name) and
    outside the repository, the worktrees and the verify root; `%VAR%` in them is expanded once, at load (an unset
    variable is a load error) — the expanded text is what is checked, frozen and run (`config show` prints it), and a
    PATH name that resolves inside those folders is refused at run time. One gate per `lock` runs at a time across all tasks;
    a second waits (`status`: `waiting for lock d365-env (held by T-12)`), `toryo stop` ends the wait. A failure
    goes back to the implementer as data and counts against `max_gate_retries`. A machine without the gate:
    `if_missing: error` refuses the task, `skip` runs without it and says so at the final gate.
  - **The script contract** (`templates/toryo.local.yaml.example`): exit code decides (PowerShell: `$ErrorActionPreference
    = 'Stop'` and `if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }` after every native call); deploy only
    `$env:TORYO_VERIFY_CHECKOUT` / `$env:TORYO_VERIFY_BUILD`, never your main repository folder; idempotent; print
    the useful error lines; never write into the checkout (`verify_mutated_tree`); never wait for input; the working
    directory is an empty folder — never rely on it, call every tool by its absolute path and do not run `git`
    against the checkout (use `TORYO_SHA`).
- **What is deployed.** Before an environment gate starts, Toryo records per lock which task and commit the
  environment receives (`environment_deployments`), and updates it at the end. `toryo status` shows one line per
  lock (`environment    d365-env holds BR-001 @ 1a2b3c4d5e6f (not approved yet)` — `approved` once that task's
  final gate is approved; `interrupted` after a crash or stop), so do `task show`, both `--json` documents
  (`environments[]`) and the final gate notes. Cancelling or cleaning up a task never clears it: the environment
  still runs that code. With an X++ compile and an environment gate that passed, the final gate says `verify:
  compile + dbsync on <machine>` instead of `compile only`.
- **The machine floor (review H2, for everyone).** The machine config's `verify: { min_boundary, verify_root }`: a
  repository `verify.boundary: none` is refused at `task create` (and `task config --reload`) unless the machine says
  `min_boundary: none`; a machine `verify_root` replaces the repository's. Environment gates are the only sanctioned
  way to run something with your full rights.

### Migrating an M10 `toryo.yaml` to `.toryo/`

1. Copy `templates/d365fo/.toryo/` (or write `.toryo/toryo-orgchart.yaml` and `.toryo/roles/{planner,implementer,reviewer}/toryo-role.yaml`).
2. `roles:` → `seats:` of the org chart (same `profile`, `model`, `fallback`). An implementer without `fallback`
   now has **none** (M10: the planner's profile) — write `fallback: [<planner profile>]` to keep the old behaviour.
   Delete `roles:` from `toryo.yaml`; the machine's `roles:` may stay or become `seats:`.
3. `commands.verify[]` → `gates:` of the org chart, `kind: check`, with `timeout: 20m` instead of `timeout_min: 20`;
   list their names, in order, in the implementer's `before_handoff`. Delete `commands:` from `toryo.yaml`.
4. `pipeline.max_fix_cycles` → `limits:` (`max_fix_cycles: 3` alone, or `max_gate_retries` / `max_review_rounds`).
   Delete it from `pipeline:` (the budget keys stay).
5. `.toryo/roles/<role>.md` → `.toryo/roles/<role>/instructions.md`, named in that role file's `instructions:` — it is
   now added after Toryo's built-in role text instead of replacing it; drop what duplicates the built-in rules.
6. `codex --profile <name>` logins → a Toryo profile per login in the machine config, each with its own
   `env: { CODEX_HOME: … }` (`$env:CODEX_HOME = '<path>'; codex login` once); seats name those profiles.
7. A repository with `verify.boundary: none` needs `verify: { min_boundary: none }` in the machine config.
8. Check with `toryo config show`: the mode, every file read, where each seat, gate and setting came from.

## The effective configuration (M11)

`toryo config show [--json]` prints the configuration `task create` would freeze for this repository and
machine: the mode (`toryo.yaml only`, or `org chart` when `<project.root>/.toryo/toryo-orgchart.yaml` exists),
every file read with its SHA-256, and where each seat, profile, gate, toolchain, `skills_dir` and verify setting
came from. Sources: `repo`, `machine`, `repo_overridden_by_machine` (a machine seat override, or the machine's
`verify.verify_root`), `null` (not set). `--json` (schema 1):

```jsonc
{ "schema": 1, "mode": "org chart",                 // or "toryo.yaml only"
  "config_sha256": "…",                             // what `task create` records as tasks.config_sha256
  "team": "d365fo-dev",                             // null in toryo.yaml-only mode
  "files": [ { "what": "toryo.yaml", "path": "…", "sha256": "…" } ],   // + machine config, org chart, role <seat>
  "seats": [ { "seat": "implementer", "profile": "codex-openai", "model": null, "fallback": ["claude-max"], "source": "repo" } ],
  "profiles": [ { "name": "claude-max", "kind": "claude", "billing": "subscription", "exe": "…", "source": "machine" } ],
  "gates": [ { "name": "dbsync", "kind": "environment", "timeout": "30m", "lock": "d365-env", "if_missing": "error",
               "defined_on_this_machine": true, "source": "machine" } ],   // toryo.yaml only: the verify commands, kind "check"
  "before_handoff": ["build", "dbsync"],             // the implementer's gate order; null without an org chart
  "toolchains": [ { "name": "xpp", "source": "machine" } ],
  "skills_dir": { "path": "C:/Temp/AgentShare/skills", "source": "machine" },   // null when unset
  "verify": { "boundary": { "value": "low_integrity", "source": "repo" },
              "min_boundary": { "value": null, "source": null },          // null: the default floor low_integrity
              "verify_root": { "value": "%LOCALAPPDATA%\\toryo-verify", "source": "repo" } },
  "limits": { "max_gate_retries": 3, "max_review_rounds": 3 },            // null without an org chart
  "checks": { "reviewer-differs-from-builder": "warn" } }
```

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
| `tests/engine_context.rs` | Windows + Linux | M4: context per attempt (fixed order, Codex without AGENTS.md, hash + artifact), Claude implementer inline pack, budget trims, `context show` |
| `tests/engine_policy.rs` | Windows + Linux | M5: `protected_ignored_files`, Git metadata change, frozen config + `--reload`, deny paths, `implementer_temp_scoped`, `implementer_fallback` a–d, `git_filter_textconv_canary` |
| `tests/engine_artifacts.rs` | Windows + Linux (+ Windows-only boundary test) | M6: `artifact_crash_points`, tampered/missing versions, clean verify checkout + `ansi_stripped`, `verify_mutated_tree`, `verify_root_label_probe`, `verify_boundary_writes` |
| `tests/engine_usage.rs` | Windows + Linux | M7: `usage_accounting` a–c′, live interrupt, `review_catches_agreeing_test` (BR-002 replay), same-vendor note |
| `tests/redaction.rs` | Windows + Linux | `redaction_split_boundaries` (C9/R42) |
| `tests/architecture.rs` | Windows + Linux | no vendor types outside the adapters |
| `tests/git_canary.rs` | Windows + Linux | Git hook/filter/textconv canaries on the real `git` |
| `tests/engine_scenarios.rs` | Windows + Linux | the prototype's scenarios on the engine: final-gate rejection, max fix cycles, the re-plan bound, unlimited human plan rejections, blocked review, partial-AC correction, the fallback chain exhausted, task-id reuse |
| `tests/regress_agents.rs` | Windows + Linux (sh-fake cases Unix only) | code review 2026-09-29, agents: outcomes decided after the stream (Codex notices), error classification, live Claude usage, output and running Codex commands count as liveness, fake/real parity, fail-closed Codex preflight, capture errors |
| `tests/regress_policy.rs` | Windows + Linux (link cases Unix only) | protected paths (entries, links, case, verify programs, pre-upgrade baselines), snapshots of links and nested repos, repo `include`/`includeIf` (R37), the AC parser, rename sources |
| `tests/regress_cli.rs` | Windows + Linux | config validation at load (roles, values, `codex.overrides`), `fallback: []`, crash-safe `task config --reload`, a snapshot before `--discard-partial`, `doctor` version pins, lock/store hardening |
| `tests/regress_implement.rs` | Windows + Linux | the HEAD guard on every exit of an Implementer turn, failed turns with changes kept as a recovery checkpoint, `--no-fallback` after a sticky switch, a stoppable backoff, schema-checked implementation reports |
| `tests/regress_engine.rs` | Windows + Linux | failed Planner/Reviewer turns never yield a plan or review, §11 retries per class, schema-validated plans and reviews, verify again after a reload, `approval_mismatch`, a refused correction turn, read-only writes |
| `tests/regress_recovery.rs` | Windows + Linux | crash recovery runs the HEAD guard first; an unfinished Git operation keeps the snapshot path; failed turns that crash stay failed |
| `tests/m10_events.rs` | Windows + Linux | M10-A1: the flight recorder of a run to the final gate (order, `seq`, generations), typed escalation classes, no secret in the file, an unwritable file |
| `tests/m10_views.rs` | Windows + Linux | M10-B: `status --json`, `task show --json`, a cleaned-up task; `watch` history, `--json`, a concurrent run followed to the gate, errors, Ctrl+C → exit 0 |
| `tests/m10_usage.rs` | Windows + Linux | M10-C: `toryo usage` numbers and costs (text and `--json`), `no price`, the model on usage rows and the pre-M10 fallback, bad and missing price tables; scripted turns of a mixed run are exact zero |
| `tests/m10_notify.rs` | Windows + Linux (`notify-test` Windows only) | M10-D1: toasts at the gates, an escalation and the end, the machine setting and `--notify`/`--no-notify`, `notify-test --dry-run` |
| `tests/m11_trust.rs` | Windows + Linux | M11-A: the machine floor for `verify.boundary`, the machine `verify_root` and its origin in `doctor` |
| `tests/m11_orgchart.rs` | Windows + Linux | M11-B/C1: `config show` (text, `--json`), a task frozen and reloaded with the same digest, a fake run to the final gate in org chart mode, layered role instructions in `context show` |
| `tests/m11_roles.rs` | Windows + Linux | M11-C: the write fence at plan time and after a build turn (`## Allow protected`), `reviewer-differs-from-builder` off/warn/error after a fallback, split limits end to end |
| `tests/m11_gates.rs` | Windows + Linux | M11-E1/E2: the gate variables, M18 admission, verify output redacted with the task's secrets, stdout/stderr order kept |
| `tests/m11_env_gates.rs` | Windows + Linux (normal-token vs Low-IL test Windows only) | M11-E3: environment gates after the checks, the deployment record in every view, the shared lock with `stop`, secrets, `if_missing: skip`, a mutated checkout, an unavailable gate, a crash during the gate |
| `tests/m11_skills.rs` | Windows + Linux | M11-D: the planner's menu, the plan's `skills` (off the menu, over the budget, none), skills per role in `context show`, a changed SKILL.md, the Claude implementer's context file |

CI (`.github/workflows/ci.yml`) runs `cargo test`, `clippy -D warnings` and the core without the kit
(`--no-default-features`) on `windows-latest` and `ubuntu-latest`, then builds a release `toryo.exe`.

## Status

M0–M7 are done: adapters (M1), state machine (M2), SQLite store + lock + crash recovery (M3),
ProjectContextLoader (M4), protected-state snapshot + frozen config + implementer fallback pauses (M5),
immutable artifacts + verify checkout under `verify_root` with the Low-IL boundary (M6), usage accounting,
admission budget and `DECISIONS.md` (M7). Real-agent validation on Windows: `scripts\m4-m7-check.ps1`.

M8 (E2E MVP) is done: the demo flow runs with no copy/paste on the calculator fixture and on the
BatchRename sample (BR-001, then BR-002 with a real fix cycle), and the real-CLI checks of V08-VERIFY 5
pass — a live budget interrupt of a Claude turn (R64), the fail-closed Codex preflight (R62), and a silent
Codex command that is not killed as stalled (R66). Acceptance on Windows: `scripts\m8-check.ps1`.
Next: plan v0.8 §17 (D1–D3, M9). `spike pipeline` stays the behavioural reference (R52).

M9 (`M9_task.md`, branch `m9/d365-ready`) is done in code: trust boundaries — agent profiles and
toolchains come only from the machine config (R60; `doctor` shows each source) and untrusted task-pack text
reaches agents only as data (R61); X++ verify on the UDE toolchain — `toolchains.xpp`, `tool: xpp`, per-project
MSBuild runs with parsed diagnostics, `scope: compile`, `doctor` X++ checks and `toryo xpp build` (R59, proposed
R72); the D365FO starter kit (`templates/d365fo/`) and the dogfooding kit (`templates/dogfood/`,
`scripts/verify.ps1`). Still to do by the owner on Windows and the UDE machine: section E of `M9_task.md` (the
real-CLI canary `scripts\m9-check.ps1 -Canary`, V08-VERIFY 2–4 with `toryo xpp build`, the first real D365FO
task). Decisions made on the way: `docs/M9-NOTES.md`.

M10 (`M10_task.md`, branch `m10/flight-recorder`) is done in code: the engine emits typed events (ARCH-02) —
recorded per task in `events.jsonl` (the flight recorder) and rendered as the engine's own stderr lines — the same
text as before, except that every escalation and pause now prints its reason once, right before its `[state]` line
(deviation 6 in `docs/M10-NOTES.md`); `status --json` / `task show --json` (DES-10); `toryo watch` follows a running task without its
lock; `toryo usage` with estimated cost from the machine's `prices.toml` (plan §12), and every usage row now
records its model; scripted turns of a mixed fake/real run cost exact zero; an opt-in Windows toast when a task
needs the owner. Decisions, deviations and open questions: `docs/M10-NOTES.md`.

M11 (`M11_task.md`, branch `m11/orgchart`) is done in code: the trust floor (review C1 — repository files are
parsed without machine-only keys and YAML tags are refused; H2 — a machine floor for `verify.boundary` and the
machine's `verify_root`); org chart mode (`.toryo/toryo-orgchart.yaml`, role files, `toryo config show`), layered
role instructions and culture, the implementer's write fence, `reviewer-differs-from-builder`, split retry limits;
check gates with gate variables, M18 entry points protected before they exist, verify output redacted with the
task's secrets (review M2), environment gates with a machine-wide lock and a deployment record; skills (repository
then machine, the planner's choice, delivered through the context loader). Decisions, deviations and open
questions: `docs/M11-NOTES.md`. Next after M11: review M13 (cp932 output on Japanese Windows also garbles DB-sync
output).
