mod array_util;
mod bytes_fn;
mod datetime;
mod datetime_calc;
mod datetime_extra;
mod datetime_fmt;
mod farmhash;
mod geo_search;
mod json;
mod json_path;
mod math_hash;
mod range_geo_net;
mod regex_fn;
mod string;
mod string_fmt;
mod string_instr;

use crate::error::BqResult;
use crate::executor::context::ExecContext;
use crate::types::BqValue;

pub(in crate::executor) use datetime_calc::add_interval_to_date;
pub(in crate::executor) use datetime_extra::{
    parse_compound_interval, single_unit_interval, timestamp_to_tz_naive,
};

/// Returns all registered scalar built-in function names (sorted/canonical uppercase).
#[must_use]
pub fn registered_scalar_functions() -> Vec<&'static str> {
    let mut all = Vec::new();
    all.extend_from_slice(string::STRING_FUNCTIONS);
    all.extend_from_slice(bytes_fn::BYTES_EXTRA_FUNCTIONS);
    all.extend_from_slice(regex_fn::REGEX_FUNCTIONS);
    all.extend_from_slice(datetime::DATETIME_FUNCTIONS);
    all.extend_from_slice(datetime_extra::DATETIME_EXTRA_FUNCTIONS);
    all.extend_from_slice(math_hash::MATH_HASH_FUNCTIONS);
    all.extend_from_slice(array_util::ARRAY_UTIL_FUNCTIONS);
    all.extend_from_slice(json::JSON_FUNCTIONS);
    all.extend_from_slice(range_geo_net::RANGE_NET_FUNCTIONS);
    all.extend_from_slice(geo_search::GEO_SEARCH_FUNCTIONS);
    all.sort_unstable();
    all.dedup();
    all
}

#[must_use]
pub fn is_scalar_registered(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    registered_scalar_functions()
        .binary_search(&upper.as_str())
        .is_ok()
}

pub(super) fn eval_scalar_builtin(
    name: &str,
    args: Vec<BqValue>,
    ctx: &ExecContext<'_>,
) -> BqResult<Option<BqValue>> {
    let upper = name.to_ascii_uppercase();
    if let Some(v) = string::eval_string_fn(&upper, args.clone())? {
        return Ok(Some(v));
    }
    if let Some(v) = bytes_fn::eval_bytes_extra_fn(&upper, &args)? {
        return Ok(Some(v));
    }
    if let Some(v) = regex_fn::eval_regex_fn(&upper, args.clone())? {
        return Ok(Some(v));
    }
    if let Some(v) = datetime::eval_datetime_fn(&upper, args.clone(), ctx)? {
        return Ok(Some(v));
    }
    if let Some(v) = datetime_extra::eval_datetime_extra_fn(&upper, &args)? {
        return Ok(Some(v));
    }
    if let Some(v) = math_hash::eval_math_hash_fn(&upper, args.clone())? {
        return Ok(Some(v));
    }
    if let Some(v) = array_util::eval_array_util_fn(&upper, args.clone())? {
        return Ok(Some(v));
    }
    if let Some(v) = json::eval_json_fn(&upper, args.clone())? {
        return Ok(Some(v));
    }
    if let Some(v) = range_geo_net::eval_range_net_fn(&upper, args.clone())? {
        return Ok(Some(v));
    }
    if let Some(v) = geo_search::eval_geo_search_fn(&upper, args, ctx)? {
        return Ok(Some(v));
    }
    Ok(None)
}
