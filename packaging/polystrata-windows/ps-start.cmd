@echo off
rem Lemonade Stand start for PolyStrata / Nauti-Strata on Windows: ps-start.cmd PORT DIR SRC
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0ps-start.ps1" %*
exit /b %errorlevel%
