use std::cmp::Ordering;
use std::net::IpAddr;

use crate::error::{BqError, BqResult};
use crate::executor::evaluator::compare_values;
use crate::types::{BqRange, BqType, BqValue, RangeElementType};

pub(super) const RANGE_NET_FUNCTIONS: &[&str] = &[
    "NET.HOST",
    "NET.IP_FROM_STRING",
    "NET.IP_TO_STRING",
    "NET.PUBLIC_SUFFIX",
    "NET.REG_DOMAIN",
    "NET.SAFE_IP_FROM_STRING",
    "RANGE",
    "RANGE_CONTAINS",
    "RANGE_END",
    "RANGE_INTERSECT",
    "RANGE_IS_END_UNBOUNDED",
    "RANGE_IS_START_UNBOUNDED",
    "RANGE_OVERLAPS",
    "RANGE_START",
];

pub(super) fn eval_range_net_fn(name: &str, args: Vec<BqValue>) -> BqResult<Option<BqValue>> {
    match name {
        "RANGE" => eval_range_ctor(args).map(Some),
        "RANGE_START" => eval_range_bound(args, true).map(Some),
        "RANGE_END" => eval_range_bound(args, false).map(Some),
        "RANGE_IS_START_UNBOUNDED" => eval_range_is_unbounded(args, true).map(Some),
        "RANGE_IS_END_UNBOUNDED" => eval_range_is_unbounded(args, false).map(Some),
        "RANGE_CONTAINS" => eval_range_contains(args).map(Some),
        "RANGE_OVERLAPS" => eval_range_overlaps(args).map(Some),
        "RANGE_INTERSECT" => eval_range_intersect(args).map(Some),
        "NET.IP_FROM_STRING" => eval_ip_from_str(args, false).map(Some),
        "NET.SAFE_IP_FROM_STRING" => eval_ip_from_str(args, true).map(Some),
        "NET.IP_TO_STRING" => eval_ip_to_str(args).map(Some),
        "NET.HOST" => eval_net_host(args).map(Some),
        "NET.PUBLIC_SUFFIX" => eval_net_suffix(args).map(Some),
        "NET.REG_DOMAIN" => eval_net_reg_domain(args).map(Some),
        _ => Ok(None),
    }
}

fn eval_range_ctor(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [start, end]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("RANGE expects 2 arguments"))?;
    let elem_ty = match (start.is_null(), end.is_null()) {
        (true, true) => {
            return Err(BqError::query(
                "RANGE(NULL, NULL) requires explicit RANGE<T> literal",
            ))
        }
        (false, _) => start.infer_type()?,
        (true, false) => end.infer_type()?,
    };
    let rt = match elem_ty {
        BqType::Date => RangeElementType::Date,
        BqType::Datetime => RangeElementType::Datetime,
        BqType::Timestamp => RangeElementType::Timestamp,
        BqType::UntypedNull
        | BqType::Bool
        | BqType::Int64
        | BqType::Float64
        | BqType::Numeric
        | BqType::BigNumeric
        | BqType::String
        | BqType::Bytes
        | BqType::Time
        | BqType::Interval
        | BqType::Json
        | BqType::Geography
        | BqType::Range(_)
        | BqType::Array(_)
        | BqType::Struct(_) => {
            return Err(BqError::query(format!(
                "RANGE does not support element type {elem_ty}"
            )))
        }
    };
    let s_opt = if start.is_null() { None } else { Some(start) };
    let e_opt = if end.is_null() { None } else { Some(end) };
    if let (Some(sv), Some(ev)) = (&s_opt, &e_opt) {
        if matches!(
            compare_values(sv, ev)?,
            Some(Ordering::Greater | Ordering::Equal)
        ) {
            return Err(BqError::semantic(
                "RANGE start must be strictly less than end",
            ));
        }
    }
    Ok(BqValue::Range(Box::new(BqRange::new(rt, s_opt, e_opt)?)))
}

fn eval_range_is_unbounded(args: Vec<BqValue>, is_start: bool) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("RANGE_IS_*_UNBOUNDED expects 1 argument"))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Range(r) = arg else {
        return Err(BqError::query("RANGE_IS_*_UNBOUNDED expects RANGE"));
    };
    let is_unbounded = if is_start {
        r.start.is_none()
    } else {
        r.end.is_none()
    };
    Ok(BqValue::Bool(is_unbounded))
}

fn eval_range_bound(args: Vec<BqValue>, is_start: bool) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("RANGE_START/END expects 1 argument"))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Range(r) = arg else {
        return Err(BqError::query("RANGE_START/END expects RANGE"));
    };
    let opt = if is_start { r.start } else { r.end };
    Ok(opt.unwrap_or(BqValue::Null))
}

fn eval_range_contains(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [r_val, target]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("RANGE_CONTAINS expects 2 arguments"))?;
    if r_val.is_null() || target.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Range(r) = r_val else {
        return Err(BqError::query(
            "RANGE_CONTAINS expects RANGE first argument",
        ));
    };
    if let BqValue::Range(inner) = target {
        let ge_s = match (&r.start, &inner.start) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(a), Some(b)) => !matches!(compare_values(a, b)?, Some(Ordering::Greater)),
        };
        let le_e = match (&r.end, &inner.end) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(a), Some(b)) => !matches!(compare_values(b, a)?, Some(Ordering::Greater)),
        };
        return Ok(BqValue::Bool(ge_s && le_e));
    }
    let ge_s = match &r.start {
        Some(s) => !matches!(compare_values(&target, s)?, Some(Ordering::Less)),
        None => true,
    };
    let lt_e = match &r.end {
        Some(e) => matches!(compare_values(&target, e)?, Some(Ordering::Less)),
        None => true,
    };
    Ok(BqValue::Bool(ge_s && lt_e))
}

