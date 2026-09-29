<#
.SYNOPSIS
  M8 acceptance on the real CLIs (Windows): the E2E MVP demo flow with no copy/paste, plus
  the real-CLI checks of V08-VERIFY 5.

.DESCRIPTION
  Everything runs in fresh sample repos under -Root\<Tag> (nothing in your own repos changes).
  Real tokens are used: Claude plans + reviews, Codex implements.

  1. doctor: 0 FAIL for the configuration.
  2. Calculator fixture: CALC-001 (Divide-Numbers) from TASK.md to Completed; merged; tests pass.
  3. BatchRename: BR-001 (Rename-ByPattern) to Completed; merged; tests pass.
  4. BatchRename: BR-002 (sequence numbers) on top of BR-001, with one real fix cycle:
     plan gate -> approve --continue -> final gate -> reject with a real change -> Fixing ->
     final gate again -> approve -> Completed; merged; all tests pass.
  5. V08-VERIFY 5 (unless -SkipV08):
     a) R64: a Claude turn is interrupted by the live budget (cap 2000) -> Paused(Budget);
        the budget is raised and the task resumes to the plan gate.
     b) R62: the Codex launch fails closed when the implementer profile has no
        windows_sandbox -> Escalated PolicyRefusal, no process started, and `next:` points
        at `task config --reload`.
     c) R66: with agent_stall_min 1, a Codex command that is silent for ~150 s is not killed
        as stalled; the task completes.
  6. Cleanup of the tasks (unless -KeepTasks); the sample repos stay for inspection.

.EXAMPLE
  pwsh -NoProfile -File scripts\m8-check.ps1
  pwsh -NoProfile -File scripts\m8-check.ps1 -SkipV08 -KeepTasks
#>
param(
    [string]$Root = 'C:\.thien\m8-check',
    [string]$Config = $(if ($env:TORYO_CONFIG) { $env:TORYO_CONFIG } else { 'C:\.thien\m0\toryo.yaml' }),
    [string]$Tag = (Get-Date -Format 'MMddHHmm'),
    # the implementer profile the V08 configurations change (R62: its windows_sandbox is removed)
    [string]$CodexProfile = 'codex-openai',
    [switch]$SkipV08,
    # only V08-VERIFY 5 (steps 1, 5, 6)
    [switch]$SkipDemo,
    [switch]$KeepTasks,
    [switch]$AcceptUnverified
)
$ErrorActionPreference = 'Stop'
# toryo writes UTF-8; decode its piped output as UTF-8, not the OEM code page (see m4-m7-check.ps1)
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$toryo = (Resolve-Path (Join-Path $PSScriptRoot '..\bin\toryo.exe')).Path
$env:TORYO_CONFIG = $Config
$dataDir = if ($env:TORYO_DATA_DIR) { $env:TORYO_DATA_DIR } else { Join-Path $env:LOCALAPPDATA 'toryo' }
$work = Join-Path $Root $Tag
if (Test-Path $work) { throw "$work exists already: pass another -Tag" }
New-Item -ItemType Directory -Force (Join-Path $work 'tasks') | Out-Null
$log = Join-Path $work 'm8-check.log'
$checks = [System.Collections.Generic.List[object]]::new()
$tasks = [System.Collections.Generic.List[object]]::new()   # (id, repo) for the cleanup

function Show-Step([string]$text) { Write-Host "`n=== $text ===" -ForegroundColor Cyan; "`n=== $text ===" | Add-Content $log }
function Get-Line($x) {
    if ($x -is [System.Management.Automation.ErrorRecord]) { "$($x.Exception.Message)" } else { "$x" }
}
function T {
    # run toryo, stream its output, keep it in the log; returns the exit code
    & $toryo @args 2>&1 | ForEach-Object { Get-Line $_ } | Tee-Object -FilePath $log -Append | Out-Host
    return $LASTEXITCODE
}
function TOut {
    # run toryo and return its combined output as one string
    (& $toryo @args 2>&1 | ForEach-Object { Get-Line $_ }) -join "`n"
}
function Check([string]$name, [bool]$ok, [string]$detail = '') {
    $checks.Add([pscustomobject]@{ Check = $name; Result = $(if ($ok) { 'PASS' } else { 'FAIL' }); Detail = $detail })
    Write-Host ("[{0}] {1} {2}" -f $(if ($ok) { 'PASS' } else { 'FAIL' }), $name, $detail) -ForegroundColor $(if ($ok) { 'Green' } else { 'Red' })
}
function State([string]$id) { ((TOut status $id) -split "`n")[0] }
function New-Task([string]$file, [string]$repo, [string]$id, [string]$cfg = $Config) {
    $c = T task create --config $cfg --file $file --repo $repo --id $id
    if ($c -ne 0) { throw "task create $id failed ($c)" }
    $tasks.Add([pscustomobject]@{ Id = $id; Repo = $repo })
}
function Merge-Task([string]$repo, [string]$id, [string]$msg) {
    git -C $repo merge --squash "toryo/$id" 2>&1 | Add-Content $log
    git -C $repo commit -q -m $msg 2>&1 | Add-Content $log
    return $LASTEXITCODE -eq 0
}
function Test-Fixture([string]$repo) {
    $out = pwsh -NoProfile -File (Join-Path $repo 'scripts/test.ps1') 2>&1 | ForEach-Object { Get-Line $_ }
    $out | Add-Content $log
    return ($LASTEXITCODE -eq 0) -and (($out -join "`n") -match 'all tests passed')
}
$drive = @()
if ($AcceptUnverified) { $drive += '--accept-unverified' }

