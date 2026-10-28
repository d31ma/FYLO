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
