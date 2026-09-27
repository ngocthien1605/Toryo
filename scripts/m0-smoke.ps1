<#
.SYNOPSIS
  One-shot M0 smoke run for the Thien Orchestrator on this Windows machine.

.DESCRIPTION
  1. Installs Claude Code (official installer, https://claude.ai/install.ps1) if it is missing.
  2. Checks Claude login, Codex CLI, Git, PowerShell 7.
  3. Writes C:\.thien\m0\thien.yaml for the detected paths (Claude Max + your Codex login).
  4. Runs thien.exe: doctor, job-test, git-canary, and the end-to-end smoke test
     (Claude plans -> Codex implements -> thien checkpoints + verifies -> Claude reviews).
  Everything is logged to C:\.thien\AgentOrchestrator\thien-m0-data\ so the results can be read back.

.EXAMPLE
  pwsh -NoProfile -ExecutionPolicy Bypass -File C:\.thien\AgentOrchestrator\thien\scripts\m0-smoke.ps1
.EXAMPLE
  ... m0-smoke.ps1 -SmokeCodexProfile foundry      # use the Foundry (API) profile as Implementer
#>
[CmdletBinding()]
param(
    [switch]$SkipInstall,
    [switch]$SkipSmoke,
    [string]$ClaudeModel = "",
    [string]$CodexModel = "",
    # Codex profiles in ~/.codex/config.toml to import (codex --profile <name>)
    [string[]]$CodexProfiles = @('openai', 'foundry'),
    # Which one the end-to-end smoke test uses as the Implementer
    [string]$SmokeCodexProfile = 'openai',
    [string]$WorkDir = 'C:\.thien\m0',
    # Codex config holding [profiles.<name>] (default: $env:CODEX_HOME\config.toml, else ~/.codex/config.toml)
    [string]$CodexConfig = ""
)

$ErrorActionPreference = 'Continue'
$root     = Split-Path -Parent $PSScriptRoot                     # ...\thien
$thien    = Join-Path $root 'bin\thien.exe'
if ($env:THIEN_EXE) { $thien = $env:THIEN_EXE }   # testing hook
$dataDir  = Join-Path (Split-Path -Parent $root) 'thien-m0-data' # ...\AgentOrchestrator\thien-m0-data
$m0       = $WorkDir
New-Item -ItemType Directory -Force -Path $dataDir, $m0 | Out-Null
$env:THIEN_DATA_DIR = $dataDir
$log = Join-Path $dataDir ("smoke-console-{0:yyyyMMdd-HHmmss}.log" -f (Get-Date))
Start-Transcript -Path $log | Out-Null

function Section($t) { Write-Host "`n==== $t ====" -ForegroundColor Cyan }
function Save($name, $text) { Set-Content -Path (Join-Path $dataDir $name) -Value $text -Encoding utf8 }

Section "Environment"
# Pick up tools installed since this terminal was opened (e.g. Git via winget).
$regPath = @([Environment]::GetEnvironmentVariable('Path', 'Machine'), [Environment]::GetEnvironmentVariable('Path', 'User')) -join ';'
foreach ($d in ($regPath -split ';')) { if ($d -and ($env:PATH -split ';') -notcontains $d) { $env:PATH = "$env:PATH;$d" } }
"PowerShell $($PSVersionTable.PSVersion) on $([Environment]::OSVersion.VersionString)"
if (-not (Test-Path $thien)) { Write-Error "thien.exe not found at $thien"; Stop-Transcript | Out-Null; exit 2 }
& $thien --version

# ---------------------------------------------------------------- Claude Code
Section "Claude Code"
$localBin = Join-Path $env:USERPROFILE '.local\bin'
if ($env:PATH -notlike "*$localBin*") { $env:PATH = "$localBin;$env:PATH" }
$claude = Get-Command claude -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $claude -and -not $SkipInstall) {
    "Claude Code not found - running the official installer (irm https://claude.ai/install.ps1 | iex)"
    Invoke-RestMethod https://claude.ai/install.ps1 | Invoke-Expression
    $claude = Get-Command claude -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $claude -and (Test-Path (Join-Path $localBin 'claude.exe'))) {
        $claude = Get-Item (Join-Path $localBin 'claude.exe')
    }
}
if (-not $claude) { Write-Error "Claude Code is not installed."; Stop-Transcript | Out-Null; exit 3 }
$claudeExe = if ($claude.Source) { $claude.Source } else { $claude.FullName }
"claude: $claudeExe"
& $claudeExe --version
& $claudeExe --help 2>&1 | Out-String | ForEach-Object { Save 'claude-help.txt' $_ }
$help = Get-Content (Join-Path $dataDir 'claude-help.txt') -Raw
foreach ($f in '--restricted','--safe-mode','--setting-sources','--permission-prompts','--tools','--json-schema','--strict-mcp-config','--add-dir','--append-system-prompt-file') {
    "{0,-28} {1}" -f $f, ($(if ($help -match [regex]::Escape($f)) { 'listed in --help' } else { 'NOT listed in --help' }))
}

