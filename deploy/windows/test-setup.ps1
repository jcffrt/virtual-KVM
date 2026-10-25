<#
.SYNOPSIS
  CI test of setup.ps1 and uninstall.ps1 on a clean Windows runner, from a
  folder with the release zip and SHA256SUMS (setup.ps1 -From). No hub:
  the edge helper starts "already paired" with a made-up config, so the
  pairing step is skipped; a forced pairing must fail cleanly.

    .\test-setup.ps1 -Dist dist
#>
[CmdletBinding()]
param([Parameter(Mandatory)] [string] $Dist)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

$dir = Join-Path $env:LOCALAPPDATA 'Programs\RawKVM'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$edgeConfig = Join-Path $env:APPDATA 'rawkvm\edge.toml'
$widgetConfig = Join-Path $env:LOCALAPPDATA 'rawkvm\widget.toml'
$oldEdge = Join-Path $env:LOCALAPPDATA 'rawkvm\rawkvm-edge.exe'
$failures = 0

function Assert-True([bool] $Condition, [string] $What) {
    if ($Condition) { Write-Host "pass  $What" } else { Write-Host "FAIL  $What" -ForegroundColor Red; $script:failures++ }
}

# Run a script in a child Windows PowerShell (what users have), all
# streams captured, stdin closed (non-interactive). Returns the exit code.
function Invoke-Child([string] $Script, [string[]] $Arguments, [string] $Log) {
    $ErrorActionPreference = 'Continue'
    $out = '' | & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $Script @Arguments *>&1 | ForEach-Object { "$_" }
    $code = $LASTEXITCODE
    $out | Set-Content $Log
    $out | ForEach-Object { Write-Host "    | $_" }
    $code
}

$dist = (Resolve-Path $Dist).Path
$zip = Get-ChildItem $dist -Filter 'RawKVM-windows-*.zip' | Select-Object -First 1
$unpacked = Join-Path $env:TEMP 'rawkvm-test-unpacked'
Expand-Archive $zip.FullName $unpacked -Force
$setup = Join-Path $unpacked 'setup.ps1'

Write-Host '== a corrupt zip is refused'
$bad = Join-Path $env:TEMP 'rawkvm-test-bad'
New-Item -ItemType Directory -Force $bad | Out-Null
Copy-Item (Join-Path $dist 'SHA256SUMS') $bad
[IO.File]::WriteAllBytes((Join-Path $bad $zip.Name), [byte[]](1, 2, 3))
$code = Invoke-Child $setup @('-From', $bad, '-NoStart') (Join-Path $env:TEMP 'bad.log')
Assert-True ($code -ne 0) 'exit code is not 0'
Assert-True ((Get-Content -Raw (Join-Path $env:TEMP 'bad.log')) -match 'SHA-256') 'says SHA-256'
Assert-True (-not (Test-Path $dir)) 'nothing installed'

Write-Host '== install over the manual install of earlier versions, already paired'
New-Item -ItemType Directory -Force (Split-Path $edgeConfig), (Split-Path $oldEdge) | Out-Null
Set-Content $oldEdge 'old'
Set-ItemProperty $runKey -Name RawKvmEdge -Value "`"$oldEdge`""
@"
node_id = "$([guid]::NewGuid())"
node_name = "runner (edge)"
hub = "127.0.0.1"
token = "$('ab' * 32)"
hub_fingerprint = "$('cd' * 32)"
for_node = "$([guid]::NewGuid())"
"@ | Set-Content -Encoding ascii $edgeConfig
$code = Invoke-Child $setup @('-From', $dist) (Join-Path $env:TEMP 'install.log')
$log = Get-Content -Raw (Join-Path $env:TEMP 'install.log')
Assert-True ($log -match 'SHA-256 matches') 'zip checked against SHA256SUMS'
Assert-True ($log -match 'already paired') 'pairing skipped'
foreach ($f in 'rawkvm-edge.exe', 'RawKVM.exe', 'setup.ps1', 'uninstall.ps1') {
    Assert-True (Test-Path (Join-Path $dir $f)) "$f installed"
}
Assert-True (-not (Test-Path $oldEdge)) 'old rawkvm-edge.exe removed'
foreach ($name in 'RawKvmEdge', 'RawKVM') {
    $v = (Get-ItemProperty $runKey -Name $name -ErrorAction SilentlyContinue).$name
    Assert-True ($v -like "*$dir*") "Run $name points to $dir ($v)"
}
Assert-True (Test-Path (Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\RawKVM.lnk')) 'Start menu entry'
Assert-True ($null -eq (Get-Item (Join-Path $dir 'RawKVM.exe') -Stream Zone.Identifier -ErrorAction SilentlyContinue)) 'unblocked'
Assert-True ($null -ne (Get-Process rawkvm-edge -ErrorAction SilentlyContinue)) 'edge running'
$edgeVersion = ('' | & (Join-Path $dir 'rawkvm-edge.exe') --version 2>&1 | Out-String).Trim()
Assert-True ($edgeVersion -match '^rawkvm-edge\S* \d+\.\d+\.\d+ \([0-9a-f]{7}\)$') "edge --version: $edgeVersion"
# The widget takes the edge's pairing when it starts (if this runner can
# run it: it needs a desktop and WebView2).
if (Get-Process RawKVM -ErrorAction SilentlyContinue) {
    Start-Sleep -Seconds 2
    $w = if (Test-Path $widgetConfig) { Get-Content -Raw $widgetConfig } else { '' }
    Assert-True ($w -match 'hub = "127.0.0.1"' -and $w -match "hub_fingerprint = `"$('cd' * 32)`"") 'widget took the pairing'
} else {
    Write-Host 'note  the widget did not start on this runner; its pairing is not checked'
}

