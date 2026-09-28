# M0 results (input for plan v0.7)

Machine: MSI · Windows 10.0.26200 (Windows 11) · PowerShell 7.6.6 · Git 2.55.0.windows.5 ·
Claude Code 2.1.283 (Max subscription) · Codex CLI 0.157.1 (ChatGPT login) · thien 0.0.1 ·
Runs: 2026-09-27 and 2026-09-28 (`thien-m0-data\m0\results.jsonl`, `thien-m0-data\runs\`).

Account: standard user in a UAC-filtered admin token (`BUILTIN\Administrators` = *deny only*),
Medium integrity. That fact explains F7.

| # | Question (plan §17) | Spike | Verdict | Evidence | Consequence for v0.7 |
|---|---|---|---|---|---|
| 1 | `--restricted --safe-mode` authenticates with Max; honours `--append-system-prompt-file` + `--json-schema` | `claude-isolation --profile claude-max` | **PASS** | 2026-09-28T04:22:24Z, 10/10 checks | Keep the R30 launch line as the primary mode. |
| 2 | `CLAUDE.md` absent in that mode | same | **PASS** | same record ("CLAUDE.md not loaded") | — |
| 3 | Codex sandbox blocks outside write, `git commit`, run-store write | `codex-canary` | **PASS** | 04:23:00Z, `windows.sandbox=unelevated`; outside write, commit and run-store append all failed, inside write worked | Keep `unelevated`. See F8 (TEMP) and F10 (ACEs). |
| 4 | Claude `result.usage` on `--resume`: per-run or cumulative | `killed-resume --profile claude-max` | **Per run** | 04:23:53Z: resumed 9 088 (cache read 8 950) vs killed turn provisional ≈ 20 226 | Store the resumed run's usage as-is. The killed turn stays *provisional* (R41). |
| 5 | Claude `--resume` after `TerminateJobObject` continues cleanly | same | **PASS** | `tree_gone=true`, `resume_success=true` | Resume is usable for crash recovery (M3). See F9 for the cost. |
| 6 | Foundry credentials under `--restricted --safe-mode` | `claude-isolation --profile claude-foundry` | **Not run** | No Foundry profile on this machine | Leave open. Run it before a Foundry profile is used. |
| 7 | `--add-dir` run store readable; outside Read denied; no settings from `--add-dir` | `claude-isolation` | **PASS** | Same record: run store read, outside Read in `permission_denials`, repo settings env and hooks not applied | — |
| 8 | Repo `.codex/config.toml` loaded with `--ignore-user-config`? | `codex-project-config` | **Not loaded** (good) | 04:23:20Z: the probe variable was absent | A repo cannot loosen the sandbox through `.codex/config.toml`. `allow_codex_project_config` can stay off by default. |
| 9 | Low-IL verify boundary works as designed; fixture builds at Low IL | `lowil-canary` | **PASS** with `verify_root` under `%LOCALAPPDATA%` | 04:58:32Z: 6/6 deny, 3/3 allow, negative control, build + tests at Low. It failed at 04:24:37Z with the data dir under `C:\.thien` (F7). | `verify_root` must be under the user profile (default `%LOCALAPPDATA%\thien-verify`). `doctor` now checks this. |
| 10 | Empty/trusted `-c filter.<d>.*` overrides stop repo filter commands | `git-canary` | **PASS** | 2026-09-27 and 2026-09-28, Git 2.55.0.windows.5, 4/4 variants | F1 still holds: a trusted filter must define `process`. |
| 11 | Codex usage subsets (`cached ⊂ input`, `reasoning ⊂ output`) | Codex raw usage | **cached ⊂ input: consistent**; reasoning: not observable | Resumed turn: input 47 125, cached 31 616. Reasoning was 0 in every turn. | Keep `cached_in_input=true`. Re-check reasoning on a model that reports it. |
| 12 | Codex resumed total includes the killed turn | `killed-resume --profile codex-openai` | **Resumed turn re-reads the killed context** | 04:24:37Z: fresh 14 922 vs resumed 47 125 (31 616 cached). The killed turn has no live usage (`unknown`). | The killed Codex turn stays `unknown` (R41). A resume costs about 3× a fresh probe (F9). |
| — | Job Object tests (stop / crash / assign failure) | `job-test` | **PASS** | Both days, 3/3 | — |
| — | Launch-mode isolation for every mode a profile uses (R44b) | `claude-isolation --mode …` | **PASS** for `restricted` | Only mode used | Run the fallback modes only if they are ever needed. |
| — | Manual dry-run: artifacts good enough to automate? | `smoke` + `spike pipeline` (BR-001) | **Yes** | Smoke 9/9 (09-27). BR-001 and BR-001-fb were both approved on the first review with concrete evidence (F12, F13). | R11 (second stop rule) satisfied. |

**Stop rule:** every M0-VERIFY item except #6 (Foundry, not applicable here) has a Windows answer,
so M0 is complete. Next steps: re-estimate for v0.7 (R43), then M1.

## Hours spent (for the R43 re-estimate)

Wall-clock times of the runs, from the records. The human hours are Thien's to fill in.

| Step | Wall-clock of runs | Hours (human) |
|---|---|---|
| Setup + doctor (incl. installing Git, 3 script runs) | ~25 min over 3 attempts | |
| Host checks (job-test, git-canary, lowil ×2) | ~3 min | |
| Claude spikes (isolation, killed-resume) | ~2 min | |
| Codex spikes (probe, canary, project-config, killed-resume) | ~3 min | |
| Smoke + pipeline runs (smoke, BR-001, BR-001-fb) | ~15 min | |
| Kit development (plan v0.6 → M0 kit → pipeline prototype) | — | |
| **Total M0** | | |

## Findings

See `M0-FINDINGS.md`: F1–F6 came from building the kit, F7–F13 from the Windows runs.
