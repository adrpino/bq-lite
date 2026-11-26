use arrow_array::{
    Array, ArrayRef, BinaryArray, BooleanArray, Date32Array, Date64Array, Decimal128Array,
    Decimal256Array, Float32Array, Float64Array, Int16Array, Int32Array, Int64Array, Int8Array,
    LargeBinaryArray, LargeListArray, LargeStringArray, ListArray, StringArray, StructArray,
    Time32MillisecondArray, Time32SecondArray, Time64MicrosecondArray, Time64NanosecondArray,
    TimestampMicrosecondArray, TimestampMillisecondArray, TimestampNanosecondArray,
    TimestampSecondArray, UInt16Array, UInt32Array, UInt64Array, UInt8Array,
};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use num_bigint::BigInt;

use crate::error::{BqError, BqResult};
use crate::types::{BqBigNumeric, BqNumeric, BqStructFieldValue, BqType, BqValue, StructField};

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Remaining scalar Arrow types extract via StringArray"
)]
pub(super) fn extract_arrow_cell(arr: &dyn Array, idx: usize, ty: &BqType) -> BqResult<BqValue> {
    if arr.is_null(idx) {
        return Ok(BqValue::Null);
    }
    match ty {
        BqType::Bool => Ok(arr
            .as_any()
            .downcast_ref::<BooleanArray>()
            .map_or(BqValue::Null, |a| BqValue::Bool(a.value(idx)))),
        BqType::Int64 => extract_int_cell(arr, idx),
        BqType::Float64 => Ok(extract_float_cell(arr, idx)),
        BqType::Numeric => extract_numeric_cell(arr, idx),
        BqType::BigNumeric => extract_bignumeric_cell(arr, idx),
        BqType::Bytes => Ok(extract_bytes_cell(arr, idx)),
        BqType::Date => Ok(extract_date_cell(arr, idx)),
        BqType::Time => Ok(extract_time_cell(arr, idx)),
        BqType::Datetime => Ok(extract_ts_or_dt_cell(arr, idx, false)),
        BqType::Timestamp => Ok(extract_ts_or_dt_cell(arr, idx, true)),
        BqType::Array(inner) => extract_list_cell(arr, idx, inner),
        BqType::Struct(fields) => extract_struct_cell(arr, idx, fields),
        _ => Ok(arr
            .as_any()
            .downcast_ref::<StringArray>()
            .map(|a| BqValue::String(a.value(idx).to_owned()))
            .or_else(|| {
                arr.as_any()
                    .downcast_ref::<LargeStringArray>()
                    .map(|a| BqValue::String(a.value(idx).to_owned()))
            })
            .unwrap_or(BqValue::Null)),
    }
}

fn extract_int_cell(arr: &dyn Array, idx: usize) -> BqResult<BqValue> {
    let any = arr.as_any();
    if let Some(a) = any.downcast_ref::<Int64Array>() {
        return Ok(BqValue::Int64(a.value(idx)));
    }
    if let Some(a) = any.downcast_ref::<Int32Array>() {
        return Ok(BqValue::Int64(i64::from(a.value(idx))));
    }
    if let Some(a) = any.downcast_ref::<Int16Array>() {
        return Ok(BqValue::Int64(i64::from(a.value(idx))));
    }
    if let Some(a) = any.downcast_ref::<Int8Array>() {
        return Ok(BqValue::Int64(i64::from(a.value(idx))));
    }
    if let Some(a) = any.downcast_ref::<UInt32Array>() {
        return Ok(BqValue::Int64(i64::from(a.value(idx))));
    }
    if let Some(a) = any.downcast_ref::<UInt16Array>() {
        return Ok(BqValue::Int64(i64::from(a.value(idx))));
    }
    if let Some(a) = any.downcast_ref::<UInt8Array>() {
        return Ok(BqValue::Int64(i64::from(a.value(idx))));
    }
    if let Some(a) = any.downcast_ref::<UInt64Array>() {
        let v = i64::try_from(a.value(idx))
            .map_err(|_| BqError::semantic("Arrow UInt64 exceeds INT64"))?;
        return Ok(BqValue::Int64(v));
    }
    Ok(BqValue::Null)
}

