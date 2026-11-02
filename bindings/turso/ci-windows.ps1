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
    $started = Get-Date
    & $bash -lc "'$(Native $fylo)/bindings/turso/build.sh' '$(Native $Work)/bindings' '$(Native $Work)/out-$target' $target"
    if ($LASTEXITCODE -ne 0) { $overall = 1 }
    Write-Output "=== bindings $target rc=$LASTEXITCODE seconds=$([int]((Get-Date) - $started).TotalSeconds)"
}

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
        & $bash -lc "'$(Native $suite)/run.sh' '$(Native $cli)' '$(Native $out)' all --method $method"
        if ($LASTEXITCODE -ne 0) { $overall = 1 }
        Write-Output "=== conformance $target $method rc=$LASTEXITCODE"
    }
    $env:PATH = $saved
    $env:GOARCH = ''
}
Step "done overall=$overall"
exit $overall
