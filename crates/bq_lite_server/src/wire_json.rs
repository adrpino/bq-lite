use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use bq_lite_core::catalog::ColumnSchema;
use bq_lite_core::{
    BqBigNumeric, BqError, BqNumeric, BqResult, BqStructFieldValue, BqType, BqValue, QueryColumn,
    StructField,
};
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use serde_json::{json, Map, Value as JsonValue};

/// Serializes `BqError` using Google Cloud's standard `BigQuery` JSON error envelope.
pub fn bq_error_response(err: &BqError) -> Response {
    let status =
        StatusCode::from_u16(err.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = json!({
        "error": {
            "code": err.http_status(),
            "message": err.message,
            "errors": [{
                "message": err.message,
                "domain": "global",
                "reason": err.api_reason(),
            }],
            "status": err.api_status(),
        }
    });
    (status, Json(body)).into_response()
}

/// Encodes `QueryColumn`s into `BigQuery` REST v2 `TableSchema` JSON (`{"fields": [...]}`).
#[must_use]
pub fn encode_schema(cols: &[QueryColumn]) -> JsonValue {
    let fields: Vec<JsonValue> = cols
        .iter()
        .map(|c| encode_field_schema(&c.name, &c.ty, true))
        .collect();
    json!({ "fields": fields })
}

/// Encodes `ColumnSchema`s into `BigQuery` REST v2 `TableSchema` JSON.
#[must_use]
pub fn encode_table_schema(cols: &[ColumnSchema]) -> JsonValue {
    let fields: Vec<JsonValue> = cols
        .iter()
        .map(|c| encode_field_schema(&c.name, &c.ty, c.nullable))
        .collect();
    json!({ "fields": fields })
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Remaining scalar BqTypes have no nested subfields"
)]
fn encode_field_schema(name: &str, ty: &BqType, nullable: bool) -> JsonValue {
    let (mode, inner_ty) = match ty {
        BqType::Array(inner) => ("REPEATED", inner.as_ref()),
        _ => (if nullable { "NULLABLE" } else { "REQUIRED" }, ty),
    };
    let mut obj = Map::new();
    obj.insert("name".to_owned(), json!(name));
    obj.insert("type".to_owned(), json!(inner_ty.rest_type_name()));
    obj.insert("mode".to_owned(), json!(mode));
    if let BqType::Struct(subfields) = inner_ty {
        let children: Vec<JsonValue> = subfields
            .iter()
            .enumerate()
            .map(|(i, sf)| {
                let fname = sf.name.clone().unwrap_or_else(|| format!("f{i}_"));
                encode_field_schema(&fname, &sf.ty, true)
            })
            .collect();
        obj.insert("fields".to_owned(), JsonValue::Array(children));
    }
    JsonValue::Object(obj)
}

/// Encodes rows into `BigQuery` REST v2 `[{"f": [{"v": ...}, ...]}]` format.
#[must_use]
pub fn encode_rows(cols: &[QueryColumn], rows: &[Vec<BqValue>]) -> Vec<JsonValue> {
    rows.iter()
        .map(|row| {
            let cells: Vec<JsonValue> = row
                .iter()
                .enumerate()
                .map(|(i, val)| {
                    let ty = cols.get(i).map(|c| &c.ty);
                    json!({ "v": encode_wire_value(val, ty) })
                })
                .collect();
            json!({ "f": cells })
        })
        .collect()
}