fn extract_float_cell(arr: &dyn Array, idx: usize) -> BqValue {
    let any = arr.as_any();
    if let Some(a) = any.downcast_ref::<Float64Array>() {
        return BqValue::Float64(a.value(idx));
    }
    if let Some(a) = any.downcast_ref::<Float32Array>() {
        return BqValue::Float64(f64::from(a.value(idx)));
    }
    BqValue::Null
}

fn extract_numeric_cell(arr: &dyn Array, idx: usize) -> BqResult<BqValue> {
    if let Some(a) = arr.as_any().downcast_ref::<Decimal128Array>() {
        let scale = a.scale();
        let raw = a.value(idx);
        let scaled = rescale_i128(raw, scale, 9)?;
        return BqNumeric::from_scaled(scaled).map(BqValue::Numeric);
    }
    if let Some(a) = arr.as_any().downcast_ref::<StringArray>() {
        return a.value(idx).parse::<BqNumeric>().map(BqValue::Numeric);
    }
    Ok(BqValue::Null)
}

fn rescale_i128(raw: i128, from_scale: i8, to_scale: i8) -> BqResult<i128> {
    let diff = i32::from(to_scale) - i32::from(from_scale);
    if diff == 0 {
        return Ok(raw);
    }
    if diff > 0 {
        let factor = 10_i128
            .checked_pow(u32::try_from(diff).unwrap_or(0))
            .ok_or_else(|| BqError::semantic("Decimal scale overflow"))?;
        raw.checked_mul(factor)
            .ok_or_else(|| BqError::semantic("Decimal overflow"))
    } else {
        let factor = 10_i128
            .checked_pow(u32::try_from(-diff).unwrap_or(0))
            .ok_or_else(|| BqError::semantic("Decimal scale overflow"))?;
        Ok(raw / factor)
    }
}

fn extract_bignumeric_cell(arr: &dyn Array, idx: usize) -> BqResult<BqValue> {
    if let Some(a) = arr.as_any().downcast_ref::<Decimal256Array>() {
        let raw = BigInt::from_signed_bytes_le(&a.value(idx).to_le_bytes());
        let scale = a.scale();
        let diff = 38_i32 - i32::from(scale);
        let adjusted = if diff >= 0 {
            raw * BigInt::from(10_u8).pow(u32::try_from(diff).unwrap_or(0))
        } else {
            raw / BigInt::from(10_u8).pow(u32::try_from(-diff).unwrap_or(0))
        };
        return BqBigNumeric::from_scaled(adjusted).map(|v| BqValue::BigNumeric(Box::new(v)));
    }
    if let Some(a) = arr.as_any().downcast_ref::<StringArray>() {
        return a
            .value(idx)
            .parse::<BqBigNumeric>()
            .map(|v| BqValue::BigNumeric(Box::new(v)));
    }
    Ok(BqValue::Null)
}

fn extract_bytes_cell(arr: &dyn Array, idx: usize) -> BqValue {
    let any = arr.as_any();
    if let Some(a) = any.downcast_ref::<BinaryArray>() {
        return BqValue::Bytes(a.value(idx).to_vec());
    }
    if let Some(a) = any.downcast_ref::<LargeBinaryArray>() {
        return BqValue::Bytes(a.value(idx).to_vec());
    }
    BqValue::Null
}

fn extract_date_cell(arr: &dyn Array, idx: usize) -> BqValue {
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap_or_default();
    if let Some(a) = arr.as_any().downcast_ref::<Date32Array>() {
        return epoch
            .checked_add_signed(Duration::days(i64::from(a.value(idx))))
            .map_or(BqValue::Null, BqValue::Date);
    }
    if let Some(a) = arr.as_any().downcast_ref::<Date64Array>() {
        return DateTime::<Utc>::from_timestamp_millis(a.value(idx))
            .map_or(BqValue::Null, |dt| BqValue::Date(dt.date_naive()));
    }
    BqValue::Null
}

