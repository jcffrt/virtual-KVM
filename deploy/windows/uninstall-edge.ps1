<#
.SYNOPSIS
  Remove the RawKVM edge helper of the current user. Keeps
  %APPDATA%\rawkvm\edge.toml.
#>
$ErrorActionPreference = 'Stop'
Remove-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'RawKvmEdge' -ErrorAction SilentlyContinue
Get-Process rawkvm-edge -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 300
Remove-Item (Join-Path $env:LOCALAPPDATA 'rawkvm\rawkvm-edge.exe') -ErrorAction SilentlyContinue
Write-Host 'Removed.' -ForegroundColor Green
