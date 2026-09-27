# BatchRename — agent instructions

- PowerShell 7 module: `src/BatchRename.psm1`. Export every public function with `Export-ModuleMember`.
- Tests: one file per feature in `tests/<Feature>.Tests.ps1`, dot-sourced by `scripts/test.ps1`.
  Use only `Assert-Equal`, `Assert-True` and `New-TestFolder` (no Pester). Never touch real user folders in tests.
- Functions that change files must support `-WhatIf` (`[CmdletBinding(SupportsShouldProcess)]`).
- Do not edit `scripts/*.ps1`, `AGENTS.md`, `thien.yaml`: they are protected.
- Do not commit, stash, or switch branches. The orchestrator owns Git.
