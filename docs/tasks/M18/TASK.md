# M18 — The command-line reader upgrade and the trust-boundary fixes of the review of 2026-11-05

Milestone M18 (repository numbering, after M17). Task ids in commits: `M18-A1` … (Part A, the command-line reader),
`M18-B1` … (Part B, the trust edges), `M18-Z1` … (closing).

Two names to keep apart:

- **"The M18 reader"** or **"the M18 rules"** is what the code and `docs/DECISIONS.md` call the entry-script
  protection: `src/engine/cmdline.rs` and the entry-script code of `src/engine/checks.rs` (X7, X9, X10, X13, from the
  plan's numbering). This milestone **upgrades** it. It is the review round that `docs/STATUS.md` §3 asks for before
  those rules change.
- **Review M18** of `docs/REVIEW-2026-10-01.md` is a finding of that review. Always write it "review M18".

This file is the full specification. It says **what** to build and when it is done. How the session works is the
master prompt, `docs/tasks/M18/MASTER-PROMPT.md`. The acceptance-criteria section below is in Toryo's own `TASK.md` format (it parses with
`checks::parse_acceptance_ids`), so the same file can later be fed to `toryo task create` for dogfooding.

## Goal

1. **The reader reads the spellings real gate scripts use.** Today a script behind `if`, `while`, `!`, `command`,
   `env` or `timeout` in a `sh` string, behind `start` or `if` in a `cmd` string, or inside `pwsh -CommandWithArgs`
   is read as an **argument**. An argument is protected only when the base commit already has it and is never
   refused. So a gate that names a script the task is meant to add is neither refused at `task create` nor
   protected. The implementer can then write its own gate, which is the hole X7 closed. After M18 the head of every
   such command is found (T5, T6, O-41). Where the reader still stops, it says so (O-42).
2. **The trust edges the review found are closed.**
   - The verify record decides which folder is reset, cleaned and deleted (T7, fixed **first**).
   - The checkpoint commits more than the path policy saw (T4).
   - The worktree's `.git` pointer decides which Git metadata is protected (T2).
   - Packed replace refs are not protected (T1).
   - Nested `.git` entries are skipped (T3).

   Each fix covers the paths M17 added: candidate worktrees, candidate checkpoints, candidate and red verify records.
3. **The reader gets property tests** (N-10), so that the next round is cheaper than this one.

