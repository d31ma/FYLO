# Build and test `fylo link` on Windows (ADR 0019): Turso's JavaScript addon
# and tursogo's SDK-kit library with FYLO inside, for each target, then the
# conformance suite against the stock SDKs. Runs inside the VM, in a clone,
# never from a network share.
#
# Usage: ci-windows.ps1 -Bundle <fylo.bundle> -Branch <branch> -Work <dir>
#        [-Targets aarch64-pc-windows-msvc,x86_64-pc-windows-msvc]
#        [-NodeX64 <node-win-x64 directory>]
# x64 artifacts are tested under Windows on ARM's x64 emulation, with an x64
# Node and GOARCH=amd64.
param(
    [Parameter(Mandatory)] [string] $Bundle,
    [Parameter(Mandatory)] [string] $Branch,
    [Parameter(Mandatory)] [string] $Work,
    [string[]] $Targets = @('aarch64-pc-windows-msvc'),
    [string] $NodeX64 = ''
)
$ErrorActionPreference = 'Continue'
# powershell -File passes "a,b" as one string.
$Targets = @($Targets | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
# The VM's toolchain PATH (Rust, MSVC, Node, Go, Git), when its CI set one up.
$ciEnv = Join-Path $env:USERPROFILE 'tachyon-ci\ci-env.ps1'
if (Test-Path $ciEnv) { . $ciEnv }
$bash = 'C:\Program Files\Git\bin\bash.exe'
$overall = 0
function Step($name) { Write-Output "=== $name $(Get-Date -Format HH:mm:ss)" }
function Native($path) { $path -replace '\\', '/' }
$saved = @{}; Get-ChildItem env: | ForEach-Object { $saved[$_.Name] = $_.Value }
function Restore-Environment {
    Get-ChildItem env: | Where-Object { -not $saved.ContainsKey($_.Name) } | ForEach-Object { Remove-Item "env:$($_.Name)" }
    foreach ($name in $saved.Keys) { Set-Item "env:$name" $saved[$name] }
}
# ci-env.ps1 set up MSVC for arm64. An x64 target needs vcvarsall's
# arm64_amd64 environment (the ARM64-hosted x64 tools), or else its x64 one,
# whose x64-hosted tools run under Windows on ARM's x64 emulation. Either
# way, name MSVC's link.exe for cargo: Git Bash puts its own /usr/bin/link
# (coreutils) first on PATH.
function Use-Msvc($target) {
    Restore-Environment
    if ($target -like 'x86_64-*') {
        $vc = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvarsall.bat'
        $arch = if (Test-Path "$env:VCToolsInstallDir\bin\HostARM64\x64\link.exe") { 'arm64_amd64' } else { 'x64' }
        Write-Output "vcvarsall $arch for $target"
        cmd /c "`"$vc`" $arch >nul && set" | ForEach-Object { if ($_ -match '^([^=]+)=(.*)$') { Set-Item -Path "env:$($matches[1])" -Value $matches[2] } }
        Remove-Item Env:Platform -ErrorAction SilentlyContinue
    }
    $link = (Get-Command link.exe -ErrorAction SilentlyContinue | Where-Object { $_.Source -like '*Microsoft Visual Studio*' } | Select-Object -First 1).Source
    Write-Output "msvc linker for ${target}: $link"
    Set-Item "env:CARGO_TARGET_$(($target -replace '-', '_').ToUpper())_LINKER" $link
}

Step 'clone'
New-Item -ItemType Directory -Force $Work | Out-Null
$fylo = Join-Path $Work 'fylo'
if (Test-Path "$fylo\.git") {
    git -C $fylo fetch -q $Bundle "+${Branch}:refs/remotes/bundle/$Branch"
    git -C $fylo checkout -q -f "refs/remotes/bundle/$Branch"
} else {
    git clone -q --branch $Branch $Bundle $fylo
}
Write-Output "fylo $(git -C $fylo rev-parse --short HEAD)"

$env:CARGO_TARGET_DIR = Join-Path $Work 'target'
foreach ($target in $Targets) {
    Step "bindings $target"
    Use-Msvc $target
    $started = Get-Date
    & $bash -lc "'$(Native $fylo)/bindings/turso/build.sh' '$(Native $Work)/bindings' '$(Native $Work)/out-$target' $target"
    if ($LASTEXITCODE -ne 0) { $overall = 1 }
    Write-Output "=== bindings $target rc=$LASTEXITCODE seconds=$([int]((Get-Date) - $started).TotalSeconds)"
}

Restore-Environment
Step 'fylo-cli'
Push-Location $fylo
$env:CARGO_TARGET_DIR = Join-Path $Work 'fylo-target'
cargo build -p fylo-cli --bin fylo-rust --locked
if ($LASTEXITCODE -ne 0) { $overall = 1 }
Pop-Location
$cli = Join-Path $Work 'fylo-target\debug\fylo-rust.exe'

Step 'link unit tests'
Push-Location $fylo
cargo test -p fylo-cli --bin fylo-rust --locked link_command
if ($LASTEXITCODE -ne 0) { $overall = 1 }
Write-Output "=== link unit tests rc=$LASTEXITCODE"
Pop-Location

foreach ($target in $Targets) {
    $out = Join-Path $Work "out-$target"
    if (-not (Test-Path "$out\fylo-link.json")) { continue }
    $saved = $env:PATH
    $env:GOARCH = ''
    if ($target -like 'x86_64-*') {
        if (-not $NodeX64) { Write-Output "=== conformance $target skipped: no x64 Node"; continue }
        $env:PATH = "$NodeX64;$env:PATH"
        $env:GOARCH = 'amd64'
    }
    Write-Output "node $(node -p process.arch) go $(go env GOARCH)"
    foreach ($method in @('auto', 'hardlink', 'copy')) {
        Step "conformance $target $method"
        $suite = Join-Path $Work "conformance-$target-$method"
        Remove-Item -Recurse -Force $suite -ErrorAction SilentlyContinue
        Copy-Item -Recurse (Join-Path $fylo 'bindings\turso\conformance') $suite
        # Java and .NET in the VM are ARM64, so x64 artifacts run JS and Go only.
        $sdks = if ($target -like 'x86_64-*') { 'js,go' } else { 'all' }
        & $bash -lc "'$(Native $suite)/run.sh' '$(Native $cli)' '$(Native $out)' $sdks --method $method"
        if ($LASTEXITCODE -ne 0) { $overall = 1 }
        Write-Output "=== conformance $target $method rc=$LASTEXITCODE"
    }
    $env:PATH = $saved
    $env:GOARCH = ''
}
Step "done overall=$overall"
exit $overall