fn extract_time_cell(arr: &dyn Array, idx: usize) -> BqValue {
    let any = arr.as_any();
    let micros = if let Some(a) = any.downcast_ref::<Time64MicrosecondArray>() {
        a.value(idx)
    } else if let Some(a) = any.downcast_ref::<Time64NanosecondArray>() {
        a.value(idx) / 1_000
    } else if let Some(a) = any.downcast_ref::<Time32MillisecondArray>() {
        i64::from(a.value(idx)) * 1_000
    } else if let Some(a) = any.downcast_ref::<Time32SecondArray>() {
        i64::from(a.value(idx)) * 1_000_000
    } else if let Some(a) = any.downcast_ref::<StringArray>() {
        return NaiveTime::parse_from_str(a.value(idx), "%H:%M:%S%.f")
            .map_or(BqValue::Null, BqValue::Time);
    } else {
        return BqValue::Null;
    };
    let secs = u32::try_from(micros.div_euclid(1_000_000)).unwrap_or(0);
    let nanos = u32::try_from(micros.rem_euclid(1_000_000) * 1_000).unwrap_or(0);
    NaiveTime::from_num_seconds_from_midnight_opt(secs, nanos).map_or(BqValue::Null, BqValue::Time)
}

fn extract_ts_or_dt_cell(arr: &dyn Array, idx: usize, is_utc_ts: bool) -> BqValue {
    let any = arr.as_any();
    let micros = if let Some(a) = any.downcast_ref::<TimestampMicrosecondArray>() {
        a.value(idx)
    } else if let Some(a) = any.downcast_ref::<TimestampMillisecondArray>() {
        a.value(idx) * 1_000
    } else if let Some(a) = any.downcast_ref::<TimestampSecondArray>() {
        a.value(idx) * 1_000_000
    } else if let Some(a) = any.downcast_ref::<TimestampNanosecondArray>() {
        a.value(idx) / 1_000
    } else if let Some(a) = any.downcast_ref::<StringArray>() {
        return NaiveDateTime::parse_from_str(a.value(idx), "%Y-%m-%d %H:%M:%S%.f")
            .or_else(|_| NaiveDateTime::parse_from_str(a.value(idx), "%Y-%m-%dT%H:%M:%S%.f"))
            .map_or(BqValue::Null, BqValue::Datetime);
    } else {
        return BqValue::Null;
    };
    let Some(dt) = DateTime::<Utc>::from_timestamp_micros(micros) else {
        return BqValue::Null;
    };
    if is_utc_ts {
        BqValue::Timestamp(dt)
    } else {
        BqValue::Datetime(dt.naive_utc())
    }
}

fn extract_list_cell(arr: &dyn Array, idx: usize, inner: &BqType) -> BqResult<BqValue> {
    let sub: ArrayRef = if let Some(list_arr) = arr.as_any().downcast_ref::<ListArray>() {
        list_arr.value(idx)
    } else if let Some(list_arr) = arr.as_any().downcast_ref::<LargeListArray>() {
        list_arr.value(idx)
    } else {
        return Ok(BqValue::Null);
    };
    let mut items = Vec::with_capacity(sub.len());
    for sub_i in 0..sub.len() {
        items.push(extract_arrow_cell(sub.as_ref(), sub_i, inner)?);
    }
    Ok(BqValue::Array(items))
}

fn extract_struct_cell(arr: &dyn Array, idx: usize, fields: &[StructField]) -> BqResult<BqValue> {
    let Some(sa) = arr.as_any().downcast_ref::<StructArray>() else {
        return Ok(BqValue::Null);
    };
    let mut out = Vec::with_capacity(fields.len());
    for (col_i, sf) in fields.iter().enumerate() {
        let val = if let Some(child) = sa.columns().get(col_i) {
            extract_arrow_cell(child.as_ref(), idx, &sf.ty)?
        } else {
            BqValue::Null
        };
        out.push(BqStructFieldValue {
            name: sf.name.clone(),
            value: val,
        });
    }
    Ok(BqValue::Struct(out))
}
