# Nauti-Strata (our PolyStrata) on Windows: build it, test it, and pack the result for the Lemonade Stand.
#
# Run in a normal PowerShell window (not as administrator). Windows asks for permission while the tools install.
# The first run takes 1 to 2 hours: about 10 GB of tools, a 13.6 GB model and two compiles.
# Needs: Windows 10/11 x64, an NVIDIA card with 8 GB or more (GTX 10-series or newer), a current NVIDIA driver,
# 24 GB of RAM or more, about 40 GB of free disk.
#
# At the end it writes nauti-strata-windows-result.zip in your user folder, also when something failed.
# Send that file back (a OneDrive or Google Drive link is fine; it is about 1 GB).
#
# What it does:
#   1. installs git, the CUDA 12.8 toolkit and Visual Studio 2022 Build Tools (C++) if they are missing
#      (CUDA 12.8, not 13: CUDA 13 dropped the GTX 10-series and Tesla P100 cards this build keeps)
#   2. downloads Nauti-Strata at a fixed commit
#   3. runs PolyStrata's own setup: Python, its packages, the LFM2.5-24B-A2B model (sha256-checked), the tokenizer
#      and the run config; it also compiles an engine for this PC's card
#   4. compiles the engine the Stand would ship: CUDA cards sm_60, sm_61, sm_75 and sm_86, AVX2 CPUs
#   5. starts that engine, asks it one question, stops it, and packs the engine, its CUDA DLLs and all logs

$ErrorActionPreference = "Stop"
$Commit = "0ab32e8530e1d0c8d430843eadc015e467aff928"     # compat/pascal-sm60-cublas-avx
$Root = Join-Path $env:USERPROFILE "nauti-strata-test"
$Repo = Join-Path $Root "Nauti-Strata"
$Build = Join-Path $Repo "build-stand"
$Out = Join-Path $Root "nauti-strata-windows"
$Zip = Join-Path $env:USERPROFILE "nauti-strata-windows-result.zip"
$Log = Join-Path $Root "log.txt"
New-Item -ItemType Directory -Force $Root | Out-Null
Start-Transcript -Path $Log -Append | Out-Null
$wg = @("--exact", "--source", "winget", "--accept-package-agreements", "--accept-source-agreements", "--disable-interactivity")
function Refresh-Path {
    $env:Path = [Environment]::GetEnvironmentVariable("Path", "Machine") + ";" + [Environment]::GetEnvironmentVariable("Path", "User")
}
function Find-VS {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path $vswhere) {
        & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    }
}
$Cuda = $null
$srv = $null
$result = "FAILED before the test"

