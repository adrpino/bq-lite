#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

FAIL=0

echo "  -> Checking source file line budgets (max 700 lines under crates/*/src/)"
while IFS= read -r -d '' file; do
    lines=$(wc -l < "$file" | tr -d ' ')
    if [ "$lines" -gt 700 ]; then
        echo "ERROR: $file has $lines lines (limit: 700)"
        FAIL=1
    elif [ "$lines" -gt 550 ]; then
        echo "WARN:  $file has $lines lines (split recommended before 700)"
    fi
done < <(find crates -type f -name '*.rs' -path '*/src/*' -print0)

search_code() {
    local pattern="$1"
    shift
    if command -v rg >/dev/null 2>&1; then
        rg -n "$pattern" "$@" || true
    else
        grep -rnE "$pattern" "$@" || true
    fi
}

echo "  -> Checking for forbidden lint suppressions in crates/*/src/"
hits=$(search_code '#!?\[allow\((clippy::all|warnings|dead_code|unused)' crates/*/src)
if [ -n "$hits" ]; then
    echo "ERROR: Forbidden #[allow(...)] attribute found:"
    echo "$hits"
    FAIL=1
fi

echo "  -> Checking for forbidden todo!/unimplemented! macros in crates/*/src/"
hits=$(search_code '\b(todo|unimplemented)!\(' crates/*/src)
if [ -n "$hits" ]; then
    echo "ERROR: Forbidden todo!/unimplemented! macro found:"
    echo "$hits"
    FAIL=1
fi

echo "  -> Checking core dependency isolation (no tokio/axum/tonic/hyper/reqwest in bq_lite_core)"
if [ -d crates/bq_lite_core/src ]; then
    hits=$(search_code '\b(tokio|axum|tonic|hyper|reqwest)::' crates/bq_lite_core/src)
    if [ -n "$hits" ]; then
        echo "ERROR: Network/async runtime crate imported in bq_lite_core:"
        echo "$hits"
        FAIL=1
    fi
fi

echo "  -> Checking internal DAG layering inside bq_lite_core"
if [ -d crates/bq_lite_core/src/types ]; then
    hits=$(search_code 'crate::(ast|parser|catalog|storage|io|executor)' crates/bq_lite_core/src/types)
    if [ -n "$hits" ]; then
        echo "ERROR: Layer violation in types/:"
        echo "$hits"
        FAIL=1
    fi
fi

if [ -d crates/bq_lite_core/src/parser ]; then
    hits=$(search_code 'crate::(catalog|storage|io|executor)' crates/bq_lite_core/src/parser)
    if [ -n "$hits" ]; then
        echo "ERROR: Layer violation in parser/:"
        echo "$hits"
        FAIL=1
    fi
fi

echo "  -> Checking for unexplained #[ignore] in tests"
hits=$(search_code '#\[ignore\]' crates)
if [ -n "$hits" ]; then
    echo "ERROR: Bare #[ignore] without reason string found (use #[ignore = \"...\"]):"
    echo "$hits"
    FAIL=1
fi

if [ "$FAIL" -ne 0 ]; then
    echo "Structure check FAILED."
    exit 1
fi
echo "  -> Structure check OK."
