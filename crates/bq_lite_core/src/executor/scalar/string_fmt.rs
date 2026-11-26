use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

use crate::error::{BqError, BqResult};
use crate::types::BqValue;

pub(super) fn eval_format(args: Vec<BqValue>) -> BqResult<BqValue> {
    let mut it = args.into_iter();
    let fmt_val = it
        .next()
        .ok_or_else(|| BqError::query("FORMAT requires at least 1 argument"))?;
    if fmt_val.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::String(fmt_str) = fmt_val else {
        return Err(BqError::query("FORMAT template must be STRING"));
    };
    let rest: Vec<BqValue> = it.collect();
    let mut out = String::new();
    let mut chars = fmt_str.chars().peekable();
    let mut arg_idx = 0_usize;
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        if chars.peek() == Some(&'%') {
            chars.next();
            out.push('%');
            continue;
        }
        let spec = parse_format_spec(&mut chars)?;
        let val = rest
            .get(arg_idx)
            .ok_or_else(|| BqError::query("Not enough arguments for FORMAT string"))?;
        arg_idx += 1;
        out.push_str(&format_value_with_spec(val, spec)?);
    }
    Ok(BqValue::String(out))
}

#[derive(Clone, Copy)]
struct FormatSpec {
    zero_pad: bool,
    left_align: bool,
    plus_sign: bool,
    width: Option<usize>,
    precision: Option<usize>,
    kind: char,
}

fn parse_format_spec(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> BqResult<FormatSpec> {
    let mut zero_pad = false;
    let mut left_align = false;
    let mut plus_sign = false;
    while let Some(&c) = chars.peek() {
        match c {
            '0' => zero_pad = true,
            '-' => left_align = true,
            '+' => plus_sign = true,
            ' ' | '#' | '\'' => {}
            _ => break,
        }
        chars.next();
    }
    let mut width_str = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            width_str.push(c);
            chars.next();
        } else {
            break;
        }
    }
    let width = if width_str.is_empty() {
        None
    } else {
        Some(
            width_str
                .parse::<usize>()
                .map_err(|_| BqError::semantic("Invalid FORMAT width"))?,
        )
    };
    let precision = if chars.peek() == Some(&'.') {
        chars.next();
        let mut prec_str = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() {
                prec_str.push(c);
                chars.next();
            } else {
                break;
            }
        }
        Some(if prec_str.is_empty() {
            0
        } else {
            prec_str
                .parse::<usize>()
                .map_err(|_| BqError::semantic("Invalid FORMAT precision"))?
        })
    } else {
        None
    };
    let kind = chars
        .next()
        .ok_or_else(|| BqError::semantic("Trailing '%' in FORMAT string"))?;
    Ok(FormatSpec {
        zero_pad,
        left_align,
        plus_sign,
        width,
        precision,
        kind,
    })
}

fn format_value_with_spec(val: &BqValue, spec: FormatSpec) -> BqResult<String> {
    let raw = match spec.kind {
        's' | 'p' => val.to_string(),
        't' => format_t_human(val),
        'T' => format_t_literal(val),
        'd' | 'i' | 'u' | 'o' | 'x' | 'X' => format_int_spec(val, spec)?,
        'f' | 'F' | 'e' | 'E' | 'g' | 'G' => format_float_spec(val, spec)?,
        other => {
            return Err(BqError::semantic(format!(
                "Unsupported FORMAT specifier '%{other}'"
            )))
        }
    };
    if let Some(w) = spec.width {
        if raw.chars().count() < w {
            let pad_len = w - raw.chars().count();
            let pad_ch = if spec.zero_pad && !spec.left_align {
                '0'
            } else {
                ' '
            };
            let pad: String = std::iter::repeat_n(pad_ch, pad_len).collect();
            return Ok(if spec.left_align {
                format!("{raw}{pad}")
            } else if pad_ch == '0' && (raw.starts_with('-') || raw.starts_with('+')) {
                let (sign, rest) = raw.split_at(1);
                format!("{sign}{pad}{rest}")
            } else {
                format!("{pad}{raw}")
            });
        }
    }
    Ok(raw)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Remaining variants use their canonical Display representation for %t"
)]
fn format_t_human(val: &BqValue) -> String {
    match val {
        BqValue::Null => "NULL".to_owned(),
        BqValue::Timestamp(ts) => {
            if ts.timestamp_subsec_micros() == 0 {
                ts.format("%Y-%m-%d %H:%M:%S+00").to_string()
            } else {
                let full = ts.format("%Y-%m-%d %H:%M:%S%.6f").to_string();
                let trimmed = full.trim_end_matches('0').trim_end_matches('.');
                format!("{trimmed}+00")
            }
        }
        BqValue::Array(items) => {
            let parts: Vec<String> = items.iter().map(format_t_human).collect();
            format!("[{}]", parts.join(", "))
        }
        BqValue::Struct(fields) => {
            let parts: Vec<String> = fields.iter().map(|f| format_t_human(&f.value)).collect();
            format!("({})", parts.join(", "))
        }
        other => other.to_string(),
    }
}

