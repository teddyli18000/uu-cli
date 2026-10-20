$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$installer = Join-Path $env:RUNNER_TEMP 'uuyc-installer.exe'
if (-not (Test-Path $installer)) {
    throw "Verified installer is missing: $installer"
}

$installerSig = Get-AuthenticodeSignature -FilePath $installer
if ($installerSig.Status -ne 'Valid' -or -not $installerSig.SignerCertificate) {
    throw 'Refusing to execute an installer without a valid Authenticode signature.'
}
if ($installerSig.SignerCertificate.Subject -notmatch 'NetEase') {
    throw "Unexpected installer signer: $($installerSig.SignerCertificate.Subject)"
}

function Invoke-BoundedProcess {
    param(
        [Parameter(Mandatory=$true)][string]$FilePath,
        [string[]]$ArgumentList = @(),
        [int]$TimeoutMs = 180000,
        [string]$WorkingDirectory = ''
    )

    $start = @{
        FilePath = $FilePath
        ArgumentList = $ArgumentList
        PassThru = $true
        WindowStyle = 'Hidden'
    }
    if ($WorkingDirectory) { $start.WorkingDirectory = $WorkingDirectory }

    $process = Start-Process @start
    if (-not $process.WaitForExit($TimeoutMs)) {
        try { $process.Kill($true) } catch {}
        throw "Timed out: $FilePath $($ArgumentList -join ' ')"
    }
    return $process.ExitCode
}

function Get-UuUninstallEntries {
    $paths = @(
        'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*'
    )
    foreach ($path in $paths) {
        Get-ItemProperty $path -ErrorAction SilentlyContinue |
            Where-Object {
                $_.DisplayName -and (
                    $_.DisplayName -match 'UU.*远程' -or
                    $_.DisplayName -match 'UU Remote' -or
                    $_.Publisher -match 'NetEase'
                )
            }
    }
}

function Show-UuProcesses {
    Get-Process -ErrorAction SilentlyContinue |
        Where-Object {
            $_.ProcessName -match '(?i)GameViewer|uuyc|UURemote|cleanup_utils|^Un$|^Uninstall$'
        } |
        Sort-Object ProcessName, Id |
        ForEach-Object { Write-Host "  process: $($_.ProcessName) pid=$($_.Id)" }
}

Write-Host 'Installing verified official UU package silently on disposable runner...'
$installExit = Invoke-BoundedProcess -FilePath $installer -ArgumentList @('/S') -TimeoutMs 240000
Write-Host "Installer exit code: $installExit"
if ($installExit -ne 0) {
    throw "Official installer returned non-zero exit code $installExit"
}

Start-Sleep -Seconds 3
$entries = @(Get-UuUninstallEntries)
Write-Host "Matching uninstall entries: $($entries.Count)"
foreach ($entry in $entries) {
    Write-Host "  DisplayName: $($entry.DisplayName)"
    Write-Host "  DisplayVersion: $($entry.DisplayVersion)"
    Write-Host "  Publisher: $($entry.Publisher)"
    Write-Host "  InstallLocation: $($entry.InstallLocation)"
    Write-Host "  UninstallString: $($entry.UninstallString)"
}

$roots = New-Object System.Collections.Generic.HashSet[string] ([StringComparer]::OrdinalIgnoreCase)
foreach ($entry in $entries) {
    if ($entry.InstallLocation -and (Test-Path $entry.InstallLocation)) {
        [void]$roots.Add([IO.Path]::GetFullPath($entry.InstallLocation))
    }
    if ($entry.UninstallString) {
        $raw = [string]$entry.UninstallString
        $exe = $null
        if ($raw -match '^\s*"([^"]+\.exe)"') { $exe = $Matches[1] }
        elseif ($raw -match '^\s*([^\s]+\.exe)') { $exe = $Matches[1] }
        if ($exe -and (Test-Path $exe)) {
            [void]$roots.Add((Split-Path -Parent $exe))
        }
    }
}

Write-Host "Candidate install roots: $($roots.Count)"
$roots | ForEach-Object { Write-Host "  $_" }

