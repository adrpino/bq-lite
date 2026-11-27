use crate::ast::{BinaryOp, Expr, FunctionCall, NullHandling, WindowSpec};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext};
use crate::executor::evaluator::{eval_binary, eval_expr};
use crate::types::BqValue;

use super::agg_builtins::{eval_aggregate_call, is_aggregate_name};
use super::exec_util::RowSlice;
use super::window_frames::{eval_offset_int, frame_bounds, keys_equal};

#[derive(Clone, Copy)]
pub(super) struct WindowPart<'a> {
    pub indices: &'a [usize],
    pub keys: &'a [Vec<BqValue>],
    pub rows: &'a [EvalRow],
}

pub(super) fn eval_window_partition(
    call: &FunctionCall,
    spec: &WindowSpec,
    part: WindowPart<'_>,
    ctx: &ExecContext<'_>,
    out: &mut [BqValue],
) -> BqResult<()> {
    let upper = call.name.normalized_parts().join(".").to_ascii_uppercase();
    if is_aggregate_name(&upper) {
        return eval_window_agg(&upper, call, spec, part, ctx, out);
    }
    if spec.frame.is_some()
        && matches!(
            upper.as_str(),
            "ROW_NUMBER"
                | "RANK"
                | "DENSE_RANK"
                | "NTILE"
                | "PERCENT_RANK"
                | "CUME_DIST"
                | "LAG"
                | "LEAD"
        )
    {
        return Err(BqError::query(format!(
            "Window frame is not allowed for analytic function {upper}"
        )));
    }
    match upper.as_str() {
        "ROW_NUMBER" => {
            for (pos, &row_idx) in part.indices.iter().enumerate() {
                set_out(
                    out,
                    row_idx,
                    BqValue::Int64(i64::try_from(pos + 1).unwrap_or(i64::MAX)),
                );
            }
            Ok(())
        }
        "RANK" | "DENSE_RANK" => eval_rank_family(&upper, part.indices, part.keys, out),
        "NTILE" => eval_ntile(call, part.indices, ctx, out),
        "PERCENT_RANK" | "CUME_DIST" => {
            eval_dist_family(&upper, part.indices, part.keys, out);
            Ok(())
        }
        "LAG" | "LEAD" => eval_lag_lead(&upper, call, part.indices, part.rows, ctx, out),
        "FIRST_VALUE" | "LAST_VALUE" | "NTH_VALUE" => {
            eval_value_fn(&upper, call, part, spec, ctx, out)
        }
        _ => Err(BqError::unimplemented(format!(
            "Window function '{upper}' is not implemented"
        ))),
    }
}

fn eval_rank_family(
    name: &str,
    part: &[usize],
    part_keys: &[Vec<BqValue>],
    out: &mut [BqValue],
) -> BqResult<()> {
    let mut rank = 1_i64;
    let mut dense = 1_i64;
    let mut prev_keys: Option<&[BqValue]> = None;
    for (pos, &row_idx) in part.iter().enumerate() {
        let keys = part_keys.get(pos).map_or(&[][..], Vec::as_slice);
        if let Some(prev) = prev_keys {
            if !keys_equal(prev, keys)? {
                rank = i64::try_from(pos + 1).unwrap_or(i64::MAX);
                dense += 1;
            }
        }
        set_out(
            out,
            row_idx,
            BqValue::Int64(if name == "RANK" { rank } else { dense }),
        );
        prev_keys = Some(keys);
    }
    Ok(())
}

#[expect(
    clippy::cast_precision_loss,
    reason = "PERCENT_RANK and CUME_DIST return IEEE-754 FLOAT64 in GoogleSQL"
)]
fn eval_dist_family(name: &str, part: &[usize], part_keys: &[Vec<BqValue>], out: &mut [BqValue]) {
    let total = part.len();
    for (pos, &row_idx) in part.iter().enumerate() {
        let cur = part_keys.get(pos).map_or(&[][..], Vec::as_slice);
        let val = if name == "PERCENT_RANK" {
            if total <= 1 {
                0.0
            } else {
                let first_peer = part_keys
                    .iter()
                    .position(|k| keys_equal(k, cur).unwrap_or(false))
                    .unwrap_or(pos);
                (first_peer as f64) / ((total - 1) as f64)
            }
        } else {
            let last_peer = part_keys
                .iter()
                .rposition(|k| keys_equal(k, cur).unwrap_or(false))
                .unwrap_or(pos);
            ((last_peer + 1) as f64) / (total as f64)
        };
        set_out(out, row_idx, BqValue::Float64(val));
    }
}

