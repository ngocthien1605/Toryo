<#
.SYNOPSIS
  M3 acceptance on the real CLIs: `toryo stop` interrupts a running Reviewer, and
  `toryo resume` resumes the same Claude session (R38, R48). One command, no timing by hand.

.DESCRIPTION
  The task must be waiting at its final gate (e.g. BR-003 after its first review).
  1. Rejects the final gate with a small, real change request -> a fix cycle (Codex).
  2. A background watcher polls `toryo task show`; as soon as a review attempt is Running
     it waits a few seconds and sends `toryo stop`.
  3. `toryo resume` runs fix-1 -> checkpoint -> verify -> review, which the watcher stops.
  4. `toryo resume` again: the review resumes the interrupted Claude session.
  The final gate is left to you: `toryo approve <task> --gate final`.

.EXAMPLE
  pwsh -NoProfile -File scripts\m3-stop-test.ps1 -Task BR-003
#>
param(
    [string]$Task = 'BR-003',
    [string]$Config = $(if ($env:TORYO_CONFIG) { $env:TORYO_CONFIG } else { 'C:\.thien\m0\toryo.yaml' }),
    [int]$DelaySeconds = 3,
    [string]$Reason = 'Undo-Rename should output one object per restored file (OldName, NewName), like Rename-ByPattern does, and a test should check it'
)
$ErrorActionPreference = 'Stop'
# toryo writes UTF-8; decode piped output as UTF-8, not the OEM code page
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$toryo = (Resolve-Path (Join-Path $PSScriptRoot '..\bin\toryo.exe')).Path
$env:TORYO_CONFIG = $Config

function Show-Step([string]$text) { Write-Host "`n=== $text ===" -ForegroundColor Cyan }

Show-Step "0. $Task must be waiting at the final gate"
$show = & $toryo task show $Task 2>&1 | Out-String
Write-Host $show
if ($show -notmatch 'WaitingFinalApproval') { throw "$Task is not in WaitingFinalApproval - nothing to test" }

Show-Step '1. reject the final gate -> fix cycle'
& $toryo reject $Task --gate final --reason $Reason
if ($LASTEXITCODE -ne 0) { throw "reject failed ($LASTEXITCODE)" }

Show-Step "2. watcher: stop $DelaySeconds s after a review attempt starts"
$job = Start-Job -ArgumentList $toryo, $Config, $Task, $DelaySeconds -ScriptBlock {
    param($t, $c, $id, $delay)
    $env:TORYO_CONFIG = $c
    $deadline = (Get-Date).AddMinutes(30)
    while ((Get-Date) -lt $deadline) {
        $s = & $t task show $id 2>$null | Out-String
        if ($s -match "$([regex]::Escape($id))/review/\d+\s+Running") {
            Start-Sleep -Seconds $delay
            "watcher: review running -> toryo stop at $(Get-Date -Format HH:mm:ss)"
            & $t stop $id 2>&1 | ForEach-Object { "$_" }
            return
        }
        Start-Sleep -Seconds 1
    }
    'watcher: timed out (no review started within 30 min)'
}

Show-Step '3. resume: fix-1 -> checkpoint -> verify -> review (the watcher stops it)'
& $toryo resume $Task
$first = $LASTEXITCODE
$watch = Receive-Job $job -Wait -AutoRemoveJob | Out-String
Write-Host "---- watcher output ----`n$watch"
if ($watch -notmatch 'stopped') { Write-Warning 'the stop did not report "stopped" (see above); the review may have finished first - rerun with a smaller -DelaySeconds' }

Show-Step '4. resume again: the review resumes the interrupted Claude session'
& $toryo resume $Task

Show-Step 'result'
& $toryo task show $Task
Write-Host "`nfirst resume exit code: $first"
Write-Host "Next: check the result, then  toryo approve $Task --gate final" -ForegroundColor Green
