mod bq_type;
mod bq_value;
pub mod coercion;
mod datetime_parse;
mod numeric;

pub use bq_type::{BqType, RangeElementType, StructField};
pub use bq_value::{BqInterval, BqRange, BqStructFieldValue, BqValue};
pub use numeric::{BqBigNumeric, BqNumeric, BIGNUMERIC_SCALE, NUMERIC_SCALE};

pub(crate) fn check_max_6_frac_digits(s: &str) -> crate::error::BqResult<()> {
    datetime_parse::check_max_6_frac_digits(s)
}
