#Requires -Version 7
<#
    M16 acceptance tests (Pester 5+): `toryo run A B C --jobs 2` — AT-1, AT-2, AT-3, AT-4, AT-8.
    The Ctrl+C tests (AT-5, AT-6, AT-7, AT-9) are in tests\e2e_m16.rs (a pseudo-console).

    Each Describe builds its own workspace with New-M16Fixture.ps1 in a fresh temp folder and
    removes it when every test of the block passed; otherwise the folder is kept, its path is
    printed and appended to test-results\kept-dirs.txt (for summary.md).

    toryo: $env:TORYO_E2E_BIN, else target\debug\toryo.exe. Its stdout and stderr go to files
    and are read back as UTF-8 (never `2>&1` into pwsh, never the console code page).
#>

BeforeAll {
    $script:RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
    $script:Toryo = if ($env:TORYO_E2E_BIN) { $env:TORYO_E2E_BIN } else { Join-Path $RepoRoot 'target\debug\toryo.exe' }
    $script:Results = Join-Path $RepoRoot 'test-results'
    New-Item -ItemType Directory -Force $Results | Out-Null
    $script:Sep = [char]0x2502   # │, the separator `run A B …` puts after a task id
    $script:Banner = [string][char]0x2550 * 10   # ══════════

    function New-Workspace([string]$Name) {
        $root = Join-Path ([IO.Path]::GetTempPath()) ("toryo-m16-$Name-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
        $info = & (Join-Path $PSScriptRoot 'New-M16Fixture.ps1') -Root $root -Toryo $script:Toryo
        $info | Add-Member -NotePropertyName Name -NotePropertyValue $Name -PassThru
    }

    # Runs toryo in workspace $W with its environment; returns exit code, stdout, stderr (UTF-8).
    # A run that does not end within $TimeoutSec is killed and fails the test with its output.
    function Invoke-M16($W, [string[]]$Arguments, [string]$Log, [int]$TimeoutSec = 300) {
        $out = Join-Path $W.root "$Log.out.txt"
        $err = Join-Path $W.root "$Log.err.txt"
        $all = @('--color', 'never', '--config', $W.config, '--machine-config', $W.machine) + $Arguments
        foreach ($k in $W.env.PSObject.Properties) { Set-Item "env:$($k.Name)" $k.Value }
        $p = Start-Process -FilePath $W.toryo -ArgumentList $all -NoNewWindow -PassThru `
            -RedirectStandardOutput $out -RedirectStandardError $err -WorkingDirectory $W.root
        if (-not $p.WaitForExit($TimeoutSec * 1000)) {
            $p.Kill($true)
            throw "toryo $($Arguments -join ' ') did not end within ${TimeoutSec}s (root $($W.root)):`n$((Get-Content -Encoding utf8 $err -Tail 80) -join "`n")"
        }
        [pscustomobject]@{
            Command = "toryo $($all -join ' ')"
            Code    = $p.ExitCode
            Out     = @(Get-Content -Encoding utf8 $out)
            Err     = @(Get-Content -Encoding utf8 $err)
        }
    }

    # What a failed assertion prints: the command, its exit code, the last 80 lines, the root.
    function Get-Context($W, $R) {
        "`n--- $($R.Command)`n--- exit $($R.Code), root $($W.root)`n" + (($R.Err | Select-Object -Last 80) -join "`n")
    }

    function Get-Events($W, [string]$Id) {
        $p = Join-Path $W.data "runs\$Id\events.jsonl"
        if (-not (Test-Path $p)) { return @() }
        @(Get-Content -Encoding utf8 $p | Where-Object { $_.Trim() } | ForEach-Object { $_ | ConvertFrom-Json -DateKind String })
    }

    function Get-Ts([string]$Ts) { [DateTimeOffset]::Parse($Ts, [Globalization.CultureInfo]::InvariantCulture) }

    function Remove-Workspace($W, [int]$Passed, [int]$Expected, [string]$Ids) {
        if ($Passed -eq $Expected) {
            Remove-Item -Recurse -Force $W.root -ErrorAction SilentlyContinue
        } else {
            Write-Host "KEPT $($W.root) (${Ids}: $Passed of $Expected passed)"
            Add-Content -Encoding utf8 (Join-Path $script:Results 'kept-dirs.txt') "$Ids`t$($W.root)"
        }
    }
}

