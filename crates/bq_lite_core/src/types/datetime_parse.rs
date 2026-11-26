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
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
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
        if let Ok(dt) = DateTime::parse_from_str(trimmed, fmt) {
            return Ok(dt.with_timezone(&Utc));
        }
    }
    let no_utc = trimmed
        .strip_suffix(" UTC")
        .or_else(|| trimmed.strip_suffix(" utc"))
        .or_else(|| trimmed.strip_suffix('Z'))
        .unwrap_or(trimmed);
    if let Ok(dt) = parse_naive_datetime(no_utc) {
        return Ok(DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc));
    }
    Err(BqError::semantic(format!(
        "Invalid TIMESTAMP string: '{s}'"
    )))
}
