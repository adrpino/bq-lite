use crate::ast::{SetCorrespondence, SetOpKind, SetQuantifier};
use crate::error::{BqError, BqResult};
use crate::executor::evaluator::values_distinct_eq;
use crate::executor::result::{QueryColumn, QueryResult};
use crate::types::coercion::{coerce_value, common_supertype};
use crate::types::BqValue;

type AlignedSetInputs = (Vec<QueryColumn>, Vec<Vec<BqValue>>, Vec<Vec<BqValue>>);

pub(super) fn execute_set_op(
    op: SetOpKind,
    quantifier: SetQuantifier,
    correspondence: SetCorrespondence,
    left: QueryResult,
    right: QueryResult,
) -> BqResult<QueryResult> {
    let (l_cols, l_rows, r_rows) = match correspondence {
        SetCorrespondence::Positional => align_positional(left, right)?,
        SetCorrespondence::ByName => align_by_name(left, right)?,
        SetCorrespondence::Corresponding => align_corresponding(left, right)?,
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

fn align_by_name(left: QueryResult, right: QueryResult) -> BqResult<AlignedSetInputs> {
    if left.columns.len() != right.columns.len() {
        return Err(BqError::query(
            "BY NAME set operation column count mismatch",
        ));
    }
    let mut r_indices = Vec::with_capacity(left.columns.len());
    let mut cols = Vec::with_capacity(left.columns.len());
    for lc in &left.columns {
        let r_idx = right.column_index(&lc.name).ok_or_else(|| {
            BqError::query(format!(
                "Column '{}' not found in right side of BY NAME",
                lc.name
            ))
        })?;
        let rc = right
            .columns
            .get(r_idx)
            .ok_or_else(|| BqError::internal("missing right column"))?;
        let ty = common_supertype(&[lc.ty.clone(), rc.ty.clone()])?;
        cols.push(QueryColumn::new(lc.name.clone(), ty));
        r_indices.push(r_idx);
    }
    let reordered_r: Vec<Vec<BqValue>> = right
        .rows
        .into_iter()
        .map(|row| {
            r_indices
                .iter()
                .map(|&idx| row.get(idx).cloned().unwrap_or(BqValue::Null))
                .collect()
        })
        .collect();
    let l_rows = coerce_rows(left.rows, &cols)?;
    let r_rows = coerce_rows(reordered_r, &cols)?;
    Ok((cols, l_rows, r_rows))
}

fn align_corresponding(left: QueryResult, right: QueryResult) -> BqResult<AlignedSetInputs> {
    let mut l_indices = Vec::new();
    let mut r_indices = Vec::new();
    let mut cols = Vec::new();
    for (li, lc) in left.columns.iter().enumerate() {
        let Some(ri) = right.column_index(&lc.name) else {
            continue;
        };
        let Some(rc) = right.columns.get(ri) else {
            continue;
        };
        let ty = common_supertype(&[lc.ty.clone(), rc.ty.clone()])?;
        cols.push(QueryColumn::new(lc.name.clone(), ty));
        l_indices.push(li);
        r_indices.push(ri);
    }
    if cols.is_empty() {
        return Err(BqError::query(
            "CORRESPONDING set operation has no matching column names",
        ));
    }
    let proj_l: Vec<Vec<BqValue>> = left
        .rows
        .into_iter()
        .map(|row| {
            l_indices
                .iter()
                .map(|&idx| row.get(idx).cloned().unwrap_or(BqValue::Null))
                .collect()
        })
        .collect();
    let proj_r: Vec<Vec<BqValue>> = right
        .rows
        .into_iter()
        .map(|row| {
            r_indices
                .iter()
                .map(|&idx| row.get(idx).cloned().unwrap_or(BqValue::Null))
                .collect()
        })
        .collect();
    let l_rows = coerce_rows(proj_l, &cols)?;
    let r_rows = coerce_rows(proj_r, &cols)?;
    Ok((cols, l_rows, r_rows))
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
