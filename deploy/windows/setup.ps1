<#
.SYNOPSIS
  Install RawKVM on this Windows PC for the current user: the edge helper
  (rawkvm-edge.exe) and the widget (RawKVM.exe). No administrator rights,
  no driver. See docs\windows-instalacion.md and ADR 0024.

.DESCRIPTION
  1. Takes the programs from -From, from the folder of this script (the
     unpacked release zip), or downloads the release zip and checks it
     against SHA256SUMS.
  2. Installs them in %LOCALAPPDATA%\Programs\RawKVM (with this script and
     uninstall.ps1), unblocked, started at logon (HKCU Run) and with a Start
     menu entry. Moves away from the manual install of earlier versions.
  3. Pairs once, unless already paired: asks for the hub address and the
     token (hidden), and finds the USB gadget of this PC on the hub. The
     widget takes the same pairing.
  4. Starts both and checks (the same as -Check).

  Safe to repeat. Run it in a normal PowerShell:

    powershell -ExecutionPolicy Bypass -File .\setup.ps1

.PARAMETER Hub
  Hub address (IP or name). Without it, setup asks; Enter finds the hub
  with mDNS.
.PARAMETER TokenFile
  A file holding the pairing token, instead of typing it.
.PARAMETER ForNode
  node_id of this PC's USB gadget, when the hub has several Windows agents.
.PARAMETER Pair
  Pair again even if already paired (e.g. a reinstalled hub).
.PARAMETER From
  Folder with the release zip and SHA256SUMS, or with the unpacked programs.
.PARAMETER Version
  Release to download (e.g. v0.5.0). Default: the latest.
.PARAMETER Check
  Only print the state of every piece, one line each, and exit. Changes
  nothing.
.PARAMETER NoStart
  Do not start the programs now (they start at the next logon).
