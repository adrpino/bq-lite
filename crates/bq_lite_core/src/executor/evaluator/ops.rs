use std::cmp::Ordering;

use crate::ast::{BinaryOp, IsTest, UnaryOp};
use crate::error::{BqError, BqResult};
use crate::types::coercion::{coerce_value, common_supertype};
use crate::types::BqValue;

pub(in crate::executor) fn eval_unary(op: UnaryOp, val: BqValue) -> BqResult<BqValue> {
    if val.is_null() {
        return Ok(BqValue::Null);
    }
    match (op, val) {
        (UnaryOp::Not, BqValue::Bool(b)) => Ok(BqValue::Bool(!b)),
        (UnaryOp::Plus, v) if v.infer_type()?.is_numeric() || matches!(v, BqValue::Interval(_)) => {
            Ok(v)
        }
        (UnaryOp::Minus, BqValue::Interval(iv)) => {
            crate::executor::scalar::interval_neg(iv).map(BqValue::Interval)
        }
        (UnaryOp::Minus, BqValue::Int64(n)) => n
            .checked_neg()
            .map(BqValue::Int64)
            .ok_or_else(|| BqError::semantic("INT64 overflow in unary negation")),
        (UnaryOp::Minus, BqValue::Float64(f)) => Ok(BqValue::Float64(-f)),
        (UnaryOp::Minus, BqValue::Numeric(n)) => n.checked_neg().map(BqValue::Numeric),
        (UnaryOp::Minus, BqValue::BigNumeric(bn)) => {
            bn.checked_neg().map(|v| BqValue::BigNumeric(Box::new(v)))
        }
        (UnaryOp::BitNot, BqValue::Int64(n)) => Ok(BqValue::Int64(!n)),
        (_, other) => Err(BqError::query(format!(
            "Unsupported unary operator {op:?} on {}",
            other.infer_type()?
        ))),
    }
}

pub(in crate::executor) const fn eval_is_test(val: &BqValue, test: IsTest) -> BqValue {
    let b = match test {
        IsTest::Null | IsTest::Unknown => val.is_null(),
        IsTest::NotNull | IsTest::NotUnknown => !val.is_null(),
        IsTest::True => matches!(val, BqValue::Bool(true)),
        IsTest::NotTrue => !matches!(val, BqValue::Bool(true)),
        IsTest::False => matches!(val, BqValue::Bool(false)),
        IsTest::NotFalse => !matches!(val, BqValue::Bool(false)),
    };
    BqValue::Bool(b)
}

pub(in crate::executor) fn eval_binary(
    op: BinaryOp,
    left: BqValue,
    right: BqValue,
) -> BqResult<BqValue> {
    match op {
        BinaryOp::IsDistinctFrom => return Ok(BqValue::Bool(!values_distinct_eq(&left, &right)?)),
        BinaryOp::IsNotDistinctFrom => {
            return Ok(BqValue::Bool(values_distinct_eq(&left, &right)?))
        }
        BinaryOp::And | BinaryOp::Or => return eval_logical(op, &left, &right),
        BinaryOp::Add
        | BinaryOp::Sub
        | BinaryOp::Mul
        | BinaryOp::Div
        | BinaryOp::Concat
        | BinaryOp::Eq
        | BinaryOp::Neq
        | BinaryOp::Lt
        | BinaryOp::Lte
        | BinaryOp::Gt
        | BinaryOp::Gte
        | BinaryOp::BitAnd
        | BinaryOp::BitOr
        | BinaryOp::BitXor
        | BinaryOp::ShiftLeft
        | BinaryOp::ShiftRight => {}
    }

    if left.is_null() || right.is_null() {
        return Ok(BqValue::Null);
    }

    match op {
        BinaryOp::Concat => eval_concat(left, right),
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div => {
            eval_arith(op, left, right)
        }
        BinaryOp::BitAnd
        | BinaryOp::BitOr
        | BinaryOp::BitXor
        | BinaryOp::ShiftLeft
        | BinaryOp::ShiftRight => eval_bitwise(op, left, right),
        BinaryOp::Eq
        | BinaryOp::Neq
        | BinaryOp::Lt
        | BinaryOp::Lte
        | BinaryOp::Gt
        | BinaryOp::Gte => eval_cmp(op, &left, &right),
        BinaryOp::And | BinaryOp::Or | BinaryOp::IsDistinctFrom | BinaryOp::IsNotDistinctFrom => {
            Ok(BqValue::Null)
        }
    }
}

