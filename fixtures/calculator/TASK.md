# CALC-001 — Add division with a clear divide-by-zero error

## Goal
Add a `Divide-Numbers` function to the calculator module.

## Acceptance criteria
- AC-1: `Divide-Numbers -A 6 -B 3` returns `2`.
- AC-2: `Divide-Numbers -A 1 -B 0` throws a terminating error whose message contains `Division by zero`.
- AC-3: `Divide-Numbers` is exported by the module and covered by tests in `tests/Divide.Tests.ps1` that run from `scripts/test.ps1`.

## Scope
- `src/Calculator.psm1`
- new file `tests/Divide.Tests.ps1`

## Out of scope
- Changing `scripts/build.ps1` or `scripts/test.ps1`.

## Verification
- `pwsh -NoProfile -File scripts/build.ps1`
- `pwsh -NoProfile -File scripts/test.ps1`