$cliCandidates = @()
foreach ($root in $roots) {
    $cliCandidates += Get-ChildItem -Path $root -Filter 'uuyc-cli.exe' -File -Recurse -ErrorAction SilentlyContinue
}
$cliCandidates = @($cliCandidates | Sort-Object FullName -Unique)

if ($cliCandidates.Count -eq 0) {
    Write-Host 'Registry roots did not expose uuyc-cli.exe; using bounded common-root discovery on fresh runner.'
    $commonRoots = @($env:LOCALAPPDATA, $env:ProgramFiles, ${env:ProgramFiles(x86)}) |
        Where-Object { $_ -and (Test-Path $_) } |
        Sort-Object -Unique
    foreach ($root in $commonRoots) {
        $cliCandidates += Get-ChildItem -Path $root -Filter 'uuyc-cli.exe' -File -Recurse -ErrorAction SilentlyContinue
    }
    $cliCandidates = @($cliCandidates | Sort-Object FullName -Unique)
}

Write-Host "Installed uuyc-cli candidates: $($cliCandidates.Count)"
foreach ($cli in $cliCandidates) {
    Write-Host "  $($cli.FullName)"
    $sig = Get-AuthenticodeSignature -FilePath $cli.FullName
    Write-Host "    version: $($cli.VersionInfo.FileVersion)"
    Write-Host "    signature: $($sig.Status)"
    if ($sig.SignerCertificate) { Write-Host "    signer: $($sig.SignerCertificate.Subject)" }
}

if ($cliCandidates.Count -ne 1) {
    throw "Expected exactly one installed uuyc-cli.exe, found $($cliCandidates.Count)"
}

$cli = $cliCandidates[0]
$cliSig = Get-AuthenticodeSignature -FilePath $cli.FullName
if ($cliSig.Status -ne 'Valid' -or -not $cliSig.SignerCertificate -or $cliSig.SignerCertificate.Subject -notmatch 'NetEase') {
    throw 'Installed uuyc-cli.exe did not pass signature validation.'
}

# Read-only commands only. The runner has no account credentials.
foreach ($args in @(@('version'), @('device', 'list', '--json'))) {
    Write-Host "CLI probe: uuyc-cli $($args -join ' ')"
    $stdout = Join-Path $env:RUNNER_TEMP 'uuyc-probe-out.txt'
    $stderr = Join-Path $env:RUNNER_TEMP 'uuyc-probe-err.txt'
    Remove-Item $stdout, $stderr -ErrorAction SilentlyContinue
    $p = Start-Process -FilePath $cli.FullName -ArgumentList $args -WorkingDirectory $cli.DirectoryName -NoNewWindow -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    if (-not $p.WaitForExit(15000)) {
        $p.Kill($true)
        throw "CLI probe timed out: $($args -join ' ')"
    }
    Write-Host "  exit: $($p.ExitCode)"
    if (Test-Path $stdout) { Get-Content $stdout | Select-Object -First 80 | ForEach-Object { Write-Host "  stdout: $_" } }
    if (Test-Path $stderr) { Get-Content $stderr | Select-Object -First 80 | ForEach-Object { Write-Host "  stderr: $_" } }
}

# If the workflow supplied the uu-cli release candidate, verify our own discovery
# against this clean official installation. Do not use UUYC_CLI here: the point is
# to prove normal registry/installation discovery works without a developer override.
# The discovered path stays an implementation detail; version plus the vendor's
# stopped-app diagnostic proves that uu actually found and invoked the installed CLI.
if ($env:UU_CLI_UNDER_TEST) {
    if (-not (Test-Path $env:UU_CLI_UNDER_TEST)) {
        throw "uu-cli binary under test is missing: $env:UU_CLI_UNDER_TEST"
    }
    $uuConfig = Join-Path $env:RUNNER_TEMP 'uu-cli-smoke-config.json'
    Remove-Item $uuConfig -ErrorAction SilentlyContinue
    $oldConfig = $env:UU_CLI_CONFIG
    $oldOverride = $env:UUYC_CLI
    try {
        $env:UU_CLI_CONFIG = $uuConfig
        Remove-Item Env:UUYC_CLI -ErrorAction SilentlyContinue
        Write-Host "uu-cli discovery probe: $env:UU_CLI_UNDER_TEST list"
        $uuOutput = & $env:UU_CLI_UNDER_TEST list 2>&1
        $uuExit = $LASTEXITCODE
        $uuText = ($uuOutput | Out-String)
        $uuOutput | ForEach-Object { Write-Host "  uu: $_" }
        if ($uuExit -ne 0) {
            throw "uu list failed with exit code $uuExit"
        }
        if ($uuText -notmatch [regex]::Escape($cli.VersionInfo.FileVersion)) {
            throw "uu list did not report the installed official CLI version: $($cli.VersionInfo.FileVersion)"
        }
        if ($uuText -notmatch 'Main application is not running') {
            throw 'uu list did not preserve the observed official stopped-app diagnostic.'
        }
        Write-Host 'uu-cli clean-install discovery probe passed.'
    } finally {
        if ($null -eq $oldConfig) { Remove-Item Env:UU_CLI_CONFIG -ErrorAction SilentlyContinue } else { $env:UU_CLI_CONFIG = $oldConfig }
        if ($null -eq $oldOverride) { Remove-Item Env:UUYC_CLI -ErrorAction SilentlyContinue } else { $env:UUYC_CLI = $oldOverride }
    }
}

