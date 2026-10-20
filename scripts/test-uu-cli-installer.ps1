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

function Invoke-CapturedProcess {
    param(
        [Parameter(Mandatory = $true)][string]$Executable,
        [string]$Arguments = '',
        [string]$WorkingDirectory
    )
    $startInfo = New-Object System.Diagnostics.ProcessStartInfo
    $startInfo.FileName = $Executable
    $startInfo.Arguments = $Arguments
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    if (-not [string]::IsNullOrWhiteSpace($WorkingDirectory)) {
        $startInfo.WorkingDirectory = $WorkingDirectory
    }

    $childProcess = New-Object System.Diagnostics.Process
    $childProcess.StartInfo = $startInfo
    try {
        if (-not $childProcess.Start()) {
            throw "Could not start '$Executable'."
        }
        $stdoutTask = $childProcess.StandardOutput.ReadToEndAsync()
        $stderrTask = $childProcess.StandardError.ReadToEndAsync()
        if (-not $childProcess.WaitForExit(15000)) {
            $childProcess.Kill()
            [void]$childProcess.WaitForExit(5000)
            throw "The process '$Executable $Arguments' timed out."
        }
        [pscustomobject]@{
            ExitCode = $childProcess.ExitCode
            StdOut = $stdoutTask.Result
            StdErr = $stderrTask.Result
            Output = $stdoutTask.Result + [Environment]::NewLine + $stderrTask.Result
        }
    }
    finally {
        $childProcess.Dispose()
    }
}

function Get-InstalledWatcherProcesses {
    param(
        [Parameter(Mandatory = $true)][string]$UuExecutable,
        [Parameter(Mandatory = $true)][string]$BackgroundExecutable
    )
    $uuPath = [IO.Path]::GetFullPath($UuExecutable)
    $backgroundPath = [IO.Path]::GetFullPath($BackgroundExecutable)
    $owned = @()
    $processes = Get-CimInstance -ClassName Win32_Process -Filter "Name = 'uu.exe' OR Name = 'uu-background.exe'"
    foreach ($process in $processes) {
        if ([string]::IsNullOrWhiteSpace([string]$process.ExecutablePath)) {
            continue
        }
        try {
            $imagePath = [IO.Path]::GetFullPath([string]$process.ExecutablePath)
        }
        catch {
            continue
        }
        $isBackground = $imagePath -ieq $backgroundPath
        $isWorkerWatcher = $imagePath -ieq $uuPath -and
            ([string]$process.CommandLine -match '(?i)(?:^|\s)__watch(?:\s|$)')
        if ($isBackground -or $isWorkerWatcher) {
            $owned += $process
        }
    }
    return $owned
}