/// Encodes a single `BqValue` into its `BigQuery` REST v2 `f`/`v` wire JSON representation.
#[must_use]
pub fn encode_wire_value(val: &BqValue, ty: Option<&BqType>) -> JsonValue {
    match val {
        BqValue::Null => {
            if matches!(ty, Some(BqType::Array(_))) {
                JsonValue::Array(Vec::new())
            } else {
                JsonValue::Null
            }
        }
        BqValue::Bool(b) => json!(if *b { "true" } else { "false" }),
        BqValue::Int64(n) => json!(n.to_string()),
        BqValue::Float64(f) => {
            if f.is_nan() {
                json!("NaN")
            } else if f.is_infinite() {
                json!(if f.is_sign_positive() {
                    "Infinity"
                } else {
                    "-Infinity"
                })
            } else {
                json!(f.to_string())
            }
        }
        BqValue::Numeric(n) => json!(n.to_string()),
        BqValue::BigNumeric(bn) => json!(bn.to_string()),
        BqValue::String(s) | BqValue::Geography(s) => json!(s),
        BqValue::Bytes(b) => json!(BASE64.encode(b)),
        BqValue::Date(d) => json!(d.format("%Y-%m-%d").to_string()),
        BqValue::Datetime(dt) => json!(dt.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()),
        BqValue::Time(t) => json!(t.format("%H:%M:%S%.6f").to_string()),
        BqValue::Timestamp(ts) => {
            let micros = ts.timestamp_micros();
            let secs = micros.div_euclid(1_000_000);
            let rem = micros.rem_euclid(1_000_000);
            if rem == 0 {
                json!(format!("{secs}"))
            } else {
                let s = format!("{secs}.{rem:06}");
                json!(s.trim_end_matches('0'))
            }
        }
        BqValue::Interval(iv) => json!(iv.to_string()),
        BqValue::Json(j) => json!(j.to_string()),
        BqValue::Range(r) => json!({
            "start": r.start.as_ref().map(|v| encode_wire_value(v, None)),
            "end": r.end.as_ref().map(|v| encode_wire_value(v, None)),
        }),
        BqValue::Array(items) => {
            let elem_ty = match ty {
                Some(BqType::Array(inner)) => Some(inner.as_ref()),
                _ => None,
            };
            let arr: Vec<JsonValue> = items
                .iter()
                .map(|el| json!({ "v": encode_wire_value(el, elem_ty) }))
                .collect();
            JsonValue::Array(arr)
        }
        BqValue::Struct(fields) => {
            let sub_ty = match ty {
                Some(BqType::Struct(sf)) => Some(sf.as_slice()),
                _ => None,
            };
            let cells: Vec<JsonValue> = fields
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let fty = sub_ty.and_then(|s| s.get(i)).map(|sf| &sf.ty);
                    json!({ "v": encode_wire_value(&f.value, fty) })
                })
                .collect();
            json!({ "f": cells })
        }
    }
}

/// Decodes a `BigQuery` REST v2 `f`/`v` wire JSON value back into a `BqValue`.
pub fn decode_wire_fv_value(val: &JsonValue, ty: &BqType) -> BqResult<BqValue> {
    if val.is_null() {
        return Ok(if matches!(ty, BqType::Array(_)) {
            BqValue::Array(Vec::new())
        } else {
            BqValue::Null
        });
    }
    match ty {
        BqType::UntypedNull => Ok(BqValue::Null),
        BqType::Bool => decode_bool(val),
        BqType::Int64 => decode_int64(val),
        BqType::Float64 => decode_float64(val),
        BqType::Numeric => decode_str(val, "NUMERIC")?
            .parse::<BqNumeric>()
            .map(BqValue::Numeric),
        BqType::BigNumeric => decode_str(val, "BIGNUMERIC")?
            .parse::<BqBigNumeric>()
            .map(|v| BqValue::BigNumeric(Box::new(v))),
        BqType::String => Ok(BqValue::String(decode_str(val, "STRING")?)),
        BqType::Geography => Ok(BqValue::Geography(decode_str(val, "GEOGRAPHY")?)),
        BqType::Bytes => {
            let s = decode_str(val, "BYTES")?;
            BASE64
                .decode(s.as_bytes())
                .map(BqValue::Bytes)
                .map_err(|e| BqError::invalid_argument(format!("Invalid base64: {e}")))
        }
        BqType::Date => {
            let s = decode_str(val, "DATE")?;
            NaiveDate::parse_from_str(&s, "%Y-%m-%d")
                .map(BqValue::Date)
                .map_err(|_| BqError::invalid_argument(format!("Invalid DATE '{s}'")))
        }
        BqType::Datetime => {
            let s = decode_str(val, "DATETIME")?;
            NaiveDateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M:%S%.f")
                .or_else(|_| NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S%.f"))
                .map(BqValue::Datetime)
                .map_err(|_| BqError::invalid_argument(format!("Invalid DATETIME '{s}'")))
        }
        BqType::Time => {
            let s = decode_str(val, "TIME")?;
            NaiveTime::parse_from_str(&s, "%H:%M:%S%.f")
                .map(BqValue::Time)
                .map_err(|_| BqError::invalid_argument(format!("Invalid TIME '{s}'")))
        }
        BqType::Timestamp => decode_timestamp(val),
        BqType::Json => {
            let j = if let Some(s) = val.as_str() {
                serde_json::from_str::<JsonValue>(s)
                    .map_err(|e| BqError::invalid_argument(format!("Invalid JSON: {e}")))?
            } else {
                val.clone()
            };
            Ok(BqValue::Json(Box::new(j)))
        }
        BqType::Array(inner) => decode_fv_array(val, inner),
        BqType::Struct(fields) => decode_fv_struct(val, fields),
        BqType::Interval | BqType::Range(_) => Err(BqError::unimplemented(
            "Wire decode for INTERVAL/RANGE is not implemented",
        )),
    }
}

/// Decodes a standard JSON cell from `tabledata.insertAll` (supports raw JSON values as well as strings).
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Remaining scalar BqTypes decode via wire_fv_value"
)]
pub fn decode_insert_all_value(val: &JsonValue, ty: &BqType) -> BqResult<BqValue> {
    if val.is_null() {
        return Ok(if matches!(ty, BqType::Array(_)) {
            BqValue::Array(Vec::new())
        } else {
            BqValue::Null
        });
    }
    match ty {
        BqType::Array(inner) => {
            if val.is_null() {
                return Ok(BqValue::Array(Vec::new()));
            }
            let arr = val.as_array().ok_or_else(|| {
                BqError::invalid_argument("Expected JSON array for REPEATED column")
            })?;
            let items = arr
                .iter()
                .map(|el| decode_insert_all_value(el, inner))
                .collect::<BqResult<Vec<_>>>()?;
            Ok(BqValue::Array(items))
        }
        BqType::Struct(fields) => {
            let obj = val.as_object().ok_or_else(|| {
                BqError::invalid_argument("Expected JSON object for RECORD column")
            })?;
            let mut out = Vec::with_capacity(fields.len());
            for sf in fields {
                let fname = sf.name.as_deref().unwrap_or("");
                let raw = obj
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(fname))
                    .map_or(&JsonValue::Null, |(_, v)| v);
                out.push(BqStructFieldValue {
                    name: sf.name.clone(),
                    value: decode_insert_all_value(raw, &sf.ty)?,
                });
            }
            Ok(BqValue::Struct(out))
        }
        _ => decode_wire_fv_value(val, ty),
    }
}