try {
    Write-Host "== 1/5 tools"
    if (-not (Get-Command git -ErrorAction SilentlyContinue)) { winget install --id Git.Git @wg; Refresh-Path }
    $Cuda = [Environment]::GetEnvironmentVariable("CUDA_PATH_V12_8", "Machine")
    if (-not $Cuda) { winget install --id Nvidia.CUDA --version 12.8 @wg; $Cuda = [Environment]::GetEnvironmentVariable("CUDA_PATH_V12_8", "Machine") }
    if (-not $Cuda -or -not (Test-Path "$Cuda\bin\nvcc.exe")) {
        throw "CUDA 12.8 did not install. 'winget show Nvidia.CUDA --versions' lists what winget has; install a 12.8 there or from developer.nvidia.com, then run this again."
    }
    if (-not (Find-VS)) {
        winget install --id Microsoft.VisualStudio.2022.BuildTools @wg --override "--quiet --wait --norestart --nocache --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
    }
    $VS = Find-VS
    if (-not $VS) { throw "Visual Studio 2022 Build Tools (C++) did not install" }
    $Vcvars = Join-Path $VS "VC\Auxiliary\Build\vcvars64.bat"
    Refresh-Path
    $env:CUDA_PATH = $Cuda

    Write-Host "== 2/5 source"
    if (-not (Test-Path (Join-Path $Repo ".git"))) { git clone https://github.com/festeraeb/Nauti-Strata.git $Repo }
    git -C $Repo fetch origin
    git -C $Repo checkout --force $Commit
    if ($LASTEXITCODE -ne 0) { throw "git could not check out $Commit" }

    Write-Host "== 3/5 PolyStrata setup (Python, model download, a first compile for this card)"
    Push-Location $Repo
    cmd /c "START-HERE.bat --build --cuda 12 --family lfm25 --model QAD-Q4_0 --yes --no-start --no-browser"
    $code = $LASTEXITCODE
    Pop-Location
    if ($code -ne 0) { throw "PolyStrata setup failed (exit $code)" }

    Write-Host "== 4/5 the Stand build (sm_60, sm_61, sm_75, sm_86; AVX2)"
    $Cmake = Join-Path $Repo ".venv\Scripts\cmake.exe"
    $Ninja = Join-Path $Repo ".venv\Scripts\ninja.exe"
    $bat = Join-Path $Root "build-stand.bat"
    @"
@echo off
call "$Vcvars" >nul || exit /b 1
"$Cmake" -G Ninja "-DCMAKE_MAKE_PROGRAM=$Ninja" -S "$Repo" -B "$Build" -DCMAKE_BUILD_TYPE=Release -DSTRATA_ENABLE_CUDA=ON -DSTRATA_EXPERIMENTAL_SM60=ON -DSTRATA_PORTABLE=ON -DSTRATA_BUILD_TESTS=OFF "-DCMAKE_CUDA_ARCHITECTURES=60;61;75;86" "-DCMAKE_CUDA_COMPILER=$Cuda\bin\nvcc.exe" "-DCMAKE_CUDA_FLAGS=-allow-unsupported-compiler" || exit /b 1
"$Cmake" --build "$Build" --target strata-poly || exit /b 1
"@ | Set-Content -Encoding ASCII $bat
    cmd /c $bat
    if ($LASTEXITCODE -ne 0) { throw "the Stand build failed" }

    Write-Host "== 5/5 test: start the Stand build, ask one question, stop"
    $src = Get-ChildItem $Repo -Filter "strata-lfm25*.json" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $src) { throw "setup wrote no strata-lfm25*.json config" }
    $cfg = Get-Content $src.FullName -Raw | ConvertFrom-Json
    $cfg | Add-Member -Force exe (Join-Path $Build "strata-poly.exe")
    $cfg | Add-Member -Force lib_dirs (@("$Cuda\bin") + @($cfg.lib_dirs | Where-Object { $_ }))
    $cfg | Add-Member -Force open_browser $false
    $testCfg = Join-Path $Root "strata-stand-test.json"
    [IO.File]::WriteAllText($testCfg, ($cfg | ConvertTo-Json -Depth 20))
    $py = Join-Path $Repo ".venv\Scripts\python.exe"
    $srv = Start-Process $py -PassThru -WorkingDirectory $Repo -WindowStyle Hidden `
        -ArgumentList @("serve\server.py", "--engine", "strata", "--config", "`"$testCfg`"", "--host", "127.0.0.1", "--port", "8095") `
        -RedirectStandardOutput (Join-Path $Root "server-out.txt") -RedirectStandardError (Join-Path $Root "server-err.txt")
    $ready = $false; $t0 = Get-Date
    while (((Get-Date) - $t0).TotalMinutes -lt 30 -and -not $srv.HasExited) {
        try { if ((Invoke-RestMethod -TimeoutSec 5 http://127.0.0.1:8095/health).status -eq "ok") { $ready = $true; break } } catch {}
        Start-Sleep 5
    }
    if (-not $ready) { throw "the server did not become ready in 30 minutes (see server-out.txt / server-err.txt)" }
    $load = [int]((Get-Date) - $t0).TotalSeconds
    $body = @{ model = $cfg.model_name; max_tokens = 64
               messages = @(@{ role = "user"; content = "In one sentence, what is a lighthouse for?" }) } | ConvertTo-Json -Depth 5
    $t1 = Get-Date
    $r = Invoke-RestMethod -Method Post -Uri http://127.0.0.1:8095/v1/chat/completions -ContentType "application/json" -Body $body -TimeoutSec 900
    $secs = [math]::Round(((Get-Date) - $t1).TotalSeconds, 1)
    $answer = "$($r.choices[0].message.content)".Trim()
    if (-not $answer) { throw "the server answered with no text: $($r | ConvertTo-Json -Depth 6 -Compress)" }
    $result = "PASS`nanswer: $answer`ntokens: $($r.usage.completion_tokens) in $secs s`nload: $load s"
    Write-Host $result
}
catch {
    $result = "FAILED: $_"
    Write-Host $result -ForegroundColor Red
}
finally {
    if ($srv -and -not $srv.HasExited) { Stop-Process -Id $srv.Id -Force -ErrorAction SilentlyContinue }
    Get-Process strata-poly -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $Out -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $Out | Out-Null
    $result | Out-File -Encoding utf8 (Join-Path $Out "RESULT.txt")
    $exe = Join-Path $Build "strata-poly.exe"
    if (Test-Path $exe) {
        Copy-Item $exe $Out
        (Get-FileHash $exe -Algorithm SHA256).Hash | Out-File -Encoding ascii (Join-Path $Out "strata-poly.exe.sha256")
        foreach ($d in "cudart64_12.dll", "cublas64_12.dll", "cublasLt64_12.dll") {
            if ($Cuda -and (Test-Path "$Cuda\bin\$d")) { Copy-Item "$Cuda\bin\$d" $Out }
        }
    }
    try { nvidia-smi | Out-File -Encoding utf8 (Join-Path $Out "nvidia-smi.txt") } catch {}
    Get-CimInstance Win32_Processor | Format-List Name, NumberOfCores, NumberOfLogicalProcessors | Out-File -Encoding utf8 (Join-Path $Out "cpu.txt")
    (Get-CimInstance Win32_OperatingSystem).Caption | Out-File -Encoding utf8 (Join-Path $Out "windows.txt")
    try { git -C $Repo rev-parse HEAD | Out-File -Encoding ascii (Join-Path $Out "commit.txt") } catch {}
    foreach ($f in "server-out.txt", "server-err.txt", "build-stand.bat") {
        if (Test-Path (Join-Path $Root $f)) { Copy-Item (Join-Path $Root $f) $Out }
    }
    Get-ChildItem $Repo -Filter "strata-lfm25*.log" -ErrorAction SilentlyContinue | Copy-Item -Destination $Out
    Stop-Transcript | Out-Null
    Copy-Item $Log $Out
    Compress-Archive -Force -Path "$Out\*" -DestinationPath $Zip
    Write-Host ""
    Write-Host "Done. Send this file back: $Zip"
}
