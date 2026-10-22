<#
.SYNOPSIS
  Install the RawKVM edge helper for the current user (no administrator
  rights, no driver). Run from the unpacked rawkvm-edge-windows folder in a
  normal PowerShell:

    .\install-edge.ps1 -Hub 192.168.1.28 -Token <64 hex> -ForNode <gadget node_id>

  -ForNode is the node_id of the USB gadget agent that feeds this PC
  (`sudo rawkvm-hub --list-nodes` on the hub). Re-running it updates the
  binary and keeps the configuration. See docs\windows-borde.md.
#>
param(
    [string] $Hub,
    [string] $Token,
    [string] $ForNode
)
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$src = Join-Path $here 'rawkvm-edge.exe'
$dir = Join-Path $env:LOCALAPPDATA 'rawkvm'
$exe = Join-Path $dir 'rawkvm-edge.exe'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$runName = 'RawKvmEdge'

function Step($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }

if (-not (Test-Path $src)) { throw "rawkvm-edge.exe not found next to this script" }

Step 'Binary'
Get-Process rawkvm-edge -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 300
New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item $src $exe -Force
# Downloaded files carry the "from the Internet" mark; SmartScreen would
# block an unsigned program started at logon.
Unblock-File $exe

Step 'Configuration'
$cfgArgs = @('--configure-only')
if ($Hub) { $cfgArgs += @('--hub', $Hub) }
if ($Token) { $cfgArgs += @('--token', $Token) }
if ($ForNode) { $cfgArgs += @('--for-node', $ForNode) }
# Piping makes PowerShell wait for this GUI-subsystem program.
& $exe @cfgArgs | Out-Host
if ($LASTEXITCODE) { throw "configuration failed ($LASTEXITCODE)" }
$cfg = Join-Path $env:APPDATA 'rawkvm\edge.toml'
$text = if (Test-Path $cfg) { Get-Content -Raw $cfg } else { '' }
foreach ($key in 'hub', 'token', 'for_node') {
    if ($text -notmatch "(?m)^$key = `"[^`"]+`"") {
        Write-Warning "$cfg has no $key yet: re-run with -Hub, -Token and -ForNode"
    }
}

Step 'Start at logon'
Set-ItemProperty -Path $runKey -Name $runName -Value "`"$exe`""
Start-Process $exe

Write-Host "Installed. Log: $dir\edge.log" -ForegroundColor Green
Write-Host "Diagnostics: & '$exe' --show | Out-Host"
