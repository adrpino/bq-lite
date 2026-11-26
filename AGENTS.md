# Agent Guidelines for bq-lite

Read `PLAN.md` and `QUALITY_AND_TESTING.md` before making changes.

## Hard Rules

1. **Read before writing:** Inspect existing module layout and `docs/compatibility-matrix.md` before adding code.
2. **Design small modules:** Keep every source file under 500 lines (split cleanly by responsibility once a file approaches 350 lines). Keep functions under 80 lines and cognitive complexity under 20.
3. **Zero shortcuts:**
   - Never weaken `[workspace.lints]` or `clippy.toml`.
   - Never use `#[allow(...)]` (use `#[expect(..., reason = "...")]` only when genuinely unavoidable).
   - Never use `.unwrap()`, `.expect()`, `panic!()`, `todo!()`, `unimplemented!()`, or unchecked slice indexing outside `#[cfg(test)]`.
   - Never import `tokio`, `axum`, `tonic`, or `reqwest` inside `crates/bq_lite_core/src/`.
   - Never add `sqlparser`, `datafusion`, `rusqlite`, or `duckdb`.
4. **Tests & Docs ship together:** Every feature must include unit tests (happy + `NULL`/empty + edge + error/`SAFE.`) and update `docs/compatibility-matrix.md`, `docs/googlesql-function-parity.md`, and `docs/error-parity.md` as applicable.
5. **Mandatory verification:** Run `./scripts/quality-gate.sh` before reporting any task complete.
6. **Version bump per commit:** Bump `[workspace.package].version` (patch/minor as appropriate) in `Cargo.toml` on every commit.

## Quality Gate

```bash
./scripts/quality-gate.sh
```
