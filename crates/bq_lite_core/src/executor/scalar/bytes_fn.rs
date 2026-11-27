use std::cmp::Ordering;

use crate::error::{BqError, BqResult};
use crate::types::BqValue;

pub(super) const BYTES_EXTRA_FUNCTIONS: &[&str] = &[
    "ASCII",
    "BIT_COUNT",
    "CODE_POINTS_TO_BYTES",
    "CODE_POINTS_TO_STRING",
    "OCTET_LENGTH",
    "TO_CODE_POINTS",
    "UNICODE",
];

pub(super) fn eval_bytes_extra_fn(name: &str, args: &[BqValue]) -> BqResult<Option<BqValue>> {
    match name {
        "OCTET_LENGTH" => eval_octet_length(args).map(Some),
        "ASCII" => eval_ascii(args).map(Some),
        "BIT_COUNT" => eval_bit_count(args).map(Some),
        "UNICODE" => eval_unicode(args).map(Some),
        "TO_CODE_POINTS" => eval_to_code_points(args).map(Some),
        "CODE_POINTS_TO_STRING" => eval_code_points_to_string(args).map(Some),
        "CODE_POINTS_TO_BYTES" => eval_code_points_to_bytes(args).map(Some),
        _ => Ok(None),
    }
}

pub(super) fn eval_reverse(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("REVERSE expects 1 argument"))?;
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "REVERSE accepts STRING, BYTES, or NULL"
    )]
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::String(s) => Ok(BqValue::String(s.chars().rev().collect())),
        BqValue::Bytes(mut b) => {
            b.reverse();
            Ok(BqValue::Bytes(b))
        }
        other => Err(BqError::query(format!(
            "REVERSE expects STRING or BYTES, got {}",
            other.infer_type()?
        ))),
    }
}

pub(super) fn eval_safe_convert_bytes_to_string(args: &[BqValue]) -> BqResult<BqValue> {
    let [arg] = args else {
        return Err(BqError::query(
            "SAFE_CONVERT_BYTES_TO_STRING expects 1 argument",
        ));
    };
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "SAFE_CONVERT_BYTES_TO_STRING accepts BYTES or NULL"
    )]
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::Bytes(b) => Ok(BqValue::String(String::from_utf8_lossy(b).into_owned())),
        other => Err(BqError::query(format!(
            "SAFE_CONVERT_BYTES_TO_STRING expects BYTES, got {}",
            other.infer_type()?
        ))),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "CONCAT on BYTES accepts BYTES or NULL"
)]
pub(super) fn eval_bytes_concat(args: Vec<BqValue>) -> BqResult<BqValue> {
    let mut out = Vec::new();
    for a in args {
        match a {
            BqValue::Null => return Ok(BqValue::Null),
            BqValue::Bytes(b) => out.extend_from_slice(&b),
            other => {
                return Err(BqError::query(format!(
                    "CONCAT expects BYTES, got {}",
                    other.infer_type()?
                )))
            }
        }
    }
    Ok(BqValue::Bytes(out))
}

pub(super) fn eval_bytes_substr(
    bytes: &[u8],
    pos: i64,
    len_v: Option<&BqValue>,
) -> BqResult<BqValue> {
    let total = bytes.len();
    let start = match pos.cmp(&0) {
        Ordering::Greater => usize::try_from(pos - 1).unwrap_or(usize::MAX).min(total),
        Ordering::Less => {
            total.saturating_sub(usize::try_from(pos.unsigned_abs()).unwrap_or(usize::MAX))
        }
        Ordering::Equal => 0,
    };
    let take = match len_v {
        Some(BqValue::Int64(n)) if *n < 0 => {
            return Err(BqError::semantic("SUBSTR length cannot be negative"))
        }
        Some(BqValue::Int64(n)) => usize::try_from(*n).unwrap_or(usize::MAX),
        Some(_) => return Err(BqError::query("SUBSTR length must be INT64")),
        None => total,
    };
    let end = start.saturating_add(take).min(total);
    Ok(BqValue::Bytes(
        bytes.get(start..end).unwrap_or(&[]).to_vec(),
    ))
}

pub(super) fn eval_bytes_strpos(hay: &[u8], needle: &[u8]) -> BqValue {
    if needle.is_empty() {
        return BqValue::Int64(1);
    }
    let pos = hay
        .windows(needle.len())
        .position(|w| w == needle)
        .map_or(0, |idx| i64::try_from(idx + 1).unwrap_or(i64::MAX));
    BqValue::Int64(pos)
}

pub(super) fn eval_bytes_split(bytes: &[u8], sep: Option<BqValue>) -> BqResult<BqValue> {
    let delim = match sep {
        Some(BqValue::Bytes(d)) => d,
        Some(_) => return Err(BqError::query("SPLIT delimiter for BYTES must be BYTES")),
        None => b",".to_vec(),
    };
    if delim.is_empty() {
        return Ok(BqValue::Array(
            bytes.iter().map(|&b| BqValue::Bytes(vec![b])).collect(),
        ));
    }
    let mut parts = Vec::new();
    let mut start = 0_usize;
    while start <= bytes.len() {
        let rest = bytes.get(start..).unwrap_or(&[]);
        if let Some(rel) = rest
            .windows(delim.len())
            .position(|w| w == delim.as_slice())
        {
            parts.push(BqValue::Bytes(
                bytes.get(start..start + rel).unwrap_or(&[]).to_vec(),
            ));
            start += rel + delim.len();
        } else {
            parts.push(BqValue::Bytes(rest.to_vec()));
            break;
        }
    }
    Ok(BqValue::Array(parts))
}

