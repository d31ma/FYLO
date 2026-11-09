param(
    [string]$Destination
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Save-ReleaseAssetWithRetry {
    param(
        [Parameter(Mandatory = $true)][string]$Url,
        [Parameter(Mandatory = $true)][string]$OutFile
    )

    $attempts = 6
    for ($attempt = 1; $attempt -le $attempts; $attempt++) {
        try {
            Invoke-WebRequest -Uri $Url -OutFile $OutFile
            return $true
        } catch {
            Write-Warning "Download attempt $attempt of $attempts failed for ${Url}: $($_.Exception.Message)"
        }
        if ($attempt -lt $attempts) {
            Start-Sleep -Seconds 10
        }
    }
    return $false
}

function Save-ReleaseAssetWithGitHubCli {
    param(
        [Parameter(Mandatory = $true)][string]$Repository,
        [Parameter(Mandatory = $true)][string]$Version,
        [Parameter(Mandatory = $true)][string]$Asset,
        [Parameter(Mandatory = $true)][string]$OutFile
    )

    $directory = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName())
    New-Item -ItemType Directory -Force -Path $directory | Out-Null
    gh release download $Version --repo $Repository --pattern $Asset --dir $directory
    if ($LASTEXITCODE -ne 0) {
        throw "gh release download failed for $Repository@$Version/$Asset."
    }
    Move-Item -Force -Path (Join-Path $directory $Asset) -Destination $OutFile
    Remove-Item -Recurse -Force -Path $directory
}

function Save-ReleaseAsset {
    param(
        [Parameter(Mandatory = $true)][string]$Repository,
        [Parameter(Mandatory = $true)][string]$Version,
        [Parameter(Mandatory = $true)][string]$Asset,
        [Parameter(Mandatory = $true)][string]$OutFile
    )

    $url = "https://github.com/$Repository/releases/download/$Version/$Asset"
    if (Save-ReleaseAssetWithRetry -Url $url -OutFile $OutFile) {
        return
    }
    if (-not $env:GH_TOKEN) {
        throw "Download of $Repository@$Version/$Asset failed and GH_TOKEN is not set for the gh fallback."
    }
    Write-Warning "Download of $Repository@$Version/$Asset failed; retrying through the GitHub API with gh."
    Save-ReleaseAssetWithGitHubCli -Repository $Repository -Version $Version -Asset $Asset -OutFile $OutFile
}

function Install-VerifiedGitHubBinary {
    param(
        [Parameter(Mandatory = $true)][string]$Repository,
        [Parameter(Mandatory = $true)][string]$Version,
        [Parameter(Mandatory = $true)][string]$Asset,
        [Parameter(Mandatory = $true)][string]$ExpectedSha256,
        [Parameter(Mandatory = $true)][string]$Executable
    )

    $assetPath = Join-Path $Destination $Executable
    Save-ReleaseAsset -Repository $Repository -Version $Version -Asset $Asset -OutFile $assetPath

    $actual = (Get-FileHash -Algorithm SHA256 $assetPath).Hash.ToLowerInvariant()
    if ($actual -ne $ExpectedSha256.ToLowerInvariant()) {
        Remove-Item $assetPath -Force -ErrorAction SilentlyContinue
        throw "Checksum mismatch for $Repository@$Version/$Asset."
    }

    Write-Host "Installed verified $Repository@$Version/$Asset."
}

if (-not $Destination) {
    $temporaryRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
    $Destination = Join-Path $temporaryRoot 'fylo-vendor-bin'
}

New-Item -ItemType Directory -Force -Path $Destination | Out-Null
Install-VerifiedGitHubBinary `
    -Repository 'd31ma/Tachyon' `
    -Version 'v26.36.01' `
    -Asset 'ty-windows-x64.exe' `
    -ExpectedSha256 '8b8cc73a8407e5397e66d7fab6f3946162847c4b5639d089d0f16af57f3631f8' `
    -Executable 'ty.exe'

$env:Path = "$Destination;$env:Path"
if ($env:GITHUB_PATH) {
    Add-Content -Path $env:GITHUB_PATH -Value $Destination
}
$tachyonVersion = & (Join-Path $Destination 'ty.exe') --version
if ($tachyonVersion.Trim() -ne '26.36.01') {
    throw "Unexpected TACHYON version: $tachyonVersion"
}
