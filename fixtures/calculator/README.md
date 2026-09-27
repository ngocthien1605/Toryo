# Calculator fixture

Tiny PowerShell 7 repository used by the Thien Orchestrator M0 spike:

- manual pipeline dry-run (TASK.md → plan.json → implement → DIFF.patch → review.json),
- Low-integrity verify canary (`thien spike lowil-canary`, M0-VERIFY 9),
- Codex sandbox canary (`thien spike codex-canary`, M0-VERIFY 3).

`thien spike init-fixture --into <dir>` materialises it as a Git repository with a `main` branch.
