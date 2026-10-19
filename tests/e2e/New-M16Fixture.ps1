<#
.SYNOPSIS
    A fresh, hermetic M16 workspace in -Root (the acceptance tests' fixture, M16-AT).

.DESCRIPTION
    Under -Root (created if missing; must be empty or new):
      repo\          the BatchRename sample (`toryo spike init-fixture --app batch-rename`), one
                     commit on main — its build.ps1 / test.ps1 are two of the check gates
      toryo.yaml     from tests\fixtures\m16\toryo.yaml.template, every path inside -Root
      machine.yaml   fake profiles (no real CLI), `verify: { min_boundary: none }`
      tasks\         RUN-A/B/C and INT-A/B/C task specs
      fake-*.json    the fake-agent scripts (full, slow implementer, slow planner)
      data\          TORYO_DATA_DIR: the store, run stores, locks
      m16.json       every path above, the toryo binary and the environment to run it with
    Unless -NoTasks, the six tasks are created (`toryo task create`). Nothing outside -Root is
    written; nothing under C:\.thien\demo is read.

    Every toryo call of a test must use the environment in m16.json (TORYO_DATA_DIR,
    TORYO_GLOBAL_CONTEXT) and pass --config / --machine-config.
#>
param(
    [Parameter(Mandatory)][string]$Root,
    [string]$Toryo = (Join-Path $PSScriptRoot '..\..\target\debug\toryo.exe'),
    [switch]$NoTasks
)
$ErrorActionPreference = 'Stop'
$fixtures = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\m16')).Path
$Toryo = (Resolve-Path $Toryo).Path
if ((Test-Path $Root) -and (Get-ChildItem -Force $Root | Select-Object -First 1)) {
    throw "New-M16Fixture: $Root is not empty"
}
New-Item -ItemType Directory -Force $Root | Out-Null
$Root = (Resolve-Path $Root).Path
$fwd = $Root -replace '\\', '/'
$repo = Join-Path $Root 'repo'
$data = Join-Path $Root 'data'
$config = Join-Path $Root 'toryo.yaml'
$machine = Join-Path $Root 'machine.yaml'
$env:TORYO_DATA_DIR = $data
$env:TORYO_GLOBAL_CONTEXT = Join-Path $Root 'global.md'
$env:TORYO_RETRY_BASE_SECS = '0'

function Invoke-Toryo([string[]]$Arguments, [string]$Log) {
    $out = Join-Path $Root "$Log.out.txt"
    $err = Join-Path $Root "$Log.err.txt"
    $p = Start-Process -FilePath $Toryo -ArgumentList $Arguments -NoNewWindow -Wait -PassThru `
        -RedirectStandardOutput $out -RedirectStandardError $err -WorkingDirectory $Root
    if ($p.ExitCode -ne 0) {
        throw "toryo $($Arguments -join ' ') exited $($p.ExitCode):`n$(Get-Content -Raw -Encoding utf8 $err)"
    }
}

Invoke-Toryo @('spike', 'init-fixture', '--app', 'batch-rename', '--into', $repo) 'fixture.init'
(Get-Content -Raw -Encoding utf8 (Join-Path $fixtures 'toryo.yaml.template')).
    Replace('@REPO@', "$fwd/repo").Replace('@ROOT@', $fwd) |
    Set-Content -NoNewline -Encoding utf8NoBOM $config
Copy-Item (Join-Path $fixtures 'machine.yaml') $machine
Copy-Item (Join-Path $fixtures 'fake-*.json') $Root
Copy-Item -Recurse (Join-Path $fixtures 'tasks') (Join-Path $Root 'tasks')

$ids = 'RUN-A', 'RUN-B', 'RUN-C', 'INT-A', 'INT-B', 'INT-C'
if (-not $NoTasks) {
    foreach ($id in $ids) {
        Invoke-Toryo @('--config', $config, '--machine-config', $machine, 'task', 'create',
            '--file', (Join-Path $Root "tasks\$id.md")) "fixture.create.$id"
    }
}

$info = [ordered]@{
    root           = $Root
    repo           = $repo
    data           = $data
    config         = $config
    machine        = $machine
    toryo          = $Toryo
    gates_log      = Join-Path $Root 'gates.log'
    fake_full      = Join-Path $Root 'fake-full.json'
    fake_slow      = Join-Path $Root 'fake-slow.json'
    fake_slow_plan = Join-Path $Root 'fake-slow-plan.json'
    tasks          = $(if ($NoTasks) { @() } else { $ids })
    env            = [ordered]@{
        TORYO_DATA_DIR        = $data
        TORYO_GLOBAL_CONTEXT  = Join-Path $Root 'global.md'
        TORYO_RETRY_BASE_SECS = '0'
    }
}
$info | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8NoBOM (Join-Path $Root 'm16.json')
[pscustomobject]$info
