use crate::error::{BqError, BqResult};
use crate::executor::evaluator::values_equal;
use crate::types::coercion::{cast_value, common_supertype};
use crate::types::BqValue;

pub(super) const ARRAY_UTIL_FUNCTIONS: &[&str] = &[
    "ARRAY_CONCAT",
    "ARRAY_FILTER",
    "ARRAY_FIRST",
    "ARRAY_LAST",
    "ARRAY_LENGTH",
    "ARRAY_REVERSE",
    "ARRAY_SLICE",
    "ARRAY_TO_STRING",
    "ARRAY_TRANSFORM",
    "COALESCE",
    "ERROR",
    "GENERATE_ARRAY",
    "GENERATE_UUID",
    "IF",
    "IFNULL",
    "NULLIF",
];

pub(super) fn eval_array_util_fn(name: &str, args: Vec<BqValue>) -> BqResult<Option<BqValue>> {
    match name {
        "COALESCE" => Ok(Some(
            args.into_iter()
                .find(|v| !v.is_null())
                .unwrap_or(BqValue::Null),
        )),
        "IFNULL" => eval_ifnull(args).map(Some),
        "NULLIF" => eval_nullif(args).map(Some),
        "IF" => eval_if(args).map(Some),
        "GENERATE_UUID" => Ok(Some(BqValue::String(uuid::Uuid::new_v4().to_string()))),
        "ERROR" => eval_error_fn(args),
        "ARRAY_LENGTH" => eval_array_length(args).map(Some),
        "ARRAY_TO_STRING" => eval_array_to_string(args).map(Some),
        "ARRAY_CONCAT" => eval_array_concat(args).map(Some),
        "ARRAY_REVERSE" => eval_array_reverse(args).map(Some),
        "ARRAY_FIRST" => eval_array_first_last(args, true).map(Some),
        "ARRAY_LAST" => eval_array_first_last(args, false).map(Some),
        "ARRAY_SLICE" => eval_array_slice(args).map(Some),
        "GENERATE_ARRAY" => eval_generate_array(args).map(Some),
        _ => Ok(None),
    }
}

fn eval_error_fn(args: Vec<BqValue>) -> BqResult<Option<BqValue>> {
    let [msg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("ERROR expects 1 argument"))?;
    Err(BqError::semantic(msg.to_string()))
}

fn eval_ifnull(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [a, b]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("IFNULL expects 2 arguments"))?;
    Ok(if a.is_null() { b } else { a })
}

fn eval_nullif(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [a, b]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("NULLIF expects 2 arguments"))?;
    if a.is_null() || b.is_null() {
        return Ok(a);
    }
    if values_equal(&a, &b)? {
        Ok(BqValue::Null)
    } else {
        Ok(a)
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only BOOL conditions are valid in IF"
)]
fn eval_if(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [cond, t_val, f_val]: [BqValue; 3] = args
        .try_into()
        .map_err(|_| BqError::query("IF expects 3 arguments"))?;
    let ty = common_supertype(&[t_val.infer_type()?, f_val.infer_type()?])?;
    let picked = match cond {
        BqValue::Bool(true) => t_val,
        BqValue::Bool(false) | BqValue::Null => f_val,
        other => {
            return Err(BqError::query(format!(
                "IF condition must be BOOL, got {}",
                other.infer_type()?
            )))
        }
    };
    if picked.is_null() {
        Ok(BqValue::Null)
    } else {
        cast_value(picked, &ty)
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only ARRAY values are valid for ARRAY_LENGTH"
)]
fn eval_array_length(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arr]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("ARRAY_LENGTH expects 1 argument"))?;
    match arr {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::Array(items) => Ok(BqValue::Int64(
            i64::try_from(items.len()).unwrap_or(i64::MAX),
        )),
        other => Err(BqError::query(format!(
            "ARRAY_LENGTH expects ARRAY, got {}",
            other.infer_type()?
        ))),
    }
}