Write-Host '== -Check'
$code = Invoke-Child (Join-Path $dir 'setup.ps1') @('-Check') (Join-Path $env:TEMP 'check.log')
$log = Get-Content -Raw (Join-Path $env:TEMP 'check.log')
Assert-True ($log -match '(?m)^ok\s+edge\s+.*token set, certificate cdcdcdcdcdcd') 'edge config line'
Assert-True ($log -match '(?m)^ok\s+for node\s+[0-9a-f-]{36}') 'for node line'
Assert-True ($log -match '(?m)^ok\s+autostart') 'autostart line'
Assert-True ($log -match '(?m)^ok\s+firewall\s+hub address 127.0.0.1') 'firewall line'
Assert-True ($log -notmatch 'ab{64}|(ab){32}') 'the token is not printed'

Write-Host '== forced pairing without a hub fails cleanly'
$tokenDir = New-Item -ItemType Directory -Force (Join-Path $env:TEMP "rawkvm-test-token-$([guid]::NewGuid())")
$tokenFile = Join-Path $tokenDir.FullName 'token.txt'
[IO.File]::WriteAllText($tokenFile, 'ef' * 32)
Assert-True (Test-Path -LiteralPath $tokenFile) "token file written ($tokenFile)"
$code = Invoke-Child (Join-Path $dir 'setup.ps1') @('-Pair', '-Hub', '127.0.0.1', '-TokenFile', $tokenFile, '-NoStart') (Join-Path $env:TEMP 'pair.log')
Assert-True ($code -ne 0) "exit code $code"
Assert-True ((Get-Content -Raw (Join-Path $env:TEMP 'pair.log')) -match 'pairing failed') 'says pairing failed'
Assert-True ((Get-Content -Raw $edgeConfig) -match "token = `"$('ef' * 32)`"") 'token from -TokenFile saved'

Write-Host '== uninstall keeps the pairing; -Purge removes it'
$code = Invoke-Child (Join-Path $dir 'uninstall.ps1') @() (Join-Path $env:TEMP 'uninstall.log')
Assert-True ($code -eq 0) 'uninstall exit code 0'
Assert-True (-not (Test-Path $dir)) 'programs removed'
Assert-True ($null -eq (Get-ItemProperty $runKey -Name RawKvmEdge -ErrorAction SilentlyContinue)) 'Run RawKvmEdge removed'
Assert-True ($null -eq (Get-ItemProperty $runKey -Name RawKVM -ErrorAction SilentlyContinue)) 'Run RawKVM removed'
Assert-True ($null -eq (Get-Process rawkvm-edge, RawKVM -ErrorAction SilentlyContinue)) 'nothing running'
Assert-True (Test-Path $edgeConfig) 'edge.toml kept'
$code = Invoke-Child (Join-Path $unpacked 'uninstall.ps1') @('-Purge') (Join-Path $env:TEMP 'purge.log')
Assert-True ($code -eq 0) 'purge exit code 0'
Assert-True (-not (Test-Path $edgeConfig)) 'edge.toml removed'
Assert-True (-not (Test-Path (Join-Path $env:LOCALAPPDATA 'rawkvm'))) '%LOCALAPPDATA%\rawkvm removed'

if ($failures) { throw "$failures check(s) failed" }
Write-Host 'all passed' -ForegroundColor Green
