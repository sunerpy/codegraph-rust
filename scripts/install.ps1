#Requires -Version 5.1
<#
.SYNOPSIS
    codegraph one-liner installer (Windows, PowerShell 5.1+).

.DESCRIPTION
    irm https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.ps1 | iex

      Env overrides:
      CODEGRAPH_VERSION      pin a release (e.g. 0.4.0 or v0.4.0); default: latest
      CODEGRAPH_INSTALL_DIR  install destination; default: %LOCALAPPDATA%\Programs\codegraph
      CODEGRAPH_SKIP_CHECKSUM
                             set to any non-empty value to proceed when the
                             download CANNOT be verified — i.e. Get-FileHash is
                             unavailable, or the release has no usable
                             SHA256SUMS (releases cut before checksums were
                             published). Without it the installer REFUSES to
                             install rather than run an unverified binary. It
                             never bypasses a checksum MISMATCH — a mismatch
                             always aborts.
#>

$ErrorActionPreference = 'Stop'

# TLS 1.2 for GitHub on PowerShell 5.1.
try {
    [Net.ServicePointManager]::SecurityProtocol = `
        [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch {
    # Older runtimes may not expose Tls12; the request below will surface any real failure.
}

$Repo = 'sunerpy/codegraph-rust'
$Bin = 'codegraph'

# Detect architecture. A 32-bit shell on 64-bit Windows reports its own
# (32-bit) arch in PROCESSOR_ARCHITECTURE and the true OS arch in
# PROCESSOR_ARCHITEW6432, so prefer the latter when present.
$archRaw = $env:PROCESSOR_ARCHITEW6432
if (-not $archRaw) { $archRaw = $env:PROCESSOR_ARCHITECTURE }
if (-not $archRaw) {
    try {
        $archRaw = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    } catch {
        $archRaw = ''
    }
}

switch -Regex ($archRaw) {
    '^(AMD64|x64|x86_64)$' { $archPart = 'x86_64' }
    '^(ARM64|aarch64)$'    { $archPart = 'aarch64' }
    default { throw "Unsupported architecture: '$archRaw' (supported: AMD64/x86_64, ARM64/aarch64)" }
}

$target = "$archPart-pc-windows-msvc"
$ext = 'zip'

# Resolve version: env override or latest-release API.
if ($env:CODEGRAPH_VERSION) {
    $version = $env:CODEGRAPH_VERSION -replace '^v', ''
} else {
    Write-Host 'Resolving latest release...'
    $api = "https://api.github.com/repos/$Repo/releases/latest"
    $headers = @{ 'User-Agent' = 'codegraph-installer' }
    $release = Invoke-RestMethod -Uri $api -Headers $headers
    $tag = $release.tag_name
    if (-not $tag) { throw "Could not resolve latest release tag from $api" }
    $version = $tag -replace '^v', ''
}

$sums = 'SHA256SUMS'
$asset = "$Bin-$version-$target.$ext"
$releaseBase = "https://github.com/$Repo/releases/download/v$version"
$url = "$releaseBase/$asset"
$sumsUrl = "$releaseBase/$sums"

# Fail closed unless the operator explicitly opted out.
function Assert-CanSkipVerification([string]$Reason) {
    if ($env:CODEGRAPH_SKIP_CHECKSUM) {
        Write-Warning "Cannot verify download ($Reason)."
        Write-Warning 'CODEGRAPH_SKIP_CHECKSUM is set — installing an UNVERIFIED binary.'
        return
    }
    throw ("Cannot verify the download: $Reason. Refusing to install an unverified " +
        'binary. To proceed anyway, set CODEGRAPH_SKIP_CHECKSUM=1.')
}

if ($env:CODEGRAPH_INSTALL_DIR) {
    $installDir = $env:CODEGRAPH_INSTALL_DIR
} else {
    $installDir = Join-Path $env:LOCALAPPDATA 'Programs\codegraph'
}

Write-Host "Installing $Bin v$version ($target)"
Write-Host "  from: $url"
Write-Host "  to:   $installDir\$Bin.exe"

# Temp workspace, cleaned up at the end.
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("codegraph-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
try {
    $zipPath = Join-Path $tmp $asset
    $headers = @{ 'User-Agent' = 'codegraph-installer' }
    Invoke-WebRequest -Uri $url -OutFile $zipPath -Headers $headers

    # Integrity gate — runs BEFORE Expand-Archive, so an unverified archive is
    # never unpacked and its binary is never executed.
    if (-not (Get-Command Get-FileHash -ErrorAction SilentlyContinue)) {
        Assert-CanSkipVerification 'Get-FileHash is unavailable'
    } else {
        $sumsPath = Join-Path $tmp $sums
        $sumsOk = $true
        try {
            Invoke-WebRequest -Uri $sumsUrl -OutFile $sumsPath -Headers $headers
        } catch {
            $sumsOk = $false
        }
        if (-not $sumsOk) {
            Assert-CanSkipVerification "could not download $sumsUrl"
        } else {
            # Each line is `<hex><sep><name>`; tolerate CRLF (Get-Content strips
            # the line ending) and the BSD `*name` marker.
            $expected = $null
            foreach ($line in (Get-Content -LiteralPath $sumsPath)) {
                $parts = $line.Trim() -split '\s+', 2
                if ($parts.Count -ne 2) { continue }
                if ($parts[1].TrimStart('*') -eq $asset) { $expected = $parts[0]; break }
            }
            if (-not $expected) {
                Assert-CanSkipVerification "$sums has no entry for $asset"
            } else {
                $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $zipPath).Hash
                if ($actual -ine $expected) {
                    throw ("Checksum MISMATCH for ${asset}: expected $expected, actual $actual. " +
                        'Refusing to install a corrupted or tampered archive.')
                }
                Write-Host "  sha256: OK ($actual)"
            }
        }
    }

    Expand-Archive -Path $zipPath -DestinationPath $tmp -Force

    $exeSrc = Join-Path $tmp "$Bin.exe"
    if (-not (Test-Path $exeSrc)) {
        throw "Archive did not contain expected binary '$Bin.exe'"
    }

    New-Item -ItemType Directory -Path $installDir -Force | Out-Null
    Copy-Item -Path $exeSrc -Destination (Join-Path $installDir "$Bin.exe") -Force
} finally {
    Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

$exePath = Join-Path $installDir "$Bin.exe"
Write-Host "Installed: $exePath"
& $exePath --version

# Add install dir to the USER PATH if missing, and note a new shell is needed.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not $userPath) { $userPath = '' }
$onPath = $false
foreach ($p in $userPath.Split(';')) {
    if ($p.TrimEnd('\') -ieq $installDir.TrimEnd('\')) { $onPath = $true; break }
}
if (-not $onPath) {
    $newPath = if ($userPath.TrimEnd(';')) { "$($userPath.TrimEnd(';'));$installDir" } else { $installDir }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Host ''
    Write-Host "Added $installDir to your USER PATH."
    Write-Host 'Open a new terminal for the PATH change to take effect.'
}

Write-Host ''
Write-Host "Done. Run '$Bin --help' to get started."
