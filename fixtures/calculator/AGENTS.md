# Calculator fixture — agent instructions

- PowerShell 7 module in `src/Calculator.psm1`; tests in `scripts/test.ps1` and `tests/*.Tests.ps1`.
- Tests must stay dependency-free (no Pester). Use the `Assert-Equal` helper from `scripts/test.ps1`.
- Do not edit `scripts/*.ps1`, `toryo.yaml`, `AGENTS.md`, `.gitattributes`: they are protected paths.
- Do not commit. The orchestrator creates the commits.
- Keep functions small; every exported function needs at least one test.
