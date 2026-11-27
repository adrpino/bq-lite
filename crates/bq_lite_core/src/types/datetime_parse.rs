use chrono::{
    DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Utc,
};
use chrono_tz::Tz;

use crate::error::{BqError, BqResult};

/// Unified `BigQuery` timezone representation (IANA zone or fixed offset).
#[expect(
    clippy::redundant_pub_crate,
    reason = "Re-exported pub(crate) via types::mod while satisfying unreachable_pub"
)]
#[derive(Debug, Clone, Copy)]
pub(crate) enum BqTimezone {
    Utc,
    Iana(Tz),
    Fixed(FixedOffset),
}

impl BqTimezone {
    pub(crate) fn parse(tz_str: &str) -> BqResult<Self> {
        let trimmed = tz_str.trim();
        if trimmed.eq_ignore_ascii_case("UTC") || trimmed == "Z" {
            return Ok(Self::Utc);
        }
        if let Ok(tz) = trimmed.parse::<Tz>() {
            return Ok(Self::Iana(tz));
        }
        if let Some(off) = parse_fixed_tz_offset(trimmed) {
            return Ok(Self::Fixed(off));
        }
        Err(BqError::semantic(format!("Invalid time zone: '{tz_str}'")))
    }

    #[must_use]
    pub(crate) fn utc_to_local(self, ts: DateTime<Utc>) -> NaiveDateTime {
        match self {
            Self::Utc => ts.naive_utc(),
            Self::Iana(tz) => ts.with_timezone(&tz).naive_local(),
            Self::Fixed(off) => ts.with_timezone(&off).naive_local(),
        }
    }

    pub(crate) fn local_to_utc(self, dt: NaiveDateTime) -> BqResult<DateTime<Utc>> {
        match self {
            Self::Utc => Ok(DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc)),
            Self::Iana(tz) => dt
                .and_local_timezone(tz)
                .earliest()
                .map(|d| d.with_timezone(&Utc))
                .ok_or_else(|| {
                    BqError::semantic("Ambiguous or invalid local datetime for timezone")
                }),
            Self::Fixed(off) => dt
                .and_local_timezone(off)
                .single()
                .map(|d| d.with_timezone(&Utc))
                .ok_or_else(|| BqError::semantic("Invalid local datetime for timezone offset")),
        }
    }

    #[must_use]
    pub(crate) fn format_timestamp(self, ts: DateTime<Utc>, fmt: &str) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let res = match self {
            Self::Utc => write!(out, "{}", ts.format(fmt)),
            Self::Iana(tz) => write!(out, "{}", ts.with_timezone(&tz).format(fmt)),
            Self::Fixed(off) => write!(out, "{}", ts.with_timezone(&off).format(fmt)),
        };
        if res.is_ok() {
            out
        } else {
            String::new()
        }
    }
}

fn parse_fixed_tz_offset(s: &str) -> Option<FixedOffset> {
    let rest_all = s
        .strip_prefix("UTC")
        .or_else(|| s.strip_prefix("utc"))
        .or_else(|| s.strip_prefix("GMT"))
        .or_else(|| s.strip_prefix("gmt"))
        .unwrap_or(s);
    let sign = if rest_all.starts_with('+') {
        1_i32
    } else if rest_all.starts_with('-') {
        -1_i32
    } else {
        return None;
    };
    let rest = rest_all.get(1..)?;
    let (h, m) = if let Some((hs, ms)) = rest.split_once(':') {
        if hs.is_empty() || ms.len() != 2 {
            return None;
        }
        (hs.parse::<i32>().ok()?, ms.parse::<i32>().ok()?)
    } else if rest.len() == 4 && rest.bytes().all(|b| b.is_ascii_digit()) {
        let hs = rest.get(..2)?;
        let ms = rest.get(2..4)?;
        (hs.parse::<i32>().ok()?, ms.parse::<i32>().ok()?)
    } else if (1..=2).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_digit()) {
        (rest.parse::<i32>().ok()?, 0_i32)
    } else {
        return None;
    };
    if !(0..=14).contains(&h) || !(0..=59).contains(&m) || (h == 14 && m > 0) {
        return None;
    }
    let total = sign * (h * 3600 + m * 60);
    if sign >= 0 {
        FixedOffset::east_opt(total)
    } else {
        FixedOffset::west_opt(-total)
    }
}

