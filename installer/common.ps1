# Shared user-scope installer helpers for Windows PowerShell 5.1.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-UuCliInstallRoot {
    $localAppData = [Environment]::GetEnvironmentVariable('LOCALAPPDATA')
    if ([string]::IsNullOrWhiteSpace($localAppData)) {
        $localAppData = $HOME
    }
    if ([string]::IsNullOrWhiteSpace($localAppData)) {
        throw 'Cannot determine the current user''s local application data directory.'
    }
    return [IO.Path]::GetFullPath((Join-Path $localAppData 'Programs\uu-cli'))
}

function Initialize-UuCliNativeMethods {
    if ('UuCliInstaller.NativeMethods' -as [type]) {
        return
    }
    $definition = @'
using System;
using System.Runtime.InteropServices;
namespace UuCliInstaller {
    public static class NativeMethods {
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true, EntryPoint = "MoveFileExW")]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool MoveFileEx(string existingName, string newName, uint flags);

        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true, EntryPoint = "SendMessageTimeoutW")]
        public static extern IntPtr SendMessageTimeout(
            IntPtr window, uint message, IntPtr wParam, string lParam,
            uint flags, uint timeout, out IntPtr result);
    }
}
'@
    Add-Type -TypeDefinition $definition -Language CSharp
}

function Invoke-UuCliAtomicCopy {
    param(
        [Parameter(Mandatory = $true)][string]$Source,
        [Parameter(Mandatory = $true)][string]$Destination
    )
    $parent = Split-Path -Parent $Destination
    if (-not (Test-Path -LiteralPath $parent -PathType Container)) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }
    $stage = $Destination + '.' + [guid]::NewGuid().ToString('N') + '.new'
    try {
        [IO.File]::Copy($Source, $stage, $false)
        Initialize-UuCliNativeMethods
        $flags = [uint32](0x1 -bor 0x8)
        if (-not [UuCliInstaller.NativeMethods]::MoveFileEx($stage, $Destination, $flags)) {
            $nativeError = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
            if ($nativeError -in 5, 32, 33) {
                throw "Cannot replace '$Destination' because uu-cli is busy. Let active uu-cli jobs finish, then retry."
            }
            throw "Atomic replacement of '$Destination' failed with Windows error $nativeError."
        }
    }
    finally {
        if (Test-Path -LiteralPath $stage -PathType Leaf) {
            Remove-Item -LiteralPath $stage -Force -ErrorAction SilentlyContinue
        }
    }
}

function ConvertTo-UuCliPathKey {
    param([AllowNull()][string]$Value)
    if ([string]::IsNullOrWhiteSpace($Value)) {
        return ''
    }
    $expanded = [Environment]::ExpandEnvironmentVariables($Value.Trim().Trim('"'))
    try {
        $expanded = [IO.Path]::GetFullPath($expanded)
    }
    catch {
        # Preserve an unusual existing PATH entry so it is not accidentally removed.
    }
    return $expanded.TrimEnd([char[]]@('\', '/'))
}

function Send-UuCliEnvironmentChanged {
    Initialize-UuCliNativeMethods
    $result = [IntPtr]::Zero
    [void][UuCliInstaller.NativeMethods]::SendMessageTimeout(
        [IntPtr]0xffff, 0x001A, [IntPtr]::Zero, 'Environment', 0x0002, 5000, [ref]$result
    )
}

function Set-UuCliUserPath {
    param(
        [Parameter(Mandatory = $true)][string]$InstallRoot,
        [switch]$Remove
    )
    $registryPath = 'Environment'
    if ($Remove) {
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($registryPath, $true)
        if ($null -eq $key) {
            return
        }
    }
    else {
        $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($registryPath)
    }

    try {
        $raw = [string]$key.GetValue(
            'Path',
            '',
            [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
        )
        $targetKey = ConvertTo-UuCliPathKey $InstallRoot
        $remaining = New-Object 'System.Collections.Generic.List[string]'
        foreach ($entry in @($raw -split ';')) {
            if ([string]::IsNullOrWhiteSpace($entry)) {
                continue
            }
            if ((ConvertTo-UuCliPathKey $entry) -ine $targetKey) {
                $remaining.Add($entry.Trim())
            }
        }

        if (-not $Remove) {
            $remaining.Insert(0, $InstallRoot)
        }
        $value = $remaining -join ';'
        if ($remaining.Count -eq 0) {
            $key.DeleteValue('Path', $false)
        }
        else {
            $key.SetValue('Path', $value, [Microsoft.Win32.RegistryValueKind]::ExpandString)
        }
    }
    finally {
        $key.Dispose()
    }
    Send-UuCliEnvironmentChanged
}

function Set-UuCliProcessPath {
    param(
        [Parameter(Mandatory = $true)][string]$InstallRoot,
        [switch]$Remove
    )
    $targetKey = ConvertTo-UuCliPathKey $InstallRoot
    $remaining = New-Object 'System.Collections.Generic.List[string]'
    foreach ($entry in @(([string]$env:PATH) -split ';')) {
        if ([string]::IsNullOrWhiteSpace($entry)) {
            continue
        }
        if ((ConvertTo-UuCliPathKey $entry) -ine $targetKey) {
            $remaining.Add($entry.Trim())
        }
    }
    if (-not $Remove) {
        $remaining.Insert(0, $InstallRoot)
    }
    $env:PATH = $remaining -join ';'
}

# Remove the startup entry written by older releases; new installs do not register one.
function Remove-UuCliRunEntry {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey(
        'Software\Microsoft\Windows\CurrentVersion\Run',
        $true
    )
    if ($null -eq $key) {
        return
    }
    try {
        $key.DeleteValue('uu-cli', $false)
    }
    finally {
        $key.Dispose()
    }
}

function Stop-UuCliWatchers {
    param([Parameter(Mandatory = $true)][string]$InstallRoot)
    $uuExe = [IO.Path]::GetFullPath((Join-Path $InstallRoot 'uu.exe'))
    $backgroundExe = [IO.Path]::GetFullPath((Join-Path $InstallRoot 'uu-background.exe'))
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
        $isBackground = $imagePath -ieq $backgroundExe
        # A persistent attachment may own an active remote job. Leave it alive;
        # the normal locked-file error safely defers an upgrade until it exits.
        if ($isBackground -and ([string]$process.CommandLine -match '(?i)(?:^|\s)__worker(?:\s|$)')) {
            continue
        }
        $isWorkerWatcher = $imagePath -ieq $uuExe -and
            ([string]$process.CommandLine -match '(?i)(?:^|\s)__watch(?:\s|$)')
        if (-not ($isBackground -or $isWorkerWatcher)) {
            continue
        }

        $processId = [int]$process.ProcessId
        Stop-Process -Id $processId -Force -ErrorAction SilentlyContinue
        $running = Get-Process -Id $processId -ErrorAction SilentlyContinue
        if ($null -ne $running -and -not $running.WaitForExit(10000)) {
            throw "Could not stop uu-cli watcher process $processId."
        }
    }
}
