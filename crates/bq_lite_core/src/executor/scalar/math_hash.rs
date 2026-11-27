use std::cmp::Ordering;

use digest::Digest;
use md5::Md5;
use sha1::Sha1;
use sha2::{Sha256, Sha512};

use crate::error::{BqError, BqResult};
use crate::executor::evaluator::{compare_values, eval_binary};
use crate::types::coercion::{coerce_value, common_supertype};
use crate::types::BqValue;

use super::farmhash::farm_fingerprint64;

pub(super) const MATH_HASH_FUNCTIONS: &[&str] = &[
    "ABS",
    "CBRT",
    "CEIL",
    "CEILING",
    "DIV",
    "EXP",
    "FARM_FINGERPRINT",
    "FLOOR",
    "GREATEST",
    "IEEE_DIVIDE",
    "LEAST",
    "LN",
    "LOG",
    "LOG10",
    "MD5",
    "MOD",
    "POW",
    "POWER",
    "RAND",
    "RANGE_BUCKET",
    "ROUND",
    "SAFE_ADD",
    "SAFE_DIVIDE",
    "SAFE_MULTIPLY",
    "SAFE_NEGATE",
    "SAFE_SUBTRACT",
    "SHA1",
    "SHA256",
    "SHA512",
    "SIGN",
    "SQRT",
    "TRUNC",
];

pub(super) fn eval_math_hash_fn(name: &str, args: Vec<BqValue>) -> BqResult<Option<BqValue>> {
    match name {
        "ABS" => eval_abs(args).map(Some),
        "SIGN" => eval_sign(args).map(Some),
        "ROUND" => eval_round(args, false).map(Some),
        "TRUNC" => eval_round(args, true).map(Some),
        "CEIL" | "CEILING" => eval_float_unary(args, name, f64::ceil).map(Some),
        "FLOOR" => eval_float_unary(args, name, f64::floor).map(Some),
        "SQRT" => eval_sqrt(args).map(Some),
        "CBRT" => eval_float_unary(args, name, f64::cbrt).map(Some),
        "EXP" => eval_float_unary(args, name, f64::exp).map(Some),
        "LN" => eval_ln(args).map(Some),
        "LOG" => eval_log(args).map(Some),
        "LOG10" => eval_log10(args).map(Some),
        "POW" | "POWER" => eval_pow(args).map(Some),
        "MOD" => eval_mod(args).map(Some),
        "DIV" => eval_int_div(args).map(Some),
        "IEEE_DIVIDE" => eval_ieee_divide(args).map(Some),
        "SAFE_DIVIDE" => eval_safe_binary(crate::ast::BinaryOp::Div, args).map(Some),
        "SAFE_ADD" => eval_safe_binary(crate::ast::BinaryOp::Add, args).map(Some),
        "SAFE_SUBTRACT" => eval_safe_binary(crate::ast::BinaryOp::Sub, args).map(Some),
        "SAFE_MULTIPLY" => eval_safe_binary(crate::ast::BinaryOp::Mul, args).map(Some),
        "SAFE_NEGATE" => eval_safe_negate(args).map(Some),
        "GREATEST" => eval_extremum(args, true).map(Some),
        "LEAST" => eval_extremum(args, false).map(Some),
        "RAND" => Ok(Some(BqValue::Float64(random_unit_f64()))),
        "RANGE_BUCKET" => eval_range_bucket(args).map(Some),
        "MD5" => eval_hash(args, |b| Md5::digest(b).to_vec()).map(Some),
        "SHA1" => eval_hash(args, |b| Sha1::digest(b).to_vec()).map(Some),
        "SHA256" => eval_hash(args, |b| Sha256::digest(b).to_vec()).map(Some),
        "SHA512" => eval_hash(args, |b| Sha512::digest(b).to_vec()).map(Some),
        "FARM_FINGERPRINT" => eval_farm_fingerprint(args).map(Some),
        _ => Ok(None),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only numeric scalars are valid math inputs"
)]
fn to_f64(v: &BqValue, name: &str) -> BqResult<f64> {
    #[expect(
        clippy::cast_precision_loss,
        reason = "BigQuery math promotes INT64 to FLOAT64"
    )]
    match v {
        BqValue::Int64(n) => Ok(*n as f64),
        BqValue::Float64(f) => Ok(*f),
        BqValue::Numeric(n) => Ok(n.to_f64()),
        BqValue::BigNumeric(bn) => Ok(bn.to_f64()),
        other => Err(BqError::query(format!(
            "{name} expects numeric argument, got {}",
            other.infer_type()?
        ))),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only numeric scalars are valid ABS arguments"
)]
fn eval_abs(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("ABS expects 1 argument"))?;
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::Int64(n) => n
            .checked_abs()
            .map(BqValue::Int64)
            .ok_or_else(|| BqError::semantic("INT64 overflow in ABS")),
        BqValue::Float64(f) => Ok(BqValue::Float64(f.abs())),
        BqValue::Numeric(n) => {
            if n.raw_scaled() < 0 {
                n.checked_neg().map(BqValue::Numeric)
            } else {
                Ok(BqValue::Numeric(n))
            }
        }
        other => Err(BqError::query(format!(
            "ABS expects numeric, got {}",
            other.infer_type()?
        ))),
    }
}

