# toryo

> **Toryo** (棟梁, *tōryō*) is the master carpenter who leads a building crew: he does not cut the wood himself,
> he hands out the work, checks every joint and signs off each stage. That is this tool's job — Claude Code and
> Codex are the craftsmen; `toryo` plans the order of work, enforces the rules, checkpoints and verifies.

## What it is

Toryo is a Rust command-line orchestrator that takes one work item (a `TASK.md`) through a fixed, gated workflow
on a real Git repository, using the Claude Code and Codex CLIs as agents:

```
plan (planner) → you approve the plan → implement (implementer) → checkpoint commit
   → gates (build, tests, deploy …) run by Toryo, not by the agent → review (reviewer) → you approve the result
```

- **Isolated.** Every task works in its own Git worktree on branch `toryo/<ID>`. Toryo never merges and never pushes.
- **Durable.** State is in SQLite; a task survives a crash, a Ctrl+C or a reboot (`toryo resume`), and recovery never
  accepts a commit Toryo did not make.
- **Verified by code, not by claims.** Gates are commands Toryo runs on the checkpoint commit; the exit code decides.
  The reviewer must answer for exactly the acceptance criteria of `TASK.md`.
- **Fenced.** Protected paths, deny paths, a per-role write fence and a protected-state scan keep the agents away from
  files that decide the outcome (configs, gate scripts, `AGENTS.md`, build projects).
- **Bounded.** Admission budgets, fix-cycle limits, quota fallback (implementer only), stall and timeout limits.
- **Honest about machine problems.** A gate that cannot start, a missing skill, an unreadable context file or a
  deploy that waits for your yes stops the task without spending an agent turn or a fix cycle.

Platform: Windows is the product platform (Job Objects, Low-integrity verification, Credential Manager, toasts); the
core also builds and tests on Linux.

## Install

```powershell
cargo build --release --locked          # → target\release\toryo.exe (Rust 1.89+)
toryo --version                          # version and the commit it was built from
```

No binary is kept in the repository; CI uploads the artifact `toryo-windows-x64`. Put `toryo.exe` on your `PATH`
(or `Set-Alias toryo …\target\release\toryo.exe`). Git for Windows, PowerShell 7 and the logged-in `claude` / `codex`
CLIs are needed to run real tasks. `scripts\m0-smoke.ps1` sets up a first machine (it looks for `bin\toryo.exe`: copy
the release binary there) and runs a smoke test.

## Quick start

```powershell
toryo doctor                                   # environment, profiles, config, gates, evidence — fix every FAIL
toryo task create --file TASK.md               # snapshot TASK.md, freeze the config, create worktree + branch toryo/<ID>
toryo run <ID>                                 # plan → stops at the plan gate
toryo approve <ID> --gate plan --continue      # implement → gates → review → stops at the final gate
toryo status <ID>                              # state, and `next:` — exactly what to type
toryo approve <ID> --gate final                # Completed; merge branch toryo/<ID> yourself
toryo task cleanup <ID>                        # remove the worktree and run store (the branch is kept)
```

Rehearse without spending tokens (the gates run for real, the agents are scripted):
`toryo run <ID> --fake-agents script.json` (hidden flag; see `docs/HUONG-DAN.md` §5.3 and §7.3).

`TASK.md` needs a title, a `## Acceptance criteria` section with one `- AC-1: …` line per criterion (after the id: `:`,
`.`, `)`, `：` or ` — ` / ` – ` / ` - `; a line that starts like a criterion and is not accepted is refused, never
dropped), and optionally `## Allow protected` (paths this task may change although they are protected). `TASK.md` is
frozen at `task create`; the task id (letters, digits, `_`, `-`, at most 64) is permanent.

## Commands

