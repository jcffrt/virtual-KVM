<#
.SYNOPSIS
  Install the RawKVM VHF driver and agent service. Run from the unpacked
  rawkvm-windows folder in an elevated PowerShell:

    .\install.ps1 -EnableTestSigning        # first time only, then reboot
    .\install.ps1 -Hub 192.168.1.28 -Token <64 hex>

  See docs\windows-instalacion.md.
#>
#Requires -RunAsAdministrator
param(
    [string] $Hub,
    [string] $Token,
    [switch] $EnableTestSigning
)
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$driverDir = Join-Path $here 'driver'
$agentSrc = Join-Path $here 'rawkvm-agent.exe'
$installDir = Join-Path $env:ProgramFiles 'RawKVM'
$agent = Join-Path $installDir 'rawkvm-agent.exe'
$dataDir = Join-Path $env:ProgramData 'rawkvm'
$serviceName = 'RawKvmAgent'
$certSubject = 'CN=RawKVM Test Signing'

function Step($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }

# 1. Test signing
Step 'Test signing'
# Options of the running boot; language independent (bcdedit's "Yes" is localized).
$startOptions = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control').SystemStartOptions
$testSigning = $startOptions -match 'TESTSIGNING'
if (-not $testSigning) {
    if (-not $EnableTestSigning) {
        throw "Test signing is off. Run '.\install.ps1 -EnableTestSigning', reboot, and run this script again."
    }
    bcdedit /set testsigning on
    if ($LASTEXITCODE) {
        throw 'bcdedit refused (usually: Secure Boot is on). Disable Secure Boot in the UEFI setup and retry.'
    }
    Write-Host 'Test signing enabled for the next boot. Reboot now, then run this script again (without -EnableTestSigning).' -ForegroundColor Yellow
    exit 0
}

# 2. Certificate of this build
Step 'Certificate'
$cerPath = Join-Path $driverDir 'RawKvmTest.cer'
$cer = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($cerPath)
foreach ($store in 'Root', 'TrustedPublisher') {
    # Each CI build signs with a fresh key: drop certificates of older builds.
    Get-ChildItem "Cert:\LocalMachine\$store" |
        Where-Object { $_.Subject -eq $certSubject -and $_.Thumbprint -ne $cer.Thumbprint } |
        Remove-Item
    Import-Certificate -FilePath $cerPath -CertStoreLocation "Cert:\LocalMachine\$store" | Out-Null
}
Write-Host "trusted $($cer.Thumbprint)"

# 3. Agent binary (the service must be stopped to replace it)
Step 'Agent'
$svc = Get-Service $serviceName -ErrorAction SilentlyContinue
if ($svc -and $svc.Status -ne 'Stopped') { Stop-Service $serviceName -Force }
New-Item -ItemType Directory -Force -Path $installDir | Out-Null
Copy-Item $agentSrc $agent -Force

# 4. Driver: create Root\RawKvmVhf if needed and install the INF on it
Step 'Driver'
& $agent driver-install --inf (Join-Path $driverDir 'RawKvmVhf.inf')
if ($LASTEXITCODE) { throw 'driver installation failed' }
$dev = Get-PnpDevice -FriendlyName 'RawKVM virtual HID bus' -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $dev) { throw 'RawKVM virtual HID bus not found in Device Manager' }
if ($dev.Status -ne 'OK') {
    Write-Warning "RawKVM virtual HID bus status: $($dev.Status) (problem $($dev.Problem)). See docs\windows-instalacion.md."
}

# 5. Configuration (hub and token), readable only by SYSTEM and Administrators
Step 'Configuration'
New-Item -ItemType Directory -Force -Path $dataDir | Out-Null
icacls $dataDir /inheritance:r /grant:r 'SYSTEM:(OI)(CI)F' 'Administrators:(OI)(CI)F' | Out-Null
$cfgArgs = @('configure')
if ($Hub) { $cfgArgs += @('--hub', $Hub) }
if ($Token) { $cfgArgs += @('--token', $Token) }
& $agent @cfgArgs
if ($LASTEXITCODE) { throw 'configure failed' }

# 6. Service (LocalSystem, automatic, restart on failure)
Step 'Service'
if (-not (Get-Service $serviceName -ErrorAction SilentlyContinue)) {
    New-Service -Name $serviceName -BinaryPathName "`"$agent`" service" `
        -DisplayName 'RawKVM agent' -StartupType Automatic `
        -Description 'Receives the shared Logitech keyboard and mouse from the RawKVM hub.' | Out-Null
}
sc.exe failure $serviceName reset= 60 actions= restart/2000/restart/2000/restart/10000 | Out-Null
Start-Service $serviceName
Get-Service $serviceName | Format-Table Name, Status, StartType

Write-Host "Log: $dataDir\agent.log" -ForegroundColor Green

# Flush the files just written to disk.
try { Write-VolumeCache -DriveLetter $env:SystemDrive[0] } catch { }
