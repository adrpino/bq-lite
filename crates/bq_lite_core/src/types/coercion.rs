use chrono::{DateTime, NaiveDate, Utc};

use crate::error::{BqError, BqResult};

use super::datetime_parse::{parse_naive_datetime, parse_time_str, parse_timestamp_str};
use super::{BqBigNumeric, BqNumeric, BqStructFieldValue, BqType, BqValue, StructField};

/// Returns `true` if `from` can implicitly coerce to `to` under `GoogleSQL` rules.
#[must_use]
pub fn can_implicitly_coerce(from: &BqType, to: &BqType) -> bool {
    if from == to || matches!(from, BqType::UntypedNull) {
        return true;
    }
    match (from, to) {
        (BqType::Int64, BqType::Numeric | BqType::BigNumeric | BqType::Float64)
        | (BqType::Numeric, BqType::BigNumeric | BqType::Float64)
        | (BqType::BigNumeric | BqType::Date, BqType::Float64 | BqType::Datetime) => {
            matches!((from, to), (BqType::Date, BqType::Datetime)) || to.is_numeric()
        }
        (BqType::Array(a), BqType::Array(b)) => can_implicitly_coerce(a, b),
        (BqType::Struct(fa), BqType::Struct(fb)) => can_coerce_struct(fa, fb),
        _ => false,
    }
}

fn can_coerce_struct(from: &[StructField], to: &[StructField]) -> bool {
    from.len() == to.len()
        && from
            .iter()
            .zip(to.iter())
            .all(|(a, b)| can_implicitly_coerce(&a.ty, &b.ty))
}

/// Computes the common `GoogleSQL` supertype of a slice of types.
pub fn common_supertype(types: &[BqType]) -> BqResult<BqType> {
    let mut acc = BqType::UntypedNull;
    for ty in types {
        acc = supertype_pair(&acc, ty)?;
    }
    Ok(acc)
}

fn supertype_pair(a: &BqType, b: &BqType) -> BqResult<BqType> {
    if a == b || matches!(b, BqType::UntypedNull) {
        return Ok(a.clone());
    }
    if matches!(a, BqType::UntypedNull) {
        return Ok(b.clone());
    }
    if let Some(num) = numeric_supertype(a, b) {
        return Ok(num);
    }
    match (a, b) {
        (BqType::Date, BqType::Datetime) | (BqType::Datetime, BqType::Date) => Ok(BqType::Datetime),
        (BqType::Array(ea), BqType::Array(eb)) => BqType::array(supertype_pair(ea, eb)?),
        (BqType::Struct(fa), BqType::Struct(fb)) => struct_supertype(fa, fb),
        _ => Err(BqError::query(format!(
            "No common supertype between {a} and {b}"
        ))),
    }
}

const fn numeric_supertype(a: &BqType, b: &BqType) -> Option<BqType> {
    match (a, b) {
        (BqType::Float64, other) | (other, BqType::Float64) if other.is_numeric() => {
            Some(BqType::Float64)
        }
        (BqType::BigNumeric, other) | (other, BqType::BigNumeric) if other.is_numeric() => {
            Some(BqType::BigNumeric)
        }
        (BqType::Numeric, other) | (other, BqType::Numeric) if other.is_numeric() => {
            Some(BqType::Numeric)
        }
        _ => None,
    }
}

fn struct_supertype(a: &[StructField], b: &[StructField]) -> BqResult<BqType> {
    if a.len() != b.len() {
        return Err(BqError::query(
            "Cannot find common supertype for STRUCTs with different field counts",
        ));
    }
    let mut fields = Vec::with_capacity(a.len());
    for (fa, fb) in a.iter().zip(b.iter()) {
        let ty = supertype_pair(&fa.ty, &fb.ty)?;
        let name = fa.name.clone().or_else(|| fb.name.clone());
        fields.push(StructField { name, ty });
    }
    Ok(BqType::Struct(fields))
}

/// Implicitly coerces `val` to `target` if permitted by `GoogleSQL` coercion rules.
pub fn coerce_value(val: BqValue, target: &BqType) -> BqResult<BqValue> {
    if val.is_null() {
        return Ok(BqValue::Null);
    }
    let source = val.infer_type()?;
    if &source == target || matches!(target, BqType::UntypedNull) {
        return Ok(val);
    }
    if !can_implicitly_coerce(&source, target) {
        return Err(BqError::query(format!(
            "Cannot implicitly coerce {source} to {target}"
        )));
    }
    cast_value(val, target)
}

