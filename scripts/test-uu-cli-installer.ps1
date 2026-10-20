#Requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$PackageRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') {
    throw 'This clean-machine installer test runs only in GitHub Actions.'
}
if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
    throw 'RUNNER_TEMP is required for the isolated installer test.'
}

function Read-RegistryValue {
    param([string]$SubKey, [string]$Name)
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($SubKey)
    if ($null -eq $key) {
        return [pscustomobject]@{ KeyExisted = $false; Exists = $false; Value = $null; Kind = $null }
    }
    try {
        $exists = @($key.GetValueNames()) -contains $Name
        if ($exists) {
            return [pscustomobject]@{
                KeyExisted = $true
                Exists = $true
                Value = $key.GetValue($Name, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
                Kind = $key.GetValueKind($Name)
            }
        }
        return [pscustomobject]@{ KeyExisted = $true; Exists = $false; Value = $null; Kind = $null }
    }
    finally {
        $key.Dispose()
    }
}

function Restore-RegistryValue {
    param([string]$SubKey, [string]$Name, $Snapshot)
    if ($Snapshot.KeyExisted) {
        $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($SubKey)
    }
    else {
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($SubKey, $true)
    }
    if ($null -eq $key) {
        return
    }

    $deleteKey = $false
    try {
        if ($Snapshot.Exists) {
            $key.SetValue($Name, $Snapshot.Value, $Snapshot.Kind)
        }
        else {
            $key.DeleteValue($Name, $false)
        }
        $deleteKey = -not $Snapshot.KeyExisted -and @($key.GetValueNames()).Count -eq 0
    }
    finally {
        $key.Dispose()
    }
    if ($deleteKey) {
        [Microsoft.Win32.Registry]::CurrentUser.DeleteSubKey($SubKey, $false)
    }
}

function Normalize-PathEntry {
    param([string]$Entry)
    $value = [Environment]::ExpandEnvironmentVariables($Entry.Trim().Trim('"'))
    try {
        $value = [IO.Path]::GetFullPath($value)
    }
    catch {
    }
    return $value.TrimEnd([char[]]@('\', '/'))
}

function Count-UserPathEntry {
    param([string]$Target)
    $snapshot = Read-RegistryValue -SubKey 'Environment' -Name 'Path'
    if (-not $snapshot.Exists) {
        return 0
    }
    $key = Normalize-PathEntry $Target
    $count = 0
    foreach ($entry in @(([string]$snapshot.Value) -split ';')) {
        if (-not [string]::IsNullOrWhiteSpace($entry) -and
            (Normalize-PathEntry $entry) -ieq $key) {
            $count++
        }
    }
    return $count
}

$oldLocalAppData = [Environment]::GetEnvironmentVariable('LOCALAPPDATA', 'Process')
$oldConfigOverride = [Environment]::GetEnvironmentVariable('UU_CLI_CONFIG', 'Process')
$oldProcessPath = $env:PATH
$oldUserPath = Read-RegistryValue -SubKey 'Environment' -Name 'Path'
$oldRunValue = Read-RegistryValue -SubKey 'Software\Microsoft\Windows\CurrentVersion\Run' -Name 'uu-cli'
$testRoot = Join-Path $env:RUNNER_TEMP ('uu-cli-installer-' + [guid]::NewGuid().ToString('N'))
$localAppData = Join-Path $testRoot 'localappdata'
$configPath = Join-Path $testRoot 'custom-state\binding.json'
$installRoot = Join-Path $localAppData 'Programs\uu-cli'
$package = [IO.Path]::GetFullPath($PackageRoot)
$installer = Join-Path $package 'installer\install.ps1'
$uninstaller = Join-Path $package 'installer\uninstall.ps1'
$configBytes = $null
$testFailure = $null

try {
    New-Item -ItemType Directory -Path (Split-Path -Parent $configPath) -Force | Out-Null
    $sentinel = '{"version":1,"bindings":[{"local_root":"C:\\installer-test-workspace","device_identity":"installer-test-device","device_name":"Install test","remote_root":"/home/sentinel"}],"authorized_workers":[],"owned_workers":[]}' + [Environment]::NewLine
    [IO.File]::WriteAllBytes($configPath, [Text.Encoding]::ASCII.GetBytes($sentinel))
    $configBytes = [Convert]::ToBase64String([IO.File]::ReadAllBytes($configPath))

    [Environment]::SetEnvironmentVariable('LOCALAPPDATA', $localAppData, 'Process')
    [Environment]::SetEnvironmentVariable('UU_CLI_CONFIG', $configPath, 'Process')

    & $installer -PackageRoot $package
    $uuExe = Join-Path $installRoot 'uu.exe'
    $backgroundExe = Join-Path $installRoot 'uu-background.exe'
    if (-not (Test-Path -LiteralPath $uuExe -PathType Leaf) -or
        -not (Test-Path -LiteralPath $backgroundExe -PathType Leaf)) {
        throw 'Install did not place both native executables.'
    }
    if (-not (Test-Path -LiteralPath (Join-Path $installRoot 'skills\uu-cli\SKILL.md') -PathType Leaf)) {
        throw 'Install did not place the uu-cli skill.'
    }
    $resolved = (Get-Command uu -CommandType Application -ErrorAction Stop).Source
    if ([IO.Path]::GetFullPath($resolved) -ine [IO.Path]::GetFullPath($uuExe)) {
        throw "PATH resolves uu to '$resolved', expected '$uuExe'."
    }
    $usage = (& $uuExe 2>&1 | Out-String)
    if ($usage -notmatch 'usage:\s*uu') {
        throw 'The installed native executable did not return its command usage.'
    }
    if (-not (Test-Path -LiteralPath $configPath -PathType Leaf) -or
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($configPath)) -cne $configBytes) {
        throw 'Install changed the existing workspace binding config.'
    }
    if ((Count-UserPathEntry $installRoot) -ne 1) {
        throw 'Install did not add exactly one user PATH entry.'
    }
    $run = Read-RegistryValue -SubKey 'Software\Microsoft\Windows\CurrentVersion\Run' -Name 'uu-cli'
    if (-not $run.Exists -or $run.Value -cne ('"{0}"' -f $backgroundExe)) {
        throw 'Install did not register the hidden watcher at the expected per-user path.'
    }

    $lockedExecutable = [IO.File]::Open(
        $uuExe,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::Read
    )
    $busyReported = $false
    try {
        & $installer -PackageRoot $package
    }
    catch {
        $busyReported = $_.ToString() -match '(?i)busy'
    }
    finally {
        $lockedExecutable.Dispose()
    }
    if (-not $busyReported) {
        throw 'Upgrade did not report a clear busy error for a locked uu.exe.'
    }

    & $installer -PackageRoot $package
    if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($configPath)) -cne $configBytes) {
        throw 'Upgrade changed the existing workspace binding config.'
    }
    if ((Count-UserPathEntry $installRoot) -ne 1) {
        throw 'Upgrade duplicated the user PATH entry.'
    }
    $resolved = (Get-Command uu -CommandType Application -ErrorAction Stop).Source
    if ([IO.Path]::GetFullPath($resolved) -ine [IO.Path]::GetFullPath($uuExe)) {
        throw 'Upgrade changed the command away from the native uu.exe path.'
    }

    & $uninstaller
    if (Test-Path -LiteralPath $installRoot) {
        throw 'Uninstall left the uu-cli program directory behind.'
    }
    if (Test-Path -LiteralPath $configPath) {
        throw 'Uninstall left uu-cli''s configured binding file behind.'
    }
    foreach ($suffix in @('.binding.json.lock', '.binding.json.bak')) {
        if (Test-Path -LiteralPath (Join-Path (Split-Path -Parent $configPath) $suffix)) {
            throw "Uninstall left a uu-cli config sidecar behind: $suffix"
        }
    }
    if ((Count-UserPathEntry $installRoot) -ne 0) {
        throw 'Uninstall left its user PATH entry behind.'
    }
    $run = Read-RegistryValue -SubKey 'Software\Microsoft\Windows\CurrentVersion\Run' -Name 'uu-cli'
    if ($run.Exists) {
        throw 'Uninstall left its per-user watcher Run entry behind.'
    }
}
catch {
    $testFailure = $_
}
finally {
    try {
        if (Test-Path -LiteralPath $installRoot) {
            & $uninstaller
        }
    }
    catch {
        Write-Warning "Best-effort uu-cli cleanup failed: $($_.Exception.Message)"
        if ($null -eq $testFailure) {
            $testFailure = $_
        }
    }

    [Environment]::SetEnvironmentVariable('LOCALAPPDATA', $oldLocalAppData, 'Process')
    [Environment]::SetEnvironmentVariable('UU_CLI_CONFIG', $oldConfigOverride, 'Process')
    $env:PATH = $oldProcessPath
    Restore-RegistryValue -SubKey 'Environment' -Name 'Path' -Snapshot $oldUserPath
    Restore-RegistryValue -SubKey 'Software\Microsoft\Windows\CurrentVersion\Run' -Name 'uu-cli' -Snapshot $oldRunValue

    if (Test-Path -LiteralPath $testRoot) {
        try {
            Remove-Item -LiteralPath $testRoot -Recurse -Force
        }
        catch {
            Write-Warning "Could not remove installer test data at '$testRoot'."
            if ($null -eq $testFailure) {
                $testFailure = $_
            }
        }
    }
}

if ($null -ne $testFailure) {
    throw $testFailure
}
Write-Output 'Clean hosted Windows install, upgrade, PATH/background integration, and uninstall checks passed.'
