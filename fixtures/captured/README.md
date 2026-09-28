# Captured real-protocol fixtures

Real streams recorded during M0 on Windows (Claude Code 2.1.283, Codex CLI 0.157.1), with the
Windows user name replaced by `user`. They are replayed through the real parsers by
`FakeAgentAdapter` (`{"kind":"replay", …}` steps) and by `tests/adapters.rs`.

| File | Run | What it shows |
|---|---|---|
| `claude-review-changes-requested.jsonl` | BR-002 review 0 | Claude reviewer (read-only), `changes_requested` with unmet AC-2/AC-4 and two blocking issues |
| `claude-implementer.jsonl` | BR-001-fb implement | Claude as fallback Implementer (R49): Edit + Write, no permission denials |
| `codex-implement.jsonl` | BR-001 implement | Codex exec: commands, file changes, structured implementation output, exact usage |
| `codex-fix.jsonl` | BR-002 fix-1 | Codex fix cycle reporting `resolved` issue ids |

Add new streams per pinned CLI version (plan §16). Secrets are redacted at capture time (R42),
but review a stream before committing it.