function Wait-ForInstalledWatcher {
    param(
        [Parameter(Mandatory = $true)][string]$UuExecutable,
        [Parameter(Mandatory = $true)][string]$BackgroundExecutable
    )
    $watchers = @()
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    $sampleCount = 0
    $countSamples = @{ Zero = 0; One = 0; Multiple = 0 }
    $history = @{}
    $observers = @{}

    try {
        do {
            $sampleCount++
            $sampleTime = [DateTime]::UtcNow
            $watchers = @(Get-InstalledWatcherProcesses -UuExecutable $UuExecutable -BackgroundExecutable $BackgroundExecutable)
            if ($watchers.Count -eq 0) {
                $countSamples.Zero++
            }
            elseif ($watchers.Count -eq 1) {
                $countSamples.One++
            }
            else {
                $countSamples.Multiple++
            }

            $seenThisSample = @{}
            foreach ($process in $watchers) {
                $processId = [int]$process.ProcessId
                $creationDate = [string]$process.CreationDate
                $identity = '{0}|{1}' -f $processId, $creationDate
                $seenThisSample[$identity] = $true

                if (-not $history.ContainsKey($identity)) {
                    $observer = $null
                    try {
                        $observer = [Diagnostics.Process]::GetProcessById($processId)
                    }
                    catch {
                    }
                    $observers[$identity] = $observer
                    $parentName = $null
                    $parentExecutablePath = $null
                    try {
                        $parentId = [int]$process.ParentProcessId
                        if ($parentId -gt 0) {
                            $parent = Get-CimInstance -ClassName Win32_Process -Filter "ProcessId = $parentId" -ErrorAction Stop | Select-Object -First 1
                            if ($null -ne $parent) {
                                $parentName = [string]$parent.Name
                                $parentExecutablePath = [string]$parent.ExecutablePath
                            }
                        }
                    }
                    catch {
                    }
                    $history[$identity] = [pscustomobject]@{
                        ProcessId = $processId
                        CreationDate = $creationDate
                        ParentProcessId = $process.ParentProcessId
                        ParentName = $parentName
                        ParentExecutablePath = $parentExecutablePath
                        Name = [string]$process.Name
                        ExecutablePath = [string]$process.ExecutablePath
                        CommandLine = [string]$process.CommandLine
                        FirstObservedUtc = $sampleTime.ToString('o')
                        LastObservedUtc = $sampleTime.ToString('o')
                        LastSeenSample = $sampleCount
                        Samples = 0
                        FirstHandleCount = $process.HandleCount
                        LastHandleCount = $process.HandleCount
                        FirstKernelModeTime = $process.KernelModeTime
                        LastKernelModeTime = $process.KernelModeTime
                        FirstUserModeTime = $process.UserModeTime
                        LastUserModeTime = $process.UserModeTime
                        Status = $process.Status
                        TerminationDate = $process.TerminationDate
                        ExitObserved = $null
                        ExitCode = $null
                        HasExitedAtDeadline = $null
                        ObserverStartTimeUtc = $null
                    }
                    if ($null -ne $observer) {
                        try {
                            $history[$identity].ObserverStartTimeUtc = $observer.StartTime.ToUniversalTime().ToString('o')
                        }
                        catch {
                        }
                    }
                }

                $record = $history[$identity]
                $record.LastObservedUtc = $sampleTime.ToString('o')
                $record.LastSeenSample = $sampleCount
                $record.Samples++
                $record.LastHandleCount = $process.HandleCount
                $record.LastKernelModeTime = $process.KernelModeTime
                $record.LastUserModeTime = $process.UserModeTime
                $record.Status = $process.Status
                $record.TerminationDate = $process.TerminationDate
            }

            foreach ($identity in @($history.Keys)) {
                $record = $history[$identity]
                if (-not $seenThisSample.ContainsKey($identity) -and $null -eq $record.ExitObserved) {
                    $record.ExitObserved = $sampleTime.ToString('o')
                    $observer = $observers[$identity]
                    if ($null -ne $observer) {
                        try {
                            $observer.Refresh()
                            if ($observer.HasExited) {
                                $record.ExitCode = $observer.ExitCode
                            }
                            else {
                                $record.ExitCode = 'PID may have been reused or process observation unavailable'
                            }
                        }
                        catch {
                            $record.ExitCode = 'unavailable: ' + $_.Exception.Message
                        }
                    }
                    else {
                        $record.ExitCode = 'unavailable: process handle could not be opened'
                    }
                }
            }

            if ($watchers.Count -eq 1 -and
                [IO.Path]::GetFullPath([string]$watchers[0].ExecutablePath) -ieq [IO.Path]::GetFullPath($BackgroundExecutable)) {
                return $watchers[0]
            }
            Start-Sleep -Milliseconds 250
        } while ([DateTime]::UtcNow -lt $deadline)

        foreach ($identity in @($history.Keys)) {
            $observer = $observers[$identity]
            if ($null -ne $observer) {
                try {
                    $observer.Refresh()
                    $history[$identity].HasExitedAtDeadline = [bool]$observer.HasExited
                    if ($observer.HasExited) {
                        $history[$identity].ExitCode = $observer.ExitCode
                    }
                }
                catch {
                    $history[$identity].HasExitedAtDeadline = 'unavailable: ' + $_.Exception.Message
                }
            }
        }

        $details = @(
            foreach ($identity in @($history.Keys | Sort-Object)) {
                $record = $history[$identity]
                'PID={0}; Created={1}; Name={2}; Path={3}; CommandLine={4}; ParentPID={5}; ParentName={6}; ParentPath={7}; Samples={8}/{9}; First={10}; Last={11}; Handles={12}->{13}; Kernel100ns={14}->{15}; User100ns={16}->{17}; Status={18}; TerminationDate={19}; ExitObserved={20}; ExitCode={21}; HasExitedAtDeadline={22}; ObserverStartUtc={23}' -f $record.ProcessId, $record.CreationDate, $record.Name, $record.ExecutablePath, $record.CommandLine, $record.ParentProcessId, $record.ParentName, $record.ParentExecutablePath, $record.Samples, $sampleCount, $record.FirstObservedUtc, $record.LastObservedUtc, $record.FirstHandleCount, $record.LastHandleCount, $record.FirstKernelModeTime, $record.LastKernelModeTime, $record.FirstUserModeTime, $record.LastUserModeTime, $record.Status, $record.TerminationDate, $record.ExitObserved, $record.ExitCode, $record.HasExitedAtDeadline, $record.ObserverStartTimeUtc
            }
        )
        Write-Host ('Watcher samples: {0}; zero={1}; one={2}; multiple={3}.' -f $sampleCount, $countSamples.Zero, $countSamples.One, $countSamples.Multiple)
        foreach ($detail in $details) {
            Write-Host ('Watcher process history: ' + $detail)
        }
        throw "Expected exactly one watcher at '$BackgroundExecutable', found $($watchers.Count)."
    }
    finally {
        foreach ($observer in $observers.Values) {
            if ($null -ne $observer) {
                $observer.Dispose()
            }
        }
    }
}

