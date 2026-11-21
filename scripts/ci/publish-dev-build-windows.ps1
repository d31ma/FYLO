# Builds the windows-arm64 fylo.exe for one develop commit in windows-vm.
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File publish-dev-build-windows.ps1 -Sha <40-character commit>
# Run by scripts/ci/publish-dev-build.mjs over SSH. Writes %USERPROFILE%\fylo-publish\out\fylo.exe and lane.json;
# the clone and cargo target stay in %USERPROFILE%\fylo-publish between runs.
param([Parameter(Mandatory = $true)][string] $Sha)
$ErrorActionPreference = 'Continue'
if ($Sha -notmatch '^[0-9a-f]{40}$') { throw 'pass the full 40-character commit' }
$Lane = "$env:USERPROFILE\fylo-publish"
$Repo = "$Lane\repo"
$Out = "$Lane\out"

function Invoke-Checked([string] $Command, [string[]] $Arguments) {
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Command $($Arguments -join ' ') failed ($LASTEXITCODE)" }
}

function Step([string] $Name) {
    "=== $Name $(Get-Date -Format 'HH:mm:ss')"
}

. "$env:USERPROFILE\tachyon-ci\ci-env.ps1"
$ErrorActionPreference = 'Continue'
New-Item -ItemType Directory -Force $Lane, "$Lane\tmp" | Out-Null
if (Test-Path $Out) { Remove-Item -Recurse -Force $Out }
New-Item -ItemType Directory -Force $Out | Out-Null
$env:CARGO_TARGET_DIR = "$Lane\target"
$env:CARGO_INCREMENTAL = '0'
$env:CC_aarch64_pc_windows_msvc = 'cl.exe'
$env:TMP = "$Lane\tmp"; $env:TEMP = "$Lane\tmp"
Remove-Item Env:RUSTC_WRAPPER -ErrorAction SilentlyContinue

Step 'source'
if (-not (Test-Path "$Repo\.git")) { Invoke-Checked git @('clone', 'https://github.com/d31ma/FYLO.git', $Repo) }
Invoke-Checked git @('-C', $Repo, 'fetch', '--force', '--tags', 'origin', 'develop')
Invoke-Checked git @('-C', $Repo, 'checkout', '--force', '--detach', $Sha)
Invoke-Checked git @('-C', $Repo, 'clean', '-fdx')
$head = (git -C $Repo rev-parse HEAD).Trim()
if ($head -ne $Sha) { throw "the clone is at $head, not $Sha" }
Set-Location $Repo
"bun $(bun --version) | $(rustc --version) | arch $env:PROCESSOR_ARCHITECTURE"

Step 'build'
$env:FYLO_BUILD_COMMIT = $Sha; $env:FYLO_BUILD_KIND = 'development-compiled'
Invoke-Checked bun @('./scripts/build-executable.mjs', '--outfile', "$Out\fylo.exe")

Step 'report'
$identity = (& "$Out\fylo.exe" version --output json) -join "`n"
if ($LASTEXITCODE -ne 0) { throw "fylo.exe version failed ($LASTEXITCODE)" }
$report = [ordered]@{
    sha256   = (Get-FileHash -Algorithm SHA256 "$Out\fylo.exe").Hash.ToLower()
    rustc    = (rustc --version).Trim()
    target   = ((rustc -vV | Select-String '^host: ').ToString() -replace '^host: ', '').Trim()
    identity = ($identity | ConvertFrom-Json)
}
$json = $report | ConvertTo-Json -Depth 5
[System.IO.File]::WriteAllText("$Out\lane.json", $json, (New-Object System.Text.UTF8Encoding $false))
"sha256 $($report.sha256)"
'=== done'
