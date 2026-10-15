#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 IMAGE.AppImage [OUTPUT_DIR]" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ ! -f "$1" ]]; then
  echo "AppImage not found: $1" >&2
  exit 1
fi
image="$(realpath -- "$1")"
if ! command -v docker > /dev/null; then
  echo "docker is required to run the AppImage catalog gate" >&2
  exit 1
fi

output_dir="${2:-${RUNNER_TEMP:-${TMPDIR:-/tmp}}/appimage-catalog-check}"
mkdir -p "$output_dir"
output_dir="$(cd "$output_dir" && pwd)"
container="compare-all-catalog-gate-$$"
cleanup() {
  docker rm -f "$container" > /dev/null 2>&1 || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

set +e
docker run --rm --privileged --name "$container" \
  --mount "type=bind,source=$image,target=/input.AppImage,readonly" \
  --mount "type=bind,source=$script_dir/appimage-catalog-gate,target=/scripts,readonly" \
  --mount "type=bind,source=$output_dir,target=/log" \
  ubuntu:22.04 bash -c '
    set -Eeuo pipefail
    mkdir -p /root/in /root/gate
    cp /input.AppImage /root/in/compare-all.AppImage
    chmod 0755 /root/in/compare-all.AppImage
    cp /scripts/*.sh /root/gate/

    set +e
    bash /root/gate/appimage-catalog-gate.sh \
      /root/in/compare-all.AppImage --work /root/gate.work 2>&1 | tee /log/catalog-gate.log
    pipeline_status=("${PIPESTATUS[@]}")
    gate_status=${pipeline_status[0]}
    log_status=${pipeline_status[1]}
    set -e

    for name in app.log appdir-lint.log runtime-mount.log screenshot.png; do
      if [[ -f "/root/gate.work/$name" ]]; then
        cp "/root/gate.work/$name" "/log/$name"
      fi
    done
    if (( gate_status != 0 )); then
      exit "$gate_status"
    fi
    exit "$log_status"
  ' 2>&1 | tee "$output_dir/container.log"
pipeline_status=("${PIPESTATUS[@]}")
status=${pipeline_status[0]}
log_status=${pipeline_status[1]}
set -e

if (( status == 0 )); then
  status=$log_status
fi

if (( status != 0 )); then
  echo "AppImage catalog gate failed (status $status); inspect $output_dir/catalog-gate.log and the uploaded diagnostics." >&2
fi
exit "$status"
