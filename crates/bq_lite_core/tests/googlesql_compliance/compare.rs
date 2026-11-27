use bq_lite_core::types::coercion::cast_value;
use bq_lite_core::{BqStructFieldValue, BqType, BqValue, QueryResult, StructField};

use super::value_parse::{ExpectedColumn, ExpectedTable, ExpectedVal};

pub(super) fn matches_query_result(
    expected: &ExpectedTable,
    actual: &QueryResult,
) -> Result<(), String> {
    if expected.rows.len() != actual.rows.len() {
        return Err(format!(
            "row count mismatch: expected {}, got {}",
            expected.rows.len(),
            actual.rows.len()
        ));
    }
    if expected.ordered {
        for (idx, (erow, arow)) in expected.rows.iter().zip(&actual.rows).enumerate() {
            if !rows_match(&expected.columns, erow, arow) {
                return Err(format!(
                    "ordered row {idx} mismatch: expected {erow:?}, got {arow:?}"
                ));
            }
        }
        return Ok(());
    }
    let mut used = vec![false; actual.rows.len()];
    for (e_idx, erow) in expected.rows.iter().enumerate() {
        let found = actual.rows.iter().enumerate().position(|(a_idx, arow)| {
            !used.get(a_idx).copied().unwrap_or(true) && rows_match(&expected.columns, erow, arow)
        });
        if let Some(pos) = found {
            if let Some(slot) = used.get_mut(pos) {
                *slot = true;
            }
        } else {
            return Err(format!(
                "unordered row {e_idx} not found: expected {erow:?} in actual ({} rows)",
                actual.rows.len()
            ));
        }
    }
    Ok(())
}

fn rows_match(cols: &[ExpectedColumn], erow: &[ExpectedVal], arow: &[BqValue]) -> bool {
    if erow.len() != arow.len() {
        return false;
    }
    for (idx, (ev, av)) in erow.iter().zip(arow).enumerate() {
        let col_ty = cols.get(idx).map(|c| &c.bq_type);
        if !val_matches(ev, av, col_ty) {
            return false;
        }
    }
    true
}

#[expect(
    clippy::cast_precision_loss,
    reason = "Compliance value comparison matches expected literals against BqValue variants"
)]
fn val_matches(ev: &ExpectedVal, av: &BqValue, col_ty: Option<&BqType>) -> bool {
    if matches!(ev, ExpectedVal::Null) {
        return av.is_null();
    }
    if av.is_null() {
        return false;
    }
    if matches!(col_ty, Some(BqType::Json)) || matches!(av, BqValue::Json(_)) {
        return json_matches(ev, av);
    }
    match (ev, av) {
        (ExpectedVal::Bool(eb), BqValue::Bool(ab)) => eb == ab,
        (ExpectedVal::Int(ei), BqValue::Int64(ai)) => ei == ai,
        (ExpectedVal::Int(ei), BqValue::Float64(af))
            if matches!(col_ty, Some(BqType::Float64) | None) =>
        {
            float_eq(*ei as f64, *af)
        }
        (ExpectedVal::Float(ef), BqValue::Float64(af)) => float_eq(*ef, *af),
        (ExpectedVal::Int(ei), BqValue::Numeric(an)) => {
            decimal_eq(&ei.to_string(), &an.to_string())
        }
        (ExpectedVal::Float(ef), BqValue::Numeric(an)) => {
            decimal_eq(&ef.to_string(), &an.to_string())
        }
        (ExpectedVal::Raw(er), BqValue::Numeric(an)) => decimal_eq(er, &an.to_string()),
        (ExpectedVal::Int(ei), BqValue::BigNumeric(an)) => {
            decimal_eq(&ei.to_string(), &an.to_string())
        }
        (ExpectedVal::Float(ef), BqValue::BigNumeric(an)) => {
            decimal_eq(&ef.to_string(), &an.to_string())
        }
        (ExpectedVal::Raw(er), BqValue::BigNumeric(an)) => decimal_eq(er, &an.to_string()),
        (ExpectedVal::String(es), BqValue::String(as_)) => es == as_,
        (ExpectedVal::Bytes(eb), BqValue::Bytes(ab)) => eb == ab,
        (ExpectedVal::Array(ea), BqValue::Array(aa)) => {
            let elem_ty = match col_ty {
                Some(BqType::Array(inner)) => Some(inner.as_ref()),
                _ => None,
            };
            ea.len() == aa.len() && ea.iter().zip(aa).all(|(e, a)| val_matches(e, a, elem_ty))
        }
        (ExpectedVal::Struct(es), BqValue::Struct(as_)) => {
            let sfields = match col_ty {
                Some(BqType::Struct(fs)) => Some(fs.as_slice()),
                _ => None,
            };
            es.len() == as_.len()
                && es.iter().zip(as_).enumerate().all(|(i, (e, a))| {
                    let fty = sfields.and_then(|fs| fs.get(i)).map(|f| &f.ty);
                    val_matches(e, &a.value, fty)
                })
        }
        (ExpectedVal::Raw(er), other) => raw_temporal_or_display_eq(er, other),
        (ExpectedVal::String(es), other) => es == &other.to_string(),
        _ => false,
    }
}