Describe 'M16 run A B C --jobs 2' {
    BeforeAll {
        $script:W = New-Workspace 'jobs'
        $script:BaseBefore = (git -C $W.repo rev-parse main).Trim()
        $script:R = Invoke-M16 $W @('run', 'RUN-A', 'RUN-B', 'RUN-C', '--jobs', '2', '--yes', '--fake-agents', $W.fake_full) 'at1'
        $script:Ids = 'RUN-A', 'RUN-B', 'RUN-C'
        $script:Ev = @{}
        foreach ($id in $Ids) { $Ev[$id] = Get-Events $W $id }
        $script:Passed = 0
    }
    AfterAll { Remove-Workspace $W $Passed 5 'AT-1 AT-2 AT-3 AT-4 AT-8' }

    It 'AT-1: every task Completed (exit 0), each with SUMMARY.md and its branch, nothing merged' {
        $ctx = Get-Context $W $R
        $R.Code | Should -Be 0 -Because $ctx
        ($R.Err -contains "$Banner 3 tasks $Banner") | Should -BeTrue -Because "the end block of 3 tasks$ctx"
        foreach ($id in $Ids) {
            @($R.Err | Where-Object { $_ -match "^$id\s+Completed\s+\(exit 0\) — next: " }).Count | Should -Be 1 -Because "$id in the end block$ctx"
            Test-Path (Join-Path $W.data "runs\$id\runstore\SUMMARY.md") | Should -BeTrue -Because "$id SUMMARY.md"
            # the branch exists (`branch --list` marks one checked out in a worktree with `+`)
            & git -C $W.repo rev-parse --verify --quiet "refs/heads/toryo/$id" | Out-Null
            $LASTEXITCODE | Should -Be 0 -Because "the branch toryo/$id exists"
            & git -C $W.repo merge-base --is-ancestor "toryo/$id" main
            $LASTEXITCODE | Should -Not -Be 0 -Because "toryo/$id is not merged into main"
        }
        (git -C $W.repo rev-parse main).Trim() | Should -Be $BaseBefore -Because 'the base branch did not move'
        $script:Passed++
    }

    It 'AT-2: the header, then only lines prefixed with one task id, interleaved' {
        $ctx = Get-Context $W $R
        $head = [array]::IndexOf($R.Err, "▶ run RUN-A RUN-B RUN-C — 2 at a time (``toryo dashboard`` shows them all)")
        $head | Should -BeGreaterOrEqual 0 -Because "the header line$ctx"
        $end = [array]::LastIndexOf($R.Err, '')
        $banner = [array]::IndexOf($R.Err, "$Banner 3 tasks $Banner")
        # the end block starts with an empty line before the banner
        $between = $R.Err[($head + 1)..($banner - 2)]
        $between.Count | Should -BeGreaterThan 10 -Because $ctx
        $prefix = "^(RUN-[ABC]) $Sep( |$)"
        $owners = foreach ($l in $between) {
            $l | Should -Match $prefix -Because "every line between the header and the end block has a task id: [$l]"
            # one task's text only: no second prefix inside the line
            ([regex]::Matches($l, "RUN-[ABC] $Sep")).Count | Should -Be 1 -Because "[$l] mixes two tasks"
            ([regex]::Match($l, $prefix)).Groups[1].Value
        }
        foreach ($id in $Ids) { $owners | Should -Contain $id }
        # interleaved: some task prints, another prints, the first prints again
        $switches = 0
        for ($i = 1; $i -lt $owners.Count; $i++) { if ($owners[$i] -ne $owners[$i - 1]) { $switches++ } }
        $switches | Should -BeGreaterOrEqual 2 -Because 'the lines of at least two tasks interleave'
        # stdout: nothing, or only prefixed lines
        foreach ($l in $R.Out) { $l | Should -Match $prefix }
        $script:Passed++
    }

    It 'AT-3: never more than 2 runs at once; RUN-C starts after RUN-A or RUN-B ended' {
        $points = foreach ($id in $Ids) {
            $s = @($Ev[$id] | Where-Object type -eq 'run_started')
            $e = @($Ev[$id] | Where-Object type -eq 'run_ended')
            $s.Count | Should -Be 1 -Because "$id has one run_started"
            $e.Count | Should -Be 1 -Because "$id has one run_ended"
            [pscustomobject]@{ Id = $id; At = Get-Ts $s[0].ts; D = 1 }
            [pscustomobject]@{ Id = $id; At = Get-Ts $e[0].ts; D = -1 }
        }
        $open = 0; $max = 0
        foreach ($p in ($points | Sort-Object At, D)) { $open += $p.D; $max = [Math]::Max($max, $open) }
        $max | Should -Be 2 -Because 'two runs overlap, never three'
        $cStart = ($points | Where-Object { $_.Id -eq 'RUN-C' -and $_.D -eq 1 }).At
        $firstEnd = ($points | Where-Object { $_.Id -ne 'RUN-C' -and $_.D -eq -1 } | Sort-Object At | Select-Object -First 1).At
        $cStart | Should -BeGreaterThan $firstEnd -Because 'RUN-C starts in the slot of a finished run'
        $script:Passed++
    }

    It 'AT-4: a waiter logs lock_wait with the right holder before its release, and acquires after it' {
        $lock = 'machine:verify'
        $held = foreach ($id in $Ids) {
            $acq = @($Ev[$id] | Where-Object { $_.type -eq 'lock_acquired' -and $_.lock -eq $lock })
            $rel = @($Ev[$id] | Where-Object { $_.type -eq 'lock_released' -and $_.lock -eq $lock })
            $acq.Count | Should -Be $rel.Count -Because "$id releases what it takes"
            for ($i = 0; $i -lt $acq.Count; $i++) {
                [pscustomobject]@{ Id = $id; From = Get-Ts $acq[$i].ts; To = Get-Ts $rel[$i].ts }
            }
        }
        $held = @($held | Sort-Object From)
        $held.Count | Should -BeGreaterOrEqual 3 -Because 'each task took the verify lock'
        for ($i = 1; $i -lt $held.Count; $i++) {
            $held[$i].From | Should -BeGreaterThan $held[$i - 1].To -Because "$($held[$i].Id) takes the lock after $($held[$i-1].Id) released it"
        }
        $waits = foreach ($id in $Ids) { $Ev[$id] | Where-Object { $_.type -eq 'lock_waiting' -and $_.lock -eq $lock } | ForEach-Object { [pscustomobject]@{ Id = $id; E = $_ } } }
        @($waits).Count | Should -BeGreaterOrEqual 1 -Because 'two tasks verified at the same time: one waited'
        foreach ($w in $waits) {
            $at = Get-Ts $w.E.ts
            $holder = $held | Where-Object { $_.Id -ne $w.Id -and $_.From -le $at -and $at -lt $_.To } | Select-Object -First 1
            $holder | Should -Not -BeNullOrEmpty -Because "someone held the lock when $($w.Id) waited"
            $w.E.holder | Should -Be $holder.Id -Because "the waiting event of $($w.Id) names the holder"
            $mine = $held | Where-Object { $_.Id -eq $w.Id -and $_.From -gt $at } | Select-Object -First 1
            $mine.From | Should -BeGreaterThan $holder.To -Because "$($w.Id) acquires after $($holder.Id) released"
            $line = "^$($w.Id) $Sep\s+waiting for lock $([regex]::Escape($lock)) \(held by $($holder.Id)\)$"
            @($R.Err | Where-Object { $_ -match $line }).Count | Should -BeGreaterOrEqual 1 -Because "the waiting line, with the waiter's id$(Get-Context $W $R)"
        }
        $script:Passed++
    }

    It 'AT-8: the same command again runs nothing that was done' {
        $spawned = @{}
        foreach ($id in $Ids) { $spawned[$id] = @($Ev[$id] | Where-Object type -eq 'agent_spawned').Count }
        $again = Invoke-M16 $W @('run', 'RUN-A', 'RUN-B', 'RUN-C', '--jobs', '2', '--yes', '--fake-agents', $W.fake_full) 'at8'
        $ctx = Get-Context $W $again
        $again.Code | Should -Be 0 -Because $ctx
        foreach ($id in $Ids) {
            @($again.Err | Where-Object { $_ -match "^$id\s+Completed\s+\(exit 0\)" }).Count | Should -Be 1 -Because "$id still Completed$ctx"
            $now = Get-Events $W $id
            @($now | Where-Object type -eq 'agent_spawned').Count | Should -Be $spawned[$id] -Because "$id ran no agent again"
            @($now | Where-Object type -eq 'stage_started').Count | Should -Be @($Ev[$id] | Where-Object type -eq 'stage_started').Count -Because "$id started no stage again"
        }
        $script:Passed++
    }
}
