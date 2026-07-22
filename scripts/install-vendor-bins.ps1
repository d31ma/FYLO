param(
    [string]$Destination
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Install-VerifiedGitHubBinary {
    param(
        [Parameter(Mandatory = $true)][string]$Repository,
        [Parameter(Mandatory = $true)][string]$Version,
        [Parameter(Mandatory = $true)][string]$Asset,
        [Parameter(Mandatory = $true)][string]$ExpectedSha256,
        [Parameter(Mandatory = $true)][string]$Executable
    )

    $assetPath = Join-Path $Destination $Executable
    $url = "https://github.com/$Repository/releases/download/$Version/$Asset"
    Invoke-WebRequest -Uri $url -OutFile $assetPath

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
    -Repository 'd31ma/TTID' `
    -Version 'v26.28.02' `
    -Asset 'ttid-windows-x64.exe' `
    -ExpectedSha256 'b4beab399741b46a82d037cfef2b298418e7596245684aed91154aee8d6771aa' `
    -Executable 'ttid.exe'
Install-VerifiedGitHubBinary `
    -Repository 'd31ma/CHEX' `
    -Version 'v26.28.02' `
    -Asset 'chex-windows-x64.exe' `
    -ExpectedSha256 'd00d48eeaf5f24fa39ec8dfc2f6963ab8ab1a38e20523e239bea63498815bd18' `
    -Executable 'chex.exe'

$env:Path = "$Destination;$env:Path"
if ($env:GITHUB_PATH) {
    Add-Content -Path $env:GITHUB_PATH -Value $Destination
}
& (Join-Path $Destination 'ttid.exe') --help | Out-Null
& (Join-Path $Destination 'chex.exe') --help | Out-Null