function Assert-IdleWatcher {
    param(
        [Parameter(Mandatory = $true)]$Watcher,
        [Parameter(Mandatory = $true)][string]$UuExecutable,
        [Parameter(Mandatory = $true)][string]$BackgroundExecutable
    )
    Start-Sleep -Seconds 1
    $process = Get-Process -Id ([int]$Watcher.ProcessId) -ErrorAction Stop
    $before = $process.TotalProcessorTime.TotalMilliseconds
    Start-Sleep -Seconds 2
    $process.Refresh()
    if ($process.HasExited) {
        throw 'The installed background watcher exited during the idle CPU sample.'
    }
    $usedMilliseconds = $process.TotalProcessorTime.TotalMilliseconds - $before
    if ($usedMilliseconds -gt 200) {
        throw "The idle background watcher used $([math]::Round($usedMilliseconds)) ms CPU in 2 seconds (limit 200 ms)."
    }

    $stillRunning = Wait-ForInstalledWatcher -UuExecutable $UuExecutable -BackgroundExecutable $BackgroundExecutable
    if ([int]$stillRunning.ProcessId -ne [int]$Watcher.ProcessId) {
        throw 'The installed background watcher was replaced during the idle CPU sample.'
    }
}

function Assert-NoInstalledWatchers {
    param(
        [Parameter(Mandatory = $true)][string]$UuExecutable,
        [Parameter(Mandatory = $true)][string]$BackgroundExecutable
    )
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        $watchers = @(Get-InstalledWatcherProcesses -UuExecutable $UuExecutable -BackgroundExecutable $BackgroundExecutable)
        if ($watchers.Count -eq 0) {
            return
        }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)

    throw "Uninstall left $($watchers.Count) watcher process(es) from '$BackgroundExecutable' running."
}

$oldLocalAppData = [Environment]::GetEnvironmentVariable('LOCALAPPDATA', 'Process')
$oldConfigOverride = [Environment]::GetEnvironmentVariable('UU_CLI_CONFIG', 'Process')
$oldProcessPath = $env:PATH
$oldUserPath = Read-RegistryValue -SubKey 'Environment' -Name 'Path'
$oldRunValue = Read-RegistryValue -SubKey 'Software\Microsoft\Windows\CurrentVersion\Run' -Name 'uu-cli'
$testRoot = Join-Path $env:RUNNER_TEMP ('uu-cli-installer-' + [guid]::NewGuid().ToString('N'))
$unicode = ([char]0x6D4B).ToString() + ([char]0x8BD5).ToString()
$localAppData = Join-Path $testRoot ('Local Data ' + $unicode)
$configPath = Join-Path $localAppData 'uu-cli\config.json'
$lockPath = Join-Path (Split-Path -Parent $configPath) '.config.json.lock'
$backupPath = Join-Path (Split-Path -Parent $configPath) '.config.json.bak'
$workerStateDirectory = Join-Path (Split-Path -Parent $configPath) 'workers'
$workerStatePath = Join-Path $workerStateDirectory 'fixture-worker.inflight'
$installRoot = Join-Path $localAppData 'Programs\uu-cli'
$package = [IO.Path]::GetFullPath($PackageRoot)
$installer = Join-Path $package 'installer\install.ps1'
$uninstaller = Join-Path $package 'installer\uninstall.ps1'
$configBytes = $null
$testFailure = $null

