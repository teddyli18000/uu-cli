#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$PackageRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'common.ps1')

if ([string]::IsNullOrWhiteSpace($PackageRoot)) {
    $PackageRoot = Split-Path -Parent $PSScriptRoot
}

$package = [IO.Path]::GetFullPath($PackageRoot)
$payloads = @(
    (Join-Path $package 'uu.exe'),
    (Join-Path $package 'uu-background.exe'),
    (Join-Path $package 'skills\uu-cli\SKILL.md')
)
foreach ($payload in $payloads) {
    if (-not (Test-Path -LiteralPath $payload -PathType Leaf)) {
        throw "The uu-cli package is incomplete; missing '$payload'."
    }
}

$installRoot = Get-UuCliInstallRoot
if (Test-Path -LiteralPath $installRoot) {
    $item = Get-Item -LiteralPath $installRoot -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Refusing to install through a reparse point at '$installRoot'."
    }
}
else {
    New-Item -ItemType Directory -Path $installRoot -Force | Out-Null
}

# Only the watcher processes belonging to this exact install are stopped. Foreground
# uu.exe jobs remain untouched; a locked executable makes the atomic update fail.
Remove-UuCliRunEntry
Stop-UuCliWatchers -InstallRoot $installRoot

$skillDestination = Join-Path $installRoot 'skills\uu-cli\SKILL.md'
Invoke-UuCliAtomicCopy -Source $payloads[2] -Destination $skillDestination
Invoke-UuCliAtomicCopy -Source $payloads[0] -Destination (Join-Path $installRoot 'uu.exe')
Invoke-UuCliAtomicCopy -Source $payloads[1] -Destination (Join-Path $installRoot 'uu-background.exe')

# Binding/config data lives outside Programs\uu-cli and is deliberately preserved.
Set-UuCliUserPath -InstallRoot $installRoot
Set-UuCliProcessPath -InstallRoot $installRoot

Write-Output "Installed uu-cli for the current Windows user at '$installRoot'."
