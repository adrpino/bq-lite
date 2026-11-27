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
    apply_order_by_with_source(rows, &[], order_by, ctx)
}

pub(super) fn apply_order_by_with_source(
    projected: &mut [EvalRow],
    source_rows: &[EvalRow],
    order_by: &[OrderByExpr],
    ctx: &ExecContext<'_>,
) -> BqResult<()> {
    let mut keyed: Vec<(EvalRow, Vec<BqValue>)> = Vec::with_capacity(projected.len());
    for (idx, proj_row) in projected.iter().enumerate() {
        let eval_ctx_row = source_rows.get(idx).map_or_else(
            || proj_row.clone(),
            |src| merge_proj_and_source(proj_row, src),
        );
        let mut keys = Vec::with_capacity(order_by.len());
        for ob in order_by {
            let val = if let Expr::Literal(crate::ast::Literal::Int(pos)) = ob.expr {
                let col_idx = usize::try_from(pos - 1).map_err(|_| {
                    BqError::query(format!("ORDER BY position {pos} is out of range"))
                })?;
                proj_row
                    .cols
                    .get(col_idx)
                    .map(|c| c.value.clone())
                    .ok_or_else(|| {
                        BqError::query(format!("ORDER BY position {pos} out of range"))
                    })?
            } else {
                eval_expr(&ob.expr, proj_row, ctx)
                    .or_else(|_| eval_expr(&ob.expr, &eval_ctx_row, ctx))?
            };
            keys.push(val);
        }
        keyed.push((proj_row.clone(), keys));
    }
    sort_with_order_keys(&mut keyed, order_by)?;
    for (slot, (sorted_row, _)) in projected.iter_mut().zip(keyed) {
        *slot = sorted_row;
    }
    Ok(())
}

pub(super) fn merge_proj_and_source(proj_row: &EvalRow, src_row: &EvalRow) -> EvalRow {
    let mut combined = src_row.clone();
    for pcol in &proj_row.cols {
        if !combined
            .cols
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(&pcol.name))
        {
            combined.cols.push(pcol.clone());
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
    let empty = EvalRow::default();
    if let Some(off_expr) = offset {
        let BqValue::Int64(n) = eval_expr(off_expr, &empty, ctx)? else {
            return Err(BqError::query("OFFSET must evaluate to INT64"));
        };
        if n < 0 {
            return Err(BqError::query("OFFSET must be non-negative"));
        }
        let skip = usize::try_from(n).unwrap_or(usize::MAX).min(rows.len());
        rows.drain(..skip);
    }
    if let Some(lim_expr) = limit {
        let BqValue::Int64(n) = eval_expr(lim_expr, &empty, ctx)? else {
            return Err(BqError::query("LIMIT must evaluate to INT64"));
        };
        if n < 0 {
            return Err(BqError::query("LIMIT must be non-negative"));
        }
        let take = usize::try_from(n).unwrap_or(usize::MAX);
        rows.truncate(take);
    }
    Ok(())
}
