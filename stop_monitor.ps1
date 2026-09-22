<#
.SYNOPSIS
    Stops the PLH Rack Monitor backend.

.DESCRIPTION
    Stops the process recorded in .plh_monitor.pid. If that file is missing or
    stale, the process listening on the configured port is used instead, but
    only after confirming it answers the PLH health endpoint, so an unrelated
    process on the same port is never stopped.
#>

[CmdletBinding()]
param(
    [string]$BindAddress = "127.0.0.1",
    [int]$Port = 8765
)

$ErrorActionPreference = "Stop"

$ProjectRoot = Split-Path -Parent $MyInvocation.MyCommand.Definition
$PidFile     = Join-Path $ProjectRoot ".plh_monitor.pid"
$EnvFile     = Join-Path $ProjectRoot ".env"

if ((Test-Path $EnvFile) -and -not $PSBoundParameters.ContainsKey("Port")) {
    $portLine = Select-String -Path $EnvFile -Pattern '^\s*APP_PORT\s*=\s*(\d+)' -ErrorAction SilentlyContinue |
                Select-Object -First 1
    if ($portLine) { $Port = [int]$portLine.Matches[0].Groups[1].Value }
}

$HealthUrl = "http://" + $BindAddress + ":" + $Port + "/api/health"

function Stop-ById([int]$processId, [string]$reason) {
    try {
        Stop-Process -Id $processId -Force -ErrorAction Stop
        Write-Host "[PLH] Stopped backend PID $processId ($reason)"
        return $true
    } catch {
        Write-Warning "Could not stop PID ${processId}: $($_.Exception.Message)"
        return $false
    }
}

function Test-Answers {
    try {
        $response = Invoke-WebRequest -Uri $HealthUrl -UseBasicParsing -TimeoutSec 2
        return ($response.StatusCode -eq 200) -and ($response.Content -match '"status"')
    } catch {
        return $false
    }
}

$stopped = $false

$listener = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue |
            Select-Object -First 1

if (Test-Path $PidFile) {
    $recorded = (Get-Content $PidFile -Raw).Trim()
    if ($recorded -match '^\d+$') {
        $recordedPid = [int]$recorded
        # The pid file names the most recently started instance, which may be
        # serving a different port. Use it only when it is the process serving
        # this port, or that process's launcher; otherwise leave it alone.
        $serving = $false
        if ($listener) {
            if ($listener.OwningProcess -eq $recordedPid) {
                $serving = $true
            } else {
                $owner = Get-CimInstance Win32_Process -Filter "ProcessId=$($listener.OwningProcess)" -ErrorAction SilentlyContinue
                $serving = [bool]($owner -and $owner.ParentProcessId -eq $recordedPid)
            }
        }
        if ($serving) {
            $stopped = Stop-ById $recordedPid "from .plh_monitor.pid"
            Remove-Item $PidFile -ErrorAction SilentlyContinue
        } elseif (Get-Process -Id $recordedPid -ErrorAction SilentlyContinue) {
            Write-Host "[PLH] .plh_monitor.pid names PID $recordedPid, which is not serving port $Port - left running."
        } else {
            Write-Host "[PLH] Recorded PID $recordedPid is not running."
            Remove-Item $PidFile -ErrorAction SilentlyContinue
        }
    } else {
        Remove-Item $PidFile -ErrorAction SilentlyContinue
    }
}

# Only a quiet health endpoint counts as stopped.
if ($stopped) {
    Start-Sleep -Milliseconds 700
    if (Test-Answers) {
        Write-Host "[PLH] Still answering on port $Port - stopping the process that owns the listener."
        $stopped = $false
    }
}

if (-not $stopped) {
    if (Test-Answers) {
        $listener = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue |
                    Select-Object -First 1
        if ($listener) {
            $stopped = Stop-ById $listener.OwningProcess "listening on port $Port"
        }
    } else {
        Write-Host "[PLH] Nothing answering $HealthUrl - backend does not appear to be running."
    }
}

if ($stopped) { Write-Host "[PLH] Backend stopped." }
