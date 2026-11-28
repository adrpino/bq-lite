#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

BQ_LITE_IMAGE="ghcr.io/adrpino/bq-lite:latest"
VANTABOARD_IMAGE="ghcr.io/vantaboard/bigquery-emulator:latest"
GOCCY_IMAGE="ghcr.io/goccy/bigquery-emulator:0.8.1"
BUILD_LOCAL=0
KEEP_IMAGES=0

ARGS=("$@")
for ((i = 0; i < ${#ARGS[@]}; i++)); do
  case "${ARGS[$i]}" in
    --bq-lite-local) BUILD_LOCAL=1 ;;
    --keep-images) KEEP_IMAGES=1 ;;
    --bq-lite-image)
      if ((i + 1 < ${#ARGS[@]})); then
        BQ_LITE_IMAGE="${ARGS[$((i + 1))]}"
      fi
      ;;
  esac
done

cleanup() {
  if command -v docker >/dev/null 2>&1; then
    local cids
    cids="$(docker ps -a --filter "name=bq-bench-" -q 2>/dev/null || true)"
    if [[ -n "$cids" ]]; then
      # shellcheck disable=SC2086
      docker rm -f $cids >/dev/null 2>&1 || true
    fi
    if [[ "$KEEP_IMAGES" -eq 0 ]]; then
      echo "==> Cleaning up benchmark Docker images"
      for img in "$BQ_LITE_IMAGE" "$VANTABOARD_IMAGE" "$GOCCY_IMAGE"; do
        if docker image inspect "$img" >/dev/null 2>&1; then
          docker rmi -f "$img" >/dev/null 2>&1 || true
        fi
      done
    fi
  fi
}
trap cleanup EXIT INT TERM

if [[ "$BUILD_LOCAL" -eq 1 ]]; then
  echo "==> Building release bq-lite-server and bench_emulators"
  cargo build --release -p bq-lite-server --bin bq-lite-server --example bench_emulators
else
  echo "==> Building release bench_emulators"
  cargo build --release -p bq-lite-server --example bench_emulators
fi

target/release/examples/bench_emulators "$@"
