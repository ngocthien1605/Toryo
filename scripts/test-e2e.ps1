#Requires -Version 7
<#
.SYNOPSIS
    The M16 acceptance run: build, every test, the pseudo-console tests, the Pester tests —
    -Repeat times — and test-results\summary.md.

.DESCRIPTION
    1. cargo build --locked (fails fast)
    then, in each of the -Repeat repetitions:
    2. cargo test --locked                          (unit + integration)
    3. cargo test --locked --test e2e_m16 -- --ignored --exact <test>, one AT at a time
                                                    (Windows pseudo-console: AT-5, AT-6, AT-7, AT-9)
    4. Invoke-Pester tests\e2e (Pester 5+), NUnit XML to test-results\pester.xml
                                                    (AT-1, AT-2, AT-3, AT-4, AT-8)
    5. test-results\summary.md: one row per AT id — PASS / FAIL per repetition, the duration of
       the last one, and the temp folder a failing test kept (test-results\kept-dirs.txt).
    6. Exit 0 only when every step passed in every repetition.

    Every log is in test-results\logs\. Nothing is retried: a failure is a failure.
#>
param(
    [ValidateRange(1, 20)][int]$Repeat = 1
)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$results = Join-Path $root 'test-results'
$logs = Join-Path $results 'logs'
New-Item -ItemType Directory -Force $logs | Out-Null
Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $results 'kept-dirs.txt'), (Join-Path $results 'summary.md')
Set-Location $root

# --- the AT ids and where they run --------------------------------------------------------
$where = [ordered]@{
    'AT-1' = 'Pester'; 'AT-2' = 'Pester'; 'AT-3' = 'Pester'; 'AT-4' = 'Pester'
    'AT-5' = 'Rust/ConPTY'; 'AT-6' = 'Rust/ConPTY'; 'AT-7' = 'Rust/ConPTY'; 'AT-8' = 'Pester'
    'AT-9' = 'Rust/ConPTY'
}
# $rows[AT][rep] = @{ Pass; Seconds; Note }
$rows = [ordered]@{}
foreach ($id in $where.Keys) { $rows[$id] = @{} }
$steps = [ordered]@{}   # name -> rep -> pass

function Invoke-Logged([string]$Log, [string]$File, [string[]]$Arguments) {
    $out = Join-Path $logs $Log
    $t0 = Get-Date
    $p = Start-Process -FilePath $File -ArgumentList $Arguments -NoNewWindow -Wait -PassThru `
        -RedirectStandardOutput "$out.out.txt" -RedirectStandardError "$out.err.txt" -WorkingDirectory $root
    [pscustomobject]@{ Code = $p.ExitCode; Seconds = [Math]::Round(((Get-Date) - $t0).TotalSeconds, 1); Log = $out }
}

function Write-Summary([bool]$Done) {
    $kept = @{}
    $keptFile = Join-Path $results 'kept-dirs.txt'
    if (Test-Path $keptFile) {
        foreach ($l in Get-Content -Encoding utf8 $keptFile) {
            $ids, $path = $l -split "`t", 2
            foreach ($i in ($ids -split ' ')) { if ($i) { $kept[$i] = $path } }
        }
    }
    $repCols = (1..$Repeat | ForEach-Object { "Run $_" }) -join ' | '
    $md = @("# M16 acceptance tests", "", "Generated $((Get-Date).ToString('yyyy-MM-dd HH:mm:ss')) by ``scripts/test-e2e.ps1 -Repeat $Repeat``" + $(if ($Done) { '' } else { ' (stopped early)' }) + ".", "")
    $md += "| Step | $repCols |"
    $md += "|---|" + ('---|' * $Repeat)
    foreach ($s in $steps.Keys) {
        $cells = 1..$Repeat | ForEach-Object { $v = $steps[$s][$_]; if ($null -eq $v) { '—' } elseif ($v) { 'PASS' } else { 'FAIL' } }
        $md += "| $s | $($cells -join ' | ') |"
    }
    $md += ""
    $md += "| AT | Where | $repCols | Duration (last run) | Kept temp dir |"
    $md += "|---|---|" + ('---|' * $Repeat) + '---|---|'
    foreach ($id in $where.Keys) {
        $cells = 1..$Repeat | ForEach-Object {
            $r = $rows[$id][$_]
            if ($null -eq $r) { '—' } elseif ($r.Pass) { 'PASS' } else { "FAIL$(if ($r.Note) { " ($($r.Note))" })" }
        }
        $last = $rows[$id].Keys | Sort-Object | Select-Object -Last 1
        $dur = if ($null -ne $last) { "$($rows[$id][$last].Seconds) s" } else { '—' }
        $md += "| $id | $($where[$id]) | $($cells -join ' | ') | $dur | $(if ($kept[$id]) { "``$($kept[$id])``" } else { '' }) |"
    }
    $md += ""
    $md += "Logs: ``test-results/logs/``; Pester NUnit XML: ``test-results/pester.xml``."
    $md -join "`n" | Set-Content -Encoding utf8NoBOM (Join-Path $results 'summary.md')
}

