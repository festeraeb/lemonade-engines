# Lemonade Stand install step for PolyStrata and Nauti-Strata on Windows.
# Runs the project's own setup without questions: Python (installed for this user if missing), its packages, the
# model (sha256-checked by setup), the tokenizer and the run config. Mainline downloads its ready-made engine;
# Nauti-Strata passes --build and compiles (setup installs Visual Studio Build Tools and CUDA when missing).
#   ps-install.ps1 DIR SRC [setup options...]     SRC = the unpacked source folder inside DIR
param([Parameter(Mandatory)][string]$Dir, [Parameter(Mandatory)][string]$Src, [Parameter(ValueFromRemainingArguments)][string[]]$SetupArgs)
$ErrorActionPreference = "Stop"
$repo = Join-Path $Dir $Src
if (-not (Test-Path (Join-Path $repo "START-HERE.bat"))) { throw "no START-HERE.bat in $repo" }
Set-Location $repo
$cmdline = "START-HERE.bat " + (($SetupArgs + @("--yes", "--no-start", "--no-browser")) -join " ") + " < NUL"
Write-Host "running: $cmdline"
cmd /c $cmdline                 # < NUL: START-HERE.bat pauses on a failure, and nobody is there to press a key
if ($LASTEXITCODE -ne 0) { throw "setup failed (exit $LASTEXITCODE)" }
$cfg = Get-ChildItem $repo -Filter "strata-*.json" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $cfg) { throw "setup wrote no strata-*.json run config" }
Write-Host "installed: $($cfg.Name)"
