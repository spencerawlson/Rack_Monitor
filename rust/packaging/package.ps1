<#
.SYNOPSIS
    Builds the release executable and packs it into a distributable zip.

.DESCRIPTION
    Produces dist\plh-rack-monitor-<version>-windows-x86_64.zip containing the
    executable, install.cmd, uninstall.cmd, the annotated example configuration
    and the README, plus a .sha256 file beside the zip.

    The build uses the MSVC toolchain with a statically linked C runtime, so
    the executable runs on Windows 10 and 11 without the Visual C++
    redistributable.

.PARAMETER NoBuild
    Package the existing target\release build without rebuilding it.

.EXAMPLE
    .\packaging\package.ps1
#>

[CmdletBinding()]
param([switch]$NoBuild)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Definition)
Push-Location $root
try {
    if (-not $NoBuild) {
        cargo +stable-x86_64-pc-windows-msvc build --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    }

    $exe = Join-Path $root "target\release\plh-rack-monitor.exe"
    if (-not (Test-Path $exe)) { throw "No release build at $exe" }

    $version = (Select-String -Path (Join-Path $root "Cargo.toml") -Pattern '^version = "(.+)"' |
                Select-Object -First 1).Matches[0].Groups[1].Value
    $name  = "plh-rack-monitor-$version-windows-x86_64"
    $dist  = Join-Path $root "dist"
    $stage = Join-Path $dist $name
    $zip   = Join-Path $dist "$name.zip"

    if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
    New-Item -ItemType Directory -Force $stage | Out-Null
    Copy-Item $exe, (Join-Path $root "README.md"), (Join-Path $root "config.example.toml") $stage
    Copy-Item (Join-Path $root "packaging\install.cmd"), (Join-Path $root "packaging\uninstall.cmd") $stage

    if (Test-Path $zip) { Remove-Item -Force $zip }
    Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip
    $hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    Set-Content -Path "$zip.sha256" -Value "$hash  $name.zip" -Encoding ascii

    Write-Host "Packed $zip"
    Write-Host "SHA-256 $hash"
}
finally {
    Pop-Location
}