fn float_eq(a: f64, b: f64) -> bool {
    if a.is_nan() && b.is_nan() {
        return true;
    }
    if a.to_bits() == b.to_bits() {
        return true;
    }
    if !a.is_finite() || !b.is_finite() {
        return false;
    }
    let diff = (a - b).abs();
    diff <= 1e-9_f64.max(1e-9 * a.abs().max(b.abs()))
}

fn decimal_eq(a: &str, b: &str) -> bool {
    normalize_decimal(a) == normalize_decimal(b)
}

fn normalize_decimal(s: &str) -> String {
    let trimmed = s.trim();
    let (neg, rest) = trimmed.strip_prefix('-').map_or_else(
        || (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
        |r| (true, r),
    );
    let (int_p, frac_p) = rest.split_once('.').unwrap_or((rest, ""));
    let int_clean = int_p.trim_start_matches('0');
    let int_part = if int_clean.is_empty() { "0" } else { int_clean };
    let frac_part = frac_p.trim_end_matches('0');
    if int_part == "0" && frac_part.is_empty() {
        return "0".to_owned();
    }
    let sign = if neg { "-" } else { "" };
    if frac_part.is_empty() {
        format!("{sign}{int_part}")
    } else {
        format!("{sign}{int_part}.{frac_part}")
    }
}

fn json_matches(ev: &ExpectedVal, av: &BqValue) -> bool {
    let BqValue::Json(aj) = av else {
        return false;
    };
    let ej_res: Result<serde_json::Value, _> = match ev {
        ExpectedVal::Raw(s) => serde_json::from_str(s),
        ExpectedVal::String(s) => Ok(serde_json::Value::String(s.clone())),
        ExpectedVal::Int(i) => Ok(serde_json::json!(i)),
        ExpectedVal::Float(f) => Ok(serde_json::json!(f)),
        ExpectedVal::Bool(b) => Ok(serde_json::json!(b)),
        ExpectedVal::Null => Ok(serde_json::Value::Null),
        ExpectedVal::Array(_) | ExpectedVal::Bytes(_) | ExpectedVal::Struct(_) => {
            return expected_val_to_json(ev) == **aj;
        }
    };
    ej_res.is_ok_and(|ej| ej == **aj)
}

fn expected_val_to_json(ev: &ExpectedVal) -> serde_json::Value {
    match ev {
        ExpectedVal::Null => serde_json::Value::Null,
        ExpectedVal::Bool(b) => serde_json::Value::Bool(*b),
        ExpectedVal::Int(i) => serde_json::json!(i),
        ExpectedVal::Float(f) => serde_json::json!(f),
        ExpectedVal::String(s) => serde_json::Value::String(s.clone()),
        ExpectedVal::Raw(s) => {
            serde_json::from_str(s).unwrap_or_else(|_| serde_json::Value::String(s.clone()))
        }
        ExpectedVal::Bytes(b) => serde_json::Value::String(String::from_utf8_lossy(b).into_owned()),
        ExpectedVal::Array(items) | ExpectedVal::Struct(items) => {
            serde_json::Value::Array(items.iter().map(expected_val_to_json).collect())
        }
    }
}

fn raw_temporal_or_display_eq(expected_raw: &str, actual: &BqValue) -> bool {
    let actual_s = actual.to_string();
    if expected_raw == actual_s {
        return true;
    }
    let norm_e = normalize_temporal(expected_raw);
    let norm_a = normalize_temporal(&actual_s);
    norm_e == norm_a
}

fn normalize_temporal(s: &str) -> String {
    let mut out = s
        .trim()
        .replace(" UTC", "+00")
        .replace("+00:00", "+00")
        .replace('T', " ");
    // Strip trailing fractional zeros like .000000 before +00 or end
    if let Some(dot) = out.find('.') {
        let tail = &out[dot + 1..];
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            let frac = &tail[..digits];
            let trimmed = frac.trim_end_matches('0');
            let rest = &tail[digits..];
            if trimmed.is_empty() {
                out = format!("{}{rest}", &out[..dot]);
            } else {
                out = format!("{}.{trimmed}{rest}", &out[..dot]);
            }
        }
    }
    out
}

