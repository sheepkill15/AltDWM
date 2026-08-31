param(
    [string]$InstallDir,
    [switch]$PerUser,
    [switch]$NoBuild
)

$ErrorActionPreference = "Stop"
$projectRoot = $PSScriptRoot

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
    throw "AltDWM's installed shell runs elevated. Re-run this installer from an Administrator PowerShell."
}

if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    $InstallDir = Join-Path $env:ProgramFiles "AltDWM"
}
$InstallDir = [IO.Path]::GetFullPath($InstallDir)
$secureRoots = @(
    [IO.Path]::GetFullPath($env:ProgramFiles)
) | Select-Object -Unique
$isSecureInstallDir = $false
foreach ($root in $secureRoots) {
    $rootPrefix = $root.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if ($InstallDir.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        $isSecureInstallDir = $true
        break
    }
}
if (-not $isSecureInstallDir) {
    throw "The elevated shell must be installed below Program Files so normal processes cannot replace it: $InstallDir"
}

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
$configDir = $InstallDir
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
$winlogonPath = "HKCU:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon"
$statePath = "HKCU:\SOFTWARE\AltDWM"
if (-not (Test-Path -LiteralPath $winlogonPath)) {
    New-Item -Path $winlogonPath | Out-Null
}
if (-not (Test-Path -LiteralPath $statePath)) {
    New-Item -Path $statePath | Out-Null
}

$taskPath = "\"
$taskName = "AltDWM-Shell-$($identity.User.Value)"
$taskFullName = "$taskPath$taskName"
$taskArguments = '--config "' + $configPath + '"'
$taskAction = New-ScheduledTaskAction -Execute $destExe -Argument $taskArguments -WorkingDirectory $InstallDir
$taskPrincipal = New-ScheduledTaskPrincipal -UserId $identity.User.Value -LogonType Interactive -RunLevel Highest
$taskSettings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew
Register-ScheduledTask -TaskPath $taskPath -TaskName $taskName -Action $taskAction -Principal $taskPrincipal -Settings $taskSettings -Description "Elevated AltDWM replacement shell for $($identity.Name)" -Force | Out-Null
$registeredTask = Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName
if ($registeredTask.Principal.RunLevel -ne "Highest") {
    throw "Scheduled task $taskFullName was not registered with Highest run level."
}

$schedulerExe = Join-Path $env:SystemRoot "System32\schtasks.exe"
$shellCommand = '"' + $schedulerExe + '" /Run /TN "' + $taskFullName + '"'
$legacyShellCommand = '"' + $destExe + '"'
$currentShell = Get-OptionalRegistryValue -Path $winlogonPath -Name Shell
$normalizedCurrent = if ($null -eq $currentShell) { "" } else { $currentShell.Trim() }
$existingBackup = Get-OptionalRegistryValue -Path $statePath -Name PreviousShellPresent
if (-not $normalizedCurrent.Equals($shellCommand, [StringComparison]::OrdinalIgnoreCase) -and
    -not $normalizedCurrent.Equals($legacyShellCommand, [StringComparison]::OrdinalIgnoreCase) -and
    $null -eq $existingBackup) {
    $hadPreviousShell = $null -ne $currentShell
    New-ItemProperty -Path $statePath -Name PreviousShellPresent -PropertyType DWord -Value ([int]$hadPreviousShell) -Force | Out-Null
    if ($hadPreviousShell) {
        New-ItemProperty -Path $statePath -Name PreviousShell -PropertyType String -Value $currentShell -Force | Out-Null
    } else {
        Remove-ItemProperty -Path $statePath -Name PreviousShell -ErrorAction SilentlyContinue
    }
}

New-ItemProperty -Path $statePath -Name ScheduledTaskPath -PropertyType String -Value $taskPath -Force | Out-Null
New-ItemProperty -Path $statePath -Name ScheduledTaskName -PropertyType String -Value $taskName -Force | Out-Null
New-ItemProperty -Path $statePath -Name InstalledShell -PropertyType String -Value $shellCommand -Force | Out-Null
Set-ItemProperty -Path $winlogonPath -Name Shell -Value $shellCommand
Write-Host "Set Shell = $shellCommand"
Write-Host "Registered $taskFullName with Highest run level."
Write-Host "The previous shell value was saved under $statePath."

Write-Host "`nDone. Log off/on to use AltDWM." -ForegroundColor Green
Write-Host "Run .\uninstall.ps1 to restore the previous shell for this account."
Write-Host "The installed shell starts through Task Scheduler with administrator privileges."
Write-Host "Windows will still prevent control of protected/system processes."
