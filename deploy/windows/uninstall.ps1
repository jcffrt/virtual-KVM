<#
.SYNOPSIS
  Remove the RawKVM agent service, driver, device and test certificate.
  Keeps %ProgramData%\rawkvm (config and log) unless -Purge is given.
  Test signing stays on: turn it off with 'bcdedit /set testsigning off'.
#>
#Requires -RunAsAdministrator
param([switch] $Purge)
$ErrorActionPreference = 'Continue'

$svc = Get-Service RawKvmAgent -ErrorAction SilentlyContinue
if ($svc) {
    Stop-Service RawKvmAgent -Force
    sc.exe delete RawKvmAgent | Out-Null
}

Get-PnpDevice -FriendlyName 'RawKVM virtual HID bus' -ErrorAction SilentlyContinue |
    ForEach-Object { pnputil /remove-device $_.InstanceId }

Get-WindowsDriver -Online |
    Where-Object { $_.OriginalFileName -like '*\rawkvmvhf.inf' } |
    ForEach-Object { pnputil /delete-driver $_.Driver /uninstall /force }

foreach ($store in 'Root', 'TrustedPublisher') {
    Get-ChildItem "Cert:\LocalMachine\$store" |
        Where-Object { $_.Subject -eq 'CN=RawKVM Test Signing' } | Remove-Item
}

Remove-Item (Join-Path $env:ProgramFiles 'RawKVM') -Recurse -Force -ErrorAction SilentlyContinue
if ($Purge) { Remove-Item (Join-Path $env:ProgramData 'rawkvm') -Recurse -Force -ErrorAction SilentlyContinue }
Write-Host 'RawKVM removed.'
