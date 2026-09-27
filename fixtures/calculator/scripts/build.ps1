# "Build" for the fixture: parse every PowerShell file and write a manifest to build/.
# It also writes to the temp directory (TEMP/TMP) so the Low-integrity verify boundary (R36) is exercised.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'build'
New-Item -ItemType Directory -Force -Path $out | Out-Null
$errors = @()
$files = Get-ChildItem -Path $root -Recurse -Include *.ps1, *.psm1 -File | Where-Object { $_.FullName -notlike "$out*" }
foreach ($f in $files) {
    $tokens = $null; $parseErrors = $null
    [System.Management.Automation.Language.Parser]::ParseFile($f.FullName, [ref]$tokens, [ref]$parseErrors) | Out-Null
    if ($parseErrors) { $errors += $parseErrors | ForEach-Object { "$($f.Name): $($_.Message)" } }
}
$manifest = $files | ForEach-Object { "{0} {1}" -f (Get-FileHash -Algorithm SHA256 $_.FullName).Hash, $_.FullName.Substring($root.Length + 1) }
Set-Content -Path (Join-Path $out 'manifest.txt') -Value $manifest -Encoding utf8
$tmpProbe = Join-Path ([System.IO.Path]::GetTempPath()) ("thien-build-probe-{0}.txt" -f $PID)
Set-Content -Path $tmpProbe -Value 'temp write ok' -Encoding utf8
Remove-Item $tmpProbe -Force
if ($errors) { $errors | ForEach-Object { Write-Error $_ -ErrorAction Continue }; exit 1 }
Write-Output ("build ok: {0} files" -f @($files).Count)
exit 0
