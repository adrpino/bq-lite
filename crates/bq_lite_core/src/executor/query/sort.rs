use std::cmp::Ordering;

use crate::ast::{Expr, OrderByExpr};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext};
use crate::executor::evaluator::{compare_values, eval_expr};
use crate::types::BqValue;

pub(super) fn sort_with_order_keys<T>(
    items: &mut [(T, Vec<BqValue>)],
    order_by: &[OrderByExpr],
) -> BqResult<()> {
    let mut sort_err: Option<Box<BqError>> = None;
    items.sort_by(|(_, ka), (_, kb)| {
        if sort_err.is_some() {
            return Ordering::Equal;
        }
        match compare_key_slices(ka, kb, order_by) {
            Ok(ord) => ord,
            Err(e) => {
                sort_err = Some(e);
                Ordering::Equal
            }
        }
    });
    sort_err.map_or(Ok(()), Err)
}

fn compare_key_slices(
    ka: &[BqValue],
    kb: &[BqValue],
    order_by: &[OrderByExpr],
) -> BqResult<Ordering> {
    for (idx, ob) in order_by.iter().enumerate() {
        let va = ka.get(idx).unwrap_or(&BqValue::Null);
        let vb = kb.get(idx).unwrap_or(&BqValue::Null);
        let asc = ob.asc.unwrap_or(true);
        let nulls_first = ob.nulls_first.unwrap_or(asc);
        let ord = match (va.is_null(), vb.is_null()) {
            (true, true) => Ordering::Equal,
            (true, false) => {
                if nulls_first {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (false, true) => {
                if nulls_first {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (false, false) => {
                let a_nan = matches!(va, BqValue::Float64(f) if f.is_nan());
                let b_nan = matches!(vb, BqValue::Float64(f) if f.is_nan());
                let raw = match (a_nan, b_nan) {
                    (true, true) => Ordering::Equal,
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    (false, false) => compare_values(va, vb)?.unwrap_or(Ordering::Equal),
                };
                if asc {
                    raw
                } else {
                    raw.reverse()
                }
            }
        };
        if ord != Ordering::Equal {
            return Ok(ord);
        }
    }
    Ok(Ordering::Equal)
}

pub(super) fn apply_order_by(
    rows: &mut [EvalRow],
    order_by: &[OrderByExpr],
    ctx: &ExecContext<'_>,
) -> BqResult<()> {
    apply_order_by_with_source(rows, &[], order_by, None, ctx)
}

pub(super) fn apply_order_by_with_source(
    projected: &mut [EvalRow],
    source_rows: &[EvalRow],
    order_by: &[OrderByExpr],
    top_k: Option<usize>,
    ctx: &ExecContext<'_>,
) -> BqResult<()> {
    if projected.is_empty() {
        return Ok(());
    }
    let mut keyed: Vec<(usize, Vec<BqValue>)> = Vec::with_capacity(projected.len());
    for (idx, proj_row) in projected.iter().enumerate() {
        let mut merged_cache: Option<EvalRow> = None;
        let mut keys = Vec::with_capacity(order_by.len());
        for ob in order_by {
            let val = if let Expr::Literal(crate::ast::Literal::Int(pos)) = ob.expr {
                let col_idx = usize::try_from(pos - 1).map_err(|_| {
                    BqError::query(format!("ORDER BY position {pos} is out of range"))
                })?;
                proj_row.values.get(col_idx).cloned().ok_or_else(|| {
                    BqError::query(format!("ORDER BY position {pos} out of range"))
                })?
            } else if let Ok(v) = eval_expr(&ob.expr, proj_row, ctx) {
                v
            } else {
                let fallback = merged_cache.get_or_insert_with(|| {
                    source_rows.get(idx).map_or_else(
                        || proj_row.clone(),
                        |src| merge_proj_and_source(proj_row, src),
                    )
                });
                eval_expr(&ob.expr, fallback, ctx)?
            };
            keys.push(val);
        }
        keyed.push((idx, keys));
    }
    sort_indexed_keys(&mut keyed, order_by, top_k)?;
    let mut moved: Vec<EvalRow> = projected.iter_mut().map(std::mem::take).collect();
    for (slot, (orig_idx, _)) in projected.iter_mut().zip(keyed) {
        if let Some(row) = moved.get_mut(orig_idx) {
            *slot = std::mem::take(row);
        }
    }
    Ok(())
}

fn sort_indexed_keys(
    items: &mut Vec<(usize, Vec<BqValue>)>,
    order_by: &[OrderByExpr],
    top_k: Option<usize>,
) -> BqResult<()> {
    let mut sort_err: Option<Box<BqError>> = None;
    let mut cmp = |(ia, ka): &(usize, Vec<BqValue>), (ib, kb): &(usize, Vec<BqValue>)| {
        if sort_err.is_some() {
            return Ordering::Equal;
        }
        match compare_key_slices(ka, kb, order_by) {
            Ok(Ordering::Equal) => ia.cmp(ib),
            Ok(ord) => ord,
            Err(e) => {
                sort_err = Some(e);
                Ordering::Equal
            }
        }
    };
    if let Some(k) = top_k {
        if k == 0 {
            items.clear();
            return Ok(());
        }
        if k < items.len() {
            items.select_nth_unstable_by(k - 1, &mut cmp);
            items.truncate(k);
        }
    }
    items.sort_unstable_by(&mut cmp);
    sort_err.map_or(Ok(()), Err)
}

pub(super) fn eval_limit_offset_bounds(
    limit: Option<&Expr>,
    offset: Option<&Expr>,
    ctx: &ExecContext<'_>,
) -> BqResult<(usize, Option<usize>)> {
    let empty = EvalRow::default();
    let skip = if let Some(off_expr) = offset {
        let BqValue::Int64(n) = eval_expr(off_expr, &empty, ctx)? else {
            return Err(BqError::query("OFFSET must evaluate to INT64"));
        };
        if n < 0 {
            return Err(BqError::query("OFFSET must be non-negative"));
        }
        usize::try_from(n).unwrap_or(usize::MAX)
    } else {
        0
    };
    let take = if let Some(lim_expr) = limit {
        let BqValue::Int64(n) = eval_expr(lim_expr, &empty, ctx)? else {
            return Err(BqError::query("LIMIT must evaluate to INT64"));
        };
        if n < 0 {
            return Err(BqError::query("LIMIT must be non-negative"));
        }
        Some(usize::try_from(n).unwrap_or(usize::MAX))
    } else {
        None
    };
    Ok((skip, take))
}

pub(super) fn merge_proj_and_source(proj_row: &EvalRow, src_row: &EvalRow) -> EvalRow {
    let mut combined = src_row.clone();
    for pcol in proj_row.iter_cols() {
        if !combined
            .schema
            .cols
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(pcol.name))
        {
            combined.push_binding(pcol.to_owned_binding());
        }
    }
    combined
}

pub(super) fn apply_limit_offset(
    rows: &mut Vec<EvalRow>,
    limit: Option<&Expr>,
    offset: Option<&Expr>,
    ctx: &ExecContext<'_>,
) -> BqResult<()> {
    let (skip, take) = eval_limit_offset_bounds(limit, offset, ctx)?;
    if skip > 0 {
        rows.drain(..skip.min(rows.len()));
    }
    if let Some(lim) = take {
        rows.truncate(lim);
    }
    Ok(())
}
