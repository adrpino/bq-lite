use chrono::{Datelike, Timelike};

use crate::ast::{ArrayAccessKind, Expr, InTarget};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext};
use crate::types::BqValue;

use super::subquery_cache::eval_in_subquery;
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
    if let InTarget::Subquery(q) = target {
        return eval_in_subquery(&lhs, negated, q, row, ctx);
    }
    let lhs_temporal = match &lhs {
        BqValue::Date(_) => Some(crate::types::BqType::Date),
        BqValue::Datetime(_) => Some(crate::types::BqType::Datetime),
        BqValue::Time(_) => Some(crate::types::BqType::Time),
        BqValue::Timestamp(_) => Some(crate::types::BqType::Timestamp),
        _ => None,
    };
    let candidates = match target {
        InTarget::List(exprs) => exprs
            .iter()
            .map(|e| {
                let v = eval_expr(e, row, ctx)?;
                if let Some(ref tty) = lhs_temporal {
                    if matches!(e, Expr::Literal(crate::ast::Literal::String(_))) && !v.is_null() {
                        return crate::types::coercion::cast_value(v, tty);
                    }
                }
                Ok(v)
            })
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
        InTarget::Subquery(_) => Vec::new(),
    };
    eval_in_values(&lhs, negated, &candidates)
}

