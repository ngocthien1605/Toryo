# Dependency-free test runner (no Pester). Each tests/*.Tests.ps1 is dot-sourced and may use:
#   Assert-Equal <name> <expected> <actual>
#   Assert-True  <name> <condition>
#   New-TestFolder  -> a fresh empty temp directory (removed at the end)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $root 'src/BatchRename.psm1') -Force -DisableNameChecking
$script:failures = 0
$script:folders = @()
function Assert-Equal($Name, $Expected, $Actual) {
    if ($Expected -ne $Actual) { Write-Output "FAIL $Name expected=[$Expected] actual=[$Actual]"; $script:failures++ }
    else { Write-Output "ok   $Name" }
}
function Assert-True($Name, [bool]$Condition) {
    if (-not $Condition) { Write-Output "FAIL $Name"; $script:failures++ } else { Write-Output "ok   $Name" }
}
function New-TestFolder {
    $d = Join-Path ([System.IO.Path]::GetTempPath()) ("br-test-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
    New-Item -ItemType Directory -Path $d | Out-Null
    $script:folders += $d
    $d
}
try {
    Get-ChildItem (Join-Path $root 'tests') -Filter *.Tests.ps1 | Sort-Object Name | ForEach-Object {
        Write-Output "# $($_.Name)"
        . $_.FullName
    }
}
catch {
    Write-Output "FAIL (exception) $($_.Exception.Message)"
    $script:failures++
}
finally {
    $script:folders | ForEach-Object { Remove-Item -LiteralPath $_ -Recurse -Force -ErrorAction SilentlyContinue }
}
if ($script:failures -gt 0) { Write-Output "$($script:failures) test(s) failed"; exit 1 }
Write-Output 'all tests passed'
exit 0