```
toryo task create --file TASK.md [--id ID] [--repo R] [--base B]    # prints "base: <branch> @ <sha>"
toryo run ID                              # 🔒 runs until a gate, an escalation, a stop or completion
toryo approve ID --gate plan|final [--continue] [--expect DIGEST] [--force]
toryo reject  ID --gate plan|final --reason "…"
toryo resume [ID] [--note "…" | --discard-partial | --keep-partial | --accept-protected-change PATH]
                                          # no id: every waiting task as it is (never one still in New)
toryo stop ID                             # ask the running owner to stop; Ctrl+C in the window of `toryo run`
                                          # or `toryo resume` does the same (a second Ctrl+C ends the process at once)
toryo status [ID] [--json] · toryo task list|show ID [--json] · toryo watch ID [--json]
toryo task config ID [--reload]           # the frozen configuration; reload only while the task waits
toryo task cleanup ID [--force]           # --force when it would delete unsaved work or snapshots
toryo budget set ID --tokens N | --none   · toryo usage ID [--json] [--prices FILE]
toryo context show ID --stage plan|implement|review    # what the agent actually receives
toryo config show [--json]                # which files were read, where every setting comes from
toryo doctor [--canaries [--profile P …]] # re-verify profiles after a CLI upgrade
toryo gate list                           # the check gates Verify runs (and the environment gates it does not run here)
toryo gate run NAME [--print-only]        # one check gate outside a task, in the repository, with your own rights
toryo xpp build --repo R --project P [--print-only] [--low]    # the `tool: xpp` build outside a task
toryo partial export ID --attempt N --into DIR · toryo secrets set <target> · toryo notify-test
```

🔒 = takes the task lock; a second command on a running task is refused (`task_active`, exit code 3).
The repository file is chosen by `--config`, else `./toryo.yaml` — the file in the folder you run `toryo` **in** (the
current directory, not the repository root) — else `$TORYO_CONFIG`.

## Configuration

Two layers, and the line between them is a security boundary.

**Machine config** — `%USERPROFILE%\.toryo\toryo.local.yaml` (or `--machine-config`), outside every repository:
agent **profiles** (the executable, its environment, `secret_env` references to Credential Manager, the Codex sandbox),
`toolchains.xpp` (MSBuild and the CI/CD NuGets), `environment_gates` (the commands of gates that change your
environment), `verify: { min_boundary, verify_root }`, `skills_dir`, `git.trusted_filters`, `notify`, `seats:`
overrides. A repository file that defines `profiles:` or `toolchains:` does not load.

**Repository config** — `toryo.yaml`: `project` (`root`, `worktrees_root`, `base_branch`), `roles` and `commands.verify`
(or the org chart, below), `policy` (`deny_paths`, `protected_paths`, …), `verify` (`checkout`, `boundary`,
`verify_root`), `timeouts`, `context.budget_kb`, `pipeline.budget_tokens`. YAML tags are refused. All files are UTF-8
(a byte-order mark is ignored; anything else is refused by name). Keys that are accepted but do nothing in this
version are announced by `config show`, `doctor` and `task create`.

A task runs on the configuration **frozen** at `task create`; after you change a file, `toryo task config <ID> --reload`
(and then `toryo resume <ID>`) brings the task along.

### Verify commands and the X++ build

`commands.verify` entries are `{ name, program, args }` or, for Dynamics 365 F&O, `{ name, tool: xpp, metadata,
projects, timeout_min }`: MSBuild on the build project of each package with the machine's toolchain, parsed
diagnostics sent back to the implementer as file, line and code. `toryo xpp build` runs the same build by hand.

A `program` entry can set `diagnostics: msbuild` (default `none`): its output is read for MSBuild error lines
(`file(line,col): error CODE: message` — MSBuild, `dotnet build`, a script wrapping them) and the implementer gets
file, line and code ahead of the output tail. Any other stack needs nothing more than `program` + `args`.
`toryo gate list` shows the check gates; `toryo gate run NAME [--print-only]` runs one outside a task, in the
repository folder with your own rights (`TORYO_SHA` is `working-tree`). It never runs an environment gate, and
`--low` is for `tool: xpp` gates only.

### Org chart mode

A repository may describe its team in `.toryo/` instead of `roles:` and `commands:`:

```
toryo.yaml                              # project, policy, verify, timeouts, context, budgets
CULTURE.md                              # optional team norms
.toryo/toryo-orgchart.yaml              # WHO: seats → profiles, gates, limits, checks
.toryo/roles/<seat>/toryo-role.yaml     # WHAT: purpose, skills, writes/never, before_handoff
.toryo/roles/<seat>/instructions.md     # added after Toryo's built-in role text, never instead of it
.toryo/skills/<name>/SKILL.md           # how-to manuals the planner picks from for the implementer
.toryo/context/*.md                     # project policy
```

- **Seats:** `planner`, `implementer`, `reviewer`, each a profile of the machine config. Only the implementer may
  have a `fallback` (an ordered list of profiles, used when it is out of quota).
- **Limits:** `max_gate_retries` and `max_review_rounds` are counted separately.
- **Checks:** `reviewer-differs-from-builder: off | warn | error`.
- **Write fence:** the implementer's `writes:` (the only files it may change) and `never:` (never, even inside
  `writes`); a change outside the fence is restored and sent back; `## Allow protected` opens one entry for one task.
