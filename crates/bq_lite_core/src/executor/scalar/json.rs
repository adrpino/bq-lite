use serde_json::{Map, Value as JsonValue};

use crate::error::{BqError, BqResult};
use crate::types::BqValue;

use super::json_path::{
    format_datetime_iso, format_timestamp_rfc3339, remove_json_path, resolve_json_path,
    set_json_path, write_bq_json_string,
};

pub(super) const JSON_FUNCTIONS: &[&str] = &[
    "BOOL",
    "FLOAT64",
    "INT64",
    "JSON_ARRAY",
    "JSON_EXTRACT",
    "JSON_EXTRACT_ARRAY",
    "JSON_EXTRACT_SCALAR",
    "JSON_EXTRACT_STRING_ARRAY",
    "JSON_KEYS",
    "JSON_OBJECT",
    "JSON_QUERY",
    "JSON_QUERY_ARRAY",
    "JSON_REMOVE",
    "JSON_SET",
    "JSON_TYPE",
    "JSON_VALUE",
    "JSON_VALUE_ARRAY",
    "PARSE_JSON",
    "STRING",
    "TO_JSON",
    "TO_JSON_STRING",
];

pub(super) fn eval_json_fn(name: &str, args: Vec<BqValue>) -> BqResult<Option<BqValue>> {
    match name {
        "PARSE_JSON" => eval_parse_json(&args).map(Some),
        "TO_JSON" => eval_to_json(&args).map(Some),
        "TO_JSON_STRING" => eval_to_json_string(&args).map(Some),
        "JSON_QUERY" => eval_json_query(args, false).map(Some),
        "JSON_EXTRACT" => eval_json_query(args, true).map(Some),
        "JSON_VALUE" => eval_json_value(args, false).map(Some),
        "JSON_EXTRACT_SCALAR" => eval_json_value(args, true).map(Some),
        "JSON_QUERY_ARRAY" => eval_json_array_query(args, false, false).map(Some),
        "JSON_EXTRACT_ARRAY" => eval_json_array_query(args, false, true).map(Some),
        "JSON_VALUE_ARRAY" => eval_json_array_query(args, true, false).map(Some),
        "JSON_EXTRACT_STRING_ARRAY" => eval_json_array_query(args, true, true).map(Some),
        "JSON_TYPE" => eval_json_type(args).map(Some),
        "JSON_OBJECT" => eval_json_object(args).map(Some),
        "JSON_ARRAY" => Ok(Some(BqValue::Json(Box::new(JsonValue::Array(
            args.iter().map(bq_to_json).collect(),
        ))))),
        "JSON_KEYS" => eval_json_keys(&args).map(Some),
        "JSON_SET" | "JSON_REMOVE" => eval_json_mutate(name, args).map(Some),
        "INT64" | "FLOAT64" | "BOOL" | "STRING" => eval_json_typed_accessor(name, args).map(Some),
        _ => Ok(None),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "PARSE_JSON expects STRING or NULL"
)]
fn eval_parse_json(args: &[BqValue]) -> BqResult<BqValue> {
    let Some(first) = args.first() else {
        return Err(BqError::query("PARSE_JSON expects 1 or 2 arguments"));
    };
    match first {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::String(s) => serde_json::from_str::<JsonValue>(s)
            .map(|v| BqValue::Json(Box::new(v)))
            .map_err(|e| BqError::semantic(format!("Invalid JSON: {e}"))),
        _ => Err(BqError::query("PARSE_JSON expects STRING")),
    }
}

fn eval_to_json(args: &[BqValue]) -> BqResult<BqValue> {
    let Some(first) = args.first() else {
        return Err(BqError::query("TO_JSON expects 1 or 2 arguments"));
    };
    Ok(BqValue::Json(Box::new(bq_to_json(first))))
}

fn eval_to_json_string(args: &[BqValue]) -> BqResult<BqValue> {
    let Some(first) = args.first() else {
        return Err(BqError::query("TO_JSON_STRING expects 1 or 2 arguments"));
    };
    let pretty = matches!(args.get(1), Some(BqValue::Bool(true)));
    let mut out = String::new();
    write_bq_json_string(first, pretty, 0, &mut out, &bq_to_json)?;
    Ok(BqValue::String(out))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Remaining BqValue scalar variants serialize via Display string"
)]
fn bq_to_json(val: &BqValue) -> JsonValue {
    match val {
        BqValue::Null => JsonValue::Null,
        BqValue::Bool(b) => JsonValue::Bool(*b),
        BqValue::Int64(n) => JsonValue::Number((*n).into()),
        BqValue::Float64(f) => {
            serde_json::Number::from_f64(*f).map_or(JsonValue::Null, JsonValue::Number)
        }
        BqValue::String(s) | BqValue::Geography(s) => JsonValue::String(s.clone()),
        BqValue::Timestamp(ts) => JsonValue::String(format_timestamp_rfc3339(*ts)),
        BqValue::Datetime(dt) => JsonValue::String(format_datetime_iso(*dt)),
        BqValue::Json(j) => (**j).clone(),
        BqValue::Array(items) => JsonValue::Array(items.iter().map(bq_to_json).collect()),
        BqValue::Struct(fields) => {
            let mut map = Map::new();
            for (idx, f) in fields.iter().enumerate() {
                let k = f.name.clone().unwrap_or_else(|| format!("f{idx}_"));
                map.insert(k, bq_to_json(&f.value));
            }
            JsonValue::Object(map)
        }
        other => JsonValue::String(other.to_string()),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "JSON functions accept JSON or STRING input"
)]
fn extract_json_input(val: &BqValue) -> BqResult<Option<JsonValue>> {
    match val {
        BqValue::Null => Ok(None),
        BqValue::Json(j) => Ok(Some((**j).clone())),
        BqValue::String(s) => serde_json::from_str::<JsonValue>(s)
            .map(Some)
            .map_err(|e| BqError::semantic(format!("Invalid JSON: {e}"))),
        _ => Err(BqError::query("JSON function expects JSON or STRING input")),
    }
}

