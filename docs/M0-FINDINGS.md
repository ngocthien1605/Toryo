# Findings from building the M0 kit (before any Windows run)

These came from implementing and testing the kit on Linux (Git 2.43.0, Rust 1.95). They should be
confirmed on Windows and folded into plan v0.7.

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

## F3 — To confirm on Windows: does the Low label reach the verify checkout's `.git` pointer file? (B1)

`SetNamedSecurityInfoW` with an inheritable label is expected to propagate to existing children, which
would make the verify checkout's `.git` file writable by verify code — the v0.6 self-review finding B1.
`lowil-canary` records the observed behaviour as an *info* row and checks that the orchestrator's
explicit `--git-dir/--work-tree` calls still see the real admin directory either way.

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

## F6 — Claude isolation: `session_id` proves nothing about auth

`system/init` (which carries `session_id`) arrives before any API call. The isolation probe therefore
bases "authenticated" and "repo `env` not applied" on the model's structured answer, and adds a
positive control showing its marker commands can run at all.