#>
[CmdletBinding()]
param(
    [string] $Hub,
    [string] $TokenFile,
    [string] $ForNode,
    [switch] $Pair,
    [string] $From,
    [string] $Version,
    [switch] $Check,
    [switch] $NoStart
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

$Repo = 'jcffrt/virtual-KVM'
$InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\RawKVM'
$Edge = Join-Path $InstallDir 'rawkvm-edge.exe'
$Widget = Join-Path $InstallDir 'RawKVM.exe'
$EdgeConfig = Join-Path $env:APPDATA 'rawkvm\edge.toml'
$WidgetConfig = Join-Path $env:LOCALAPPDATA 'rawkvm\widget.toml'
$WidgetLog = Join-Path $env:LOCALAPPDATA 'rawkvm\rawkvm-widget.log'
$OldEdge = Join-Path $env:LOCALAPPDATA 'rawkvm\rawkvm-edge.exe'
$RunKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$ApprovedKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run'
# RawKVM: the name the widget's own "start at logon" switch uses.
$RunNames = @{ RawKvmEdge = $Edge; RawKVM = $Widget }
$Shortcut = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\RawKVM.lnk'
$Programs = 'rawkvm-edge.exe', 'RawKVM.exe'
# Set by Invoke-Program when Microsoft Defender refuses to start a program.
$script:Blocked = $null

function Write-Step([string] $Message) { Write-Host "`n== $Message" -ForegroundColor Cyan }
function Write-Note([string] $Message) { Write-Host "   $Message" }

# ---------------------------------------------------------------- check

$script:Worst = 0
function Write-CheckLine([string] $Level, [string] $What, [string] $Detail) {
    $rank = @{ 'ok' = 0; 'info' = 1; 'warn' = 2; 'FAIL' = 3 }[$Level]
    if ($rank -gt $script:Worst) { $script:Worst = $rank }
    # To the console, not the output: Invoke-Check returns only its verdict.
    Write-Host ('{0,-4}  {1,-10} {2}' -f $Level, $What, $Detail)
}

# Run a GUI-subsystem program of ours and wait for it (the pipe makes
# PowerShell wait). Returns its output lines; $LASTEXITCODE is its code.
# If Microsoft Defender blocks it, $script:Blocked names it, nothing is
# returned and $LASTEXITCODE is 225.
function Invoke-Program([string] $Exe, [string[]] $Arguments) {
    # Native stderr must not become a terminating error.
    $ErrorActionPreference = 'Continue'
    try {
        & $Exe @Arguments 2>&1 | ForEach-Object { "$_" }
    } catch {
        if (-not (Test-DefenderBlock $_)) { throw }
        $script:Blocked = $Exe
        $global:LASTEXITCODE = 225
    }
}

# Defender refused to start a program: ERROR_VIRUS_INFECTED (225) or
# ERROR_VIRUS_DELETED (226) somewhere in the exception, whatever the
# language of the message.
function Test-DefenderBlock([Management.Automation.ErrorRecord] $Record) {
    for ($e = $Record.Exception; $e; $e = $e.InnerException) {
        if ($e -is [ComponentModel.Win32Exception] -and $e.NativeErrorCode -in 225, 226) { return $true }
        # HRESULT_FROM_WIN32 of both: 0x800700E1, 0x800700E2.
        if ($e.HResult -in -2147024671, -2147024670) { return $true }
    }
    $Record.ToString() -match 'virus|potentially unwanted|potencialmente no deseado'
}

# The name of Defender's detection of $Exe, if this user may read it.
function Get-DefenderThreat([string] $Exe) {
    try {
        $leaf = Split-Path -Leaf $Exe
        $d = Get-MpThreatDetection -ErrorAction Stop | Where-Object { "$($_.Resources)" -like "*$leaf*" } |
            Sort-Object InitialDetectionTime -Descending | Select-Object -First 1
        if ($d) { return (Get-MpThreat -ThreatID $d.ThreatID -ErrorAction Stop).ThreatName }
    } catch {
        Write-Verbose "Get-MpThreatDetection: $_"
    }
    $null
}

# What to do when Defender blocks or removes one of our programs: an
# unsigned program flagged by its machine learning (ADR 0024).
function Write-DefenderHelp([string] $Exe, [string] $Hash) {
    $threat = Get-DefenderThreat $Exe
    $what = if ($threat) { " as $threat" } else { '' }
    Write-Host ''
    Write-Host "Microsoft Defender blocked $(Split-Path -Leaf $Exe)$what." -ForegroundColor Yellow
    Write-Host '  It is a false positive of its automatic detection of unsigned programs. Either:'
    Write-Host '  1. Report it to Microsoft (no administrator needed; usually fixed in a few days):'
    Write-Host '     https://www.microsoft.com/wdsi/filesubmission  (Home customer, "Incorrectly detected")'
    if ($Hash) { Write-Host "     SHA-256 $Hash" }
    Write-Host '  2. Or allow it on this PC (needs an administrator): Windows Security >'
    Write-Host '     Virus & threat protection > Protection history > the detection > Actions >'
    Write-Host '     Allow on device (Restore, if it was quarantined). Then run setup.ps1 again.'
}

# hub, token and pinned certificate of a config, never the token itself.
# $null when the key or the value is missing (StrictMode throws on a
# missing property of Get-ItemProperty).
function Get-RegistryValue([string] $Key, [string] $Name) {
    $item = Get-Item $Key -ErrorAction SilentlyContinue
    if ($item) { $item.GetValue($Name) } else { $null }
}

function Get-PairingSummary([string] $Path) {
    if (-not (Test-Path $Path)) { return $null }
    $text = Get-Content -Raw $Path
    $hubAddress = if ($text -match '(?m)^hub = "([^"]+)"') { $Matches[1] } else { 'mDNS' }
    $hasToken = $text -match '(?m)^token = "[0-9a-fA-F]{64}"'
    $pin = if ($text -match '(?m)^hub_fingerprint = "([0-9a-fA-F]{12})') { $Matches[1] } else { $null }
    $forNode = if ($text -match '(?m)^for_node = "([^"]+)"') { $Matches[1] } else { $null }
    [pscustomobject]@{
        Hub = $hubAddress; Token = $hasToken; Pin = $pin; ForNode = $forNode
        Paired = $hasToken -and $pin
    }
}

# The last "connected", reconnect or error line of a log.
function Get-HubState([string] $Path) {
    if (-not (Test-Path $Path)) { return 'warn', "no log yet ($Path)" }
    $last = $null
    foreach ($l in (Get-Content $Path -Tail 300)) {
        if ($l -match ' connected addr=') { $last = 'ok', $l.Trim() }
        elseif ($l -match 'ERROR|rejected') { $last = 'FAIL', $l.Trim() }
        elseif ($l -match 'reconnecting in') { $last = 'warn', $l.Trim() }
    }
    if ($last) { return $last }
    'warn', "no connection lines in $Path"
}

function Invoke-Check {
    $script:Worst = 0
    $script:Blocked = $null
    $edgeVersion = if (Test-Path $Edge) { (Invoke-Program $Edge @('--version')) -join ' ' } else { 'missing' }
    if ($script:Blocked) { $edgeVersion = 'blocked by Microsoft Defender' }
    $widgetVersion = if (Test-Path $Widget) { (Get-Item $Widget).VersionInfo.ProductVersion } else { 'missing' }
    $level = if ((Test-Path $Edge) -and (Test-Path $Widget)) { 'info' } else { 'FAIL' }
    Write-CheckLine $level 'version' "$edgeVersion; widget $widgetVersion; in $InstallDir"

    if ($script:Blocked) {
        Write-CheckLine 'FAIL' 'defender' "$Edge blocked by Microsoft Defender (see below)"
    } elseif (Test-Path $Edge) {
        # config, for node and hub of the edge helper.
        foreach ($l in (Invoke-Program $Edge @('--check'))) {
            if ($l -match '^(ok|info|warn|FAIL)\s+(\S+(?: node)?)\s+(.*)$' -and $Matches[2] -ne 'version') {
                $what = if ($Matches[2] -eq 'config') { 'edge' } else { $Matches[2] }
                Write-CheckLine $Matches[1] $what $Matches[3]
            }
        }
    }

    $w = Get-PairingSummary $WidgetConfig
    if (-not $w) {
        # A widget started a moment ago (setup.ps1 itself) may not have
        # written it yet.
        $started = Get-Process RawKVM -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -eq $Widget } | Select-Object -First 1
        $age = if ($started) { ((Get-Date) - $started.StartTime).TotalSeconds } else { $null }
        if ($null -ne $age -and $age -lt 10) {
            Write-CheckLine 'info' 'widget' ("$WidgetConfig not written yet (the widget started {0:N0} s ago)" -f $age)
        } else {
            Write-CheckLine 'warn' 'widget' "$WidgetConfig missing (the widget writes it when it starts)"
        }
    } else {
        $level = if ($w.Paired) { 'ok' } else { 'FAIL' }
        $pin = if ($w.Pin) { "certificate $($w.Pin)..." } else { 'not paired' }
        $token = if ($w.Token) { 'token set' } else { 'no token' }
        Write-CheckLine $level 'widget' "${WidgetConfig}: hub $($w.Hub), $token, $pin"
    }
    $level, $detail = Get-HubState $WidgetLog
    Write-CheckLine $level 'widget hub' $detail

    $missing = @()
    foreach ($name in $RunNames.Keys) {
        $value = Get-RegistryValue $RunKey $name
        $approved = Get-RegistryValue $ApprovedKey $name
        # Task Manager's "Disabled" keeps the Run value and marks it here.
        $disabled = $approved -and ($approved[0] -band 1)
        if (-not $value -or $value -notlike "*$($RunNames[$name])*" -or $disabled) { $missing += $name }
    }
    if ($missing) {
        Write-CheckLine 'FAIL' 'autostart' "HKCU Run missing, elsewhere or disabled: $($missing -join ', ') (setup.ps1)"
    } else {
        Write-CheckLine 'ok' 'autostart' 'HKCU Run: RawKvmEdge, RawKVM'
    }

    $running = @()
    $stopped = @()
    foreach ($exe in $Edge, $Widget) {
        $name = [IO.Path]::GetFileNameWithoutExtension($exe)
        $p = Get-Process $name -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe } | Select-Object -First 1
        if ($p) { $running += "$name ($($p.Id))" } else { $stopped += $name }
    }
    if ($stopped) {
        Write-CheckLine 'FAIL' 'processes' "not running: $($stopped -join ', ') (log out and in, or setup.ps1)"
    } else {
        Write-CheckLine 'ok' 'processes' ($running -join ', ')
    }

    # The QUIC session to the hub is outbound: no firewall rule. mDNS
    # replies come in on UDP 5353, which the firewall may drop for a
    # program a non-administrator cannot allow.
    $e = Get-PairingSummary $EdgeConfig
    if ($e -and $e.Hub -ne 'mDNS') {
        Write-CheckLine 'ok' 'firewall' "hub address $($e.Hub) saved: no mDNS, outbound QUIC only"
    } else {
        Write-CheckLine 'warn' 'firewall' 'hub found with mDNS at every start; the firewall may drop the replies (UDP 5353): setup.ps1 -Pair -Hub ADDRESS'
    }

    if (Test-Path $OldEdge) {
        Write-CheckLine 'warn' 'old files' "$OldEdge (setup.ps1 removes it)"
    }
    if ($script:Blocked) {
        $hash = try { (Get-FileHash -Algorithm SHA256 $Edge -ErrorAction Stop).Hash.ToLower() } catch { $null }
        Write-DefenderHelp $Edge $hash
    }
    $script:Worst -lt 3
}

