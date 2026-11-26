use std::cmp::Ordering;

use crate::error::{BqError, BqResult};
use crate::types::BqValue;

use super::string_fmt::{
    eval_format, eval_from_base64, eval_from_hex, eval_to_base64, eval_to_hex,
};

pub(super) const STRING_FUNCTIONS: &[&str] = &[
    "BYTE_LENGTH",
    "CHAR_LENGTH",
    "CONCAT",
    "ENDS_WITH",
    "FORMAT",
    "FROM_BASE64",
    "FROM_HEX",
    "INITCAP",
    "INSTR",
    "LEFT",
    "LENGTH",
    "LOWER",
    "LPAD",
    "LTRIM",
    "REPEAT",
    "REPLACE",
    "REVERSE",
    "RIGHT",
    "RPAD",
    "RTRIM",
    "SPLIT",
    "STARTS_WITH",
    "STRPOS",
    "SUBSTR",
    "SUBSTRING",
    "TO_BASE64",
    "TO_HEX",
    "TRANSLATE",
    "TRIM",
    "UPPER",
];

pub(super) fn eval_string_fn(name: &str, args: Vec<BqValue>) -> BqResult<Option<BqValue>> {
    match name {
        "CONCAT" => eval_concat_fn(args).map(Some),
        "SUBSTR" | "SUBSTRING" => eval_substr(args).map(Some),
        "LENGTH" | "CHAR_LENGTH" => eval_length(args, false).map(Some),
        "BYTE_LENGTH" => eval_length(args, true).map(Some),
        "LOWER" => eval_unary_str(args, "LOWER", str::to_lowercase).map(Some),
        "UPPER" => eval_unary_str(args, "UPPER", str::to_uppercase).map(Some),
        "INITCAP" => eval_initcap(args).map(Some),
        "REVERSE" => eval_unary_str(args, "REVERSE", |s| s.chars().rev().collect()).map(Some),
        "STRPOS" => eval_strpos(args).map(Some),
        "INSTR" => eval_instr(args).map(Some),
        "SPLIT" => eval_split(args).map(Some),
        "TRIM" => eval_trim(args, true, true).map(Some),
        "LTRIM" => eval_trim(args, true, false).map(Some),
        "RTRIM" => eval_trim(args, false, true).map(Some),
        "LPAD" => eval_pad(args, true).map(Some),
        "RPAD" => eval_pad(args, false).map(Some),
        "REPLACE" => eval_replace(args).map(Some),
        "TRANSLATE" => eval_translate(args).map(Some),
        "REPEAT" => eval_repeat(args).map(Some),
        "LEFT" => eval_left_right(args, true).map(Some),
        "RIGHT" => eval_left_right(args, false).map(Some),
        "STARTS_WITH" => eval_starts_ends(args, true).map(Some),
        "ENDS_WITH" => eval_starts_ends(args, false).map(Some),
        "FORMAT" => eval_format(args).map(Some),
        "TO_BASE64" => eval_to_base64(args).map(Some),
        "FROM_BASE64" => eval_from_base64(args).map(Some),
        "TO_HEX" => eval_to_hex(args).map(Some),
        "FROM_HEX" => eval_from_hex(args).map(Some),
        _ => Ok(None),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "CONCAT expects STRING or NULL"
)]
fn eval_concat_fn(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.is_empty() {
        return Err(BqError::query("CONCAT requires at least 1 argument"));
    }
    let mut out = String::new();
    for a in args {
        match a {
            BqValue::Null => return Ok(BqValue::Null),
            BqValue::String(s) => out.push_str(&s),
            other => {
                return Err(BqError::query(format!(
                    "CONCAT expects STRING, got {}",
                    other.infer_type()?
                )))
            }
        }
    }
    Ok(BqValue::String(out))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Unary string helpers accept STRING or NULL"
)]
fn eval_unary_str(
    args: Vec<BqValue>,
    name: &str,
    f: impl FnOnce(&str) -> String,
) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query(format!("{name} expects 1 argument")))?;
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::String(s) => Ok(BqValue::String(f(&s))),
        other => Err(BqError::query(format!(
            "{name} expects STRING, got {}",
            other.infer_type()?
        ))),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "LENGTH expects STRING, BYTES, or NULL"
)]
fn eval_length(args: Vec<BqValue>, byte_len: bool) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("LENGTH expects 1 argument"))?;
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::String(s) => {
            let len = if byte_len { s.len() } else { s.chars().count() };
            Ok(BqValue::Int64(i64::try_from(len).unwrap_or(i64::MAX)))
        }
        BqValue::Bytes(b) => Ok(BqValue::Int64(i64::try_from(b.len()).unwrap_or(i64::MAX))),
        other => Err(BqError::query(format!(
            "LENGTH expects STRING or BYTES, got {}",
            other.infer_type()?
        ))),
    }
}

