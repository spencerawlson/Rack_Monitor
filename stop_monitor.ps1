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

$stopped = $false

if (Test-Path $PidFile) {
    $recorded = (Get-Content $PidFile -Raw).Trim()
    if ($recorded -match '^\d+$') {
        $process = Get-Process -Id ([int]$recorded) -ErrorAction SilentlyContinue
        if ($process) {
            $stopped = Stop-ById ([int]$recorded) "from .plh_monitor.pid"
        } else {
            Write-Host "[PLH] Recorded PID $recorded is not running."
        }
    }
    Remove-Item $PidFile -ErrorAction SilentlyContinue
}

if (-not $stopped) {
    $answers = $false
    try {
        $response = Invoke-WebRequest -Uri $HealthUrl -UseBasicParsing -TimeoutSec 2
        $answers = ($response.StatusCode -eq 200) -and ($response.Content -match '"status"')
    } catch {
        $answers = $false
    }

    if ($answers) {
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
