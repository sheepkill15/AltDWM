#Requires -RunAsAdministrator
param(
    [switch]$PerUser,
    [string]$InstallDir = "C:\Program Files\AltDWM"
)

$ErrorActionPreference = "Stop"
Write-Host "== AltDWM Uninstaller ==" -ForegroundColor Cyan

$regPath = if ($PerUser) { "HKCU:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon" } else { "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon" }
try {
    $cur = (Get-ItemProperty -Path $regPath -Name Shell -ErrorAction SilentlyContinue).Shell
    Write-Host "Current Shell: $cur"
    if ($cur -like "*alt-dwm*") {
        if ($PerUser) {
            Remove-ItemProperty -Path $regPath -Name Shell -ErrorAction SilentlyContinue
            Write-Host "Removed HKCU Shell (will default to explorer.exe)"
        } else {
            Set-ItemProperty -Path $regPath -Name Shell -Value "explorer.exe"
            Write-Host "Restored HKLM Shell = explorer.exe"
        }
    } else {
        Write-Host "Shell is not AltDWM, leaving as-is: $cur"
    }
} catch { Write-Warning $_ }

if (Test-Path $InstallDir) {
    Write-Host "Install dir exists: $InstallDir (not deleting automatically — remove manually if desired)"
}

Write-Host "`nDone. Log off/on or run explorer.exe to restore shell."
