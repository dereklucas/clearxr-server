param(
    [string]$StreamManagerZip,
    [string]$CloudXrRuntimeZip,
    [string]$VendorRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Get-RepoRoot {
    return (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
}

function Find-SingleZip {
    param(
        [string]$Root,
        [string]$Pattern,
        [string]$Label
    )

    $matches = @(Get-ChildItem -Path $Root -File -Filter $Pattern | Sort-Object Name)
    if ($matches.Count -eq 1) {
        return $matches[0].FullName
    }

    if ($matches.Count -eq 0) {
        throw "Could not find $Label zip matching '$Pattern' under '$Root'."
    }

    $paths = $matches | ForEach-Object { $_.FullName }
    throw "Found multiple $Label zips. Pass -$Label explicitly. Matches: $($paths -join ', ')"
}

function New-TempDirectory {
    $path = Join-Path ([System.IO.Path]::GetTempPath()) ("clearxr-vendor-build-" + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $path | Out-Null
    return $path
}

function Expand-Zip {
    param(
        [string]$ZipPath,
        [string]$DestinationPath
    )

    New-Item -ItemType Directory -Path $DestinationPath -Force | Out-Null
    Expand-Archive -LiteralPath $ZipPath -DestinationPath $DestinationPath -Force
}

function Copy-FileInto {
    param(
        [string]$SourcePath,
        [string]$DestinationPath
    )

    $parent = Split-Path -Parent $DestinationPath
    if ($parent) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }

    Copy-Item -LiteralPath $SourcePath -Destination $DestinationPath -Force
}

function Copy-DirectoryInto {
    param(
        [string]$SourcePath,
        [string]$DestinationPath
    )

    New-Item -ItemType Directory -Path $DestinationPath -Force | Out-Null
    Get-ChildItem -LiteralPath $SourcePath -Force | ForEach-Object {
        Copy-Item -LiteralPath $_.FullName -Destination $DestinationPath -Recurse -Force
    }
}

function Get-RelativePath {
    param(
        [string]$BasePath,
        [string]$Path
    )

    $baseFullPath = [System.IO.Path]::GetFullPath($BasePath)
    if (-not $baseFullPath.EndsWith([System.IO.Path]::DirectorySeparatorChar)) {
        $baseFullPath += [System.IO.Path]::DirectorySeparatorChar
    }

    $pathFullPath = [System.IO.Path]::GetFullPath($Path)
    $baseUri = New-Object System.Uri($baseFullPath)
    $pathUri = New-Object System.Uri($pathFullPath)
    return $baseUri.MakeRelativeUri($pathUri).ToString().Replace("\", "/")
}

function Test-IsManagedVendorPath {
    param([string]$RelativePath)

    $managedPrefixes = @(
        "NvStreamManagerClient.dll",
        "NvStreamManagerClient.h",
        "Server/CloudXrService.exe",
        "Server/NvStreamManager.exe",
        "Server/releases/"
    )

    foreach ($prefix in $managedPrefixes) {
        if ($RelativePath.Equals($prefix, [System.StringComparison]::OrdinalIgnoreCase)) {
            return $true
        }

        if ($RelativePath.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)) {
            return $true
        }
    }

    return $false
}

function Copy-PreservedVendorFiles {
    param(
        [string]$CurrentVendorRoot,
        [string]$StageVendorRoot
    )

    if (-not (Test-Path -LiteralPath $CurrentVendorRoot)) {
        return
    }

    $overlayPaths = @(
        "Server/cloudxr-runtime.yaml"
    )

    Get-ChildItem -LiteralPath $CurrentVendorRoot -Recurse -File | ForEach-Object {
        $relativePath = Get-RelativePath -BasePath $CurrentVendorRoot -Path $_.FullName
        $shouldPreserve = $overlayPaths -contains $relativePath

        if (-not $shouldPreserve) {
            $shouldPreserve = -not (Test-IsManagedVendorPath -RelativePath $relativePath)
        }

        if ($shouldPreserve) {
            $destinationPath = Join-Path $StageVendorRoot $relativePath
            Copy-FileInto -SourcePath $_.FullName -DestinationPath $destinationPath
        }
    }
}

$repoRoot = Get-RepoRoot

if (-not $StreamManagerZip) {
    $StreamManagerZip = Find-SingleZip -Root $repoRoot -Pattern "Stream-Manager-*-win64.zip" -Label "StreamManagerZip"
}

if (-not $CloudXrRuntimeZip) {
    $CloudXrRuntimeZip = Find-SingleZip -Root $repoRoot -Pattern "cloudxr-runtime_*.zip" -Label "CloudXrRuntimeZip"
}

if (-not $VendorRoot) {
    $VendorRoot = Join-Path $repoRoot "vendor"
}

$StreamManagerZip = (Resolve-Path $StreamManagerZip).Path
$CloudXrRuntimeZip = (Resolve-Path $CloudXrRuntimeZip).Path

$tempRoot = New-TempDirectory
try {
    $streamManagerExtractRoot = Join-Path $tempRoot "stream-manager"
    $cloudXrOuterExtractRoot = Join-Path $tempRoot "cloudxr-outer"
    $cloudXrSdkExtractRoot = Join-Path $tempRoot "cloudxr-sdk"
    $stageVendorRoot = Join-Path $tempRoot "vendor-stage"

    Write-Host "Extracting Stream Manager from $StreamManagerZip"
    Expand-Zip -ZipPath $StreamManagerZip -DestinationPath $streamManagerExtractRoot

    Write-Host "Extracting CloudXR runtime bundle from $CloudXrRuntimeZip"
    Expand-Zip -ZipPath $CloudXrRuntimeZip -DestinationPath $cloudXrOuterExtractRoot

    $runtimeSdkZip = Get-ChildItem -Path $cloudXrOuterExtractRoot -Recurse -File -Filter "CloudXR-*-Win64-sdk.zip" | Select-Object -First 1
    if (-not $runtimeSdkZip) {
        throw "Could not find the nested CloudXR Win64 SDK zip inside '$CloudXrRuntimeZip'."
    }

    Write-Host "Extracting nested CloudXR SDK from $($runtimeSdkZip.FullName)"
    Expand-Zip -ZipPath $runtimeSdkZip.FullName -DestinationPath $cloudXrSdkExtractRoot

    $versionFile = Join-Path $cloudXrSdkExtractRoot "VERSION"
    if (-not (Test-Path -LiteralPath $versionFile)) {
        throw "CloudXR SDK extraction is missing VERSION."
    }

    $runtimeVersion = Get-Content -LiteralPath $versionFile |
        ForEach-Object { $_.Trim() } |
        Where-Object { $_ } |
        Select-Object -First 1
    if (-not $runtimeVersion) {
        throw "CloudXR SDK VERSION file is empty."
    }

    $sampleClientRoot = Join-Path $streamManagerExtractRoot "SampleClient"
    $serverRoot = Join-Path $streamManagerExtractRoot "Server"
    if (-not (Test-Path -LiteralPath $sampleClientRoot)) {
        throw "Stream Manager zip is missing SampleClient/."
    }
    if (-not (Test-Path -LiteralPath $serverRoot)) {
        throw "Stream Manager zip is missing Server/."
    }

    New-Item -ItemType Directory -Path $stageVendorRoot -Force | Out-Null

    Copy-FileInto -SourcePath (Join-Path $sampleClientRoot "NvStreamManagerClient.dll") -DestinationPath (Join-Path $stageVendorRoot "NvStreamManagerClient.dll")
    Copy-FileInto -SourcePath (Join-Path $sampleClientRoot "NvStreamManagerClient.h") -DestinationPath (Join-Path $stageVendorRoot "NvStreamManagerClient.h")
    Copy-FileInto -SourcePath (Join-Path $serverRoot "CloudXrService.exe") -DestinationPath (Join-Path $stageVendorRoot "Server/CloudXrService.exe")
    Copy-FileInto -SourcePath (Join-Path $serverRoot "NvStreamManager.exe") -DestinationPath (Join-Path $stageVendorRoot "Server/NvStreamManager.exe")
    Copy-FileInto -SourcePath (Join-Path $serverRoot "cloudxr-runtime.yaml") -DestinationPath (Join-Path $stageVendorRoot "Server/cloudxr-runtime.yaml")
    Copy-DirectoryInto -SourcePath $cloudXrSdkExtractRoot -DestinationPath (Join-Path $stageVendorRoot "Server/releases/$runtimeVersion")

    Copy-PreservedVendorFiles -CurrentVendorRoot $VendorRoot -StageVendorRoot $stageVendorRoot

    if (Test-Path -LiteralPath $VendorRoot) {
        Remove-Item -LiteralPath $VendorRoot -Recurse -Force
    }

    Move-Item -LiteralPath $stageVendorRoot -Destination $VendorRoot

    Write-Host "Vendor directory rebuilt at $VendorRoot"
    Write-Host "  Stream Manager zip: $StreamManagerZip"
    Write-Host "  CloudXR runtime zip: $CloudXrRuntimeZip"
    Write-Host "  Runtime version: $runtimeVersion"
}
finally {
    if (Test-Path -LiteralPath $tempRoot) {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force
    }
}
