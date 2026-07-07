# Fylo installer for Windows.
#   irm https://fylo.del.ma/install.ps1 | iex
# Downloads the latest Windows binary from GitHub releases, verifies its
# checksum, and installs it under %LOCALAPPDATA%\Fylo (added to your user PATH).
$ErrorActionPreference = 'Stop'

$repo = 'd31ma/Fylo'
$base = "https://github.com/$repo/releases/latest/download"
$asset = 'fylo-windows-x64.exe'

$dest = Join-Path $env:LOCALAPPDATA 'Fylo'
New-Item -ItemType Directory -Force -Path $dest | Out-Null
$exe = Join-Path $dest 'fylo.exe'

Write-Host "Downloading $asset..."
Invoke-WebRequest -Uri "$base/$asset" -OutFile $exe

# Verify checksum (best-effort).
try {
    $sums = (Invoke-WebRequest -Uri "$base/SHA256SUMS" -UseBasicParsing).Content
    $line = ($sums -split "`n") | Where-Object { $_ -match "\s$([regex]::Escape($asset))$" } | Select-Object -First 1
    if ($line) {
        $expected = ($line -split '\s+')[0].ToLower()
        $actual = (Get-FileHash -Algorithm SHA256 $exe).Hash.ToLower()
        if ($expected -ne $actual) {
            Remove-Item $exe -Force
            throw "Checksum mismatch for $asset. Aborting."
        }
    }
} catch {
    Write-Warning "Could not verify checksum: $_"
}

# Add install dir to the user PATH if it isn't already there.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$dest*") {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$dest", 'User')
    Write-Host "Added $dest to your user PATH (restart your terminal to pick it up)."
}

Write-Host "Installed fylo to $exe"
Write-Host "Run 'fylo --help' to get started."
