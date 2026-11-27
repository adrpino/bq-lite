use crate::ast::{Ident, InsertMode};
use crate::catalog::TableMeta;
use crate::error::{BqError, BqResult};
use crate::executor::evaluator::values_distinct_eq;
use crate::types::BqValue;

pub(super) struct PkInsertOutcome {
    /// When `Some`, replaces the table's rows (used for `REPLACE`/`UPDATE` mutations).
    pub replaced_table: Option<Vec<Vec<BqValue>>>,
    /// When `replaced_table` is `None`, appends these rows directly via chunked storage.
    pub append_rows: Vec<Vec<BqValue>>,
    /// Modified/inserted rows paired with their `THEN RETURN WITH ACTION` label.
    pub modified: Vec<(Vec<BqValue>, &'static str)>,
    pub affected: u64,
}

fn resolve_pk_indices(meta: &TableMeta) -> Vec<usize> {
    meta.primary_key
        .iter()
        .filter_map(|pk| meta.column_index(pk))
        .collect()
}

fn rows_match_pk(a: &[BqValue], b: &[BqValue], pk_indices: &[usize]) -> bool {
    pk_indices.iter().all(|&idx| {
        let va = a.get(idx).unwrap_or(&BqValue::Null);
        let vb = b.get(idx).unwrap_or(&BqValue::Null);
        values_distinct_eq(va, vb).unwrap_or(false)
    })
}

fn find_pk_match(
    rows: &[Vec<BqValue>],
    candidate: &[BqValue],
    pk_indices: &[usize],
) -> Option<usize> {
    rows.iter()
        .position(|existing| rows_match_pk(existing, candidate, pk_indices))
}

fn resolve_target_col_indices(meta: &TableMeta, target_cols: &[Ident]) -> Vec<usize> {
    if target_cols.is_empty() {
        (0..meta.columns.len()).collect()
    } else {
        target_cols
            .iter()
            .filter_map(|id| meta.column_index(&id.value))
            .collect()
    }
}

pub(super) fn apply_pk_insert(
    meta: &TableMeta,
    mode: InsertMode,
    target_cols: &[Ident],
    existing_rows: Vec<Vec<BqValue>>,
    incoming: Vec<Vec<BqValue>>,
) -> BqResult<PkInsertOutcome> {
    let pk_indices = resolve_pk_indices(meta);
    if pk_indices.is_empty() {
        if mode != InsertMode::Default {
            return Err(BqError::semantic(
                "INSERT IGNORE/REPLACE/UPDATE requires a table with a primary key",
            ));
        }
        let affected = u64::try_from(incoming.len()).unwrap_or(u64::MAX);
        let modified = incoming.iter().map(|r| (r.clone(), "INSERT")).collect();
        return Ok(PkInsertOutcome {
            replaced_table: None,
            append_rows: incoming,
            modified,
            affected,
        });
    }

    let update_cols = resolve_target_col_indices(meta, target_cols);
    let mut table_rows = existing_rows;
    let mut touched: Vec<(usize, &'static str)> = Vec::new();
    let mut mutated_existing = false;

    {
        let mut state = PkRowMut {
            table_rows: &mut table_rows,
            touched: &mut touched,
            mutated_existing: &mut mutated_existing,
        };
        for new_row in incoming {
            apply_single_pk_row(mode, &pk_indices, &update_cols, new_row, &mut state)?;
        }
    }

    let affected = u64::try_from(touched.len()).unwrap_or(u64::MAX);
    let modified: Vec<(Vec<BqValue>, &'static str)> = touched
        .iter()
        .filter_map(|&(idx, act)| table_rows.get(idx).map(|r| (r.clone(), act)))
        .collect();
    if mutated_existing {
        Ok(PkInsertOutcome {
            replaced_table: Some(table_rows),
            append_rows: Vec::new(),
            modified,
            affected,
        })
    } else {
        let append_rows: Vec<Vec<BqValue>> = modified.iter().map(|(r, _)| r.clone()).collect();
        Ok(PkInsertOutcome {
            replaced_table: None,
            append_rows,
            modified,
            affected,
        })
    }
}

struct PkRowMut<'a> {
    table_rows: &'a mut Vec<Vec<BqValue>>,
    touched: &'a mut Vec<(usize, &'static str)>,
    mutated_existing: &'a mut bool,
}

fn apply_single_pk_row(
    mode: InsertMode,
    pk_indices: &[usize],
    update_cols: &[usize],
    new_row: Vec<BqValue>,
    state: &mut PkRowMut<'_>,
) -> BqResult<()> {
    let found = find_pk_match(state.table_rows, &new_row, pk_indices);
    match (mode, found) {
        (InsertMode::Default, Some(_)) => Err(BqError::semantic(
            "Failed to insert row with duplicate primary key",
        )),
        (InsertMode::Ignore, Some(_)) => Ok(()),
        (InsertMode::Replace, Some(idx)) => {
            if let Some(slot) = state.table_rows.get_mut(idx) {
                *slot = new_row;
            }
            *state.mutated_existing = true;
            if !state.touched.iter().any(|(i, _)| *i == idx) {
                state.touched.push((idx, "REPLACE"));
            }
            Ok(())
        }
        (InsertMode::Update, Some(idx)) => {
            if let Some(slot) = state.table_rows.get_mut(idx) {
                for &cidx in update_cols {
                    if let (Some(dst), Some(src)) = (slot.get_mut(cidx), new_row.get(cidx)) {
                        *dst = src.clone();
                    }
                }
            }
            *state.mutated_existing = true;
            if !state.touched.iter().any(|(i, _)| *i == idx) {
                state.touched.push((idx, "UPDATE"));
            }
            Ok(())
        }
        (_, None) => {
            let idx = state.table_rows.len();
            state.table_rows.push(new_row);
            state.touched.push((idx, "INSERT"));
            Ok(())
        }
    }
}

pub(super) fn verify_pk_uniqueness(meta: &TableMeta, rows: &[Vec<BqValue>]) -> BqResult<()> {
    let pk_indices = resolve_pk_indices(meta);
    if pk_indices.is_empty() {
        return Ok(());
    }
    for (i, row_a) in rows.iter().enumerate() {
        for row_b in rows.get(i + 1..).unwrap_or(&[]) {
            if rows_match_pk(row_a, row_b, &pk_indices) {
                return Err(BqError::semantic(
                    "Modification resulted in duplicate primary key",
                ));
            }
        }
    }
    Ok(())
}