if ($Check) {
    if (Invoke-Check) { exit 0 } else { exit 1 }
}

# ---------------------------------------------------------------- source

# A folder with RawKVM-windows-<ver>.zip and SHA256SUMS: check and unpack.
function Expand-Release([string] $Folder) {
    $zip = Get-ChildItem $Folder -Filter 'RawKVM-windows-*.zip' | Sort-Object Name | Select-Object -Last 1
    if (-not $zip) { return $null }
    $sums = Join-Path $Folder 'SHA256SUMS'
    if (-not (Test-Path $sums)) { throw "SHA256SUMS missing next to $($zip.Name)" }
    $line = Get-Content $sums | Where-Object { $_ -match "^[0-9a-f]{64}\s+\*?$([regex]::Escape($zip.Name))$" } | Select-Object -First 1
    if (-not $line) { throw "$($zip.Name) is not in SHA256SUMS" }
    $expected = $line.Substring(0, 64)
    $actual = (Get-FileHash -Algorithm SHA256 $zip.FullName).Hash.ToLower()
    if ($actual -ne $expected) { throw "$($zip.Name): SHA-256 $actual, SHA256SUMS says $expected" }
    Write-Note "$($zip.Name): SHA-256 matches SHA256SUMS"
    $dest = Join-Path ([IO.Path]::GetTempPath()) ("rawkvm-" + [guid]::NewGuid())
    Expand-Archive $zip.FullName $dest
    $dest
}