pub(super) fn eval_bytes_trim(
    bytes: &[u8],
    chars_v: Option<BqValue>,
    left: bool,
    right: bool,
) -> BqResult<BqValue> {
    let Some(BqValue::Bytes(cset)) = chars_v else {
        return Err(BqError::query(
            "TRIM/LTRIM/RTRIM on BYTES requires BYTES trim set",
        ));
    };
    let mut start = 0_usize;
    let mut end = bytes.len();
    if left {
        while start < end && bytes.get(start).is_some_and(|b| cset.contains(b)) {
            start += 1;
        }
    }
    if right {
        while end > start && bytes.get(end - 1).is_some_and(|b| cset.contains(b)) {
            end -= 1;
        }
    }
    Ok(BqValue::Bytes(
        bytes.get(start..end).unwrap_or(&[]).to_vec(),
    ))
}

pub(super) fn eval_bytes_pad(
    bytes: &[u8],
    target: i64,
    pad_v: Option<BqValue>,
    left: bool,
) -> BqResult<BqValue> {
    if target < 0 {
        return Err(BqError::semantic("LPAD/RPAD length cannot be negative"));
    }
    let pad = match pad_v {
        Some(BqValue::Bytes(p)) => p,
        Some(_) => return Err(BqError::query("LPAD/RPAD pattern for BYTES must be BYTES")),
        None => vec![b' '],
    };
    if pad.is_empty() {
        return Err(BqError::semantic("LPAD/RPAD pattern cannot be empty"));
    }
    let target_len = usize::try_from(target).unwrap_or(usize::MAX);
    if bytes.len() >= target_len {
        return Ok(BqValue::Bytes(
            bytes.get(..target_len).unwrap_or(&[]).to_vec(),
        ));
    }
    let needed = target_len - bytes.len();
    let pad_bytes: Vec<u8> = pad.iter().copied().cycle().take(needed).collect();
    let mut out = Vec::with_capacity(target_len);
    if left {
        out.extend_from_slice(&pad_bytes);
        out.extend_from_slice(bytes);
    } else {
        out.extend_from_slice(bytes);
        out.extend_from_slice(&pad_bytes);
    }
    Ok(BqValue::Bytes(out))
}

pub(super) fn eval_bytes_replace(src: &[u8], from: &[u8], to: &[u8]) -> BqValue {
    if from.is_empty() {
        return BqValue::Bytes(src.to_vec());
    }
    let mut out = Vec::with_capacity(src.len());
    let mut idx = 0_usize;
    while idx < src.len() {
        let rest = src.get(idx..).unwrap_or(&[]);
        if rest.starts_with(from) {
            out.extend_from_slice(to);
            idx += from.len();
        } else if let Some(&b) = src.get(idx) {
            out.push(b);
            idx += 1;
        }
    }
    BqValue::Bytes(out)
}

pub(super) fn eval_bytes_repeat(bytes: &[u8], cnt: i64) -> BqResult<BqValue> {
    const MAX_OUT: usize = 1_048_576;
    if cnt < 0 {
        return Err(BqError::semantic("REPEAT count cannot be negative"));
    }
    let times = usize::try_from(cnt).unwrap_or(usize::MAX);
    if bytes.len().saturating_mul(times) > MAX_OUT {
        return Err(BqError::semantic("REPEAT output size exceeds limit"));
    }
    Ok(BqValue::Bytes(bytes.repeat(times)))
}

