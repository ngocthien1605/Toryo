# M0 runbook — Toryo (plan v0.6 §17)

M0 answers the twelve `M0-VERIFY` questions on **your Windows machine**, runs the Job Object and
boundary tests, and does one manual pipeline dry-run. Every `toryo spike …` command records its
evidence in `%LOCALAPPDATA%\toryo\m0\results.jsonl`; `toryo spike report` turns that into the
table that goes into plan v0.7.

Think of it as a **commissioning checklist for a new factory line**: before the conveyor (M1–M8)
is built, each station's safety guard is tried once with a test piece, and the result is written
in the log book.

Budget: about 12–17 h (plan §17). Most steps take minutes; the dry-run takes the longest.

---

## 0. Prerequisites (once)

| Tool | Check |
|---|---|
| Rust (MSVC toolchain) | `rustup default stable-x86_64-pc-windows-msvc`, `cargo --version` |
| Git for Windows | `git --version` |
| PowerShell 7 | `pwsh --version` |
| Claude Code ≥ 2.1.259 | `claude --version` (logged in with `/login` for the Max profile) |
| Codex CLI | `codex --version`; `codex login` done with `CODEX_HOME` = the profile's folder if you use it |

Build (from the `toryo` folder):

```powershell
cargo build --release
$env:PATH = "$PWD\target\release;$env:PATH"   # or copy toryo.exe somewhere on PATH
toryo --version
```

Run the unit + integration tests once on Windows too (the Job Object code only runs here):

```powershell
cargo test --release
```

## 1. Configuration

```powershell
mkdir C:\.thien\m0 -Force; cd C:\.thien\m0
toryo spike init-fixture --into C:\.thien\m0\calc --contracts C:\.thien\m0\contracts
Copy-Item <toryo>\templates\toryo.yaml.example .\toryo.yaml       # then edit exe paths / Foundry names
mkdir $env:USERPROFILE\.toryo -Force
Copy-Item <toryo>\templates\toryo.local.yaml.example $env:USERPROFILE\.toryo\toryo.local.yaml
```

Secrets (never in YAML; stored in Windows Credential Manager):

```powershell
Read-Host -MaskInput "Foundry OpenAI key" | toryo secrets set toryo/foundry-openai
Read-Host -MaskInput "Foundry Claude key" | toryo secrets set toryo/foundry-claude   # company laptop only
```

Then:

```powershell
toryo doctor
```

Expect FAILs only for "no M0 record" lines at this point (they turn green as you go). Fix every
other FAIL first (exe paths, versions, Git Bash, secrets, `windows_sandbox`).

## 2. Host checks (no tokens)

| # | Command | Answers | Pass means |
|---|---|---|---|
| 2.1 | `toryo spike job-test` | R23 / §13 Job test | stopping one stage kills its whole tree and leaves the other; killing `toryo.exe` from outside kills every owned (pid, creation time); a failed Job assignment never lets the child run |
| 2.2 | `toryo spike git-canary` | **M0-VERIFY 10**, R33, R37 | plain Git runs the booby-trapped hooks/filters/textconv (control), `GitCmd` runs none of them, the trusted `lfs` process driver replaces the repo's, admission refuses an untrusted referenced filter |
| 2.3 | `toryo spike lowil-canary` | **M0-VERIFY 9**, R36 | at Low integrity: writes to the run store, `%LOCALAPPDATA%\toryo`, `%USERPROFILE%\.toryo`, the task worktree and `.git` are denied; build/tmp/verify-checkout writes succeed; the calculator build + tests pass at Low IL; without the boundary everything is writable (control) |

If 2.3 fails only in the build phase, look at `details.build.steps[].stderr` in the record: a tool
that writes to the Medium profile needs one more redirect. Note it — that is exactly the
information v0.7 needs for Open decision 12.

## 3. Claude (Max profile first, Foundry on the company laptop)

```powershell
toryo spike claude-isolation --profile claude-max            # M0-VERIFY 1, 2, 7 + R44b
toryo spike killed-resume   --profile claude-max             # M0-VERIFY 4, 5
```

`claude-isolation` builds a worktree full of traps — `CLAUDE.md` with a codeword, `.claude/settings*.json`
with `env`, `apiKeyHelper`, hooks, a `.mcp.json` server — and asks Claude to read one file in the run
store (must work), one file outside (must be denied) and to report its special instructions (must
show the context-file codeword, never the CLAUDE.md one). Ten checks, all must pass.

If it fails because a flag is rejected, try the fallbacks **in order** and record each:

```powershell
toryo spike claude-isolation --profile claude-max --mode setting-sources
toryo spike claude-isolation --profile claude-max --mode cwd-run-store
```

A fallback is usable only if it passes the same ten checks (R44b). To diagnose a single flag:
`toryo spike claude --profile claude-max --prompt-file p.md --run-store <dir> --drop-flag --permission-prompts`.

Company laptop: repeat both commands with `--profile claude-foundry` (M0-VERIFY 6).

`killed-resume` kills a long turn after 25 s, resumes the session, and runs a fresh short turn
for comparison. Pass = the resume works and the killed tree is gone. The usage numbers are
**data**: read the three `raw` blocks and decide whether Claude's `result.usage` on `--resume` is
per-run or cumulative (M0-VERIFY 4). Write your conclusion in `docs/M0-RESULTS.md`.

## 4. Codex

