use crate::error::{BqError, BqResult};

use super::BqInterval;

const MAX_INTERVAL_MONTHS: i128 = 120_000;
const MAX_INTERVAL_DAYS: i128 = 3_660_000;
const MAX_INTERVAL_MICROS: i128 = 316_224_000_000_000_000;

/// Validates `GoogleSQL` 10,000-year bounds for an `INTERVAL` value.
#[expect(
    clippy::redundant_pub_crate,
    reason = "Re-exported pub(crate) via types::mod while satisfying unreachable_pub"
)]
pub(crate) fn validate_interval(months: i128, days: i128, micros: i128) -> BqResult<BqInterval> {
    if !(-MAX_INTERVAL_MONTHS..=MAX_INTERVAL_MONTHS).contains(&months) {
        return Err(BqError::semantic(format!(
            "Interval field months '{months}' is out of range -{MAX_INTERVAL_MONTHS} to {MAX_INTERVAL_MONTHS}"
        )));
    }
    if !(-MAX_INTERVAL_DAYS..=MAX_INTERVAL_DAYS).contains(&days) {
        return Err(BqError::semantic(format!(
            "Interval field days '{days}' is out of range -{MAX_INTERVAL_DAYS} to {MAX_INTERVAL_DAYS}"
        )));
    }
    if !(-MAX_INTERVAL_MICROS..=MAX_INTERVAL_MICROS).contains(&micros) {
        return Err(BqError::semantic("Interval microseconds out of range"));
    }
    let m32 = i32::try_from(months).map_err(|_| BqError::semantic("Interval months overflow"))?;
    let d32 = i32::try_from(days).map_err(|_| BqError::semantic("Interval days overflow"))?;
    let us64 = i64::try_from(micros).map_err(|_| BqError::semantic("Interval micros overflow"))?;
    Ok(BqInterval::new(m32, d32, us64))
}

/// Constructs a single-unit `INTERVAL` from an `INT64` quantity and upper-case unit name.
#[expect(
    clippy::redundant_pub_crate,
    reason = "Re-exported pub(crate) via types::mod while satisfying unreachable_pub"
)]
pub(crate) fn single_unit_interval(n: i64, upper_unit: &str) -> BqResult<BqInterval> {
    let n128 = i128::from(n);
    match upper_unit {
        "YEAR" => validate_interval(n128 * 12, 0, 0),
        "QUARTER" => validate_interval(n128 * 3, 0, 0),
        "MONTH" => validate_interval(n128, 0, 0),
        "WEEK" => validate_interval(0, n128 * 7, 0),
        "DAY" => validate_interval(0, n128, 0),
        "HOUR" => validate_interval(0, 0, n128 * 3_600_000_000),
        "MINUTE" => validate_interval(0, 0, n128 * 60_000_000),
        "SECOND" => validate_interval(0, 0, n128 * 1_000_000),
        "MILLISECOND" => validate_interval(0, 0, n128 * 1_000),
        "MICROSECOND" => validate_interval(0, 0, n128),
        _ => Err(BqError::query(format!(
            "Unsupported INTERVAL unit '{upper_unit}'"
        ))),
    }
}

