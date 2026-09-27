# Dependency-free tests (no Pester needed). Exit code 1 on any failure.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $root 'src/Calculator.psm1') -Force
$failures = 0
function Assert-Equal($Name, $Expected, $Actual) {
    if ($Expected -ne $Actual) { Write-Output "FAIL $Name expected=$Expected actual=$Actual"; $script:failures++ }
    else { Write-Output "ok   $Name" }
}
Assert-Equal 'add'      5  (Add-Numbers 2 3)
Assert-Equal 'subtract' -1 (Subtract-Numbers 2 3)
Assert-Equal 'multiply' 6  (Multiply-Numbers 2 3)
# Tests added by tasks live in tests/*.Tests.ps1 and are dot-sourced here.
$extra = Join-Path $root 'tests'
if (Test-Path $extra) {
    Get-ChildItem $extra -Filter *.Tests.ps1 | ForEach-Object { . $_.FullName }
}
$out = Join-Path $root 'build'
New-Item -ItemType Directory -Force -Path $out | Out-Null
Set-Content -Path (Join-Path $out 'test-results.txt') -Value "failures=$failures" -Encoding utf8
if ($failures -gt 0) { exit 1 }
Write-Output 'all tests passed'
exit 0
