<#
.SYNOPSIS
  M4-M7 acceptance on the real CLIs (Windows): one small task on the BatchRename demo repo,
  run by the engine with the project context, the protected-state snapshot, the Low-IL
  verify boundary, immutable artifacts, the admission budget and DECISIONS.md.

.DESCRIPTION
  Nothing in your repo is committed or changed by this script: the task file and the
  global context live in a scratch folder; the task gets its own worktree/branch toryo/<Task>.
  Real tokens are used: Claude plans + reviews, Codex implements (two fix cycles at most).

  1. Canaries for this machine's CLI versions:
       claude-implementer-isolation (R49, V07-VERIFY 4) and codex-temp-probe (R46, V07-VERIFY 2).
  2. task create + `context show` (M4): the planner context has global.md + AGENTS.md;
     the Codex implementer context has no AGENTS.md (Codex reads it itself).
  3. run -> plan gate -> approve --continue -> implement -> verify (Low IL, verify checkout
     under %LOCALAPPDATA%\toryo-verify) -> review -> final gate (or a fix cycle first).
  4. At the final gate: reject with a small real change -> Fixing, then
       a) budget 1 token  -> resume -> Paused(Budget) before any agent starts (M7)
       b) budget removed + an ignored protected file in the worktree -> resume
          -> Escalated protected_config_changed before Codex starts (M5)
       c) file removed -> resume -> the fix cycle runs -> final gate again.
  5. Prints the evidence: VERIFY_RESULT boundary, artifacts, usage rows, DECISIONS.md, SUMMARY.md.
  The final approval is left to you.

.EXAMPLE
  pwsh -NoProfile -File scripts\m4-m7-check.ps1
  pwsh -NoProfile -File scripts\m4-m7-check.ps1 -Task BR-004b -SkipCanaries
#>
param(
    [string]$Task = 'BR-004',
    [string]$Repo = 'C:\.thien\AgentOrchestrator\demo\batch-rename',
    [string]$Config = $(if ($env:TORYO_CONFIG) { $env:TORYO_CONFIG } else { 'C:\.thien\m0\thien.yaml' }),
    [string]$ClaudeProfile = 'claude-max',
    [string]$CodexProfile = 'codex-openai',
    [switch]$SkipCanaries,
    [switch]$AcceptUnverified,
    # dry run of this script without tokens (a FakeAgent script, see docs/HUONG-DAN.md 5.3)
    [string]$FakeAgents
)
$ErrorActionPreference = 'Stop'
$toryo = (Resolve-Path (Join-Path $PSScriptRoot '..\bin\toryo.exe')).Path
$env:TORYO_CONFIG = $Config
$dataDir = if ($env:TORYO_DATA_DIR) { $env:TORYO_DATA_DIR } else { Join-Path $env:LOCALAPPDATA 'toryo' }
# scratch files and the log live in the toryo data dir, next to the run store
$scratch = Join-Path $dataDir "m4-m7-check/$Task"
New-Item -ItemType Directory -Force $scratch | Out-Null
$log = Join-Path $scratch 'm4-m7-check.log'
$checks = [System.Collections.Generic.List[object]]::new()

