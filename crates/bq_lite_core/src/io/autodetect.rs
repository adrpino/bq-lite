use std::collections::BTreeMap;

use serde_json::Value as JsonValue;

use crate::catalog::ColumnSchema;
use crate::error::{BqError, BqResult};
use crate::types::coercion::common_supertype;
use crate::types::{BqType, StructField};

use super::csv_json::CsvLoadOptions;

/// Infers `ColumnSchema`s and `skip_leading_rows` (0 or 1) from CSV bytes.
pub fn autodetect_csv_schema(
    bytes: &[u8],
    opts: &CsvLoadOptions,
) -> BqResult<(Vec<ColumnSchema>, usize)> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(opts.field_delimiter)
        .quote(opts.quote)
        .from_reader(bytes);
    let mut records: Vec<Vec<String>> = Vec::new();
    for rec_res in rdr.records().take(100) {
        let rec = rec_res
            .map_err(|e| BqError::invalid_argument(format!("CSV autodetect parse error: {e}")))?;
        records.push(rec.iter().map(str::to_owned).collect());
    }
    let Some(first) = records.first() else {
        return Err(BqError::invalid_argument(
            "Cannot autodetect schema from empty CSV",
        ));
    };
    let width = first.len();
    let rest_rows = records.get(1..).unwrap_or(&[]);
    let has_header = !rest_rows.is_empty() && row_looks_like_header(first, rest_rows);
    let data_start = if has_header {
        opts.skip_leading_rows.max(1)
    } else {
        opts.skip_leading_rows
    };

    let mut cols = Vec::with_capacity(width);
    for col_idx in 0..width {
        let mut inferred = Vec::new();
        for row in records.iter().skip(data_start) {
            if let Some(cell) = row.get(col_idx) {
                if !is_csv_null_cell(cell, opts) {
                    inferred.push(infer_csv_cell_type(cell));
                }
            }
        }
        let ty = if inferred.is_empty() {
            BqType::String
        } else {
            common_supertype(&inferred).unwrap_or(BqType::String)
        };
        let name = if has_header {
            first.get(col_idx).map_or_else(
                || format!("col_{col_idx}"),
                |s| sanitize_col_name(s, col_idx),
            )
        } else {
            format!("{}_field_{col_idx}", ty.rest_type_name().to_lowercase())
        };
        cols.push(ColumnSchema::new(name, ty));
    }
    Ok((cols, data_start))
}

fn is_csv_null_cell(cell: &str, opts: &CsvLoadOptions) -> bool {
    cell.is_empty()
        || opts
            .null_marker
            .as_deref()
            .is_some_and(|marker| cell == marker)
}

fn row_looks_like_header(first: &[String], rest: &[Vec<String>]) -> bool {
    let first_all_ident = first.iter().all(|s| {
        let t = s.trim();
        !t.is_empty()
            && t.parse::<i64>().is_err()
            && t.parse::<f64>().is_err()
            && !t.eq_ignore_ascii_case("true")
            && !t.eq_ignore_ascii_case("false")
    });
    if !first_all_ident {
        return false;
    }
    rest.iter().any(|row| {
        row.iter().any(|c| {
            let t = c.trim();
            t.parse::<i64>().is_ok()
                || t.parse::<f64>().is_ok()
                || t.eq_ignore_ascii_case("true")
                || t.eq_ignore_ascii_case("false")
        })
    }) || first
        .iter()
        .all(|s| s.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_'))
}

fn infer_csv_cell_type(cell: &str) -> BqType {
    let t = cell.trim();
    if t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("false") {
        return BqType::Bool;
    }
    if t.parse::<i64>().is_ok() {
        return BqType::Int64;
    }
    if t.parse::<f64>().is_ok() {
        return BqType::Float64;
    }
    if chrono::NaiveDate::parse_from_str(t, "%Y-%m-%d").is_ok() {
        return BqType::Date;
    }
    if chrono::DateTime::parse_from_rfc3339(t).is_ok() {
        return BqType::Timestamp;
    }
    BqType::String
}

fn sanitize_col_name(raw: &str, idx: usize) -> String {
    let cleaned: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        format!("col_{idx}")
    } else {
        cleaned
    }
}