/// Parses `BigQuery` REST `TableSchema.fields` JSON array into `Vec<ColumnSchema>`.
pub fn decode_schema_fields(fields_json: &[JsonValue]) -> BqResult<Vec<ColumnSchema>> {
    fields_json.iter().map(decode_single_field_schema).collect()
}

fn decode_single_field_schema(val: &JsonValue) -> BqResult<ColumnSchema> {
    let obj = val
        .as_object()
        .ok_or_else(|| BqError::invalid_argument("Schema field must be a JSON object"))?;
    let name = obj
        .get("name")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| BqError::invalid_argument("Schema field missing 'name'"))?;
    let type_str = obj
        .get("type")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| BqError::invalid_argument("Schema field missing 'type'"))?;
    let mode = obj
        .get("mode")
        .and_then(JsonValue::as_str)
        .unwrap_or("NULLABLE");

    let base_ty = match type_str.to_ascii_uppercase().as_str() {
        "STRING" => BqType::String,
        "BYTES" => BqType::Bytes,
        "INTEGER" | "INT64" => BqType::Int64,
        "FLOAT" | "FLOAT64" => BqType::Float64,
        "BOOLEAN" | "BOOL" => BqType::Bool,
        "NUMERIC" => BqType::Numeric,
        "BIGNUMERIC" => BqType::BigNumeric,
        "DATE" => BqType::Date,
        "DATETIME" => BqType::Datetime,
        "TIME" => BqType::Time,
        "TIMESTAMP" => BqType::Timestamp,
        "JSON" => BqType::Json,
        "GEOGRAPHY" => BqType::Geography,
        "RECORD" | "STRUCT" => {
            let sub_json = obj
                .get("fields")
                .and_then(JsonValue::as_array)
                .ok_or_else(|| BqError::invalid_argument("RECORD field missing 'fields'"))?;
            let sub_cols = decode_schema_fields(sub_json)?;
            BqType::Struct(
                sub_cols
                    .into_iter()
                    .map(|c| StructField::named(c.name, c.ty))
                    .collect(),
            )
        }
        other => {
            return Err(BqError::invalid_argument(format!(
                "Unsupported BigQuery field type '{other}'"
            )))
        }
    };

    let final_ty = if mode.eq_ignore_ascii_case("REPEATED") {
        BqType::array(base_ty)?
    } else {
        base_ty
    };
    let mut col = ColumnSchema::new(name, final_ty);
    col.nullable = !mode.eq_ignore_ascii_case("REQUIRED");
    Ok(col)
}