fn eval_range_overlaps(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [a_val, b_val]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("RANGE_OVERLAPS expects 2 arguments"))?;
    if a_val.is_null() || b_val.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::Range(a), BqValue::Range(b)) = (a_val, b_val) else {
        return Err(BqError::query("RANGE_OVERLAPS expects (RANGE, RANGE)"));
    };
    Ok(BqValue::Bool(ranges_overlap(&a, &b)?))
}

fn eval_range_intersect(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [a_val, b_val]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("RANGE_INTERSECT expects 2 arguments"))?;
    if a_val.is_null() || b_val.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::Range(a), BqValue::Range(b)) = (a_val, b_val) else {
        return Err(BqError::query("RANGE_INTERSECT expects (RANGE, RANGE)"));
    };
    if !ranges_overlap(&a, &b)? {
        return Err(BqError::semantic("RANGE_INTERSECT ranges do not overlap"));
    }
    let start = match (a.start, b.start) {
        (None, x) | (x, None) => x,
        (Some(sa), Some(sb)) => Some(
            if matches!(compare_values(&sa, &sb)?, Some(Ordering::Greater)) {
                sa
            } else {
                sb
            },
        ),
    };
    let end = match (a.end, b.end) {
        (None, x) | (x, None) => x,
        (Some(ea), Some(eb)) => Some(
            if matches!(compare_values(&ea, &eb)?, Some(Ordering::Less)) {
                ea
            } else {
                eb
            },
        ),
    };
    Ok(BqValue::Range(Box::new(BqRange::new(
        a.elem_type,
        start,
        end,
    )?)))
}

fn ranges_overlap(a: &BqRange, b: &BqRange) -> BqResult<bool> {
    let a_lt_b = match (&a.start, &b.end) {
        (Some(sa), Some(eb)) => matches!(compare_values(sa, eb)?, Some(Ordering::Less)),
        _ => true,
    };
    let b_lt_a = match (&b.start, &a.end) {
        (Some(sb), Some(ea)) => matches!(compare_values(sb, ea)?, Some(Ordering::Less)),
        _ => true,
    };
    Ok(a_lt_b && b_lt_a)
}

fn eval_ip_from_str(args: Vec<BqValue>, safe: bool) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("NET.IP_FROM_STRING expects 1 argument"))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::String(s) = arg else {
        return Err(BqError::query("NET.IP_FROM_STRING expects STRING"));
    };
    match s.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => Ok(BqValue::Bytes(v4.octets().to_vec())),
        Ok(IpAddr::V6(v6)) => Ok(BqValue::Bytes(v6.octets().to_vec())),
        Err(_) if safe => Ok(BqValue::Null),
        Err(_) => Err(BqError::semantic(format!("Invalid IP address: '{s}'"))),
    }
}

fn eval_ip_to_str(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("NET.IP_TO_STRING expects 1 argument"))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Bytes(b) = arg else {
        return Err(BqError::query("NET.IP_TO_STRING expects BYTES"));
    };
    if let Ok(arr) = <[u8; 4]>::try_from(b.as_slice()) {
        return Ok(BqValue::String(std::net::Ipv4Addr::from(arr).to_string()));
    }
    if let Ok(arr) = <[u8; 16]>::try_from(b.as_slice()) {
        return Ok(BqValue::String(std::net::Ipv6Addr::from(arr).to_string()));
    }
    Err(BqError::semantic(
        "Invalid IP byte length (expected 4 or 16)",
    ))
}

fn extract_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, after)| after);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let no_user = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    no_user
        .split_once(':')
        .map_or(no_user, |(h, _)| h)
        .to_owned()
}

fn eval_net_host(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("NET.HOST expects 1 argument"))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::String(url) = arg else {
        return Err(BqError::query("NET.HOST expects STRING"));
    };
    let h = extract_host(&url);
    Ok(if h.is_empty() {
        BqValue::Null
    } else {
        BqValue::String(h)
    })
}

fn eval_net_suffix(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("NET.PUBLIC_SUFFIX expects 1 argument"))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::String(url) = arg else {
        return Err(BqError::query("NET.PUBLIC_SUFFIX expects STRING"));
    };
    let host = extract_host(&url);
    Ok(host
        .rsplit_once('.')
        .map_or(BqValue::Null, |(_, sfx)| BqValue::String(sfx.to_owned())))
}

fn eval_net_reg_domain(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("NET.REG_DOMAIN expects 1 argument"))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::String(url) = arg else {
        return Err(BqError::query("NET.REG_DOMAIN expects STRING"));
    };
    let host = extract_host(&url);
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() < 2 {
        return Ok(BqValue::Null);
    }
    let tail = parts.get(parts.len() - 2..).unwrap_or(&[]);
    Ok(BqValue::String(tail.join(".")))
}