fn eval_sign(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("SIGN expects 1 argument"))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let f = to_f64(&arg, "SIGN")?;
    let s = if f > 0.0 {
        1
    } else if f < 0.0 {
        -1
    } else {
        0
    };
    Ok(BqValue::Int64(s))
}

fn eval_round(args: Vec<BqValue>, trunc_mode: bool) -> BqResult<BqValue> {
    let max_args = if trunc_mode { 2 } else { 3 };
    if args.is_empty() || args.len() > max_args {
        return Err(BqError::query(if trunc_mode {
            "TRUNC expects 1 or 2 arguments"
        } else {
            "ROUND expects 1, 2, or 3 arguments"
        }));
    }
    let mut it = args.into_iter();
    let v = it.next().unwrap_or(BqValue::Null);
    let digits = match it.next() {
        Some(BqValue::Null) => return Ok(BqValue::Null),
        Some(BqValue::Int64(d)) => i32::try_from(d).unwrap_or(0),
        Some(_) => return Err(BqError::query("ROUND/TRUNC digits must be INT64")),
        None => 0,
    };
    let half_even = match it.next() {
        None => false,
        Some(BqValue::Null) => return Ok(BqValue::Null),
        Some(BqValue::String(mode)) => match mode.to_ascii_uppercase().as_str() {
            "ROUND_HALF_AWAY_FROM_ZERO" => false,
            "ROUND_HALF_EVEN" => true,
            _ => {
                return Err(BqError::semantic(format!(
                    "Invalid ROUND rounding_mode: '{mode}'"
                )))
            }
        },
        Some(_) => return Err(BqError::query("ROUND rounding_mode must be STRING")),
    };
    if v.is_null() {
        return Ok(BqValue::Null);
    }
    if let BqValue::Numeric(n) = v {
        return n
            .round_or_trunc(digits, trunc_mode, half_even)
            .map(BqValue::Numeric);
    }
    if let BqValue::BigNumeric(bn) = v {
        return bn
            .round_or_trunc(digits, trunc_mode, half_even)
            .map(|r| BqValue::BigNumeric(Box::new(r)));
    }
    let f = to_f64(&v, "ROUND")?;
    let factor = 10.0_f64.powi(digits);
    let res = if trunc_mode {
        (f * factor).trunc() / factor
    } else if half_even {
        (f * factor).round_ties_even() / factor
    } else {
        (f * factor).round() / factor
    };
    Ok(BqValue::Float64(res))
}

fn eval_float_unary(
    args: Vec<BqValue>,
    name: &str,
    f: impl FnOnce(f64) -> f64,
) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query(format!("{name} expects 1 argument")))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    Ok(BqValue::Float64(f(to_f64(&arg, name)?)))
}

fn eval_sqrt(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("SQRT expects 1 argument"))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let f = to_f64(&arg, "SQRT")?;
    if f < 0.0 {
        return Err(BqError::semantic("SQRT input cannot be negative"));
    }
    Ok(BqValue::Float64(f.sqrt()))
}

fn eval_ln(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("LN expects 1 argument"))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let f = to_f64(&arg, "LN")?;
    if f <= 0.0 {
        return Err(BqError::semantic("LN input must be positive"));
    }
    Ok(BqValue::Float64(f.ln()))
}

fn eval_log10(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("LOG10 expects 1 argument"))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let f = to_f64(&arg, "LOG10")?;
    if f <= 0.0 {
        return Err(BqError::semantic("LOG10 input must be positive"));
    }
    Ok(BqValue::Float64(f.log10()))
}

fn eval_log(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(BqError::query("LOG expects 1 or 2 arguments"));
    }
    let mut it = args.into_iter();
    let v = it.next().unwrap_or(BqValue::Null);
    let base_v = it.next();
    if v.is_null() || base_v.as_ref().is_some_and(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    let f = to_f64(&v, "LOG")?;
    if f <= 0.0 {
        return Err(BqError::semantic("LOG input must be positive"));
    }
    match base_v {
        Some(b) => {
            let bf = to_f64(&b, "LOG")?;
            if bf <= 0.0 || (bf - 1.0).abs() == 0.0 {
                return Err(BqError::semantic("LOG base must be positive and != 1"));
            }
            Ok(BqValue::Float64(f.log(bf)))
        }
        None => Ok(BqValue::Float64(f.ln())),
    }
}

fn eval_pow(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [a, b]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("POW expects 2 arguments"))?;
    if a.is_null() || b.is_null() {
        return Ok(BqValue::Null);
    }
    Ok(BqValue::Float64(
        to_f64(&a, "POW")?.powf(to_f64(&b, "POW")?),
    ))
}

