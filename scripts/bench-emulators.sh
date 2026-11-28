#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

echo "==> Building release bq-lite-server and bench_emulators"
cargo build --release -p bq-lite-server --bin bq-lite-server --example bench_emulators

exec target/release/examples/bench_emulators "$@"
