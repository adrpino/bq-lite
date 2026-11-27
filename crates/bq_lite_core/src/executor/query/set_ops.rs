use crate::ast::{SetColumnMatchMode, SetCorrespondence, SetOpKind, SetQuantifier};
use crate::error::{BqError, BqResult};
use crate::executor::evaluator::values_distinct_eq;
use crate::executor::result::{QueryColumn, QueryResult};
use crate::types::coercion::{coerce_value, common_supertype};
use crate::types::BqValue;

type AlignedSetInputs = (Vec<QueryColumn>, Vec<Vec<BqValue>>, Vec<Vec<BqValue>>);

pub(super) fn execute_set_op(
    op: SetOpKind,
    quantifier: SetQuantifier,
    correspondence: &SetCorrespondence,
    left: QueryResult,
    right: QueryResult,
) -> BqResult<QueryResult> {
    let (l_cols, l_rows, r_rows) = match correspondence {
        SetCorrespondence::Positional => align_positional(left, right)?,
        SetCorrespondence::ByName { mode, on_columns } => {
            align_by_name_mode(left, right, *mode, on_columns)?
        }
    };
    let rows = match op {
        SetOpKind::Union => eval_union(l_rows, r_rows, quantifier)?,
        SetOpKind::Intersect => eval_intersect(l_rows, r_rows, quantifier)?,
        SetOpKind::Except => eval_except(l_rows, r_rows, quantifier)?,
    };
    Ok(QueryResult::new(l_cols, rows))
}

fn align_positional(left: QueryResult, right: QueryResult) -> BqResult<AlignedSetInputs> {
    if left.columns.len() != right.columns.len() {
        return Err(BqError::query(format!(
            "Set operation column count mismatch: {} vs {}",
            left.columns.len(),
            right.columns.len()
        )));
    }
    let mut cols = Vec::with_capacity(left.columns.len());
    for (lc, rc) in left.columns.iter().zip(right.columns.iter()) {
        let ty = common_supertype(&[lc.ty.clone(), rc.ty.clone()])?;
        cols.push(QueryColumn::new(lc.name.clone(), ty));
    }
    let l_rows = coerce_rows(left.rows, &cols)?;
    let r_rows = coerce_rows(right.rows, &cols)?;
    Ok((cols, l_rows, r_rows))
}

fn resolve_target_columns(
    left: &QueryResult,
    right: &QueryResult,
    mode: SetColumnMatchMode,
    on_columns: &[String],
) -> BqResult<Vec<String>> {
    if !on_columns.is_empty() {
        return Ok(on_columns.to_vec());
    }
    match mode {
        SetColumnMatchMode::Strict => {
            if left.columns.len() != right.columns.len() {
                return Err(BqError::query(
                    "BY NAME set operation column count mismatch",
                ));
            }
            Ok(left.columns.iter().map(|c| c.name.clone()).collect())
        }
        SetColumnMatchMode::Inner => {
            let names: Vec<String> = left
                .columns
                .iter()
                .filter(|lc| right.column_index(&lc.name).is_some())
                .map(|lc| lc.name.clone())
                .collect();
            if names.is_empty() {
                return Err(BqError::query(
                    "CORRESPONDING set operation has no matching column names",
                ));
            }
            Ok(names)
        }
        SetColumnMatchMode::Left => Ok(left.columns.iter().map(|c| c.name.clone()).collect()),
        SetColumnMatchMode::Full => {
            let mut names: Vec<String> = left.columns.iter().map(|c| c.name.clone()).collect();
            for rc in &right.columns {
                if left.column_index(&rc.name).is_none() {
                    names.push(rc.name.clone());
                }
            }
            Ok(names)
        }
    }
}

fn align_by_name_mode(
    left: QueryResult,
    right: QueryResult,
    mode: SetColumnMatchMode,
    on_columns: &[String],
) -> BqResult<AlignedSetInputs> {
    let target_names = resolve_target_columns(&left, &right, mode, on_columns)?;
    let mut l_indices: Vec<Option<usize>> = Vec::with_capacity(target_names.len());
    let mut r_indices: Vec<Option<usize>> = Vec::with_capacity(target_names.len());
    let mut cols: Vec<QueryColumn> = Vec::with_capacity(target_names.len());
    for name in target_names {
        let li = left.column_index(&name);
        let ri = right.column_index(&name);
        let ty = match (li, ri) {
            (Some(l), Some(r)) => {
                let lc = left
                    .columns
                    .get(l)
                    .ok_or_else(|| BqError::internal("missing left column"))?;
                let rc = right
                    .columns
                    .get(r)
                    .ok_or_else(|| BqError::internal("missing right column"))?;
                common_supertype(&[lc.ty.clone(), rc.ty.clone()])?
            }
            (Some(l), None)
                if matches!(mode, SetColumnMatchMode::Left | SetColumnMatchMode::Full) =>
            {
                left.columns
                    .get(l)
                    .ok_or_else(|| BqError::internal("missing left column"))?
                    .ty
                    .clone()
            }
            (None, Some(r)) if matches!(mode, SetColumnMatchMode::Full) => right
                .columns
                .get(r)
                .ok_or_else(|| BqError::internal("missing right column"))?
                .ty
                .clone(),
            _ => {
                return Err(BqError::query(format!(
                    "Column '{name}' is missing in BY NAME / CORRESPONDING set operation"
                )));
            }
        };
        cols.push(QueryColumn::new(name, ty));
        l_indices.push(li);
        r_indices.push(ri);
    }
    let proj_l = project_optional_indices(left.rows, &l_indices);
    let proj_r = project_optional_indices(right.rows, &r_indices);
    let l_rows = coerce_rows(proj_l, &cols)?;
    let r_rows = coerce_rows(proj_r, &cols)?;
    Ok((cols, l_rows, r_rows))
}

