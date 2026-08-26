param(
    [string]$InstallDir,
    [switch]$PerUser,
    [switch]$NoBuild
)

$ErrorActionPreference = "Stop"
$projectRoot = $PSScriptRoot

if (-not $PerUser) {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw "Per-machine installation requires an elevated PowerShell. Use -PerUser for an admin-free install."
    }
}

if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    $InstallDir = if ($PerUser) {
        Join-Path $env:LOCALAPPDATA "Programs\AltDWM"
    } else {
        "C:\Program Files\AltDWM"
    }
}
$InstallDir = [IO.Path]::GetFullPath($InstallDir)

Write-Host "== AltDWM Installer ==" -ForegroundColor Cyan
Write-Host "InstallDir: $InstallDir  PerUser: $PerUser  NoBuild: $NoBuild"

if (-not $NoBuild) {
    Write-Host "`n[1/4] Building release..." -ForegroundColor Yellow
    $env:PATH += ";$(Join-Path $env:USERPROFILE '.cargo\bin')"
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
} else {
    Write-Host "`n[1/4] Skipping build (-NoBuild)" -ForegroundColor Yellow
}

$sourceExe = Join-Path $projectRoot "target\release\alt-dwm.exe"
if (-not (Test-Path -LiteralPath $sourceExe)) { throw "Missing $sourceExe - build first" }

Write-Host "`n[2/4] Installing to $InstallDir..." -ForegroundColor Yellow
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Copy-Item -Force -LiteralPath $sourceExe -Destination (Join-Path $InstallDir "alt-dwm.exe")
Copy-Item -Force -LiteralPath (Join-Path $projectRoot "examples\config.example.toml") -Destination (Join-Path $InstallDir "config.example.toml") -ErrorAction SilentlyContinue
$scriptsDir = Join-Path $projectRoot "scripts"
if (Test-Path -LiteralPath $scriptsDir) {
    Copy-Item -Recurse -Force -LiteralPath $scriptsDir -Destination (Join-Path $InstallDir "scripts") -ErrorAction SilentlyContinue
}

$destExe = Join-Path $InstallDir "alt-dwm.exe"
Write-Host "Installed: $destExe"

Write-Host "`n[3/4] Writing default config..." -ForegroundColor Yellow
$configDir = if ($PerUser) { Join-Path $env:APPDATA "AltDWM" } else { $InstallDir }
New-Item -ItemType Directory -Force -Path $configDir | Out-Null
$configPath = Join-Path $configDir "config.toml"
if (-not (Test-Path -LiteralPath $configPath)) {
    & $destExe --config $configPath --generate-config
    if ($LASTEXITCODE -ne 0) { throw "AltDWM failed to generate $configPath" }
    Write-Host "Generated: $configPath"
} else {
    Write-Host "Exists, skipping: $configPath"
}

Write-Host "`n[4/4] Registering as shell..." -ForegroundColor Yellow
$winlogonPath = if ($PerUser) {
    "HKCU:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon"
} else {
    "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon"
}
$statePath = if ($PerUser) { "HKCU:\SOFTWARE\AltDWM" } else { "HKLM:\SOFTWARE\AltDWM" }
New-Item -Force -Path $winlogonPath | Out-Null
New-Item -Force -Path $statePath | Out-Null

$currentShell = Get-ItemPropertyValue -Path $winlogonPath -Name Shell -ErrorAction SilentlyContinue
$normalizedCurrent = if ($null -eq $currentShell) { "" } else { $currentShell.Trim().Trim('"') }
$existingBackup = Get-ItemPropertyValue -Path $statePath -Name PreviousShellPresent -ErrorAction SilentlyContinue
if (-not $normalizedCurrent.Equals($destExe, [StringComparison]::OrdinalIgnoreCase) -and $null -eq $existingBackup) {
    $hadPreviousShell = $null -ne $currentShell
    New-ItemProperty -Path $statePath -Name PreviousShellPresent -PropertyType DWord -Value ([int]$hadPreviousShell) -Force | Out-Null
    if ($hadPreviousShell) {
        New-ItemProperty -Path $statePath -Name PreviousShell -PropertyType String -Value $currentShell -Force | Out-Null
    } else {
        Remove-ItemProperty -Path $statePath -Name PreviousShell -ErrorAction SilentlyContinue
    }
}

$shellCommand = '"' + $destExe + '"'
Set-ItemProperty -Path $winlogonPath -Name Shell -Value $shellCommand
Write-Host "Set Shell = $shellCommand"
Write-Host "The previous shell value was saved under $statePath."

Write-Host "`nDone. Log off/on to use AltDWM." -ForegroundColor Green
$uninstallHint = if ($PerUser) { ".\uninstall.ps1 -PerUser" } else { ".\uninstall.ps1" }
Write-Host "Run $uninstallHint to restore the previous shell."
Write-Host "For uiAccess, sign the executable, enable uiAccess in alt-dwm.manifest, and install to a trusted location."
