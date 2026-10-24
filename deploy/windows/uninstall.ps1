<#
.SYNOPSIS
  Remove RawKVM (edge helper and widget) of the current user, as setup.ps1
  installed it. Keeps the pairing and the logs unless -Purge is given.

    powershell -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\Programs\RawKVM\uninstall.ps1"

.PARAMETER Purge
  Also remove the configuration (pairing) and the logs. The hub still
  lists this PC's nodes until 'sudo rawkvm-hub --forget-node' there.
#>
[CmdletBinding()]
param([switch] $Purge)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

$InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\RawKVM'
$RunKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$ApprovedKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run'
$Shortcut = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\RawKVM.lnk'

Get-Process rawkvm-edge, RawKVM -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500
foreach ($name in 'RawKvmEdge', 'RawKVM') {
    Remove-ItemProperty -Path $RunKey -Name $name -ErrorAction SilentlyContinue
    Remove-ItemProperty -Path $ApprovedKey -Name $name -ErrorAction SilentlyContinue
}
Remove-Item $Shortcut -Force -ErrorAction SilentlyContinue
# This script may live in the folder it removes.
Set-Location $env:USERPROFILE
if (Test-Path $InstallDir) { Remove-Item $InstallDir -Recurse -Force }
# The manual install of earlier versions.
Remove-Item (Join-Path $env:LOCALAPPDATA 'rawkvm\rawkvm-edge.exe') -Force -ErrorAction SilentlyContinue
Write-Host "Removed the programs, the start at logon and the Start menu entry."

if ($Purge) {
    $edgeConfig = Join-Path $env:APPDATA 'rawkvm\edge.toml'
    Remove-Item $edgeConfig -Force -ErrorAction SilentlyContinue
    $roaming = Join-Path $env:APPDATA 'rawkvm'
    if ((Test-Path $roaming) -and -not (Get-ChildItem $roaming)) { Remove-Item $roaming }
    Remove-Item (Join-Path $env:LOCALAPPDATA 'rawkvm') -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host 'Removed the pairing and the logs. To pair again: setup.ps1 (with the token).'
} else {
    Write-Host "Kept the pairing ($env:APPDATA\rawkvm\edge.toml, $env:LOCALAPPDATA\rawkvm\widget.toml) and the logs: -Purge removes them."
}
