use crate::error::{BqError, BqResult};
use crate::types::BqValue;

pub(super) fn eval_instr(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 4 {
        return Err(BqError::query("INSTR expects 2 to 4 arguments"));
    }
    let mut it = args.into_iter();
    let hay = it.next().unwrap_or(BqValue::Null);
    let needle = it.next().unwrap_or(BqValue::Null);
    let pos_v = it.next();
    let occ_v = it.next();
    if hay.is_null()
        || needle.is_null()
        || pos_v.as_ref().is_some_and(BqValue::is_null)
        || occ_v.as_ref().is_some_and(BqValue::is_null)
    {
        return Ok(BqValue::Null);
    }
    let pos = match pos_v {
        None => 1_i64,
        Some(BqValue::Int64(p)) => p,
        Some(_) => return Err(BqError::query("INSTR position must be INT64")),
    };
    let occ = match occ_v {
        None => 1_i64,
        Some(BqValue::Int64(o)) => o,
        Some(_) => return Err(BqError::query("INSTR occurrence must be INT64")),
    };
    if pos == 0 {
        return Err(BqError::semantic("INSTR position must be non-zero"));
    }
    if occ < 1 {
        return Err(BqError::semantic("INSTR occurrence must be >= 1"));
    }
    match (hay, needle) {
        (BqValue::String(h), BqValue::String(n)) => {
            let hc: Vec<char> = h.chars().collect();
            let nc: Vec<char> = n.chars().collect();
            Ok(BqValue::Int64(find_instr_slice(&hc, &nc, pos, occ)))
        }
        (BqValue::Bytes(h), BqValue::Bytes(n)) => {
            Ok(BqValue::Int64(find_instr_slice(&h, &n, pos, occ)))
        }
        _ => Err(BqError::query("INSTR expects STRING or BYTES")),
    }
}

fn find_instr_slice<T: PartialEq>(hay: &[T], needle: &[T], pos: i64, occ: i64) -> i64 {
    let h_len = hay.len();
    let n_len = needle.len();
    if n_len > h_len {
        return 0;
    }
    let max_idx = h_len - n_len;
    let mut seen = 0_i64;
    if pos > 0 {
        let Ok(start) = usize::try_from(pos - 1) else {
            return 0;
        };
        if start > max_idx {
            return 0;
        }
        for idx in start..=max_idx {
            if hay.get(idx..idx + n_len) == Some(needle) {
                seen += 1;
                if seen == occ {
                    return i64::try_from(idx + 1).unwrap_or(i64::MAX);
                }
            }
        }
        return 0;
    }
    let abs_pos = usize::try_from(pos.unsigned_abs()).unwrap_or(usize::MAX);
    if abs_pos > h_len {
        return 0;
    }
    let start_rev = (h_len - abs_pos).min(max_idx);
    for idx in (0..=start_rev).rev() {
        if hay.get(idx..idx + n_len) == Some(needle) {
            seen += 1;
            if seen == occ {
                return i64::try_from(idx + 1).unwrap_or(i64::MAX);
            }
        }
    }
    0
}

pub(super) fn eval_chr(args: &[BqValue]) -> BqResult<BqValue> {
    let [arg] = args else {
        return Err(BqError::query("CHR expects 1 argument"));
    };
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Int64(cp) = arg else {
        return Err(BqError::query("CHR expects INT64"));
    };
    let ch = u32::try_from(*cp)
        .ok()
        .and_then(char::from_u32)
        .ok_or_else(|| BqError::semantic(format!("Invalid Unicode code point for CHR: {cp}")))?;
    Ok(BqValue::String(ch.to_string()))
}

pub(super) fn eval_soundex(args: &[BqValue]) -> BqResult<BqValue> {
    let [arg] = args else {
        return Err(BqError::query("SOUNDEX expects 1 argument"));
    };
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::String(s) = arg else {
        return Err(BqError::query("SOUNDEX expects STRING"));
    };
    let mut out = String::with_capacity(4);
    let mut prev_code = 0_u8;
    let mut started = false;
    for ch in s.chars() {
        if !ch.is_ascii_alphabetic() {
            continue;
        }
        let upper = ch.to_ascii_uppercase();
        let code = soundex_digit(upper);
        if !started {
            out.push(upper);
            started = true;
            prev_code = code;
            continue;
        }
        if upper == 'H' || upper == 'W' {
            continue;
        }
        if code != 0 && code != prev_code {
            out.push(char::from(b'0' + code));
            if out.len() == 4 {
                break;
            }
        }
        prev_code = code;
    }
    if started {
        while out.len() < 4 {
            out.push('0');
        }
    }
    Ok(BqValue::String(out))
}

const fn soundex_digit(c: char) -> u8 {
    match c {
        'B' | 'F' | 'P' | 'V' => 1,
        'C' | 'G' | 'J' | 'K' | 'Q' | 'S' | 'X' | 'Z' => 2,
        'D' | 'T' => 3,
        'L' => 4,
        'M' | 'N' => 5,
        'R' => 6,
        _ => 0,
    }
}