fn eval_json_query(args: Vec<BqValue>, legacy_brackets: bool) -> BqResult<BqValue> {
    let [doc_v, path_v]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("JSON_QUERY expects 2 arguments"))?;
    let Some(doc) = extract_json_input(&doc_v)? else {
        return Ok(BqValue::Null);
    };
    let BqValue::String(path) = path_v else {
        return if path_v.is_null() {
            Ok(BqValue::Null)
        } else {
            Err(BqError::query("JSONPath must be STRING"))
        };
    };
    let Some(target) = resolve_json_path(&doc, &path, legacy_brackets)? else {
        return Ok(BqValue::Null);
    };
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "Return JSON when input is JSON, STRING otherwise"
    )]
    match doc_v {
        BqValue::Json(_) => Ok(BqValue::Json(Box::new(target.clone()))),
        _ => Ok(BqValue::String(target.to_string())),
    }
}

fn eval_json_value(args: Vec<BqValue>, legacy_brackets: bool) -> BqResult<BqValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(BqError::query("JSON_VALUE expects 1 or 2 arguments"));
    }
    let mut it = args.into_iter();
    let doc_v = it.next().unwrap_or(BqValue::Null);
    let path_v = it.next().unwrap_or_else(|| BqValue::String("$".to_owned()));
    let Some(doc) = extract_json_input(&doc_v)? else {
        return Ok(BqValue::Null);
    };
    let BqValue::String(path) = path_v else {
        return if path_v.is_null() {
            Ok(BqValue::Null)
        } else {
            Err(BqError::query("JSONPath must be STRING"))
        };
    };
    let Some(target) = resolve_json_path(&doc, &path, legacy_brackets)? else {
        return Ok(BqValue::Null);
    };
    Ok(json_scalar_to_string(target).map_or(BqValue::Null, BqValue::String))
}

fn eval_json_array_query(
    args: Vec<BqValue>,
    scalar_only: bool,
    legacy_brackets: bool,
) -> BqResult<BqValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(BqError::query("JSON_*_ARRAY expects 1 or 2 arguments"));
    }
    let mut it = args.into_iter();
    let doc_v = it.next().unwrap_or(BqValue::Null);
    let path_v = it.next().unwrap_or_else(|| BqValue::String("$".to_owned()));
    let Some(doc) = extract_json_input(&doc_v)? else {
        return Ok(BqValue::Null);
    };
    let BqValue::String(path) = path_v else {
        return Ok(BqValue::Null);
    };
    let Some(JsonValue::Array(arr)) = resolve_json_path(&doc, &path, legacy_brackets)? else {
        return Ok(BqValue::Null);
    };
    let items: Vec<BqValue> = arr
        .iter()
        .map(|el| {
            if scalar_only {
                json_scalar_to_string(el).map_or(BqValue::Null, BqValue::String)
            } else if matches!(doc_v, BqValue::Json(_)) {
                BqValue::Json(Box::new(el.clone()))
            } else {
                BqValue::String(el.to_string())
            }
        })
        .collect();
    Ok(BqValue::Array(items))
}

fn json_scalar_to_string(v: &JsonValue) -> Option<String> {
    match v {
        JsonValue::String(s) => Some(s.clone()),
        JsonValue::Number(n) => Some(n.to_string()),
        JsonValue::Bool(b) => Some(b.to_string()),
        JsonValue::Null | JsonValue::Array(_) | JsonValue::Object(_) => None,
    }
}

fn eval_json_type(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("JSON_TYPE expects 1 argument"))?;
    let BqValue::Json(j) = arg else {
        return if arg.is_null() {
            Ok(BqValue::Null)
        } else {
            Err(BqError::query("JSON_TYPE expects JSON"))
        };
    };
    let label = match &*j {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "boolean",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    };
    Ok(BqValue::String(label.to_owned()))
}