pub(super) fn expected_val_to_bq(ev: &ExpectedVal, target: &BqType) -> BqValue {
    match ev {
        ExpectedVal::Null => BqValue::Null,
        ExpectedVal::Bool(b) => cast_or_fallback(BqValue::Bool(*b), target),
        ExpectedVal::Int(i) => cast_or_fallback(BqValue::Int64(*i), target),
        ExpectedVal::Float(f) => cast_or_fallback(BqValue::Float64(*f), target),
        ExpectedVal::String(s) => cast_or_fallback(BqValue::String(s.clone()), target),
        ExpectedVal::Bytes(b) => BqValue::Bytes(b.clone()),
        ExpectedVal::Raw(r) => raw_to_bq(r, target),
        ExpectedVal::Array(items) => {
            let elem_ty = if let BqType::Array(inner) = target {
                inner.as_ref()
            } else {
                &BqType::String
            };
            BqValue::Array(
                items
                    .iter()
                    .map(|x| expected_val_to_bq(x, elem_ty))
                    .collect(),
            )
        }
        ExpectedVal::Struct(items) => struct_to_bq(items, target),
    }
}

fn cast_or_fallback(v: BqValue, target: &BqType) -> BqValue {
    cast_value(v.clone(), target).unwrap_or(v)
}

fn raw_to_bq(r: &str, target: &BqType) -> BqValue {
    if matches!(target, BqType::Json) {
        if let Ok(j) = serde_json::from_str::<serde_json::Value>(r) {
            return BqValue::Json(Box::new(j));
        }
    }
    if matches!(target, BqType::Range(_)) {
        return cast_or_fallback(BqValue::String(r.to_owned()), target);
    }
    let norm = r.replace("+00", " UTC");
    cast_value(BqValue::String(norm), target)
        .or_else(|_| cast_value(BqValue::String(r.to_owned()), target))
        .unwrap_or_else(|_| BqValue::String(r.to_owned()))
}

fn struct_to_bq(items: &[ExpectedVal], target: &BqType) -> BqValue {
    let fields: Vec<StructField> = if let BqType::Struct(fs) = target {
        fs.clone()
    } else {
        Vec::new()
    };
    let out = items
        .iter()
        .enumerate()
        .map(|(i, val)| {
            let f = fields.get(i);
            let fty = f.map_or(&BqType::String, |x| &x.ty);
            BqStructFieldValue {
                name: f.and_then(|x| x.name.clone()),
                value: expected_val_to_bq(val, fty),
            }
        })
        .collect();
    BqValue::Struct(out)
}