- **Skills:** repository first, then the machine's `skills_dir`; delivered by Toryo through the context file, never
  trimmed, hashed and listed by `context show`.
- **Starter set:** `templates/d365fo/` — org chart, role files, skills, context, a `toryo.yaml`, the machine example
  and two print-only gate scripts. Every file lists its `TODO(owner)` items at the top.

## Gates

The implementer's `before_handoff` lists gates by name; every `check` gate comes before every `environment` gate; they
run on the checkpoint commit, stop at the first failure, and the exit code decides. Every gate gets `TORYO_TASK`,
`TORYO_SHA`, `TORYO_VERIFY_CHECKOUT` and `TORYO_VERIFY_BUILD`, and English UI-language variables.

- **`kind: check`** is defined in the repository (`program`/`args` or `tool: xpp`, `timeout`). It runs in a clean
  checkout of the commit under `verify_root`, at **Low integrity** on Windows, with no secrets. A gate's entry script
  is protected before it exists, and `task create` refuses a configuration whose base commit lacks it.
- **`kind: environment`** is only *declared* in the repository (`timeout`, `if_missing: error | skip`); its command is
  `environment_gates.<name>` in the machine config (`program`, `args`, `lock`, `env`, `secret_env`, `confirm`). It runs
  with your normal rights, in an empty working directory, from a **byte-exact export** of the checkpoint commit, under a
  machine-wide lock (one deploy per lock at a time). `confirm: never | first | always` asks you **before** the deploy
  (`first`: once per task — its later checkpoints deploy without a question; `always`: before the deploy of every
  checkpoint; after a `task config --reload` that changed the configuration you are asked again);
  a plain `toryo resume <ID>` is the yes. To say no: `toryo resume <ID> --note "…"` (the implementer gets your note; one
  fix cycle) or `toryo task cleanup <ID>` (gives the task up). Toryo records which task and commit each lock's
  environment holds (`toryo status`).
