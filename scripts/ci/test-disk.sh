#!/usr/bin/env bash
# shellcheck shell=bash
#
# FYLO test disks (ADR 0033 section 12) for Linux and macOS.
#
#   scripts/ci/test-disk.sh make --folder <dir> --mount <dir> [--size-gib <n>]
#   scripts/ci/test-disk.sh run --mount <dir> [--owner <name>] -- <command> [<argument>...]
#   scripts/ci/test-disk.sh detach --mount <dir> [--folder <dir> --delete]
#   scripts/ci/test-disk.sh status --mount <dir>
#
# make: creates a small sparse disk image in <folder> (ext4 on Linux, APFS on
# macOS) and attaches it at <mount>. The folder must be on a disk that is not
# the boot disk (GitHub Linux runners: /mnt). Used on laptops and GitHub
# runners. The lanes on the build Mac never use it: the macOS lane uses
# scripts/ci/test-image.sh, windows-vm gets its disk from testvm, and the
# Linux lane mounts a Docker volume (scripts/ci/linux-lane.sh).
#
# run: runs one job on a mounted test disk. It checks that <mount> is a mount
# point other than /, writes and deletes a probe file, needs 1 GiB free,
# removes job folders older than 24 hours whose process is gone, then runs the
# command with FYLO_TEST_ROOT=<mount>/roots/<owner>-<host>-<pid> and removes
# that folder on every exit path.
#
# On failure it prints "FYLO test disk not ready: <reason>" and exits 1. It
# never falls back to another folder.

set -o pipefail

fylo_disk_image_name() {
  echo "fylo-test-disk"
}

fylo_disk_volume_name() {
  echo "FYLOTEST"
}

fylo_disk_default_size_gib() {
  echo 8
}

fylo_disk_check_seconds() {
  echo 10
}

fylo_disk_minimum_free_kib() {
  echo $((1024 * 1024))
}

fylo_disk_stale_minutes() {
  echo $((24 * 60))
}

fylo_disk_say() {
  echo "=== fylo test disk $(date +%H:%M:%S): $*" >&2
}

fylo_disk_fail() {
  echo "FYLO test disk not ready: $*" >&2
  return 1
}

fylo_disk_system() {
  uname -s
}

fylo_disk_wait_for() {
  local pid="$1" limit="$2" tenths=0
  while kill -0 "$pid" 2> /dev/null; do
    if [ "$tenths" -ge "$((limit * 10))" ]; then return 1; fi
    sleep 0.1
    tenths=$((tenths + 1))
  done
}

fylo_disk_within() {
  local limit="$1" pid
  shift
  "$@" < /dev/null &
  pid=$!
  if fylo_disk_wait_for "$pid" "$limit"; then wait "$pid"; return; fi
  kill "$pid" 2> /dev/null
  return 124
}

fylo_disk_real_path() {
  (cd "$1" 2> /dev/null && pwd -P)
}

