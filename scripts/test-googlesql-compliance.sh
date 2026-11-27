#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

GOOGLESQL_SHA="762ca7a16f2b555c85b6f9250c713a82bd1052d6"
CACHE_ROOT="${BQ_LITE_GOOGLESQL_CACHE:-/tmp/bq-lite-googlesql-compliance}"
TESTDATA_DIR="$CACHE_ROOT/googlesql/compliance/testdata"

CURRENT_SHA="$(git -C "$CACHE_ROOT" rev-parse HEAD 2>/dev/null || true)"
if [ ! -d "$TESTDATA_DIR" ] || [ "$CURRENT_SHA" != "$GOOGLESQL_SHA" ]; then
    echo "==> Fetching pinned GoogleSQL compliance testdata ($GOOGLESQL_SHA)"
    rm -rf "$CACHE_ROOT"
    mkdir -p "$CACHE_ROOT"
    git -C "$CACHE_ROOT" init -q
    git -C "$CACHE_ROOT" remote add origin https://github.com/google/googlesql.git
    git -C "$CACHE_ROOT" sparse-checkout init --cone
    git -C "$CACHE_ROOT" sparse-checkout set googlesql/compliance/testdata
    git -C "$CACHE_ROOT" fetch --depth=1 --filter=blob:none origin "$GOOGLESQL_SHA"
    git -C "$CACHE_ROOT" checkout -q FETCH_HEAD
fi

echo "==> Running GoogleSQL Bucket A compliance harness"
BQ_LITE_GOOGLESQL_DIR="$TESTDATA_DIR" \
    cargo test -p bq-lite-core --all-features --test googlesql_compliance -- --nocapture