/// Parses a `GoogleSQL` single-unit or compound `INTERVAL` string literal.
#[expect(
    clippy::redundant_pub_crate,
    reason = "Re-exported pub(crate) via types::mod while satisfying unreachable_pub"
)]
pub(crate) fn parse_compound_interval(
    raw: &str,
    start_unit: &str,
    end_unit: Option<&str>,
) -> BqResult<BqInterval> {
    let trimmed = raw.trim();
    let su = start_unit.to_ascii_uppercase();
    let eu = end_unit.map(str::to_ascii_uppercase);
    let Some(end) = eu.as_deref() else {
        if su == "SECOND" {
            let micros = parse_signed_sec_micros(trimmed, raw)?;
            return validate_interval(0, 0, micros);
        }
        let n = trimmed
            .parse::<i64>()
            .map_err(|_| BqError::semantic(format!("Invalid INTERVAL literal '{raw}'")))?;
        return single_unit_interval(n, &su);
    };
    let toks: Vec<&str> = trimmed.split_whitespace().collect();
    let mut months: i128 = 0;
    let mut days: i128 = 0;
    let mut micros: i128 = 0;
    match (su.as_str(), end, toks.as_slice()) {
        ("YEAR", "MONTH", [ym]) => months = parse_signed_ym(ym, raw)?,
        ("YEAR", "DAY", [ym, d_str]) => {
            months = parse_signed_ym(ym, raw)?;
            days = parse_i128(d_str, raw)?;
        }
        ("YEAR", "HOUR" | "MINUTE" | "SECOND", [ym, d_str, hms]) => {
            months = parse_signed_ym(ym, raw)?;
            days = parse_i128(d_str, raw)?;
            micros = parse_signed_hms_micros(hms, "HOUR", end, raw)?;
        }
        ("MONTH", "DAY", [m_str, d_str]) => {
            months = parse_i128(m_str, raw)?;
            days = parse_i128(d_str, raw)?;
        }
        ("MONTH", "HOUR" | "MINUTE" | "SECOND", [m_str, d_str, hms]) => {
            months = parse_i128(m_str, raw)?;
            days = parse_i128(d_str, raw)?;
            micros = parse_signed_hms_micros(hms, "HOUR", end, raw)?;
        }
        ("DAY", "HOUR" | "MINUTE" | "SECOND", [d_str, hms]) => {
            days = parse_i128(d_str, raw)?;
            micros = parse_signed_hms_micros(hms, "HOUR", end, raw)?;
        }
        ("HOUR", "MINUTE" | "SECOND", [hms]) | ("MINUTE", "SECOND", [hms]) => {
            micros = parse_signed_hms_micros(hms, &su, end, raw)?;
        }
        (
            "YEAR" | "MONTH" | "DAY" | "HOUR" | "MINUTE",
            "MONTH" | "DAY" | "HOUR" | "MINUTE" | "SECOND",
            _,
        ) => return Err(invalid_lit(raw)),
        _ => {
            return Err(BqError::query(format!(
                "Unsupported INTERVAL range '{start_unit} TO {end}'"
            )))
        }
    }
    validate_interval(months, days, micros)
}

/// Parses `CAST(STRING AS INTERVAL)` supporting canonical SQL and ISO 8601 duration formats.
pub(super) fn parse_interval_cast(raw: &str) -> BqResult<BqInterval> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(BqError::semantic(format!("Invalid INTERVAL value '{raw}'")));
    }
    let after_sign = trimmed.strip_prefix(['+', '-']).unwrap_or(trimmed);
    if after_sign.starts_with(['P', 'p']) {
        return parse_iso8601_interval(trimmed, raw);
    }
    parse_canonical_interval_cast(trimmed, raw)
}

fn parse_canonical_interval_cast(trimmed: &str, raw: &str) -> BqResult<BqInterval> {
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    match parts.as_slice() {
        [ym, d, hms] => {
            let months = parse_signed_ym(ym, raw)?;
            let days = parse_i128(d, raw)?;
            let micros = parse_signed_hms_micros(hms, "HOUR", "SECOND", raw)?;
            validate_interval(months, days, micros)
        }
        [a, b] => {
            if has_inner_hyphen(a) {
                let months = parse_signed_ym(a, raw)?;
                let days = parse_i128(b, raw)?;
                validate_interval(months, days, 0)
            } else if b.contains(':') {
                let days = parse_i128(a, raw)?;
                let micros = parse_signed_hms_micros(b, "HOUR", "SECOND", raw)?;
                validate_interval(0, days, micros)
            } else {
                Err(BqError::semantic(format!("Invalid INTERVAL value '{raw}'")))
            }
        }
        [single] => {
            if single.contains(':') {
                let colons = single.bytes().filter(|&b| b == b':').count();
                let end = if colons == 1 { "MINUTE" } else { "SECOND" };
                let micros = parse_signed_hms_micros(single, "HOUR", end, raw)?;
                validate_interval(0, 0, micros)
            } else if has_inner_hyphen(single) {
                let months = parse_signed_ym(single, raw)?;
                validate_interval(months, 0, 0)
            } else {
                Err(BqError::semantic(format!("Invalid INTERVAL value '{raw}'")))
            }
        }
        _ => Err(BqError::semantic(format!("Invalid INTERVAL value '{raw}'"))),
    }
}