fn eval_substr(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(BqError::query("SUBSTR expects 2 or 3 arguments"));
    }
    let mut it = args.into_iter();
    let val = it.next().unwrap_or(BqValue::Null);
    let pos_v = it.next().unwrap_or(BqValue::Null);
    let len_v = it.next();
    if val.is_null() || pos_v.is_null() || len_v.as_ref().is_some_and(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    let BqValue::String(s) = val else {
        return Err(BqError::query("SUBSTR expects STRING"));
    };
    let BqValue::Int64(pos) = pos_v else {
        return Err(BqError::query("SUBSTR position must be INT64"));
    };
    let chars: Vec<char> = s.chars().collect();
    let total = chars.len();
    let start = match pos.cmp(&0) {
        Ordering::Greater => usize::try_from(pos - 1).unwrap_or(usize::MAX).min(total),
        Ordering::Less => {
            let back = usize::try_from(pos.unsigned_abs()).unwrap_or(usize::MAX);
            total.saturating_sub(back)
        }
        Ordering::Equal => 0,
    };
    let take = match len_v {
        Some(BqValue::Int64(n)) if n < 0 => {
            return Err(BqError::semantic("SUBSTR length cannot be negative"))
        }
        Some(BqValue::Int64(n)) => usize::try_from(n).unwrap_or(usize::MAX),
        Some(_) => return Err(BqError::query("SUBSTR length must be INT64")),
        None => total,
    };
    let end = start.saturating_add(take).min(total);
    let slice: String = chars.get(start..end).unwrap_or(&[]).iter().collect();
    Ok(BqValue::String(slice))
}

fn eval_initcap(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("INITCAP expects 1 argument"))?;
    let BqValue::String(s) = arg else {
        return if arg.is_null() {
            Ok(BqValue::Null)
        } else {
            Err(BqError::query("INITCAP expects STRING"))
        };
    };
    let mut out = String::with_capacity(s.len());
    let mut new_word = true;
    for ch in s.chars() {
        if !ch.is_alphanumeric() {
            new_word = true;
            out.push(ch);
        } else if new_word {
            out.extend(ch.to_uppercase());
            new_word = false;
        } else {
            out.extend(ch.to_lowercase());
        }
    }
    Ok(BqValue::String(out))
}

fn eval_strpos(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [hay, needle]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("STRPOS expects 2 arguments"))?;
    if hay.is_null() || needle.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::String(h), BqValue::String(n)) = (hay, needle) else {
        return Err(BqError::query("STRPOS expects STRING arguments"));
    };
    let pos = h.find(&n).map_or(0, |byte_idx| {
        i64::try_from(h.get(..byte_idx).unwrap_or("").chars().count() + 1).unwrap_or(i64::MAX)
    });
    Ok(BqValue::Int64(pos))
}

fn eval_instr(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 4 {
        return Err(BqError::query("INSTR expects 2 to 4 arguments"));
    }
    let mut it = args.into_iter();
    let hay = it.next().unwrap_or(BqValue::Null);
    let needle = it.next().unwrap_or(BqValue::Null);
    if hay.is_null() || needle.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::String(h), BqValue::String(n)) = (hay, needle) else {
        return Err(BqError::query("INSTR expects STRING"));
    };
    let pos = h.find(&n).map_or(0, |byte_idx| {
        i64::try_from(h.get(..byte_idx).unwrap_or("").chars().count() + 1).unwrap_or(i64::MAX)
    });
    Ok(BqValue::Int64(pos))
}

fn eval_split(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(BqError::query("SPLIT expects 1 or 2 arguments"));
    }
    let mut it = args.into_iter();
    let s = it.next().unwrap_or(BqValue::Null);
    let sep = it.next();
    if s.is_null() || sep.as_ref().is_some_and(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    let BqValue::String(text) = s else {
        return Err(BqError::query("SPLIT expects STRING"));
    };
    let delim = match sep {
        Some(BqValue::String(d)) => d,
        Some(_) => return Err(BqError::query("SPLIT delimiter must be STRING")),
        None => ",".to_owned(),
    };
    let parts = if delim.is_empty() {
        text.chars()
            .map(|c| BqValue::String(c.to_string()))
            .collect()
    } else {
        text.split(&delim)
            .map(|p| BqValue::String(p.to_owned()))
            .collect()
    };
    Ok(BqValue::Array(parts))
}

fn eval_trim(args: Vec<BqValue>, left: bool, right: bool) -> BqResult<BqValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(BqError::query("TRIM expects 1 or 2 arguments"));
    }
    let mut it = args.into_iter();
    let val = it.next().unwrap_or(BqValue::Null);
    let chars_v = it.next();
    if val.is_null() || chars_v.as_ref().is_some_and(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    let BqValue::String(s) = val else {
        return Err(BqError::query("TRIM expects STRING"));
    };
    let out = match chars_v {
        Some(BqValue::String(cset)) => {
            let pat: Vec<char> = cset.chars().collect();
            let s1 = if left {
                s.trim_start_matches(pat.as_slice())
            } else {
                &s
            };
            let s2 = if right {
                s1.trim_end_matches(pat.as_slice())
            } else {
                s1
            };
            s2.to_owned()
        }
        Some(_) => return Err(BqError::query("TRIM chars must be STRING")),
        None => match (left, right) {
            (true, true) => s.trim().to_owned(),
            (true, false) => s.trim_start().to_owned(),
            (false, true) => s.trim_end().to_owned(),
            (false, false) => s,
        },
    };
    Ok(BqValue::String(out))
}

