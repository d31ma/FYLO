#!/usr/bin/env bash
# shellcheck shell=bash
#
# FYLO's macOS test image (ADR 0033 section 12): one sparse APFS disk image on
# the HANGAR SSD, attached once at a fixed mount point and kept attached
# between jobs. Tests create their roots under FYLO_TEST_ROOT, which `run`
# sets to <mount>/roots/<owner>-<pid> for one job and removes afterwards.
#
#   scripts/ci/test-image.sh run [--owner <name>] -- <command> [<argument>...]
#   scripts/ci/test-image.sh attach | status | detach | compact
#
# Run it inside a mac-heavy or jobq job. It never falls back to another
# folder: when the image cannot be used it prints
# "FYLO test image not attached: <reason>" and exits 1.

set -o pipefail

fylo_image_file() {
  echo "${FYLO_TEST_IMAGE:-/Volumes/HANGAR/fylo-test/fylo-macos-test.sparsebundle}"
}

fylo_image_allowed_folder() {
  echo "/Volumes/HANGAR/fylo-test"
}

fylo_image_volume() {
  echo "/Volumes/HANGAR"
}

fylo_image_mount_point() {
  echo "${FYLO_TEST_IMAGE_MOUNT:-$HOME/Developer/FyloTestImage}"
}

fylo_image_size_gib() {
  echo "${FYLO_TEST_IMAGE_SIZE_GIB:-8}"
}

fylo_image_lock() {
  echo "${FYLO_TEST_IMAGE_LOCK:-$HOME/Library/Caches/mac-locks/fylo-test-image}"
}

fylo_image_check_seconds() {
  echo 10
}

fylo_image_attach_seconds() {
  echo 120
}

fylo_image_lock_wait_seconds() {
  echo 300
}

fylo_image_busy_seconds() {
  echo 30
}

fylo_image_minimum_free_kib() {
  echo $((1024 * 1024))
}

fylo_image_stale_minutes() {
  echo $((24 * 60))
}

fylo_image_say() {
  echo "=== fylo test image $(date +%H:%M:%S): $*" >&2
}

fylo_image_fail() {
  echo "FYLO test image not attached: $*" >&2
  return 1
}

fylo_image_wait_for() {
  local pid="$1" limit="$2" tenths=0
  while kill -0 "$pid" 2> /dev/null; do
    if [ "$tenths" -ge "$((limit * 10))" ]; then return 1; fi
    sleep 0.1
    tenths=$((tenths + 1))
  done
}

fylo_image_within() {
  local limit="$1" pid
  shift
  "$@" < /dev/null &
  pid=$!
  if fylo_image_wait_for "$pid" "$limit"; then wait "$pid"; return; fi
  kill "$pid" 2> /dev/null
  return 124
}

fylo_image_real_mount_point() {
  local mount_point
  mount_point="$(fylo_image_mount_point)"
  mkdir -p "$mount_point" || return 1
  (cd "$mount_point" && pwd -P)
}

fylo_image_mounted() {
  mount | grep -qF " on $(fylo_image_real_mount_point) ("
}