function Test-ProgramSet([string] $Folder) {
    $Folder -and @($Programs | Where-Object { -not (Test-Path (Join-Path $Folder $_)) }).Count -eq 0
}

function Get-Release([string] $Tag) {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    if (-not $tag) {
        try {
            $tag = (Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest").tag_name
        } catch {
            throw "no release of $Repo found ($($_.Exception.Message)). Pass -From with the zip and SHA256SUMS, or the unpacked programs."
        }
    }
    $dest = Join-Path ([IO.Path]::GetTempPath()) ("rawkvm-" + [guid]::NewGuid())
    New-Item -ItemType Directory $dest | Out-Null
    $zip = "RawKVM-windows-$($tag.TrimStart('v')).zip"
    Write-Note "downloading $zip and SHA256SUMS of $tag"
    try {
        foreach ($f in $zip, 'SHA256SUMS') {
            Invoke-WebRequest -UseBasicParsing "https://github.com/$Repo/releases/download/$tag/$f" -OutFile (Join-Path $dest $f)
        }
    } catch {
        if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { throw }
        Write-Note "download failed ($($_.Exception.Message)); trying gh"
        gh release download $tag -R $Repo -p $zip -p SHA256SUMS -D $dest --clobber
        if ($LASTEXITCODE) { throw "gh release download failed ($LASTEXITCODE)" }
    }
    $dest
}

Write-Step 'Programs'
$here = if ($PSScriptRoot) { $PSScriptRoot } else { $null }
$source = $null
if ($From) {
    $source = Expand-Release $From
    if (-not $source) { $source = (Resolve-Path $From).Path }
} elseif (-not $Version -and (Test-ProgramSet $here)) {
    # The unpacked zip, or the install itself (setup.ps1 -Pair or -Check
    # later): the programs already there.
    $source = $here
} else {
    $source = Expand-Release (Get-Release $Version)
}
if (-not (Test-ProgramSet $source)) {
    if ($source -and (Test-Path (Join-Path $source 'RawKVM.exe'))) {
        # Defender removes a file it detects right after it is unpacked.
        Write-DefenderHelp (Join-Path $source 'rawkvm-edge.exe') $null
        exit 1
    }
    throw "$source has no $($Programs -join ' and ')"
}
Write-Note "from $source"

# Stop both, wherever they run from (older installs too).
Get-Process rawkvm-edge, RawKVM -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500
New-Item -ItemType Directory -Force $InstallDir | Out-Null
$sameFolder = [IO.Path]::GetFullPath($source).TrimEnd('\') -eq [IO.Path]::GetFullPath($InstallDir).TrimEnd('\')
foreach ($f in $Programs + 'setup.ps1', 'uninstall.ps1', 'LEEME.md', 'COMMIT.txt') {
    $src = Join-Path $source $f
    if ((Test-Path $src) -and -not $sameFolder) { Copy-Item $src $InstallDir -Force }
}
# Downloaded files carry the "from the Internet" mark: SmartScreen would
# stop an unsigned program at logon.
Get-ChildItem $InstallDir | Unblock-File
Write-Note "installed in $InstallDir"

# Defender may refuse the edge helper (ADR 0024): say so now, not as an
# exception dump later.
$edgeHash = try { (Get-FileHash -Algorithm SHA256 (Join-Path $source 'rawkvm-edge.exe')).Hash.ToLower() } catch { $null }
$script:Blocked = $null
if (Test-Path $Edge) { Invoke-Program $Edge @('--version') | Out-Null }
if ($script:Blocked -or -not (Test-Path $Edge)) {
    Write-DefenderHelp $Edge $edgeHash
    exit 1
}

if (Test-Path $OldEdge) {
    Remove-Item $OldEdge -Force
    Write-Note "removed the previous manual install ($OldEdge)"
}

# ---------------------------------------------------------------- pairing

# The token, hidden, into a file only this user reads (%TEMP%); the
# caller removes it. $null: keep the saved one.
function Read-Token([bool] $HaveOne) {
    $prompt = 'Pairing token (on the hub: sudo rawkvm-hub --show-token)'
    if ($HaveOne) { $prompt += '; Enter keeps the saved one' }
    while ($true) {
        $secure = Read-Host -AsSecureString $prompt
        $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
        try { $plain = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr).Trim() }
        finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr) }
        if (-not $plain -and $HaveOne) { return $null }
        if ($plain -match '^[0-9a-fA-F]{64}$') {
            $file = [IO.Path]::GetTempFileName()
            [IO.File]::WriteAllText($file, $plain)
            return $file
        }
        Write-Host 'That is not 64 hex characters; try again.' -ForegroundColor Yellow
    }
}

