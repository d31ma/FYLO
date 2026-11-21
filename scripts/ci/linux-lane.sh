#!/usr/bin/env bash
# shellcheck shell=bash
#
# FYLO's Linux lane on the build Mac (ADR 0033 section 12): runs a command in
# a Linux container in Colima's VM, with FYLO's test disk mounted at /data.
#
#   MAC_HEAVY_COORDINATOR=fylo mac-heavy --owner <name> --vm=colima -- \
#     scripts/ci/linux-lane.sh [--image <image>] [--owner <name>] -- <command>
#
# The test disk is the named Docker volume fylo-test-volume. Docker keeps its
# volumes on Colima's data disk (/mnt/lima-colima), which is a file on the
# HANGAR SSD and not the VM's boot disk, so it passes the drive rule. No image
# file, no loop device and no --privileged are needed.
#
# The command runs in a copy of this worktree's files (tracked and untracked,
# not ignored) at /build/src, with CARGO_TARGET_DIR=/build/target on the
# volume fylo-linux-lane-build, which is kept between runs (TMPDIR is
# /build/tmp there too, an ext4 folder, never the container's overlay /tmp).
# Core dumps are off, because the crash tests abort children on purpose. It runs through
# scripts/ci/test-disk.sh run --mount /data, so FYLO_TEST_ROOT is a job folder
# on the test disk that is removed afterwards.
#
# It starts Colima when docker does not answer, and stops it again only if it
# started it.

set -o pipefail

fylo_lane_colima_env() {
  echo "${FYLO_COLIMA_ENV:-$HOME/.local/share/tachyon-ci/docker-linux/colima-env.zsh}"
}

fylo_lane_test_volume() {
  echo "${FYLO_LINUX_TEST_VOLUME:-fylo-test-volume}"
}

fylo_lane_build_volume() {
  echo "${FYLO_LINUX_BUILD_VOLUME:-fylo-linux-lane-build}"
}

fylo_lane_container_user() {
  echo 1001
}

fylo_lane_stage_root() {
  echo "$HOME/Library/Caches/fylo-linux-lane"
}

fylo_lane_say() {
  echo "=== linux lane $(date +%H:%M:%S): $*" >&2
}

fylo_lane_fail() {
  echo "FYLO Linux lane failed: $*" >&2
  return 1
}

fylo_lane_colima() {
  zsh -c 'source "$1" && "$2" "$3"' fylo-lane "$(fylo_lane_colima_env)" "$1" "$2"
}

fylo_lane_docker_host() {
  zsh -c 'source "$1" && echo "$DOCKER_HOST"' fylo-lane "$(fylo_lane_colima_env)"
}

fylo_lane_docker_answers() {
  docker info > /dev/null 2>&1
}

fylo_lane_start_colima() {
  local log="$1"
  fylo_lane_docker_answers && return 1
  fylo_lane_say "starting Colima (log $log)"
  fylo_lane_colima colima_start "$log" || { fylo_lane_fail "Colima did not start"; return 2; }
}

fylo_lane_stop_colima() {
  fylo_lane_say "stopping Colima"
  fylo_lane_colima colima_stop "$1"
}

fylo_lane_newest_image() {
  docker images --format '{{.Repository}}:{{.Tag}}' | grep '^tachyon-linux-ci:' | head -1
}

fylo_lane_prepare_volumes() {
  local image="$1"
  docker volume create "$(fylo_lane_test_volume)" > /dev/null || return 1
  docker volume create "$(fylo_lane_build_volume)" > /dev/null || return 1
  docker run --rm -u 0 -v "$(fylo_lane_test_volume):/data" -v "$(fylo_lane_build_volume):/build" "$image" \
    chown "$(fylo_lane_container_user)" /data /build
}

fylo_lane_pack() {
  local stage="$1"
  mkdir -p "$stage" || return 1
  git ls-files -z --cached --others --exclude-standard | tar --null -T - -cf "$stage/src.tar"
}

fylo_lane_container() {
  local image="$1" stage="$2" owner="$3"
  shift 3
  docker run --rm --init --ulimit core=0 -u "$(fylo_lane_container_user)" \
    -e "FYLO_TEST_OWNER=$owner" -e "CARGO_BUILD_JOBS=${MAC_HEAVY_CORES:-4}" \
    -e "NEXTEST_TEST_THREADS=${MAC_HEAVY_CORES:-4}" \
    -v "$(fylo_lane_test_volume):/data" -v "$(fylo_lane_build_volume):/build" -v "$stage:/stage:ro" \
    "$image" bash /stage/inner.sh "$@"
}

fylo_lane_write_inner() {
  cat > "$1/inner.sh" << 'INNER'
set -o pipefail
source /opt/tachyon/ci-env.sh 2> /dev/null
ln -sfn /opt/tachyon/rustup "$HOME/.rustup" 2> /dev/null
rm -rf /build/src && mkdir -p /build/src && tar -xf /stage/src.tar -C /build/src || exit 1
cd /build/src || exit 1
export CARGO_TARGET_DIR=/build/target TMPDIR=/build/tmp
mkdir -p "$TMPDIR"
echo "=== linux lane: /data is $(findmnt -n -o SOURCE,FSTYPE --target /data)"
exec bash scripts/ci/test-disk.sh run --mount /data -- "$@"
INNER
}

fylo_lane_run_in() {
  local stage="$1" owner="$2" image="$3"
  shift 3
  [ -n "$image" ] || image="$(fylo_lane_newest_image)"
  [ -n "$image" ] || { fylo_lane_fail "no tachyon-linux-ci image; pass --image"; return 1; }
  fylo_lane_say "image $image, test volume $(fylo_lane_test_volume), owner $owner"
  fylo_lane_prepare_volumes "$image" || { fylo_lane_fail "could not prepare the Docker volumes"; return 1; }
  fylo_lane_pack "$stage" || { fylo_lane_fail "could not pack the worktree"; return 1; }
  fylo_lane_write_inner "$stage"
  fylo_lane_container "$image" "$stage" "$owner" "$@"
}

fylo_lane_run() {
  local owner="$1" image="$2" stage started status
  shift 2
  stage="$(fylo_lane_stage_root)/$$"
  mkdir -p "$(fylo_lane_stage_root)" || return 1
  DOCKER_HOST="$(fylo_lane_docker_host)" || return 1
  export DOCKER_HOST
  fylo_lane_start_colima "$stage.colima.log"
  started=$?
  [ "$started" -ne 2 ] || return 1
  fylo_lane_run_in "$stage" "$owner" "$image" "$@"
  status=$?
  rm -rf "$stage"
  [ "$started" -ne 0 ] || fylo_lane_stop_colima "$stage.colima.log"
  fylo_lane_say "done rc=$status"
  return "$status"
}

fylo_lane_usage() {
  sed -n '7,8p' "$0" | sed 's/^# \{0,1\}//' >&2
  return 2
}

fylo_lane_main() {
  local owner="${FYLO_TEST_OWNER:-${MAC_HEAVY_OWNER:-linux-lane}}" image=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --owner) owner="$2"; shift 2 ;;
      --image) image="$2"; shift 2 ;;
      --) shift; break ;;
      *) fylo_lane_usage; return ;;
    esac
  done
  [ "$#" -gt 0 ] || { fylo_lane_usage; return; }
  fylo_lane_run "$owner" "$image" "$@"
}

fylo_lane_main "$@"
