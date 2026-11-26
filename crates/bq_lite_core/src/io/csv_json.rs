use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::Value as JsonValue;

use crate::catalog::ColumnSchema;
use crate::error::{BqError, BqResult};
use crate::types::coercion::cast_value;
use crate::types::{BqStructFieldValue, BqType, BqValue};

/// Configurable options for CSV loading.
#[derive(Debug, Clone)]
pub struct CsvLoadOptions {
    pub skip_leading_rows: usize,
    pub field_delimiter: u8,
    pub quote: u8,
    pub null_marker: Option<String>,
    pub allow_jagged_rows: bool,
    pub ignore_unknown_values: bool,
}

impl Default for CsvLoadOptions {
    fn default() -> Self {
        Self {
            skip_leading_rows: 0,
            field_delimiter: b',',
            quote: b'"',
            null_marker: None,
            allow_jagged_rows: false,
            ignore_unknown_values: false,
        }
    }
}

/// Loads rows from CSV bytes matching `columns` (skipping optional header lines).
pub fn load_csv(
    bytes: &[u8],
    columns: &[ColumnSchema],
    skip_leading_rows: usize,
) -> BqResult<Vec<Vec<BqValue>>> {
    let opts = CsvLoadOptions {
        skip_leading_rows,
        ..CsvLoadOptions::default()
    };
    load_csv_with_options(bytes, columns, &opts)
}

/// Loads rows from CSV bytes using custom `BigQuery` load options (`nullMarker`, delimiter, quote).
pub fn load_csv_with_options(
    bytes: &[u8],
    columns: &[ColumnSchema],
    opts: &CsvLoadOptions,
) -> BqResult<Vec<Vec<BqValue>>> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(opts.allow_jagged_rows || opts.ignore_unknown_values)
        .delimiter(opts.field_delimiter)
        .quote(opts.quote)
        .from_reader(bytes);
    let mut rows = Vec::new();
    for (line_idx, rec_res) in rdr.records().enumerate() {
        let record = rec_res.map_err(|e| {
            BqError::invalid_argument(format!("CSV parse error at row {line_idx}: {e}"))
        })?;
        if line_idx < opts.skip_leading_rows {
            continue;
        }
        let too_short = record.len() < columns.len() && !opts.allow_jagged_rows;
        let too_long = record.len() > columns.len() && !opts.ignore_unknown_values;
        if too_short || too_long {
            return Err(BqError::invalid_argument(format!(
                "CSV row {line_idx} column count mismatch: expected {}, got {}",
                columns.len(),
                record.len()
            )));
        }
        let mut row = Vec::with_capacity(columns.len());
        for (col_idx, col) in columns.iter().enumerate() {
            let cell = record.get(col_idx).unwrap_or("");
            let is_null = cell.is_empty()
                || opts
                    .null_marker
                    .as_deref()
                    .is_some_and(|marker| cell == marker);
            if is_null {
                if !col.nullable {
                    return Err(BqError::invalid_argument(format!(
                        "Required column '{}' is empty/null in CSV row {line_idx}",
                        col.name
                    )));
                }
                row.push(BqValue::Null);
            } else {
                let val = cast_value(BqValue::String(cell.to_owned()), &col.ty)?;
                row.push(val);
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

/// Exports rows as CSV bytes with a header row.
pub fn export_csv(columns: &[ColumnSchema], rows: &[Vec<BqValue>]) -> BqResult<Vec<u8>> {
    let mut wtr = csv::WriterBuilder::new().from_writer(Vec::new());
    let headers: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
    wtr.write_record(headers)
        .map_err(|e| BqError::internal(format!("CSV header write failed: {e}")))?;
    for row in rows {
        let cells: Vec<String> = row
            .iter()
            .map(|v| {
                if v.is_null() {
                    String::new()
                } else {
                    v.to_string()
                }
            })
            .collect();
        wtr.write_record(&cells)
            .map_err(|e| BqError::internal(format!("CSV row write failed: {e}")))?;
    }
    wtr.into_inner()
        .map_err(|e| BqError::internal(format!("CSV flush failed: {e}")))
}

/// Loads rows from newline-delimited JSON (`NEWLINE_DELIMITED_JSON`).
pub fn load_ndjson(bytes: &[u8], columns: &[ColumnSchema]) -> BqResult<Vec<Vec<BqValue>>> {
    load_ndjson_with_options(bytes, columns, true)
}

/// Loads rows from NDJSON with optional strict unknown-field checking.
pub fn load_ndjson_with_options(
    bytes: &[u8],
    columns: &[ColumnSchema],
    ignore_unknown_values: bool,
) -> BqResult<Vec<Vec<BqValue>>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| BqError::invalid_argument(format!("Invalid UTF-8 in NDJSON: {e}")))?;
    let mut rows = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let obj_val: JsonValue = serde_json::from_str(trimmed).map_err(|e| {
            BqError::invalid_argument(format!("NDJSON parse error at line {idx}: {e}"))
        })?;
        let obj = obj_val
            .as_object()
            .ok_or_else(|| BqError::invalid_argument("NDJSON line must be a JSON object"))?;
        if !ignore_unknown_values {
            for k in obj.keys() {
                if !columns.iter().any(|c| c.name.eq_ignore_ascii_case(k)) {
                    return Err(BqError::invalid_argument(format!(
                        "Unknown field '{k}' in NDJSON line {idx}"
                    )));
                }
            }
        }
        let mut row = Vec::with_capacity(columns.len());
        for col in columns {
            let raw = obj
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(&col.name))
                .map_or(&JsonValue::Null, |(_, v)| v);
            let decoded = json_to_bq(raw, &col.ty)?;
            if !col.nullable && decoded.is_null() {
                return Err(BqError::invalid_argument(format!(
                    "Required column '{}' is null in NDJSON line {idx}",
                    col.name
                )));
            }
            row.push(decoded);
        }
        rows.push(row);
    }
    Ok(rows)
}

