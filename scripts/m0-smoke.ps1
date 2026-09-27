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
#>
[CmdletBinding()]
param(
    [switch]$SkipInstall,
    [switch]$SkipSmoke,
    [string]$ClaudeModel = "",
    [string]$CodexModel = ""
)

$ErrorActionPreference = 'Continue'
$root     = Split-Path -Parent $PSScriptRoot                     # ...\thien
$thien    = Join-Path $root 'bin\thien.exe'
$dataDir  = Join-Path (Split-Path -Parent $root) 'thien-m0-data' # ...\AgentOrchestrator\thien-m0-data
$m0       = 'C:\.thien\m0'
New-Item -ItemType Directory -Force -Path $dataDir, $m0 | Out-Null
$env:THIEN_DATA_DIR = $dataDir
$log = Join-Path $dataDir ("smoke-console-{0:yyyyMMdd-HHmmss}.log" -f (Get-Date))
Start-Transcript -Path $log | Out-Null

function Section($t) { Write-Host "`n==== $t ====" -ForegroundColor Cyan }
function Save($name, $text) { Set-Content -Path (Join-Path $dataDir $name) -Value $text -Encoding utf8 }

Section "Environment"
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
$codex = Get-Command codex -CommandType Application -ErrorAction SilentlyContinue |
    Where-Object { $_.Source -match '\.(exe|cmd)$' } | Select-Object -First 1
if (-not $codex) { Write-Error "codex (.exe/.cmd) not found in PATH"; Stop-Transcript | Out-Null; exit 5 }
$codexExe = $codex.Source
"codex: $codexExe"
& $codexExe --version
$loginStatus = & $codexExe login status 2>&1 | Out-String
"codex login status: $($loginStatus.Trim())"
$codexBilling = if ($loginStatus -match 'ChatGPT') { 'subscription' } elseif ($loginStatus -match 'API key') { 'api' } else { 'subscription' }
$userCfg = Join-Path $env:USERPROFILE '.codex\config.toml'
if (Test-Path $userCfg) {
    $prov = Select-String -Path $userCfg -Pattern '^\s*model_provider\s*=' -ErrorAction SilentlyContinue
    if ($prov) { Write-Warning "Your ~/.codex/config.toml sets '$($prov.Line.Trim())'. thien runs Codex with --ignore-user-config, so that provider is NOT used; Codex falls back to your login." }
}

# ---------------------------------------------------------------- Git / Git Bash
Section "Git"
git --version
$gitBash = @('C:\Program Files\Git\bin\bash.exe', "$env:LOCALAPPDATA\Programs\Git\bin\bash.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
"Git Bash: $gitBash"

# ---------------------------------------------------------------- config
Section "thien.yaml"
$calc = Join-Path $m0 'calc'
if (-not (Test-Path (Join-Path $calc '.git'))) { & $thien spike init-fixture --into $calc --contracts (Join-Path $m0 'contracts') }
$envBlock = if ($gitBash) { "    env:`n      CLAUDE_CODE_GIT_BASH_PATH: '$gitBash'" } else { "    env: {}" }
$yaml = @"
# generated by m0-smoke.ps1 on $(Get-Date -Format s)
version: 1
project:
  root: '$calc'
  worktrees_root: '$m0\worktrees'
profiles:
  claude-max:
    kind: claude
    billing: subscription
    exe: '$claudeExe'
$envBlock
  codex:
    kind: codex
    billing: $codexBilling
    exe: '$codexExe'
    codex:
      sandbox_mode: workspace-write
      windows_sandbox: unelevated
roles:
  planner:     { profile: claude-max }
  implementer: { profile: codex }
  reviewer:    { profile: claude-max }
"@
$cfgPath = Join-Path $m0 'thien.yaml'
Set-Content -Path $cfgPath -Value $yaml -Encoding utf8
Get-Content $cfgPath
Copy-Item $cfgPath (Join-Path $dataDir 'thien.yaml') -Force

# ---------------------------------------------------------------- thien
Set-Location $m0
Section "thien doctor"
& $thien --config $cfgPath doctor
Section "thien spike job-test (R23)"
& $thien --config $cfgPath spike job-test
Section "thien spike git-canary (R33/R37, M0-VERIFY 10)"
& $thien --config $cfgPath spike git-canary
if (-not $SkipSmoke) {
    Section "thien spike smoke (Claude plans -> Codex implements -> verify -> Claude reviews)"
    $smokeArgs = @('--config', $cfgPath, 'spike', 'smoke', '--claude-profile', 'claude-max', '--codex-profile', 'codex')
    if ($ClaudeModel) { $smokeArgs += @('--claude-model', $ClaudeModel) }
    if ($CodexModel)  { $smokeArgs += @('--codex-model', $CodexModel) }
    & $thien @smokeArgs
}
Section "Report"
& $thien --config $cfgPath spike report --out (Join-Path $dataDir 'M0-RESULTS.generated.md')
Write-Host "`nAll output is in $dataDir" -ForegroundColor Green
Stop-Transcript | Out-Null