"Checking Claude login with a one-line prompt..."
$probe = & $claudeExe -p "Reply with exactly: OK" --output-format json 2>&1 | Out-String
Save 'claude-login-probe.txt' $probe
if ($probe -notmatch '"is_error"\s*:\s*false') {
    Write-Warning "Claude does not seem to be logged in (see claude-login-probe.txt)."
    Write-Host  "Run 'claude' in a new PowerShell window, complete /login with your Max account, type /exit, then re-run this script." -ForegroundColor Yellow
    Stop-Transcript | Out-Null
    exit 4
}
"Claude login OK"

# ---------------------------------------------------------------- Codex
Section "Codex CLI"
$codexAll = @(Get-Command codex -CommandType Application -ErrorAction SilentlyContinue)
$codex = $codexAll | Where-Object { $_.Source -match '\.(exe|cmd)$' } | Select-Object -First 1
if (-not $codex) { $codex = $codexAll | Select-Object -First 1 }
if (-not $codex) { Write-Error "codex (.exe/.cmd) not found in PATH"; Stop-Transcript | Out-Null; exit 5 }
$codexExe = $codex.Source
"codex: $codexExe"
& $codexExe --version
$loginStatus = & $codexExe login status 2>&1 | Out-String
"codex login status: $($loginStatus.Trim())"
$codexHome = if ($env:CODEX_HOME) { $env:CODEX_HOME } else { Join-Path $env:USERPROFILE '.codex' }
$codexCfg = if ($CodexConfig) { $CodexConfig } else { Join-Path $codexHome 'config.toml' }
"CODEX_HOME: $codexHome $(if ($env:CODEX_HOME) { '(from env)' } else { '(default)' })"
if (Test-Path $codexCfg) {
    "table headers in ${codexCfg} (values not shown):"
    Select-String -Path $codexCfg -Pattern '^\s*\[' | ForEach-Object { "  " + $_.Line.Trim() }
} else { Write-Warning "$codexCfg not found" }
$codexHelp = & $codexExe --help 2>&1 | Out-String
"codex --profile supported: $($codexHelp -match '--profile')"

