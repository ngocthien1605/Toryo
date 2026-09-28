# M0 findings

> **Note (2026-09-28):** the tool was renamed from `thien` to `toryo` (plan v0.7, R55). This record keeps the
> names, commands and paths exactly as they were used during the M0 runs (`thien …`, `THIEN_*`, `thien-m0-data`).

F1–F6 came from implementing and testing the kit on Linux (Git 2.43.0, Rust 1.95). F7–F13 came from
the Windows runs on 2026-09-27/28 (see `M0-RESULTS.md`). All of them feed plan v0.7.

## F1 — An empty `process` override also disables `clean`/`smudge` (affects R37)

Git's `convert.c` uses `clean`/`smudge` only when `filter.<d>.process` is **NULL**, and runs
`process` only when it is **non-empty**. `-c filter.<d>.process=` sets it to the empty string
(not NULL), and `-c` cannot unset a key. So:

- neutralising a driver with empty `process`/`clean`/`smudge` works (canary: no marker);
- but a trusted driver defined only by `clean`/`smudge` can never be enforced — the empty `process`
  override (needed to block the repo's own `process`) switches it off.

**Consequence:** a trusted filter entry must define `process` (git-lfs does: `git-lfs filter-process`).
`GitCmd::plan_filters` now refuses clean/smudge-only trusted entries. Plan §8/§14 need one sentence.

Evidence: `tests/git_canary.rs` (variants `neutralized`, `trusted-lfs` with a pkt-line process filter
built into `thien spike filter-process`), and the negative control showing plain Git runs 7 hooks,
the repo's `lfs` process filter, `evil` clean filter, textconv, `diff.external` and fsmonitor.

## F2 — M0-VERIFY 10 is answered for Git 2.43 on Linux

All four canary variants pass. Re-run `thien spike git-canary` with Git for Windows before closing
the item (hooks and filters run through Git's bundled `sh` there).

**Windows (2026-09-27/28):** confirmed with Git 2.55.0.windows.5, 4/4 variants. Closed.

## F3 — To confirm on Windows: does the Low label reach the verify checkout's `.git` pointer file? (B1)

`SetNamedSecurityInfoW` with an inheritable label is expected to propagate to existing children, which
would make the verify checkout's `.git` file writable by verify code — the v0.6 self-review finding B1.
`lowil-canary` records the observed behaviour as an *info* row and checks that the orchestrator's
explicit `--git-dir/--work-tree` calls still see the real admin directory either way.

**Windows (2026-09-28):** confirmed. The `.git` pointer file in the Low-labelled verify checkout **is
writable** at Low IL, while `.git/worktrees/wt.verify` (the admin dir) is not. The tracked tree was
unchanged after the build, as seen through explicit `--git-dir`. B1 is therefore real, and the
explicit `--git-dir` rule is required, not optional.

## F4 — The §13 environment allowlist is probably too narrow for Windows tools

The kit adds `SystemDrive`, `ProgramData`, `ProgramFiles*`, `CommonProgramFiles`, `USERNAME`,
`USERDOMAIN`, `HOMEDRIVE`/`HOMEPATH`, `PROCESSOR_ARCHITECTURE`, `NUMBER_OF_PROCESSORS` and `OS`
(none selects a provider or carries a secret). If the first real turns work without them, v0.7 can
drop them again; if Git Bash, Node or .NET misbehave, this list is the first place to look.

## F5 — `codex exec resume`: options are placed before the subcommand

The plan's argv puts `resume` right after `exec`. The kit puts every option before
`resume <thread> -`, which clap accepts whether or not an option is `global`; after `resume` only
global ones are accepted, and `--ignore-user-config` / `--ignore-rules` were not confirmed global.
`killed-resume --profile codex-foundry` shows whether this form works; v0.7 should state the final form.

**Windows (2026-09-28):** `killed-resume --profile codex-openai` resumed successfully with every option
before `resume <thread> -`. v0.7 should use this form.

## F6 — Claude isolation: `session_id` proves nothing about auth

`system/init` (which carries `session_id`) arrives before any API call. The isolation probe therefore
bases "authenticated" and "repo `env` not applied" on the model's structured answer, and adds a
positive control showing its marker commands can run at all.

---

# Findings from the Windows runs (2026-09-27/28)

## F7 — A Low label cannot be set on folders created directly under `C:\` (affects R36)

`lowil-canary` failed with `SetNamedSecurityInfoW … Win32 error 5` when the data dir was
`C:\.thien\AgentOrchestrator\thien-m0-data`. It passed completely when run under `%LOCALAPPDATA%`.

Cause: `icacls` shows only ACEs inherited from `C:\` (Administrators F, SYSTEM F, Authenticated Users M,
Users RX), and there is no Full Control ACE for the user. With UAC the user's `Administrators`
membership is *deny only*, so only **Modify** applies. Modify lacks `WRITE_OWNER`, which setting a
mandatory label requires. This will happen on any default Windows setup where the verify root is
below `C:\<folder>`.

**Consequence / done in the kit:**

- `verify.verify_root` stays under the user profile (default `%LOCALAPPDATA%\thien-verify`). Worktrees
  and repos can live anywhere, because the clean verify checkout (R29) is created inside `verify_root`.
- `lowil-canary` now puts the verify checkout and its build/cache/tmp/home under `verify_root`. It also
  records a FAIL row when it errors (before, the error left no record) and explains the `WRITE_OWNER` cause.
- `thien doctor` probes `verify_root` by setting a Low label on a scratch dir. It reports FAIL when this
  is impossible and the boundary is `low_integrity`.
- Plan §8 / §14 need one sentence: "verify_root must be a location where the account has WRITE_OWNER".

## F8 — The Codex sandbox cannot write `%TEMP%`

In BR-001, Codex had to set `$env:TEMP = (Get-Location).Path` before the tests could create temp
folders (`New-TestFolder` uses `GetTempPath()`). `workspace-write` only allows the worktree.

**Done in the kit:** `spike pipeline` gives every implementer turn `TEMP`/`TMP` =
`<worktree>\.thien-tmp`, recreated before each turn. The folder is excluded through the common
`info/exclude`, so it never reaches a checkpoint. `TurnOpts.env` carries per-turn environment.
Plan §13 (Codex env) should list it.

**BR-002 (after the change):** the turn's env had `TEMP=<worktree>\.thien-tmp`, yet Codex still ran
`$env:TEMP = (Get-Location).Path` before the tests, in both implement and fix-1. There are two possible
reasons. Either Codex's sandbox or shell environment policy does not forward `TEMP` to its commands,
or the model redirects TEMP by habit. **To do:** add an `echo $env:TEMP` probe to `codex-canary`, and add
one line to the implementer prompt: "TEMP/TMP already point to a writable folder in the worktree".

## F9 — Resume is not free

| | fresh probe | resumed after kill |
|---|---|---|
| Claude | 4 856 tokens | 9 088 (8 950 cache read) |
| Codex | 14 922 | 47 125 (31 616 cached) |

The resumed turn re-reads the killed turn's transcript. For Claude most of it is cheap cache reads.
For Codex the resumed turn is about 3× a fresh one, and the killed turn's own consumption is unknown.

**Consequence for M3:** for crash recovery, compare "resume" with "fresh turn + hand-over note" (the
fallback path already builds one). Default proposal: resume Claude sessions; for Codex, start fresh
with a hand-over when the killed turn ran for more than a few minutes. The budget (M7) must reserve for
the unknown killed turn (R41 reservations).

## F10 — `CodexSandboxUsers` ACEs are inherited across the whole working folder

`icacls` on a folder under `C:\.thien\AgentOrchestrator` shows inherited `MSI\CodexSandboxUsers:(M)`
plus three unresolved `S-1-5-21-…:(M)` SIDs. Codex's Windows sandbox setup most likely added them to an
ancestor folder when Codex ran there. Despite that, `codex-canary` in the same tree blocked the outside
write. The sandbox therefore does not rely on that group ACE alone.

**Consequence:** no action now. Re-run `codex-canary` after every Codex upgrade (add this to `doctor`
evidence age checks), and keep the Low-IL verify root outside folders Codex has used as a workspace.

## F11 — pwsh colours warnings even with `NO_COLOR`

Captured build/test output contained `ESC[33;1mWARNING…ESC[0m`. **Done:** `process::strip_ansi` removes
CSI/OSC sequences from every stored verify/build output (pipeline, smoke, lowil).

## F12 — Claude works as a file-tools-only implementer (fallback)

With `--restricted --safe-mode`, `--tools Read,Grep,Glob,Edit,Write`, `--allowedTools Read,Grep,Glob`,
`--permission-mode acceptEdits`, `--permission-prompts none`, cwd = worktree and **no** `--add-dir`
(task pack inlined), Claude used Edit + Write with 0 permission denials. It implemented BR-001 in 47 s,
and the result was approved on the first review (BR-001-fb). This confirms the fallback design. The reviewer
is then also Claude (fresh session). The pipeline logs this and prints a warning.

## F13 — First real pipeline numbers (for R43)

| Run | Turns | Wall-clock | Tokens | Result |
|---|---|---|---|---|
| BR-001 (Codex implements, plan rejected once) | plan ×2 (56 s, 41 s), implement 119 s, review 26 s | ~10 min incl. human gate | 193 k (62 % cache read); Codex implement 102 k | approved, 0 fix cycles |
| BR-001-fb (Claude implements) | plan 53 s, implement 47 s, review 24 s | ~2 min | 143 k | approved, 0 fix cycles |
| BR-002 (Codex, one real fix cycle) | plan 65 s, implement 141 s, review 78 s, fix-1 96 s, review 21 s | ~8 min | 477 k (75 % cache read); the fix cycle cost ≈ 218 k (46 %) | approved after fix-1 |

- Plan gate: the rejection reason reached Claude through `--resume` of the planner session, and the
  revised plan added the requested test.
- Review quality: the evidence cites diff lines and test names. The review also raised one non-blocking
  low issue (I-1). Such issues are now listed in `SUMMARY.md` as follow-ups.
- **Fix cycle with real agents (BR-002):** the implementation and its own tests passed verify, yet the
  reviewer requested changes. It had worked out that the expected order in the test contradicted
  `OrdinalIgnoreCase` ('_' sorts after 'Z'), so the test could only pass if the sort did not run. Codex
  reproduced the bug in fix-1. PowerShell had bound `[Array]::Sort($keys, $files, …)` to an overload that
  sorted a copy, so `$files` stayed unsorted. Codex fixed it with explicit casts, corrected the test,
  added a culture-vs-ordinal case and reported `resolved: AC-2, AC-4, I-1, I-2`. Review 2 approved it.
  This is exactly the defect class the verify step alone cannot catch (a test agreeing with a bug), and
  it justifies the independent reviewer.
