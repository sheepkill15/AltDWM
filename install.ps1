#Requires -RunAsAdministrator
param(
    [string]$InstallDir = "C:\Program Files\AltDWM",
    [switch]$PerUser,
    [switch]$NoBuild
)

$ErrorActionPreference = "Stop"
$Root = $PSScriptRoot

Write-Host "== AltDWM Installer ==" -ForegroundColor Cyan
Write-Host "InstallDir: $InstallDir  PerUser: $PerUser  NoBuild: $NoBuild"

if (-not $NoBuild) {
    Write-Host "`n[1/4] Building release..." -ForegroundColor Yellow
    $env:PATH += ";$HOME\.cargo\bin"
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
} else {
    Write-Host "`n[1/4] Skipping build (--NoBuild)" -ForegroundColor Yellow
}

$exe = Join-Path $Root "target\release\alt-dwm.exe"
if (-not (Test-Path $exe)) { throw "Missing $exe - build first" }

Write-Host "`n[2/4] Installing to $InstallDir..." -ForegroundColor Yellow
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Copy-Item -Force $exe (Join-Path $InstallDir "alt-dwm.exe")
Copy-Item -Force (Join-Path $Root "examples\config.example.toml") (Join-Path $InstallDir "config.example.toml") -ErrorAction SilentlyContinue
if (Test-Path (Join-Path $Root "scripts")) { Copy-Item -Recurse -Force (Join-Path $Root "scripts") (Join-Path $InstallDir "scripts") -ErrorAction SilentlyContinue }

$destExe = Join-Path $InstallDir "alt-dwm.exe"
Write-Host "Installed: $destExe"

Write-Host "`n[3/4] Writing default config..." -ForegroundColor Yellow
$cfgDir = if ($PerUser) { Join-Path $env:APPDATA "AltDWM" } else { $InstallDir }
New-Item -ItemType Directory -Force -Path $cfgDir | Out-Null
$cfgPath = Join-Path $cfgDir "config.toml"
if (-not (Test-Path $cfgPath)) {
    & $destExe --config $cfgPath --generate-config
    Write-Host "Generated: $cfgPath"
} else {
    Write-Host "Exists, skipping: $cfgPath (use --generate-config to overwrite)"
}

Write-Host "`n[4/4] Registering as shell..." -ForegroundColor Yellow
if ($PerUser) {
    $regPath = "HKCU:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon"
    New-Item -Force -Path $regPath | Out-Null
    Set-ItemProperty -Path $regPath -Name Shell -Value $destExe
    Write-Host "Set HKCU Shell = $destExe"
    Write-Host "Log off/on to test. Restore: Remove-ItemProperty -Path $regPath -Name Shell  (or set to explorer.exe)"
} else {
    $regPath = "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon"
    Set-ItemProperty -Path $regPath -Name Shell -Value $destExe
    # Also keep explorer as fallback via Shell value? No, Shell is single.
    Write-Host "Set HKLM Shell = $destExe"
    Write-Host "Restore: Set-ItemProperty -Path $regPath -Name Shell -Value explorer.exe"
}

Write-Host "`nDone. Test without reboot:" -ForegroundColor Green
Write-Host "  taskkill /f /im explorer.exe  # AltDWM keeps tiling"
Write-Host "  explorer.exe                  # restore"
Write-Host "`nFor uiAccess (move elevated windows): sign exe, set uiAccess=true in alt-dwm.manifest, reinstall to $InstallDir, rebuild."

# Verify manifest dpiAware
Write-Host "`nTip: cargo check -- building is enough per user request (no run during install)."
