#Requires -Version 5.1
<# Install codegraph from a GitHub Release after mandatory SHA-256 verification.
Environment: CODEGRAPH_VERSION and CODEGRAPH_INSTALL_DIR. #>
$ErrorActionPreference = 'Stop'

$Repo = 'sunerpy/codegraph-rust'
$Bin = 'codegraph'
$sums = 'SHA256SUMS'
$ext = 'zip'

try {
    [Net.ServicePointManager]::SecurityProtocol = `
        [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch {}

$archRaw = $env:PROCESSOR_ARCHITEW6432
if (-not $archRaw) { $archRaw = $env:PROCESSOR_ARCHITECTURE }
if (-not $archRaw) {
    try { $archRaw = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString() }
    catch { $archRaw = '' }
}
switch -Regex ($archRaw) {
    '^(AMD64|x64|x86_64)$' { $archPart = 'x86_64' }
    '^(ARM64|aarch64)$'    { $archPart = 'aarch64' }
    default { throw "Unsupported architecture: '$archRaw'" }
}
$target = "$archPart-pc-windows-msvc"

if ($env:CODEGRAPH_VERSION) {
    $version = $env:CODEGRAPH_VERSION -replace '^v', ''
} else {
    $release = Invoke-RestMethod `
        -Uri "https://api.github.com/repos/$Repo/releases/latest" `
        -Headers @{ 'User-Agent' = 'codegraph-installer' }
    $version = $release.tag_name -replace '^v', ''
    if (-not $version) { throw 'Could not resolve the latest release' }
}

$asset = "$Bin-$version-$target.$ext"
$baseUrl = "https://github.com/$Repo/releases/download/v$version"
$installDir = if ($env:CODEGRAPH_INSTALL_DIR) {
    $env:CODEGRAPH_INSTALL_DIR
} else {
    Join-Path $env:LOCALAPPDATA 'Programs\codegraph'
}
if (-not (Get-Command Get-FileHash -ErrorAction SilentlyContinue)) {
    throw 'Get-FileHash is required; refusing an unverified install'
}

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("codegraph-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
try {
    $archive = Join-Path $tmp $asset
    $sumsPath = Join-Path $tmp $sums
    $headers = @{ 'User-Agent' = 'codegraph-installer' }
    Invoke-WebRequest -Uri "$baseUrl/$asset" -OutFile $archive -Headers $headers
    try {
        Invoke-WebRequest -Uri "$baseUrl/$sums" -OutFile $sumsPath -Headers $headers
    } catch {
        throw "Could not download $sums; refusing an unverified install"
    }

    $expected = $null
    foreach ($line in (Get-Content -LiteralPath $sumsPath)) {
        $parts = $line.Trim() -split '\s+', 2
        if ($parts.Count -eq 2 -and $parts[1].TrimStart('*') -eq $asset) {
            $expected = $parts[0]
            break
        }
    }
    if (-not $expected) { throw "$sums has no entry for $asset; refusing an unverified install" }
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash
    if ($actual -ine $expected) {
        throw "Checksum mismatch for $asset; refusing a corrupted or tampered archive"
    }
    Write-Host "sha256: OK ($actual)"

    Expand-Archive -Path $archive -DestinationPath $tmp -Force
    $source = Join-Path $tmp "$Bin.exe"
    if (-not (Test-Path -LiteralPath $source)) { throw "Archive did not contain $Bin.exe" }
    New-Item -ItemType Directory -Path $installDir -Force | Out-Null
    Copy-Item -LiteralPath $source -Destination (Join-Path $installDir "$Bin.exe") -Force
} finally {
    Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

$exe = Join-Path $installDir "$Bin.exe"
Write-Host "Installed: $exe"
& $exe --version

# Persist only the User PATH. Never write the process-composed $env:Path back to
# the user environment because it can contain System PATH entries and expanded
# variable references. Update the current process separately for immediate use.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not $userPath) { $userPath = '' }
$normalizedInstallDir = $installDir.TrimEnd('\')
$onUserPath = $false
foreach ($entry in $userPath.Split(';')) {
    if ($entry.Trim().TrimEnd('\') -ieq $normalizedInstallDir) {
        $onUserPath = $true
        break
    }
}
if (-not $onUserPath) {
    $trimmedUserPath = $userPath.Trim().TrimEnd(';')
    $newUserPath = if ($trimmedUserPath) { "$trimmedUserPath;$installDir" } else { $installDir }
    [Environment]::SetEnvironmentVariable('Path', $newUserPath, 'User')
    Write-Host "Added $installDir to the User PATH."
}

$onProcessPath = $false
foreach ($entry in $env:Path.Split(';')) {
    if ($entry.Trim().TrimEnd('\') -ieq $normalizedInstallDir) {
        $onProcessPath = $true
        break
    }
}
if (-not $onProcessPath) {
    $env:Path = if ($env:Path.TrimEnd(';')) { "$($env:Path.TrimEnd(';'));$installDir" } else { $installDir }
}

Write-Host "Done. Run '$Bin --help' to get started."