# --- 1. build ---------------------------------------------------------------------------------
$b = Invoke-Logged 'build' 'cargo' @('build', '--locked')
$steps['cargo build'] = @{ 1 = ($b.Code -eq 0) }
if ($b.Code -ne 0) {
    Write-Summary $false
    Write-Host "cargo build failed: $($b.Log).err.txt"
    exit 1
}
$env:TORYO_E2E_BIN = Join-Path $root 'target\debug\toryo.exe'

# the pseudo-console tests, by AT id (their names start with at<N>_)
$list = Invoke-Logged 'e2e-list' 'cargo' @('test', '--locked', '--test', 'e2e_m16', '--', '--ignored', '--list')
$rustTests = @{}
foreach ($l in Get-Content -Encoding utf8 "$($list.Log).out.txt") {
    if ($l -match '^(at(\d+)_\w+): test$') { $rustTests["AT-$($Matches[2])"] = $Matches[1] }
}

$allPassed = $true
for ($rep = 1; $rep -le $Repeat; $rep++) {
    Write-Host "== repetition $rep of $Repeat"
    # --- 2. every test ------------------------------------------------------------------------
    $t = Invoke-Logged "rep$rep-cargo-test" 'cargo' @('test', '--locked')
    $steps['cargo test'] = $steps['cargo test'] ?? @{}
    $steps['cargo test'][$rep] = ($t.Code -eq 0)
    if ($t.Code -ne 0) { $allPassed = $false; Write-Host "cargo test failed: $($t.Log).out.txt" }

    # --- 3. the pseudo-console tests, one at a time --------------------------------------------
    foreach ($id in $where.Keys | Where-Object { $where[$_] -eq 'Rust/ConPTY' }) {
        $name = $rustTests[$id]
        if (-not $name) {
            $rows[$id][$rep] = @{ Pass = $false; Seconds = 0; Note = 'no test' }
            $allPassed = $false
            continue
        }
        $r = Invoke-Logged "rep$rep-$id" 'cargo' @('test', '--locked', '--test', 'e2e_m16', '--', '--ignored', '--exact', $name, '--test-threads=1')
        $ok = ($r.Code -eq 0) -and (Select-String -Quiet -Path "$($r.Log).out.txt" -Pattern "^test $name \.\.\. ok$")
        $rows[$id][$rep] = @{ Pass = [bool]$ok; Seconds = $r.Seconds; Note = $(if ($ok) { '' } else { "exit $($r.Code)" }) }
        if (-not $ok) { $allPassed = $false; Write-Host "$id failed: $($r.Log).out.txt" }
    }

    # --- 4. Pester -------------------------------------------------------------------------------
    $xml = Join-Path $results "pester-rep$rep.xml"
    $cmd = @"
`$ErrorActionPreference = 'Stop'
Import-Module Pester -MinimumVersion 5.0.0
`$c = New-PesterConfiguration
`$c.Run.Path = '$(Join-Path $root 'tests\e2e')'
`$c.Run.Exit = `$true
`$c.Output.Verbosity = 'Detailed'
`$c.TestResult.Enabled = `$true
`$c.TestResult.OutputFormat = 'NUnitXml'
`$c.TestResult.OutputPath = '$xml'
Invoke-Pester -Configuration `$c
"@
    # a script file: a multi-line -Command does not survive Start-Process's argument quoting
    $runner = Join-Path $logs "rep$rep-pester.ps1"
    Set-Content -Encoding utf8NoBOM $runner $cmd
    $p = Invoke-Logged "rep$rep-pester" 'pwsh' @('-NoProfile', '-NonInteractive', '-File', $runner)
    $steps['Pester'] = $steps['Pester'] ?? @{}
    $steps['Pester'][$rep] = ($p.Code -eq 0)
    if ($p.Code -ne 0) { $allPassed = $false; Write-Host "Pester failed: $($p.Log).out.txt" }
    if (Test-Path $xml) {
        Copy-Item -Force $xml (Join-Path $results 'pester.xml')
        [xml]$doc = Get-Content -Raw -Encoding utf8 $xml
        foreach ($case in $doc.SelectNodes('//test-case')) {
            if ($case.name -match '(AT-\d+)') {
                $id = $Matches[1]
                $rows[$id][$rep] = @{ Pass = ($case.result -eq 'Success'); Seconds = [Math]::Round([double]$case.time, 1); Note = $(if ($case.result -eq 'Success') { '' } else { $case.result }) }
            }
        }
    }
    foreach ($id in $where.Keys | Where-Object { $where[$_] -eq 'Pester' }) {
        if ($null -eq $rows[$id][$rep]) { $rows[$id][$rep] = @{ Pass = $false; Seconds = 0; Note = 'not run' }; $allPassed = $false }
        elseif (-not $rows[$id][$rep].Pass) { $allPassed = $false }
    }
    Write-Summary $false
}

Write-Summary $true
Get-Content -Encoding utf8 (Join-Path $results 'summary.md') | Write-Host
if ($allPassed) { exit 0 } else { exit 1 }
