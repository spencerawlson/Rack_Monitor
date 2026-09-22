@echo off
rem Installs PLH Rack Monitor for the current user. No administrator rights
rem are needed. Extra options are passed through, for example:
rem   install.cmd --autostart      also start the dashboard at sign-in
setlocal
set "HERE=%~dp0"
"%HERE%plh-rack-monitor.exe" install %*
if errorlevel 1 (
  echo.
  echo Installation failed. Details are in the lines above.
  pause
  exit /b 1
)
echo.
echo Installed. Start it any time from the Start Menu: PLH Rack Monitor
echo Configuration: %APPDATA%\PLH Rack Monitor\config\config.toml
pause
