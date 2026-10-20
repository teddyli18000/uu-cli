$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$installer = Join-Path $env:RUNNER_TEMP 'uuyc-installer.exe'
if (-not (Test-Path $installer)) {
    throw "Verified installer is missing: $installer"
}

$signature = Get-AuthenticodeSignature -FilePath $installer
if ($signature.Status -ne 'Valid' -or -not $signature.SignerCertificate) {
    throw 'Refusing to inspect an installer without a valid Authenticode signature.'
}

Write-Host "Inspecting signed installer: $installer"
Write-Host "Signer: $($signature.SignerCertificate.Subject)"

$sevenZip = Get-Command 7z -ErrorAction SilentlyContinue
if (-not $sevenZip) {
    Write-Host '7z is unavailable on this runner; package extraction inspection skipped.'
    exit 0
}

Write-Host 'Archive/package listing (first 120 lines):'
$listOutput = & $sevenZip.Source l $installer 2>&1
$listOutput | Select-Object -First 120 | ForEach-Object { Write-Host $_ }

$extractDir = Join-Path $env:RUNNER_TEMP 'uuyc-extracted'
Remove-Item -Recurse -Force $extractDir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $extractDir | Out-Null

& $sevenZip.Source x '-y' "-o$extractDir" $installer | Out-Host
if ($LASTEXITCODE -ne 0) {
    Write-Host "7z could not extract this installer (exit $LASTEXITCODE); continuing without executing it."
    exit 0
}

$files = Get-ChildItem -Path $extractDir -Recurse -File
Write-Host "Extracted files: $($files.Count)"
$files |
    Where-Object { $_.Name -match '(?i)uuyc|gameviewer|terminal|unins|setup' } |
    Select-Object -First 100 |
    ForEach-Object {
        $relative = [IO.Path]::GetRelativePath($extractDir, $_.FullName)
        Write-Host "  $relative [$($_.Length) bytes]"
    }

function Invoke-ReadOnlyCliProbe {
    param(
        [Parameter(Mandatory=$true)][string]$CliPath,
        [Parameter(Mandatory=$true)][string[]]$Arguments,
        [int]$TimeoutMs = 10000
    )

    $tag = (($Arguments -join '-') -replace '[^A-Za-z0-9_-]', '_')
    if (-not $tag) { $tag = 'root' }
    $stdout = Join-Path $env:RUNNER_TEMP "uuyc-cli-$tag.out.txt"
    $stderr = Join-Path $env:RUNNER_TEMP "uuyc-cli-$tag.err.txt"
    Remove-Item $stdout, $stderr -ErrorAction SilentlyContinue

    Write-Host "  probe: uuyc-cli $($Arguments -join ' ')"
    $process = Start-Process -FilePath $CliPath -ArgumentList $Arguments -WorkingDirectory (Split-Path -Parent $CliPath) -NoNewWindow -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    if (-not $process.WaitForExit($TimeoutMs)) {
        $process.Kill($true)
        Write-Host '    timed out and was terminated.'
        return
    }
    Write-Host "    exit: $($process.ExitCode)"
    if (Test-Path $stdout) { Get-Content $stdout | Select-Object -First 120 | ForEach-Object { Write-Host "    stdout: $_" } }
    if (Test-Path $stderr) { Get-Content $stderr | Select-Object -First 120 | ForEach-Object { Write-Host "    stderr: $_" } }
}

$cliCandidates = @($files | Where-Object { $_.Name -ieq 'uuyc-cli.exe' })
Write-Host "uuyc-cli.exe candidates: $($cliCandidates.Count)"

foreach ($cli in $cliCandidates) {
    $relative = [IO.Path]::GetRelativePath($extractDir, $cli.FullName)
    $cliSig = Get-AuthenticodeSignature -FilePath $cli.FullName
    Write-Host "CLI candidate: $relative"
    Write-Host "  version: $($cli.VersionInfo.FileVersion)"
    Write-Host "  product: $($cli.VersionInfo.ProductName)"
    Write-Host "  signature: $($cliSig.Status)"
    if ($cliSig.SignerCertificate) {
        Write-Host "  signer: $($cliSig.SignerCertificate.Subject)"
    }

    if ($cliSig.Status -ne 'Valid') {
        Write-Host '  not executing: embedded CLI signature is not valid.'
        continue
    }

    # All probes below are local/read-only. The hosted runner has no UU account or
    # remote device. We capture current vendor command shapes rather than hard-code
    # assumptions from an older release.
    Invoke-ReadOnlyCliProbe -CliPath $cli.FullName -Arguments @('version')
    Invoke-ReadOnlyCliProbe -CliPath $cli.FullName -Arguments @('--help')
    Invoke-ReadOnlyCliProbe -CliPath $cli.FullName -Arguments @('term', '--help')
    Invoke-ReadOnlyCliProbe -CliPath $cli.FullName -Arguments @('term', 'open', '--help')
}
