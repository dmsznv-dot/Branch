# Run from an x64 Developer PowerShell with reviewed MinGW64 tools in PATH.
# This builds a source SDK; bundle verification remains a separate gate.
param([switch]$CheckPlan)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$arguments = @((Join-Path $repo 'scripts/native/build-target.py'), '--target', 'x86_64-pc-windows-msvc')
if ($CheckPlan) { $arguments += '--check-plan' }
& python @arguments
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
