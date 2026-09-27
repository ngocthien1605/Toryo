# M0 results — fill in after the runbook (input for plan v0.7)

Machine: ______ · Windows build: ______ · Git: ______ · Claude Code: ______ · Codex: ______ · Date: ______

| # | Question (plan §17) | Spike | Verdict | Evidence (record ts / run dir) | Consequence for v0.7 |
|---|---|---|---|---|---|
| 1 | `--restricted --safe-mode` authenticates with Max; honours `--append-system-prompt-file` + `--json-schema` | `claude-isolation --profile claude-max` | | | |
| 2 | `CLAUDE.md` absent in that mode | same | | | |
| 3 | Codex sandbox blocks outside write, `git commit`, run-store write | `codex-canary` | | | |
| 4 | Claude `result.usage` on `--resume`: per-run or cumulative | `killed-resume --profile claude-max` | | | |
| 5 | Claude `--resume` after `TerminateJobObject` continues cleanly | same | | | |
| 6 | Foundry credentials authenticate under `--restricted --safe-mode` | `claude-isolation --profile claude-foundry` | | | |
| 7 | `--add-dir` run store readable; outside Read denied; no settings from `--add-dir` | `claude-isolation` (+ `--mode cwd-run-store` if used) | | | |
| 8 | Repo `.codex/config.toml` loaded with `--ignore-user-config`? | `codex-project-config` | | | |
| 9 | Low-IL verify boundary: writes denied/allowed as designed; fixture builds at Low IL | `lowil-canary` | | | |
| 10 | Empty/trusted `-c filter.<d>.*` overrides stop repo filter commands | `git-canary` | | | see F1 below |
| 11 | Codex usage subsets (`cached ⊂ input`, `reasoning ⊂ output`) | any Codex `raw` usage | | | |
| 12 | Codex resumed total includes the killed turn | `killed-resume --profile codex-foundry` | | | |
| — | Job Object tests (stop / crash / assign failure) | `job-test` | | | |
| — | Launch-mode isolation for every mode a profile uses (R44b) | `claude-isolation --mode …` | | | |
| — | Manual dry-run: artifacts good enough to automate? | runbook §5 | | | |

## Hours spent (for the R43 re-estimate)

| Step | Hours |
|---|---|
| Setup + doctor | |
| Host checks | |
| Claude | |
| Codex | |
| Dry-run | |
| **Total M0** | |

## Findings already known from building the kit

See `M0-FINDINGS.md` (F1–F6).