/// Infers `ColumnSchema`s from newline-delimited JSON bytes.
pub fn autodetect_ndjson_schema(bytes: &[u8]) -> BqResult<Vec<ColumnSchema>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| BqError::invalid_argument(format!("Invalid UTF-8 in NDJSON: {e}")))?;
    let mut field_types: BTreeMap<String, Vec<BqType>> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for (idx, line) in text.lines().take(200).enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let val: JsonValue = serde_json::from_str(trimmed).map_err(|e| {
            BqError::invalid_argument(format!("NDJSON autodetect parse error at line {idx}: {e}"))
        })?;
        let obj = val
            .as_object()
            .ok_or_else(|| BqError::invalid_argument("NDJSON line must be a JSON object"))?;
        for (k, v) in obj {
            let inferred = infer_json_val_type(v);
            if !field_types.contains_key(k) {
                order.push(k.clone());
            }
            field_types.entry(k.clone()).or_default().push(inferred);
        }
    }
    if order.is_empty() {
        return Err(BqError::invalid_argument(
            "Cannot autodetect schema from empty NDJSON",
        ));
    }
    Ok(order
        .into_iter()
        .map(|name| {
            let tys = field_types.get(&name).map_or(&[][..], Vec::as_slice);
            let ty = merge_inferred_types(tys);
            ColumnSchema::new(name, ty)
        })
        .collect())
}

fn infer_json_val_type(v: &JsonValue) -> BqType {
    match v {
        JsonValue::Null => BqType::UntypedNull,
        JsonValue::Bool(_) => BqType::Bool,
        JsonValue::Number(n) => {
            if n.is_i64() {
                BqType::Int64
            } else {
                BqType::Float64
            }
        }
        JsonValue::String(s) => {
            if chrono::DateTime::parse_from_rfc3339(s).is_ok() {
                BqType::Timestamp
            } else if chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok() {
                BqType::Date
            } else {
                BqType::String
            }
        }
        JsonValue::Array(items) => {
            let elem_tys: Vec<BqType> = items.iter().map(infer_json_val_type).collect();
            let elem = merge_inferred_types(&elem_tys);
            BqType::array(elem).unwrap_or(BqType::String)
        }
        JsonValue::Object(map) => {
            let fields = map
                .iter()
                .map(|(k, fv)| StructField::named(k, infer_json_val_type(fv)))
                .collect();
            BqType::Struct(fields)
        }
    }
}

fn merge_inferred_types(tys: &[BqType]) -> BqType {
    let non_null: Vec<BqType> = tys
        .iter()
        .filter(|t| !matches!(t, BqType::UntypedNull))
        .cloned()
        .collect();
    if non_null.is_empty() {
        return BqType::String;
    }
    if let Some(BqType::Struct(_)) = non_null.first() {
        let mut merged: BTreeMap<String, Vec<BqType>> = BTreeMap::new();
        let mut names = Vec::new();
        for t in &non_null {
            if let BqType::Struct(fields) = t {
                for sf in fields {
                    if let Some(ref name) = sf.name {
                        if !merged.contains_key(name) {
                            names.push(name.clone());
                        }
                        merged.entry(name.clone()).or_default().push(sf.ty.clone());
                    }
                }
            }
        }
        let sfields = names
            .into_iter()
            .map(|n| {
                let sub = merged.get(&n).map_or(&[][..], Vec::as_slice);
                StructField::named(n, merge_inferred_types(sub))
            })
            .collect();
        return BqType::Struct(sfields);
    }
    common_supertype(&non_null).unwrap_or(BqType::String)
}
