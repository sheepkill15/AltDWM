param(
    [switch]$PerUser,
    [string]$InstallDir
)

$ErrorActionPreference = "Stop"

function Get-OptionalRegistryValue {
    param(
        [Parameter(Mandatory)] [string] $Path,
        [Parameter(Mandatory)] [string] $Name
    )

    $item = Get-ItemProperty -LiteralPath $Path -ErrorAction SilentlyContinue
    if ($null -eq $item) { return $null }

    $property = $item.PSObject.Properties[$Name]
    if ($null -eq $property) { return $null }
    return $property.Value
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "AltDWM's elevated scheduled shell must be removed from an Administrator PowerShell."
}

if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    $InstallDir = Join-Path $env:ProgramFiles "AltDWM"
}
$InstallDir = [IO.Path]::GetFullPath($InstallDir)

Write-Host "== AltDWM Uninstaller ==" -ForegroundColor Cyan

$winlogonPath = "HKCU:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon"
$statePath = "HKCU:\SOFTWARE\AltDWM"
$destExe = Join-Path $InstallDir "alt-dwm.exe"
$legacyShellCommand = '"' + $destExe + '"'
$taskPath = Get-OptionalRegistryValue -Path $statePath -Name ScheduledTaskPath
$taskName = Get-OptionalRegistryValue -Path $statePath -Name ScheduledTaskName
$installedShell = Get-OptionalRegistryValue -Path $statePath -Name InstalledShell
if ([string]::IsNullOrWhiteSpace($taskPath)) { $taskPath = "\" }
if ([string]::IsNullOrWhiteSpace($taskName)) { $taskName = "AltDWM-Shell-$($identity.User.Value)" }
$currentShell = Get-OptionalRegistryValue -Path $winlogonPath -Name Shell
$normalizedCurrent = if ($null -eq $currentShell) { "" } else { $currentShell.Trim() }
$ownsCurrentShell = $normalizedCurrent.Equals($legacyShellCommand, [StringComparison]::OrdinalIgnoreCase) -or
    (-not [string]::IsNullOrWhiteSpace($installedShell) -and $normalizedCurrent.Equals($installedShell.Trim(), [StringComparison]::OrdinalIgnoreCase))

Write-Host "Current Shell: $currentShell"
if ($ownsCurrentShell) {
    $previousPresent = Get-OptionalRegistryValue -Path $statePath -Name PreviousShellPresent
    $previousShell = Get-OptionalRegistryValue -Path $statePath -Name PreviousShell
    if ($previousPresent -eq 1 -and -not [string]::IsNullOrWhiteSpace($previousShell)) {
        Set-ItemProperty -Path $winlogonPath -Name Shell -Value $previousShell
        Write-Host "Restored previous Shell = $previousShell"
    } else {
        Remove-ItemProperty -Path $winlogonPath -Name Shell -ErrorAction SilentlyContinue
        Write-Host "Removed the per-user Shell override."
    }
    Remove-Item -LiteralPath $statePath -Recurse -Force -ErrorAction SilentlyContinue
} else {
    Write-Warning "Shell does not exactly match this AltDWM installation; registry state was left unchanged."
}

$scheduledTask = Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName -ErrorAction SilentlyContinue
if ($null -ne $scheduledTask) {
    Unregister-ScheduledTask -TaskPath $taskPath -TaskName $taskName -Confirm:$false
    Write-Host "Removed scheduled task $taskPath$taskName"
}

if (Test-Path -LiteralPath $InstallDir) {
    Write-Host "Install directory retained for recovery: $InstallDir"
}

Write-Host "`nDone. Log off/on or run explorer.exe to start the restored shell."
