use std::cmp::Ordering;

use crate::ast::{BinaryOp, WindowFrame, WindowFrameBound, WindowFrameUnit, WindowSpec};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext};
use crate::executor::evaluator::{compare_values, eval_binary, eval_expr, values_distinct_eq};
use crate::types::BqValue;

pub(super) fn frame_bounds(
    spec: &WindowSpec,
    pos: usize,
    part: &[usize],
    rows: &[EvalRow],
    ctx: &ExecContext<'_>,
) -> BqResult<(usize, usize)> {
    let last = part.len().saturating_sub(1);
    let Some(ref frame) = spec.frame else {
        if spec.order_by.is_empty() {
            return Ok((0, last));
        }
        let peer_end = find_peer_end(spec, pos, part, rows, ctx)?;
        return Ok((0, peer_end));
    };
    if frame.unit == WindowFrameUnit::Range {
        return range_frame_bounds(spec, frame, pos, part, rows, ctx);
    }
    let last_i = i128::try_from(last).unwrap_or(i128::MAX);
    let pos_i = i128::try_from(pos).unwrap_or(0);
    let start_i = resolve_row_bound(&frame.start, pos_i, last_i, ctx)?;
    let end_i = match &frame.end {
        Some(b) => resolve_row_bound(b, pos_i, last_i, ctx)?,
        None => pos_i,
    };
    if end_i < 0 || start_i > last_i || start_i > end_i {
        return Ok((1, 0));
    }
    let start = usize::try_from(start_i.max(0)).unwrap_or(0);
    let end = usize::try_from(end_i.min(last_i)).unwrap_or(last);
    Ok((start, end))
}

fn range_frame_bounds(
    spec: &WindowSpec,
    frame: &WindowFrame,
    pos: usize,
    part: &[usize],
    rows: &[EvalRow],
    ctx: &ExecContext<'_>,
) -> BqResult<(usize, usize)> {
    if !frame_has_numeric_offsets(frame) {
        return range_peer_bounds(spec, frame, pos, part, rows, ctx);
    }
    let Some(ob) = spec.order_by.first() else {
        return Err(BqError::semantic(
            "RANGE frame with offset requires an ORDER BY clause",
        ));
    };
    if spec.order_by.len() != 1 {
        return Err(BqError::semantic(
            "RANGE frame with offset requires exactly one ORDER BY expression",
        ));
    }
    let cur_row = rows
        .get(*part.get(pos).unwrap_or(&0))
        .ok_or_else(|| BqError::internal("row"))?;
    let cur_val = eval_expr(&ob.expr, cur_row, ctx)?;
    if cur_val.is_null() || matches!(cur_val, BqValue::Float64(f) if f.is_nan()) {
        return range_null_bounds(spec, frame, pos, part, rows, ctx);
    }
    range_offset_bounds(ob, frame, &cur_val, part, rows, ctx)
}

fn range_peer_bounds(
    spec: &WindowSpec,
    frame: &WindowFrame,
    pos: usize,
    part: &[usize],
    rows: &[EvalRow],
    ctx: &ExecContext<'_>,
) -> BqResult<(usize, usize)> {
    let last = part.len().saturating_sub(1);
    let end_bound = frame.end.as_ref().unwrap_or(&WindowFrameBound::CurrentRow);
    let start = match frame.start {
        WindowFrameBound::CurrentRow if spec.order_by.is_empty() => 0,
        WindowFrameBound::CurrentRow => find_peer_start(spec, pos, part, rows, ctx)?,
        WindowFrameBound::Following(None) => last,
        WindowFrameBound::Preceding(_) | WindowFrameBound::Following(Some(_)) => 0,
    };
    let end = match end_bound {
        WindowFrameBound::CurrentRow if spec.order_by.is_empty() => last,
        WindowFrameBound::CurrentRow => find_peer_end(spec, pos, part, rows, ctx)?,
        WindowFrameBound::Preceding(None) => 0,
        WindowFrameBound::Following(_) | WindowFrameBound::Preceding(Some(_)) => last,
    };
    Ok((start, end))
}

fn range_null_bounds(
    spec: &WindowSpec,
    frame: &WindowFrame,
    pos: usize,
    part: &[usize],
    rows: &[EvalRow],
    ctx: &ExecContext<'_>,
) -> BqResult<(usize, usize)> {
    let last = part.len().saturating_sub(1);
    let end_bound = frame.end.as_ref().unwrap_or(&WindowFrameBound::CurrentRow);
    let p_start = find_peer_start(spec, pos, part, rows, ctx)?;
    let p_end = find_peer_end(spec, pos, part, rows, ctx)?;
    let start = if matches!(frame.start, WindowFrameBound::Preceding(None)) {
        0
    } else {
        p_start
    };
    let end = if matches!(end_bound, WindowFrameBound::Following(None)) {
        last
    } else {
        p_end
    };
    Ok((start, end))
}

#[derive(Debug, Clone)]
enum RangeBoundVal {
    Unbounded,
    NegInf,
    PosInf,
    Int128(i128),
    Value(BqValue),
}