Write-Step 'Pairing'
$saved = Get-PairingSummary $EdgeConfig
$paired = $saved -and $saved.Paired -and $saved.ForNode
if ($paired -and -not ($Pair -or $Hub -or $TokenFile -or $ForNode)) {
    Write-Note "already paired with $($saved.Hub), for node $($saved.ForNode) (setup.ps1 -Pair to pair again)"
} else {
    $interactive = [Environment]::UserInteractive -and -not [Console]::IsInputRedirected
    if (-not $Hub -and $interactive) {
        $default = if ($saved -and $saved.Hub -ne 'mDNS') { $saved.Hub } else { $null }
        $question = if ($default) { "Hub address [$default]" } else { 'Hub address (Enter: find it with mDNS)' }
        $answer = (Read-Host $question).Trim()
        $Hub = if ($answer) { $answer } else { $default }
    }
    $tempToken = $null
    try {
        if (-not $TokenFile) {
            if ($interactive) {
                $tempToken = Read-Token ($saved -and $saved.Token)
            } elseif (-not ($saved -and $saved.Token)) {
                throw 'no pairing token: pass -TokenFile FILE (or run in a console to type it)'
            }
        }
        $pairArgs = @('--pair')
        if ($Hub) { $pairArgs += @('--hub', $Hub) }
        if ($TokenFile -and -not (Test-Path -LiteralPath $TokenFile -PathType Leaf)) {
            throw "no token file $TokenFile"
        }
        $file = if ($TokenFile) { (Resolve-Path -LiteralPath $TokenFile).Path } else { $tempToken }
        if ($file) { $pairArgs += @('--token-file', $file) }
        if ($ForNode) { $pairArgs += @('--for-node', $ForNode) }
        $out = @(Invoke-Program $Edge $pairArgs)
        $code = $LASTEXITCODE
        $out | ForEach-Object { Write-Note $_ }
        # Several Windows agents on the hub: let the user choose.
        $candidates = @($out | Where-Object { $_ -match '^\s+[0-9a-f]{8}-[0-9a-f-]{27}\s' })
        if ($code -and $candidates -and $interactive) {
            for ($i = 0; $i -lt $candidates.Count; $i++) { Write-Host "  $($i + 1)) $($candidates[$i].Trim())" }
            $n = [int](Read-Host "Which one is this PC's USB gadget? 1-$($candidates.Count)")
            if ($n -lt 1 -or $n -gt $candidates.Count) { throw 'no such entry' }
            $id = ($candidates[$n - 1].Trim() -split '\s+')[0]
            $out = @(Invoke-Program $Edge @('--pair', '--for-node', $id))
            $code = $LASTEXITCODE
            $out | ForEach-Object { Write-Note $_ }
        }
        if ($code) { throw "pairing failed (rawkvm-edge --pair: $code)" }
    } finally {
        if ($tempToken) { Remove-Item $tempToken -Force -ErrorAction SilentlyContinue }
    }
}

