use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{BqError, BqResult};

/// A named or anonymous field inside a `BigQuery` `STRUCT`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StructField {
    pub name: Option<String>,
    pub ty: BqType,
}

impl StructField {
    #[must_use]
    pub fn named(name: impl Into<String>, ty: BqType) -> Self {
        Self {
            name: Some(name.into()),
            ty,
        }
    }

    #[must_use]
    pub const fn anonymous(ty: BqType) -> Self {
        Self { name: None, ty }
    }
}

/// Allowed element types for `BigQuery` `RANGE<T>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RangeElementType {
    Date,
    Datetime,
    Timestamp,
}

impl RangeElementType {
    #[must_use]
    pub const fn to_bq_type(self) -> BqType {
        match self {
            Self::Date => BqType::Date,
            Self::Datetime => BqType::Datetime,
            Self::Timestamp => BqType::Timestamp,
        }
    }
}

/// Complete `BigQuery` Standard SQL (`GoogleSQL`) type system.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BqType {
    UntypedNull,
    Bool,
    Int64,
    Float64,
    Numeric,
    BigNumeric,
    String,
    Bytes,
    Date,
    Datetime,
    Time,
    Timestamp,
    Interval,
    Json,
    Geography,
    Range(RangeElementType),
    Array(Box<Self>),
    Struct(Vec<StructField>),
}

impl BqType {
    /// Constructs an `ARRAY<T>` type, enforcing `BigQuery`'s rule forbidding nested `ARRAY<ARRAY<T>>`.
    pub fn array(elem: Self) -> BqResult<Self> {
        if matches!(elem, Self::Array(_)) {
            return Err(BqError::query(
                "Arrays of arrays are not supported in BigQuery; wrap inner ARRAY in a STRUCT",
            ));
        }
        Ok(Self::Array(Box::new(elem)))
    }

    /// Constructs a `STRUCT<...>` type.
    #[must_use]
    pub const fn structure(fields: Vec<StructField>) -> Self {
        Self::Struct(fields)
    }

    /// Constructs a `RANGE<T>` type if `elem` is `DATE`, `DATETIME`, or `TIMESTAMP`.
    pub fn range(elem: &Self) -> BqResult<Self> {
        match elem {
            Self::Date => Ok(Self::Range(RangeElementType::Date)),
            Self::Datetime => Ok(Self::Range(RangeElementType::Datetime)),
            Self::Timestamp => Ok(Self::Range(RangeElementType::Timestamp)),
            Self::UntypedNull
            | Self::Bool
            | Self::Int64
            | Self::Float64
            | Self::Numeric
            | Self::BigNumeric
            | Self::String
            | Self::Bytes
            | Self::Time
            | Self::Interval
            | Self::Json
            | Self::Geography
            | Self::Range(_)
            | Self::Array(_)
            | Self::Struct(_) => Err(BqError::query(format!(
                "RANGE element type must be DATE, DATETIME, or TIMESTAMP, got {elem}"
            ))),
        }
    }

    /// `BigQuery` REST API v2 schema `type` string.
    pub const fn rest_type_name(&self) -> &'static str {
        match self {
            Self::UntypedNull | Self::String => "STRING",
            Self::Bool => "BOOLEAN",
            Self::Int64 => "INTEGER",
            Self::Float64 => "FLOAT",
            Self::Numeric => "NUMERIC",
            Self::BigNumeric => "BIGNUMERIC",
            Self::Bytes => "BYTES",
            Self::Date => "DATE",
            Self::Datetime => "DATETIME",
            Self::Time => "TIME",
            Self::Timestamp => "TIMESTAMP",
            Self::Interval => "INTERVAL",
            Self::Json => "JSON",
            Self::Geography => "GEOGRAPHY",
            Self::Range(_) => "RANGE",
            Self::Array(inner) => inner.rest_type_name(),
            Self::Struct(_) => "RECORD",
        }
    }

    /// Returns true if the type is a numeric scalar (`INT64`, `NUMERIC`, `BIGNUMERIC`, `FLOAT64`).
    pub const fn is_numeric(&self) -> bool {
        matches!(
            self,
            Self::Int64 | Self::Numeric | Self::BigNumeric | Self::Float64
        )
    }

    /// Returns true if the type is orderable in `ORDER BY`, `<`, `>`, etc.
    pub const fn is_orderable(&self) -> bool {
        !matches!(
            self,
            Self::Json | Self::Geography | Self::Array(_) | Self::Struct(_) | Self::Range(_)
        )
    }
}

impl fmt::Display for BqType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool => f.write_str("BOOL"),
            Self::UntypedNull | Self::Int64 => f.write_str("INT64"),
            Self::Float64 => f.write_str("FLOAT64"),
            Self::Numeric => f.write_str("NUMERIC"),
            Self::BigNumeric => f.write_str("BIGNUMERIC"),
            Self::String => f.write_str("STRING"),
            Self::Bytes => f.write_str("BYTES"),
            Self::Date => f.write_str("DATE"),
            Self::Datetime => f.write_str("DATETIME"),
            Self::Time => f.write_str("TIME"),
            Self::Timestamp => f.write_str("TIMESTAMP"),
            Self::Interval => f.write_str("INTERVAL"),
            Self::Json => f.write_str("JSON"),
            Self::Geography => f.write_str("GEOGRAPHY"),
            Self::Range(elem) => write!(f, "RANGE<{}>", elem.to_bq_type()),
            Self::Array(inner) => write!(f, "ARRAY<{inner}>"),
            Self::Struct(fields) => fmt_struct_type(f, fields),
        }
    }
}

fn fmt_struct_type(f: &mut fmt::Formatter<'_>, fields: &[StructField]) -> fmt::Result {
    f.write_str("STRUCT<")?;
    for (idx, field) in fields.iter().enumerate() {
        if idx > 0 {
            f.write_str(", ")?;
        }
        match &field.name {
            Some(name) => write!(f, "{name} {}", field.ty)?,
            None => write!(f, "{}", field.ty)?,
        }
    }
    f.write_str(">")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_of_array_is_rejected() {
        let inner = BqType::array(BqType::Int64).unwrap();
        let err = BqType::array(inner).unwrap_err();
        assert_eq!(err.kind, crate::error::BqErrorKind::Query);
    }

    #[test]
    fn array_of_struct_of_array_is_allowed() {
        let inner_arr = BqType::array(BqType::String).unwrap();
        let wrapper = BqType::structure(vec![StructField::named("items", inner_arr)]);
        let outer = BqType::array(wrapper).unwrap();
        assert_eq!(outer.to_string(), "ARRAY<STRUCT<items ARRAY<STRING>>>");
        assert_eq!(outer.rest_type_name(), "RECORD");
    }

    #[test]
    fn range_only_accepts_date_datetime_timestamp() {
        assert!(BqType::range(&BqType::Date).is_ok());
        assert!(BqType::range(&BqType::Datetime).is_ok());
        assert!(BqType::range(&BqType::Timestamp).is_ok());
        let err = BqType::range(&BqType::Int64).unwrap_err();
        assert_eq!(err.kind, crate::error::BqErrorKind::Query);
    }
}