/// Explicit `GoogleSQL` `CAST(val AS target)`. Runtime failures yield
/// [`BqErrorKind::Semantic`](crate::error::BqErrorKind::Semantic) so `SAFE_CAST` returns `NULL`.
pub fn cast_value(val: BqValue, target: &BqType) -> BqResult<BqValue> {
    if val.is_null() || matches!(target, BqType::UntypedNull) {
        return Ok(BqValue::Null);
    }
    match target {
        BqType::Bool => cast_to_bool(val),
        BqType::Int64 => cast_to_int64(val),
        BqType::Float64 => cast_to_float64(val),
        BqType::Numeric => cast_to_numeric(val),
        BqType::BigNumeric => cast_to_bignumeric(val),
        BqType::String => cast_to_string(val),
        BqType::Bytes => cast_to_bytes(val),
        BqType::Date => cast_to_date(val),
        BqType::Datetime => cast_to_datetime(val),
        BqType::Time => cast_to_time(val),
        BqType::Timestamp => cast_to_timestamp(val),
        BqType::Json => cast_to_json(&val),
        BqType::Interval => cast_to_interval(val),
        BqType::Array(elem) => cast_to_array(val, elem),
        BqType::Struct(fields) => cast_to_struct(val, fields),
        BqType::UntypedNull | BqType::Geography | BqType::Range(_) => Err(BqError::query(format!(
            "Unsupported CAST target type: {target}"
        ))),
    }
}

fn cast_to_string(val: BqValue) -> BqResult<BqValue> {
    if let BqValue::Bytes(b) = val {
        return String::from_utf8(b)
            .map(BqValue::String)
            .map_err(|_| BqError::semantic("Invalid cast of BYTES to UTF-8 STRING"));
    }
    Ok(BqValue::String(val.to_string()))
}

fn unsupported_cast(val: &BqValue, target: &str) -> BqResult<BqValue> {
    Err(BqError::query(format!(
        "Invalid cast from {} to {target}",
        val.infer_type()?
    )))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Remaining BqValue variants are invalid CAST sources to BOOL"
)]
fn cast_to_bool(val: BqValue) -> BqResult<BqValue> {
    match val {
        BqValue::Bool(b) => Ok(BqValue::Bool(b)),
        BqValue::Int64(n) => Ok(BqValue::Bool(n != 0)),
        BqValue::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" => Ok(BqValue::Bool(true)),
            "false" => Ok(BqValue::Bool(false)),
            _ => Err(BqError::semantic(format!("Invalid BOOL value: '{s}'"))),
        },
        other => unsupported_cast(&other, "BOOL"),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Remaining BqValue variants are invalid CAST sources to INT64"
)]
fn cast_to_int64(val: BqValue) -> BqResult<BqValue> {
    match val {
        BqValue::Int64(n) => Ok(BqValue::Int64(n)),
        BqValue::Bool(b) => Ok(BqValue::Int64(i64::from(b))),
        BqValue::Float64(f) => float_to_int64(f),
        BqValue::Numeric(n) => n
            .to_string()
            .split('.')
            .next()
            .unwrap_or("0")
            .parse::<i64>()
            .map(BqValue::Int64)
            .map_err(|_| BqError::semantic("NUMERIC out of INT64 range")),
        BqValue::String(s) => parse_i64_str(&s),
        other => unsupported_cast(&other, "INT64"),
    }
}

fn parse_i64_str(s: &str) -> BqResult<BqValue> {
    let trimmed = s.trim();
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .or_else(|| trimmed.strip_prefix("+0x"))
        .or_else(|| trimmed.strip_prefix("+0X"))
    {
        return i64::from_str_radix(hex, 16)
            .map(BqValue::Int64)
            .map_err(|_| BqError::semantic(format!("Bad int64 value: '{s}'")));
    }
    if let Some(hex) = trimmed
        .strip_prefix("-0x")
        .or_else(|| trimmed.strip_prefix("-0X"))
    {
        let abs = u64::from_str_radix(hex, 16)
            .map_err(|_| BqError::semantic(format!("Bad int64 value: '{s}'")))?;
        if abs == (i64::MAX as u64) + 1 {
            return Ok(BqValue::Int64(i64::MIN));
        }
        let signed =
            i64::try_from(abs).map_err(|_| BqError::semantic(format!("Bad int64 value: '{s}'")))?;
        return Ok(BqValue::Int64(-signed));
    }
    trimmed
        .parse::<i64>()
        .map(BqValue::Int64)
        .map_err(|_| BqError::semantic(format!("Bad int64 value: '{s}'")))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "Bounds checked explicitly against exact [-2^63, 2^63) range before cast"
)]
fn float_to_int64(f: f64) -> BqResult<BqValue> {
    if !f.is_finite() {
        return Err(BqError::semantic(format!("Cannot cast {f} to INT64")));
    }
    let rounded = f.round_ties_even();
    let min_bound = -(2.0_f64.powi(63));
    let max_bound = 2.0_f64.powi(63);
    if rounded < min_bound || rounded >= max_bound {
        return Err(BqError::semantic(format!(
            "FLOAT64 out of INT64 range: {f}"
        )));
    }
    Ok(BqValue::Int64(rounded as i64))
}

