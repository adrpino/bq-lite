use chrono::{Datelike, Timelike};

use crate::ast::{ArrayAccessKind, Expr, InTarget, Query};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext};
use crate::executor::query::execute_query;
use crate::types::BqValue;

use super::{eval_expr, ops};

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "UNNEST target must be Null or Array"
)]
pub(super) fn eval_in_expr(
    expr: &Expr,
    negated: bool,
    target: &InTarget,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let lhs = eval_expr(expr, row, ctx)?;
    let candidates = match target {
        InTarget::List(exprs) => exprs
            .iter()
            .map(|e| eval_expr(e, row, ctx))
            .collect::<BqResult<Vec<_>>>()?,
        InTarget::Unnest(arr_expr) => match eval_expr(arr_expr, row, ctx)? {
            BqValue::Null => Vec::new(),
            BqValue::Array(items) => items,
            other => {
                return Err(BqError::query(format!(
                    "UNNEST expects ARRAY, got {}",
                    other.infer_type()?
                )))
            }
        },
        InTarget::Subquery(q) => {
            let sub_ctx = ctx.with_outer(row);
            let res = execute_query(q, &sub_ctx)?;
            if res.columns.len() != 1 {
                return Err(BqError::query("IN subquery must return exactly 1 column"));
            }
            res.rows
                .into_iter()
                .filter_map(|r| r.into_iter().next())
                .collect()
        }
    };
    if lhs.is_null() {
        return Ok(if candidates.is_empty() {
            BqValue::Bool(negated)
        } else {
            BqValue::Null
        });
    }
    let mut saw_null = false;
    for cand in candidates {
        match ops::sql_values_equal(&lhs, &cand)? {
            Some(true) => return Ok(BqValue::Bool(!negated)),
            Some(false) => {}
            None => saw_null = true,
        }
    }
    Ok(if saw_null {
        BqValue::Null
    } else {
        BqValue::Bool(negated)
    })
}

pub(super) fn eval_subscript(
    expr: &Expr,
    kind: ArrayAccessKind,
    index: &Expr,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let base = eval_expr(expr, row, ctx)?;
    let idx_val = eval_expr(index, row, ctx)?;
    if base.is_null() || idx_val.is_null() {
        return Ok(BqValue::Null);
    }
    if let BqValue::Json(j) = base {
        return match (kind, idx_val) {
            (ArrayAccessKind::Raw, BqValue::Int64(i)) if i >= 0 => {
                let ui = usize::try_from(i).unwrap_or(usize::MAX);
                Ok(j.get(ui)
                    .map_or(BqValue::Null, |v| BqValue::Json(Box::new(v.clone()))))
            }
            (ArrayAccessKind::Raw, BqValue::String(k)) => Ok(j
                .get(&k)
                .map_or(BqValue::Null, |v| BqValue::Json(Box::new(v.clone())))),
            _ => Err(BqError::query("Invalid JSON subscript")),
        };
    }
    let BqValue::Array(items) = base else {
        return Err(BqError::query("Subscript requires ARRAY value"));
    };
    let BqValue::Int64(raw_idx) = idx_val else {
        return Err(BqError::query("Array subscript index must be INT64"));
    };
    let zero_based = match kind {
        ArrayAccessKind::Raw | ArrayAccessKind::Offset | ArrayAccessKind::SafeOffset => raw_idx,
        ArrayAccessKind::Ordinal | ArrayAccessKind::SafeOrdinal => raw_idx.saturating_sub(1),
    };
    let safe = matches!(
        kind,
        ArrayAccessKind::SafeOffset | ArrayAccessKind::SafeOrdinal
    );
    if zero_based < 0 {
        return if safe {
            Ok(BqValue::Null)
        } else {
            Err(BqError::semantic(format!(
                "Array index {raw_idx} is out of bounds"
            )))
        };
    }
    let idx = usize::try_from(zero_based).ok();
    match idx.and_then(|i| items.get(i).cloned()) {
        Some(v) => Ok(v),
        None if safe => Ok(BqValue::Null),
        None => Err(BqError::semantic(format!(
            "Array index {raw_idx} is out of bounds for array of length {}",
            items.len()
        ))),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "EXTRACT supports Date and Timestamp variants"
)]
pub(super) fn eval_extract(
    part: &str,
    expr: &Expr,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let val = eval_expr(expr, row, ctx)?;
    if val.is_null() {
        return Ok(BqValue::Null);
    }
    let upper = part.to_ascii_uppercase();
    match val {
        BqValue::Date(d) => match upper.as_str() {
            "YEAR" => Ok(BqValue::Int64(i64::from(d.year()))),
            "MONTH" => Ok(BqValue::Int64(i64::from(d.month()))),
            "DAY" => Ok(BqValue::Int64(i64::from(d.day()))),
            "DAYOFWEEK" => Ok(BqValue::Int64(
                i64::from(d.weekday().num_days_from_sunday()) + 1,
            )),
            "DAYOFYEAR" => Ok(BqValue::Int64(i64::from(d.ordinal()))),
            _ => Err(BqError::query(format!(
                "Unsupported EXTRACT part '{part}' for DATE"
            ))),
        },
        BqValue::Timestamp(ts) => match upper.as_str() {
            "YEAR" => Ok(BqValue::Int64(i64::from(ts.year()))),
            "MONTH" => Ok(BqValue::Int64(i64::from(ts.month()))),
            "DAY" => Ok(BqValue::Int64(i64::from(ts.day()))),
            "HOUR" => Ok(BqValue::Int64(i64::from(ts.hour()))),
            "MINUTE" => Ok(BqValue::Int64(i64::from(ts.minute()))),
            "SECOND" => Ok(BqValue::Int64(i64::from(ts.second()))),
            _ => Err(BqError::query(format!(
                "Unsupported EXTRACT part '{part}' for TIMESTAMP"
            ))),
        },
        other => Err(BqError::query(format!(
            "EXTRACT not supported for {}",
            other.infer_type()?
        ))),
    }
}

pub(super) fn eval_scalar_subquery(
    q: &Query,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let sub_ctx = ctx.with_outer(row);
    let res = execute_query(q, &sub_ctx)?;
    if res.columns.len() != 1 {
        return Err(BqError::query(
            "Scalar subquery must produce exactly 1 column",
        ));
    }
    if res.rows.len() > 1 {
        return Err(BqError::semantic(
            "Scalar subquery produced more than one element",
        ));
    }
    Ok(res
        .rows
        .into_iter()
        .next()
        .and_then(|r| r.into_iter().next())
        .unwrap_or(BqValue::Null))
}

pub(super) fn eval_array_subquery(
    q: &Query,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let sub_ctx = ctx.with_outer(row);
    let res = execute_query(q, &sub_ctx)?;
    if res.columns.len() != 1 {
        return Err(BqError::query(
            "ARRAY(SELECT ...) subquery must produce exactly 1 column",
        ));
    }
    let items = res
        .rows
        .into_iter()
        .map(|r| r.into_iter().next().unwrap_or(BqValue::Null))
        .collect();
    Ok(BqValue::Array(items))
}
