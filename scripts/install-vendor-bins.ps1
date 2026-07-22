param(
    [string]$Destination
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Install-VerifiedGitHubBinary {
    param(
        [Parameter(Mandatory = $true)][string]$Repository,
        [Parameter(Mandatory = $true)][string]$Asset,
        [Parameter(Mandatory = $true)][string]$Executable
    )

    $headers = @{
        Accept = 'application/vnd.github+json'
        'X-GitHub-Api-Version' = '2022-11-28'
        'User-Agent' = 'fylo-windows-ci'
    }
    if ($env:GITHUB_TOKEN) {
        $headers.Authorization = "Bearer $env:GITHUB_TOKEN"
    }

    $release = Invoke-RestMethod -Headers $headers -Uri "https://api.github.com/repos/$Repository/releases/latest"
    $tag = $release.tag_name
    if (-not $tag) {
        throw "Unable to resolve the latest release tag for $Repository."
    }

    $assetMetadata = $release.assets | Where-Object { $_.name -eq $Asset } | Select-Object -First 1
    $checksumMetadata = $release.assets | Where-Object { $_.name -eq 'SHA256SUMS' } | Select-Object -First 1
    if (-not $assetMetadata -or -not $checksumMetadata) {
        throw "Release $Repository@$tag does not contain $Asset and SHA256SUMS."
    }

    $assetPath = Join-Path $Destination $Executable
    $checksumPath = Join-Path $Destination "$Executable.SHA256SUMS"
    Invoke-WebRequest -Uri $assetMetadata.browser_download_url -OutFile $assetPath
    Invoke-WebRequest -Uri $checksumMetadata.browser_download_url -OutFile $checksumPath

    $escapedAsset = [Regex]::Escape($Asset)
    $checksumLine = Get-Content $checksumPath | Where-Object { $_ -match "^([a-fA-F0-9]{64})\s+\*?$escapedAsset$" } | Select-Object -First 1
    if (-not $checksumLine) {
        Remove-Item $assetPath -Force -ErrorAction SilentlyContinue
        throw "SHA256SUMS for $Repository@$tag does not contain a strict checksum for $Asset."
    }

    $expected = ([Regex]::Match($checksumLine, '^([a-fA-F0-9]{64})')).Groups[1].Value.ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 $assetPath).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        Remove-Item $assetPath -Force -ErrorAction SilentlyContinue
        throw "Checksum mismatch for $Repository@$tag/$Asset."
    }

    Write-Host "Installed and verified $Repository@$tag/$Asset."
}

if (-not $Destination) {
    $temporaryRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
    $Destination = Join-Path $temporaryRoot 'fylo-vendor-bin'
}

New-Item -ItemType Directory -Force -Path $Destination | Out-Null
Install-VerifiedGitHubBinary -Repository 'd31ma/TTID' -Asset 'ttid-windows-x64.exe' -Executable 'ttid.exe'
Install-VerifiedGitHubBinary -Repository 'd31ma/CHEX' -Asset 'chex-windows-x64.exe' -Executable 'chex.exe'

$env:Path = "$Destination;$env:Path"
if ($env:GITHUB_PATH) {
    Add-Content -Path $env:GITHUB_PATH -Value $Destination
}
& (Join-Path $Destination 'ttid.exe') --help | Out-Null
& (Join-Path $Destination 'chex.exe') --help | Out-Null