#[expect(
    clippy::cast_precision_loss,
    reason = "BigQuery CAST(INT64 AS FLOAT64) intentionally uses IEEE-754 double conversion"
)]
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Remaining BqValue variants are invalid CAST sources to FLOAT64"
)]
fn cast_to_float64(val: BqValue) -> BqResult<BqValue> {
    match val {
        BqValue::Float64(f) => Ok(BqValue::Float64(f)),
        BqValue::Int64(n) => Ok(BqValue::Float64(n as f64)),
        BqValue::Numeric(n) => Ok(BqValue::Float64(n.to_f64())),
        BqValue::BigNumeric(n) => Ok(BqValue::Float64(n.to_f64())),
        BqValue::String(s) => s
            .trim()
            .parse::<f64>()
            .map(BqValue::Float64)
            .map_err(|_| BqError::semantic(format!("Bad double value: '{s}'"))),
        other => unsupported_cast(&other, "FLOAT64"),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Remaining BqValue variants are invalid CAST sources to NUMERIC"
)]
fn cast_to_numeric(val: BqValue) -> BqResult<BqValue> {
    match val {
        BqValue::Numeric(n) => Ok(BqValue::Numeric(n)),
        BqValue::Int64(n) => Ok(BqValue::Numeric(BqNumeric::from_i64(n))),
        BqValue::Float64(f) => BqNumeric::from_f64(f).map(BqValue::Numeric),
        BqValue::BigNumeric(bn) => bn.to_string().parse().map(BqValue::Numeric),
        BqValue::String(s) => s.parse().map(BqValue::Numeric),
        other => unsupported_cast(&other, "NUMERIC"),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Remaining BqValue variants are invalid CAST sources to BIGNUMERIC"
)]
fn cast_to_bignumeric(val: BqValue) -> BqResult<BqValue> {
    match val {
        BqValue::BigNumeric(bn) => Ok(BqValue::BigNumeric(bn)),
        BqValue::Numeric(n) => Ok(BqValue::BigNumeric(Box::new(BqBigNumeric::from_numeric(n)))),
        BqValue::Int64(n) => Ok(BqValue::BigNumeric(Box::new(BqBigNumeric::from_i64(n)))),
        BqValue::Float64(f) => BqBigNumeric::from_f64(f).map(|v| BqValue::BigNumeric(Box::new(v))),
        BqValue::String(s) => s.parse().map(|v| BqValue::BigNumeric(Box::new(v))),
        other => unsupported_cast(&other, "BIGNUMERIC"),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Remaining BqValue variants are invalid CAST sources to BYTES"
)]
fn cast_to_bytes(val: BqValue) -> BqResult<BqValue> {
    match val {
        BqValue::Bytes(b) => Ok(BqValue::Bytes(b)),
        BqValue::String(s) => Ok(BqValue::Bytes(s.into_bytes())),
        other => unsupported_cast(&other, "BYTES"),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Remaining BqValue variants are invalid CAST sources to DATE"
)]
fn cast_to_date(val: BqValue) -> BqResult<BqValue> {
    match val {
        BqValue::Date(d) => Ok(BqValue::Date(d)),
        BqValue::Datetime(dt) => Ok(BqValue::Date(dt.date())),
        BqValue::Timestamp(ts) => Ok(BqValue::Date(ts.date_naive())),
        BqValue::String(s) => NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
            .map(BqValue::Date)
            .map_err(|_| BqError::semantic(format!("Invalid DATE string: '{s}'"))),
        other => unsupported_cast(&other, "DATE"),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Remaining BqValue variants are invalid CAST sources to DATETIME"
)]
fn cast_to_datetime(val: BqValue) -> BqResult<BqValue> {
    match val {
        BqValue::Datetime(dt) => Ok(BqValue::Datetime(dt)),
        BqValue::Date(d) => Ok(BqValue::Datetime(
            d.and_hms_opt(0, 0, 0).unwrap_or_default(),
        )),
        BqValue::Timestamp(ts) => Ok(BqValue::Datetime(ts.naive_utc())),
        BqValue::String(s) => parse_naive_datetime(&s).map(BqValue::Datetime),
        other => unsupported_cast(&other, "DATETIME"),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Remaining BqValue variants are invalid CAST sources to TIME"
)]
fn cast_to_time(val: BqValue) -> BqResult<BqValue> {
    match val {
        BqValue::Time(t) => Ok(BqValue::Time(t)),
        BqValue::Datetime(dt) => Ok(BqValue::Time(dt.time())),
        BqValue::Timestamp(ts) => Ok(BqValue::Time(ts.time())),
        BqValue::String(s) => parse_time_str(&s).map(BqValue::Time),
        other => unsupported_cast(&other, "TIME"),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Remaining BqValue variants are invalid CAST sources to TIMESTAMP"
)]
fn cast_to_timestamp(val: BqValue) -> BqResult<BqValue> {
    match val {
        BqValue::Timestamp(ts) => Ok(BqValue::Timestamp(ts)),
        BqValue::Date(d) => Ok(BqValue::Timestamp(
            DateTime::<Utc>::from_naive_utc_and_offset(
                d.and_hms_opt(0, 0, 0).unwrap_or_default(),
                Utc,
            ),
        )),
        BqValue::Datetime(dt) => Ok(BqValue::Timestamp(
            DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc),
        )),
        BqValue::String(s) => parse_timestamp_str(&s).map(BqValue::Timestamp),
        other => unsupported_cast(&other, "TIMESTAMP"),
    }
}

