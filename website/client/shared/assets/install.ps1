# Fylo installer for Windows.
#   irm https://fylo.del.ma/install.ps1 | iex
# Downloads the latest Windows binary from GitHub releases, verifies its
# checksum, and installs it under %LOCALAPPDATA%\Fylo (added to your user PATH).
$ErrorActionPreference = 'Stop'

$repo = 'd31ma/Fylo'
$base = if ($env:FYLO_INSTALL_BASE_URL) {
    $env:FYLO_INSTALL_BASE_URL.TrimEnd('/')
} else {
    "https://github.com/$repo/releases/latest/download"
}
$asset = 'fylo-windows-x64.exe'

$dest = Join-Path $env:LOCALAPPDATA 'Fylo'
New-Item -ItemType Directory -Force -Path $dest | Out-Null
$exe = Join-Path $dest 'fylo.exe'
$download = Join-Path $dest "fylo-$([Guid]::NewGuid().ToString('N')).download"

Write-Host "Downloading $asset..."
try {
    Invoke-WebRequest -Uri "$base/$asset" -OutFile $download
    $sums = (Invoke-WebRequest -Uri "$base/SHA256SUMS" -UseBasicParsing).Content
    $escapedAsset = [regex]::Escape($asset)
    $line = ($sums -split "`n") |
        Where-Object { $_ -match "^[0-9a-fA-F]{64}\s+\*?$escapedAsset\s*$" } |
        Select-Object -First 1
    if (-not $line) {
        throw "Checksum metadata does not contain $asset. Aborting."
    }

    $expected = ($line.Trim() -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 $download).Hash.ToLowerInvariant()
    if ($expected -ne $actual) {
        throw "Checksum mismatch for $asset. Aborting."
    }

    Move-Item -LiteralPath $download -Destination $exe -Force
} finally {
    if (Test-Path -LiteralPath $download) {
        Remove-Item -LiteralPath $download -Force
    }
}

if (-not (Test-Path -LiteralPath $exe -PathType Leaf) -or (Get-Item -LiteralPath $exe).Length -eq 0) {
    throw "Fylo executable was not installed. Aborting."
}

# Add install dir to the user PATH if it isn't already there.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$dest*") {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$dest", 'User')
    Write-Host "Added $dest to your user PATH (restart your terminal to pick it up)."
}

Write-Host "Installed fylo to $exe"
Write-Host "Run 'fylo --help' to get started."
