@echo off
rem Removes PLH Rack Monitor for the current user. The configuration is kept;
rem pass --purge to delete it as well.
setlocal
set "EXE=%LOCALAPPDATA%\Programs\PLH Rack Monitor\plh-rack-monitor.exe"
if not exist "%EXE%" set "EXE=%~dp0plh-rack-monitor.exe"
"%EXE%" uninstall %*
echo.
pause
