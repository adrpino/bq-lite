#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

echo "==> 1. Formatting check"
cargo fmt --all -- --check

echo "==> 2. Structure & anti-pattern check"
./scripts/check-structure.sh

echo "==> 3. Clippy (all targets, all features, deny warnings)"
cargo clippy --workspace --all-targets --all-features -- -D warnings

echo "==> 4. Clippy (bq-lite-core pure in-memory / no-default-features)"
cargo clippy -p bq-lite-core --no-default-features -- -D warnings

echo "==> 5. Unused dependencies & dependency policy"
if command -v cargo-machete >/dev/null 2>&1; then
    cargo machete --with-metadata
else
    echo "  -> cargo-machete not installed, skipping"
fi
if command -v cargo-deny >/dev/null 2>&1; then
    cargo deny check bans licenses
else
    echo "  -> cargo-deny not installed, skipping"
fi

echo "==> 6. Full workspace test suite (including parity docs & proptests)"
cargo test --workspace --all-features

echo "==> 7. Pure in-memory core test suite"
cargo test -p bq-lite-core --no-default-features

echo "==> Quality gate PASSED"