#[expect(
    clippy::redundant_pub_crate,
    reason = "Re-exported pub(crate) via types::mod while satisfying unreachable_pub"
)]
pub(crate) fn validate_bq_date(d: NaiveDate) -> BqResult<NaiveDate> {
    if (1..=9999).contains(&d.year()) {
        Ok(d)
    } else {
        Err(BqError::semantic(format!("DATE out of range: {d}")))
    }
}

#[expect(
    clippy::redundant_pub_crate,
    reason = "Re-exported pub(crate) via types::mod while satisfying unreachable_pub"
)]
pub(crate) fn validate_bq_datetime(dt: NaiveDateTime) -> BqResult<NaiveDateTime> {
    validate_bq_date(dt.date())?;
    Ok(dt)
}

#[expect(
    clippy::redundant_pub_crate,
    reason = "Re-exported pub(crate) via types::mod while satisfying unreachable_pub"
)]
pub(crate) fn check_max_6_frac_digits(s: &str) -> BqResult<()> {
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

/// Normalizes leap second (`:60[.fff]`) to the next minute `:00.000000` per `GoogleSQL` rules.
fn normalize_leap_time(t: NaiveTime) -> (NaiveTime, bool) {
    if t.nanosecond() >= 1_000_000_000 {
        let base = NaiveTime::from_hms_opt(t.hour(), t.minute(), 59).unwrap_or(t);
        let (next, overflow_secs) = base.overflowing_add_signed(Duration::seconds(1));
        (next, overflow_secs != 0)
    } else {
        (t, false)
    }
}

#[expect(
    clippy::redundant_pub_crate,
    reason = "Re-exported pub(crate) via types::mod while satisfying unreachable_pub"
)]
pub(crate) fn parse_time_str(s: &str) -> BqResult<NaiveTime> {
    let trimmed = s.trim();
    check_max_6_frac_digits(trimmed)?;
    let parsed = NaiveTime::parse_from_str(trimmed, "%H:%M:%S%.f")
        .or_else(|_| NaiveTime::parse_from_str(trimmed, "%H:%M:%S"))
        .map_err(|_| BqError::semantic(format!("Invalid TIME string: '{s}'")))?;
    let (norm, _) = normalize_leap_time(parsed);
    Ok(norm)
}

#[expect(
    clippy::redundant_pub_crate,
    reason = "Re-exported pub(crate) via types::mod while satisfying unreachable_pub"
)]
pub(crate) fn parse_naive_datetime(s: &str) -> BqResult<NaiveDateTime> {
    let trimmed = s.trim();
    check_max_6_frac_digits(trimmed)?;
    let normalized = normalize_datetime_t_sep(trimmed);
    let cand = normalized.as_str();
    for fmt in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(cand, fmt) {
            let (norm_t, day_rollover) = normalize_leap_time(dt.time());
            let date = if day_rollover {
                dt.date()
                    .succ_opt()
                    .ok_or_else(|| BqError::semantic("DATETIME overflow"))?
            } else {
                dt.date()
            };
            return validate_bq_datetime(NaiveDateTime::new(date, norm_t));
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(cand, "%Y-%m-%d") {
        let d = validate_bq_date(d)?;
        return Ok(d.and_hms_opt(0, 0, 0).unwrap_or_default());
    }
    Err(BqError::semantic(format!("Invalid DATETIME string: '{s}'")))
}

fn normalize_datetime_t_sep(s: &str) -> String {
    if let Some(pos) = s.find('t') {
        if pos >= 8 && s.as_bytes().get(pos + 1).is_some_and(u8::is_ascii_digit) {
            let mut out = s.to_owned();
            out.replace_range(pos..=pos, "T");
            return out;
        }
    }
    s.to_owned()
}

#[expect(
    clippy::redundant_pub_crate,
    reason = "Re-exported pub(crate) via types::mod while satisfying unreachable_pub"
)]
pub(crate) fn parse_timestamp_str(s: &str) -> BqResult<DateTime<Utc>> {
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