fn decode_bool(val: &JsonValue) -> BqResult<BqValue> {
    if let Some(b) = val.as_bool() {
        return Ok(BqValue::Bool(b));
    }
    if let Some(s) = val.as_str() {
        return match s.to_ascii_lowercase().as_str() {
            "true" => Ok(BqValue::Bool(true)),
            "false" => Ok(BqValue::Bool(false)),
            _ => Err(BqError::invalid_argument(format!("Invalid BOOL '{s}'"))),
        };
    }
    Err(BqError::invalid_argument("Expected BOOL"))
}

fn decode_int64(val: &JsonValue) -> BqResult<BqValue> {
    if let Some(n) = val.as_i64() {
        return Ok(BqValue::Int64(n));
    }
    if let Some(s) = val.as_str() {
        return s
            .parse::<i64>()
            .map(BqValue::Int64)
            .map_err(|_| BqError::invalid_argument(format!("Invalid INT64 '{s}'")));
    }
    Err(BqError::invalid_argument("Expected INT64"))
}

fn decode_float64(val: &JsonValue) -> BqResult<BqValue> {
    if let Some(f) = val.as_f64() {
        return Ok(BqValue::Float64(f));
    }
    if let Some(s) = val.as_str() {
        return s
            .parse::<f64>()
            .map(BqValue::Float64)
            .map_err(|_| BqError::invalid_argument(format!("Invalid FLOAT64 '{s}'")));
    }
    Err(BqError::invalid_argument("Expected FLOAT64"))
}

fn decode_str(val: &JsonValue, label: &str) -> BqResult<String> {
    val.as_str()
        .map(str::to_owned)
        .ok_or_else(|| BqError::invalid_argument(format!("Expected string for {label}")))
}

fn decode_timestamp(val: &JsonValue) -> BqResult<BqValue> {
    if let Some(secs) = val.as_i64() {
        return DateTime::from_timestamp(secs, 0)
            .map(BqValue::Timestamp)
            .ok_or_else(|| BqError::invalid_argument("TIMESTAMP out of range"));
    }
    if let Some(f) = val.as_f64() {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "Converting BigQuery epoch seconds float to integer microseconds"
        )]
        let micros = (f * 1_000_000.0).round() as i64;
        return DateTime::from_timestamp_micros(micros)
            .map(BqValue::Timestamp)
            .ok_or_else(|| BqError::invalid_argument("TIMESTAMP out of range"));
    }
    let s = decode_str(val, "TIMESTAMP")?;
    if let Ok(dt) = DateTime::parse_from_rfc3339(&s) {
        return Ok(BqValue::Timestamp(dt.with_timezone(&Utc)));
    }
    let trimmed = s.trim();
    let no_utc = trimmed
        .strip_suffix(" UTC")
        .or_else(|| trimmed.strip_suffix(" utc"))
        .unwrap_or(trimmed);
    for fmt in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
    ] {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(no_utc, fmt) {
            return Ok(BqValue::Timestamp(
                DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc),
            ));
        }
    }
    let f = s
        .parse::<f64>()
        .map_err(|_| BqError::invalid_argument(format!("Invalid TIMESTAMP '{s}'")))?;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Converting BigQuery epoch seconds float to integer microseconds"
    )]
    let micros = (f * 1_000_000.0).round() as i64;
    DateTime::from_timestamp_micros(micros)
        .map(BqValue::Timestamp)
        .ok_or_else(|| BqError::invalid_argument("TIMESTAMP out of range"))
}

fn decode_fv_array(val: &JsonValue, inner: &BqType) -> BqResult<BqValue> {
    let arr = val
        .as_array()
        .ok_or_else(|| BqError::invalid_argument("Expected JSON array"))?;
    let mut out = Vec::with_capacity(arr.len());
    for el in arr {
        let cell = el.get("v").unwrap_or(el);
        out.push(decode_wire_fv_value(cell, inner)?);
    }
    Ok(BqValue::Array(out))
}

fn decode_fv_struct(val: &JsonValue, fields: &[StructField]) -> BqResult<BqValue> {
    let cells = val
        .get("f")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| BqError::invalid_argument("Expected STRUCT {'f': [...]} envelope"))?;
    let mut out = Vec::with_capacity(fields.len());
    for (sf, cell) in fields.iter().zip(cells.iter()) {
        let inner_v = cell.get("v").unwrap_or(&JsonValue::Null);
        out.push(BqStructFieldValue {
            name: sf.name.clone(),
            value: decode_wire_fv_value(inner_v, &sf.ty)?,
        });
    }
    Ok(BqValue::Struct(out))
}