Write-Host 'UU-related processes before uninstall:'
Show-UuProcesses

$uninstallExe = $null
foreach ($entry in $entries) {
    $raw = [string]$entry.UninstallString
    if ($raw -match '^\s*"([^"]+\.exe)"') { $uninstallExe = $Matches[1]; break }
    if ($raw -match '^\s*([^\s]+\.exe)') { $uninstallExe = $Matches[1]; break }
}
if (-not $uninstallExe -or -not (Test-Path $uninstallExe)) {
    $uninstallExe = Get-ChildItem -Path $cli.DirectoryName -Filter 'Uninstall.exe' -File -Recurse -ErrorAction SilentlyContinue |
        Select-Object -First 1 -ExpandProperty FullName
}
if (-not $uninstallExe -or -not (Test-Path $uninstallExe)) {
    throw 'Could not locate official UU uninstaller after clean install.'
}

$cliPaths = @($cliCandidates | ForEach-Object { $_.FullName })
Write-Host "Uninstalling via: $uninstallExe /S"
$uninstallExit = Invoke-BoundedProcess -FilePath $uninstallExe -ArgumentList @('/S') -TimeoutMs 240000 -WorkingDirectory (Split-Path -Parent $uninstallExe)
Write-Host "Uninstaller launcher exit code: $uninstallExit"
if ($uninstallExit -ne 0) {
    throw "Official uninstaller returned non-zero exit code $uninstallExit"
}

# NSIS launches helper cleanup processes and may return before they finish. Observe the
# official asynchronous cleanup rather than killing anything just to make CI green.
$deadline = [DateTime]::UtcNow.AddSeconds(90)
$lastReport = [DateTime]::MinValue
$clean = $false
while ([DateTime]::UtcNow -lt $deadline) {
    $remainingPaths = @($cliPaths | Where-Object { Test-Path $_ })
    $remainingEntries = @(Get-UuUninstallEntries)
    if ($remainingPaths.Count -eq 0 -and $remainingEntries.Count -eq 0) {
        $clean = $true
        break
    }

    if (([DateTime]::UtcNow - $lastReport).TotalSeconds -ge 5) {
        Write-Host "Waiting for official cleanup: CLI files=$($remainingPaths.Count), uninstall entries=$($remainingEntries.Count)"
        Show-UuProcesses
        $lastReport = [DateTime]::UtcNow
    }
    Start-Sleep -Milliseconds 750
}

$remainingPaths = @($cliPaths | Where-Object { Test-Path $_ })
$remainingEntries = @(Get-UuUninstallEntries)
Write-Host "Remaining CLI files: $($remainingPaths.Count)"
$remainingPaths | ForEach-Object { Write-Host "  $_" }
Write-Host "Remaining uninstall entries: $($remainingEntries.Count)"
if (-not $clean) {
    Write-Host 'UU-related processes after cleanup timeout:'
    Show-UuProcesses
    throw 'Official UU silent uninstall did not complete within the bounded cleanup window.'
}

Write-Host 'Official UU asynchronous uninstall cleanup completed.'
