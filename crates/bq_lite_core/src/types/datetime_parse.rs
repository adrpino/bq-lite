use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};

use crate::error::{BqError, BqResult};

pub(super) fn check_max_6_frac_digits(s: &str) -> BqResult<()> {
    let Some(dot_idx) = s.find('.') else {
        return Ok(());
    };
    let after_dot = s.get(dot_idx + 1..).unwrap_or("");
    let digits = after_dot.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 6 {
        return Err(BqError::semantic(format!(
            "Sub-microsecond precision is not supported (max 6 fractional digits): '{s}'"
        )));
    }
    Ok(())
}

pub(super) fn parse_time_str(s: &str) -> BqResult<NaiveTime> {
    let trimmed = s.trim();
    check_max_6_frac_digits(trimmed)?;
    NaiveTime::parse_from_str(trimmed, "%H:%M:%S%.f")
        .or_else(|_| NaiveTime::parse_from_str(trimmed, "%H:%M:%S"))
        .map_err(|_| BqError::semantic(format!("Invalid TIME string: '{s}'")))
}

pub(super) fn parse_naive_datetime(s: &str) -> BqResult<NaiveDateTime> {
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
    if let Ok(d) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        return Ok(d.and_hms_opt(0, 0, 0).unwrap_or_default());
    }
    Err(BqError::semantic(format!("Invalid DATETIME string: '{s}'")))
}

pub(super) fn parse_timestamp_str(s: &str) -> BqResult<DateTime<Utc>> {
    let trimmed = s.trim();
    check_max_6_frac_digits(trimmed)?;
    let normalized = normalize_tz_offset(trimmed);
    let cand = normalized.as_str();
    if let Ok(dt) = DateTime::parse_from_rfc3339(cand) {
        return Ok(dt.with_timezone(&Utc));
    }
    for fmt in [
        "%Y-%m-%d %H:%M:%S%.f%:z",
        "%Y-%m-%d %H:%M:%S%:z",
        "%Y-%m-%d %H:%M:%S%.f%#z",
        "%Y-%m-%d %H:%M:%S%#z",
        "%Y-%m-%dT%H:%M:%S%.f%#z",
        "%Y-%m-%dT%H:%M:%S%#z",
    ] {
        if let Ok(dt) = DateTime::parse_from_str(cand, fmt) {
            return Ok(dt.with_timezone(&Utc));
        }
    }
    let no_utc = cand
        .strip_suffix(" UTC")
        .or_else(|| cand.strip_suffix(" utc"))
        .or_else(|| cand.strip_suffix('Z'))
        .unwrap_or(cand);
    if let Ok(dt) = parse_naive_datetime(no_utc) {
        return Ok(DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc));
    }
    Err(BqError::semantic(format!(
        "Invalid TIMESTAMP string: '{s}'"
    )))
}

/// Normalizes `BigQuery` timezone offsets (`+H`, `+HH`, `+H:MM`, `+HH:MM`, `+HHMM`, with optional
/// leading whitespace before the sign) into `±HH:MM`.
fn normalize_tz_offset(s: &str) -> String {
    let Some(sep_pos) = s.find([' ', 'T', 't']) else {
        return s.to_owned();
    };
    let tail = s.get(sep_pos + 1..).unwrap_or("");
    let Some(rel_sign) = tail.rfind(['+', '-']) else {
        return s.to_owned();
    };
    let sign_idx = sep_pos + 1 + rel_sign;
    let prefix = s.get(..sign_idx).unwrap_or("").trim_end();
    let sign = s.get(sign_idx..=sign_idx).unwrap_or("+");
    let off = s.get(sign_idx + 1..).unwrap_or("");
    let bytes = off.as_bytes();
    match bytes {
        [h] if h.is_ascii_digit() => format!("{prefix}{sign}0{off}:00"),
        [h1, h2] if h1.is_ascii_digit() && h2.is_ascii_digit() => {
            format!("{prefix}{sign}{off}:00")
        }
        [h, b':', m1, m2] if h.is_ascii_digit() && m1.is_ascii_digit() && m2.is_ascii_digit() => {
            format!("{prefix}{sign}0{off}")
        }
        [h1, h2, b':', m1, m2]
            if h1.is_ascii_digit()
                && h2.is_ascii_digit()
                && m1.is_ascii_digit()
                && m2.is_ascii_digit() =>
        {
            format!("{prefix}{sign}{off}")
        }
        [h1, h2, m1, m2]
            if h1.is_ascii_digit()
                && h2.is_ascii_digit()
                && m1.is_ascii_digit()
                && m2.is_ascii_digit() =>
        {
            let hh = off.get(..2).unwrap_or("00");
            let mm = off.get(2..4).unwrap_or("00");
            format!("{prefix}{sign}{hh}:{mm}")
        }
        _ => s.to_owned(),
    }
}