pub(super) fn eval_split_substr(args: &[BqValue]) -> BqResult<BqValue> {
    if args.len() < 3 || args.len() > 4 {
        return Err(BqError::query("SPLIT_SUBSTR expects 3 or 4 arguments"));
    }
    if args.iter().any(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    let (Some(BqValue::String(text)), Some(BqValue::String(delim)), Some(BqValue::Int64(pos))) =
        (args.first(), args.get(1), args.get(2))
    else {
        return Err(BqError::query(
            "SPLIT_SUBSTR expects (STRING, STRING, INT64[, INT64])",
        ));
    };
    let count = match args.get(3) {
        None => None,
        Some(BqValue::Int64(c)) if *c < 0 => {
            return Err(BqError::semantic("SPLIT_SUBSTR count cannot be negative"))
        }
        Some(BqValue::Int64(c)) => Some(usize::try_from(*c).unwrap_or(usize::MAX)),
        Some(_) => return Err(BqError::query("SPLIT_SUBSTR count must be INT64")),
    };
    if delim.is_empty() {
        return Err(BqError::semantic("SPLIT_SUBSTR delimiter cannot be empty"));
    }
    let parts: Vec<&str> = text.split(delim.as_str()).collect();
    let n = parts.len();
    let start = match (*pos).cmp(&0) {
        std::cmp::Ordering::Equal => 0_usize,
        std::cmp::Ordering::Greater => usize::try_from(*pos - 1).unwrap_or(usize::MAX),
        std::cmp::Ordering::Less => {
            let back = usize::try_from(pos.unsigned_abs()).unwrap_or(usize::MAX);
            n.saturating_sub(back)
        }
    };
    if start >= n {
        return Ok(BqValue::String(String::new()));
    }
    let slice = count.map_or_else(
        || parts.get(start..).unwrap_or(&[]),
        |c| {
            parts
                .get(start..start.saturating_add(c).min(n))
                .unwrap_or(&[])
        },
    );
    Ok(BqValue::String(slice.join(delim)))
}

pub(super) fn eval_parse_numeric_fn(args: &[BqValue], is_big: bool) -> BqResult<BqValue> {
    let [arg] = args else {
        return Err(BqError::query("PARSE_NUMERIC expects 1 argument"));
    };
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::String(raw) = arg else {
        return Err(BqError::query("PARSE_NUMERIC expects STRING"));
    };
    let cleaned = normalize_parse_numeric_str(raw)?;
    if is_big {
        cleaned
            .parse::<crate::types::BqBigNumeric>()
            .map(|v| BqValue::BigNumeric(Box::new(v)))
    } else {
        cleaned
            .parse::<crate::types::BqNumeric>()
            .map(BqValue::Numeric)
    }
}

fn normalize_parse_numeric_str(raw: &str) -> BqResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(BqError::semantic("Empty PARSE_NUMERIC input"));
    }
    let (sign, rest) = trimmed.strip_prefix('-').map_or_else(
        || {
            trimmed.strip_prefix('+').map_or_else(
                || {
                    trimmed.strip_suffix('-').map_or_else(
                        || {
                            trimmed
                                .strip_suffix('+')
                                .map_or(("", trimmed), |r| ("", r.trim_end()))
                        },
                        |r| ("-", r.trim_end()),
                    )
                },
                |r| ("", r.trim_start()),
            )
        },
        |r| ("-", r.trim_start()),
    );
    if rest.chars().any(char::is_whitespace) {
        return Err(BqError::semantic(format!(
            "Invalid PARSE_NUMERIC input '{raw}'"
        )));
    }
    let no_commas = rest.replace(',', "");
    Ok(format!("{sign}{no_commas}"))
}

pub(super) fn eval_initcap(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(BqError::query("INITCAP expects 1 or 2 arguments"));
    }
    let mut it = args.into_iter();
    let val = it.next().unwrap_or(BqValue::Null);
    let delims_v = it.next();
    if val.is_null() || delims_v.as_ref().is_some_and(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    let BqValue::String(s) = val else {
        return Err(BqError::query("INITCAP expects STRING"));
    };
    let custom_delims = match delims_v {
        None => None,
        Some(BqValue::String(d)) => Some(d),
        Some(_) => return Err(BqError::query("INITCAP delimiters must be STRING")),
    };
    let mut out = String::with_capacity(s.len());
    let mut new_word = true;
    for ch in s.chars() {
        let is_delim = custom_delims
            .as_ref()
            .map_or_else(|| !ch.is_alphanumeric(), |d| d.contains(ch));
        if is_delim {
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

pub(super) fn eval_translate(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [s, from, to]: [BqValue; 3] = args
        .try_into()
        .map_err(|_| BqError::query("TRANSLATE expects 3 arguments"))?;
    if s.is_null() || from.is_null() || to.is_null() {
        return Ok(BqValue::Null);
    }
    if let (BqValue::Bytes(sb), BqValue::Bytes(fb), BqValue::Bytes(tb)) = (&s, &from, &to) {
        for (i, &b) in fb.iter().enumerate() {
            if fb.get(i + 1..).is_some_and(|rest| rest.contains(&b)) {
                return Err(BqError::semantic(
                    "TRANSLATE source_bytes cannot contain duplicate bytes",
                ));
            }
        }
        let mut out = Vec::with_capacity(sb.len());
        for &b in sb {
            if let Some(pos) = fb.iter().position(|&x| x == b) {
                if let Some(&rep) = tb.get(pos) {
                    out.push(rep);
                }
            } else {
                out.push(b);
            }
        }
        return Ok(BqValue::Bytes(out));
    }
    let (BqValue::String(s), BqValue::String(f), BqValue::String(t)) = (s, from, to) else {
        return Err(BqError::query(
            "TRANSLATE expects STRING or BYTES arguments",
        ));
    };
    let from_chars: Vec<char> = f.chars().collect();
    for (i, &ch) in from_chars.iter().enumerate() {
        if from_chars
            .get(i + 1..)
            .is_some_and(|rest| rest.contains(&ch))
        {
            return Err(BqError::semantic(
                "TRANSLATE source_characters cannot contain duplicate characters",
            ));
        }
    }
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