Analogy for the reader: the M18 reader is the customs officer who reads the bill of lading (the gate's command line)
to find the cargo that will actually be run (the entry script) and seal it before anyone can swap it. Today the
officer misreads documents in a few common layouts (`if ./check.sh; then`), and the real cargo goes through unsealed.
Part B is the warehouse: the officer's own paperwork (the verify record, the `.git` pointer) must not be something the
crew can rewrite.

## Owner decisions (proposed — to confirm in the session's one question round)

Unlike M17, these were **not** asked before writing this file. They are the recommended defaults. The session asks
its one round of questions about any it cannot keep. Then they are settled: record them in `docs/DECISIONS.md` as X64
onward (one line each), with the code location once it exists.

| # | Decision (proposed) |
|---|---|
| D-1 | **Scope**: the review's T1–T7, O-41's wrappers (`env`, `timeout`, `nice`, `nohup`, `!`), `pwsh -EncodedCommand`, interpreters and `make -f` / `nmake /f` inside a command string, O-42's silent stops, and N-10's property tests for `cmdline.rs`. Not O-41's long tail of other programs: the net of X13 stays the answer there. |
| D-2 | **Refusal stays certain** (X10, X13): a new reading may add entries. An entry is refused only when it is the head of a command and clearly names a script (`./`, `../`, a script extension), as today. Everything else is protected, never refused. |
| D-3 | **Existing tasks are not refused mid-task.** A task created before M18 gets the wider protection on its next engine start, because it is computed from the base commit (X9). Only `task create` and `task config --reload` refuse. |
| D-4 | **T7**: the verify record is checked, not trusted. Its folder must be the slot's own and its admin folder an entry of the repository's `worktrees/` that points back at it, else a new checkout is made. `task cleanup` deletes only `<verify_root>/<task>`. No artifact registration of the record (it changes on every verify). |
| D-5 | **T4**: the checkpoint is checked **after** the commit: the path policy is asked of `changed_between(anchor, sha)`, and a violation soft-resets to the anchor. This guards every way a path can enter the commit; staging an exact pathspec would only narrow one of them. |
| D-6 | **T2**: a worktree's Git directories come from the **main repository** (`git -C <repo> worktree list --porcelain`), never through the worktree's `.git` pointer. They are recorded at creation (store schema **5**), and every Git call on that worktree uses them (`Loc::Explicit`). The pointer's bytes are protected state. |
| D-7 | **No new dependency**: the property tests use a seeded generator written in the test file, not `proptest` or `rand`. |
| D-8 | The session runs in **Claude Code on the web** (Linux container); Windows behaviour is proven by GitHub CI (`windows-latest`). Delivery: commit per task, push, **one pull request** to `main`, CI green. **Never merge.** |

Decisions the session takes alone (where this file is silent) follow the master prompt: choose the conservative
option, write it down as an X-decision, and list it in `docs/M18-NOTES.md` under "Decided without the owner".

## Background: where things are today (read before coding)

- **The review**: `docs/REVIEW-2026-11-05.md` has T1–T7, each with a reproduction and a proposed fix.
  `tests/review_2026_11_05.rs` holds eight `#[ignore]` tests that fail at their own assertion. On the M17 branch
  (2026-11-05), `cargo test --locked --test review_2026_11_05 -- --ignored` gives 0 passed, 8 failed. The review asks
  each fix commit to remove its test's `#[ignore]`.
- **The reader in a command string**: `src/engine/cmdline.rs`; its module comment is the rule book.
  - `simple` takes the head after the prefixes `call`, `exec`, `time`, `source`, `.` and assignments (`do` / `else`
    for `cmd` only): about line 589.
  - `CMD_BUILTINS` (line 105) contains `start` and `if`: "a bare head that is one of them runs no file".
  - The caps are `NESTING` = 6 (line 777), `MAX_WORDS` = 4096 and `MAX_FOLDERS` = 32 (lines 834–835). `everywhere`
    (line 788) is the net of X13, which only ever adds names to protect.
- **The reader outside a command string**: `src/engine/checks.rs`.
  - `entry_scripts` (line 1298), `INTERPRETERS` (line 993) and `SH_FAMILY` (line 1058).
  - `is_command_flag` (line 1112) knows `-command`, `-c`, `/command` and `/c`, and `cmd` knows `/c` and `/k`
    (line 1151).
  - `verify_files_at_base` (line 834) protects arguments the base commit has.
  - `with_red_green` (line 735) is M17's X47: the red-green run's args are read as one more verify command, with
    `{test}` an empty word.
- **The rules in force**: X7 (an entry script is protected before it exists, and refused when the base lacks it), X9
  (decided from the base commit), X10 (only heads are entries; what cannot be followed is not refused) and X13
  (refusing needs certainty, protecting does not). The export comparison of `verify.rs` (X15) is **not** part of this
  round: leave it untouched.
- **T7, the verify record**: `src/engine/verify.rs`.
  - The record is `verify-checkout.json` in the run store (`record_path`, line 287).
  - `prepare` and `prepare_in` (lines 409 and 422) take it when its folders exist. They then write its pointer, check
    out `--force` and run `clean -ffdx` with the record's `admin` and `dir`.
  - M17 added `verify::Slot`, the records `verify-checkout-c<k>.json` (candidates) and the red checkout's record.
  - `cleanup` (line 1241) also derives the folder to delete from those records when the task's own record is gone
    (line 1266).
- **T4, the checkpoint**: `src/engine/runner/checkpoint.rs`.
  - It asks `changed()` → `violates()` (lines 85–92), then `nothing_to_commit()` (line 105, which runs
    `git add -A`) and `commit()` (line 115, which runs it again). After the commit only `protect::check` runs
    (line 142).
  - M17's candidate checkpoint (`runner/fanout.rs` `candidate_checkpoint`) has the same order.
  - Recovery's partial checkpoint (`recovery.rs`) and the promotion (`fanout.rs` `promote`, which makes no commit but
    moves the branch) are the other places a checkpoint SHA appears.
- **T2, T1, T3, the protected-state scan**: `src/engine/protect.rs`.
  - `git_meta` (line 107) finds the common and own Git directory with `rev-parse` from the worktree, that is, through
    its pointer.
  - `replace_refs` (line 163) reads loose `refs/replace/*` only.
  - The scan skips every entry named `.git` (line 209).
  - Every Git call on a task worktree discovers the repository from the worktree (`Loc::Discover` in `worktree.rs`).
  - M17's candidates are scanned by `candidate_protected` (`fanout.rs`) with the same code.