fn cast_to_json(val: &BqValue) -> BqResult<BqValue> {
    super::json_conv::bq_value_to_json(val, true).map(|j| BqValue::Json(Box::new(j)))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Remaining BqValue variants are invalid CAST sources to INTERVAL"
)]
fn cast_to_interval(val: BqValue) -> BqResult<BqValue> {
    match val {
        BqValue::Interval(iv) => Ok(BqValue::Interval(iv)),
        BqValue::String(s) => super::interval_parse::parse_interval_cast(&s).map(BqValue::Interval),
        other => unsupported_cast(&other, "INTERVAL"),
    }
}

fn cast_to_array(val: BqValue, elem_target: &BqType) -> BqResult<BqValue> {
    let BqValue::Array(items) = val else {
        return Err(BqError::query(format!(
            "Invalid cast from {} to ARRAY<{elem_target}>",
            val.infer_type()?
        )));
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(cast_value(item, elem_target)?);
    }
    Ok(BqValue::Array(out))
}

fn cast_to_struct(val: BqValue, fields_target: &[StructField]) -> BqResult<BqValue> {
    let BqValue::Struct(fields) = val else {
        return Err(BqError::query("Invalid cast from non-STRUCT to STRUCT"));
    };
    if fields.len() != fields_target.len() {
        return Err(BqError::query(format!(
            "STRUCT field count mismatch: expected {}, got {}",
            fields_target.len(),
            fields.len()
        )));
    }
    let mut out = Vec::with_capacity(fields.len());
    for (src, dst) in fields.into_iter().zip(fields_target.iter()) {
        out.push(BqStructFieldValue {
            name: dst.name.clone().or(src.name),
            value: cast_value(src.value, &dst.ty)?,
        });
    }
    Ok(BqValue::Struct(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::BqErrorKind;

    #[test]
    fn numeric_supertype_lattice() {
        assert_eq!(
            common_supertype(&[BqType::UntypedNull, BqType::Int64, BqType::Numeric]).unwrap(),
            BqType::Numeric
        );
        assert_eq!(
            common_supertype(&[BqType::Int64, BqType::BigNumeric]).unwrap(),
            BqType::BigNumeric
        );
        assert_eq!(
            common_supertype(&[BqType::Numeric, BqType::Float64]).unwrap(),
            BqType::Float64
        );
    }

    #[test]
    fn invalid_string_cast_returns_semantic_error_for_safe_cast() {
        let err =
            cast_value(BqValue::String("not_a_number".to_owned()), &BqType::Int64).unwrap_err();
        assert_eq!(err.kind, BqErrorKind::Semantic);
        assert!(err.is_safe_suppressible());
    }

    #[test]
    fn positional_struct_coercion_relabels_fields() {
        let val = BqValue::Struct(vec![BqStructFieldValue {
            name: None,
            value: BqValue::Int64(42),
        }]);
        let target = BqType::structure(vec![StructField::named("x", BqType::Numeric)]);
        let coerced = coerce_value(val, &target).unwrap();
        assert_eq!(coerced.infer_type().unwrap(), target);
        assert_eq!(coerced.to_string(), "{x: 42}");
    }
}