function Show-Step([string]$text) { Write-Host "`n=== $text ===" -ForegroundColor Cyan; "`n=== $text ===" | Add-Content $log }
function Get-Line($x) {
    # a stderr line arrives as an ErrorRecord; its text is the exception message (empty lines too)
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
$drive = @()
if ($AcceptUnverified) { $drive += '--accept-unverified' }
if ($FakeAgents) { $drive += @('--fake-agents', $FakeAgents); $SkipCanaries = $true }

# ── 1. canaries ─────────────────────────────────────────────────────────────────────
if (-not $SkipCanaries) {
    Show-Step "1a. claude-implementer-isolation --profile $ClaudeProfile (R49, V07-VERIFY 4)"
    $c = T spike claude-implementer-isolation --profile $ClaudeProfile
    Check 'claude_implementer_isolation' ($c -eq 0)
    if ($c -ne 0 -and -not $AcceptUnverified) {
        Write-Warning ("$ClaudeProfile is in the implementer fallback chain, so ``toryo run`` refuses to start without a passing " +
            "claude-implementer-isolation for this Claude version (R51). Send the log to Claude, or rerun with -AcceptUnverified. log: $log")
        exit 1
    }
    Show-Step "1b. codex-temp-probe --profile $CodexProfile (R46, V07-VERIFY 2)"
    $c = T spike codex-temp-probe --profile $CodexProfile
    Check 'codex TEMP reaches sandboxed commands' ($c -eq 0)
}
Show-Step '1c. doctor (verify root probe, R45)'
$doc = TOut doctor
$doc | Add-Content $log
($doc -split "`n") | Where-Object { $_ -match 'verify root|FAIL' } | ForEach-Object { Write-Host $_ }
Check 'verify_root can take a Low label' ($doc -match 'verify root.*Low integrity label can be set')

# ── 2. task + context ───────────────────────────────────────────────────────────────
Show-Step "2. task create $Task + context show (M4)"
$taskFile = Join-Path $scratch "$Task.md"
@"
# $Task — Maximum length for safe file names

## Goal
Long names from cameras and exports break some sync tools. Let ``ConvertTo-SafeFileName`` shorten them.

## Acceptance criteria
- AC-1: ``ConvertTo-SafeFileName`` gets a ``-MaxLength`` parameter (default 255). A result longer than ``-MaxLength`` characters is shortened by cutting the end of the base name; the extension is kept.
- AC-2: ``-MaxLength`` smaller than the extension length plus one character throws a terminating error whose message contains ``MaxLength``.
- AC-3: Tests in ``tests/MaxLength.Tests.ps1`` cover AC-1 and AC-2; all existing tests still pass.

## Scope
- ``src/BatchRename.psm1``
- new file ``tests/MaxLength.Tests.ps1``

## Out of scope
- Changing ``scripts/*.ps1``.

## Verification
- ``pwsh -NoProfile -File scripts/build.ps1``
- ``pwsh -NoProfile -File scripts/test.ps1``
"@ | Set-Content -Path $taskFile -Encoding utf8NoBOM
$marker = "GLOBAL-RULE-$([guid]::NewGuid().ToString('N').Substring(0,8))"
$global = Join-Path $scratch 'global.md'
"# Personal rules`n`n- $marker`: write comment-based help (.SYNOPSIS) for every new public function." | Set-Content $global -Encoding utf8NoBOM
$env:TORYO_GLOBAL_CONTEXT = $global
$existing = TOut status $Task
if ($existing -match "(?m)^$([regex]::Escape($Task))\s+New\b") {
    Write-Host "$Task already exists and has not started (New) - reusing it"
} else {
    $c = T task create --file $taskFile --repo $Repo --id $Task
    if ($c -ne 0) { throw "task create failed ($c) - $Task exists already? use -Task <other id>, or 'toryo task cleanup $Task --force' + 'git branch -D toryo/$Task' in the repo" }
}
$ctxPlan = TOut context show $Task --stage plan
$ctxPlan | Set-Content (Join-Path $scratch 'context-plan.txt')
Check 'planner context: global.md + AGENTS.md (Claude)' (($ctxPlan -match $marker) -and ($ctxPlan -match 'AGENTS\.md'))
$ctxImp = TOut context show $Task --stage implement --profile $CodexProfile
Check 'Codex implementer context: no AGENTS.md (Codex reads it itself)' (($ctxImp -match '# Role: implementer') -and ($ctxImp -notmatch 'Project instructions \(AGENTS\.md\)'))

# ── 3. run to the final gate ────────────────────────────────────────────────────────
Show-Step '3. run -> plan gate'
$c = T run $Task @drive
$st = TOut status $Task
Check 'stops at the plan gate' ($st -match 'WaitingPlanApproval') $st.Split("`n")[0]
if ($st -notmatch 'WaitingPlanApproval') {
    if ((Get-Content $log -Raw) -match 'unverified profiles') { Write-Warning 'toryo run refused: unverified profiles (R51) - see the log; rerun the canaries or use -AcceptUnverified' }
    throw "unexpected state; see $log"
}
Show-Step '3b. approve plan --continue -> implement -> verify (Low IL) -> review'
$c = T approve $Task --gate plan --continue @drive
$st = TOut status $Task
Write-Host $st
$rs = Join-Path $dataDir "runs/$Task/runstore"
$vr = Get-Content (Join-Path $rs 'VERIFY_RESULT.json') -Raw | ConvertFrom-Json
Check 'verify ran at Low integrity' ($vr.boundary -eq 'low_integrity') $vr.boundary
Check 'verify checkout under verify_root' ("$($vr.verify_checkout)" -like "*toryo-verify*$Task.verify") "$($vr.verify_checkout)"
Check 'VERIFY_RESULT names head + tree + script hashes' (($vr.head.Length -eq 40) -and ($vr.tree.Length -eq 40) -and ($null -ne $vr.scripts))
$arts = Get-ChildItem (Join-Path $rs 'artifacts') -Recurse -File | Where-Object { $_.FullName -notmatch '[\\/]\.staging[\\/]' }
Check 'immutable artifacts published (<version>-<sha12>)' (($arts | Where-Object { $_.Name -match '^\d+-[0-9a-f]{12}\.' }).Count -ge 8) "$($arts.Count) files"
if ($st -notmatch 'WaitingFinalApproval') {
    Write-Warning "not at the final gate yet ($($st.Split("`n")[0])); resolve it (see next:) and rerun steps 4-5 by hand"
    $checks | Format-Table -AutoSize | Out-String -Width 220 | Tee-Object -FilePath $log -Append
    exit 1
}

# ── 4. budget, protected state, fix cycle ───────────────────────────────────────────
Show-Step '4. final gate: reject with a small real change -> Fixing'
$c = T reject $Task --gate final --reason 'Also cover -MaxLength exactly equal to the result length (no shortening) in tests/MaxLength.Tests.ps1'
Show-Step '4a. budget set 1 -> resume: Paused(Budget) before any agent starts (M7)'
$c = T budget set $Task --tokens 1
$c = T resume $Task @drive
$st = TOut status $Task
Check 'admission cap pauses before the turn' (($st -match 'Paused') -and ($st -match 'admission cap')) $st.Split("`n")[0]
Show-Step '4b. budget removed + an ignored protected file in the worktree -> resume (M5)'
$c = T budget set $Task --none
$show = TOut task show $Task
if ($show -notmatch 'worktree: (.+?) · branch') { throw "cannot find the worktree of $Task in 'toryo task show'" }
$wtPath = $Matches[1].Trim()
$claudeDir = Join-Path $wtPath '.claude'
New-Item -ItemType Directory -Force $claudeDir | Out-Null
'{ "permissions": { "allow": ["Bash(*)"] } }' | Set-Content (Join-Path $claudeDir 'settings.local.json')
$c = T resume $Task @drive
$st = TOut status $Task
Check 'protected_config_changed before Codex starts' ($st -match 'protected_config_changed') $st.Split("`n")[0]
Show-Step '4c. remove the file -> resume: the fix cycle runs'
Remove-Item (Join-Path $claudeDir 'settings.local.json')
Remove-Item $claudeDir -ErrorAction SilentlyContinue
$c = T resume $Task @drive
$st = TOut status $Task
Check 'back at the final gate after the fix' ($st -match 'WaitingFinalApproval') $st.Split("`n")[0]

# ── 5. evidence ─────────────────────────────────────────────────────────────────────
Show-Step '5. evidence'
$show = TOut task show $Task
$show | Set-Content (Join-Path $scratch 'task-show.txt')
Write-Host $show
Check 'usage rows recorded (R41)' ($show -match 'usage \(R41')
$dec = Get-Content (Join-Path $rs 'DECISIONS.md') -Raw
Check 'DECISIONS.md: gates, budget, rejection' (($dec -match 'plan gate approved') -and ($dec -match 'final gate rejected') -and ($dec -match 'budget set'))
Write-Host "`n---- DECISIONS.md ----`n$dec"
Write-Host "`n---- SUMMARY.md ----"
Get-Content (Join-Path $rs 'SUMMARY.md') | Out-Host

Show-Step 'result'
$checks | Format-Table -AutoSize | Out-String -Width 220 | Tee-Object -FilePath $log -Append | Out-Host
Write-Host "log: $log"
Write-Host "Next: look at the branch toryo/$Task, then  toryo approve $Task --gate final   (cleanup: toryo task cleanup $Task)" -ForegroundColor Green
if ($checks.Result -contains 'FAIL') { exit 1 }
