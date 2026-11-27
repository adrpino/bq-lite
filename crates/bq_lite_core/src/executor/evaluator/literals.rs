use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};

use crate::ast::Literal;
use crate::error::{BqError, BqResult};
use crate::types::{
    check_max_6_frac_digits, parse_naive_datetime, parse_time_str, parse_timestamp_str,
    validate_bq_date, BqBigNumeric, BqNumeric, BqValue,
};

pub(super) fn eval_literal(lit: &Literal) -> BqResult<BqValue> {
    match lit {
        Literal::Null => Ok(BqValue::Null),
        Literal::Bool(b) => Ok(BqValue::Bool(*b)),
        Literal::Int(n) => Ok(BqValue::Int64(*n)),
        Literal::Float(f) => Ok(BqValue::Float64(*f)),
        Literal::Numeric(s) => s.parse::<BqNumeric>().map(BqValue::Numeric),
        Literal::BigNumeric(s) => s
            .parse::<BqBigNumeric>()
            .map(|v| BqValue::BigNumeric(Box::new(v))),
        Literal::String(s) => Ok(BqValue::String(s.clone())),
        Literal::Bytes(b) => Ok(BqValue::Bytes(b.clone())),
        Literal::Date(s) => NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
            .map_err(|_| BqError::query(format!("Invalid DATE literal: '{s}'")))
            .and_then(validate_bq_date)
            .map(BqValue::Date),
        Literal::Datetime(s) => {
            check_max_6_frac_digits(s.trim())?;
            parse_datetime_lit(s).map(BqValue::Datetime)
        }
        Literal::Time(s) => {
            check_max_6_frac_digits(s.trim())?;
            parse_time_str(s)
                .map(BqValue::Time)
                .map_err(|_| BqError::query(format!("Invalid TIME literal: '{s}'")))
        }
        Literal::Timestamp(s) => parse_timestamp_lit(s).map(BqValue::Timestamp),
        Literal::Json(s) => serde_json::from_str::<serde_json::Value>(s)
            .map(|v| BqValue::Json(Box::new(v)))
            .map_err(|e| BqError::query(format!("Invalid JSON literal: {e}"))),
    }
}

fn parse_datetime_lit(s: &str) -> BqResult<NaiveDateTime> {
    parse_naive_datetime(s).map_err(|_| BqError::query(format!("Invalid DATETIME literal: '{s}'")))
}

fn parse_timestamp_lit(s: &str) -> BqResult<DateTime<Utc>> {
    let trimmed = s.trim();
    check_max_6_frac_digits(trimmed)?;
    parse_timestamp_str(trimmed)
        .map_err(|_| BqError::query(format!("Invalid TIMESTAMP literal: '{s}'")))
}

/// Evaluates `LIKE` with `%`, `_`, and `\` backslash escape support.
pub(super) fn eval_like(text: &str, pattern: &str) -> bool {
    #[derive(Clone, Copy)]
    enum Pat {
        Any,
        One,
        Lit(char),
    }
    let mut p = Vec::new();
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => p.push(Pat::Lit(chars.next().unwrap_or('\\'))),
            '%' => p.push(Pat::Any),
            '_' => p.push(Pat::One),
            other => p.push(Pat::Lit(other)),
        }
    }
    let mut dp = vec![false; p.len() + 1];
    if let Some(first) = dp.first_mut() {
        *first = true;
    }
    for j in 1..=p.len() {
        if matches!(p.get(j - 1), Some(Pat::Any)) {
            let prev = dp.get(j - 1).copied().unwrap_or(false);
            if let Some(slot) = dp.get_mut(j) {
                *slot = prev;
            }
        }
    }
    for tc in text.chars() {
        let mut next = vec![false; p.len() + 1];
        for j in 1..=p.len() {
            let matched = match p.get(j - 1) {
                Some(Pat::Any) => {
                    next.get(j - 1).copied().unwrap_or(false) || dp.get(j).copied().unwrap_or(false)
                }
                Some(Pat::One) => dp.get(j - 1).copied().unwrap_or(false),
                Some(Pat::Lit(pc)) => dp.get(j - 1).copied().unwrap_or(false) && tc == *pc,
                None => false,
            };
            if let Some(slot) = next.get_mut(j) {
                *slot = matched;
            }
        }
        dp = next;
    }
    dp.get(p.len()).copied().unwrap_or(false)
}

pub(super) const fn interval_sort_key(iv: crate::types::BqInterval) -> i128 {
    (iv.months as i128) * 30 * 86_400_000_000
        + (iv.days as i128) * 86_400_000_000
        + (iv.micros as i128)
}

pub(super) fn compare_ranges(
    rx: &crate::types::BqRange,
    ry: &crate::types::BqRange,
) -> BqResult<Option<std::cmp::Ordering>> {
    let start_ord = match (&rx.start, &ry.start) {
        (None, None) => Some(std::cmp::Ordering::Equal),
        (None, Some(_)) => Some(std::cmp::Ordering::Less),
        (Some(_), None) => Some(std::cmp::Ordering::Greater),
        (Some(sx), Some(sy)) => super::ops::compare_values(sx, sy)?,
    };
    if start_ord != Some(std::cmp::Ordering::Equal) {
        return Ok(start_ord);
    }
    match (&rx.end, &ry.end) {
        (None, None) => Ok(Some(std::cmp::Ordering::Equal)),
        (None, Some(_)) => Ok(Some(std::cmp::Ordering::Greater)),
        (Some(_), None) => Ok(Some(std::cmp::Ordering::Less)),
        (Some(ex), Some(ey)) => super::ops::compare_values(ex, ey),
    }
}

