use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};

use crate::ast::DataTypeAst;
use crate::error::{BqError, BqResult};
use crate::types::coercion::cast_value;
use crate::types::{validate_bq_date, validate_bq_datetime, BqTimezone, BqType, BqValue};

/// Evaluates `CAST(val AS ty [FORMAT fmt [AT TIME ZONE tz]])` with type parameter validation.
pub(super) fn eval_cast_with_format(
    val: BqValue,
    ast_ty: &DataTypeAst,
    fmt_opt: Option<BqValue>,
    tz_opt: Option<BqValue>,
) -> BqResult<BqValue> {
    if let Some(ref f) = fmt_opt {
        if f.is_null() {
            return Ok(BqValue::Null);
        }
    }
    if let Some(ref tz) = tz_opt {
        if tz.is_null() {
            return Ok(BqValue::Null);
        }
    }
    let fmt_str = match fmt_opt {
        Some(BqValue::String(s)) => Some(s),
        Some(_) => return Err(BqError::query("CAST FORMAT expression must be STRING")),
        None => None,
    };
    let tz = match tz_opt {
        Some(BqValue::String(s)) => Some(BqTimezone::parse(&s)?),
        Some(_) => {
            return Err(BqError::query(
                "CAST AT TIME ZONE expression must be STRING",
            ))
        }
        None => None,
    };
    if val.is_null() {
        return Ok(BqValue::Null);
    }
    let target = ast_ty.to_bq_type()?;
    let result = if let Some(fmt) = fmt_str {
        cast_with_format_impl(val, &target, &fmt, tz)?
    } else {
        cast_value(val, &target)?
    };
    super::literals::validate_type_params(&result, ast_ty)?;
    Ok(result)
}

fn cast_with_format_impl(
    val: BqValue,
    target: &BqType,
    fmt: &str,
    tz: Option<BqTimezone>,
) -> BqResult<BqValue> {
    match (&val, target) {
        (BqValue::Bytes(bytes), BqType::String) => format_bytes_to_string(bytes, fmt),
        (BqValue::String(s), BqType::Bytes) => parse_string_to_bytes(s, fmt),
        (BqValue::Date(d), BqType::String) => Ok(BqValue::String(format_bq_temporal(
            fmt,
            d.and_hms_opt(0, 0, 0).unwrap_or_default(),
            None,
        )?)),
        (BqValue::Datetime(dt), BqType::String) => {
            Ok(BqValue::String(format_bq_temporal(fmt, *dt, None)?))
        }
        (BqValue::Time(t), BqType::String) => {
            let d = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap_or_default();
            Ok(BqValue::String(format_bq_temporal(
                fmt,
                NaiveDateTime::new(d, *t),
                None,
            )?))
        }
        (BqValue::Timestamp(ts), BqType::String) => {
            let zone = tz.unwrap_or(BqTimezone::Utc);
            let local = zone.utc_to_local(*ts);
            Ok(BqValue::String(format_bq_temporal(
                fmt,
                local,
                Some((*ts, zone)),
            )?))
        }
        (BqValue::String(s), BqType::Date) => parse_bq_date_fmt(s, fmt).map(BqValue::Date),
        (BqValue::String(s), BqType::Datetime) => {
            parse_bq_datetime_fmt(s, fmt).map(BqValue::Datetime)
        }
        (BqValue::String(s), BqType::Time) => parse_bq_time_fmt(s, fmt).map(BqValue::Time),
        (BqValue::String(s), BqType::Timestamp) => {
            parse_bq_timestamp_fmt(s, fmt, tz).map(BqValue::Timestamp)
        }
        _ => cast_value(val, target),
    }
}