fn eval_json_object(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.len() == 2
        && (matches!(args.first(), Some(BqValue::Array(_)))
            || (matches!(args.first(), Some(BqValue::Null))
                && matches!(args.get(1), Some(BqValue::Array(_) | BqValue::Null))))
    {
        let [keys_v, vals_v]: [BqValue; 2] = args
            .try_into()
            .map_err(|_| BqError::query("JSON_OBJECT expects 2 array arguments"))?;
        return eval_json_object_arrays(keys_v, vals_v);
    }
    if args.len() % 2 != 0 {
        return Err(BqError::query(
            "JSON_OBJECT expects an even number of arguments",
        ));
    }
    let mut map = Map::new();
    let mut it = args.into_iter();
    while let (Some(k), Some(v)) = (it.next(), it.next()) {
        if k.is_null() {
            return Err(BqError::semantic("JSON_OBJECT key cannot be NULL"));
        }
        let BqValue::String(key) = k else {
            return Err(BqError::query("JSON_OBJECT keys must be STRING"));
        };
        map.entry(key).or_insert_with(|| bq_to_json(&v));
    }
    Ok(BqValue::Json(Box::new(JsonValue::Object(map))))
}

fn eval_json_object_arrays(keys_v: BqValue, vals_v: BqValue) -> BqResult<BqValue> {
    if keys_v.is_null() || vals_v.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Array(keys) = keys_v else {
        return Err(BqError::query(
            "JSON_OBJECT first argument must be ARRAY<STRING>",
        ));
    };
    let BqValue::Array(vals) = vals_v else {
        return Err(BqError::query("JSON_OBJECT second argument must be ARRAY"));
    };
    if keys.len() != vals.len() {
        return Err(BqError::semantic(
            "JSON_OBJECT key and value arrays must have the same length",
        ));
    }
    let mut map = Map::new();
    for (k, v) in keys.into_iter().zip(vals) {
        if k.is_null() {
            return Err(BqError::semantic("JSON_OBJECT key cannot be NULL"));
        }
        let BqValue::String(key) = k else {
            return Err(BqError::query(
                "JSON_OBJECT key array elements must be STRING",
            ));
        };
        map.entry(key).or_insert_with(|| bq_to_json(&v));
    }
    Ok(BqValue::Json(Box::new(JsonValue::Object(map))))
}

fn eval_json_keys(args: &[BqValue]) -> BqResult<BqValue> {
    let Some(first) = args.first() else {
        return Err(BqError::query("JSON_KEYS expects 1 argument"));
    };
    let BqValue::Json(j) = first else {
        return if first.is_null() {
            Ok(BqValue::Null)
        } else {
            Err(BqError::query("JSON_KEYS expects JSON"))
        };
    };
    let JsonValue::Object(map) = &**j else {
        return Ok(BqValue::Null);
    };
    Ok(BqValue::Array(
        map.keys().map(|k| BqValue::String(k.clone())).collect(),
    ))
}

fn eval_json_mutate(name: &str, args: Vec<BqValue>) -> BqResult<BqValue> {
    let Some(doc_v) = args.first() else {
        return Err(BqError::query(format!(
            "{name} expects at least 2 arguments"
        )));
    };
    let BqValue::Json(doc) = doc_v else {
        return if doc_v.is_null() {
            Ok(BqValue::Null)
        } else {
            Err(BqError::query(format!(
                "{name} expects JSON first argument"
            )))
        };
    };
    let mut cloned = (**doc).clone();
    if name == "JSON_REMOVE" {
        for path_v in args.iter().skip(1) {
            if let BqValue::String(p) = path_v {
                remove_json_path(&mut cloned, p)?;
            }
        }
    } else {
        let mut it = args.into_iter().skip(1);
        while let (Some(p_v), Some(val_v)) = (it.next(), it.next()) {
            if let BqValue::String(p) = p_v {
                set_json_path(&mut cloned, &p, bq_to_json(&val_v))?;
            }
        }
    }
    Ok(BqValue::Json(Box::new(cloned)))
}

fn eval_json_typed_accessor(name: &str, args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query(format!("{name} expects 1 JSON argument")))?;
    let BqValue::Json(j) = arg else {
        return if arg.is_null() {
            Ok(BqValue::Null)
        } else {
            Err(BqError::query(format!("{name} expects JSON")))
        };
    };
    match (name, &*j) {
        ("INT64", JsonValue::Number(n)) => n
            .as_i64()
            .map(BqValue::Int64)
            .ok_or_else(|| BqError::semantic("JSON number out of INT64 range")),
        ("FLOAT64", JsonValue::Number(n)) => n
            .as_f64()
            .map(BqValue::Float64)
            .ok_or_else(|| BqError::semantic("JSON number out of FLOAT64 range")),
        ("BOOL", JsonValue::Bool(b)) => Ok(BqValue::Bool(*b)),
        ("STRING", JsonValue::String(s)) => Ok(BqValue::String(s.clone())),
        _ => Err(BqError::semantic(format!(
            "JSON value cannot be converted to {name}"
        ))),
    }
}
