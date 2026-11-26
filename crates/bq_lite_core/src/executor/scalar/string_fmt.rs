use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

use crate::error::{BqError, BqResult};
use crate::types::BqValue;

pub(super) fn eval_format(args: Vec<BqValue>) -> BqResult<BqValue> {
    let mut it = args.into_iter();
    let fmt_val = it
        .next()
        .ok_or_else(|| BqError::query("FORMAT requires at least 1 argument"))?;
    if fmt_val.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::String(fmt_str) = fmt_val else {
        return Err(BqError::query("FORMAT template must be STRING"));
    };
    let rest: Vec<BqValue> = it.collect();
    let mut out = String::new();
    let mut chars = fmt_str.chars();
    let mut arg_idx = 0_usize;
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('s' | 'd' | 'i' | 'u' | 'f' | 'g' | 't' | 'T' | 'p') => {
                let val = rest
                    .get(arg_idx)
                    .ok_or_else(|| BqError::query("Not enough arguments for FORMAT string"))?;
                arg_idx += 1;
                out.push_str(&val.to_string());
            }
            Some(other) => {
                return Err(BqError::semantic(format!(
                    "Unsupported FORMAT specifier '%{other}'"
                )))
            }
            None => return Err(BqError::semantic("Trailing '%' in FORMAT string")),
        }
    }
    Ok(BqValue::String(out))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "TO_BASE64 expects BYTES or NULL"
)]
pub(super) fn eval_to_base64(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("TO_BASE64 expects 1 argument"))?;
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::Bytes(b) => Ok(BqValue::String(BASE64.encode(b))),
        other => Err(BqError::query(format!(
            "TO_BASE64 expects BYTES, got {}",
            other.infer_type()?
        ))),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "FROM_BASE64 expects STRING or NULL"
)]
pub(super) fn eval_from_base64(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("FROM_BASE64 expects 1 argument"))?;
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::String(s) => BASE64
            .decode(s.as_bytes())
            .map(BqValue::Bytes)
            .map_err(|e| BqError::semantic(format!("Invalid base64 string: {e}"))),
        other => Err(BqError::query(format!(
            "FROM_BASE64 expects STRING, got {}",
            other.infer_type()?
        ))),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "TO_HEX expects BYTES or NULL"
)]
pub(super) fn eval_to_hex(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("TO_HEX expects 1 argument"))?;
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::Bytes(b) => Ok(BqValue::String(hex::encode(b))),
        other => Err(BqError::query(format!(
            "TO_HEX expects BYTES, got {}",
            other.infer_type()?
        ))),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "FROM_HEX expects STRING or NULL"
)]
pub(super) fn eval_from_hex(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("FROM_HEX expects 1 argument"))?;
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::String(s) => hex::decode(s.trim())
            .map(BqValue::Bytes)
            .map_err(|e| BqError::semantic(format!("Invalid hex string: {e}"))),
        other => Err(BqError::query(format!(
            "FROM_HEX expects STRING, got {}",
            other.infer_type()?
        ))),
    }
}
