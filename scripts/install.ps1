#Requires -Version 5.1
<#
.SYNOPSIS
    codegraph one-liner installer (Windows, PowerShell 5.1+).

.DESCRIPTION
    irm https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.ps1 | iex

    Env overrides:
      CODEGRAPH_VERSION      pin a release (e.g. 0.4.0 or v0.4.0); default: latest
      CODEGRAPH_INSTALL_DIR  install destination; default: %LOCALAPPDATA%\Programs\codegraph
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

$asset = "$Bin-$version-$target.$ext"
$url = "https://github.com/$Repo/releases/download/v$version/$asset"

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