fn format_bytes_to_string(bytes: &[u8], fmt: &str) -> BqResult<BqValue> {
    use std::fmt::Write as _;
    match fmt.to_ascii_lowercase().as_str() {
        "base2" => {
            let mut out = String::with_capacity(bytes.len() * 8);
            for &b in bytes {
                let _ = write!(out, "{b:08b}");
            }
            Ok(BqValue::String(out))
        }
        "base8" => Ok(BqValue::String(encode_octal_bytes(bytes))),
        "base16" | "hex" => {
            let mut out = String::with_capacity(bytes.len() * 2);
            for &b in bytes {
                let _ = write!(out, "{b:02x}");
            }
            Ok(BqValue::String(out))
        }
        "base32" => Ok(BqValue::String(encode_base32(bytes))),
        "base64" => Ok(BqValue::String(BASE64.encode(bytes))),
        "base64m" => {
            let raw = BASE64.encode(bytes);
            let mut out = String::with_capacity(raw.len() + raw.len() / 76);
            for (idx, chunk) in raw.as_bytes().chunks(76).enumerate() {
                if idx > 0 {
                    out.push('\n');
                }
                if let Ok(s) = std::str::from_utf8(chunk) {
                    out.push_str(s);
                }
            }
            Ok(BqValue::String(out))
        }
        "ascii" => {
            if !bytes.iter().all(u8::is_ascii) {
                return Err(BqError::semantic("Failed to decode invalid ASCII string"));
            }
            String::from_utf8(bytes.to_vec())
                .map(BqValue::String)
                .map_err(|_| BqError::semantic("Failed to decode invalid ASCII string"))
        }
        "utf8" | "utf-8" => String::from_utf8(bytes.to_vec())
            .map(BqValue::String)
            .map_err(|_| BqError::semantic("Invalid UTF-8 bytes")),
        _ => Err(BqError::semantic(format!(
            "Unsupported BYTES format '{fmt}'"
        ))),
    }
}

fn parse_string_to_bytes(s: &str, fmt: &str) -> BqResult<BqValue> {
    match fmt.to_ascii_lowercase().as_str() {
        "base2" => decode_base2(s).map(BqValue::Bytes),
        "base8" => decode_octal_bytes(s).map(BqValue::Bytes),
        "base16" | "hex" => decode_hex(s).map(BqValue::Bytes),
        "base32" => decode_base32(s).map(BqValue::Bytes),
        "base64" => BASE64
            .decode(s)
            .map(BqValue::Bytes)
            .map_err(|_| BqError::semantic("Invalid BASE64 input")),
        "base64m" => {
            let stripped: String = s.chars().filter(|c| !c.is_ascii_whitespace()).collect();
            BASE64
                .decode(&stripped)
                .map(BqValue::Bytes)
                .map_err(|_| BqError::semantic("Invalid BASE64M input"))
        }
        "ascii" => {
            if !s.is_ascii() {
                return Err(BqError::semantic("Failed to decode invalid ASCII string"));
            }
            Ok(BqValue::Bytes(s.as_bytes().to_vec()))
        }
        "utf8" | "utf-8" => Ok(BqValue::Bytes(s.as_bytes().to_vec())),
        _ => Err(BqError::semantic(format!(
            "Unsupported BYTES format '{fmt}'"
        ))),
    }
}

fn encode_octal_bytes(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let total_bits = bytes.len() * 8;
    let rem = total_bits % 3;
    let pad = if rem == 0 { 0 } else { 3 - rem };
    let mut out = String::new();
    let mut acc = 0_u32;
    let mut bits = pad;
    for &b in bytes {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 3 {
            bits -= 3;
            let digit = (acc >> bits) & 0x7;
            out.push(char::from(b'0' + u8::try_from(digit).unwrap_or(0)));
        }
    }
    out
}

fn decode_octal_bytes(s: &str) -> BqResult<Vec<u8>> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    if !s.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return Err(BqError::semantic("Invalid BASE8 input"));
    }
    let total_bits = s.len() * 3;
    let byte_count = total_bits / 8;
    let extra_bits = total_bits % 8;
    let mut bits_vec = Vec::with_capacity(total_bits);
    for b in s.bytes() {
        let d = b - b'0';
        bits_vec.push((d >> 2) & 1);
        bits_vec.push((d >> 1) & 1);
        bits_vec.push(d & 1);
    }
    if bits_vec.iter().take(extra_bits).any(|&bit| bit != 0) {
        return Err(BqError::semantic("Invalid BASE8 padding bits"));
    }
    let mut out = Vec::with_capacity(byte_count);
    for chunk in bits_vec.get(extra_bits..).unwrap_or(&[]).chunks_exact(8) {
        let mut byte = 0_u8;
        for &bit in chunk {
            byte = (byte << 1) | bit;
        }
        out.push(byte);
    }
    Ok(out)
}