# ---------------------------------------------------------------- Git / Git Bash
Section "Git"
$git = Get-Command git -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
$gitPath = if ($git) { $git.Source } else {
    @('C:\Program Files\Git\cmd\git.exe', "$env:LOCALAPPDATA\Programs\Git\cmd\git.exe", "$env:USERPROFILE\scoop\shims\git.exe") +
      @(Get-ChildItem "$env:LOCALAPPDATA\GitHubDesktop\app-*\resources\app\git\cmd\git.exe" -ErrorAction SilentlyContinue | ForEach-Object FullName) |
      Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (-not $gitPath) {
    Write-Host "Git for Windows is not installed. thien needs it (worktrees, checkpoints, canaries); Claude Code also uses its Git Bash." -ForegroundColor Yellow
    Write-Host "Install it, then re-run this script (no new terminal needed):" -ForegroundColor Yellow
    Write-Host "    winget install --id Git.Git -e --source winget" -ForegroundColor Yellow
    Stop-Transcript | Out-Null
    exit 7
}
$env:THIEN_GIT = $gitPath
if (($env:PATH -split ';') -notcontains (Split-Path $gitPath)) { $env:PATH = "$(Split-Path $gitPath);$env:PATH" }
"git: $gitPath"
& $gitPath --version
$gitBash = @('C:\Program Files\Git\bin\bash.exe', "$env:LOCALAPPDATA\Programs\Git\bin\bash.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
"Git Bash: $gitBash"

# ---------------------------------------------------------------- config
Section "thien.yaml"
$calc = Join-Path $m0 'calc'
$contracts = Join-Path $m0 'contracts'
if (-not (Test-Path (Join-Path $calc '.git'))) { & $thien spike init-fixture --into $calc --contracts $contracts }
elseif (-not (Test-Path $contracts)) { & $thien spike init-fixture --into (Join-Path $m0 ("calc-{0}" -f (Get-Date -Format yyyyMMddHHmmss))) --contracts $contracts }
$cfgPath = Join-Path $m0 'thien.yaml'
$initArgs = @('spike', 'init-config', '--out', $cfgPath, '--project-root', $calc, '--worktrees-root', "$m0\worktrees",
              '--claude-exe', $claudeExe, '--codex-exe', $codexExe, '--codex-config', $codexCfg,
              '--codex-profiles', ($CodexProfiles -join ','), '--lenient')
if ($gitBash) { $initArgs += @('--git-bash', $gitBash) }
& $thien @initArgs
if ($LASTEXITCODE -ne 0) { Write-Error "init-config failed"; Stop-Transcript | Out-Null; exit 6 }
Copy-Item $cfgPath (Join-Path $dataDir 'thien.yaml') -Force

# ---------------------------------------------------------------- thien
Set-Location $m0
Section "thien doctor"
& $thien --config $cfgPath doctor
Section "thien spike job-test (R23)"
& $thien --config $cfgPath spike job-test
Section "thien spike git-canary (R33/R37, M0-VERIFY 10)"
& $thien --config $cfgPath spike git-canary
Section "Codex quick check per profile (one turn each, no edits)"
$probePrompt = Join-Path $m0 'codex-probe.md'
Set-Content -Path $probePrompt -Encoding utf8 -Value 'Do not run any command and do not edit any file. Return the JSON object with summary = "ok", and every other field = [].'
$implSchema = Join-Path $m0 'contracts\schemas\implementation.schema.json'
$generated = Get-Content $cfgPath -Raw
foreach ($p in $CodexProfiles) {
    if ($generated -notmatch "(?m)^\s+codex-${p}:") { "--- codex-${p}: not configured (see warnings above), skipped ---"; continue }
    "--- codex-$p ---"
    & $thien --config $cfgPath spike codex --profile "codex-$p" --label "probe-$p" --worktree $calc --prompt-file $probePrompt --schema $implSchema
}
if (-not $SkipSmoke -and $generated -notmatch "(?m)^\s+codex-${SmokeCodexProfile}:") {
    Write-Warning "codex-$SmokeCodexProfile is not configured; smoke test skipped"
    $SkipSmoke = $true
}
if (-not $SkipSmoke) {
    Section "thien spike smoke (Claude plans -> Codex [$SmokeCodexProfile] implements -> verify -> Claude reviews)"
    $smokeArgs = @('--config', $cfgPath, 'spike', 'smoke', '--claude-profile', 'claude-max', '--codex-profile', "codex-$SmokeCodexProfile")
    if ($ClaudeModel) { $smokeArgs += @('--claude-model', $ClaudeModel) }
    if ($CodexModel)  { $smokeArgs += @('--codex-model', $CodexModel) }
    & $thien @smokeArgs
}
Section "Report"
& $thien --config $cfgPath spike report --out (Join-Path $dataDir 'M0-RESULTS.generated.md')
Write-Host "`nAll output is in $dataDir" -ForegroundColor Green
Stop-Transcript | Out-Null
