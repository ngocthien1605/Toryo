# BatchRename (sample app for toryo)

A tiny PowerShell 7 module used to learn how `toryo spike pipeline` turns a `TASK.md` into
reviewed, tested commits. Start state: one function, `ConvertTo-SafeFileName`.

```powershell
pwsh -NoProfile -File scripts/build.ps1
pwsh -NoProfile -File scripts/test.ps1
```

Tasks to try, in order: `tasks/BR-001.md`, `tasks/BR-002.md`.
