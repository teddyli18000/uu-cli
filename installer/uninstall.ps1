#Requires -Version 5.1
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'common.ps1')

$installRoot = Get-UuCliInstallRoot
Stop-UuCliWatchers -InstallRoot $installRoot
Remove-UuCliRunEntry

if (Test-Path -LiteralPath $installRoot) {
    $item = Get-Item -LiteralPath $installRoot -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Refusing to remove a reparse point at '$installRoot'."
    }
    try {
        Remove-Item -LiteralPath $installRoot -Recurse -Force
    }
    catch {
        throw "Could not remove '$installRoot'. A uu-cli job may still be using its executable; let it finish and retry. The user PATH and binding config are still present."
    }
    if (Test-Path -LiteralPath $installRoot) {
        throw "Could not fully remove '$installRoot'. The user PATH and binding config are still present."
    }
}

Set-UuCliUserPath -InstallRoot $installRoot -Remove
Set-UuCliProcessPath -InstallRoot $installRoot -Remove
Remove-UuCliConfig

Write-Output 'Removed uu-cli, its user PATH/Run entries, and its own binding config. Official UU was not modified.'