fn project_optional_indices(
    rows: Vec<Vec<BqValue>>,
    indices: &[Option<usize>],
) -> Vec<Vec<BqValue>> {
    rows.into_iter()
        .map(|row| {
            indices
                .iter()
                .map(|opt| {
                    opt.and_then(|idx| row.get(idx).cloned())
                        .unwrap_or(BqValue::Null)
                })
                .collect()
        })
        .collect()
}

fn coerce_rows(rows: Vec<Vec<BqValue>>, cols: &[QueryColumn]) -> BqResult<Vec<Vec<BqValue>>> {
    rows.into_iter()
        .map(|row| {
            row.into_iter()
                .zip(cols.iter())
                .map(|(v, c)| coerce_value(v, &c.ty))
                .collect()
        })
        .collect()
}

fn eval_union(
    mut left: Vec<Vec<BqValue>>,
    right: Vec<Vec<BqValue>>,
    quantifier: SetQuantifier,
) -> BqResult<Vec<Vec<BqValue>>> {
    left.extend(right);
    if quantifier == SetQuantifier::Distinct {
        dedup_rows(left)
    } else {
        Ok(left)
    }
}

fn eval_intersect(
    left: Vec<Vec<BqValue>>,
    mut right: Vec<Vec<BqValue>>,
    quantifier: SetQuantifier,
) -> BqResult<Vec<Vec<BqValue>>> {
    let mut out = Vec::new();
    for lrow in left {
        if let Some(pos) = find_row(&right, &lrow)? {
            if quantifier == SetQuantifier::All {
                right.remove(pos);
            }
            out.push(lrow);
        }
    }
    if quantifier == SetQuantifier::Distinct {
        dedup_rows(out)
    } else {
        Ok(out)
    }
}

fn eval_except(
    left: Vec<Vec<BqValue>>,
    mut right: Vec<Vec<BqValue>>,
    quantifier: SetQuantifier,
) -> BqResult<Vec<Vec<BqValue>>> {
    let mut out = Vec::new();
    for lrow in left {
        if let Some(pos) = find_row(&right, &lrow)? {
            if quantifier == SetQuantifier::All {
                right.remove(pos);
            }
        } else {
            out.push(lrow);
        }
    }
    if quantifier == SetQuantifier::Distinct {
        dedup_rows(out)
    } else {
        Ok(out)
    }
}

pub(super) fn dedup_rows(rows: Vec<Vec<BqValue>>) -> BqResult<Vec<Vec<BqValue>>> {
    let mut out: Vec<Vec<BqValue>> = Vec::with_capacity(rows.len());
    let mut index: std::collections::HashMap<u64, Vec<usize>> =
        std::collections::HashMap::with_capacity(rows.len());
    for row in rows {
        let h = super::exec_util::hash_value_slice(&row);
        let cands = index.entry(h).or_default();
        let mut seen = false;
        for &idx in cands.iter() {
            if let Some(ex) = out.get(idx) {
                if rows_equal(ex, &row)? {
                    seen = true;
                    break;
                }
            }
        }
        if !seen {
            let new_idx = out.len();
            out.push(row);
            cands.push(new_idx);
        }
    }
    Ok(out)
}

fn find_row(haystack: &[Vec<BqValue>], needle: &[BqValue]) -> BqResult<Option<usize>> {
    for (idx, row) in haystack.iter().enumerate() {
        if rows_equal(row, needle)? {
            return Ok(Some(idx));
        }
    }
    Ok(None)
}

pub(super) fn rows_equal(a: &[BqValue], b: &[BqValue]) -> BqResult<bool> {
    if a.len() != b.len() {
        return Ok(false);
    }
    for (va, vb) in a.iter().zip(b.iter()) {
        if !values_distinct_eq(va, vb)? {
            return Ok(false);
        }
    }
    Ok(true)
}