fn eval_pad(args: Vec<BqValue>, left: bool) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(BqError::query("LPAD/RPAD expects 2 or 3 arguments"));
    }
    let mut it = args.into_iter();
    let v = it.next().unwrap_or(BqValue::Null);
    let len_v = it.next().unwrap_or(BqValue::Null);
    let pad_v = it.next();
    if v.is_null() || len_v.is_null() || pad_v.as_ref().is_some_and(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    let BqValue::String(s) = v else {
        return Err(BqError::query("LPAD/RPAD expects STRING"));
    };
    let BqValue::Int64(target) = len_v else {
        return Err(BqError::query("LPAD/RPAD length must be INT64"));
    };
    if target < 0 {
        return Err(BqError::semantic("LPAD/RPAD length cannot be negative"));
    }
    let pad_str = match pad_v {
        Some(BqValue::String(p)) => p,
        Some(_) => return Err(BqError::query("LPAD/RPAD pattern must be STRING")),
        None => " ".to_owned(),
    };
    if pad_str.is_empty() {
        return Err(BqError::semantic("LPAD/RPAD pattern cannot be empty"));
    }
    let target_len = usize::try_from(target).unwrap_or(usize::MAX);
    let cur_chars: Vec<char> = s.chars().collect();
    if cur_chars.len() >= target_len {
        return Ok(BqValue::String(
            cur_chars.into_iter().take(target_len).collect(),
        ));
    }
    let needed = target_len - cur_chars.len();
    let pad_chars: String = pad_str.chars().cycle().take(needed).collect();
    Ok(BqValue::String(if left {
        format!("{pad_chars}{s}")
    } else {
        format!("{s}{pad_chars}")
    }))
}

fn eval_replace(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [s, from, to]: [BqValue; 3] = args
        .try_into()
        .map_err(|_| BqError::query("REPLACE expects 3 arguments"))?;
    if s.is_null() || from.is_null() || to.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::String(s), BqValue::String(f), BqValue::String(t)) = (s, from, to) else {
        return Err(BqError::query("REPLACE expects STRING arguments"));
    };
    Ok(BqValue::String(s.replace(&f, &t)))
}

fn eval_translate(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [s, from, to]: [BqValue; 3] = args
        .try_into()
        .map_err(|_| BqError::query("TRANSLATE expects 3 arguments"))?;
    if s.is_null() || from.is_null() || to.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::String(s), BqValue::String(f), BqValue::String(t)) = (s, from, to) else {
        return Err(BqError::query("TRANSLATE expects STRING arguments"));
    };
    let from_chars: Vec<char> = f.chars().collect();
    let to_chars: Vec<char> = t.chars().collect();
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if let Some(pos) = from_chars.iter().position(|&c| c == ch) {
            if let Some(&rep) = to_chars.get(pos) {
                out.push(rep);
            }
        } else {
            out.push(ch);
        }
    }
    Ok(BqValue::String(out))
}

fn eval_repeat(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [s, n]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("REPEAT expects 2 arguments"))?;
    if s.is_null() || n.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::String(text), BqValue::Int64(cnt)) = (s, n) else {
        return Err(BqError::query("REPEAT expects (STRING, INT64)"));
    };
    if cnt < 0 {
        return Err(BqError::semantic("REPEAT count cannot be negative"));
    }
    let times = usize::try_from(cnt).unwrap_or(usize::MAX).min(1_000_000);
    Ok(BqValue::String(text.repeat(times)))
}

fn eval_left_right(args: Vec<BqValue>, is_left: bool) -> BqResult<BqValue> {
    let [s, n]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("LEFT/RIGHT expects 2 arguments"))?;
    if s.is_null() || n.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::String(text), BqValue::Int64(cnt)) = (s, n) else {
        return Err(BqError::query("LEFT/RIGHT expects (STRING, INT64)"));
    };
    if cnt < 0 {
        return Err(BqError::semantic("LEFT/RIGHT length cannot be negative"));
    }
    let k = usize::try_from(cnt).unwrap_or(usize::MAX);
    let chars: Vec<char> = text.chars().collect();
    let out: String = if is_left {
        chars.into_iter().take(k).collect()
    } else {
        let start = chars.len().saturating_sub(k);
        chars.get(start..).unwrap_or(&[]).iter().collect()
    };
    Ok(BqValue::String(out))
}

fn eval_starts_ends(args: Vec<BqValue>, starts: bool) -> BqResult<BqValue> {
    let [s, pat]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("STARTS_WITH/ENDS_WITH expects 2 arguments"))?;
    if s.is_null() || pat.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::String(text), BqValue::String(p)) = (s, pat) else {
        return Err(BqError::query("STARTS_WITH/ENDS_WITH expects STRING"));
    };
    Ok(BqValue::Bool(if starts {
        text.starts_with(&p)
    } else {
        text.ends_with(&p)
    }))
}
