use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};

use crate::ast::Literal;
use crate::error::{BqError, BqResult};
use crate::types::{
    check_max_6_frac_digits, parse_timestamp_str, BqBigNumeric, BqNumeric, BqValue,
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
            .map(BqValue::Date)
            .map_err(|_| BqError::query(format!("Invalid DATE literal: '{s}'"))),
        Literal::Datetime(s) => parse_datetime_lit(s).map(BqValue::Datetime),
        Literal::Time(s) => {
            let trimmed = s.trim();
            check_max_6_frac_digits(trimmed)?;
            NaiveTime::parse_from_str(trimmed, "%H:%M:%S%.f")
                .or_else(|_| NaiveTime::parse_from_str(trimmed, "%H:%M:%S"))
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
    let trimmed = s.trim();
    check_max_6_frac_digits(trimmed)?;
    for fmt in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(trimmed, fmt) {
            return Ok(dt);
        }
    }
    Err(BqError::query(format!("Invalid DATETIME literal: '{s}'")))
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
