# Build = parse every .ps1/.psm1 and import the module once. Exit 1 on any error.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$errors = @()
Get-ChildItem -Path $root -Recurse -Include *.ps1, *.psm1 -File | ForEach-Object {
    $t = $null; $e = $null
    [System.Management.Automation.Language.Parser]::ParseFile($_.FullName, [ref]$t, [ref]$e) | Out-Null
    if ($e) { $errors += $e | ForEach-Object { "$($_.Extent.File):$($_.Extent.StartLineNumber): $($_.Message)" } }
}
if ($errors) { $errors | ForEach-Object { Write-Output "PARSE ERROR $_" }; exit 1 }
Import-Module (Join-Path $root 'src/BatchRename.psm1') -Force -DisableNameChecking
$exported = (Get-Module BatchRename).ExportedFunctions.Keys -join ', '
Write-Output "build ok: exported $exported"
exit 0