pub(super) fn eval_bytes_left_right(bytes: &[u8], cnt: i64, is_left: bool) -> BqResult<BqValue> {
    if cnt < 0 {
        return Err(BqError::semantic("LEFT/RIGHT length cannot be negative"));
    }
    let k = usize::try_from(cnt).unwrap_or(usize::MAX);
    let slice = if is_left {
        bytes.get(..k.min(bytes.len())).unwrap_or(&[])
    } else {
        let start = bytes.len().saturating_sub(k);
        bytes.get(start..).unwrap_or(&[])
    };
    Ok(BqValue::Bytes(slice.to_vec()))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "OCTET_LENGTH accepts STRING, BYTES, or NULL"
)]
fn eval_octet_length(args: &[BqValue]) -> BqResult<BqValue> {
    let [arg] = args else {
        return Err(BqError::query("OCTET_LENGTH expects 1 argument"));
    };
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::String(s) => Ok(BqValue::Int64(i64::try_from(s.len()).unwrap_or(i64::MAX))),
        BqValue::Bytes(b) => Ok(BqValue::Int64(i64::try_from(b.len()).unwrap_or(i64::MAX))),
        other => Err(BqError::query(format!(
            "OCTET_LENGTH expects STRING or BYTES, got {}",
            other.infer_type()?
        ))),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "ASCII accepts STRING, BYTES, or NULL"
)]
fn eval_ascii(args: &[BqValue]) -> BqResult<BqValue> {
    let [arg] = args else {
        return Err(BqError::query("ASCII expects 1 argument"));
    };
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::String(s) => {
            let b = s.as_bytes().first().copied().unwrap_or(0);
            if b > 127 {
                return Err(BqError::semantic(
                    "ASCII argument must start with an ASCII character",
                ));
            }
            Ok(BqValue::Int64(i64::from(b)))
        }
        BqValue::Bytes(b) => Ok(BqValue::Int64(i64::from(b.first().copied().unwrap_or(0)))),
        other => Err(BqError::query(format!(
            "ASCII expects STRING or BYTES, got {}",
            other.infer_type()?
        ))),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "BIT_COUNT accepts INT64, BYTES, or NULL"
)]
fn eval_bit_count(args: &[BqValue]) -> BqResult<BqValue> {
    let [arg] = args else {
        return Err(BqError::query("BIT_COUNT expects 1 argument"));
    };
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::Int64(n) => Ok(BqValue::Int64(i64::from(n.count_ones()))),
        BqValue::Bytes(b) => {
            let ones: u64 = b.iter().map(|byte| u64::from(byte.count_ones())).sum();
            Ok(BqValue::Int64(i64::try_from(ones).unwrap_or(i64::MAX)))
        }
        BqValue::String(s) => {
            let ones: u64 = s
                .as_bytes()
                .iter()
                .map(|byte| u64::from(byte.count_ones()))
                .sum();
            Ok(BqValue::Int64(i64::try_from(ones).unwrap_or(i64::MAX)))
        }
        other => Err(BqError::query(format!(
            "BIT_COUNT expects INT64 or BYTES, got {}",
            other.infer_type()?
        ))),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "UNICODE accepts STRING or NULL"
)]
fn eval_unicode(args: &[BqValue]) -> BqResult<BqValue> {
    let [arg] = args else {
        return Err(BqError::query("UNICODE expects 1 argument"));
    };
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::String(s) => Ok(BqValue::Int64(
            s.chars().next().map_or(0, |c| i64::from(u32::from(c))),
        )),
        other => Err(BqError::query(format!(
            "UNICODE expects STRING, got {}",
            other.infer_type()?
        ))),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "TO_CODE_POINTS accepts STRING, BYTES, or NULL"
)]
fn eval_to_code_points(args: &[BqValue]) -> BqResult<BqValue> {
    let [arg] = args else {
        return Err(BqError::query("TO_CODE_POINTS expects 1 argument"));
    };
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::String(s) => Ok(BqValue::Array(
            s.chars()
                .map(|c| BqValue::Int64(i64::from(u32::from(c))))
                .collect(),
        )),
        BqValue::Bytes(b) => Ok(BqValue::Array(
            b.iter()
                .map(|&byte| BqValue::Int64(i64::from(byte)))
                .collect(),
        )),
        other => Err(BqError::query(format!(
            "TO_CODE_POINTS expects STRING or BYTES, got {}",
            other.infer_type()?
        ))),
    }
}

fn eval_code_points_to_string(args: &[BqValue]) -> BqResult<BqValue> {
    let [arg] = args else {
        return Err(BqError::query("CODE_POINTS_TO_STRING expects 1 argument"));
    };
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Array(items) = arg else {
        return Err(BqError::query("CODE_POINTS_TO_STRING expects ARRAY<INT64>"));
    };
    let mut out = String::with_capacity(items.len());
    for item in items {
        if item.is_null() {
            return Ok(BqValue::Null);
        }
        let BqValue::Int64(cp) = item else {
            return Err(BqError::query(
                "CODE_POINTS_TO_STRING expects INT64 elements",
            ));
        };
        let u = u32::try_from(*cp)
            .ok()
            .and_then(char::from_u32)
            .ok_or_else(|| BqError::semantic(format!("Invalid Unicode code point: {cp}")))?;
        out.push(u);
    }
    Ok(BqValue::String(out))
}

fn eval_code_points_to_bytes(args: &[BqValue]) -> BqResult<BqValue> {
    let [arg] = args else {
        return Err(BqError::query("CODE_POINTS_TO_BYTES expects 1 argument"));
    };
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Array(items) = arg else {
        return Err(BqError::query("CODE_POINTS_TO_BYTES expects ARRAY<INT64>"));
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if item.is_null() {
            return Ok(BqValue::Null);
        }
        let BqValue::Int64(cp) = item else {
            return Err(BqError::query(
                "CODE_POINTS_TO_BYTES expects INT64 elements",
            ));
        };
        let b = u8::try_from(*cp).map_err(|_| {
            BqError::semantic(format!("Byte code point out of range [0, 255]: {cp}"))
        })?;
        out.push(b);
    }
    Ok(BqValue::Bytes(out))
}
