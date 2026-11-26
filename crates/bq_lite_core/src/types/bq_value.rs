use std::fmt;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{BqError, BqResult};

use super::{BqBigNumeric, BqNumeric, BqType, RangeElementType, StructField};

/// `BigQuery` `INTERVAL` value (months, days, microseconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BqInterval {
    pub months: i32,
    pub days: i32,
    pub micros: i64,
}

impl BqInterval {
    #[must_use]
    pub const fn new(months: i32, days: i32, micros: i64) -> Self {
        Self {
            months,
            days,
            micros,
        }
    }
}

impl fmt::Display for BqInterval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let years = self.months / 12;
        let rem_months = self.months % 12;
        let total_secs = self.micros / 1_000_000;
        let rem_micros = (self.micros % 1_000_000).abs();
        let hours = total_secs / 3600;
        let mins = (total_secs % 3600) / 60;
        let secs = total_secs % 60;
        write!(
            f,
            "{years}-{rem_months} {} {hours}:{:02}:{:02}.{rem_micros:06}",
            self.days,
            mins.abs(),
            secs.abs()
        )
    }
}

/// `BigQuery` `RANGE<T>` value with optional inclusive lower bound and exclusive upper bound.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BqRange {
    pub elem_type: RangeElementType,
    pub start: Option<BqValue>,
    pub end: Option<BqValue>,
}

impl BqRange {
    pub fn new(
        elem_type: RangeElementType,
        start: Option<BqValue>,
        end: Option<BqValue>,
    ) -> BqResult<Self> {
        let expected = elem_type.to_bq_type();
        if let Some(ref s) = start {
            if s.infer_type()? != expected {
                return Err(BqError::query(format!(
                    "RANGE start type mismatch: expected {expected}"
                )));
            }
        }
        if let Some(ref e) = end {
            if e.infer_type()? != expected {
                return Err(BqError::query(format!(
                    "RANGE end type mismatch: expected {expected}"
                )));
            }
        }
        Ok(Self {
            elem_type,
            start,
            end,
        })
    }
}

impl fmt::Display for BqRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let start_s = self
            .start
            .as_ref()
            .map_or_else(|| "UNBOUNDED".to_owned(), ToString::to_string);
        let end_s = self
            .end
            .as_ref()
            .map_or_else(|| "UNBOUNDED".to_owned(), ToString::to_string);
        write!(f, "[{start_s}, {end_s})")
    }
}

/// A named or anonymous field in a `STRUCT` runtime value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BqStructFieldValue {
    pub name: Option<String>,
    pub value: BqValue,
}

/// Runtime value for every `BigQuery` Standard SQL type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BqValue {
    Null,
    Bool(bool),
    Int64(i64),
    Float64(f64),
    Numeric(BqNumeric),
    BigNumeric(Box<BqBigNumeric>),
    String(String),
    Bytes(Vec<u8>),
    Date(NaiveDate),
    Datetime(NaiveDateTime),
    Time(NaiveTime),
    Timestamp(DateTime<Utc>),
    Interval(BqInterval),
    Json(Box<serde_json::Value>),
    Geography(String),
    Range(Box<BqRange>),
    Array(Vec<Self>),
    Struct(Vec<BqStructFieldValue>),
}

impl BqValue {
    #[must_use]
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Infers the most specific [`BqType`] of this runtime value.
    pub fn infer_type(&self) -> BqResult<BqType> {
        match self {
            Self::Null => Ok(BqType::UntypedNull),
            Self::Bool(_) => Ok(BqType::Bool),
            Self::Int64(_) => Ok(BqType::Int64),
            Self::Float64(_) => Ok(BqType::Float64),
            Self::Numeric(_) => Ok(BqType::Numeric),
            Self::BigNumeric(_) => Ok(BqType::BigNumeric),
            Self::String(_) => Ok(BqType::String),
            Self::Bytes(_) => Ok(BqType::Bytes),
            Self::Date(_) => Ok(BqType::Date),
            Self::Datetime(_) => Ok(BqType::Datetime),
            Self::Time(_) => Ok(BqType::Time),
            Self::Timestamp(_) => Ok(BqType::Timestamp),
            Self::Interval(_) => Ok(BqType::Interval),
            Self::Json(_) => Ok(BqType::Json),
            Self::Geography(_) => Ok(BqType::Geography),
            Self::Range(r) => Ok(BqType::Range(r.elem_type)),
            Self::Array(items) => infer_array_type(items),
            Self::Struct(fields) => infer_struct_type(fields),
        }
    }
}