fn eval_ntile(
    call: &FunctionCall,
    part: &[usize],
    ctx: &ExecContext<'_>,
    out: &mut [BqValue],
) -> BqResult<()> {
    let arg = call
        .args
        .first()
        .ok_or_else(|| BqError::query("NTILE requires 1 argument"))?;
    let BqValue::Int64(buckets) = eval_expr(&arg.expr, &EvalRow::default(), ctx)? else {
        return Err(BqError::query("NTILE argument must be INT64"));
    };
    if buckets <= 0 {
        return Err(BqError::semantic("NTILE argument must be positive"));
    }
    let n = part.len();
    let b = usize::try_from(buckets).unwrap_or(usize::MAX);
    let base = n / b;
    let rem = n % b;
    for (pos, &row_idx) in part.iter().enumerate() {
        let bucket = if pos < rem * (base + 1) {
            pos / (base + 1) + 1
        } else {
            rem + (pos - rem * (base + 1)) / base.max(1) + 1
        };
        set_out(
            out,
            row_idx,
            BqValue::Int64(i64::try_from(bucket).unwrap_or(i64::MAX)),
        );
    }
    Ok(())
}

fn eval_lag_lead(
    name: &str,
    call: &FunctionCall,
    part: &[usize],
    rows: &[EvalRow],
    ctx: &ExecContext<'_>,
    out: &mut [BqValue],
) -> BqResult<()> {
    let expr = &call
        .args
        .first()
        .ok_or_else(|| BqError::query(format!("{name} requires at least 1 argument")))?
        .expr;
    let offset = match call.args.get(1) {
        Some(a) => eval_offset_int(&a.expr, ctx)?,
        None => 1,
    };
    let default_val = match call.args.get(2) {
        Some(a) => eval_expr(&a.expr, &EvalRow::default(), ctx)?,
        None => BqValue::Null,
    };
    let ignore_nulls = call.null_handling == Some(NullHandling::IgnoreNulls);
    let vals: Vec<BqValue> = part
        .iter()
        .map(|&idx| {
            let row = rows.get(idx).ok_or_else(|| BqError::internal("row idx"))?;
            eval_expr(expr, row, ctx)
        })
        .collect::<BqResult<_>>()?;

    for (pos, &row_idx) in part.iter().enumerate() {
        let target = if offset == 0 {
            vals.get(pos).cloned()
        } else if name == "LAG" {
            step_backward(pos, offset, &vals, ignore_nulls)
        } else {
            step_forward(pos, offset, &vals, ignore_nulls)
        };
        set_out(out, row_idx, target.unwrap_or_else(|| default_val.clone()));
    }
    Ok(())
}

fn step_backward(
    pos: usize,
    offset: usize,
    vals: &[BqValue],
    ignore_nulls: bool,
) -> Option<BqValue> {
    if !ignore_nulls {
        return pos.checked_sub(offset).and_then(|i| vals.get(i).cloned());
    }
    let mut rem = offset;
    let mut idx = pos;
    while idx > 0 {
        idx -= 1;
        if let Some(v) = vals.get(idx) {
            if !v.is_null() {
                rem -= 1;
                if rem == 0 {
                    return Some(v.clone());
                }
            }
        }
    }
    None
}