pub(super) fn compare_arrays(
    ax: &[BqValue],
    ay: &[BqValue],
) -> BqResult<Option<std::cmp::Ordering>> {
    for (x, y) in ax.iter().zip(ay.iter()) {
        if x.is_null() || y.is_null() {
            if x.is_null() && y.is_null() {
                continue;
            }
            return Ok(None);
        }
        match super::ops::compare_values(x, y)? {
            Some(std::cmp::Ordering::Equal) => {}
            other => return Ok(other),
        }
    }
    Ok(Some(ax.len().cmp(&ay.len())))
}

pub(super) fn eval_range_literal(
    elem_type: &crate::ast::DataTypeAst,
    start: &crate::ast::Expr,
    end: &crate::ast::Expr,
    row: &crate::executor::context::EvalRow,
    ctx: &crate::executor::context::ExecContext<'_>,
) -> BqResult<BqValue> {
    let bq_ty = elem_type.to_bq_type()?;
    let rt = match bq_ty {
        crate::types::BqType::Date => crate::types::RangeElementType::Date,
        crate::types::BqType::Datetime => crate::types::RangeElementType::Datetime,
        crate::types::BqType::Timestamp => crate::types::RangeElementType::Timestamp,
        crate::types::BqType::Range(inner) => inner,
        crate::types::BqType::UntypedNull
        | crate::types::BqType::Bool
        | crate::types::BqType::Int64
        | crate::types::BqType::Float64
        | crate::types::BqType::Numeric
        | crate::types::BqType::BigNumeric
        | crate::types::BqType::String
        | crate::types::BqType::Bytes
        | crate::types::BqType::Time
        | crate::types::BqType::Interval
        | crate::types::BqType::Json
        | crate::types::BqType::Geography
        | crate::types::BqType::Array(_)
        | crate::types::BqType::Struct(_) => {
            return Err(BqError::query(
                "RANGE literal requires DATE/DATETIME/TIMESTAMP element type",
            ))
        }
    };
    let s_raw = super::eval_expr(start, row, ctx)?;
    let e_raw = super::eval_expr(end, row, ctx)?;
    let target = rt.to_bq_type();
    let s_opt = if s_raw.is_null() {
        None
    } else {
        Some(crate::types::coercion::cast_value(s_raw, &target)?)
    };
    let e_opt = if e_raw.is_null() {
        None
    } else {
        Some(crate::types::coercion::cast_value(e_raw, &target)?)
    };
    if let (Some(sv), Some(ev)) = (&s_opt, &e_opt) {
        if matches!(
            super::ops::compare_values(sv, ev)?,
            Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
        ) {
            return Err(BqError::semantic(
                "RANGE start must be strictly less than end",
            ));
        }
    }
    Ok(BqValue::Range(Box::new(crate::types::BqRange::new(
        rt, s_opt, e_opt,
    )?)))
}

pub(super) fn validate_type_params(
    val: &BqValue,
    ast_ty: &crate::ast::DataTypeAst,
) -> BqResult<()> {
    use crate::ast::DataTypeAst;
    if val.is_null() {
        return Ok(());
    }
    match (val, ast_ty) {
        (BqValue::String(s), DataTypeAst::String { max_len: Some(max) }) => {
            let len = u64::try_from(s.chars().count()).unwrap_or(u64::MAX);
            if len > *max {
                return Err(BqError::semantic(format!(
                    "STRING({max}) has maximum length {max} but got a value with length {len}"
                )));
            }
        }
        (BqValue::Bytes(b), DataTypeAst::Bytes { max_len: Some(max) }) => {
            let len = u64::try_from(b.len()).unwrap_or(u64::MAX);
            if len > *max {
                return Err(BqError::semantic(format!(
                    "BYTES({max}) has maximum length {max} but got a value with length {len}"
                )));
            }
        }
        (
            BqValue::Numeric(n),
            DataTypeAst::Numeric {
                precision: Some(p),
                scale,
            },
        ) => check_decimal_precision(&n.to_string(), *p, scale.unwrap_or(0), "NUMERIC")?,
        (
            BqValue::BigNumeric(bn),
            DataTypeAst::BigNumeric {
                precision: Some(p),
                scale,
            },
        ) => check_decimal_precision(&bn.to_string(), *p, scale.unwrap_or(0), "BIGNUMERIC")?,
        (BqValue::Array(items), DataTypeAst::Array(elem_ty)) => {
            for item in items {
                validate_type_params(item, elem_ty)?;
            }
        }
        (BqValue::Struct(fields), DataTypeAst::Struct(field_tys)) => {
            for (fv, ft) in fields.iter().zip(field_tys.iter()) {
                validate_type_params(&fv.value, &ft.ty)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn check_decimal_precision(raw: &str, precision: u32, scale: u32, name: &str) -> BqResult<()> {
    let unsigned = raw.trim().trim_start_matches(['+', '-']);
    let int_part = unsigned
        .split('.')
        .next()
        .unwrap_or("0")
        .trim_start_matches('0');
    let max_int_digits = usize::try_from(precision.saturating_sub(scale)).unwrap_or(0);
    if int_part.len() > max_int_digits {
        return Err(BqError::semantic(format!(
            "{name}({precision}, {scale}) precision overflow for '{raw}'"
        )));
    }
    Ok(())
}
