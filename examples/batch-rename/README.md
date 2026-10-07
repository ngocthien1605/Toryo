# BatchRename (sample app for toryo)

A tiny PowerShell 7 module used to learn how `toryo` turns a `TASK.md` into reviewed, tested
commits. Start state: one function, `ConvertTo-SafeFileName`.

```powershell
pwsh -NoProfile -File scripts/build.ps1
pwsh -NoProfile -File scripts/test.ps1
```

Tasks to try, in order: `tasks/BR-001.md`, `tasks/BR-002.md`. This repository has no `toryo.yaml`
of its own: point `toryo` at a configuration (`--config` or `$TORYO_CONFIG`) and at this folder
(`--repo`):

```powershell
toryo task create --file tasks\BR-001.md --repo $PWD
toryo run BR-001                                  # plan → stops at the plan gate
toryo approve BR-001 --gate plan --continue       # implement → build, test → review → final gate
toryo approve BR-001 --gate final                 # Completed; merge branch toryo/BR-001 yourself
toryo task cleanup BR-001
```

The walkthrough is section 6 of `docs/HUONG-DAN.md` in the toryo repository.