fn step_forward(
    pos: usize,
    offset: usize,
    vals: &[BqValue],
    ignore_nulls: bool,
) -> Option<BqValue> {
    if !ignore_nulls {
        return pos.checked_add(offset).and_then(|i| vals.get(i).cloned());
    }
    let mut rem = offset;
    let mut idx = pos + 1;
    while let Some(v) = vals.get(idx) {
        if !v.is_null() {
            rem -= 1;
            if rem == 0 {
                return Some(v.clone());
            }
        }
        idx += 1;
    }
    None
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "NTH_VALUE position must be INT64 >= 1"
)]
fn eval_value_fn(
    name: &str,
    call: &FunctionCall,
    part: WindowPart<'_>,
    spec: &WindowSpec,
    ctx: &ExecContext<'_>,
    out: &mut [BqValue],
) -> BqResult<()> {
    let expr = &call
        .args
        .first()
        .ok_or_else(|| BqError::query(format!("{name} requires 1 argument")))?
        .expr;
    let nth = if name == "NTH_VALUE" {
        let arg = call
            .args
            .get(1)
            .ok_or_else(|| BqError::query("NTH_VALUE requires 2 arguments"))?;
        match eval_expr(&arg.expr, &EvalRow::default(), ctx)? {
            BqValue::Int64(n) if n >= 1 => usize::try_from(n - 1).unwrap_or(usize::MAX),
            _ => return Err(BqError::semantic("NTH_VALUE position must be >= 1")),
        }
    } else {
        0
    };
    let ignore_nulls = call.null_handling == Some(NullHandling::IgnoreNulls);
    let vals: Vec<BqValue> = part
        .indices
        .iter()
        .map(|&idx| {
            let row = part
                .rows
                .get(idx)
                .ok_or_else(|| BqError::internal("row idx"))?;
            eval_expr(expr, row, ctx)
        })
        .collect::<BqResult<_>>()?;
    for (pos, &row_idx) in part.indices.iter().enumerate() {
        let (start, end) = frame_bounds(spec, pos, part.indices.len(), part.keys, ctx)?;
        let slice = vals.get(start..=end).unwrap_or(&[]);
        let picked = if ignore_nulls {
            let mut iter = slice.iter().filter(|v| !v.is_null());
            match name {
                "FIRST_VALUE" => iter.next().cloned(),
                "LAST_VALUE" => iter.next_back().cloned(),
                _ => iter.nth(nth).cloned(),
            }
        } else {
            match name {
                "FIRST_VALUE" => slice.first().cloned(),
                "LAST_VALUE" => slice.last().cloned(),
                _ => slice.get(nth).cloned(),
            }
        };
        set_out(out, row_idx, picked.unwrap_or(BqValue::Null));
    }
    Ok(())
}

fn eval_window_agg(
    upper: &str,
    call: &FunctionCall,
    spec: &WindowSpec,
    part: WindowPart<'_>,
    ctx: &ExecContext<'_>,
    out: &mut [BqValue],
) -> BqResult<()> {
    let is_count_star = upper == "COUNT"
        && call.having.is_none()
        && matches!(call.args.first().map(|a| &a.expr), Some(Expr::Star));
    if is_count_star {
        for (pos, &row_idx) in part.indices.iter().enumerate() {
            let (start, end) = frame_bounds(spec, pos, part.indices.len(), part.keys, ctx)?;
            let count = if start <= end && end < part.indices.len() {
                end - start + 1
            } else {
                0
            };
            set_out(
                out,
                row_idx,
                BqValue::Int64(i64::try_from(count).unwrap_or(i64::MAX)),
            );
        }
        return Ok(());
    }
    if upper == "SUM"
        && !call.distinct
        && call.having.is_none()
        && spec.frame.is_none()
        && !call.safe_prefix
    {
        if let Some(arg) = call.args.first() {
            return eval_cumulative_sum(arg, spec, part, ctx, out);
        }
    }
    for (pos, &row_idx) in part.indices.iter().enumerate() {
        let (start, end) = frame_bounds(spec, pos, part.indices.len(), part.keys, ctx)?;
        let indices = part.indices.get(start..=end).unwrap_or(&[]);
        let val = eval_aggregate_call(
            call,
            RowSlice::Indexed {
                rows: part.rows,
                indices,
            },
            ctx,
        )?;
        set_out(out, row_idx, val);
    }
    Ok(())
}

fn eval_cumulative_sum(
    arg: &crate::ast::FunctionArg,
    spec: &WindowSpec,
    part: WindowPart<'_>,
    ctx: &ExecContext<'_>,
    out: &mut [BqValue],
) -> BqResult<()> {
    let mut vals = Vec::with_capacity(part.indices.len());
    for &row_idx in part.indices {
        let row = part
            .rows
            .get(row_idx)
            .ok_or_else(|| BqError::internal("row idx"))?;
        vals.push(eval_expr(&arg.expr, row, ctx)?);
    }
    let mut prefix: Vec<BqValue> = Vec::with_capacity(part.indices.len());
    let mut acc: Option<BqValue> = None;
    for v in vals {
        if !v.is_null() {
            acc = match acc {
                None => Some(v),
                Some(cur) => Some(eval_binary(BinaryOp::Add, cur, v)?),
            };
        }
        prefix.push(acc.clone().unwrap_or(BqValue::Null));
    }
    for (pos, &row_idx) in part.indices.iter().enumerate() {
        let (_, end) = frame_bounds(spec, pos, part.indices.len(), part.keys, ctx)?;
        let val = prefix.get(end).cloned().unwrap_or(BqValue::Null);
        set_out(out, row_idx, val);
    }
    Ok(())
}

fn set_out(out: &mut [BqValue], idx: usize, val: BqValue) {
    if let Some(slot) = out.get_mut(idx) {
        *slot = val;
    }
}