fn eval_array_to_string(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(BqError::query("ARRAY_TO_STRING expects 2 or 3 arguments"));
    }
    let mut it = args.into_iter();
    let arr_v = it.next().unwrap_or(BqValue::Null);
    let delim_v = it.next().unwrap_or(BqValue::Null);
    let null_rep = match it.next() {
        None => None,
        Some(BqValue::Null) => return Ok(BqValue::Null),
        Some(BqValue::String(s)) => Some(s),
        Some(other) => {
            return Err(BqError::query(format!(
                "ARRAY_TO_STRING null_text expects STRING, got {}",
                other.infer_type()?
            )))
        }
    };
    if arr_v.is_null() || delim_v.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::Array(items), BqValue::String(delim)) = (arr_v, delim_v) else {
        return Err(BqError::query("ARRAY_TO_STRING expects (ARRAY, STRING)"));
    };
    let mut parts = Vec::new();
    for item in items {
        if item.is_null() {
            if let Some(ref rep) = null_rep {
                parts.push(rep.clone());
            }
        } else {
            parts.push(item.to_string());
        }
    }
    Ok(BqValue::String(parts.join(&delim)))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only ARRAY and NULL are valid for ARRAY_CONCAT"
)]
fn eval_array_concat(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.is_empty() {
        return Err(BqError::query("ARRAY_CONCAT expects at least 1 argument"));
    }
    let mut out = Vec::new();
    for arg in args {
        match arg {
            BqValue::Null => return Ok(BqValue::Null),
            BqValue::Array(items) => out.extend(items),
            _ => return Err(BqError::query("ARRAY_CONCAT expects ARRAY arguments")),
        }
    }
    Ok(BqValue::Array(out))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only ARRAY and NULL are valid for ARRAY_REVERSE"
)]
fn eval_array_reverse(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arr]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("ARRAY_REVERSE expects 1 argument"))?;
    match arr {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::Array(mut items) => {
            items.reverse();
            Ok(BqValue::Array(items))
        }
        _ => Err(BqError::query("ARRAY_REVERSE expects ARRAY")),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only ARRAY and NULL are valid for ARRAY_FIRST/LAST"
)]
fn eval_array_first_last(args: Vec<BqValue>, is_first: bool) -> BqResult<BqValue> {
    let [arr]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("ARRAY_FIRST/LAST expects 1 argument"))?;
    match arr {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::Array(items) => {
            let picked = if is_first {
                items.into_iter().next()
            } else {
                items.into_iter().last()
            };
            picked.ok_or_else(|| BqError::semantic("ARRAY_FIRST/LAST called on empty array"))
        }
        _ => Err(BqError::query("ARRAY_FIRST/LAST expects ARRAY")),
    }
}

fn eval_array_slice(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arr_v, start_v, end_v]: [BqValue; 3] = args
        .try_into()
        .map_err(|_| BqError::query("ARRAY_SLICE expects 3 arguments"))?;
    if arr_v.is_null() || start_v.is_null() || end_v.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::Array(items), BqValue::Int64(s), BqValue::Int64(e)) = (arr_v, start_v, end_v)
    else {
        return Err(BqError::query("ARRAY_SLICE expects (ARRAY, INT64, INT64)"));
    };
    let len = i64::try_from(items.len()).unwrap_or(i64::MAX);
    let norm = |idx: i64| -> usize {
        let resolved = if idx < 0 { len + idx } else { idx };
        usize::try_from(resolved.clamp(0, len)).unwrap_or(0)
    };
    let lo = norm(s);
    let hi = norm(e);
    let sliced = if lo <= hi {
        items
            .get(lo..=hi.min(items.len().saturating_sub(1)))
            .unwrap_or(&[])
            .to_vec()
    } else {
        Vec::new()
    };
    Ok(BqValue::Array(sliced))
}

fn eval_generate_array(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(BqError::query("GENERATE_ARRAY expects 2 or 3 arguments"));
    }
    let mut it = args.into_iter();
    let start_v = it.next().unwrap_or(BqValue::Null);
    let end_v = it.next().unwrap_or(BqValue::Null);
    let step_v = it.next();
    if start_v.is_null() || end_v.is_null() || step_v.as_ref().is_some_and(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    let (BqValue::Int64(start), BqValue::Int64(end)) = (start_v, end_v) else {
        return Err(BqError::query("GENERATE_ARRAY expects INT64 bounds"));
    };
    let step = match step_v {
        Some(BqValue::Int64(s)) => s,
        None => 1,
        _ => return Err(BqError::query("GENERATE_ARRAY step must be INT64")),
    };
    if step == 0 {
        return Err(BqError::semantic("GENERATE_ARRAY step cannot be 0"));
    }
    let mut out = Vec::new();
    let mut cur = start;
    while ((step > 0 && cur <= end) || (step < 0 && cur >= end)) && out.len() < 100_000 {
        out.push(BqValue::Int64(cur));
        match cur.checked_add(step) {
            Some(next) => cur = next,
            None => break,
        }
    }
    Ok(BqValue::Array(out))
}
