<#
.SYNOPSIS
    Builds all Clear XR crates in release mode and packages a redistribution zip.
.DESCRIPTION
    Runs cargo build --release for clearxr-space, clearxr-layer, and clearxr-streamer
    (in dependency order), then assembles ClearXR-Server.zip at the repo root with
    only the files needed at runtime.
#>

param(
    [switch]$SkipBuild,
    [string]$OutputPath
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$releaseDir = Join-Path $repoRoot 'clearxr-streamer\target\release'

if (-not $OutputPath) {
    $OutputPath = Join-Path $repoRoot 'ClearXR-Server.zip'
}

# --- Build ---
if (-not $SkipBuild) {
    Write-Host '==> Building clearxr-space (release)' -ForegroundColor Cyan
    cargo build --manifest-path "$repoRoot\clearxr-space\Cargo.toml" --release
    if ($LASTEXITCODE -ne 0) { throw 'clearxr-space build failed' }

    Write-Host '==> Building clearxr-layer (release)' -ForegroundColor Cyan
    cargo build --manifest-path "$repoRoot\clearxr-layer\Cargo.toml" --release
    if ($LASTEXITCODE -ne 0) { throw 'clearxr-layer build failed' }

    Write-Host '==> Building clearxr-streamer (release)' -ForegroundColor Cyan
    cargo build --manifest-path "$repoRoot\clearxr-streamer\Cargo.toml" --release
    if ($LASTEXITCODE -ne 0) { throw 'clearxr-streamer build failed' }
}

# --- Stage ---
$staging = Join-Path $repoRoot '_zip_staging'
if (Test-Path $staging) { Remove-Item -Recurse -Force $staging }
New-Item -ItemType Directory -Path $staging | Out-Null

Write-Host '==> Staging redistributable files' -ForegroundColor Cyan

# Top-level binaries and manifests
$topLevel = @(
    'clearxr-streamer.exe'
    'clear-xr.exe'
    'clear_xr_layer.dll'
    'clear-xr-layer.json'
    'openxr_loader.dll'
    'NvStreamManagerClient.dll'
)
foreach ($file in $topLevel) {
    $src = Join-Path $releaseDir $file
    if (Test-Path $src) {
        Copy-Item $src $staging
    } else {
        Write-Warning "Missing: $file"
    }
}

# Server directory (CloudXR runtime)
$serverSrc = Join-Path $releaseDir 'Server'
if (Test-Path $serverSrc) {
    Copy-Item -Recurse $serverSrc (Join-Path $staging 'Server')
    # Remove dev-only files (.lib, .h, include dirs)
    Get-ChildItem -Path (Join-Path $staging 'Server') -Recurse -Include '*.lib','*.h' |
        Remove-Item -Force
    Get-ChildItem -Path (Join-Path $staging 'Server') -Recurse -Directory -Filter 'include' |
        Remove-Item -Recurse -Force
} else {
    Write-Warning 'Missing: Server directory'
}

# --- Zip ---
if (Test-Path $OutputPath) { Remove-Item $OutputPath }

Write-Host '==> Creating zip' -ForegroundColor Cyan
Compress-Archive -Path "$staging\*" -DestinationPath $OutputPath -CompressionLevel Optimal

Remove-Item -Recurse -Force $staging

$sizeMB = [math]::Round((Get-Item $OutputPath).Length / 1MB, 1)
Write-Host "Created $OutputPath ($sizeMB MB)" -ForegroundColor Green
