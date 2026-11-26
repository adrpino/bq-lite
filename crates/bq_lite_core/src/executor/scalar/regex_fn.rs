use regex::Regex;

use crate::error::{BqError, BqResult};
use crate::types::BqValue;

pub(super) const REGEX_FUNCTIONS: &[&str] = &[
    "REGEXP_CONTAINS",
    "REGEXP_EXTRACT",
    "REGEXP_EXTRACT_ALL",
    "REGEXP_INSTR",
    "REGEXP_REPLACE",
    "REGEXP_SUBSTR",
];

pub(super) fn eval_regex_fn(name: &str, args: Vec<BqValue>) -> BqResult<Option<BqValue>> {
    match name {
        "REGEXP_CONTAINS" => eval_regexp_contains(args).map(Some),
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

fn eval_regexp_contains(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [val, pat]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("REGEXP_CONTAINS expects 2 arguments"))?;
    if val.is_null() || pat.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::String(s), BqValue::String(p)) = (val, pat) else {
        return Err(BqError::query("REGEXP_CONTAINS expects STRING arguments"));
    };
    let re = compile_regex(&p)?;
    Ok(BqValue::Bool(re.is_match(&s)))
}

fn eval_regexp_extract(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 4 {
        return Err(BqError::query("REGEXP_EXTRACT expects 2 to 4 arguments"));
    }
    let mut it = args.into_iter();
    let val = it.next().unwrap_or(BqValue::Null);
    let pat = it.next().unwrap_or(BqValue::Null);
    if val.is_null() || pat.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::String(s), BqValue::String(p)) = (val, pat) else {
        return Err(BqError::query("REGEXP_EXTRACT expects STRING arguments"));
    };
    let re = compile_regex(&p)?;
    let Some(caps) = re.captures(&s) else {
        return Ok(BqValue::Null);
    };
    let group_idx = usize::from(re.captures_len() > 1);
    Ok(caps
        .get(group_idx)
        .map_or(BqValue::Null, |m| BqValue::String(m.as_str().to_owned())))
}

fn eval_regexp_extract_all(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [val, pat]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("REGEXP_EXTRACT_ALL expects 2 arguments"))?;
    if val.is_null() || pat.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::String(s), BqValue::String(p)) = (val, pat) else {
        return Err(BqError::query(
            "REGEXP_EXTRACT_ALL expects STRING arguments",
        ));
    };
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

fn eval_regexp_instr(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 4 {
        return Err(BqError::query("REGEXP_INSTR expects 2 to 4 arguments"));
    }
    let mut it = args.into_iter();
    let val = it.next().unwrap_or(BqValue::Null);
    let pat = it.next().unwrap_or(BqValue::Null);
    if val.is_null() || pat.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::String(s), BqValue::String(p)) = (val, pat) else {
        return Err(BqError::query("REGEXP_INSTR expects STRING arguments"));
    };
    let re = compile_regex(&p)?;
    let pos = re.find(&s).map_or(0, |m| {
        i64::try_from(s.get(..m.start()).unwrap_or("").chars().count() + 1).unwrap_or(i64::MAX)
    });
    Ok(BqValue::Int64(pos))
}

fn eval_regexp_replace(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [val, pat, rep]: [BqValue; 3] = args
        .try_into()
        .map_err(|_| BqError::query("REGEXP_REPLACE expects 3 arguments"))?;
    if val.is_null() || pat.is_null() || rep.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::String(s), BqValue::String(p), BqValue::String(r)) = (val, pat, rep) else {
        return Err(BqError::query("REGEXP_REPLACE expects STRING arguments"));
    };
    let re = compile_regex(&p)?;
    Ok(BqValue::String(re.replace_all(&s, r.as_str()).into_owned()))
}
