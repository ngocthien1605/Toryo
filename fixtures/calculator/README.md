# Calculator fixture

Tiny PowerShell 7 repository used by the Toryo M0 spike:

- manual pipeline dry-run (TASK.md → plan.json → implement → DIFF.patch → review.json),
- Low-integrity verify canary (`toryo spike lowil-canary`, M0-VERIFY 9),
- Codex sandbox canary (`toryo spike codex-canary`, M0-VERIFY 3).

`toryo spike init-fixture --into <dir>` materialises it as a Git repository with a `main` branch.