fn eval_mod(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [a, b]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("MOD expects 2 arguments"))?;
    if a.is_null() || b.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::Int64(x), BqValue::Int64(y)) = (a, b) else {
        return Err(BqError::query("MOD currently requires INT64 operands"));
    };
    if y == 0 {
        return Err(BqError::semantic("division by zero in MOD"));
    }
    Ok(BqValue::Int64(x % y))
}

fn eval_int_div(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [a, b]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("DIV expects 2 arguments"))?;
    if a.is_null() || b.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::Int64(x), BqValue::Int64(y)) = (a, b) else {
        return Err(BqError::query("DIV currently requires INT64 operands"));
    };
    if y == 0 {
        return Err(BqError::semantic("division by zero in DIV"));
    }
    x.checked_div(y)
        .map(BqValue::Int64)
        .ok_or_else(|| BqError::semantic("INT64 overflow in DIV"))
}

fn eval_ieee_divide(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [a, b]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("IEEE_DIVIDE expects 2 arguments"))?;
    if a.is_null() || b.is_null() {
        return Ok(BqValue::Null);
    }
    Ok(BqValue::Float64(
        to_f64(&a, "IEEE_DIVIDE")? / to_f64(&b, "IEEE_DIVIDE")?,
    ))
}

fn eval_safe_binary(op: crate::ast::BinaryOp, args: Vec<BqValue>) -> BqResult<BqValue> {
    let [a, b]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("SAFE binary op expects 2 arguments"))?;
    if a.is_null() || b.is_null() {
        return Ok(BqValue::Null);
    }
    match eval_binary(op, a, b) {
        Ok(v) => Ok(v),
        Err(e) if e.is_safe_suppressible() => Ok(BqValue::Null),
        Err(e) => Err(e),
    }
}

fn eval_safe_negate(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [a]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("SAFE_NEGATE expects 1 argument"))?;
    if a.is_null() {
        return Ok(BqValue::Null);
    }
    match crate::executor::evaluator::eval_unary(crate::ast::UnaryOp::Minus, a) {
        Ok(v) => Ok(v),
        Err(e) if e.is_safe_suppressible() => Ok(BqValue::Null),
        Err(e) => Err(e),
    }
}

fn eval_extremum(args: Vec<BqValue>, is_greatest: bool) -> BqResult<BqValue> {
    if args.is_empty() {
        return Err(BqError::query("GREATEST/LEAST expects at least 1 argument"));
    }
    if args.iter().any(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    let types: Vec<_> = args
        .iter()
        .map(BqValue::infer_type)
        .collect::<BqResult<_>>()?;
    let supertype = common_supertype(&types)?;
    let mut best: Option<BqValue> = None;
    for a in args {
        let v = coerce_value(a, &supertype)?;
        best = match best {
            None => Some(v),
            Some(cur) => {
                let ord = compare_values(&v, &cur)?;
                let take = if is_greatest {
                    ord == Some(Ordering::Greater)
                } else {
                    ord == Some(Ordering::Less)
                };
                Some(if take { v } else { cur })
            }
        };
    }
    Ok(best.unwrap_or(BqValue::Null))
}

fn eval_range_bucket(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [point, bounds]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("RANGE_BUCKET expects 2 arguments"))?;
    if point.is_null() || bounds.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Array(arr) = bounds else {
        return Err(BqError::query("RANGE_BUCKET second argument must be ARRAY"));
    };
    let mut bucket = 0_i64;
    for b in arr {
        if b.is_null() {
            return Ok(BqValue::Null);
        }
        if matches!(
            compare_values(&point, &b)?,
            Some(Ordering::Greater | Ordering::Equal)
        ) {
            bucket += 1;
        }
    }
    Ok(BqValue::Int64(bucket))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only STRING and BYTES are valid hash inputs"
)]
fn eval_hash(args: Vec<BqValue>, hasher: impl FnOnce(&[u8]) -> Vec<u8>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("Hash function expects 1 argument"))?;
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::String(s) => Ok(BqValue::Bytes(hasher(s.as_bytes()))),
        BqValue::Bytes(b) => Ok(BqValue::Bytes(hasher(&b))),
        other => Err(BqError::query(format!(
            "Hash function expects STRING or BYTES, got {}",
            other.infer_type()?
        ))),
    }
}

fn random_unit_f64() -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "53-bit masked mantissa fits losslessly in IEEE-754 f64"
    )]
    let val = {
        let bits =
            u64::try_from(uuid::Uuid::new_v4().as_u128() & ((1_u128 << 53) - 1)).unwrap_or(0);
        (bits as f64) / ((1_u64 << 53) as f64)
    };
    val
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only STRING and BYTES are valid FARM_FINGERPRINT inputs"
)]
fn eval_farm_fingerprint(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("FARM_FINGERPRINT expects 1 argument"))?;
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::String(s) => Ok(BqValue::Int64(farm_fingerprint64(s.as_bytes()))),
        BqValue::Bytes(b) => Ok(BqValue::Int64(farm_fingerprint64(&b))),
        other => Err(BqError::query(format!(
            "FARM_FINGERPRINT expects STRING or BYTES, got {}",
            other.infer_type()?
        ))),
    }
}