fn format_t_literal(val: &BqValue) -> String {
    match val {
        BqValue::Null => "NULL".to_owned(),
        BqValue::Bool(b) => b.to_string(),
        BqValue::Int64(n) => n.to_string(),
        BqValue::Float64(f) => {
            if f.is_nan() {
                "CAST('NaN' AS FLOAT64)".to_owned()
            } else if f.is_infinite() {
                if f.is_sign_negative() {
                    "CAST('-Infinity' AS FLOAT64)".to_owned()
                } else {
                    "CAST('Infinity' AS FLOAT64)".to_owned()
                }
            } else {
                let s = f.to_string();
                if s.contains('.') || s.contains('e') || s.contains('E') {
                    s
                } else {
                    format!("{s}.0")
                }
            }
        }
        BqValue::Numeric(n) => format!("NUMERIC \"{n}\""),
        BqValue::BigNumeric(bn) => format!("BIGNUMERIC \"{bn}\""),
        BqValue::String(s) => format!("\"{}\"", escape_dq(s)),
        BqValue::Bytes(b) => format!("b\"{}\"", encode_bytes_literal(b)),
        BqValue::Date(d) => format!("DATE \"{d}\""),
        BqValue::Datetime(dt) => format!("DATETIME \"{dt}\""),
        BqValue::Time(t) => format!("TIME \"{t}\""),
        BqValue::Timestamp(_) => format!("TIMESTAMP \"{}\"", format_t_human(val)),
        BqValue::Interval(iv) => format!("INTERVAL \"{iv}\""),
        BqValue::Range(r) => format!(
            "RANGE<{}> \"[{}, {})\"",
            r.elem_type.to_bq_type(),
            r.start
                .as_ref()
                .map_or_else(|| "UNBOUNDED".to_owned(), ToString::to_string),
            r.end
                .as_ref()
                .map_or_else(|| "UNBOUNDED".to_owned(), ToString::to_string)
        ),
        BqValue::Geography(g) => format!("ST_GEOGFROMTEXT('{}')", g.replace('\'', "\\'")),
        BqValue::Json(j) => format!("JSON '{}'", j.to_string().replace('\'', "\\'")),
        BqValue::Array(items) => {
            let parts: Vec<String> = items.iter().map(format_t_literal).collect();
            format!("[{}]", parts.join(", "))
        }
        BqValue::Struct(fields) => {
            let parts: Vec<String> = fields.iter().map(|f| format_t_literal(&f.value)).collect();
            format!("({})", parts.join(", "))
        }
    }
}