- **Output.** The redacted output of a gate is written to `<run store>\gate-logs\` — the same bounded text the result
  holds: the first 1 MB and the last 8 MB, cut on line boundaries, with one line where output was dropped (not the whole
  of a longer output). A long-running gate prints a line every minute, and the failure line names the log. Text is
  decoded line by line with the machine's OEM code page when it is not UTF-8.
- **Line endings.** A repository with `text` / `eol=crlf` attributes over blobs that still hold CRLF makes Git report the
  verify checkout as modified: run `git add --renormalize .` once and commit, or every verify stops with
  `verify_mutated_tree`.
- **Cannot start** → the task stops before any gate runs, and no implementer turn is spent:
  a **check gate** whose program is not on the machine → `gate_unavailable`;
  an **environment gate** whose program, script or secret is missing → `environment_gate_unavailable`
  (`task create` already refuses one whose script is missing). An environment gate that runs into its timeout stops the
  task for you (`environment_gate_timeout`), also with no implementer turn.

## What the boundary does and does not buy

In plain words, so that "Low integrity" is not read as "sandbox":

- A **check gate** cannot write the profile, the repository, the task's worktree, Toryo's database or the deploy
  folder. It **can read** everything you can read and it can use the network, including local services that accept
  your Windows login. Keep secrets out of the verify environment.
- Toryo protects a gate's **entry script**, not what that script loads (dot-sourced, imported, called). Keep the script in
  one file, or list its helpers under `policy.protected_paths` and the role's `never:`.
- An **environment gate's** command comes only from the machine config and it reads a byte-exact export of the commit —
  not the checkout the check gates used. That guards your own script: a contract, not a boundary.
- What is deployed is **still agent-written** and runs in your environment **before the review and before the final
  approval**, unless `confirm` is `first` or `always`.
- The last 40 lines of a gate's output go to the model vendors (as the implementer's fix feedback), and only `secret_env`
  values are redacted. Print no connection string, key or token.
- Untrusted text (a task pack file, verify output) reaches the agents only as data, never as instructions.
- Text an agent, a vendor or a gate wrote reaches **your console** as inert text: escape sequences are removed, carriage
  returns dropped, other control and direction-changing characters shown as `�` — a plan or an escalation reason cannot
  write your clipboard, retitle the window or paint a forged "Completed" line. `--json` output is data and keeps what was
  sent (JSON escapes it); `toryo context show` is byte-exact into a pipe and inert on a terminal.

## When a task stops

`toryo status <ID>` ends with `next:` — the command to type. `docs/HUONG-DAN.md` §9.1 lists every escalation class
with its meaning and what to type. Three rules cover most of it: a **machine-caused** stop (`gate_unavailable`,
`environment_gate_unavailable`, `environment_gate_timeout`, `environment_gate_confirm`, `skill_unavailable`,
`context_over_budget`, `context_unreadable`, `xpp_toolchain_unavailable`, `verify_boundary_unavailable`) costs no fix
cycle — fix the cause and `toryo resume <ID>` (after `toryo task config <ID> --reload` if the fix is in a config file);
a protected file that changed → restore it, or `resume --accept-protected-change <path>`; everything else →
`resume --note "…"` (one more fix cycle), `approve --gate final --force` (opens the final gate without the AI review) or
`task cleanup`.

## Following a task

- `toryo watch <ID>` follows the flight recorder `<data dir>\runs\<ID>\events.jsonl` without taking the lock; `--json`
  prints the raw events (`run_started`, `state_changed`, `stage_started/finished`, `turn_finished`, `checkpoint_created`,
  `verify_command`, `gate_opened`, `escalated` with a typed `reason_class`, `paused`, `run_ended`, …).
- `toryo status [ID] --json`, `toryo task list --json` and `toryo task show ID --json` print one JSON document on
  stdout (snake_case; a key is always present, `null` when there is no value): state, fix cycle, token spend and
  budget, `active_owner`, `next_action`, attempts, usage rows, gate occurrences and the environments' deployment rows.
- `toryo usage ID` shows tokens per stage × profile × model, and an estimated cost for per-token billing from the
  machine's `prices.toml` (`templates/prices.toml.example`); subscription rows show `—`.
- `notify: { toast: true }` in the machine config (or `--notify`) shows a Windows toast when a task needs you.
- **Colour.** On a terminal, toryo paints its own lines by what they mean — green `✔` / `Completed`, red `✘` / `!!` /
  `Escalated`, yellow for what waits for you, dim bookkeeping, the commands to type in cyan — and ends a run in an open
  frame instead of the `══════════` banner. `--color auto|always|never` (default `auto`; `NO_COLOR` turns `auto` off,
  `CLICOLOR_FORCE` on). Into a pipe or a file the output is the plain text, as before; `--json` is never painted. Text an
  agent wrote (the plan, the review, the lines of a reason) is never painted: colour marks the lines that are toryo's own.

## Repository layout

| Path | What |
|---|---|
| `src/engine/` | the engine: state machine, SQLite store, task lock, recovery and snapshots, context loader, protected-state scan, artifacts, verify checkout and boundary, gates, `tool: xpp`, flight recorder, views, usage |
| `src/agents/` | Claude and Codex adapters (exact launch lines, stream parsers), the fake agent, the hub |
| `src/process/` | Job Objects, Low-integrity spawning, environment allowlist, secret redaction, output decoding |
| `src/config.rs`, `src/team.rs` | machine and repository config, the org chart, skills |
| `src/doctor.rs`, `src/evidence.rs` | `doctor` and the version-bound canary evidence |
| `src/spike/` | the M0 kit and the reference `spike pipeline` (cargo feature `m0-kit`, on by default) |
| `templates/` | starter configs: `toryo.yaml.example`, `d365fo/`, `dogfood/`, `prices.toml.example` |
| `tests/` | integration tests against a fixture repository with scripted fake agents |

## Development

```powershell
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings              # and again with --no-default-features
cargo test --locked
```

CI (`.github/workflows/ci.yml`) runs the tests and clippy on `windows-latest` and `ubuntu-latest`, clippy on the core
without the M0 kit (no tests run without it), and builds the release `toryo.exe`. Never run real agents in tests: use
`--fake-agents`.

## Documentation

- `docs/HUONG-DAN.md` — the guide (Vietnamese): the flow, installation, configuration, a practice project, **§7.3 the
  first real task on D365FO**, **§9.1 every escalation class**.
- `docs/STATUS.md` — where the project stands and the one open list; `docs/DECISIONS.md` — the rules in force.
- `docs/M0-RUNBOOK.md`, `docs/M0-RESULTS.md`, `docs/M0-FINDINGS.md` — the spike kit and what it measured.
- `docs/M9-NOTES.md`, `docs/M10-NOTES.md`, `docs/M11-NOTES.md`, `docs/M12-NOTES.md` — per-milestone records;
  `docs/REVIEW-2026-10-01.md` and its status — the review whose ids (`H1`, `M13`, …) appear in code comments.