# ---------------------------------------------------------------- logon

Write-Step 'Start at logon'
foreach ($name in $RunNames.Keys) {
    Set-ItemProperty -Path $RunKey -Name $name -Value "`"$($RunNames[$name])`""
    # Forget a "Disabled" from Task Manager: installing means enabled.
    Remove-ItemProperty -Path $ApprovedKey -Name $name -ErrorAction SilentlyContinue
}
$shell = New-Object -ComObject WScript.Shell
$lnk = $shell.CreateShortcut($Shortcut)
$lnk.TargetPath = $Widget
$lnk.WorkingDirectory = $InstallDir
$lnk.Description = 'RawKVM'
$lnk.Save()
Write-Note 'HKCU Run: RawKvmEdge, RawKVM; Start menu: RawKVM'

if ($NoStart) {
    Write-Note 'not started (-NoStart): they start at the next logon'
} else {
    Start-Process $Edge
    Start-Process $Widget
    # Give them a moment to connect before checking.
    Start-Sleep -Seconds 4
}

Write-Step 'Check'
$ok = Invoke-Check
Write-Host ''
Write-Host "Check again at any time: powershell -ExecutionPolicy Bypass -File `"$InstallDir\setup.ps1`" -Check"
Write-Host "Uninstall: powershell -ExecutionPolicy Bypass -File `"$InstallDir\uninstall.ps1`""
if (-not $ok) { exit 1 }
