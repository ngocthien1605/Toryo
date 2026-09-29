<#
.SYNOPSIS
  The verify gate of this repository when Toryo builds Toryo (M9-D1): what CI runs — fmt, clippy
  (with and without the M0 kit), tests.

.DESCRIPTION
  Toryo runs it in the verify checkout (templates/dogfood/toryo.yaml.example), at Low integrity:
  there cargo may not write %USERPROFILE%\.cargo, so CARGO_HOME and CARGO_TARGET_DIR point at the
  Low-writable folders Toryo gives every verify command (XDG_CACHE_HOME, TORYO_VERIFY_BUILD). Crates
  download into that cache on the first run. Run by hand, outside Toryo, cargo's defaults apply.
  --locked keeps cargo from rewriting Cargo.lock, which Toryo would report as verify_mutated_tree.

.EXAMPLE
  pwsh -NoProfile -File scripts\verify.ps1
#>
$ErrorActionPreference = 'Stop'
if ($env:XDG_CACHE_HOME)     { $env:CARGO_HOME       = Join-Path $env:XDG_CACHE_HOME 'cargo' }
if ($env:TORYO_VERIFY_BUILD) { $env:CARGO_TARGET_DIR = Join-Path $env:TORYO_VERIFY_BUILD 'target' }

cargo fmt --all -- --check;                                                if ($LASTEXITCODE) { exit $LASTEXITCODE }
cargo clippy --locked --all-targets -- -D warnings;                        if ($LASTEXITCODE) { exit $LASTEXITCODE }
cargo clippy --locked --all-targets --no-default-features -- -D warnings;  if ($LASTEXITCODE) { exit $LASTEXITCODE }
cargo test --locked
exit $LASTEXITCODE