fn eval_logical(op: BinaryOp, left: &BqValue, right: &BqValue) -> BqResult<BqValue> {
    let lb = to_opt_bool(left)?;
    let rb = to_opt_bool(right)?;
    let out = match (op, lb, rb) {
        (BinaryOp::And, Some(true), Some(true))
        | (BinaryOp::Or, Some(true), _)
        | (BinaryOp::Or, _, Some(true)) => Some(true),
        (BinaryOp::And, Some(false), _)
        | (BinaryOp::And, _, Some(false))
        | (BinaryOp::Or, Some(false), Some(false)) => Some(false),
        _ => None,
    };
    Ok(out.map_or(BqValue::Null, BqValue::Bool))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only Null and Bool are valid logical operands"
)]
pub(in crate::executor) fn to_opt_bool(v: &BqValue) -> BqResult<Option<bool>> {
    match v {
        BqValue::Null => Ok(None),
        BqValue::Bool(b) => Ok(Some(*b)),
        _ => Err(BqError::query(format!(
            "Expected BOOL expression, got {}",
            v.infer_type()?
        ))),
    }
}

pub(in crate::executor) fn combine_logical(
    op: BinaryOp,
    lb: Option<bool>,
    rb: Option<bool>,
) -> BqValue {
    let out = match (op, lb, rb) {
        (BinaryOp::And, Some(true), Some(true))
        | (BinaryOp::Or, Some(true), _)
        | (BinaryOp::Or, _, Some(true)) => Some(true),
        (BinaryOp::And, Some(false), _)
        | (BinaryOp::And, _, Some(false))
        | (BinaryOp::Or, Some(false), Some(false)) => Some(false),
        _ => None,
    };
    out.map_or(BqValue::Null, BqValue::Bool)
}

fn eval_concat(left: BqValue, right: BqValue) -> BqResult<BqValue> {
    match (left, right) {
        (BqValue::String(a), BqValue::String(b)) => Ok(BqValue::String(format!("{a}{b}"))),
        (BqValue::Bytes(mut a), BqValue::Bytes(b)) => {
            a.extend_from_slice(&b);
            Ok(BqValue::Bytes(a))
        }
        (BqValue::Array(mut a), BqValue::Array(b)) => {
            a.extend(b);
            Ok(BqValue::Array(a))
        }
        (l, r)
            if !matches!(
                l,
                BqValue::Array(_) | BqValue::Struct(_) | BqValue::Bytes(_)
            ) && !matches!(
                r,
                BqValue::Array(_) | BqValue::Struct(_) | BqValue::Bytes(_)
            ) =>
        {
            Ok(BqValue::String(format!("{l}{r}")))
        }
        (l, r) => Err(BqError::query(format!(
            "Cannot concatenate {} and {}",
            l.infer_type()?,
            r.infer_type()?
        ))),
    }
}