fn escape_dq(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

fn encode_bytes_literal(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        match b {
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            0x20..=0x7e => out.push(char::from(b)),
            other => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\x{other:02x}");
            }
        }
    }
    out
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Integer format specifiers only accept INT64 or NULL"
)]
fn format_int_spec(val: &BqValue, spec: FormatSpec) -> BqResult<String> {
    match val {
        BqValue::Null => Ok("NULL".to_owned()),
        BqValue::Int64(n) => {
            let s = match spec.kind {
                'o' => format!("{n:o}"),
                'x' => format!("{n:x}"),
                'X' => format!("{n:X}"),
                _ => n.to_string(),
            };
            if spec.plus_sign && *n >= 0 && matches!(spec.kind, 'd' | 'i') {
                Ok(format!("+{s}"))
            } else {
                Ok(s)
            }
        }
        other => Err(BqError::query(format!(
            "FORMAT '%{}' expects INT64, got {}",
            spec.kind,
            other.infer_type()?
        ))),
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "FORMAT float specifiers promote numeric inputs to f64"
)]
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Float format specifiers only accept numeric types or NULL"
)]
fn format_float_spec(val: &BqValue, spec: FormatSpec) -> BqResult<String> {
    let f = match val {
        BqValue::Null => return Ok("NULL".to_owned()),
        BqValue::Float64(v) => *v,
        BqValue::Int64(n) => *n as f64,
        BqValue::Numeric(n) => n.to_f64(),
        BqValue::BigNumeric(bn) => bn.to_f64(),
        other => {
            return Err(BqError::query(format!(
                "FORMAT '%{}' expects numeric value, got {}",
                spec.kind,
                other.infer_type()?
            )))
        }
    };
    let prec = spec.precision.unwrap_or(6);
    let mut s = match spec.kind {
        'f' | 'F' => format!("{f:.prec$}"),
        'e' => format!("{f:.prec$e}"),
        'E' => format!("{f:.prec$E}"),
        _ => {
            if spec.precision.is_some() {
                format!("{f:.prec$}")
            } else {
                f.to_string()
            }
        }
    };
    if spec.plus_sign && f.is_sign_positive() && f.is_finite() {
        s.insert(0, '+');
    }
    Ok(s)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "TO_BASE64 expects BYTES or NULL"
)]
pub(super) fn eval_to_base64(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("TO_BASE64 expects 1 argument"))?;
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::Bytes(b) => Ok(BqValue::String(BASE64.encode(b))),
        other => Err(BqError::query(format!(
            "TO_BASE64 expects BYTES, got {}",
            other.infer_type()?
        ))),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "FROM_BASE64 expects STRING or NULL"
)]
pub(super) fn eval_from_base64(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("FROM_BASE64 expects 1 argument"))?;
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::String(s) => BASE64
            .decode(s.as_bytes())
            .map(BqValue::Bytes)
            .map_err(|e| BqError::semantic(format!("Invalid base64 string: {e}"))),
        other => Err(BqError::query(format!(
            "FROM_BASE64 expects STRING, got {}",
            other.infer_type()?
        ))),
    }
}

fn encode_hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        let hi = usize::from(b >> 4);
        let lo = usize::from(b & 0x0f);
        if let (Some(&c1), Some(&c2)) = (HEX.get(hi), HEX.get(lo)) {
            out.push(char::from(c1));
            out.push(char::from(c2));
        }
    }
    out
}

fn decode_hex_str(s: &str) -> Result<Vec<u8>, &'static str> {
    let bytes = s.as_bytes();
    if bytes.len() % 2 != 0 {
        return Err("odd number of digits");
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let (Some(&hi_b), Some(&lo_b)) = (pair.first(), pair.get(1)) else {
            return Err("invalid hex length");
        };
        let hi = hex_nibble(hi_b).ok_or("invalid hex character")?;
        let lo = hex_nibble(lo_b).ok_or("invalid hex character")?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

const fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "TO_HEX expects BYTES or NULL"
)]
pub(super) fn eval_to_hex(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("TO_HEX expects 1 argument"))?;
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::Bytes(b) => Ok(BqValue::String(encode_hex_lower(&b))),
        other => Err(BqError::query(format!(
            "TO_HEX expects BYTES, got {}",
            other.infer_type()?
        ))),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "FROM_HEX expects STRING or NULL"
)]
pub(super) fn eval_from_hex(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("FROM_HEX expects 1 argument"))?;
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::String(s) => decode_hex_str(s.trim())
            .map(BqValue::Bytes)
            .map_err(|e| BqError::semantic(format!("Invalid hex string: {e}"))),
        other => Err(BqError::query(format!(
            "FROM_HEX expects STRING, got {}",
            other.infer_type()?
        ))),
    }
}