- **The open list** (`docs/STATUS.md`): O-41 and O-42 (the reader's limits), O-34 (review M4 = T2, review M7 = T4) and
  N-10 (property tests for `cmdline.rs`). The review asks for new STATUS rows for T1 and T5–T7.
- **Fixtures**: `tests/common/mod.rs` (`setup_orgchart`, `ORGCHART`, `commit_repo`, fake-agent scripts) and
  `tests/common/m17.rs` (`gate_program`: a `.cmd` on Windows and an `sh` script elsewhere, with native `\` paths for
  `cmd`'s `move` and `del`). The review's own reproductions are in `tests/review_2026_11_05.rs`, including the
  `TORYO_GIT` wrapper of T4.
- **Rules of the house**: `docs/STATUS.md` §3.
  - One task = one commit `area: what (M18-A3)`.
  - Every fix starts with a test that fails before it.
  - Never weaken or delete a test. A review test loses its `#[ignore]` in the commit that fixes it.
  - Never run real `claude` or `codex`.

## Part A — The command-line reader (design)

### A1 `sh` keywords and builtins as prefixes (T5)

The word after `if`, `then`, `elif`, `else`, `while`, `until`, `do`, `!` and `command` is a **head**. The word after
`builtin` is not a file and runs none.

- `sh -c 'if ./scripts/check.sh; then echo ok; fi'` → `scripts/check.sh` is an entry script.
- So are the other forms of the review: `if true; then ./x.sh; fi`, `while ./x.sh; do …; done`, `until …`,
  `command ./x.sh`, `! ./x.sh`.
- A `cd` inside such a block follows the rule of X10: a `cd` that runs only sometimes cannot be followed.

### A2 Wrappers (O-41)

At a head, `env`, `timeout`, `nice` and `nohup` pass the head on to the command they run. Their options are read
exactly:

| Wrapper | Options read | The head is |
|---|---|---|
| `env` | `-i`, `-0`, `-v`, `-u NAME`, `--`, `NAME=value` | the first other word |
| `timeout` | `-s SIG`, `-k DUR`, `-v`, `--preserve-status`, `--foreground`, then `DURATION` | the word after the duration |
| `nice` | `-n N`, `-N` | the first other word |
| `nohup` | none | the next word |

An option the reader does not know (`env -C`, `env -S`, `timeout --bogus`) **stops** the reading of that command. Its
words become `maybe`, never refused. Wrappers chain: `nice nohup env FOO=1 ./x.sh`.

### A3 `cmd`: `start` and `if` (T5)

- **`start`**:
  - Skip an optional first quoted title (`start "" x.cmd`, `start "t" x.cmd`).
  - Skip the switches `/b`, `/wait`, `/min`, `/max`, `/i`, `/separate`, `/shared`, the priority switches, and
    `/node N`, `/affinity H`, `/machine M`.
  - The next word is the head. `/d <folder>` resolves the head in a literal relative folder and stops the reading
    otherwise.
  - `start` with nothing after it, or a URL, runs no file.
- **`if`**:
  - Skip the condition: `not`, `/i`, `exist <x>` (x is an argument), `errorlevel <n>`, `defined <x>`,
    `cmdextversion <n>`, `<a>==<b>`, and `<a> <op> <b>` with `EQU` `NEQ` `LSS` `LEQ` `GTR` `GEQ`.
  - The command after the condition is a head. `(…)` and `else (…)` blocks read as today.
- Both leave `CMD_BUILTINS`' other members as they are.

### A4 PowerShell: `-CommandWithArgs` and `-EncodedCommand` (T5, O-41)

- **`-CommandWithArgs` / `-cwa`** (PowerShell 7.4, with its abbreviations) is a command flag. Its string is the
  **next argument only**; the arguments after it are arguments.
- **`-EncodedCommand`** and its accepted abbreviations: the value is Base64 of UTF-16LE text. It is decoded and read
  as a `-Command` string. A value that does not decode, or does not decode to UTF-16LE text, stops the reading and is
  never refused.

### A5 Interpreters and makefiles inside a command string (O-41)

In a `sh -c`, `cmd /c` or `pwsh -Command` string, a head that is an interpreter of `INTERPRETERS` (`python`, `node`,
`ruby`, `perl`, `bash x.sh`, `pwsh -File`, …) gets **the same entry rule as outside a command string**. Examples:

- `sh -c 'python scripts/check.py'` → `scripts/check.py` is an entry.
- `make -f <file>` and `nmake /f <file>` make `<file>` an entry: a makefile is code the gate runs.

### A6 Option and interpreter spellings (T6)

- In the `sh` family a `+` option reads like a `-` option (`+o` takes a value): `bash +o posix x.sh` → `x.sh`.
- A versioned interpreter name is its table entry: `python3.12`, `pypy3`, `perl5.36`, `ruby3.3` (one rule per
  family, written as a pattern).
- `cmd /r` reads like `cmd /c`.
- **Unlisted valued flags**: when the first positional word has no path separator and no extension, but a later word
  has a script extension, the later word is the entry (`node --inspect-port 9229 x.js` → `x.js`). It is protected,
  and refusable only by the rules of D-2. The first word is never refused because of this rule.

### A7 The reader says where it stopped (O-42)

A command string that the reader stops reading gives **one note per gate**. The causes are a cap (depth 6,
4096 words, 32 folders), a here-document, a `cd` it cannot follow, and an unknown wrapper option. The note names the
gate and the reason and says that the rest is protected only where the base commit has it. It appears in
`task create`, `task config --reload`, `doctor` and `gate list`. Nothing is refused because of it.

### A8 Property tests (N-10)

A seeded generator in the test file (D-7) builds `sh`, `cmd` and `pwsh` command lines from a small grammar. The
grammar covers separators, blocks, the prefixes and wrappers of A1–A4, quoting, assignments, `cd`, and a script at a
known position. Properties:

1. the script at the known position is an entry, or a stop note of A7 is given for that command;
2. putting a prefix or wrapper of A1–A4 in front of a command never removes one of its entries;
3. nothing the reader returns as refusable is a word that is not a head;
4. the reader terminates within its caps on every input, never panics, and gives the same answer twice.

At least 10 000 cases per shell run in `cargo test`. A failure prints its seed and its command line.

## Part B — The trust edges (design)

### B1 The verify record is checked, not trusted (T7 — first)

- `prepare` and `prepare_in` accept a record only when all three hold:
  - its `dir` is the slot's own folder: `<verify_root>/<task>/<task>.verify`, a candidate's
    `<verify_root>/<task>/c<k>/<task>.c<k>.verify`, or the red checkout's;
  - its `admin` resolves to an entry under the repository's `<common dir>/worktrees/`;
  - that entry's `gitdir` file names `<dir>/.git`.

  Otherwise they make a new checkout, as for a missing record, and record a diagnostic event (`verify_record_refused`).
  Nothing outside the slot is reset, cleaned or detached.
- `task cleanup` deletes **only `<verify_root>/<task>`**, and never a path because a record names it. This also holds
  for M17's candidate and red records.
  - Without a readable configuration (EC-12) it deletes no verify folder and names where one may be.
  - `git worktree remove` runs only for a record that passes the check above.

### B2 The checkpoint commits only what the policy saw (T4)

- After every commit of a checkpoint, the path policy is asked of `changed_between(anchor, sha)`. This covers the
  normal checkpoint, the recovery's partial checkpoint and a candidate's checkpoint. It is the question
  `foreign_commit` already asks of a found commit.
- A violation soft-resets to the anchor, so the changes stay in the tree. The task escalates `policy_violation` with
  the paths; a candidate is disqualified (`disqualified:policy_violation`), as its path policy already does.
- The promotion makes no commit. It is covered because it moves the branch only to a candidate checkpoint that passed
  B2.

### B3 A worktree's Git directories are Toryo's, not the pointer's (T2)

- At `task create` and at a candidate's creation, the worktree's Git directory and the common directory are taken from
  the **main repository**: `git -C <repo> worktree list --porcelain` and `rev-parse --git-common-dir` in `<repo>`.
- They are stored in store schema **5** (`tasks` and `candidates`). A version-4 database is raised on open. A task
  created before M18 gets them, from the main repository, on its next start.
- Every Git call on that worktree uses them (`Loc::Explicit { git_dir, work_tree }`), `git_meta` included.
- The pointer's bytes are protected state (`@git/pointer`). A change is hard: `protected_config_changed`.

### B4 Replace refs as Git lists them (T1)

The `@git/` set gains the replace refs as Git itself lists them (`for-each-ref refs/replace/` through Toryo's Git),
loose, packed and reftable alike. They form one entry, `@git/replace-refs`.

### B5 Nested `.git` entries (T3)

- Only the worktree's own top-level `.git` is skipped; it is B3's `@git/pointer`.
- Any other `.git` file or folder, at any depth, is scanned as protected state. An untracked or ignored one is a hard
  change.
- A gitlink (mode `160000`) that appears in `changed_between(anchor, sha)` is a path-policy violation unless
  `policy.allow_submodules`.

### B6 What M17 added

B1–B5 apply to candidate worktrees (`candidate_protected`), candidate checkpoints (`candidate_checkpoint`), and the
candidate and red verify records. Part A reaches the red-green run's args through `with_red_green` (X47).

## Acceptance criteria

### Part A — the command-line reader

- AC-1: In a `sh`-family command string the word after `if`, `then`, `elif`, `else`, `while`, `until`, `do`, `!` and `command` is a head: each of the review's T5 forms names its script as an entry script (protected; refused at `task create` when the base commit lacks it and the repository does not ignore it).
- AC-2: `env`, `timeout`, `nice` and `nohup` at a head pass the head to the command they run, reading exactly the options of A2 and chaining; an unknown option stops the reading of that command (its words are `maybe`), never a refusal.
- AC-3: In a `cmd` string, `start` (title, switches, `/d` as in A3) and `if` (every condition form of A3) pass the head to the command after them; `start` with no command or with a URL names no entry.
- AC-4: `pwsh -CommandWithArgs` / `-cwa` (and abbreviations) is a command flag whose string is the next argument only; `-EncodedCommand` (and abbreviations) is decoded and read as a command string; a value that does not decode stops the reading, never refuses.
- AC-5: Inside a command string, an interpreter head of `INTERPRETERS` gets the same entry rule as outside one, and `make -f` / `nmake /f` make their file an entry.
- AC-6: `+` options in the `sh` family, versioned interpreter names, `cmd /r` and the unlisted-valued-flag rule of A6 find the script; the first positional word is never refused because of that rule.
- AC-7: Every stop of the reader (a cap, a here-document, a `cd` that cannot be followed, an unknown wrapper option) gives one note per gate in `task create`, `task config --reload`, `doctor` and `gate list`, naming the gate and the reason; nothing is refused because of it.
- AC-8: Nothing protected before M18 is unprotected; only a head that clearly names a script is refusable (D-2); every existing `cmdline.rs` and `checks.rs` test passes unchanged; each configuration form that is newly refused is listed in `docs/M18-NOTES.md` with the rule that finds it.
- AC-9: A task created before M18 keeps running: its protected set widens on its next engine start (X9), it is never refused mid-task, and `task config --reload` applies the new refusals (D-3).
- AC-10: Property tests over generated `sh`, `cmd` and `pwsh` command lines (A8, no new dependency) hold the four properties of A8 with at least 10 000 cases per shell; a failure prints its seed and its command line.
- AC-11: The red-green run's args get every new reading through `with_red_green` (X47): forms such as `cargo test --locked {test} -- --exact`, `dotnet test --filter "FullyQualifiedName~{test}"` and `sh -c 'if ./t.sh {test}; then …; fi'` are tested.
- AC-12: `t5_shell_keywords_and_launchers_do_not_hide_an_entry_script` and `t6_option_and_interpreter_spellings_do_not_hide_an_entry_script` pass with their `#[ignore]` removed.

### Part B — the trust edges

- AC-13: `prepare` accepts a verify record only as B1 says, for the task's, a candidate's and the red checkout's record; otherwise it makes a new checkout and records `verify_record_refused`; nothing outside the slot is reset, cleaned or detached.
- AC-14: `task cleanup` deletes only `<verify_root>/<task>`, never a path because a record names it (also M17's candidate and red records); without a readable configuration it deletes no verify folder and names where one may be.
- AC-15: After every checkpoint commit (normal, partial, candidate), `changed_between(anchor, sha)` is asked of the path policy; a violation soft-resets to the anchor and escalates `policy_violation` naming the paths, or disqualifies the candidate.
- AC-16: A task's and each candidate's Git directories come from the main repository at creation, are stored (schema 5), and are used for every Git call on that worktree, `git_meta` included; the `.git` pointer's bytes are protected state (`@git/pointer`); a task created before M18 gets them from the main repository on its next start.
- AC-17: Replace refs are protected as Git lists them (loose, packed, reftable): one `@git/replace-refs` entry.
- AC-18: A `.git` file or folder anywhere in a worktree except its own pointer is protected state (a hard change when untracked or ignored); a gitlink appearing in a checkpoint is a path-policy violation unless `policy.allow_submodules`.
- AC-19: B1–B5 hold for M17's candidate worktrees, candidate checkpoints and candidate and red verify records.
- AC-20: `t1_a_packed_replace_ref_is_protected_git_metadata`, `t2_the_worktree_git_pointer_is_protected_state`, `t3_a_nested_git_entry_is_scanned`, `t4_the_checkpoint_commits_only_what_the_policy_saw`, `t7_a_forged_verify_record_cannot_point_verify_at_another_folder` and `t7_task_cleanup_deletes_only_the_tasks_own_verify_folder` pass with their `#[ignore]` removed; `tests/review_2026_11_05.rs` has no `#[ignore]` left.
- AC-21: The store schema is version 5; a version-4 database is raised on open, and a task in the middle of an M17 fan-out survives the upgrade and completes.

### Closing

- AC-22: Every behaviour above and every item of the "Edge cases" section below is a named test, with fake agents only, mainly in `tests/m18_reader.rs` and `tests/m18_trust.rs` (unit tests next to the code where that fits).
- AC-23: `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, the same with `--no-default-features`, and `cargo test --locked` pass on Linux; GitHub CI is green on `windows-latest` and `ubuntu-latest`; no existing test was weakened or deleted.
- AC-24: Documentation is updated: `README.md` (what the reader reads, its notes, the trust edges in the boundary section), `docs/HUONG-DAN.md` (in Vietnamese: the gate forms that are read, the notes, §8.1, and §9.1 if a class changes), `docs/DECISIONS.md` (X64 onward), `docs/STATUS.md` (T1–T7, O-41, O-42, O-34 and N-10 edited in place, never struck through), `docs/REVIEW-2026-11-05.md` (its status table), and a new `docs/M18-NOTES.md` (record per task, an AC → test table, an edge case → test table, "Decided without the owner", open items).

## Edge cases (each one is a named test)

### Part A

1. `sh -c` with `if ./x.sh; then …; fi`, `if true; then ./x.sh; fi`, `while ./x.sh; do …; done`, `until ./x.sh; do …; done`, `! ./x.sh` and `command ./x.sh`: each names `x.sh`. `builtin echo` names nothing.
2. `cd scripts; if ./x.sh; then …; fi` resolves to `scripts/x.sh`. `cd scripts && if ./x.sh; …` cannot follow the `cd` (X10), so the rest is `maybe`.
3. `env -i PATH=/usr/bin ./x.sh`, `env -u HOME ./x.sh` and `env -- ./x.sh` name `x.sh`; `env -C dir ./x.sh` and `env -S 'a b'` stop the reading.
4. `timeout 5m ./x.sh`, `timeout -s KILL -k 10 5m ./x.sh` and `timeout --foreground 5 ./x.sh` name `x.sh`; `timeout --bogus 5 ./x.sh` stops the reading.
5. `nice -n 5 ./x.sh`, `nohup ./x.sh` and `nice nohup env FOO=1 ./x.sh` (chained) name `x.sh`.
6. `cmd /c start /wait x.cmd`, `start "" /b x.cmd`, `start "title" x.cmd` and `start /d sub x.cmd` (→ `sub\x.cmd`) name the script; `start /d %TEMP% x.cmd` stops the reading; `start` alone and `start https://example.com` name nothing.
7. `cmd /c if exist f x.cmd`, `if not exist f x.cmd`, `if errorlevel 1 x.cmd`, `if defined X x.cmd`, `if 1 EQU 1 x.cmd`, `if cmdextversion 2 x.cmd`, `if /i a==b x.cmd` and `if exist f (x.cmd) else (y.cmd)`: each names its scripts.
8. `pwsh -cwa "& ./x.ps1"` and `pwsh -CommandWithArgs '& ./x.ps1 $args[0]' a` name `x.ps1`; `-EncodedCommand` and an abbreviation carrying `& ./x.ps1` name it; invalid Base64, and Base64 that is not UTF-16LE text, stop the reading; the existing `-co` / `-comm` abbreviations read as before.
9. `sh -c 'python scripts/check.py'`, `cmd /c node tools\x.js`, `pwsh -c 'python ./x.py'`, `sh -c 'make -f build/x.mk'` and `cmd /c nmake /f build\x.mak`: each file is an entry.
10. `bash +o posix x.sh`, `sh +x x.sh`, `node --experimental-loader ./l.mjs x.js` (entry `x.js`, `l.mjs` protected), `node --inspect-port 9229 x.js`, `python3.12 x.py`, `pypy3 x.py`, `perl5.36 x.pl`, `cmd /r x.cmd` and `python --check-hash-based-pycs always x.py`: each finds its script.
11. A first positional word that is a real script (it has a separator) stays the entry even when a later word has a script extension.
12. A string nested 7 levels deep, one of 4097 words, one with 33 `cd` folders and one with a here-document: each gives its note in `task create`, `doctor` and `gate list`, and none refuses.
13. A configuration that `task create` admitted before M18 and that now has a refusable entry missing at base: `task create` refuses it, naming the rule; an existing task with it keeps running and its script is protected; `task config --reload` refuses it.
14. `red_green.args` in the forms `if`, `env` and `timeout`: the per-test command's script is protected and refused like the gate's own.
15. Every form the review says held up reads exactly as before (a table test): `cd` / `pushd` / `Set-Location`, `&&`, `{ …; }`, a function, `<` redirection, `-ec` / `-ce`, the PowerShell abbreviations, `& 'quoted'`, `cmd.exe /d /s /c`, `for /f`, `PATHEXT`.
16. The property tests of A8: 10 000 seeded cases per shell; a failing case reports its seed and its command line.

### Part B

17. A record whose `dir` is another folder holding an untracked file (the review's reproduction): nothing is deleted, a new checkout is made, and the owner's `main` HEAD stays a symbolic ref.
18. A record whose `base` is another folder: `task cleanup --force` deletes only `<verify_root>/<task>`, and the other folder stays.
19. A forged `verify-checkout-c1.json` and a forged red record (M17): the same as edge cases 17 and 18.
20. `task cleanup` without a readable configuration (EC-12) deletes no verify folder and names where one may be.
21. A process outside the Job appends to a tracked protected file between the path check and `git add -A` (the review's `TORYO_GIT` wrapper): `policy_violation`, HEAD back at the anchor, the change in the tree.
22. The same during a candidate's checkpoint: that candidate is disqualified and the other wins.
23. A forbidden path slipped into a recovery's partial checkpoint: escalation, nothing kept on the branch.
24. The worktree's `.git` pointer pointed at a byte-identical copy and a hook added to the real Git directory (the review's reproduction): `protected_config_changed`; the same for a candidate worktree.
25. The pointer edited to name another repository: `protected_config_changed` before any Git call goes through it.
26. A task created before M18 (a version-4 database): its Git directories come from the main repository on its next start, and a pointer already changed by then is detected.
27. `git replace` followed by `git pack-refs --all` (the review's reproduction): `protected_config_changed`; a loose replace ref is still detected.
28. `src/sub/.git/config` with `core.fsmonitor`, a nested `.git` file, and a candidate's nested repository: each escalates.
29. A nested repository that `git add -A` turns into a gitlink of the checkpoint: `policy_violation`; with `policy.allow_submodules: true` it is allowed.
30. A version-4 database with a task in the middle of a fan-out: raised to 5, and the fan-out completes.
31. A test that needs a Unix mechanism (`setsid`, a symlink) says so in its `#[cfg(unix)]`, as the review's tests do. Where Windows has the mechanism, a Windows test is added beside it.

## Out of scope (write them under Open in `docs/STATUS.md` when they matter)

- The export comparison in `verify.rs` (X15), and anything about gates beyond Low integrity (O-34 review M5 and M6).
- O-41's long tail: reading the option grammars of programs other than the wrappers, interpreters and makefile tools
  of A2, A5 and A6. The net of X13 stays the answer for them.
- Whether the vendors' sandboxes (Claude `--add-dir`, Codex `workspace-write`) protect a `.git` **file** as they
  protect a `.git` folder: one measurement on the real machine, not code.
- A process that escapes the Job or the process group (WMI, the task scheduler, `setsid`) before the checkpoint: B2
  makes its write visible after the commit, but stopping it is the sandbox's job.
- Everything listed as out of scope in `docs/tasks/M17/TASK.md`.
