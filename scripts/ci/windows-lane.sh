#!/usr/bin/env bash
# shellcheck shell=bash
#
# FYLO's Windows lane on the build Mac (ADR 0033 section 12): runs a
# PowerShell script in windows-vm, with FYLO's NTFS test disk as drive T:.
#
#   MAC_HEAVY_COORDINATOR=fylo mac-heavy --owner <name> --vm=windows -- \
#     scripts/ci/windows-lane.sh [--owner <name>] -- <script.ps1> [<argument>...]
#
# The test disk is one reusable sparse raw image on the HANGAR SSD
# (FYLO_WINDOWS_TEST_DISK, default below), made by Platform. Only testvm
# attaches it, as the VM's second NVMe disk with serial TEST-DISK for this
# boot (TESTVM_TEST_DISK), so the vm-windows lock makes it one boot at a time.
# The Mac never attaches it. Windows formats it NTFS on first use only, when
# the disk has no partitions (scripts/ci/test-disk.ps1 -Action Online).
#
# The script runs in a copy of this worktree's files (tracked and untracked,
# not ignored) through scripts/ci/test-disk.ps1 -Action Run, so
# FYLO_TEST_ROOT is a job folder on T: that is removed afterwards. Cargo's
# output stays on C:. The lane boots the VM in testvm's ci profile and stops
# it at the end, so the next boot is without the test disk.

set -o pipefail

fylo_win_test_disk() {
  echo "${FYLO_WINDOWS_TEST_DISK:-/Volumes/HANGAR/fylo-test/windows-vm.raw}"
}

fylo_win_letter() {
  echo T
}

fylo_win_remote_folder() {
  echo fylo-lane
}

fylo_win_stage_root() {
  echo "$HOME/Library/Caches/fylo-windows-lane"
}

fylo_win_say() {
  echo "=== windows lane $(date +%H:%M:%S): $*" >&2
}

fylo_win_fail() {
  echo "FYLO Windows lane failed: $*" >&2
  return 1
}

fylo_win_place_problem() {
  case "$1" in
    /Volumes/HAVEN/* | /Volumes/ATLAS/*) echo "$1 is on a production volume (HAVEN or ATLAS)" ;;
    /Volumes/HANGAR/fylo-test/*.raw) echo none ;;
    *) echo "$1 is not a .raw file in /Volumes/HANGAR/fylo-test" ;;
  esac
}

fylo_win_require_disk() {
  local disk="$1" problem
  problem="$(fylo_win_place_problem "$disk")"
  [ "$problem" = none ] || { fylo_win_fail "$problem"; return 1; }
  [ -f "$disk" ] && return 0
  fylo_win_fail "$disk does not exist (Platform makes it)"
}

fylo_win_pack() {
  local stage="$1"
  mkdir -p "$stage" || return 1
  git ls-files -z --cached --others --exclude-standard | tar --null -T - -cf "$stage/src.tar"
}

fylo_win_write_inner() {
  cat > "$1/inner.ps1" << 'INNER'
param([string]$Owner, [string]$Script, [string[]]$Arguments = @())
$ErrorActionPreference = 'Stop'
$lane = Join-Path $env:USERPROFILE 'fylo-lane'
if (Test-Path "$env:USERPROFILE\tachyon-ci\ci-env.ps1") { . "$env:USERPROFILE\tachyon-ci\ci-env.ps1" }
$source = Join-Path $lane 'src'
Remove-Item -Recurse -Force $source -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $source | Out-Null
tar -xf (Join-Path $lane 'src.tar') -C $source
if ($LASTEXITCODE -ne 0) { throw 'could not unpack the worktree' }
Set-Location $source
$env:CARGO_TARGET_DIR = Join-Path $env:USERPROFILE 'tachyon-ci\fylo-lane-target'
$disk = Join-Path $source 'scripts\ci\test-disk.ps1'
& $disk -Action Online -Letter T
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& $disk -Action Run -Letter T -Owner $Owner -Script (Join-Path $source $Script) -Arguments $Arguments
exit $LASTEXITCODE
INNER
}

fylo_win_retry() {
  local status
  "$@"
  status=$?
  [ "$status" -eq 255 ] || return "$status"
  fylo_win_say "rc=255 (sshd), one retry in 30 s"
  sleep 30
  "$@"
}

fylo_win_copy() {
  local stage="$1" remote
  remote="$(fylo_win_remote_folder)"
  fylo_win_retry ssh windows-vm "New-Item -ItemType Directory -Force $remote | Out-Null" || return 1
  fylo_win_retry scp -q "$stage/src.tar" "$stage/inner.ps1" "$stage/run.ps1" "windows-vm:$remote/"
}

fylo_win_quote() {
  printf "'%s'" "$(printf '%s' "$1" | sed "s/'/''/g")"
}

fylo_win_array() {
  local joined="" argument
  for argument in "$@"; do joined="$joined,$(fylo_win_quote "$argument")"; done
  echo "@(${joined#,})"
}

fylo_win_write_run() {
  local stage="$1" owner="$2" script="$3"
  shift 3
  {
    echo "& (Join-Path \$PSScriptRoot 'inner.ps1') -Owner $(fylo_win_quote "$owner") -Script $(fylo_win_quote "$script") -Arguments $(fylo_win_array "$@")"
    echo "exit \$LASTEXITCODE"
  } > "$stage/run.ps1"
}

fylo_win_remote_command() {
  echo "powershell -NoProfile -ExecutionPolicy Bypass -File $(fylo_win_remote_folder)/run.ps1"
}

fylo_win_boot() {
  local disk="$1"
  testvm profile ci windows || return 1
  fylo_win_say "booting windows-vm with test disk $disk"
  TESTVM_TEST_DISK="$disk" testvm start windows
}

fylo_win_shutdown() {
  fylo_win_say "stopping windows-vm"
  testvm stop windows
  testvm profile idle windows
}

fylo_win_run_booted() {
  local stage="$1"
  fylo_win_copy "$stage" || { fylo_win_fail "could not copy the worktree to windows-vm"; return 1; }
  fylo_win_retry ssh windows-vm "$(fylo_win_remote_command)"
}

fylo_win_run() {
  local owner="$1" disk stage status
  shift
  disk="$(fylo_win_test_disk)"
  stage="$(fylo_win_stage_root)/$$"
  fylo_win_require_disk "$disk" || return 1
  fylo_win_pack "$stage" || { fylo_win_fail "could not pack the worktree"; return 1; }
  fylo_win_write_inner "$stage"
  fylo_win_write_run "$stage" "$owner" "$@"
  if fylo_win_boot "$disk"; then
    fylo_win_run_booted "$stage"
    status=$?
  else
    status=1
    fylo_win_fail "windows-vm did not start with the test disk"
  fi
  fylo_win_shutdown
  rm -rf "$stage"
  fylo_win_say "done rc=$status"
  return "$status"
}

fylo_win_usage() {
  sed -n '7,8p' "$0" | sed 's/^# \{0,1\}//' >&2
  return 2
}

fylo_win_main() {
  local owner="${FYLO_TEST_OWNER:-${MAC_HEAVY_OWNER:-windows-lane}}"
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --owner) owner="$2"; shift 2 ;;
      --) shift; break ;;
      *) fylo_win_usage; return ;;
    esac
  done
  [ "$#" -gt 0 ] || { fylo_win_usage; return; }
  fylo_win_run "$owner" "$@"
}

fylo_win_main "$@"
