use bq_lite_core::error::BqResult;
use bq_lite_core::{BqType, BqValue, QueryColumn, QueryResult};
use chrono::NaiveDate;
use serde_json::{json, Value as JsonValue};

/// Generates an Apache Avro JSON record schema matching a [`QueryResult`].
#[must_use]
pub fn query_result_to_avro_schema_json(qr: &QueryResult) -> String {
    let fields: Vec<JsonValue> = qr
        .columns
        .iter()
        .map(|c| {
            json!({
                "name": c.name,
                "type": ["null", bq_type_to_avro_schema(&c.ty)],
                "default": null,
            })
        })
        .collect();
    json!({
        "type": "record",
        "name": "BqLiteRow",
        "fields": fields,
    })
    .to_string()
}

fn bq_type_to_avro_schema(ty: &BqType) -> JsonValue {
    match ty {
        BqType::Bool => json!("boolean"),
        BqType::Int64 => json!("long"),
        BqType::Float64 => json!("double"),
        BqType::Bytes => json!("bytes"),
        BqType::Date => json!({ "type": "int", "logicalType": "date" }),
        BqType::Timestamp => json!({ "type": "long", "logicalType": "timestamp-micros" }),
        BqType::Array(inner) => json!({
            "type": "array",
            "items": ["null", bq_type_to_avro_schema(inner)],
        }),
        BqType::Struct(fields) => {
            let sfields: Vec<JsonValue> = fields
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let fname = f.name.clone().unwrap_or_else(|| format!("f{i}_"));
                    json!({
                        "name": fname,
                        "type": ["null", bq_type_to_avro_schema(&f.ty)],
                        "default": null,
                    })
                })
                .collect();
            json!({
                "type": "record",
                "name": format!("Rec_{}", fields.len()),
                "fields": sfields,
            })
        }
        BqType::UntypedNull
        | BqType::Numeric
        | BqType::BigNumeric
        | BqType::String
        | BqType::Datetime
        | BqType::Time
        | BqType::Interval
        | BqType::Json
        | BqType::Geography
        | BqType::Range(_) => json!("string"),
    }
}

/// Encodes all rows in `qr` into concatenated Avro binary rows matching [`query_result_to_avro_schema_json`].
pub fn query_result_to_avro_rows(qr: &QueryResult) -> BqResult<Vec<u8>> {
    let mut out = Vec::new();
    for row in &qr.rows {
        encode_avro_record(&qr.columns, row, &mut out);
    }
    Ok(out)
}

fn encode_avro_record(cols: &[QueryColumn], row: &[BqValue], buf: &mut Vec<u8>) {
    for (idx, col) in cols.iter().enumerate() {
        let val = row.get(idx).unwrap_or(&BqValue::Null);
        encode_nullable_datum(val, &col.ty, buf);
    }
}

fn encode_nullable_datum(val: &BqValue, ty: &BqType, buf: &mut Vec<u8>) {
    if val.is_null() {
        write_zigzag_i64(0, buf);
        return;
    }
    write_zigzag_i64(1, buf);
    encode_datum(val, ty, buf);
}

fn encode_datum(val: &BqValue, ty: &BqType, buf: &mut Vec<u8>) {
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap_or_default();
    match (ty, val) {
        (BqType::Bool, BqValue::Bool(b)) => buf.push(u8::from(*b)),
        (BqType::Int64, BqValue::Int64(n)) => write_zigzag_i64(*n, buf),
        (BqType::Float64, BqValue::Float64(f)) => buf.extend_from_slice(&f.to_le_bytes()),
        (BqType::Bytes, BqValue::Bytes(b)) => {
            write_zigzag_i64(i64::try_from(b.len()).unwrap_or(0), buf);
            buf.extend_from_slice(b);
        }
        (BqType::Date, BqValue::Date(d)) => {
            let days = (*d - epoch).num_days();
            write_zigzag_i64(days, buf);
        }
        (BqType::Timestamp, BqValue::Timestamp(ts)) => {
            write_zigzag_i64(ts.timestamp_micros(), buf);
        }
        (BqType::Array(inner), BqValue::Array(items)) => {
            if !items.is_empty() {
                write_zigzag_i64(i64::try_from(items.len()).unwrap_or(0), buf);
                for el in items {
                    encode_nullable_datum(el, inner, buf);
                }
            }
            write_zigzag_i64(0, buf);
        }
        (BqType::Struct(fields), BqValue::Struct(svals)) => {
            for (i, sf) in fields.iter().enumerate() {
                let v = svals.get(i).map_or(&BqValue::Null, |fv| &fv.value);
                encode_nullable_datum(v, &sf.ty, buf);
            }
        }
        _ => {
            let s = val.to_string();
            write_zigzag_i64(i64::try_from(s.len()).unwrap_or(0), buf);
            buf.extend_from_slice(s.as_bytes());
        }
    }
}

fn write_zigzag_i64(n: i64, buf: &mut Vec<u8>) {
    let zz = u64::from_le_bytes(((n << 1) ^ (n >> 63)).to_le_bytes());
    write_varint_u64(zz, buf);
}

fn write_varint_u64(mut v: u64, buf: &mut Vec<u8>) {
    loop {
        let byte = u8::try_from(v & 0x7F).unwrap_or(0);
        v >>= 7;
        if v == 0 {
            buf.push(byte);
            break;
        }
        buf.push(byte | 0x80);
    }
}