fn infer_array_type(items: &[BqValue]) -> BqResult<BqType> {
    let mut elem_types = Vec::with_capacity(items.len());
    for item in items {
        elem_types.push(item.infer_type()?);
    }
    let elem_ty = if elem_types.is_empty() {
        BqType::UntypedNull
    } else {
        super::coercion::common_supertype(&elem_types)?
    };
    BqType::array(elem_ty)
}

fn infer_struct_type(fields: &[BqStructFieldValue]) -> BqResult<BqType> {
    let mut out = Vec::with_capacity(fields.len());
    for field in fields {
        let ty = field.value.infer_type()?;
        out.push(StructField {
            name: field.name.clone(),
            ty,
        });
    }
    Ok(BqType::Struct(out))
}

impl fmt::Display for BqValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("NULL"),
            Self::Bool(v) => write!(f, "{v}"),
            Self::Int64(v) => write!(f, "{v}"),
            Self::Float64(v) => fmt_float64(f, *v),
            Self::Numeric(v) => write!(f, "{v}"),
            Self::BigNumeric(v) => write!(f, "{v}"),
            Self::String(v) | Self::Geography(v) => f.write_str(v),
            Self::Bytes(v) => f.write_str(&BASE64.encode(v)),
            Self::Date(v) => write!(f, "{}", v.format("%Y-%m-%d")),
            Self::Datetime(v) => write!(f, "{}", v.format("%Y-%m-%d %H:%M:%S%.f")),
            Self::Time(v) => write!(f, "{}", v.format("%H:%M:%S%.f")),
            Self::Timestamp(v) => write!(f, "{}", v.format("%Y-%m-%d %H:%M:%S%.f UTC")),
            Self::Interval(v) => write!(f, "{v}"),
            Self::Json(v) => write!(f, "{v}"),
            Self::Range(v) => write!(f, "{v}"),
            Self::Array(items) => fmt_array(f, items),
            Self::Struct(fields) => fmt_struct(f, fields),
        }
    }
}

fn fmt_float64(f: &mut fmt::Formatter<'_>, v: f64) -> fmt::Result {
    if v.is_nan() {
        f.write_str("NaN")
    } else if v.is_infinite() {
        f.write_str(if v.is_sign_positive() {
            "Infinity"
        } else {
            "-Infinity"
        })
    } else {
        write!(f, "{v}")
    }
}

fn fmt_array(f: &mut fmt::Formatter<'_>, items: &[BqValue]) -> fmt::Result {
    f.write_str("[")?;
    for (idx, item) in items.iter().enumerate() {
        if idx > 0 {
            f.write_str(", ")?;
        }
        write!(f, "{item}")?;
    }
    f.write_str("]")
}

fn fmt_struct(f: &mut fmt::Formatter<'_>, fields: &[BqStructFieldValue]) -> fmt::Result {
    f.write_str("{")?;
    for (idx, field) in fields.iter().enumerate() {
        if idx > 0 {
            f.write_str(", ")?;
        }
        match &field.name {
            Some(name) => write!(f, "{name}: {}", field.value)?,
            None => write!(f, "{}", field.value)?,
        }
    }
    f.write_str("}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_nested_struct_and_array_types() {
        let val = BqValue::Array(vec![BqValue::Struct(vec![
            BqStructFieldValue {
                name: Some("id".to_owned()),
                value: BqValue::Int64(1),
            },
            BqStructFieldValue {
                name: Some("score".to_owned()),
                value: BqValue::Float64(2.5),
            },
        ])]);
        let ty = val.infer_type().unwrap();
        assert_eq!(ty.to_string(), "ARRAY<STRUCT<id INT64, score FLOAT64>>");
    }

    #[test]
    fn array_containing_array_is_rejected_on_infer() {
        let nested = BqValue::Array(vec![BqValue::Array(vec![BqValue::Int64(1)])]);
        assert!(nested.infer_type().is_err());
    }
}