fn eval_arith(op: BinaryOp, left: BqValue, right: BqValue) -> BqResult<BqValue> {
    if op == BinaryOp::Div {
        return eval_div(left, right);
    }
    if matches!(left, BqValue::Interval(_)) || matches!(right, BqValue::Interval(_)) {
        return crate::executor::scalar::eval_temporal_interval_arith(op, &left, &right);
    }
    let supertype = common_supertype(&[left.infer_type()?, right.infer_type()?]);
    if let Ok(st) = supertype {
        let l = coerce_value(left, &st)?;
        let r = coerce_value(right, &st)?;
        return eval_numeric_arith(op, l, r);
    }
    crate::executor::scalar::eval_temporal_interval_arith(op, &left, &right)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only Add/Sub/Mul reach this arm after Div is dispatched"
)]
fn eval_numeric_arith(op: BinaryOp, l: BqValue, r: BqValue) -> BqResult<BqValue> {
    match (l, r) {
        (BqValue::Int64(a), BqValue::Int64(b)) => int64_arith(op, a, b),
        (BqValue::Float64(a), BqValue::Float64(b)) => Ok(BqValue::Float64(match op {
            BinaryOp::Add => a + b,
            BinaryOp::Sub => a - b,
            BinaryOp::Mul => a * b,
            _ => a / b,
        })),
        (BqValue::Numeric(a), BqValue::Numeric(b)) => match op {
            BinaryOp::Add => a.checked_add(b).map(BqValue::Numeric),
            BinaryOp::Sub => a.checked_sub(b).map(BqValue::Numeric),
            BinaryOp::Mul => a.checked_mul(b).map(BqValue::Numeric),
            _ => a.checked_div(b).map(BqValue::Numeric),
        },
        (BqValue::BigNumeric(a), BqValue::BigNumeric(b)) => match op {
            BinaryOp::Add => a.checked_add(&b).map(|v| BqValue::BigNumeric(Box::new(v))),
            BinaryOp::Sub => a.checked_sub(&b).map(|v| BqValue::BigNumeric(Box::new(v))),
            BinaryOp::Mul => a.checked_mul(&b).map(|v| BqValue::BigNumeric(Box::new(v))),
            _ => a.checked_div(&b).map(|v| BqValue::BigNumeric(Box::new(v))),
        },
        (l, r) => Err(BqError::query(format!(
            "Unsupported arithmetic between {} and {}",
            l.infer_type()?,
            r.infer_type()?
        ))),
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "GoogleSQL `/` on INT64 operands returns FLOAT64"
)]
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only INT64 and FLOAT64 are valid INTERVAL divisors"
)]
fn eval_div(left: BqValue, right: BqValue) -> BqResult<BqValue> {
    if let BqValue::Interval(iv) = left {
        return match right {
            BqValue::Int64(k) => {
                crate::executor::scalar::interval_div_i64(iv, k).map(BqValue::Interval)
            }
            BqValue::Float64(k) => {
                crate::executor::scalar::interval_div_f64(iv, k).map(BqValue::Interval)
            }
            other => Err(BqError::query(format!(
                "Cannot divide INTERVAL by {}",
                other.infer_type()?
            ))),
        };
    }
    let lt = left.infer_type()?;
    let rt = right.infer_type()?;
    let target =
        if matches!(lt, crate::types::BqType::Int64) && matches!(rt, crate::types::BqType::Int64) {
            crate::types::BqType::Float64
        } else {
            common_supertype(&[lt, rt])?
        };
    let l = coerce_value(left, &target)?;
    let r = coerce_value(right, &target)?;
    match (l, r) {
        (BqValue::Float64(a), BqValue::Float64(b)) => {
            if b == 0.0_f64 {
                Err(BqError::semantic("division by zero"))
            } else {
                Ok(BqValue::Float64(a / b))
            }
        }
        (BqValue::Numeric(a), BqValue::Numeric(b)) => a.checked_div(b).map(BqValue::Numeric),
        (BqValue::BigNumeric(a), BqValue::BigNumeric(b)) => {
            a.checked_div(&b).map(|v| BqValue::BigNumeric(Box::new(v)))
        }
        (BqValue::Int64(a), BqValue::Int64(b)) => {
            if b == 0 {
                Err(BqError::semantic("division by zero"))
            } else {
                Ok(BqValue::Float64((a as f64) / (b as f64)))
            }
        }
        _ => Err(BqError::query("Invalid operands for '/'")),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only Add/Sub/Mul are integer arithmetic ops"
)]
fn int64_arith(op: BinaryOp, a: i64, b: i64) -> BqResult<BqValue> {
    let res = match op {
        BinaryOp::Add => a.checked_add(b),
        BinaryOp::Sub => a.checked_sub(b),
        BinaryOp::Mul => a.checked_mul(b),
        _ => None,
    };
    res.map(BqValue::Int64)
        .ok_or_else(|| BqError::semantic("INT64 overflow in arithmetic"))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only bitwise BinaryOp variants reach eval_bitwise"
)]
fn eval_bitwise(op: BinaryOp, left: BqValue, right: BqValue) -> BqResult<BqValue> {
    let (BqValue::Int64(a), BqValue::Int64(b)) = (left, right) else {
        return Err(BqError::query("Bitwise operators require INT64 operands"));
    };
    let shift = u32::try_from(b).unwrap_or(64);
    let val = match op {
        BinaryOp::BitAnd => a & b,
        BinaryOp::BitOr => a | b,
        BinaryOp::BitXor => a ^ b,
        BinaryOp::ShiftLeft => a.checked_shl(shift).unwrap_or(0),
        BinaryOp::ShiftRight => a.checked_shr(shift).unwrap_or(0),
        _ => return Err(BqError::internal("unexpected bitwise op")),
    };
    Ok(BqValue::Int64(val))
}