try {
    New-Item -ItemType Directory -Path (Split-Path -Parent $configPath) -Force | Out-Null
    $sentinel = '{"version":1,"bindings":[{"local_root":"C:\\installer-test-workspace","device_identity":"installer-test-device","device_name":"Install test","remote_root":"/home/sentinel"}],"authorized_workers":["authorized-worker-fingerprint"],"declined_workers":["declined-worker-fingerprint"]}' + [Environment]::NewLine
    [IO.File]::WriteAllBytes($configPath, [Text.Encoding]::ASCII.GetBytes($sentinel))
    $configBytes = [Convert]::ToBase64String([IO.File]::ReadAllBytes($configPath))
    [IO.File]::WriteAllText($lockPath, 'persistent config lock')
    [IO.File]::WriteAllText($backupPath, 'persistent config backup')
    New-Item -ItemType Directory -Path $workerStateDirectory -Force | Out-Null
    [IO.File]::WriteAllText($workerStatePath, 'pid=999999' + [Environment]::NewLine + 'worker=fixture-worker')

    [Environment]::SetEnvironmentVariable('LOCALAPPDATA', $localAppData, 'Process')
    [Environment]::SetEnvironmentVariable('UU_CLI_CONFIG', $null, 'Process')

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
    $usage = Invoke-CapturedProcess -Executable $uuExe -WorkingDirectory $localAppData
    if ($usage.ExitCode -ne 2 -or $usage.Output -notmatch 'usage:\s*uu') {
        throw "The installed executable returned unexpected usage output or exit code $($usage.ExitCode): $($usage.Output)"
    }
    $installedWatcher = Wait-ForInstalledWatcher -UuExecutable $uuExe -BackgroundExecutable $backgroundExe
    Assert-IdleWatcher -Watcher $installedWatcher -UuExecutable $uuExe -BackgroundExecutable $backgroundExe

    $listResult = Invoke-CapturedProcess -Executable $uuExe -Arguments 'list' -WorkingDirectory $localAppData
    if ($listResult.ExitCode -ne 0 -or $listResult.Output -notmatch '(?im)^UU\s+not found\b') {
        throw "uu list did not report a missing official UU cleanly (exit $($listResult.ExitCode)): $($listResult.Output)"
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
    $upgradedWatcher = Wait-ForInstalledWatcher -UuExecutable $uuExe -BackgroundExecutable $backgroundExe
    if ([IO.Path]::GetFullPath([string]$upgradedWatcher.ExecutablePath) -ine [IO.Path]::GetFullPath($backgroundExe)) {
        throw 'Upgrade did not leave one watcher at the exact installed background path.'
    }

    & $uninstaller
    Assert-NoInstalledWatchers -UuExecutable $uuExe -BackgroundExecutable $backgroundExe
    if (Test-Path -LiteralPath $installRoot) {
        throw 'Uninstall left the uu-cli program directory behind.'
    }
    if (-not (Test-Path -LiteralPath $configPath -PathType Leaf) -or
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($configPath)) -cne $configBytes) {
        throw 'Uninstall changed or removed the user binding and authorization config.'
    }
    if (-not (Test-Path -LiteralPath $lockPath -PathType Leaf) -or
        [IO.File]::ReadAllText($lockPath) -cne 'persistent config lock') {
        throw 'Uninstall changed or removed the per-user config lock.'
    }
    if (-not (Test-Path -LiteralPath $backupPath -PathType Leaf) -or
        [IO.File]::ReadAllText($backupPath) -cne 'persistent config backup') {
        throw 'Uninstall changed or removed the per-user config backup.'
    }
    if (-not (Test-Path -LiteralPath $workerStatePath -PathType Leaf) -or
        [IO.File]::ReadAllText($workerStatePath) -cne ('pid=999999' + [Environment]::NewLine + 'worker=fixture-worker')) {
        throw 'Uninstall changed or removed unknown/quarantine worker runtime state.'
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
Write-Output 'Clean hosted Windows install, exact watcher process, idle CPU, missing-UU list, upgrade, Unicode/space PATH, and state-preserving uninstall checks passed.'
