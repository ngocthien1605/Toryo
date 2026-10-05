# M11 notes — the short version

M11 (org chart mode, role files, skills, the write fence, check and environment gates) is merged into `main`. The full record —
what was built per task, which tests pin it, five review rounds — is in [`archive/M11-NOTES.md`](archive/M11-NOTES.md)
(2,300 lines; **do not read it end to end**: search it by task id, `R13`, `X9`, a file name). The open items that were in its five
"Open" lists are one list, with priorities, in [`STATUS.md`](STATUS.md); the decisions X1–X16 are in
[`DECISIONS.md`](DECISIONS.md) (X2 replaced by X6; X7 refined by X9 and X10).

## Commits

| Merged as | Range | What |
|---|---|---|
| pull request #3, `df84b02` | `b11dad5` … `e3a199e` | **M11 and rounds 1–2.** A: the trust floor (review C1: no machine-only keys or YAML tags in the repository file; H2: a machine floor for `verify.boundary` and the machine's `verify_root`). B: org chart mode (`.toryo/toryo-orgchart.yaml`, role files, `toryo config show`). C: layered role instructions and culture, the write fence (`writes` / `never`), `reviewer-differs-from-builder`, split limits. D: skills (repository, then machine; the planner's menu). E: gate variables, M18 entry-script protection, verify output redacted with the task's secrets (M2), environment gates with a machine-wide lock and a deployment record. F: the D365FO org chart template, README, HUONG-DAN §5.9. R1–R12: review fixes. |
| pull request #4, `4ad97c3` | `59fded3` … `a2563d9` | **Rounds 3–5** (`m11/review-3`): R13 byte-exact export, a Low-writable verify root refused; R14 M18 protection from the base commit (X9); R15 command strings read (X10); R16 an environment gate's needs checked before the build (X12); R18 a folder as entry; R19 "refusing needs certainty, protecting does not" (X13); R20 the export check compares bytes (X15); R21 Low fixes; R22 redirection targets; R23 case folding of the export (X16); R24 Low fixes. |

Tests: 570 at `df84b02`, **623 passed, 0 failed, 6 ignored** at `4ad97c3` (Windows). Nothing ran against a D365FO environment and no
real agent was run during M11.

## What changed for a repository without an org chart

Most of M11 is org chart mode and does not touch a `toryo.yaml`-only repository. These do, for everyone, and a configuration keeps its
hash unless it has a build-output entry script:

- **Machine floor (review H2).** A repository `verify.boundary: none` is refused at `task create` and `task config --reload` unless
  the machine config says `verify: { min_boundary: none }`; the machine's `verify.verify_root` replaces the repository's.
- **M18 entry scripts** (X7, X9, X10, X13, X14): a gate's entry script is protected before it exists and `task create` refuses a
  configuration whose base commit lacks it (`verify_script_missing`); command strings are read; every other word that names a file of
  the base commit is protected, never refused. A script missing at base **and ignored** (`node dist/cli.js`) is build output: recorded
  with the task (`build_entries`), never committed. A task created before this gets it after `toryo task config <id> --reload`.
- **Verify output is redacted** with the task's secrets (M2) — also colour-code-split ones.
- **Machine-caused escalations** (X8): a plain `toryo resume <id>` goes back into the stage, no fix cycle.
- **The export check** is byte-exact (X15) and folds case on Windows (X16); a repository with `text` / `eol=crlf` over blobs that
  hold CRLF needs `git add --renormalize .` once.
- **Same-vendor review** counts every implementer turn that ran (X4), so the note appears in more cases than in M10.

Where M12 changed these again: [`M12-NOTES.md`](M12-NOTES.md).