```powershell
toryo spike codex-canary          --profile codex-foundry     # M0-VERIFY 3 (R18/R27)
toryo spike codex-project-config  --profile codex-foundry     # M0-VERIFY 8
toryo spike killed-resume         --profile codex-foundry     # M0-VERIFY 12 (+ 11 from the raw usage)
```

- **codex-canary**: Codex is asked to write `inside.txt` (must succeed — positive control), write
  `..\outside.txt`, run `git commit`, and append to the run store's `TASK.md` (all must fail).
  If anything escapes, `toryo doctor` keeps the profile FAILED. With `unelevated` failing and the
  one-time elevated setup done, set `windows_sandbox: elevated` and re-run.
- **codex-project-config**: records whether a repo `.codex/config.toml` is loaded despite
  `--ignore-user-config`. Keep `allow_codex_project_config: false` if it says LOADED.
- **killed-resume (Codex)**: turn 1 must be recorded as `unknown` usage (never 0). Compare turn 2's
  raw `input_tokens` with turn 3's: if turn 2 includes the killed turn's tokens, M0-VERIFY 12 = yes.
  From any `raw` block, check M0-VERIFY 11: `cached_input_tokens` ≤ `input_tokens` and
  `reasoning_output_tokens` ≤ `output_tokens`.

## 5. Manual pipeline dry-run (R11 — the second stop rule)

Goal: one task on the calculator repo, by hand, through the real artifact contracts. The question
is only: **are these artifacts good enough to automate?**

```powershell
cd C:\.thien\m0
$rs = "C:\.thien\m0\runstore-calc001"; mkdir $rs -Force
Copy-Item calc\TASK.md $rs\TASK.md
git -C calc worktree add -b toryo/CALC-001 C:\.thien\m0\worktrees\CALC-001 main
$wt = "C:\.thien\m0\worktrees\CALC-001"

# 5.1 Plan (Claude, read-only)
@"
$(Get-Content contracts\roles\planner.md -Raw)

## Task pack
- TASK.md: $rs\TASK.md
"@ | Set-Content $rs\context-plan.md
"Create the plan for the task in $rs\TASK.md." | Set-Content $rs\prompt-plan.md
toryo spike claude --profile claude-max --label planner --worktree $wt --run-store $rs `
  --prompt-file $rs\prompt-plan.md --context-file $rs\context-plan.md `
  --schema contracts\schemas\plan.schema.json --out $rs\plan.json
toryo spike render --kind plan $rs\plan.json > $rs\PLAN.md     # read it: would you approve it?

# 5.2 Implement (Codex, sandboxed)
@"
$(Get-Content contracts\roles\implementer.md -Raw)

## Task pack (read-only)
- TASK.md: $rs\TASK.md
- PLAN.md: $rs\PLAN.md
"@ | Set-Content $rs\prompt-impl.md
toryo spike codex --profile codex-foundry --label implementer --worktree $wt `
  --prompt-file $rs\prompt-impl.md --schema contracts\schemas\implementation.schema.json --out $rs\implementation.json
git -C $wt status --short                                        # expect edits, NO commit
git -C $wt add -A; git -C $wt commit -m "toryo(CALC-001): implement attempt 1" --no-verify   # you play the orchestrator
toryo spike diff --repo $wt --from main --to HEAD --out $rs\DIFF.patch

# 5.3 Verify (by hand in M0)
pwsh -NoProfile -File $wt\scripts\build.ps1; pwsh -NoProfile -File $wt\scripts\test.ps1
#   write the result as VERIFY_RESULT.json by hand: {"head":"<sha>","pass":true,"commands":[...]}

# 5.4 Review (Claude, read-only)
@"
$(Get-Content contracts\roles\reviewer.md -Raw)

## Task pack
- TASK.md: $rs\TASK.md
- PLAN.md: $rs\PLAN.md
- DIFF.patch: $rs\DIFF.patch
- VERIFY_RESULT.json: $rs\VERIFY_RESULT.json
"@ | Set-Content $rs\context-review.md
"Review the checkpoint in this worktree against TASK.md." | Set-Content $rs\prompt-review.md
toryo spike claude --profile claude-max --label reviewer --worktree $wt --run-store $rs `
  --prompt-file $rs\prompt-review.md --context-file $rs\context-review.md `
  --schema contracts\schemas\review.schema.json --out $rs\review.json
toryo spike render --kind review $rs\review.json > $rs\REVIEW.md
```

Judge, and note in `docs/M0-RESULTS.md`:

- Did the plan list the right files and no protected ones?
- Did Codex stay in the worktree, not commit, and return valid `implementation.json`?
- Does `review.json` contain **exactly** AC-1, AC-2, AC-3 with real evidence?
- Anything in the role templates or schemas you would change → change the contracts **before M1**
  (second stop rule).

## 6. Close M0

```powershell
toryo spike report            # also writes %LOCALAPPDATA%\toryo\m0\M0-RESULTS.generated.md
toryo doctor                  # everything that M0 covers should now be OK or explained
```

Fill in `docs/M0-RESULTS.md` (verdict per item + your hours per step, for the R43 re-estimate),
copy useful captures to `fixtures/captured/`, and hand both files over for plan v0.7.

## Cleanup

Everything lives under `%LOCALAPPDATA%\toryo\m0\` and `C:\.thien\m0\`. The Low-labelled folders
are under `…\m0\runs\*-lowil-canary-*\` — delete them like any other folder.