fylo_disk_production_problem() {
  case "$1/" in
    /Volumes/HAVEN/* | /Volumes/ATLAS/*) echo "$1 is on a production volume (HAVEN or ATLAS)" ;;
    /Volumes/HANGAR/fylo-test/* | /Volumes/HANGAR/test-disks/*) echo "$1 holds the build Mac's lane images; each lane attaches only its own image" ;;
    *) echo none ;;
  esac
}

fylo_disk_require_allowed_place() {
  local problem
  problem="$(fylo_disk_production_problem "$1")"
  if [ "$problem" = none ]; then return 0; fi
  fylo_disk_fail "$problem"
}

fylo_disk_mounted_on() {
  df -P "$1" 2> /dev/null | awk 'NR == 2 { print $NF }'
}

fylo_disk_is_mount_point() {
  local real
  real="$(fylo_disk_real_path "$1")" || return 1
  [ "$real" != / ] && [ "$(fylo_disk_mounted_on "$real")" = "$real" ]
}

fylo_disk_require_mount_point() {
  fylo_disk_within "$(fylo_disk_check_seconds)" fylo_disk_is_mount_point "$1" && return 0
  fylo_disk_fail "$1 is not a mounted test disk (not a mount point, or it is /)"
}

fylo_disk_linux_whole_disk() {
  local source
  source="$(findmnt -n -o SOURCE --target "$1" 2> /dev/null)"
  source="${source%%\[*}"
  lsblk -n -d -o PKNAME "$source" 2> /dev/null | head -1
}

fylo_disk_device() {
  df -P "$1" 2> /dev/null | awk 'NR == 2 { print $1 }'
}

fylo_disk_macos_whole_disk() {
  diskutil info "$(fylo_disk_device "$1")" 2> /dev/null | awk -F': *' '/Part of Whole/ { print $2 }'
}

fylo_disk_whole_disk() {
  case "$(fylo_disk_system)" in
    Darwin) fylo_disk_macos_whole_disk "$1" ;;
    *) fylo_disk_linux_whole_disk "$1" ;;
  esac
}

fylo_disk_boot_problem() {
  local folder_disk="$1" boot_disk="$2"
  if [ -z "$folder_disk" ] || [ -z "$boot_disk" ]; then echo "could not tell which disk holds it"; return; fi
  if [ "$folder_disk" = "$boot_disk" ]; then echo "it is on the boot disk ($boot_disk)"; return; fi
  echo none
}

fylo_disk_require_data_disk() {
  local problem
  problem="$(fylo_disk_boot_problem "$(fylo_disk_whole_disk "$1")" "$(fylo_disk_whole_disk /)")"
  if [ "$problem" = none ]; then return 0; fi
  fylo_disk_fail "the image folder $1 must be on a disk that is not the boot disk, but $problem"
}

fylo_disk_write_probe() {
  local probe="$1/.fylo-write-probe-$$"
  : > "$probe" 2> "$2" && rm -f "$probe" 2>> "$2"
}

fylo_disk_probe_problem() {
  case "$1" in
    0) echo none ;;
    124) echo "stuck: the write probe did not finish within $(fylo_disk_check_seconds) s" ;;
    *) echo "not writable: $(tr '\n' ' ' < "$2")" ;;
  esac
}

fylo_disk_require_writable() {
  local errors status problem
  errors="$(mktemp)"
  fylo_disk_within "$(fylo_disk_check_seconds)" fylo_disk_write_probe "$1" "$errors"
  status=$?
  problem="$(fylo_disk_probe_problem "$status" "$errors")"
  rm -f "$errors"
  if [ "$problem" = none ]; then return 0; fi
  fylo_disk_fail "$1 is mounted but $problem"
}

fylo_disk_free_kib() {
  df -Pk "$1" | awk 'NR == 2 { print $4 }'
}

fylo_disk_require_space() {
  local free
  free="$(fylo_disk_free_kib "$1")"
  if [ "${free:-0}" -ge "$(fylo_disk_minimum_free_kib)" ]; then return 0; fi
  fylo_disk_fail "only ${free:-0} KiB free on $1 (1 GiB needed)"
}

fylo_disk_as_root() {
  if [ "$(id -u)" -eq 0 ]; then "$@"; return; fi
  sudo "$@"
}

fylo_disk_linux_image() {
  echo "$1/$(fylo_disk_image_name).ext4"
}

fylo_disk_linux_create() {
  local image="$1" size="$2"
  [ -f "$image" ] && return 0
  fylo_disk_say "creating $image (sparse ext4, $size GiB maximum)"
  truncate -s "${size}G" "$image" || return 1
  mkfs.ext4 -q -F -L "$(fylo_disk_volume_name)" "$image" >&2
}

fylo_disk_linux_attach() {
  local image="$1" mount_point="$2"
  fylo_disk_say "attaching $image at $mount_point"
  fylo_disk_as_root mkdir -p "$mount_point" || return 1
  fylo_disk_as_root mount -o loop,discard "$image" "$mount_point" || return 1
  fylo_disk_as_root chown "$(id -u):$(id -g)" "$mount_point"
}

fylo_disk_macos_image() {
  echo "$1/$(fylo_disk_image_name).sparsebundle"
}

fylo_disk_macos_has_image_tool() {
  diskutil image > /dev/null 2>&1
}

fylo_disk_macos_create_with_diskutil() {
  diskutil image create blank --format UDSB --fs APFS --volumeName "$(fylo_disk_volume_name)" --size "$(($2 * 1024 * 1024 * 1024))" "$1" >&2
}

fylo_disk_macos_create_with_hdiutil() {
  hdiutil create -size "${2}g" -type SPARSEBUNDLE -fs APFS -volname "$(fylo_disk_volume_name)" "$1" >&2
}

fylo_disk_macos_create() {
  local image="$1" size="$2"
  [ -d "$image" ] && return 0
  fylo_disk_say "creating $image (sparse APFS, $size GiB maximum)"
  if fylo_disk_macos_has_image_tool; then fylo_disk_macos_create_with_diskutil "$image" "$size"; return; fi
  fylo_disk_macos_create_with_hdiutil "$image" "$size"
}

fylo_disk_macos_attach() {
  local image="$1" mount_point="$2"
  fylo_disk_say "attaching $image at $mount_point"
  mkdir -p "$mount_point" || return 1
  if fylo_disk_macos_has_image_tool; then
    diskutil image attach --mountOptions nobrowse --mountPoint "$mount_point" "$image" >&2
    return
  fi
  hdiutil attach -nobrowse -mountpoint "$mount_point" "$image" >&2
}

fylo_disk_image_path() {
  case "$(fylo_disk_system)" in
    Darwin) fylo_disk_macos_image "$1" ;;
    *) fylo_disk_linux_image "$1" ;;
  esac
}

fylo_disk_create() {
  case "$(fylo_disk_system)" in
    Darwin) fylo_disk_macos_create "$1" "$2" ;;
    *) fylo_disk_linux_create "$1" "$2" ;;
  esac
}

fylo_disk_attach() {
  case "$(fylo_disk_system)" in
    Darwin) fylo_disk_macos_attach "$1" "$2" ;;
    *) fylo_disk_linux_attach "$1" "$2" ;;
  esac
}

fylo_disk_unmount() {
  case "$(fylo_disk_system)" in
    Darwin) diskutil eject "$1" >&2 ;;
    *) fylo_disk_as_root umount "$1" ;;
  esac
}

fylo_disk_publish() {
  echo "FYLO_TEST_DISK=$1"
  [ -n "${GITHUB_ENV:-}" ] || return 0
  echo "FYLO_TEST_DISK=$1" >> "$GITHUB_ENV"
}

fylo_disk_attach_new() {
  local folder="$1" mount_point="$2" size="$3" image
  image="$(fylo_disk_image_path "$folder")"
  fylo_disk_create "$image" "$size" || { fylo_disk_fail "could not create $image"; return 1; }
  fylo_disk_attach "$image" "$mount_point" || { fylo_disk_fail "could not attach $image at $mount_point"; return 1; }
  fylo_disk_require_mount_point "$mount_point"
}

fylo_disk_make() {
  local folder="$1" mount_point="$2" size="$3"
  fylo_disk_require_allowed_place "$folder" || return 1
  fylo_disk_require_allowed_place "$mount_point" || return 1
  mkdir -p "$folder" || { fylo_disk_fail "cannot create $folder"; return 1; }
  folder="$(fylo_disk_real_path "$folder")"
  fylo_disk_require_data_disk "$folder" || return 1
  if ! fylo_disk_is_mount_point "$mount_point"; then fylo_disk_attach_new "$folder" "$mount_point" "$size" || return 1; fi
  fylo_disk_require_writable "$mount_point" || return 1
  fylo_disk_publish "$(fylo_disk_real_path "$mount_point")"
}

fylo_disk_host() {
  local host
  host="$(hostname 2> /dev/null | cut -d. -f1 | tr -cd 'A-Za-z0-9')"
  echo "${host:-host}"
}

fylo_disk_job_host() {
  local name="${1%-*}"
  echo "${name##*-}"
}

fylo_disk_job_pid() {
  echo "${1##*-}"
}

fylo_disk_job_alive() {
  local name
  name="$(basename "$1")"
  [ "$(fylo_disk_job_host "$name")" = "$(fylo_disk_host)" ] || return 0
  kill -0 "$(fylo_disk_job_pid "$name")" 2> /dev/null
}

fylo_disk_sweep_one() {
  fylo_disk_job_alive "$1" && return 0
  fylo_disk_say "removing the leftover job folder $1"
  rm -rf "$1"
}

fylo_disk_sweep() {
  local folder
  while IFS= read -r folder; do
    fylo_disk_sweep_one "$folder"
  done < <(find "$1/roots" -mindepth 1 -maxdepth 1 -type d -mmin +"$(fylo_disk_stale_minutes)" 2> /dev/null)
}

fylo_disk_clean_owner() {
  local owner
  owner="$(printf '%s' "$1" | tr -c 'A-Za-z0-9-' '-')"
  echo "${owner:-job}"
}

fylo_disk_job_folder() {
  echo "$1/roots/$(fylo_disk_clean_owner "$2")-$(fylo_disk_host)-$$"
}

fylo_disk_remove_job() {
  rm -rf "$1"
  fylo_disk_say "removed $1"
}

fylo_disk_run_job() {
  local folder="$1" status
  shift
  mkdir -p "$folder" || { fylo_disk_fail "cannot create $folder"; return 1; }
  fylo_disk_say "FYLO_TEST_ROOT=$folder"
  trap 'fylo_disk_remove_job "$folder"; exit 130' INT TERM
  FYLO_TEST_ROOT="$folder" "$@"
  status=$?
  trap - INT TERM
  fylo_disk_remove_job "$folder"
  return "$status"
}

fylo_disk_run() {
  local mount_point="$1" owner="$2"
  shift 2
  fylo_disk_require_mount_point "$mount_point" || return 1
  mount_point="$(fylo_disk_real_path "$mount_point")"
  fylo_disk_require_allowed_place "$mount_point" || return 1
  fylo_disk_require_writable "$mount_point" || return 1
  fylo_disk_require_space "$mount_point" || return 1
  fylo_disk_sweep "$mount_point"
  fylo_disk_run_job "$(fylo_disk_job_folder "$mount_point" "$owner")" "$@"
}

fylo_disk_live_jobs() {
  local folder
  while IFS= read -r folder; do
    fylo_disk_job_alive "$folder" && echo "$folder"
  done < <(find "$1/roots" -mindepth 1 -maxdepth 1 -type d 2> /dev/null)
}

fylo_disk_delete_image() {
  local image
  [ -n "$1" ] || { fylo_disk_fail "--delete needs --folder"; return 1; }
  image="$(fylo_disk_image_path "$1")"
  fylo_disk_say "deleting $image"
  rm -rf "$image"
}

fylo_disk_detach() {
  local mount_point="$1" folder="$2" delete="$3" jobs
  fylo_disk_is_mount_point "$mount_point" || { fylo_disk_say "$mount_point is not attached"; return 0; }
  jobs="$(fylo_disk_live_jobs "$mount_point")"
  [ -z "$jobs" ] || { fylo_disk_say "jobs still use the disk, so it stays attached:"; echo "$jobs" >&2; return 1; }
  fylo_disk_say "detaching $mount_point"
  fylo_disk_unmount "$mount_point" || return 1
  [ "$delete" = yes ] || return 0
  fylo_disk_delete_image "$folder"
}

fylo_disk_status() {
  fylo_disk_is_mount_point "$1" || { echo "not attached: $1"; return 1; }
  df -Ph "$1"
  fylo_disk_live_jobs "$1"
}

fylo_disk_usage() {
  sed -n '4,9p' "$0" | sed 's/^# \{0,1\}//' >&2
  return 2
}

fylo_disk_main() {
  local command="${1:-}" folder="" mount_point="" owner="${FYLO_TEST_OWNER:-${MAC_HEAVY_OWNER:-job}}"
  local size delete=no
  size="$(fylo_disk_default_size_gib)"
  [ "$#" -gt 0 ] && shift
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --folder) folder="$2"; shift 2 ;;
      --mount) mount_point="$2"; shift 2 ;;
      --owner) owner="$2"; shift 2 ;;
      --size-gib) size="$2"; shift 2 ;;
      --delete) delete=yes; shift ;;
      --) shift; break ;;
      *) fylo_disk_usage; return ;;
    esac
  done
  [ -n "$mount_point" ] || { fylo_disk_usage; return; }
  case "$command" in
    make) [ -n "$folder" ] || { fylo_disk_usage; return; }; fylo_disk_make "$folder" "$mount_point" "$size" ;;
    run) [ "$#" -gt 0 ] || { fylo_disk_usage; return; }; fylo_disk_run "$mount_point" "$owner" "$@" ;;
    detach) fylo_disk_detach "$mount_point" "$folder" "$delete" ;;
    status) fylo_disk_status "$mount_point" ;;
    *) fylo_disk_usage ;;
  esac
}

fylo_disk_main "$@"