fn range_offset_bounds(
    ob: &crate::ast::OrderByExpr,
    frame: &WindowFrame,
    cur_val: &BqValue,
    part: &[usize],
    rows: &[EvalRow],
    ctx: &ExecContext<'_>,
) -> BqResult<(usize, usize)> {
    let asc = ob.asc.unwrap_or(true);
    let nulls_first = ob.nulls_first.unwrap_or(asc);
    let end_bound = frame.end.as_ref().unwrap_or(&WindowFrameBound::CurrentRow);
    let lower = eval_range_bound(&frame.start, cur_val, asc, ctx)?;
    let upper = eval_range_bound(end_bound, cur_val, asc, ctx)?;
    let include_nulls = if nulls_first {
        matches!(frame.start, WindowFrameBound::Preceding(None))
    } else {
        matches!(end_bound, WindowFrameBound::Following(None))
    };
    let mut first_idx: Option<usize> = None;
    let mut last_idx: usize = 0;
    for (idx, &row_idx) in part.iter().enumerate() {
        let row = rows.get(row_idx).ok_or_else(|| BqError::internal("row"))?;
        let key = eval_expr(&ob.expr, row, ctx)?;
        let matches = if key.is_null() {
            include_nulls
        } else if matches!(key, BqValue::Float64(f) if f.is_nan()) {
            if asc {
                matches!(frame.start, WindowFrameBound::Preceding(None))
            } else {
                matches!(end_bound, WindowFrameBound::Following(None))
            }
        } else {
            key_in_range(&key, &lower, &upper, asc)?
        };
        if matches {
            if first_idx.is_none() {
                first_idx = Some(idx);
            }
            last_idx = idx;
        }
    }
    Ok(first_idx.map_or((1, 0), |s| (s, last_idx)))
}

fn compare_key_to_bound(key: &BqValue, bound: &RangeBoundVal) -> BqResult<Option<Ordering>> {
    match (key, bound) {
        (_, RangeBoundVal::Unbounded) => Ok(None),
        (_, RangeBoundVal::NegInf) => Ok(Some(Ordering::Greater)),
        (_, RangeBoundVal::PosInf) => Ok(Some(Ordering::Less)),
        (BqValue::Int64(k), RangeBoundVal::Int128(b)) => Ok(Some(i128::from(*k).cmp(b))),
        (_, RangeBoundVal::Int128(b)) => {
            let bv = i64::try_from(*b).map_or_else(
                |_| {
                    if *b < 0 {
                        RangeBoundVal::NegInf
                    } else {
                        RangeBoundVal::PosInf
                    }
                },
                |n| RangeBoundVal::Value(BqValue::Int64(n)),
            );
            compare_key_to_bound(key, &bv)
        }
        (_, RangeBoundVal::Value(bv)) => compare_values(key, bv),
    }
}