/// Exports rows as newline-delimited JSON bytes.
pub fn export_ndjson(columns: &[ColumnSchema], rows: &[Vec<BqValue>]) -> BqResult<Vec<u8>> {
    let mut out = String::new();
    for row in rows {
        let mut map = serde_json::Map::new();
        for (col, val) in columns.iter().zip(row.iter()) {
            map.insert(col.name.clone(), bq_to_ndjson_cell(val));
        }
        let line = serde_json::to_string(&JsonValue::Object(map))
            .map_err(|e| BqError::internal(format!("NDJSON encode failed: {e}")))?;
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out.into_bytes())
}

fn json_to_bq(val: &JsonValue, ty: &BqType) -> BqResult<BqValue> {
    if val.is_null() {
        return Ok(if matches!(ty, BqType::Array(_)) {
            BqValue::Array(Vec::new())
        } else {
            BqValue::Null
        });
    }
    match (ty, val) {
        (BqType::Bool, JsonValue::Bool(b)) => Ok(BqValue::Bool(*b)),
        (BqType::Int64, JsonValue::Number(n)) => n
            .as_i64()
            .map(BqValue::Int64)
            .ok_or_else(|| BqError::invalid_argument("Invalid INT64 in NDJSON")),
        (BqType::Float64, JsonValue::Number(n)) => n
            .as_f64()
            .map(BqValue::Float64)
            .ok_or_else(|| BqError::invalid_argument("Invalid FLOAT64 in NDJSON")),
        (BqType::String, JsonValue::String(s)) => Ok(BqValue::String(s.clone())),
        (BqType::Bytes, JsonValue::String(s)) => BASE64
            .decode(s.as_bytes())
            .map(BqValue::Bytes)
            .map_err(|e| BqError::invalid_argument(format!("Invalid base64 BYTES in NDJSON: {e}"))),
        (BqType::Json, other) => Ok(BqValue::Json(Box::new(other.clone()))),
        (BqType::Array(inner), JsonValue::Array(arr)) => {
            let items = arr
                .iter()
                .map(|el| json_to_bq(el, inner))
                .collect::<BqResult<Vec<_>>>()?;
            Ok(BqValue::Array(items))
        }
        (BqType::Struct(fields), JsonValue::Object(obj)) => {
            let mut out = Vec::with_capacity(fields.len());
            for sf in fields {
                let fname = sf.name.as_deref().unwrap_or("");
                let raw = obj
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(fname))
                    .map_or(&JsonValue::Null, |(_, v)| v);
                out.push(BqStructFieldValue {
                    name: sf.name.clone(),
                    value: json_to_bq(raw, &sf.ty)?,
                });
            }
            Ok(BqValue::Struct(out))
        }
        (target, JsonValue::String(s)) => cast_value(BqValue::String(s.clone()), target),
        (target, other) => cast_value(BqValue::String(other.to_string()), target),
    }
}

fn bq_to_ndjson_cell(val: &BqValue) -> JsonValue {
    match val {
        BqValue::Null => JsonValue::Null,
        BqValue::Bool(b) => JsonValue::Bool(*b),
        BqValue::Int64(n) => JsonValue::Number((*n).into()),
        BqValue::Float64(f) => {
            serde_json::Number::from_f64(*f).map_or(JsonValue::Null, JsonValue::Number)
        }
        BqValue::String(s) | BqValue::Geography(s) => JsonValue::String(s.clone()),
        BqValue::Bytes(b) => JsonValue::String(BASE64.encode(b)),
        BqValue::Json(j) => (**j).clone(),
        BqValue::Array(items) => JsonValue::Array(items.iter().map(bq_to_ndjson_cell).collect()),
        BqValue::Struct(fields) => {
            let mut map = serde_json::Map::new();
            for (idx, f) in fields.iter().enumerate() {
                let k = f.name.clone().unwrap_or_else(|| format!("f{idx}_"));
                map.insert(k, bq_to_ndjson_cell(&f.value));
            }
            JsonValue::Object(map)
        }
        BqValue::Numeric(_)
        | BqValue::BigNumeric(_)
        | BqValue::Date(_)
        | BqValue::Datetime(_)
        | BqValue::Time(_)
        | BqValue::Timestamp(_)
        | BqValue::Interval(_)
        | BqValue::Range(_) => JsonValue::String(val.to_string()),
    }
}