fn parse_iso8601_interval(trimmed: &str, raw: &str) -> BqResult<BqInterval> {
    let (outer_sign, rest) = strip_sign(trimmed);
    let body = rest
        .strip_prefix(['P', 'p'])
        .ok_or_else(|| BqError::semantic(format!("Invalid INTERVAL value '{raw}'")))?;
    if body.is_empty() {
        return Err(BqError::semantic(format!("Invalid INTERVAL value '{raw}'")));
    }
    let bytes = body.as_bytes();
    let mut i = 0_usize;
    let mut in_time = false;
    let mut saw_part = false;
    let mut last_rank = 0_u8;
    let mut months: i128 = 0;
    let mut days: i128 = 0;
    let mut micros: i128 = 0;

    while i < bytes.len() {
        let b = bytes.get(i).copied().unwrap_or(0);
        if b == b'T' || b == b't' {
            if in_time {
                return Err(BqError::semantic(format!("Invalid INTERVAL value '{raw}'")));
            }
            in_time = true;
            i += 1;
            if i == bytes.len() {
                return Err(BqError::semantic(format!("Invalid INTERVAL value '{raw}'")));
            }
            continue;
        }
        let part_start = i;
        if matches!(bytes.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let digit_start = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        let mut has_dot = false;
        if bytes.get(i) == Some(&b'.') {
            has_dot = true;
            i += 1;
            while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
        }
        if i == digit_start || (has_dot && i == digit_start + 1) {
            return Err(BqError::semantic(format!("Invalid INTERVAL value '{raw}'")));
        }
        let num_str = body.get(part_start..i).unwrap_or("");
        let Some(&designator) = bytes.get(i) else {
            return Err(BqError::semantic(format!("Invalid INTERVAL value '{raw}'")));
        };
        i += 1;
        let des = designator.to_ascii_uppercase();
        let rank = iso_designator_rank(in_time, des, has_dot, raw)?;
        if rank <= last_rank {
            return Err(BqError::semantic(format!("Invalid INTERVAL value '{raw}'")));
        }
        last_rank = rank;
        saw_part = true;
        let mut acc = (months, days, micros);
        apply_iso_part(in_time, des, num_str, raw, &mut acc)?;
        (months, days, micros) = acc;
    }
    if !saw_part {
        return Err(BqError::semantic(format!("Invalid INTERVAL value '{raw}'")));
    }
    validate_interval(
        months.saturating_mul(outer_sign),
        days.saturating_mul(outer_sign),
        micros.saturating_mul(outer_sign),
    )
}

fn iso_designator_rank(in_time: bool, des: u8, has_dot: bool, raw: &str) -> BqResult<u8> {
    if has_dot && !(in_time && des == b'S') {
        return Err(BqError::semantic(format!("Invalid INTERVAL value '{raw}'")));
    }
    let rank = match (in_time, des) {
        (false, b'Y') => 1,
        (false, b'M') => 2,
        (false, b'W') => 3,
        (false, b'D') => 4,
        (true, b'H') => 5,
        (true, b'M') => 6,
        (true, b'S') => 7,
        _ => return Err(BqError::semantic(format!("Invalid INTERVAL value '{raw}'"))),
    };
    Ok(rank)
}

fn apply_iso_part(
    in_time: bool,
    des: u8,
    num_str: &str,
    raw: &str,
    acc: &mut (i128, i128, i128),
) -> BqResult<()> {
    if in_time && des == b'S' {
        acc.2 = acc.2.saturating_add(parse_signed_sec_micros(num_str, raw)?);
        return Ok(());
    }
    let n = parse_i128(num_str, raw)?;
    match (in_time, des) {
        (false, b'Y') => acc.0 = acc.0.saturating_add(n.saturating_mul(12)),
        (false, b'M') => acc.0 = acc.0.saturating_add(n),
        (false, b'W') => acc.1 = acc.1.saturating_add(n.saturating_mul(7)),
        (false, b'D') => acc.1 = acc.1.saturating_add(n),
        (true, b'H') => acc.2 = acc.2.saturating_add(n.saturating_mul(3_600_000_000)),
        (true, b'M') => acc.2 = acc.2.saturating_add(n.saturating_mul(60_000_000)),
        _ => return Err(BqError::semantic(format!("Invalid INTERVAL value '{raw}'"))),
    }
    Ok(())
}

fn has_inner_hyphen(s: &str) -> bool {
    let rest = s.strip_prefix(['+', '-']).unwrap_or(s);
    rest.contains('-')
}

fn strip_sign(s: &str) -> (i128, &str) {
    s.strip_prefix('-').map_or_else(
        || s.strip_prefix('+').map_or((1, s), |r| (1, r)),
        |r| (-1, r),
    )
}

fn parse_signed_ym(s: &str, raw: &str) -> BqResult<i128> {
    let (sign, body) = strip_sign(s);
    let (y_s, m_s) = body.split_once('-').ok_or_else(|| invalid_lit(raw))?;
    let y = parse_u128_digits(y_s, raw)?;
    let m = parse_u128_digits(m_s, raw)?;
    Ok(sign.saturating_mul(y.saturating_mul(12).saturating_add(m)))
}

fn parse_signed_hms_micros(s: &str, start: &str, end: &str, raw: &str) -> BqResult<i128> {
    let (sign, body) = strip_sign(s);
    let parts: Vec<&str> = body.split(':').collect();
    let abs_us = match (start, end, parts.as_slice()) {
        ("HOUR", "HOUR", [h]) => parse_u128_digits(h, raw)?.saturating_mul(3_600_000_000),
        ("HOUR", "MINUTE", [h, m]) => parse_u128_digits(h, raw)?
            .saturating_mul(3_600_000_000)
            .saturating_add(parse_u128_digits(m, raw)?.saturating_mul(60_000_000)),
        ("HOUR", "SECOND", [h, m, sec]) => parse_u128_digits(h, raw)?
            .saturating_mul(3_600_000_000)
            .saturating_add(parse_u128_digits(m, raw)?.saturating_mul(60_000_000))
            .saturating_add(parse_unsigned_sec_micros(sec, raw)?),
        ("MINUTE", "SECOND", [m, sec]) => parse_u128_digits(m, raw)?
            .saturating_mul(60_000_000)
            .saturating_add(parse_unsigned_sec_micros(sec, raw)?),
        _ => return Err(invalid_lit(raw)),
    };
    Ok(sign.saturating_mul(abs_us))
}

fn parse_signed_sec_micros(s: &str, raw: &str) -> BqResult<i128> {
    let (sign, body) = strip_sign(s);
    let abs_us = parse_unsigned_sec_micros(body, raw)?;
    Ok(sign.saturating_mul(abs_us))
}

fn parse_unsigned_sec_micros(s: &str, raw: &str) -> BqResult<i128> {
    let (whole_s, frac_s) = s.split_once('.').unwrap_or((s, ""));
    let secs = if whole_s.is_empty() && !frac_s.is_empty() {
        0_i128
    } else {
        parse_u128_digits(whole_s, raw)?
    };
    if frac_s.is_empty() {
        return Ok(secs.saturating_mul(1_000_000));
    }
    if !frac_s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid_lit(raw));
    }
    // Truncate sub-microsecond digits toward zero (matching BigQuery without TIMESTAMP_NANOS).
    let mut padded = frac_s.chars().take(6).collect::<String>();
    while padded.len() < 6 {
        padded.push('0');
    }
    let frac = parse_u128_digits(&padded, raw)?;
    Ok(secs.saturating_mul(1_000_000).saturating_add(frac))
}

fn parse_i128(s: &str, raw: &str) -> BqResult<i128> {
    s.parse::<i128>().map_err(|_| invalid_lit(raw))
}

fn parse_u128_digits(s: &str, raw: &str) -> BqResult<i128> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid_lit(raw));
    }
    s.parse::<i128>().map_err(|_| invalid_lit(raw))
}

#[expect(
    clippy::unnecessary_box_returns,
    reason = "BqResult uses Box<BqError> directly"
)]
fn invalid_lit(raw: &str) -> Box<BqError> {
    BqError::semantic(format!("Invalid INTERVAL literal '{raw}'"))
}
