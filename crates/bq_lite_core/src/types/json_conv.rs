use std::fmt::Write as _;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{Map, Number, Value as JsonValue};

use crate::error::{BqError, BqResult};

use super::{BqInterval, BqRange, BqValue};

/// Converts a [`BqValue`] into a [`serde_json::Value`].
/// When `strict` is `true` (`CAST(... AS JSON)`), non-finite `FLOAT64` (`NaN`/`±Infinity`)
/// produces a semantic error (suppressible by `SAFE_CAST`).
/// When `strict` is `false` (`TO_JSON` / `JSON_ARRAY` / `JSON_OBJECT`), non-finite floats
/// serialize as `"NaN"`, `"Infinity"`, or `"-Infinity"`.
#[expect(
    clippy::redundant_pub_crate,
    reason = "Re-exported pub(crate) via types::mod while satisfying unreachable_pub"
)]
pub(crate) fn bq_value_to_json(val: &BqValue, strict: bool) -> BqResult<JsonValue> {
    match val {
        BqValue::Null => Ok(JsonValue::Null),
        BqValue::Bool(b) => Ok(JsonValue::Bool(*b)),
        BqValue::Int64(n) => Ok(JsonValue::Number((*n).into())),
        BqValue::Float64(f) => float_to_json(*f, strict),
        BqValue::Numeric(n) => Ok(decimal_str_to_json(&n.to_string(), n.to_f64())),
        BqValue::BigNumeric(bn) => Ok(decimal_str_to_json(&bn.to_string(), bn.to_f64())),
        BqValue::String(s) | BqValue::Geography(s) => Ok(JsonValue::String(s.clone())),
        BqValue::Bytes(b) => Ok(JsonValue::String(BASE64.encode(b))),
        BqValue::Date(d) => Ok(JsonValue::String(d.format("%Y-%m-%d").to_string())),
        BqValue::Datetime(dt) => Ok(JsonValue::String(format_datetime_iso(*dt))),
        BqValue::Time(t) => Ok(JsonValue::String(t.format("%H:%M:%S%.f").to_string())),
        BqValue::Timestamp(ts) => Ok(JsonValue::String(format_timestamp_rfc3339(*ts))),
        BqValue::Interval(iv) => Ok(JsonValue::String(format_interval_iso8601(*iv))),
        BqValue::Json(j) => Ok((**j).clone()),
        BqValue::Range(r) => range_to_json(r, strict),
        BqValue::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(bq_value_to_json(item, strict)?);
            }
            Ok(JsonValue::Array(out))
        }
        BqValue::Struct(fields) => {
            let mut map = Map::new();
            for f in fields {
                let k = f.name.clone().unwrap_or_default();
                map.insert(k, bq_value_to_json(&f.value, strict)?);
            }
            Ok(JsonValue::Object(map))
        }
    }
}

fn range_to_json(r: &BqRange, strict: bool) -> BqResult<JsonValue> {
    let mut map = Map::new();
    let start_j = match &r.start {
        Some(v) => bq_value_to_json(v, strict)?,
        None => JsonValue::Null,
    };
    let end_j = match &r.end {
        Some(v) => bq_value_to_json(v, strict)?,
        None => JsonValue::Null,
    };
    map.insert("start".to_owned(), start_j);
    map.insert("end".to_owned(), end_j);
    Ok(JsonValue::Object(map))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "Float is verified finite, integral, and within exact IEEE-754 integer range [-2^53, 2^53]"
)]
fn float_to_json(f: f64, strict: bool) -> BqResult<JsonValue> {
    const MAX_EXACT_INT: f64 = 9_007_199_254_740_992.0;
    if !f.is_finite() {
        if strict {
            let label = if f.is_nan() {
                "NaN"
            } else if f.is_sign_positive() {
                "Infinity"
            } else {
                "-Infinity"
            };
            return Err(BqError::semantic(format!(
                "{label} is not a valid JSON number"
            )));
        }
        let s = if f.is_nan() {
            "NaN"
        } else if f.is_sign_positive() {
            "Infinity"
        } else {
            "-Infinity"
        };
        return Ok(JsonValue::String(s.to_owned()));
    }
    if f == 0.0 {
        return Ok(JsonValue::Number(0_i64.into()));
    }
    if f.trunc().to_bits() == f.to_bits() && (-MAX_EXACT_INT..=MAX_EXACT_INT).contains(&f) {
        return Ok(JsonValue::Number((f as i64).into()));
    }
    Ok(Number::from_f64(f).map_or_else(|| JsonValue::String(f.to_string()), JsonValue::Number))
}

fn decimal_str_to_json(raw: &str, as_f64: f64) -> JsonValue {
    if raw.contains('.') {
        let trimmed = raw.trim_end_matches('0');
        if let Some(int_str) = trimmed.strip_suffix('.') {
            if let Ok(n) = int_str.parse::<i64>() {
                return JsonValue::Number(n.into());
            }
        }
    } else if let Ok(n) = raw.parse::<i64>() {
        return JsonValue::Number(n.into());
    }
    Number::from_f64(as_f64).map_or_else(|| JsonValue::String(raw.to_owned()), JsonValue::Number)
}

fn format_timestamp_rfc3339(ts: chrono::DateTime<chrono::Utc>) -> String {
    if ts.timestamp_subsec_nanos() == 0 {
        return ts.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    }
    let raw = ts.format("%Y-%m-%dT%H:%M:%S.%6f").to_string();
    format!("{}Z", raw.trim_end_matches('0').trim_end_matches('.'))
}

fn format_datetime_iso(dt: chrono::NaiveDateTime) -> String {
    if dt.and_utc().timestamp_subsec_nanos() == 0 {
        return dt.format("%Y-%m-%dT%H:%M:%S").to_string();
    }
    let raw = dt.format("%Y-%m-%dT%H:%M:%S.%6f").to_string();
    raw.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn format_interval_iso8601(iv: BqInterval) -> String {
    let mut out = String::from("P");
    let years = iv.months / 12;
    let months = iv.months % 12;
    if years != 0 {
        let _ = write!(out, "{years}Y");
    }
    if months != 0 {
        let _ = write!(out, "{months}M");
    }
    if iv.days != 0 {
        let _ = write!(out, "{}D", iv.days);
    }
    if iv.micros != 0 {
        out.push('T');
        let hours = iv.micros / 3_600_000_000;
        let rem_h = iv.micros % 3_600_000_000;
        let mins = rem_h / 60_000_000;
        let rem_m = rem_h % 60_000_000;
        let secs = rem_m / 1_000_000;
        let us = (rem_m % 1_000_000).abs();
        if hours != 0 {
            let _ = write!(out, "{hours}H");
        }
        if mins != 0 {
            let _ = write!(out, "{mins}M");
        }
        if secs != 0 || us != 0 {
            if us == 0 {
                let _ = write!(out, "{secs}S");
            } else {
                let sign = if rem_m < 0 && secs == 0 { "-" } else { "" };
                let frac_raw = format!("{us:06}");
                let frac = frac_raw.trim_end_matches('0');
                let _ = write!(out, "{sign}{secs}.{frac}S");
            }
        }
    }
    if out == "P" {
        out.push_str("0D");
    }
    out
}