# ── 1. doctor ───────────────────────────────────────────────────────────────────────
Show-Step "1. doctor --config $Config"
$doc = TOut doctor --config $Config
$doc | Add-Content $log
($doc -split "`n") | Where-Object { $_ -match 'FAIL|WARN' } | ForEach-Object { Write-Host $_ }
Check 'doctor: 0 FAIL' ($doc -match '(?m)^0 FAIL') (($doc -split "`n") | Select-Object -Last 1)
if ($doc -notmatch '(?m)^0 FAIL' -and -not $AcceptUnverified) {
    Write-Warning "doctor reports FAILs; fix them first (e.g. ``toryo doctor --canaries`` after a CLI upgrade). log: $log"
    exit 1
}

# ── 2. calculator: CALC-001 ─────────────────────────────────────────────────────────
$calc = Join-Path $work 'calc'
$br = Join-Path $work 'batch-rename'
$c = T spike init-fixture --app calculator --into $calc
$c = T spike init-fixture --app batch-rename --into $br
$idCalc = "M8C-$Tag-CALC"
if (-not $SkipDemo) {
    Show-Step "2. $idCalc (calculator fixture) -> Completed"
    New-Task (Join-Path $calc 'TASK.md') $calc $idCalc
    $c = T run $idCalc --yes @drive
    Check 'CALC-001 completed' ((State $idCalc) -match 'Completed') (State $idCalc)
    Check 'CALC-001 merged, fixture tests pass' ((Merge-Task $calc $idCalc 'CALC-001: Divide-Numbers') -and (Test-Fixture $calc))

    # ── 3. BatchRename: BR-001 ──────────────────────────────────────────────────────────
    $id1 = "M8C-$Tag-BR1"
    Show-Step "3. $id1 (BR-001) -> Completed"
    New-Task (Join-Path $br 'tasks/BR-001.md') $br $id1
    $c = T run $id1 --yes @drive
    Check 'BR-001 completed' ((State $id1) -match 'Completed') (State $id1)
    Check 'BR-001 merged, fixture tests pass' ((Merge-Task $br $id1 'BR-001: Rename-ByPattern') -and (Test-Fixture $br))

    # ── 4. BatchRename: BR-002 with a real fix cycle ────────────────────────────────────
    $id2 = "M8C-$Tag-BR2"
    Show-Step "4. $id2 (BR-002) -> plan gate -> final gate -> reject -> fix -> Completed"
    New-Task (Join-Path $br 'tasks/BR-002.md') $br $id2
    $c = T run $id2 @drive
    Check 'BR-002 stops at the plan gate' ((State $id2) -match 'WaitingPlanApproval') (State $id2)
    $c = T approve $id2 --gate plan --continue @drive
    Check 'BR-002 reaches the final gate' ((State $id2) -match 'WaitingFinalApproval') (State $id2)
    if ((State $id2) -match 'WaitingFinalApproval') {
        $c = T reject $id2 --gate final --reason 'Also cover -Sequence with -Start 0 in tests/Sequence.Tests.ps1: the first file gets 000. Keep every existing test passing.'
        $c = T run $id2 @drive
        $st = State $id2
        $fix = if ($st -match 'fix (\d+)/') { [int]$Matches[1] } else { 0 }
        Check 'final-gate rejection runs a real fix cycle and returns to the final gate' (($st -match 'WaitingFinalApproval') -and ($fix -ge 1)) $st
        $dec = Get-Content (Join-Path $dataDir "runs/$id2/runstore/DECISIONS.md") -Raw
        Check 'DECISIONS.md records the gates and the rejection' (($dec -match 'plan gate approved') -and ($dec -match 'final gate rejected'))
        $c = T approve $id2 --gate final
    }
    Check 'BR-002 completed' ((State $id2) -match 'Completed') (State $id2)
    Check 'BR-002 merged, all BR-001 + BR-002 tests pass' ((Merge-Task $br $id2 'BR-002: sequence numbers') -and (Test-Fixture $br))
}

