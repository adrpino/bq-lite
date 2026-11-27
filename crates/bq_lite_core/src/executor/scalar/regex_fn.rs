use regex::bytes::Regex as BytesRegex;
use regex::Regex;

use crate::error::{BqError, BqResult};
use crate::types::BqValue;

pub(super) const REGEX_FUNCTIONS: &[&str] = &[
    "REGEXP_CONTAINS",
    "REGEXP_EXTRACT",
    "REGEXP_EXTRACT_ALL",
    "REGEXP_INSTR",
    "REGEXP_MATCH",
    "REGEXP_REPLACE",
    "REGEXP_SUBSTR",
];

pub(super) fn eval_regex_fn(name: &str, args: Vec<BqValue>) -> BqResult<Option<BqValue>> {
    match name {
        "REGEXP_CONTAINS" => eval_regexp_contains(args, false).map(Some),
        "REGEXP_MATCH" => eval_regexp_contains(args, true).map(Some),
        "REGEXP_EXTRACT" | "REGEXP_SUBSTR" => eval_regexp_extract(args).map(Some),
        "REGEXP_EXTRACT_ALL" => eval_regexp_extract_all(args).map(Some),
        "REGEXP_INSTR" => eval_regexp_instr(args).map(Some),
        "REGEXP_REPLACE" => eval_regexp_replace(args).map(Some),
        _ => Ok(None),
    }
}

fn compile_regex(pat: &str) -> BqResult<Regex> {
    Regex::new(pat).map_err(|e| BqError::semantic(format!("Invalid regular expression: {e}")))
}

fn compile_bytes_regex(pat: &[u8]) -> BqResult<BytesRegex> {
    let pat_s = std::str::from_utf8(pat)
        .map_err(|_| BqError::semantic("Invalid UTF-8 in BYTES regular expression"))?;
    BytesRegex::new(pat_s)
        .map_err(|e| BqError::semantic(format!("Invalid regular expression: {e}")))
}

fn eval_regexp_contains(args: Vec<BqValue>, full_match: bool) -> BqResult<BqValue> {
    let [val, pat]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("REGEXP_CONTAINS expects 2 arguments"))?;
    if val.is_null() || pat.is_null() {
        return Ok(BqValue::Null);
    }
    match (val, pat) {
        (BqValue::String(s), BqValue::String(p)) => {
            let re = if full_match {
                compile_regex(&format!("^(?:{p})$"))?
            } else {
                compile_regex(&p)?
            };
            Ok(BqValue::Bool(re.is_match(&s)))
        }
        (BqValue::Bytes(b), BqValue::Bytes(p)) => {
            let re = if full_match {
                let mut wrapped = b"^(?:".to_vec();
                wrapped.extend_from_slice(&p);
                wrapped.extend_from_slice(b")$");
                compile_bytes_regex(&wrapped)?
            } else {
                compile_bytes_regex(&p)?
            };
            Ok(BqValue::Bool(re.is_match(&b)))
        }
        _ => Err(BqError::query(
            "REGEXP_CONTAINS expects STRING or BYTES arguments",
        )),
    }
}

fn eval_regexp_extract(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 4 {
        return Err(BqError::query("REGEXP_EXTRACT expects 2 to 4 arguments"));
    }
    if args.iter().any(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    let mut it = args.into_iter();
    let val = it.next().unwrap_or(BqValue::Null);
    let pat = it.next().unwrap_or(BqValue::Null);
    let pos = parse_opt_pos(it.next().as_ref())?;
    let occ = parse_opt_occ(it.next().as_ref())?;
    match (val, pat) {
        (BqValue::String(s), BqValue::String(p)) => {
            let re = compile_regex(&p)?;
            if re.captures_len() > 2 {
                return Err(BqError::semantic(
                    "REGEXP_EXTRACT supports at most 1 capturing group",
                ));
            }
            let start_byte = char_pos_to_byte_offset(&s, pos);
            let Some(slice) = s.get(start_byte..) else {
                return Ok(BqValue::Null);
            };
            let group_idx = usize::from(re.captures_len() > 1);
            let found = re.captures_iter(slice).nth(occ - 1);
            Ok(found
                .and_then(|c| c.get(group_idx))
                .map_or(BqValue::Null, |m| BqValue::String(m.as_str().to_owned())))
        }
        (BqValue::Bytes(b), BqValue::Bytes(p)) => {
            let re = compile_bytes_regex(&p)?;
            if re.captures_len() > 2 {
                return Err(BqError::semantic(
                    "REGEXP_EXTRACT supports at most 1 capturing group",
                ));
            }
            let Some(slice) = b.get(pos - 1..) else {
                return Ok(BqValue::Null);
            };
            let group_idx = usize::from(re.captures_len() > 1);
            let found = re.captures_iter(slice).nth(occ - 1);
            Ok(found
                .and_then(|c| c.get(group_idx))
                .map_or(BqValue::Null, |m| BqValue::Bytes(m.as_bytes().to_vec())))
        }
        _ => Err(BqError::query(
            "REGEXP_EXTRACT expects STRING or BYTES arguments",
        )),
    }
}

fn eval_regexp_extract_all(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [val, pat]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("REGEXP_EXTRACT_ALL expects 2 arguments"))?;
    if val.is_null() || pat.is_null() {
        return Ok(BqValue::Null);
    }
    match (val, pat) {
        (BqValue::String(s), BqValue::String(p)) => {
            let re = compile_regex(&p)?;
            let group_idx = usize::from(re.captures_len() > 1);
            let items: Vec<BqValue> = re
                .captures_iter(&s)
                .filter_map(|c| {
                    c.get(group_idx)
                        .map(|m| BqValue::String(m.as_str().to_owned()))
                })
                .collect();
            Ok(BqValue::Array(items))
        }
        (BqValue::Bytes(b), BqValue::Bytes(p)) => {
            let re = compile_bytes_regex(&p)?;
            let group_idx = usize::from(re.captures_len() > 1);
            let items: Vec<BqValue> = re
                .captures_iter(&b)
                .filter_map(|c| {
                    c.get(group_idx)
                        .map(|m| BqValue::Bytes(m.as_bytes().to_vec()))
                })
                .collect();
            Ok(BqValue::Array(items))
        }
        _ => Err(BqError::query(
            "REGEXP_EXTRACT_ALL expects STRING or BYTES arguments",
        )),
    }
}

