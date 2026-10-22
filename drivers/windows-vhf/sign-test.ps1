<#
.SYNOPSIS
  Test-sign the RawKVM VHF driver package (personal use, see docs/adr/0006).

.DESCRIPTION
  Copies RawKvmVhf.sys and the stamped RawKvmVhf.inf from -BuildDir to
  -OutDir, signs the .sys, creates the catalog with inf2cat and signs it.
  The certificate comes from -PfxPath (CI keeps one in a secret so every
  build is signed by the same certificate) or, without it, a new
  self-signed code-signing certificate is created in CurrentUser\My.
  The public certificate is exported as RawKvmTest.cer next to the package;
  install.ps1 imports it into the target machine's Trusted Root and
  Trusted Publishers stores.

  Needs the WDK (inf2cat, signtool). Runs on the build machine, not on the
  PCs that install the driver.
#>
param(
    [Parameter(Mandatory)] [string] $BuildDir,
    [Parameter(Mandatory)] [string] $OutDir,
    [string] $PfxPath,
    [string] $PfxPassword = $env:RAWKVM_PFX_PASSWORD
)
$ErrorActionPreference = 'Stop'

function Find-KitTool([string] $name, [string] $arch) {
    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $hit = Get-ChildItem -Path $kits -Filter $name -Recurse -ErrorAction SilentlyContinue |
        Where-Object { $_.Directory.Name -eq $arch } |
        Sort-Object { $_.Directory.Parent.Name } -Descending |
        Select-Object -First 1
    if (-not $hit) { throw "$name ($arch) not found under $kits; install the WDK" }
    $hit.FullName
}

$signtool = Find-KitTool 'signtool.exe' 'x64'
$inf2cat  = Find-KitTool 'Inf2Cat.exe' 'x86'

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
foreach ($f in 'RawKvmVhf.sys', 'RawKvmVhf.inf') {
    $src = Get-ChildItem -Path $BuildDir -Filter $f -Recurse | Select-Object -First 1
    if (-not $src) { throw "$f not found under $BuildDir" }
    Copy-Item $src.FullName -Destination $OutDir -Force
}

if ($PfxPath) {
    $signArgs = @('/f', $PfxPath, '/p', $PfxPassword)
    $cert = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($PfxPath, $PfxPassword)
} else {
    $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=RawKVM Test Signing' `
        -CertStoreLocation Cert:\CurrentUser\My -HashAlgorithm SHA256 -KeyLength 3072 `
        -NotAfter (Get-Date).AddYears(10)
    $signArgs = @('/sha1', $cert.Thumbprint, '/s', 'My')
}
Export-Certificate -Cert $cert -FilePath (Join-Path $OutDir 'RawKvmTest.cer') -Type CERT | Out-Null
Write-Host "signing with $($cert.Subject) ($($cert.Thumbprint))"

& $signtool sign /fd sha256 @signArgs (Join-Path $OutDir 'RawKvmVhf.sys')
if ($LASTEXITCODE) { throw "signtool failed on RawKvmVhf.sys" }

# Windows 10 2004 (VB) and later, matching the INF's 10.0...19041 models.
& $inf2cat /driver:$OutDir /os:10_VB_X64,10_CO_X64,10_NI_X64,10_GE_X64 /uselocaltime
if ($LASTEXITCODE) { throw "inf2cat failed" }

& $signtool sign /fd sha256 @signArgs (Join-Path $OutDir 'RawKvmVhf.cat')
if ($LASTEXITCODE) { throw "signtool failed on RawKvmVhf.cat" }

Get-ChildItem $OutDir | Format-Table Name, Length
