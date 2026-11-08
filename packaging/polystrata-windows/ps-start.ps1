# Lemonade Stand start for PolyStrata and Nauti-Strata on Windows: ps-start.ps1 PORT DIR SRC
# Takes the run config setup wrote, sets this engine's port, writes strata-stand.json and runs the server.
#   NAUTI_STRATA_GPU       card to use, numbered as nvidia-smi numbers them (default: the one setup chose)
#   NAUTI_STRATA_THINKING  thinking level when a request names none: none (default), low, medium or high
# NautiBurn routing scrapes are OFF by default (they record each answer's token ids and routed experts). To turn
# them on, write a folder path into DIR\nautiburn-scrape-dir (or set NAUTIBURN_SCRAPE_DIR); delete the file to turn
# them off. Only engines built with the scraper write anything. Takes effect at the next start.
param([Parameter(Mandatory)][int]$Port, [Parameter(Mandatory)][string]$Dir, [Parameter(Mandatory)][string]$Src)
$ErrorActionPreference = "Stop"
$repo = Join-Path $Dir $Src
$src = Get-ChildItem $repo -Filter "strata-*.json" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $src) { throw "no run config in ${repo}: run Set up again" }
$cfg = Get-Content $src.FullName -Raw | ConvertFrom-Json
$cfg | Add-Member -Force host "127.0.0.1"
$cfg | Add-Member -Force port $Port
$cfg | Add-Member -Force open_browser $false
$think = if ($env:NAUTI_STRATA_THINKING) { $env:NAUTI_STRATA_THINKING } else { "none" }
$cfg | Add-Member -Force reasoning_effort $think
if ($env:NAUTI_STRATA_GPU) { $cfg | Add-Member -Force gpu @([int]$env:NAUTI_STRATA_GPU) }
$scrape = $env:NAUTIBURN_SCRAPE_DIR
$flag = Join-Path $Dir "nautiburn-scrape-dir"
if (-not $scrape -and (Test-Path $flag)) { $scrape = "$(Get-Content $flag -TotalCount 1)".Trim() }
$envMap = [ordered]@{}
if ($cfg.env) { $cfg.env.PSObject.Properties | Where-Object { $_.Name -notlike "NAUTIBURN_*" } | ForEach-Object { $envMap[$_.Name] = $_.Value } }
if ($scrape) {
    New-Item -ItemType Directory -Force $scrape | Out-Null
    $envMap["NAUTIBURN_SCRAPE_DIR"] = $scrape
    $envMap["NAUTIBURN_MODEL_ID"] = "$($cfg.model_name)"
    $envMap["NAUTIBURN_CLASS_ID"] = "stand-$($cfg.model_name)"
    Write-Host "NautiBurn scrapes: ON -> $scrape"
} else { Write-Host "NautiBurn scrapes: off" }
$cfg | Add-Member -Force env ([pscustomobject]$envMap)
$out = Join-Path $Dir "strata-stand.json"
[IO.File]::WriteAllText($out, ($cfg | ConvertTo-Json -Depth 20))     # no BOM
Set-Location $repo
& (Join-Path $repo ".venv\Scripts\python.exe") serve\server.py --engine strata --config $out --host 127.0.0.1 --port $Port
exit $LASTEXITCODE
