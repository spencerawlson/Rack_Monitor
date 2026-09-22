<#
.SYNOPSIS
    Starts the PLH Rack Monitor backend and opens the dashboard in Chrome.

.DESCRIPTION
    Locates the project, verifies the virtual environment, starts the FastAPI
    backend if it is not already listening, waits for the health endpoint to
    answer and then opens the dashboard. Running the script twice does not
    start a second backend.

.PARAMETER Port
    TCP port for the backend. Must match APP_PORT in .env when that is set.

.PARAMETER App
    Open Chrome in application mode: a window with no address bar or toolbar,
    which suits the 1424x280 panel. Fullscreen still works with F11.

.PARAMETER Fullscreen
    Ask Chrome to start fullscreen.

.PARAMETER NoBrowser
    Start the backend only.

.EXAMPLE
    .\start_monitor.ps1
.EXAMPLE
    .\start_monitor.ps1 -App -Fullscreen
#>

[CmdletBinding()]
param(
    [string]$BindAddress = "127.0.0.1",
    [int]$Port = 8765,
    [switch]$App,
    [switch]$Fullscreen,
    [switch]$NoBrowser
)

$ErrorActionPreference = "Stop"

$ProjectRoot = Split-Path -Parent $MyInvocation.MyCommand.Definition
$VenvPython  = Join-Path $ProjectRoot ".venv\Scripts\python.exe"
$PidFile     = Join-Path $ProjectRoot ".plh_monitor.pid"
$BaseUrl     = "http://" + $BindAddress + ":" + $Port
$HealthUrl   = $BaseUrl + "/api/health"

function Write-Step($message) { Write-Host "[PLH] $message" }

# --- 1. Project and virtual environment ------------------------------------

if (-not (Test-Path $VenvPython)) {
    Write-Error @"
Virtual environment not found at:
  $VenvPython

Create it and install the dependencies:
  cd "$ProjectRoot"
  py -3.14 -m venv .venv
  .\.venv\Scripts\python.exe -m pip install -r requirements.txt
"@
    exit 1
}

# APP_PORT in .env wins over the default, so the script and the app agree.
$EnvFile = Join-Path $ProjectRoot ".env"
if ((Test-Path $EnvFile) -and -not $PSBoundParameters.ContainsKey("Port")) {
    $portLine = Select-String -Path $EnvFile -Pattern '^\s*APP_PORT\s*=\s*(\d+)' -ErrorAction SilentlyContinue |
                Select-Object -First 1
    if ($portLine) {
        $Port = [int]$portLine.Matches[0].Groups[1].Value
        $BaseUrl = "http://" + $BindAddress + ":" + $Port
        $HealthUrl = $BaseUrl + "/api/health"
        Write-Step "Using APP_PORT=$Port from .env"
    }
}

# --- 2. Is a backend already running? --------------------------------------

function Test-Health {
    try {
        $response = Invoke-WebRequest -Uri $HealthUrl -UseBasicParsing -TimeoutSec 2
        return $response.StatusCode -eq 200
    } catch {
        return $false
    }
}

$alreadyRunning = Test-Health

if (-not $alreadyRunning) {
    # A listener that does not answer the health endpoint is something else on
    # this port; starting a second backend would just fail to bind.
    $listener = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue |
                Select-Object -First 1
    if ($listener) {
        Write-Error "Port $Port is in use by PID $($listener.OwningProcess), which is not PLH Rack Monitor. Stop it or choose another port with -Port."
        exit 1
    }
}

# --- 3. Start the backend ---------------------------------------------------

if ($alreadyRunning) {
    Write-Step "Backend already running on $BaseUrl - not starting another."
} else {
    Write-Step "Starting backend on $BaseUrl"
    $arguments = @(
        "-m", "uvicorn", "backend.main:app",
        "--host", $BindAddress,
        "--port", $Port,
        "--log-level", "warning"
    )
    $process = Start-Process -FilePath $VenvPython -ArgumentList $arguments `
        -WorkingDirectory $ProjectRoot -WindowStyle Hidden -PassThru
    Set-Content -Path $PidFile -Value $process.Id -Encoding ascii
    Write-Step "Backend PID $($process.Id)"

    # --- 4. Wait for the health endpoint ------------------------------------
    $deadline = (Get-Date).AddSeconds(40)
    $ready = $false
    while ((Get-Date) -lt $deadline) {
        if ($process.HasExited) {
            Write-Error "Backend exited during startup (exit code $($process.ExitCode)). Run it in the foreground to see why:`n  cd `"$ProjectRoot`"; .\.venv\Scripts\python.exe -m uvicorn backend.main:app"
            exit 1
        }
        if (Test-Health) { $ready = $true; break }
        Start-Sleep -Milliseconds 500
    }
    if (-not $ready) {
        Write-Error "Backend did not answer $HealthUrl within 40 seconds."
        exit 1
    }
    Write-Step "Health endpoint responded."

    # A venv's python.exe is a launcher that runs the real interpreter as a
    # child, and the child owns the socket. Record the listener rather than the
    # launcher, so stop_monitor.ps1 stops the process that is actually serving.
    $listener = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue |
                Select-Object -First 1
    if ($listener) {
        Set-Content -Path $PidFile -Value $listener.OwningProcess -Encoding ascii
        Write-Step "Serving PID $($listener.OwningProcess) (launcher $($process.Id))"
    }
}

# --- 5. Open the dashboard --------------------------------------------------

if ($NoBrowser) {
    Write-Step "Dashboard ready at $BaseUrl"
    exit 0
}

$chromePaths = @(
    "$env:ProgramFiles\Google\Chrome\Application\chrome.exe",
    "${env:ProgramFiles(x86)}\Google\Chrome\Application\chrome.exe",
    "$env:LocalAppData\Google\Chrome\Application\chrome.exe"
)
$chrome = $chromePaths | Where-Object { Test-Path $_ } | Select-Object -First 1

if ($chrome) {
    $chromeArgs = @()
    if ($App) { $chromeArgs += "--app=$BaseUrl" } else { $chromeArgs += @("--new-window", $BaseUrl) }
    if ($Fullscreen) { $chromeArgs += "--start-fullscreen" }
    Write-Step "Opening Chrome"
    Start-Process -FilePath $chrome -ArgumentList $chromeArgs | Out-Null
} else {
    Write-Step "Chrome not found; opening the default browser instead."
    Start-Process $BaseUrl | Out-Null
}

Write-Step "Dashboard ready at $BaseUrl  (F11 fullscreen, D detail, 0/1/2/3 views)"
