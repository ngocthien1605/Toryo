# Toryo — decisions in force

One line per decision. **Status** says whether it stands or was replaced (the replacement is named). "Where" is the place in the
code that carries it. The R-numbers (R1 … R72) in the code and the notes come from the plan v0.8 (`plan-v0.8.md`, **not in this
repository**) and from the review rounds of M11; `docs/archive/M11-NOTES.md` has the record per round. This file is the input for
plan v0.9. Add new decisions here; never restate them in a new notes file.

| ID | Decision | Status | Where |
|---|---|---|---|
| **X1** | An environment gate's working directory is an empty folder that agent code cannot write (`<verify_root>\<task>\envgate`). (M11-R2) | in force | `verify::envgate_dir` |
| **X2** | Before the first environment gate the checkout must be exactly the checkpoint, so an org chart with an environment gate needs `verify.checkout: clean`. (M11-R2) | **replaced by X6** | — |
| **X3** | `%VAR%` in an environment gate's `program` / `args` is expanded once, at load; the expanded text is what is checked, frozen and run. (M11-R3) | in force | `config.rs` (machine config load) |
| **X4** | `reviewer-differs-from-builder` counts every implementer turn that ran, including a failed one that wrote code. (M11-R7) | in force | `engine/runner.rs` (`same_vendor_*`) |
| **X5** | The same-vendor note keeps its wording `(Claude both)`. (M11-R7) | in force | `engine/runner.rs` |
| **X6** | An environment gate never reads the Low-integrity verify checkout: it gets a fresh export of the checkpoint commit in a folder agent code cannot write; `verify_checkout_dirty` is no longer an escalation; `verify.checkout: clean` is no longer required. (M11-R9) | in force; replaces X2 | `verify::deploy_source` |
| **X7** | M18: a gate's **entry script** is protected before it exists and `task create` refuses a configuration whose base commit lacks it, because the implementer could write it; other arguments are protected when they exist, never refused. (M11-R10) | in force; rule 1 replaced by X10, refined by X9 | `engine/checks.rs`, `engine/cmdline.rs` (untouchable without a review round) |
| **X8** | Leaving an escalation the machine caused costs no fix cycle: a plain `toryo resume <id>` goes back into the stage. M12 adds `gate_unavailable`, `environment_gate_timeout`, `environment_gate_confirm`, `context_unreadable` to the list (nine classes). (M11-R11) | in force, extended in M12 | `engine/state.rs` `MACHINE_CAUSED` |
| **X9** | What M18 protects is decided from the **base commit**, never from what exists in the worktree; an entry script admitted as build output (missing at base, ignored) may exist in the worktree but is never committed. (M11-R14) | in force; refines X7 | `engine/task.rs` `base_files` |
| **X10** | In a command string (`pwsh -Command`, `cmd /c`, `sh -c`) only the head of each simple command is an entry script; a `cd` changes where the rest resolves; what Toryo cannot follow it does not refuse; a bare head of a `cmd` string is protected, never refused. (M11-R15) | in force; replaces rule 1 of X7 | `engine/cmdline.rs` |
| **X11** | Creating a protected entry script that does not exist yet (`sh check`) stops the task with `protected_config_changed` instead of "restore + fix note". (M11-R10) | in force | `engine/protect.rs` |
| **X12** | What an environment gate needs from the machine (secrets, program, lock name) is checked **before** the check gates run, and again when the gate starts. (M11-R16) | in force; M12-05 adds the check gates and the scripts | `engine/runner.rs`, `engine/gates.rs` |
| **X13** | "Refusing needs certainty; protecting does not": every word of every argument that names a file of the base commit is protected, also inside nested command lines and after the point where reading stopped; this never refuses and never notes. (M11-R19) | in force | `engine/checks.rs`, `engine/cmdline.rs` |
| **X14** | A gate's entry may be a folder of the base commit; it is admitted and protected under that name, nothing inside it because of it. (M11-R18) | in force | `engine/task.rs` `base_paths` |
| **X15** | The byte-exact export check compares bytes (`git cat-file --filters`), not Git's view after line-ending conversion. (M11-R20) | in force | `engine/verify.rs` (untouchable without a review round) |
| **X16** | On a file system that does not tell `Src` from `src` (Windows) the export comparison does not either. (M11-R23) | in force | `engine/verify.rs` |
| **X17** | An environment gate can ask the owner before it deploys: machine key `environment_gates.<name>.confirm: never \| first \| always` (default `never`); the question is a machine stop before any gate runs; a plain `toryo resume <id>` is the "yes", `--yes` never is. (M12-07) | in force | `engine/gates.rs`, `engine/runner.rs` `confirm_preflight` |
| **X18** | An environment gate that runs into its timeout is the owner's problem, not the implementer's: the task stops (`environment_gate_timeout`), no implementer turn, no retry on its own. (M12-06) | in force | `engine/runner.rs` `EnvGates::TimedOut` |
| **X19** | `project.base_branch`, when the file sets it, is the base of `task create` (a missing branch is an error naming the key); otherwise the checked-out branch, as before. (M12-08) | in force | `engine/task.rs`, `config.rs` |
| **X20** | A key that is accepted must do something, or say that it does nothing: `project.runs_root`, `verify.final_clean`, `pipeline.budget_usd_est` are announced by `config show`, `doctor` and `task create`. (M12-08) | in force | `Effective::keys_without_effect` |
| **X21** | Config precedence: `--config`, then `./toryo.yaml` when it exists, then `$TORYO_CONFIG`; one note names both files when they differ. (M12-09) | in force | `main.rs` `config_path` |
| **X22** | Text files Toryo reads are UTF-8; a byte-order mark is ignored; anything else is refused with the file's name, never turned into `�`. A context file that is not UTF-8 stops a turn (`context_unreadable`). (M12-10) | in force | `src/textfile.rs`, `engine/context.rs` |
| **X23** | Gate output that is not UTF-8 is decoded line by line with the machine's OEM code page; every check gate and environment gate gets `DOTNET_CLI_UI_LANGUAGE=en-US` and `VSLANG=1033` (an environment gate's own `env:` wins). (M12-15) | in force | `process/decode.rs`, `engine/xpp.rs` `set_ui_language` |