pub(super) fn eval_in_values(
    lhs: &BqValue,
    negated: bool,
    candidates: &[BqValue],
) -> BqResult<BqValue> {
    if lhs.is_null() {
        return Ok(if candidates.is_empty() {
            BqValue::Bool(negated)
        } else {
            BqValue::Null
        });
    }
    let mut saw_null = false;
    for cand in candidates {
        match ops::sql_values_equal(lhs, cand)? {
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
    eval_subscript_values(base, kind, idx_val)
}

pub(super) fn eval_subscript_values(
    base: BqValue,
    kind: ArrayAccessKind,
    idx_val: BqValue,
) -> BqResult<BqValue> {
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
    let BqValue::Int64(raw_idx) = idx_val else {
        return Err(BqError::query("Subscript index must be INT64"));
    };
    let zero_based = match kind {
        ArrayAccessKind::Raw | ArrayAccessKind::Offset | ArrayAccessKind::SafeOffset => raw_idx,
        ArrayAccessKind::Ordinal | ArrayAccessKind::SafeOrdinal => raw_idx.saturating_sub(1),
    };
    let safe = matches!(
        kind,
        ArrayAccessKind::SafeOffset | ArrayAccessKind::SafeOrdinal
    );
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "Only ARRAY and STRUCT support integer offset/ordinal subscripts"
    )]
    match base {
        BqValue::Array(items) => {
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
        BqValue::Struct(fields) => {
            if zero_based < 0 {
                return if safe {
                    Ok(BqValue::Null)
                } else {
                    Err(BqError::semantic(format!(
                        "Struct field index {raw_idx} is out of bounds"
                    )))
                };
            }
            let idx = usize::try_from(zero_based).ok();
            match idx.and_then(|i| fields.get(i).map(|f| f.value.clone())) {
                Some(v) => Ok(v),
                None if safe => Ok(BqValue::Null),
                None => Err(BqError::semantic(format!(
                    "Struct field index {raw_idx} is out of bounds for struct of length {}",
                    fields.len()
                ))),
            }
        }
        _ => Err(BqError::query("Subscript requires ARRAY or STRUCT value")),
    }
}

pub(super) fn eval_extract(
    part: &str,
    expr: &Expr,
    time_zone: Option<&Expr>,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let val = eval_expr(expr, row, ctx)?;
    let tz_val = time_zone
        .map(|tz_e| eval_expr(tz_e, row, ctx))
        .transpose()?;
    eval_extract_value(part, val, tz_val.as_ref())
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "EXTRACT supports Date, Datetime, Time, and Timestamp variants"
)]
pub(super) fn eval_extract_value(
    part: &str,
    val: BqValue,
    tz_val: Option<&BqValue>,
) -> BqResult<BqValue> {
    if val.is_null() || tz_val.is_some_and(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    let upper = part.to_ascii_uppercase();
    match val {
        BqValue::Date(d) => extract_date_part(&upper, part, d),
        BqValue::Datetime(dt) => extract_datetime_part(&upper, part, dt, false),
        BqValue::Time(t) => extract_time_part(&upper, part, t),
        BqValue::Timestamp(ts) => {
            let local_dt = match tz_val {
                None => ts.naive_utc(),
                Some(BqValue::String(tz_s)) => {
                    crate::executor::scalar::timestamp_to_tz_naive(ts, tz_s)?
                }
                Some(_) => {
                    return Err(BqError::query(
                        "EXTRACT AT TIME ZONE expects STRING timezone",
                    ))
                }
            };
            extract_datetime_part(&upper, part, local_dt, true)
        }
        BqValue::Interval(iv) => crate::executor::scalar::extract_interval_part(&upper, part, iv),
        other => Err(BqError::query(format!(
            "EXTRACT not supported for {}",
            other.infer_type()?
        ))),
    }
}

fn extract_date_part(upper: &str, raw_part: &str, d: chrono::NaiveDate) -> BqResult<BqValue> {
    match upper {
        "YEAR" => Ok(BqValue::Int64(i64::from(d.year()))),
        "ISOYEAR" => Ok(BqValue::Int64(i64::from(d.iso_week().year()))),
        "QUARTER" => Ok(BqValue::Int64(i64::from((d.month() - 1) / 3 + 1))),
        "MONTH" => Ok(BqValue::Int64(i64::from(d.month()))),
        "WEEK" => {
            let dow_sun = d.weekday().num_days_from_sunday();
            Ok(BqValue::Int64(i64::from((d.ordinal0() + 7 - dow_sun) / 7)))
        }
        "ISOWEEK" => Ok(BqValue::Int64(i64::from(d.iso_week().week()))),
        "DAY" => Ok(BqValue::Int64(i64::from(d.day()))),
        "DAYOFWEEK" => Ok(BqValue::Int64(
            i64::from(d.weekday().num_days_from_sunday()) + 1,
        )),
        "DAYOFYEAR" => Ok(BqValue::Int64(i64::from(d.ordinal()))),
        _ => Err(BqError::query(format!(
            "Unsupported EXTRACT part '{raw_part}' for DATE"
        ))),
    }
}

fn extract_time_part(upper: &str, raw_part: &str, t: chrono::NaiveTime) -> BqResult<BqValue> {
    match upper {
        "HOUR" => Ok(BqValue::Int64(i64::from(t.hour()))),
        "MINUTE" => Ok(BqValue::Int64(i64::from(t.minute()))),
        "SECOND" => Ok(BqValue::Int64(i64::from(t.second()))),
        "MILLISECOND" => Ok(BqValue::Int64(i64::from(t.nanosecond() / 1_000_000))),
        "MICROSECOND" => Ok(BqValue::Int64(i64::from(t.nanosecond() / 1_000))),
        _ => Err(BqError::query(format!(
            "Unsupported EXTRACT part '{raw_part}' for TIME"
        ))),
    }
}

fn extract_datetime_part(
    upper: &str,
    raw_part: &str,
    dt: chrono::NaiveDateTime,
    is_timestamp: bool,
) -> BqResult<BqValue> {
    match upper {
        "DATE" => Ok(BqValue::Date(dt.date())),
        "TIME" => Ok(BqValue::Time(dt.time())),
        "DATETIME" if is_timestamp => Ok(BqValue::Datetime(dt)),
        "YEAR" | "ISOYEAR" | "QUARTER" | "MONTH" | "WEEK" | "ISOWEEK" | "DAY" | "DAYOFWEEK"
        | "DAYOFYEAR" => extract_date_part(upper, raw_part, dt.date()),
        "HOUR" | "MINUTE" | "SECOND" | "MILLISECOND" | "MICROSECOND" => {
            extract_time_part(upper, raw_part, dt.time())
        }
        _ => Err(BqError::query(format!(
            "Unsupported EXTRACT part '{raw_part}' for {}",
            if is_timestamp {
                "TIMESTAMP"
            } else {
                "DATETIME"
            }
        ))),
    }
}

pub(super) fn eval_with_expr(
    bindings: &[(crate::ast::Ident, Expr)],
    body: &Expr,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let mut scoped = row.clone();
    for (name, expr) in bindings {
        let val = eval_expr(expr, &scoped, ctx)?;
        let ty = val.infer_type()?;
        let mut kept_cols = Vec::with_capacity(scoped.col_len() + 1);
        for b in scoped.iter_cols() {
            if !b.name.eq_ignore_ascii_case(&name.value) {
                kept_cols.push(b.to_owned_binding());
            }
        }
        kept_cols.push(crate::executor::context::RowBinding {
            qualifier: None,
            name: std::sync::Arc::from(name.value.as_str()),
            ty,
            value: val,
        });
        let pseudo: Vec<_> = scoped
            .iter_pseudo()
            .map(crate::executor::row::BindingRef::to_owned_binding)
            .collect();
        scoped = EvalRow::with_pseudo(kept_cols, pseudo);
    }
    eval_expr(body, &scoped, ctx)
}

pub(super) fn eval_case_expr(
    operand: Option<&Expr>,
    whens: &[crate::ast::WhenBranch],
    else_result: Option<&Expr>,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let op_val = operand.map(|e| eval_expr(e, row, ctx)).transpose()?;
    let mut chosen: Option<BqValue> = None;
    for branch in whens {
        let cond = eval_expr(&branch.condition, row, ctx)?;
        let matched = match &op_val {
            Some(ov) => !ov.is_null() && !cond.is_null() && super::values_equal(ov, &cond)?,
            None => matches!(cond, BqValue::Bool(true)),
        };
        if matched {
            chosen = Some(eval_expr(&branch.result, row, ctx)?);
            break;
        }
    }
    let val = match chosen {
        Some(v) => v,
        None => else_result.map_or(Ok(BqValue::Null), |e| eval_expr(e, row, ctx))?,
    };
    if val.is_null() || all_case_branches_match_literal(&val, whens, else_result) {
        return Ok(val);
    }
    let mut result_exprs: Vec<&Expr> =
        Vec::with_capacity(whens.len() + usize::from(else_result.is_some()));
    for w in whens {
        result_exprs.push(&w.result);
    }
    if let Some(e) = else_result {
        result_exprs.push(e);
    }
    coerce_to_expr_supertype(val, &result_exprs, row, ctx)
}

const fn expr_literal_matches_value(val: &BqValue, expr: &Expr) -> bool {
    let Expr::Literal(lit) = expr else {
        return false;
    };
    matches!(
        (val, lit),
        (_, crate::ast::Literal::Null)
            | (BqValue::String(_), crate::ast::Literal::String(_))
            | (BqValue::Int64(_), crate::ast::Literal::Int(_))
            | (BqValue::Float64(_), crate::ast::Literal::Float(_))
            | (BqValue::Bool(_), crate::ast::Literal::Bool(_))
            | (BqValue::Bytes(_), crate::ast::Literal::Bytes(_))
    )
}

fn all_case_branches_match_literal(
    val: &BqValue,
    whens: &[crate::ast::WhenBranch],
    else_result: Option<&Expr>,
) -> bool {
    whens
        .iter()
        .all(|w| expr_literal_matches_value(val, &w.result))
        && else_result.is_none_or(|e| expr_literal_matches_value(val, e))
}

pub(in crate::executor) fn coerce_to_expr_supertype(
    val: BqValue,
    exprs: &[&Expr],
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    if val.is_null() || exprs.iter().all(|e| expr_literal_matches_value(&val, e)) {
        return Ok(val);
    }
    let tys: Vec<crate::types::BqType> = exprs
        .iter()
        .map(|e| crate::executor::query::infer_expr_type_public(e, row, ctx))
        .collect();
    let Ok(super_ty) = crate::types::coercion::common_supertype(&tys) else {
        return Ok(val);
    };
    let val_ty = val.infer_type()?;
    if !matches!(super_ty, crate::types::BqType::UntypedNull)
        && val_ty != super_ty
        && crate::types::coercion::can_implicitly_coerce(&val_ty, &super_ty)
    {
        return crate::types::coercion::coerce_value(val, &super_ty);
    }
    Ok(val)
}