# ── 5. V08-VERIFY 5 ─────────────────────────────────────────────────────────────────
if (-not $SkipV08) {
    $base = Get-Content $Config -Raw
    # a) + b): the implementer profile without windows_sandbox (only inside its own block)
    $out = [System.Collections.Generic.List[string]]::new(); $indent = -1
    foreach ($line in ($base -split "\r?\n")) {
        if ($line -match "^(\s*)$([regex]::Escape($CodexProfile)):\s*(#.*)?$") { $indent = $Matches[1].Length }
        elseif ($indent -ge 0 -and $line.Trim() -and ($line.Length - $line.TrimStart().Length) -le $indent) { $indent = -1 }
        if ($indent -ge 0 -and $line -match '^\s*windows_sandbox:') { continue }
        $out.Add($line)
    }
    $cfgA = Join-Path $work 'v08-nosandbox.yaml'
    ($out -join "`n") | Set-Content $cfgA -Encoding utf8NoBOM
    if ((Get-Content $cfgA -Raw) -eq $base) { throw "no windows_sandbox found in profile $CodexProfile of $Config" }
    # c): a one-minute stall clock
    $b = if ($base -match 'agent_stall_min:\s*\d+') { $base -replace 'agent_stall_min:\s*\d+', 'agent_stall_min: 1' }
         elseif ($base -match '(?m)^timeouts:\s*$') { $base -replace '(?m)^timeouts:\s*$', "timeouts:`n  agent_stall_min: 1" }
         else { $base + "`ntimeouts:`n  agent_stall_min: 1`n" }
    $cfgB = Join-Path $work 'v08-stall.yaml'
    $b | Set-Content $cfgB -Encoding utf8NoBOM

    $taskA = Join-Path $work 'tasks/V08-A.md'
    @'
# V08-A — Add a Power-Numbers function

## Goal
Add `Power-Numbers` to the calculator module.

## Acceptance criteria
- AC-1: `Power-Numbers -A 2 -B 10` returns `1024`.
- AC-2: `Power-Numbers` is exported and covered by tests in `tests/Power.Tests.ps1` that run from `scripts/test.ps1`.

## Scope
- `src/Calculator.psm1`
- new file `tests/Power.Tests.ps1`

## Verification
- `pwsh -NoProfile -File scripts/test.ps1`
'@ | Set-Content $taskA -Encoding utf8NoBOM
    $taskB = Join-Path $work 'tasks/V08-B.md'
    @'
# V08-B — Record the slow self-check

## Goal
Record the output of the repository's slow self-check.

## Acceptance criteria
- AC-1: Run `pwsh -NoProfile -File tools/slow-check.ps1` exactly once, as a single command. It takes about 150 seconds and prints nothing until it finishes, so give that command a timeout of at least 300 seconds (300000 ms). Do not shorten, edit or skip the script.
- AC-2: `docs/slow-check.txt` contains the single line the script printed, unchanged.

## Scope
- new file `docs/slow-check.txt`

## Out of scope
- Changing `tools/slow-check.ps1` or `scripts/*.ps1`.

## Verification
- `pwsh -NoProfile -File scripts/test.ps1`
'@ | Set-Content $taskB -Encoding utf8NoBOM
    New-Item -ItemType Directory -Force (Join-Path $calc 'tools') | Out-Null
    @'
# Slow self-check: silent for about 150 seconds, then prints one line.
Start-Sleep -Seconds 150
"slow-check ok $([DateTime]::UtcNow.ToString('yyyy-MM-ddTHH:mm:ssZ'))"
'@ | Set-Content (Join-Path $calc 'tools/slow-check.ps1') -Encoding utf8NoBOM
    git -C $calc add tools/slow-check.ps1 2>&1 | Add-Content $log
    git -C $calc commit -q -m 'tools: slow self-check' 2>&1 | Add-Content $log

    $idA = "M8C-$Tag-V08A"
    Show-Step "5a. ${idA}: R64 live budget interrupt (cap 2000) -> Paused(Budget) -> raise -> plan gate"
    $d = TOut doctor --config $cfgA
    Check 'doctor flags the profile without windows_sandbox' ($d -match "$([regex]::Escape($CodexProfile)).*windows_sandbox not set")
    New-Task $taskA $calc $idA $cfgA
    $c = T budget set $idA --tokens 2000 --config $cfgA
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $c = T run $idA --config $cfgA @drive
    $show = TOut task show $idA --config $cfgA
    Check 'R64: the Claude turn is interrupted by the live budget' (($show -match '(?m)^\S+\s+Paused') -and ($show -match 'plan/1\s+Interrupted\s+\S+\s+budget')) ("{0:N0}s" -f $sw.Elapsed.TotalSeconds)
    Check 'R64: the interrupted turn is counted, not zero' ($show -match 'plan/1\s+\S+\s+provisional\s+[1-9]')
    $c = T budget set $idA --tokens 3000000 --config $cfgA
    $c = T resume $idA --config $cfgA @drive
    Check 'resume after raising the budget reaches the plan gate' ((State $idA) -match 'WaitingPlanApproval') (State $idA)

    Show-Step "5b. ${idA}: R62 fail-closed Codex preflight"
    $c = T approve $idA --gate plan --continue --config $cfgA @drive
    $st = TOut status $idA
    Check 'R62: Escalated with PolicyRefusal' (($st -match 'Escalated') -and ($st -match 'PolicyRefusal') -and ($st -match 'windows_sandbox')) (($st -split "`n")[0])
    # (the preflight's own text says "the sandbox type is None": look for Rust's Option syntax only)
    Check 'R62: next names task config --reload; no Option syntax' (($st -match 'task config \S+ --reload') -and ($st -notmatch 'Some\(|failed: None\b'))
    $oc = Get-ChildItem (Join-Path $dataDir "runs/$idA/captures") -Filter 'implement-1.outcome.json' | Select-Object -First 1
    $o = if ($oc) { Get-Content $oc.FullName -Raw | ConvertFrom-Json } else { $null }
    Check 'R62: no process started (null exit, refused argv kept)' (($null -ne $o) -and ($null -eq $o.exit) -and ($o.refused_argv.Count -gt 0))

    $idB = "M8C-$Tag-V08B"
    Show-Step "5c. ${idB}: R66 a silent ~150 s Codex command with agent_stall_min 1"
    New-Task $taskB $calc $idB $cfgB
    $times = [System.Collections.Generic.List[double]]::new()
    $sw = [Diagnostics.Stopwatch]::StartNew()
    & $toryo run $idB --yes --config $cfgB @drive 2>&1 | ForEach-Object {
        $times.Add($sw.Elapsed.TotalSeconds); Get-Line $_ } | Tee-Object -FilePath $log -Append | Out-Host
    $gap = 0; for ($i = 1; $i -lt $times.Count; $i++) { $gap = [Math]::Max($gap, $times[$i] - $times[$i - 1]) }
    $stdout = Get-ChildItem (Join-Path $dataDir "runs/$idB/captures") -Filter 'implement-*.stdout.jsonl' | ForEach-Object { Get-Content $_.FullName }
    $slow = $stdout | Where-Object { $_ -match 'slow-check\.ps1' } | ForEach-Object { $_ | ConvertFrom-Json } |
        Where-Object { $_.type -eq 'item.completed' -and $_.item.type -eq 'command_execution' }
    Check 'R66: the silent command ran to the end (exit 0)' (@($slow | Where-Object { $_.item.exit_code -eq 0 }).Count -ge 1)
    Check 'R66: silence longer than the stall clock did not stop the turn' ($gap -gt 60) ("longest silence {0:N0}s, stall 60s" -f $gap)
    Check 'R66: the task completes' ((State $idB) -match 'Completed') (State $idB)
}

# ── 6. cleanup ──────────────────────────────────────────────────────────────────────
if (-not $KeepTasks) {
    Show-Step '6. cleanup'
    foreach ($t in $tasks) {
        $c = T task cleanup $t.Id --force
        git -C $t.Repo branch -D "toryo/$($t.Id)" 2>&1 | Add-Content $log
    }
    if ($tasks.Count -gt 0) {
        Check 'status after cleanup has no cleanup hint' (-not ((TOut status $tasks[0].Id) -match 'task cleanup'))
    }
}

Show-Step 'result'
$checks | Format-Table -AutoSize | Out-String -Width 220 | Tee-Object -FilePath $log -Append | Out-Host
Write-Host "sample repos: $work    log: $log"
if ($checks.Result -contains 'FAIL') { exit 1 }
