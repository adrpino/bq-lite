use std::collections::BTreeMap;

use bq_lite_core::{BqError, BqResult, BqStructFieldValue, BqType, BqValue, StructField};
use serde_json::Value as JsonValue;

use crate::wire_json::decode_wire_fv_value;

pub(super) type QueryParamPair = (BTreeMap<String, BqValue>, Vec<BqValue>);

pub(super) fn parse_query_params(body: &JsonValue) -> BqResult<QueryParamPair> {
    let mut named = BTreeMap::new();
    let mut pos = Vec::new();
    let Some(params) = body.get("queryParameters").and_then(JsonValue::as_array) else {
        return Ok((named, pos));
    };
    for p in params {
        let ty_json = p.get("parameterType").unwrap_or(&JsonValue::Null);
        let bq_ty = parse_param_type(ty_json)?;
        let val_json = p.get("parameterValue").unwrap_or(&JsonValue::Null);
        let val = decode_param_value(val_json, &bq_ty)?;
        if let Some(name) = p.get("name").and_then(JsonValue::as_str) {
            if !name.is_empty() {
                named.insert(name.to_owned(), val);
                continue;
            }
        }
        pos.push(val);
    }
    Ok((named, pos))
}

fn parse_param_type(ty_json: &JsonValue) -> BqResult<BqType> {
    let ptype = ty_json
        .get("type")
        .and_then(JsonValue::as_str)
        .unwrap_or("STRING");
    match ptype.to_ascii_uppercase().as_str() {
        "INT64" | "INTEGER" => Ok(BqType::Int64),
        "FLOAT64" | "FLOAT" => Ok(BqType::Float64),
        "BOOL" | "BOOLEAN" => Ok(BqType::Bool),
        "NUMERIC" => Ok(BqType::Numeric),
        "BIGNUMERIC" => Ok(BqType::BigNumeric),
        "BYTES" => Ok(BqType::Bytes),
        "DATE" => Ok(BqType::Date),
        "DATETIME" => Ok(BqType::Datetime),
        "TIME" => Ok(BqType::Time),
        "TIMESTAMP" => Ok(BqType::Timestamp),
        "JSON" => Ok(BqType::Json),
        "GEOGRAPHY" => Ok(BqType::Geography),
        "ARRAY" => {
            let elem = ty_json
                .get("arrayType")
                .map(parse_param_type)
                .transpose()?
                .unwrap_or(BqType::String);
            BqType::array(elem)
        }
        "STRUCT" | "RECORD" => {
            let mut fields = Vec::new();
            if let Some(st_fields) = ty_json.get("structTypes").and_then(JsonValue::as_array) {
                for sf in st_fields {
                    let fname = sf.get("name").and_then(JsonValue::as_str);
                    let fty = sf
                        .get("type")
                        .map(parse_param_type)
                        .transpose()?
                        .unwrap_or(BqType::String);
                    fields.push(match fname {
                        Some(n) => StructField::named(n, fty),
                        None => StructField::anonymous(fty),
                    });
                }
            }
            Ok(BqType::Struct(fields))
        }
        _ => Ok(BqType::String),
    }
}

fn decode_param_value(val_json: &JsonValue, ty: &BqType) -> BqResult<BqValue> {
    match ty {
        BqType::Array(inner) => {
            let Some(items_json) = val_json.get("arrayValues").and_then(JsonValue::as_array) else {
                if val_json.get("value").is_some_and(JsonValue::is_null) || val_json.is_null() {
                    return Ok(BqValue::Null);
                }
                return Ok(BqValue::Array(Vec::new()));
            };
            let mut items = Vec::with_capacity(items_json.len());
            for el in items_json {
                items.push(decode_param_value(el, inner)?);
            }
            Ok(BqValue::Array(items))
        }
        BqType::Struct(fields) => {
            let Some(obj) = val_json.get("structValues").and_then(JsonValue::as_object) else {
                if val_json.get("value").is_some_and(JsonValue::is_null) || val_json.is_null() {
                    return Ok(BqValue::Null);
                }
                return Err(BqError::invalid_argument(
                    "STRUCT parameter missing structValues",
                ));
            };
            let mut out = Vec::with_capacity(fields.len());
            for sf in fields {
                let fname = sf.name.as_deref().unwrap_or("");
                let raw = obj.get(fname).unwrap_or(&JsonValue::Null);
                out.push(BqStructFieldValue {
                    name: sf.name.clone(),
                    value: decode_param_value(raw, &sf.ty)?,
                });
            }
            Ok(BqValue::Struct(out))
        }
        BqType::UntypedNull
        | BqType::Bool
        | BqType::Int64
        | BqType::Float64
        | BqType::Numeric
        | BqType::BigNumeric
        | BqType::String
        | BqType::Bytes
        | BqType::Date
        | BqType::Datetime
        | BqType::Time
        | BqType::Timestamp
        | BqType::Interval
        | BqType::Json
        | BqType::Geography
        | BqType::Range(_) => {
            let raw_val = val_json.get("value").unwrap_or(&JsonValue::Null);
            decode_wire_fv_value(raw_val, ty)
        }
    }
}