fn decode_base2(s: &str) -> BqResult<Vec<u8>> {
    if s.len() % 8 != 0 || !s.bytes().all(|b| b == b'0' || b == b'1') {
        return Err(BqError::semantic("Invalid BASE2 input"));
    }
    let mut out = Vec::with_capacity(s.len() / 8);
    for chunk in s.as_bytes().chunks_exact(8) {
        let mut byte = 0_u8;
        for &b in chunk {
            byte = (byte << 1) | u8::from(b == b'1');
        }
        out.push(byte);
    }
    Ok(out)
}

fn decode_hex(s: &str) -> BqResult<Vec<u8>> {
    if s.len() % 2 != 0 {
        return Err(BqError::semantic("Invalid HEX input"));
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for chunk in s.as_bytes().chunks_exact(2) {
        let hi = hex_nibble(chunk.first().copied().unwrap_or(0))?;
        let lo = hex_nibble(chunk.get(1).copied().unwrap_or(0))?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> BqResult<u8> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(BqError::semantic("Invalid HEX digit")),
    }
}

fn encode_base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    for chunk in bytes.chunks(5) {
        let mut buf = [0_u8; 5];
        for (i, &b) in chunk.iter().enumerate() {
            if let Some(slot) = buf.get_mut(i) {
                *slot = b;
            }
        }
        let n = (u64::from(buf[0]) << 32)
            | (u64::from(buf[1]) << 24)
            | (u64::from(buf[2]) << 16)
            | (u64::from(buf[3]) << 8)
            | u64::from(buf[4]);
        let chars_count = match chunk.len() {
            1 => 2,
            2 => 4,
            3 => 5,
            4 => 7,
            _ => 8,
        };
        for i in 0..8 {
            if i < chars_count {
                let idx = usize::try_from((n >> (35 - i * 5)) & 0x1F).unwrap_or(0);
                out.push(char::from(ALPHABET.get(idx).copied().unwrap_or(b'A')));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn decode_base32(s: &str) -> BqResult<Vec<u8>> {
    if s.len() % 8 != 0 {
        return Err(BqError::semantic("Invalid BASE32 input"));
    }
    let mut out = Vec::new();
    for chunk in s.as_bytes().chunks_exact(8) {
        let pad = chunk.iter().rev().take_while(|&&b| b == b'=').count();
        let data_len = 8 - pad;
        let mut acc = 0_u64;
        for &b in chunk.iter().take(data_len) {
            let val = match b.to_ascii_uppercase() {
                b'A'..=b'Z' => u64::from(b.to_ascii_uppercase() - b'A'),
                b'2'..=b'7' => u64::from(b - b'2' + 26),
                _ => return Err(BqError::semantic("Invalid BASE32 character")),
            };
            acc = (acc << 5) | val;
        }
        acc <<= pad * 5;
        let out_bytes = match pad {
            0 => 5,
            1 => 4,
            3 => 3,
            4 => 2,
            6 => 1,
            _ => return Err(BqError::semantic("Invalid BASE32 padding")),
        };
        for i in 0..out_bytes {
            out.push(u8::try_from((acc >> (32 - i * 8)) & 0xFF).unwrap_or(0));
        }
    }
    Ok(out)
}

fn translate_bq_format_to_chrono(fmt: &str) -> String {
    let mut out = String::with_capacity(fmt.len() * 2);
    let upper = fmt.to_ascii_uppercase();
    let bytes = fmt.as_bytes();
    let mut i = 0_usize;
    let tokens: &[(&str, &str)] = &[
        ("YYYY", "%Y"),
        ("YY", "%y"),
        ("MONTH", "%B"),
        ("MON", "%b"),
        ("MM", "%m"),
        ("DDD", "%j"),
        ("DD", "%d"),
        ("DAY", "%A"),
        ("DY", "%a"),
        ("HH24", "%H"),
        ("HH12", "%I"),
        ("HH", "%I"),
        ("MI", "%M"),
        ("SS", "%S"),
        ("FF6", "%6f"),
        ("FF3", "%3f"),
        ("FF", "%.f"),
        ("TZH:TZM", "%:z"),
        ("TZH", "%z"),
        ("AM", "%p"),
        ("PM", "%p"),
    ];
    while i < bytes.len() {
        let rest_upper = upper.get(i..).unwrap_or("");
        if let Some(&(bq_tok, ch_tok)) = tokens.iter().find(|&&(t, _)| rest_upper.starts_with(t)) {
            out.push_str(ch_tok);
            i += bq_tok.len();
        } else {
            let ch = fmt.get(i..).and_then(|s| s.chars().next()).unwrap_or(' ');
            if ch == '%' {
                out.push_str("%%");
            } else {
                out.push(ch);
            }
            i += ch.len_utf8();
        }
    }
    out
}

fn format_bq_temporal(
    fmt: &str,
    local: NaiveDateTime,
    ts_tz: Option<(DateTime<Utc>, BqTimezone)>,
) -> BqResult<String> {
    use std::fmt::Write as _;
    let chrono_fmt = translate_bq_format_to_chrono(fmt);
    if let Some((ts, zone)) = ts_tz {
        return Ok(zone.format_timestamp(ts, &chrono_fmt));
    }
    let mut out = String::new();
    write!(out, "{}", local.format(&chrono_fmt))
        .map_err(|_| BqError::semantic(format!("Invalid temporal format string: '{fmt}'")))?;
    Ok(out)
}

fn parse_bq_date_fmt(s: &str, fmt: &str) -> BqResult<NaiveDate> {
    if fmt.is_empty() && s.is_empty() {
        return Ok(NaiveDate::from_ymd_opt(1970, 1, 1).unwrap_or_default());
    }
    let chrono_fmt = translate_bq_format_to_chrono(fmt);
    NaiveDate::parse_from_str(s.trim(), &chrono_fmt)
        .map_err(|e| BqError::semantic(format!("Failed to cast '{s}' to DATE: {e}")))
        .and_then(validate_bq_date)
}

fn parse_bq_datetime_fmt(s: &str, fmt: &str) -> BqResult<NaiveDateTime> {
    if fmt.is_empty() && s.is_empty() {
        return Ok(NaiveDate::from_ymd_opt(1970, 1, 1)
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .unwrap_or_default());
    }
    let chrono_fmt = translate_bq_format_to_chrono(fmt);
    NaiveDateTime::parse_from_str(s.trim(), &chrono_fmt)
        .or_else(|_| {
            NaiveDate::parse_from_str(s.trim(), &chrono_fmt)
                .map(|d| d.and_hms_opt(0, 0, 0).unwrap_or_default())
        })
        .map_err(|e| BqError::semantic(format!("Failed to cast '{s}' to DATETIME: {e}")))
        .and_then(validate_bq_datetime)
}

fn parse_bq_time_fmt(s: &str, fmt: &str) -> BqResult<NaiveTime> {
    if fmt.is_empty() && s.is_empty() {
        return Ok(NaiveTime::from_hms_opt(0, 0, 0).unwrap_or_default());
    }
    let chrono_fmt = translate_bq_format_to_chrono(fmt);
    NaiveTime::parse_from_str(s.trim(), &chrono_fmt)
        .map_err(|e| BqError::semantic(format!("Failed to cast '{s}' to TIME: {e}")))
}

fn parse_bq_timestamp_fmt(s: &str, fmt: &str, tz: Option<BqTimezone>) -> BqResult<DateTime<Utc>> {
    if fmt.is_empty() && s.is_empty() {
        let base = NaiveDate::from_ymd_opt(1970, 1, 1)
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .unwrap_or_default();
        return tz.unwrap_or(BqTimezone::Utc).local_to_utc(base);
    }
    let chrono_fmt = translate_bq_format_to_chrono(fmt);
    if let Ok(dt) = DateTime::parse_from_str(s.trim(), &chrono_fmt) {
        return Ok(dt.with_timezone(&Utc));
    }
    let ndt = parse_bq_datetime_fmt(s, fmt)?;
    tz.unwrap_or(BqTimezone::Utc).local_to_utc(ndt)
}