fn key_in_range(
    key: &BqValue,
    lower: &RangeBoundVal,
    upper: &RangeBoundVal,
    asc: bool,
) -> BqResult<bool> {
    if !matches!(lower, RangeBoundVal::Unbounded) {
        let ord = compare_key_to_bound(key, lower)?;
        let ok = if asc {
            matches!(ord, Some(Ordering::Greater | Ordering::Equal))
        } else {
            matches!(ord, Some(Ordering::Less | Ordering::Equal))
        };
        if !ok {
            return Ok(false);
        }
    }
    if !matches!(upper, RangeBoundVal::Unbounded) {
        let ord = compare_key_to_bound(key, upper)?;
        let ok = if asc {
            matches!(ord, Some(Ordering::Less | Ordering::Equal))
        } else {
            matches!(ord, Some(Ordering::Greater | Ordering::Equal))
        };
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}

fn eval_range_bound(
    bound: &WindowFrameBound,
    cur_val: &BqValue,
    asc: bool,
    ctx: &ExecContext<'_>,
) -> BqResult<RangeBoundVal> {
    match bound {
        WindowFrameBound::Preceding(None) | WindowFrameBound::Following(None) => {
            Ok(RangeBoundVal::Unbounded)
        }
        WindowFrameBound::CurrentRow => Ok(RangeBoundVal::Value(cur_val.clone())),
        WindowFrameBound::Preceding(Some(expr)) => {
            let off = eval_nonneg_offset(expr, ctx)?;
            let op = if asc { BinaryOp::Sub } else { BinaryOp::Add };
            eval_range_shift(cur_val, &off, op)
        }
        WindowFrameBound::Following(Some(expr)) => {
            let off = eval_nonneg_offset(expr, ctx)?;
            let op = if asc { BinaryOp::Add } else { BinaryOp::Sub };
            eval_range_shift(cur_val, &off, op)
        }
    }
}

fn eval_range_shift(cur_val: &BqValue, off: &BqValue, op: BinaryOp) -> BqResult<RangeBoundVal> {
    if let (BqValue::Int64(c), BqValue::Int64(o)) = (cur_val, off) {
        let shifted = if op == BinaryOp::Sub {
            i128::from(*c) - i128::from(*o)
        } else {
            i128::from(*c) + i128::from(*o)
        };
        return Ok(RangeBoundVal::Int128(shifted));
    }
    if let BqValue::Float64(o) = off {
        if o.is_infinite() {
            return Ok(if op == BinaryOp::Sub {
                RangeBoundVal::NegInf
            } else {
                RangeBoundVal::PosInf
            });
        }
    }
    match eval_binary(op, cur_val.clone(), off.clone()) {
        Ok(v) => Ok(RangeBoundVal::Value(v)),
        Err(e) if e.kind == crate::error::BqErrorKind::Semantic => Ok(if op == BinaryOp::Sub {
            RangeBoundVal::NegInf
        } else {
            RangeBoundVal::PosInf
        }),
        Err(e) => Err(e),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "RANGE frame offset must be numeric or INTERVAL"
)]
fn eval_nonneg_offset(expr: &crate::ast::Expr, ctx: &ExecContext<'_>) -> BqResult<BqValue> {
    let v = eval_expr(expr, &EvalRow::default(), ctx)?;
    match &v {
        BqValue::Int64(n) if *n >= 0 => Ok(v),
        BqValue::Float64(f) if *f >= 0.0 && !f.is_nan() => Ok(v),
        BqValue::Numeric(n) if n.raw_scaled() >= 0 => Ok(v),
        BqValue::BigNumeric(_) | BqValue::Interval(_) => Ok(v),
        _ => Err(BqError::semantic(
            "RANGE frame offset must be non-negative numeric or INTERVAL",
        )),
    }
}

const fn frame_has_numeric_offsets(frame: &WindowFrame) -> bool {
    matches!(
        frame.start,
        WindowFrameBound::Preceding(Some(_)) | WindowFrameBound::Following(Some(_))
    ) || matches!(
        frame.end,
        Some(WindowFrameBound::Preceding(Some(_)) | WindowFrameBound::Following(Some(_)))
    )
}

fn resolve_row_bound(
    bound: &WindowFrameBound,
    pos: i128,
    last: i128,
    ctx: &ExecContext<'_>,
) -> BqResult<i128> {
    match bound {
        WindowFrameBound::CurrentRow => Ok(pos),
        WindowFrameBound::Preceding(None) => Ok(0),
        WindowFrameBound::Following(None) => Ok(last),
        WindowFrameBound::Preceding(Some(e)) => {
            let n = i128::try_from(eval_offset_int(e, ctx)?).unwrap_or(i128::MAX);
            Ok(pos.saturating_sub(n))
        }
        WindowFrameBound::Following(Some(e)) => {
            let n = i128::try_from(eval_offset_int(e, ctx)?).unwrap_or(i128::MAX);
            Ok(pos.saturating_add(n))
        }
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Window frame offset must be INT64"
)]
pub(super) fn eval_offset_int(expr: &crate::ast::Expr, ctx: &ExecContext<'_>) -> BqResult<usize> {
    match eval_expr(expr, &EvalRow::default(), ctx)? {
        BqValue::Int64(n) if n >= 0 => Ok(usize::try_from(n).unwrap_or(usize::MAX)),
        _ => Err(BqError::query(
            "Window frame offset must be non-negative INT64",
        )),
    }
}

fn find_peer_start(
    spec: &WindowSpec,
    pos: usize,
    part: &[usize],
    rows: &[EvalRow],
    ctx: &ExecContext<'_>,
) -> BqResult<usize> {
    let cur_row = rows
        .get(*part.get(pos).unwrap_or(&0))
        .ok_or_else(|| BqError::internal("row"))?;
    let cur_keys = order_keys(spec, cur_row, ctx)?;
    let mut start = pos;
    while start > 0 {
        let prev_idx = *part.get(start - 1).unwrap_or(&0);
        let prev_row = rows.get(prev_idx).ok_or_else(|| BqError::internal("row"))?;
        let prev_keys = order_keys(spec, prev_row, ctx)?;
        if keys_equal(&cur_keys, &prev_keys)? {
            start -= 1;
        } else {
            break;
        }
    }
    Ok(start)
}

pub(super) fn find_peer_end(
    spec: &WindowSpec,
    pos: usize,
    part: &[usize],
    rows: &[EvalRow],
    ctx: &ExecContext<'_>,
) -> BqResult<usize> {
    let cur_row = rows
        .get(*part.get(pos).unwrap_or(&0))
        .ok_or_else(|| BqError::internal("row"))?;
    let cur_keys = order_keys(spec, cur_row, ctx)?;
    let mut end = pos;
    while let Some(&next_idx) = part.get(end + 1) {
        let next_row = rows.get(next_idx).ok_or_else(|| BqError::internal("row"))?;
        let next_keys = order_keys(spec, next_row, ctx)?;
        if keys_equal(&cur_keys, &next_keys)? {
            end += 1;
        } else {
            break;
        }
    }
    Ok(end)
}

pub(super) fn order_keys(
    spec: &WindowSpec,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<BqValue>> {
    spec.order_by
        .iter()
        .map(|ob| eval_expr(&ob.expr, row, ctx))
        .collect()
}

pub(super) fn keys_equal(a: &[BqValue], b: &[BqValue]) -> BqResult<bool> {
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
