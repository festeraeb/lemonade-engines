@echo off
rem Builds the "iced lemon cake" package for Windows x86-64 (see build-linux.sh for what it contains).
rem Needs: Visual Studio 2022 Build Tools (MSVC + the CMake/Ninja that ship with it), git, cargo, curl, tar,
rem and a Python 3 for CMake's code generation (the python.org "embeddable" zip unpacked anywhere is enough).
rem
rem   build-windows.cmd WORKDIR FORK_REPO BRIDGE_REPO PYTHON_EXE
rem   FORK_REPO   = a clone of lemonade-fork with branch iced-lemcake/2026.43.3
rem   BRIDGE_REPO = a clone of lemonade_cake_integration with branch bridge/runtime-name-2026-10-22
rem   PYTHON_EXE  = full path with forward slashes, e.g. C:/tools/py/python.exe
setlocal
set "W=%~1" & set "FORK=%~2" & set "BRIDGE=%~3" & set "PY=%~4"
set "N=iced-lemcake-2026.43.3-x86_64-windows"
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
set "PATH=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja;%PATH%"
mkdir "%W%\src" "%W%\bridge" "%W%\burn" "%W%\%N%"
git -C "%FORK%" archive iced-lemcake/2026.43.3 | tar x -C "%W%\src"
git -C "%BRIDGE%" archive bridge/runtime-name-2026-10-22 | tar x -C "%W%\bridge"
cd /d "%W%\src"
git init -q & git apply "%~dp00001-port-cake-and-burn-backends-to-2026.43.3.patch" || exit /b 1
cmake -B build -G Ninja -DCMAKE_BUILD_TYPE=Release -DBUILD_WEB_APP=OFF -DPython3_EXECUTABLE=%PY% || exit /b 1
cmake --build build --target lemond lemonade || exit /b 1
cd /d "%W%\bridge\rust_bridge" & cargo build --release || exit /b 1
cd /d "%W%"
copy /y src\build\lemond.exe %N%\ & copy /y src\build\Release\lemonade.exe %N%\
xcopy /e /i /q src\build\resources %N%\resources
copy /y bridge\rust_bridge\target\release\lemonade-cake-bridge.exe %N%\
copy /y src\LICENSE %N%\LICENSE-lemonade
curl -fsSL -o burn\burn.tar.gz https://github.com/festeraeb/lemonade-burn/releases/download/v0.1.1/lemonade-burn-server-0.1.1-x86_64-windows.tar.gz
certutil -hashfile burn\burn.tar.gz SHA256
rem (must print d3ead93d40b42bc00096b71d54d9a24d3d169ffcc04a663cec286115696189ed)
tar xzf burn\burn.tar.gz -C burn
copy /y burn\lemonade-burn-server.exe %N%\ & copy /y burn\LICENSE-* %N%\
copy /y "%~dp00001-*.patch" %N%\
tar -czf %N%.tar.gz -C %N% .
certutil -hashfile %N%.tar.gz SHA256