fn eval_cmp(op: BinaryOp, left: &BqValue, right: &BqValue) -> BqResult<BqValue> {
    if matches!(op, BinaryOp::Eq | BinaryOp::Neq) {
        return Ok(sql_values_equal(left, right)?.map_or(BqValue::Null, |eq| {
            BqValue::Bool(if op == BinaryOp::Eq { eq } else { !eq })
        }));
    }
    let ord = compare_values(left, right)?;
    let res = match ord {
        Some(Ordering::Less) => matches!(op, BinaryOp::Lt | BinaryOp::Lte),
        Some(Ordering::Equal) => matches!(op, BinaryOp::Lte | BinaryOp::Gte),
        Some(Ordering::Greater) => matches!(op, BinaryOp::Gt | BinaryOp::Gte),
        None => false,
    };
    Ok(BqValue::Bool(res))
}

pub(in crate::executor) fn values_distinct_eq(a: &BqValue, b: &BqValue) -> BqResult<bool> {
    if a.is_null() && b.is_null() {
        return Ok(true);
    }
    if a.is_null() || b.is_null() {
        return Ok(false);
    }
    if let (BqValue::Float64(fa), BqValue::Float64(fb)) = (a, b) {
        if fa.is_nan() && fb.is_nan() {
            return Ok(true);
        }
    }
    match (&a, &b) {
        (BqValue::Array(la), BqValue::Array(lb)) => {
            if la.len() != lb.len() {
                return Ok(false);
            }
            for (xa, xb) in la.iter().zip(lb.iter()) {
                if !values_distinct_eq(xa, xb)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (BqValue::Struct(sa), BqValue::Struct(sb)) => {
            if sa.len() != sb.len() {
                return Ok(false);
            }
            for (fa, fb) in sa.iter().zip(sb.iter()) {
                if !values_distinct_eq(&fa.value, &fb.value)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        _ => values_equal(a, b),
    }
}

/// 3-valued SQL equality (`None` means `NULL` / unknown when a nested `ARRAY`/`STRUCT` element is `NULL`).
#[expect(
    clippy::float_cmp,
    reason = "SQL Float64 equality intentionally implements IEEE-754 comparison (NaN != NaN)"
)]
pub(in crate::executor) fn sql_values_equal(a: &BqValue, b: &BqValue) -> BqResult<Option<bool>> {
    if a.is_null() || b.is_null() {
        return Ok(None);
    }
    if let Some(eq) = super::literals::try_fast_sql_equal(a, b) {
        return Ok(Some(eq));
    }
    let supertype = common_supertype(&[a.infer_type()?, b.infer_type()?])?;
    let ca = coerce_value(a.clone(), &supertype)?;
    let cb = coerce_value(b.clone(), &supertype)?;
    match (&ca, &cb) {
        (BqValue::Float64(fa), BqValue::Float64(fb)) => Ok(Some(fa == fb)),
        (BqValue::Array(la), BqValue::Array(lb)) => {
            if la.len() != lb.len() {
                return Ok(Some(false));
            }
            let mut saw_null = false;
            for (xa, xb) in la.iter().zip(lb.iter()) {
                match sql_values_equal(xa, xb)? {
                    Some(false) => return Ok(Some(false)),
                    None => saw_null = true,
                    Some(true) => {}
                }
            }
            Ok(if saw_null { None } else { Some(true) })
        }
        (BqValue::Struct(sa), BqValue::Struct(sb)) => {
            if sa.len() != sb.len() {
                return Ok(Some(false));
            }
            let mut saw_null = false;
            for (fa, fb) in sa.iter().zip(sb.iter()) {
                match sql_values_equal(&fa.value, &fb.value)? {
                    Some(false) => return Ok(Some(false)),
                    None => saw_null = true,
                    Some(true) => {}
                }
            }
            Ok(if saw_null { None } else { Some(true) })
        }
        (BqValue::Interval(ia), BqValue::Interval(ib)) => Ok(Some(
            super::literals::interval_sort_key(*ia) == super::literals::interval_sort_key(*ib),
        )),
        _ => Ok(Some(ca == cb)),
    }
}

pub(in crate::executor) fn values_equal(a: &BqValue, b: &BqValue) -> BqResult<bool> {
    Ok(matches!(sql_values_equal(a, b)?, Some(true)))
}

pub(in crate::executor) fn compare_values(a: &BqValue, b: &BqValue) -> BqResult<Option<Ordering>> {
    if a.is_null() || b.is_null() {
        return Ok(None);
    }
    if let super::literals::FastCompare::Handled(ord) = super::literals::try_fast_compare(a, b)? {
        return Ok(ord);
    }
    let supertype = common_supertype(&[a.infer_type()?, b.infer_type()?])?;
    let ca = coerce_value(a.clone(), &supertype)?;
    let cb = coerce_value(b.clone(), &supertype)?;
    let ord = match (ca, cb) {
        (BqValue::Bool(x), BqValue::Bool(y)) => x.partial_cmp(&y),
        (BqValue::Int64(x), BqValue::Int64(y)) => x.partial_cmp(&y),
        (BqValue::Float64(x), BqValue::Float64(y)) => x.partial_cmp(&y),
        (BqValue::Numeric(x), BqValue::Numeric(y)) => x.partial_cmp(&y),
        (BqValue::BigNumeric(x), BqValue::BigNumeric(y)) => x.partial_cmp(&y),
        (BqValue::String(x), BqValue::String(y)) => x.partial_cmp(&y),
        (BqValue::Bytes(x), BqValue::Bytes(y)) => x.partial_cmp(&y),
        (BqValue::Date(x), BqValue::Date(y)) => x.partial_cmp(&y),
        (BqValue::Datetime(x), BqValue::Datetime(y)) => x.partial_cmp(&y),
        (BqValue::Time(x), BqValue::Time(y)) => x.partial_cmp(&y),
        (BqValue::Timestamp(x), BqValue::Timestamp(y)) => x.partial_cmp(&y),
        (BqValue::Interval(x), BqValue::Interval(y)) => {
            Some(super::literals::interval_sort_key(x).cmp(&super::literals::interval_sort_key(y)))
        }
        (BqValue::Range(rx), BqValue::Range(ry)) => super::literals::compare_ranges(&rx, &ry)?,
        (BqValue::Array(ax), BqValue::Array(ay)) => super::literals::compare_arrays(&ax, &ay)?,
        _ => {
            return Err(BqError::query(format!(
                "Values of type {supertype} cannot be ordered"
            )))
        }
    };
    Ok(ord)
}
