mod array_extra;
mod array_util;
mod bytes_fn;
mod datetime;
mod datetime_calc;
mod datetime_extra;
mod datetime_fmt;
mod farmhash;
mod geo_search;
mod interval;
mod json;
mod json_fast;
mod json_path;
mod math_hash;
mod range_geo_net;
mod regex_fn;
mod string;
mod string_fmt;
mod string_instr;

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::error::BqResult;
use crate::executor::context::ExecContext;
use crate::types::BqValue;

pub(in crate::executor) use crate::types::{parse_compound_interval, single_unit_interval};
pub(in crate::executor) use datetime_extra::timestamp_to_tz_naive;
pub(in crate::executor) use interval::{
    avg_intervals, eval_temporal_interval_arith, extract_interval_part, interval_div_f64,
    interval_div_i64, interval_neg, sum_intervals,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScalarGroup {
    String,
    BytesExtra,
    Regex,
    Datetime,
    DatetimeExtra,
    Interval,
    MathHash,
    ArrayUtil,
    ArrayExtra,
    Json,
    RangeNet,
    GeoSearch,
}

static SCALAR_DISPATCH: OnceLock<HashMap<&'static str, ScalarGroup>> = OnceLock::new();

fn scalar_dispatch() -> &'static HashMap<&'static str, ScalarGroup> {
    SCALAR_DISPATCH.get_or_init(|| {
        let groups: [(&[&'static str], ScalarGroup); 12] = [
            (string::STRING_FUNCTIONS, ScalarGroup::String),
            (bytes_fn::BYTES_EXTRA_FUNCTIONS, ScalarGroup::BytesExtra),
            (regex_fn::REGEX_FUNCTIONS, ScalarGroup::Regex),
            (datetime::DATETIME_FUNCTIONS, ScalarGroup::Datetime),
            (
                datetime_extra::DATETIME_EXTRA_FUNCTIONS,
                ScalarGroup::DatetimeExtra,
            ),
            (interval::INTERVAL_FUNCTIONS, ScalarGroup::Interval),
            (math_hash::MATH_HASH_FUNCTIONS, ScalarGroup::MathHash),
            (array_util::ARRAY_UTIL_FUNCTIONS, ScalarGroup::ArrayUtil),
            (array_extra::ARRAY_EXTRA_FUNCTIONS, ScalarGroup::ArrayExtra),
            (json::JSON_FUNCTIONS, ScalarGroup::Json),
            (range_geo_net::RANGE_NET_FUNCTIONS, ScalarGroup::RangeNet),
            (geo_search::GEO_SEARCH_FUNCTIONS, ScalarGroup::GeoSearch),
        ];
        let total: usize = groups.iter().map(|(names, _)| names.len()).sum();
        let mut map = HashMap::with_capacity(total);
        for (names, group) in groups {
            for &fn_name in names {
                map.entry(fn_name).or_insert(group);
            }
        }
        map
    })
}

fn lookup_scalar(name: &str) -> Option<(&'static str, ScalarGroup)> {
    let map = scalar_dispatch();
    if name.is_ascii() && name.len() <= 64 {
        let mut buf = [0_u8; 64];
        for (dst, src) in buf.iter_mut().zip(name.bytes()) {
            *dst = src.to_ascii_uppercase();
        }
        if let Some(upper) = buf
            .get(..name.len())
            .and_then(|b| std::str::from_utf8(b).ok())
        {
            return map.get_key_value(upper).map(|(&k, &g)| (k, g));
        }
    }
    let upper = name.to_ascii_uppercase();
    map.get_key_value(upper.as_str()).map(|(&k, &g)| (k, g))
}

/// Returns all registered scalar built-in function names (sorted/canonical uppercase).
#[must_use]
pub fn registered_scalar_functions() -> Vec<&'static str> {
    let mut all: Vec<&'static str> = scalar_dispatch().keys().copied().collect();
    all.sort_unstable();
    all
}

#[must_use]
pub fn is_scalar_registered(name: &str) -> bool {
    lookup_scalar(name).is_some()
}

pub(super) fn eval_scalar_builtin(
    name: &str,
    args: Vec<BqValue>,
    ctx: &ExecContext<'_>,
) -> BqResult<Option<BqValue>> {
    let Some((canon, group)) = lookup_scalar(name) else {
        return Ok(None);
    };
    match group {
        ScalarGroup::String => string::eval_string_fn(canon, args),
        ScalarGroup::BytesExtra => bytes_fn::eval_bytes_extra_fn(canon, &args),
        ScalarGroup::Regex => regex_fn::eval_regex_fn(canon, args),
        ScalarGroup::Datetime => datetime::eval_datetime_fn(canon, args, ctx),
        ScalarGroup::DatetimeExtra => datetime_extra::eval_datetime_extra_fn(canon, &args),
        ScalarGroup::Interval => interval::eval_interval_fn(canon, &args),
        ScalarGroup::MathHash => math_hash::eval_math_hash_fn(canon, args),
        ScalarGroup::ArrayUtil => array_util::eval_array_util_fn(canon, args),
        ScalarGroup::ArrayExtra => array_extra::eval_array_extra_fn(canon, args),
        ScalarGroup::Json => json::eval_json_fn(canon, args),
        ScalarGroup::RangeNet => range_geo_net::eval_range_net_fn(canon, args),
        ScalarGroup::GeoSearch => geo_search::eval_geo_search_fn(canon, args, ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_dispatch_table_has_no_duplicate_registrations() {
        let lists: [&[&str]; 12] = [
            string::STRING_FUNCTIONS,
            bytes_fn::BYTES_EXTRA_FUNCTIONS,
            regex_fn::REGEX_FUNCTIONS,
            datetime::DATETIME_FUNCTIONS,
            datetime_extra::DATETIME_EXTRA_FUNCTIONS,
            interval::INTERVAL_FUNCTIONS,
            math_hash::MATH_HASH_FUNCTIONS,
            array_util::ARRAY_UTIL_FUNCTIONS,
            array_extra::ARRAY_EXTRA_FUNCTIONS,
            json::JSON_FUNCTIONS,
            range_geo_net::RANGE_NET_FUNCTIONS,
            geo_search::GEO_SEARCH_FUNCTIONS,
        ];
        let total: usize = lists.iter().map(|l| l.len()).sum();
        assert_eq!(scalar_dispatch().len(), total);
        assert!(is_scalar_registered("json_value"));
        assert!(is_scalar_registered("NET.HOST"));
        assert!(!is_scalar_registered("NOT_A_REAL_SCALAR_FN"));
    }
}