fylo_image_place_problem() {
  local image="$1"
  case "$image" in
    /Volumes/HAVEN/* | /Volumes/ATLAS/*) echo "$image is on a production volume (HAVEN or ATLAS)" ;;
    "$(fylo_image_allowed_folder)"/*.sparsebundle) echo none ;;
    *) echo "$image is not a .sparsebundle in $(fylo_image_allowed_folder); the macOS lane attaches only its own image" ;;
  esac
}

fylo_image_require_place() {
  local problem
  problem="$(fylo_image_place_problem "$(fylo_image_file)")"
  if [ "$problem" = none ]; then return 0; fi
  fylo_image_fail "$problem"
}

fylo_image_volume_mounted() {
  mount | grep -qF " on $(fylo_image_volume) ("
}

fylo_image_folder_readable() {
  ls "$1" > /dev/null 2>&1
}

fylo_image_require_volume() {
  local volume
  volume="$(fylo_image_volume)"
  fylo_image_volume_mounted || { fylo_image_fail "$volume is not mounted (is the HANGAR SSD plugged in?)"; return 1; }
  fylo_image_within "$(fylo_image_check_seconds)" fylo_image_folder_readable "$volume" && return 0
  fylo_image_fail "$volume did not answer within $(fylo_image_check_seconds) s"
}

fylo_image_create() {
  local image size
  image="$(fylo_image_file)"
  size="$(fylo_image_size_gib)"
  fylo_image_say "creating $image (sparse APFS, $size GiB maximum)"
  mkdir -p "$(dirname "$image")" || return 1
  diskutil image create blank --format UDSB --fs APFS --volumeName FYLOTEST --size "$((size * 1024 * 1024 * 1024))" "$image" >&2
}

fylo_image_require_file() {
  local image
  image="$(fylo_image_file)"
  if fylo_image_within "$(fylo_image_check_seconds)" test -d "$image"; then return 0; fi
  fylo_image_create || fylo_image_fail "hdiutil create of $image failed"
}

fylo_image_attached_elsewhere() {
  local image="$1" mount_point="$2"
  hdiutil info | awk -v image="$image" -v mount_point="$mount_point" '
    /^image-path/ { current = substr($0, index($0, ":") + 2) }
    current == image && /\/dev\/disk/ && NF > 2 && $NF ~ /^\// && $NF != mount_point { found = 1 }
    END { exit found ? 0 : 1 }'
}

fylo_image_require_single_attach() {
  local image mount_point
  image="$(fylo_image_file)"
  mount_point="$(fylo_image_real_mount_point)"
  fylo_image_attached_elsewhere "$image" "$mount_point" || return 0
  fylo_image_fail "$image is already attached at another mount point (hdiutil info); it is never attached twice"
}

fylo_image_run_attach() {
  diskutil image attach --mountOptions nobrowse --mountPoint "$1" "$2" >&2
}

fylo_image_attach_now() {
  local image mount_point
  image="$(fylo_image_file)"
  mount_point="$(fylo_image_real_mount_point)"
  fylo_image_say "attaching $image at $mount_point"
  fylo_image_within "$(fylo_image_attach_seconds)" fylo_image_run_attach "$mount_point" "$image" && fylo_image_mounted && return 0
  fylo_image_fail "hdiutil attach of $image did not finish within $(fylo_image_attach_seconds) s or failed"
}

fylo_image_write_probe() {
  local probe="$1/.fylo-write-probe-$$"
  : > "$probe" 2> "$2" && rm -f "$probe" 2>> "$2"
}

fylo_image_write_problem() {
  local errors status
  errors="$(mktemp)"
  fylo_image_within "$(fylo_image_check_seconds)" fylo_image_write_probe "$(fylo_image_real_mount_point)" "$errors"
  status=$?
  fylo_image_name_problem "$status" "$errors"
  rm -f "$errors"
}

fylo_image_name_problem() {
  case "$1:$(cat "$2")" in
    0:*) echo none ;;
    124:*) echo "stuck: the write probe did not finish within $(fylo_image_check_seconds) s" ;;
    *"Input/output error"*) echo io-error ;;
    *) echo "not writable: $(tr '\n' ' ' < "$2")" ;;
  esac
}

fylo_image_list_users() {
  lsof -w +D "$1" > "$2" 2>&1
}

fylo_image_unused() {
  local users status
  users="$(mktemp)"
  fylo_image_within "$(fylo_image_busy_seconds)" fylo_image_list_users "$(fylo_image_real_mount_point)" "$users"
  status=$?
  fylo_image_show_users "$status" "$users"
  status=$?
  rm -f "$users"
  return "$status"
}

fylo_image_show_users() {
  if [ "$1" -eq 1 ] && [ ! -s "$2" ]; then return 0; fi
  fylo_image_say "the image is in use (or lsof did not answer):"
  sed 's/^/    /' "$2" >&2
  return 1
}

fylo_image_run_detach() {
  diskutil eject "$1" >&2
}

fylo_image_detach_now() {
  local mount_point
  mount_point="$(fylo_image_real_mount_point)"
  fylo_image_say "detaching $mount_point"
  fylo_image_within "$(fylo_image_attach_seconds)" fylo_image_run_detach "$mount_point"
}

fylo_image_reattach() {
  fylo_image_say "the image gives I/O errors; nothing uses it, so it is attached again once"
  fylo_image_detach_now || { fylo_image_fail "hdiutil detach after an I/O error failed"; return 1; }
  fylo_image_attach_now || return 1
  fylo_image_require_writable_once
}

fylo_image_repair_io_error() {
  fylo_image_unused || { fylo_image_fail "the image gives I/O errors but is in use, so it is not attached again"; return 1; }
  fylo_image_reattach
}

fylo_image_require_writable_once() {
  local problem
  problem="$(fylo_image_write_problem)"
  if [ "$problem" = none ]; then return 0; fi
  fylo_image_fail "$(fylo_image_real_mount_point) is mounted but $problem"
}

fylo_image_require_writable() {
  local problem
  problem="$(fylo_image_write_problem)"
  case "$problem" in
    none) return 0 ;;
    io-error) fylo_image_repair_io_error ;;
    *) fylo_image_fail "$(fylo_image_real_mount_point) is mounted but $problem" ;;
  esac
}

fylo_image_lock_owner_alive() {
  local owner
  owner="$(cat "$1/pid" 2> /dev/null)" || return 1
  kill -0 "$owner" 2> /dev/null
}

fylo_image_remove_dead_lock() {
  fylo_image_lock_owner_alive "$1" && return 0
  rm -rf "$1"
}

fylo_image_take_lock() {
  local lock="$1" waited=0
  mkdir -p "$(dirname "$lock")"
  until mkdir "$lock" 2> /dev/null; do
    if [ "$waited" -ge "$(fylo_image_lock_wait_seconds)" ]; then return 1; fi
    fylo_image_remove_dead_lock "$lock"
    sleep 1
    waited=$((waited + 1))
  done
  echo "$$" > "$lock/pid"
}

fylo_image_under_lock() {
  local lock status
  lock="$(fylo_image_lock)"
  fylo_image_take_lock "$lock" || { fylo_image_fail "another job has held $lock for over $(fylo_image_lock_wait_seconds) s"; return 1; }
  "$@"
  status=$?
  rm -rf "$lock"
  return "$status"
}

fylo_image_attach_under_lock() {
  fylo_image_require_place || return 1
  fylo_image_require_volume || return 1
  if fylo_image_mounted; then fylo_image_require_writable; return; fi
  fylo_image_require_file || return 1
  fylo_image_require_single_attach || return 1
  fylo_image_attach_now || return 1
  fylo_image_require_writable
}

fylo_image_ready() {
  fylo_image_under_lock fylo_image_attach_under_lock
}

fylo_image_free_kib() {
  df -k "$(fylo_image_real_mount_point)" | awk 'NR == 2 { print $4 }'
}

fylo_image_require_space() {
  local free
  free="$(fylo_image_free_kib)"
  if [ "${free:-0}" -ge "$(fylo_image_minimum_free_kib)" ]; then return 0; fi
  fylo_image_fail "only ${free:-0} KiB free inside the image (1 GiB needed); run: $0 compact"
}

fylo_image_roots() {
  echo "$(fylo_image_real_mount_point)/roots"
}

fylo_image_job_pid() {
  echo "${1##*-}"
}

fylo_image_job_dead() {
  local pid
  pid="$(fylo_image_job_pid "$(basename "$1")")"
  ! kill -0 "$pid" 2> /dev/null
}

fylo_image_sweep_one() {
  fylo_image_job_dead "$1" || return 0
  fylo_image_say "removing the leftover job folder $1"
  rm -rf "$1"
}

fylo_image_sweep() {
  local folder
  while IFS= read -r folder; do
    fylo_image_sweep_one "$folder"
  done < <(find "$(fylo_image_roots)" -mindepth 1 -maxdepth 1 -type d -mmin +"$(fylo_image_stale_minutes)" 2> /dev/null)
}

fylo_image_clean_owner() {
  local owner
  owner="$(printf '%s' "$1" | tr -c 'A-Za-z0-9-' '-')"
  echo "${owner:-job}"
}

fylo_image_job_folder() {
  echo "$(fylo_image_roots)/$(fylo_image_clean_owner "$1")-$$"
}

fylo_image_remove_job() {
  rm -rf "$1"
  fylo_image_say "removed $1"
}

fylo_image_run_job() {
  local folder="$1" status
  shift
  mkdir -p "$folder" || { fylo_image_fail "cannot create $folder"; return 1; }
  fylo_image_say "FYLO_TEST_ROOT=$folder"
  trap 'fylo_image_remove_job "$folder"; exit 130' INT TERM
  FYLO_TEST_ROOT="$folder" "$@"
  status=$?
  trap - INT TERM
  fylo_image_remove_job "$folder"
  return "$status"
}

fylo_image_default_owner() {
  echo "${FYLO_TEST_OWNER:-${MAC_HEAVY_OWNER:-job}}"
}

fylo_image_command_run() {
  local owner
  owner="$(fylo_image_default_owner)"
  if [ "${1:-}" = --owner ]; then owner="$2"; shift 2; fi
  [ "${1:-}" = -- ] && shift
  [ "$#" -gt 0 ] || { echo "usage: $0 run [--owner <name>] -- <command> [<argument>...]" >&2; return 2; }
  fylo_image_ready || return 1
  fylo_image_require_space || return 1
  fylo_image_sweep
  fylo_image_run_job "$(fylo_image_job_folder "$owner")" "$@"
}

fylo_image_command_attach() {
  fylo_image_ready || return 1
  fylo_image_real_mount_point
}

fylo_image_live_jobs() {
  local folder
  while IFS= read -r folder; do
    fylo_image_job_dead "$folder" || echo "$folder"
  done < <(find "$(fylo_image_roots)" -mindepth 1 -maxdepth 1 -type d 2> /dev/null)
}

fylo_image_detach_under_lock() {
  local jobs
  fylo_image_mounted || { fylo_image_say "not attached"; return 0; }
  jobs="$(fylo_image_live_jobs)"
  [ -z "$jobs" ] || { fylo_image_say "jobs still use the image, so it stays attached:"; echo "$jobs" >&2; return 1; }
  fylo_image_unused || { fylo_image_say "the image is in use, so it stays attached"; return 1; }
  fylo_image_detach_now
}

fylo_image_command_detach() {
  fylo_image_under_lock fylo_image_detach_under_lock
}

fylo_image_compact_under_lock() {
  local image
  image="$(fylo_image_file)"
  fylo_image_require_place || return 1
  fylo_image_mounted && { fylo_image_say "detach the image first; compact runs only on a detached image"; return 1; }
  fylo_image_say "compacting $image"
  hdiutil compact "$image" >&2
}

fylo_image_command_compact() {
  fylo_image_under_lock fylo_image_compact_under_lock
}

fylo_image_command_status() {
  local image mount_point
  image="$(fylo_image_file)"
  mount_point="$(fylo_image_real_mount_point)"
  echo "image: $image ($(du -sh "$image" 2> /dev/null | cut -f1) on disk)"
  fylo_image_mounted && echo "attached: $mount_point, $(fylo_image_free_kib) KiB free" || echo "attached: no"
  echo "jobs: $(fylo_image_live_jobs | wc -l | tr -d ' ')"
}

fylo_image_main() {
  local command="${1:-}"
  shift
  case "$command" in
    run) fylo_image_command_run "$@" ;;
    attach) fylo_image_command_attach ;;
    detach) fylo_image_command_detach ;;
    compact) fylo_image_command_compact ;;
    status) fylo_image_command_status ;;
    *) echo "usage: $0 run [--owner <name>] -- <command> | attach | status | detach | compact" >&2; return 2 ;;
  esac
}

fylo_image_main "$@"