fn eval_regexp_instr(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 5 {
        return Err(BqError::query("REGEXP_INSTR expects 2 to 5 arguments"));
    }
    if args.iter().any(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    let mut it = args.into_iter();
    let val = it.next().unwrap_or(BqValue::Null);
    let pat = it.next().unwrap_or(BqValue::Null);
    let pos = parse_opt_pos(it.next().as_ref())?;
    let occ = parse_opt_occ(it.next().as_ref())?;
    let occ_pos = match it.next() {
        None => 0_i64,
        Some(BqValue::Int64(v)) if v == 0 || v == 1 => v,
        Some(BqValue::Int64(_)) => {
            return Err(BqError::semantic(
                "REGEXP_INSTR occurrence_position must be 0 or 1",
            ))
        }
        Some(_) => {
            return Err(BqError::query(
                "REGEXP_INSTR occurrence_position must be INT64",
            ))
        }
    };
    match (val, pat) {
        (BqValue::String(s), BqValue::String(p)) => {
            let re = compile_regex(&p)?;
            if re.captures_len() > 2 {
                return Err(BqError::semantic(
                    "REGEXP_INSTR supports at most 1 capturing group",
                ));
            }
            let start_byte = char_pos_to_byte_offset(&s, pos);
            let Some(slice) = s.get(start_byte..) else {
                return Ok(BqValue::Int64(0));
            };
            let group_idx = usize::from(re.captures_len() > 1);
            let found = re
                .captures_iter(slice)
                .nth(occ - 1)
                .and_then(|c| c.get(group_idx));
            let res = found.map_or(0, |m| {
                let target_byte = start_byte + if occ_pos == 1 { m.end() } else { m.start() };
                i64::try_from(s.get(..target_byte).unwrap_or("").chars().count() + 1)
                    .unwrap_or(i64::MAX)
            });
            Ok(BqValue::Int64(res))
        }
        (BqValue::Bytes(b), BqValue::Bytes(p)) => {
            let re = compile_bytes_regex(&p)?;
            if re.captures_len() > 2 {
                return Err(BqError::semantic(
                    "REGEXP_INSTR supports at most 1 capturing group",
                ));
            }
            let Some(slice) = b.get(pos - 1..) else {
                return Ok(BqValue::Int64(0));
            };
            let group_idx = usize::from(re.captures_len() > 1);
            let found = re
                .captures_iter(slice)
                .nth(occ - 1)
                .and_then(|c| c.get(group_idx));
            let res = found.map_or(0, |m| {
                let target_byte = (pos - 1) + if occ_pos == 1 { m.end() } else { m.start() };
                i64::try_from(target_byte + 1).unwrap_or(i64::MAX)
            });
            Ok(BqValue::Int64(res))
        }
        _ => Err(BqError::query(
            "REGEXP_INSTR expects STRING or BYTES arguments",
        )),
    }
}

fn eval_regexp_replace(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [val, pat, rep]: [BqValue; 3] = args
        .try_into()
        .map_err(|_| BqError::query("REGEXP_REPLACE expects 3 arguments"))?;
    if val.is_null() || pat.is_null() || rep.is_null() {
        return Ok(BqValue::Null);
    }
    match (val, pat, rep) {
        (BqValue::String(s), BqValue::String(p), BqValue::String(r)) => {
            let re = compile_regex(&p)?;
            Ok(BqValue::String(re.replace_all(&s, r.as_str()).into_owned()))
        }
        (BqValue::Bytes(b), BqValue::Bytes(p), BqValue::Bytes(r)) => {
            let re = compile_bytes_regex(&p)?;
            Ok(BqValue::Bytes(
                re.replace_all(&b, r.as_slice()).into_owned(),
            ))
        }
        _ => Err(BqError::query(
            "REGEXP_REPLACE expects STRING or BYTES arguments",
        )),
    }
}

fn parse_opt_pos(v: Option<&BqValue>) -> BqResult<usize> {
    match v {
        None => Ok(1),
        Some(BqValue::Int64(p)) if *p >= 1 => Ok(usize::try_from(*p).unwrap_or(usize::MAX)),
        Some(BqValue::Int64(_)) => Err(BqError::semantic("REGEXP position must be >= 1")),
        Some(_) => Err(BqError::query("REGEXP position must be INT64")),
    }
}

fn parse_opt_occ(v: Option<&BqValue>) -> BqResult<usize> {
    match v {
        None => Ok(1),
        Some(BqValue::Int64(o)) if *o >= 1 => Ok(usize::try_from(*o).unwrap_or(usize::MAX)),
        Some(BqValue::Int64(_)) => Err(BqError::semantic("REGEXP occurrence must be >= 1")),
        Some(_) => Err(BqError::query("REGEXP occurrence must be INT64")),
    }
}

fn char_pos_to_byte_offset(s: &str, pos_1based: usize) -> usize {
    if pos_1based <= 1 {
        return 0;
    }
    s.char_indices()
        .nth(pos_1based - 1)
        .map_or(s.len(), |(idx, _)| idx)
}
