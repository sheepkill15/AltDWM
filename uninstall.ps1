param(
    [switch]$PerUser,
    [string]$InstallDir
)

$ErrorActionPreference = "Stop"

if (-not $PerUser) {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw "Per-machine uninstall requires an elevated PowerShell. Use -PerUser for a per-user install."
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

Write-Host "== AltDWM Uninstaller ==" -ForegroundColor Cyan

$winlogonPath = if ($PerUser) {
    "HKCU:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon"
} else {
    "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon"
}
$statePath = if ($PerUser) { "HKCU:\SOFTWARE\AltDWM" } else { "HKLM:\SOFTWARE\AltDWM" }
$destExe = Join-Path $InstallDir "alt-dwm.exe"
$currentShell = Get-ItemPropertyValue -Path $winlogonPath -Name Shell -ErrorAction SilentlyContinue
$normalizedCurrent = if ($null -eq $currentShell) { "" } else { $currentShell.Trim().Trim('"') }

Write-Host "Current Shell: $currentShell"
if ($normalizedCurrent.Equals($destExe, [StringComparison]::OrdinalIgnoreCase)) {
    $previousPresent = Get-ItemPropertyValue -Path $statePath -Name PreviousShellPresent -ErrorAction SilentlyContinue
    $previousShell = Get-ItemPropertyValue -Path $statePath -Name PreviousShell -ErrorAction SilentlyContinue
    if ($previousPresent -eq 1 -and -not [string]::IsNullOrWhiteSpace($previousShell)) {
        Set-ItemProperty -Path $winlogonPath -Name Shell -Value $previousShell
        Write-Host "Restored previous Shell = $previousShell"
    } elseif ($PerUser) {
        Remove-ItemProperty -Path $winlogonPath -Name Shell -ErrorAction SilentlyContinue
        Write-Host "Removed the per-user Shell override."
    } else {
        Set-ItemProperty -Path $winlogonPath -Name Shell -Value "explorer.exe"
        Write-Host "No backup was available; restored Shell = explorer.exe"
    }
    Remove-Item -LiteralPath $statePath -Recurse -Force -ErrorAction SilentlyContinue
} else {
    Write-Warning "Shell does not exactly match $destExe; registry state was left unchanged."
}

if (Test-Path -LiteralPath $InstallDir) {
    Write-Host "Install directory retained for recovery: $InstallDir"
}

Write-Host "`nDone. Log off/on or run explorer.exe to start the restored shell."
