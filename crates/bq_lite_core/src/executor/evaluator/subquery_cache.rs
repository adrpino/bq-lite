use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::mem::discriminant;
use std::sync::Arc;

use chrono::{DateTime, NaiveDateTime, NaiveTime, Utc};

use crate::ast::Query;
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext, RowSchema};
use crate::executor::query::execute_query;
use crate::executor::result::QueryResult;
use crate::types::{BqStructFieldValue, BqValue};

use super::ops;
use super::subquery::eval_in_values;

#[derive(Debug, Default)]
pub(in crate::executor) struct SubqueryCache {
    entries: HashMap<usize, CachedSubquery>,
}

#[derive(Debug)]
enum CachedSubquery {
    Uncacheable,
    In {
        outer_schema: Arc<RowSchema>,
        col_count: usize,
        index: InLookupIndex,
    },
    Exists {
        outer_schema: Arc<RowSchema>,
        value: bool,
    },
    Scalar {
        outer_schema: Arc<RowSchema>,
        value: BqValue,
    },
    Array {
        outer_schema: Arc<RowSchema>,
        value: BqValue,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum InHashKey {
    Bool(bool),
    Num(u64),
    String(String),
    Bytes(Vec<u8>),
    Datetime(NaiveDateTime),
    Time(NaiveTime),
    Timestamp(DateTime<Utc>),
}

#[derive(Debug)]
enum InLookupIndex {
    Empty,
    Int64 {
        set: HashSet<i64>,
        has_null: bool,
    },
    String {
        set: HashSet<String>,
        has_null: bool,
    },
    Hashed {
        buckets: HashMap<InHashKey, Vec<BqValue>>,
        has_null: bool,
        sample: BqValue,
    },
    Linear(Vec<BqValue>),
}

#[inline]
fn query_key(q: &Query) -> usize {
    std::ptr::from_ref(q) as usize
}

fn execute_tracked(
    q: &Query,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<(QueryResult, bool)> {
    let touched_outer = Cell::new(false);
    let local_impure = Cell::new(false);
    let sub_ctx = ctx.with_outer_tracked(row, &touched_outer, &local_impure);
    let res = execute_query(q, &sub_ctx)?;
    if local_impure.get() {
        ctx.mark_impure();
    }
    let cacheable = !touched_outer.get() && !local_impure.get();
    Ok((res, cacheable))
}

pub(super) fn eval_in_subquery(
    lhs: &BqValue,
    negated: bool,
    q: &Query,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let key = query_key(q);
    let mut is_uncacheable = false;
    if let Ok(cache) = ctx.subquery_cache.try_borrow() {
        match cache.entries.get(&key) {
            Some(CachedSubquery::In {
                outer_schema,
                col_count,
                index,
            }) if Arc::ptr_eq(outer_schema, &row.schema) => {
                validate_in_col_count(lhs, *col_count)?;
                return probe_in_index(lhs, negated, index);
            }
            Some(CachedSubquery::Uncacheable) => {
                is_uncacheable = true;
            }
            _ => {}
        }
    }

    if is_uncacheable {
        let sub_ctx = ctx.with_outer(row);
        let res = execute_query(q, &sub_ctx)?;
        let col_count = res.columns.len();
        validate_in_col_count(lhs, col_count)?;
        let candidates = extract_in_candidates(res);
        return eval_in_values(lhs, negated, &candidates);
    }

    let (res, cacheable) = execute_tracked(q, row, ctx)?;
    let col_count = res.columns.len();
    if cacheable {
        let index = build_in_index(res);
        let result = validate_in_col_count(lhs, col_count)
            .and_then(|()| probe_in_index(lhs, negated, &index));
        if let Ok(mut cache) = ctx.subquery_cache.try_borrow_mut() {
            cache.entries.insert(
                key,
                CachedSubquery::In {
                    outer_schema: Arc::clone(&row.schema),
                    col_count,
                    index,
                },
            );
        }
        return result;
    }

    if let Ok(mut cache) = ctx.subquery_cache.try_borrow_mut() {
        cache.entries.insert(key, CachedSubquery::Uncacheable);
    }
    validate_in_col_count(lhs, col_count)?;
    let candidates = extract_in_candidates(res);
    eval_in_values(lhs, negated, &candidates)
}

pub(super) fn eval_exists_subquery(
    q: &Query,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let key = query_key(q);
    let mut is_uncacheable = false;
    if let Ok(cache) = ctx.subquery_cache.try_borrow() {
        match cache.entries.get(&key) {
            Some(CachedSubquery::Exists {
                outer_schema,
                value,
            }) if Arc::ptr_eq(outer_schema, &row.schema) => {
                return Ok(BqValue::Bool(*value));
            }
            Some(CachedSubquery::Uncacheable) => is_uncacheable = true,
            _ => {}
        }
    }
    if is_uncacheable {
        let res = execute_query(q, &ctx.with_outer(row))?;
        return Ok(BqValue::Bool(!res.rows.is_empty()));
    }
    let (res, cacheable) = execute_tracked(q, row, ctx)?;
    let exists = !res.rows.is_empty();
    if let Ok(mut cache) = ctx.subquery_cache.try_borrow_mut() {
        cache.entries.insert(
            key,
            if cacheable {
                CachedSubquery::Exists {
                    outer_schema: Arc::clone(&row.schema),
                    value: exists,
                }
            } else {
                CachedSubquery::Uncacheable
            },
        );
    }
    Ok(BqValue::Bool(exists))
}

pub(super) fn eval_scalar_subquery_cached(
    q: &Query,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let key = query_key(q);
    let mut is_uncacheable = false;
    if let Ok(cache) = ctx.subquery_cache.try_borrow() {
        match cache.entries.get(&key) {
            Some(CachedSubquery::Scalar {
                outer_schema,
                value,
            }) if Arc::ptr_eq(outer_schema, &row.schema) => {
                return Ok(value.clone());
            }
            Some(CachedSubquery::Uncacheable) => is_uncacheable = true,
            _ => {}
        }
    }
    if is_uncacheable {
        let res = execute_query(q, &ctx.with_outer(row))?;
        return extract_scalar_result(res);
    }
    let (res, cacheable) = execute_tracked(q, row, ctx)?;
    let val = extract_scalar_result(res)?;
    if let Ok(mut cache) = ctx.subquery_cache.try_borrow_mut() {
        cache.entries.insert(
            key,
            if cacheable {
                CachedSubquery::Scalar {
                    outer_schema: Arc::clone(&row.schema),
                    value: val.clone(),
                }
            } else {
                CachedSubquery::Uncacheable
            },
        );
    }
    Ok(val)
}

pub(super) fn eval_array_subquery_cached(
    q: &Query,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let key = query_key(q);
    let mut is_uncacheable = false;
    if let Ok(cache) = ctx.subquery_cache.try_borrow() {
        match cache.entries.get(&key) {
            Some(CachedSubquery::Array {
                outer_schema,
                value,
            }) if Arc::ptr_eq(outer_schema, &row.schema) => {
                return Ok(value.clone());
            }
            Some(CachedSubquery::Uncacheable) => is_uncacheable = true,
            _ => {}
        }
    }
    if is_uncacheable {
        let res = execute_query(q, &ctx.with_outer(row))?;
        return extract_array_result(res);
    }
    let (res, cacheable) = execute_tracked(q, row, ctx)?;
    let val = extract_array_result(res)?;
    if let Ok(mut cache) = ctx.subquery_cache.try_borrow_mut() {
        cache.entries.insert(
            key,
            if cacheable {
                CachedSubquery::Array {
                    outer_schema: Arc::clone(&row.schema),
                    value: val.clone(),
                }
            } else {
                CachedSubquery::Uncacheable
            },
        );
    }
    Ok(val)
}

fn extract_scalar_result(res: QueryResult) -> BqResult<BqValue> {
    if res.columns.len() != 1 {
        return Err(BqError::query(
            "Scalar subquery must produce exactly 1 column",
        ));
    }
    if res.rows.len() > 1 {
        return Err(BqError::semantic(
            "Scalar subquery produced more than one element",
        ));
    }
    Ok(res
        .rows
        .into_iter()
        .next()
        .and_then(|r| r.into_iter().next())
        .unwrap_or(BqValue::Null))
}

fn extract_array_result(res: QueryResult) -> BqResult<BqValue> {
    if res.columns.len() != 1 {
        return Err(BqError::query(
            "ARRAY(SELECT ...) subquery must produce exactly 1 column",
        ));
    }
    let items = res
        .rows
        .into_iter()
        .map(|r| r.into_iter().next().unwrap_or(BqValue::Null))
        .collect();
    Ok(BqValue::Array(items))
}

fn validate_in_col_count(lhs: &BqValue, col_count: usize) -> BqResult<()> {
    if let BqValue::Struct(fields) = lhs {
        if (col_count == fields.len() && col_count > 1) || col_count == 1 {
            Ok(())
        } else {
            Err(BqError::query(format!(
                "IN subquery column count mismatch: expected {}, got {col_count}",
                fields.len(),
            )))
        }
    } else if col_count == 1 {
        Ok(())
    } else {
        Err(BqError::query("IN subquery must return exactly 1 column"))
    }
}

fn extract_in_candidates(res: QueryResult) -> Vec<BqValue> {
    if res.columns.len() > 1 {
        let cols = res.columns;
        res.rows
            .into_iter()
            .map(|r| {
                BqValue::Struct(
                    r.into_iter()
                        .zip(cols.iter())
                        .map(|(v, c)| BqStructFieldValue {
                            name: Some(c.name.clone()),
                            value: v,
                        })
                        .collect(),
                )
            })
            .collect()
    } else {
        res.rows
            .into_iter()
            .filter_map(|r| r.into_iter().next())
            .collect()
    }
}

fn build_in_index(res: QueryResult) -> InLookupIndex {
    if res.rows.is_empty() {
        return InLookupIndex::Empty;
    }
    let candidates = extract_in_candidates(res);
    if candidates.is_empty() {
        return InLookupIndex::Empty;
    }
    let mut has_null = false;
    let mut all_int64 = true;
    let mut all_string = true;
    let mut all_hashable = true;
    for v in &candidates {
        if v.is_null() {
            has_null = true;
            continue;
        }
        if !matches!(v, BqValue::Int64(_)) {
            all_int64 = false;
        }
        if !matches!(v, BqValue::String(_)) {
            all_string = false;
        }
        if value_to_in_key(v).is_none() {
            all_hashable = false;
        }
    }
    if all_int64 {
        let mut set = HashSet::with_capacity(candidates.len());
        for v in candidates {
            if let BqValue::Int64(n) = v {
                set.insert(n);
            }
        }
        return InLookupIndex::Int64 { set, has_null };
    }
    if all_string {
        let mut set = HashSet::with_capacity(candidates.len());
        for v in candidates {
            if let BqValue::String(s) = v {
                set.insert(s);
            }
        }
        return InLookupIndex::String { set, has_null };
    }
    if all_hashable {
        let mut buckets: HashMap<InHashKey, Vec<BqValue>> =
            HashMap::with_capacity(candidates.len());
        let mut sample: Option<BqValue> = None;
        for v in candidates {
            if v.is_null() {
                continue;
            }
            if sample.is_none() {
                sample = Some(v.clone());
            }
            if let Some(key) = value_to_in_key(&v) {
                buckets.entry(key).or_default().push(v);
            }
        }
        return sample.map_or_else(
            || InLookupIndex::Int64 {
                set: HashSet::new(),
                has_null,
            },
            |sample_val| InLookupIndex::Hashed {
                buckets,
                has_null,
                sample: sample_val,
            },
        );
    }
    InLookupIndex::Linear(candidates)
}

fn probe_in_index(lhs: &BqValue, negated: bool, index: &InLookupIndex) -> BqResult<BqValue> {
    match index {
        InLookupIndex::Empty => Ok(BqValue::Bool(negated)),
        InLookupIndex::Int64 { set, has_null } => probe_int64_index(lhs, negated, set, *has_null),
        InLookupIndex::String { set, has_null } => probe_string_index(lhs, negated, set, *has_null),
        InLookupIndex::Hashed {
            buckets,
            has_null,
            sample,
        } => probe_hashed_index(lhs, negated, buckets, *has_null, sample),
        InLookupIndex::Linear(candidates) => eval_in_values(lhs, negated, candidates),
    }
}

fn probe_int64_index(
    lhs: &BqValue,
    negated: bool,
    set: &HashSet<i64>,
    has_null: bool,
) -> BqResult<BqValue> {
    if lhs.is_null() {
        return Ok(if set.is_empty() && !has_null {
            BqValue::Bool(negated)
        } else {
            BqValue::Null
        });
    }
    if let BqValue::Int64(n) = lhs {
        if set.contains(n) {
            return Ok(BqValue::Bool(!negated));
        }
        return Ok(if has_null {
            BqValue::Null
        } else {
            BqValue::Bool(negated)
        });
    }
    if matches!(
        lhs,
        BqValue::Float64(_) | BqValue::Numeric(_) | BqValue::BigNumeric(_)
    ) {
        for &n in set {
            if matches!(ops::sql_values_equal(lhs, &BqValue::Int64(n))?, Some(true)) {
                return Ok(BqValue::Bool(!negated));
            }
        }
        return Ok(if has_null {
            BqValue::Null
        } else {
            BqValue::Bool(negated)
        });
    }
    if let Some(&first) = set.iter().next() {
        let _ = ops::sql_values_equal(lhs, &BqValue::Int64(first))?;
    }
    Ok(if has_null {
        BqValue::Null
    } else {
        BqValue::Bool(negated)
    })
}

fn probe_string_index(
    lhs: &BqValue,
    negated: bool,
    set: &HashSet<String>,
    has_null: bool,
) -> BqResult<BqValue> {
    if lhs.is_null() {
        return Ok(if set.is_empty() && !has_null {
            BqValue::Bool(negated)
        } else {
            BqValue::Null
        });
    }
    if let BqValue::String(s) = lhs {
        if set.contains(s.as_str()) {
            return Ok(BqValue::Bool(!negated));
        }
        return Ok(if has_null {
            BqValue::Null
        } else {
            BqValue::Bool(negated)
        });
    }
    if let Some(first) = set.iter().next() {
        let _ = ops::sql_values_equal(lhs, &BqValue::String(first.clone()))?;
    }
    Ok(if has_null {
        BqValue::Null
    } else {
        BqValue::Bool(negated)
    })
}

fn probe_hashed_index(
    lhs: &BqValue,
    negated: bool,
    buckets: &HashMap<InHashKey, Vec<BqValue>>,
    has_null: bool,
    sample: &BqValue,
) -> BqResult<BqValue> {
    if lhs.is_null() {
        return Ok(BqValue::Null);
    }
    if let BqValue::Float64(f) = lhs {
        if f.is_nan() {
            if discriminant(lhs) != discriminant(sample) {
                let _ = ops::sql_values_equal(lhs, sample)?;
            }
            return Ok(if has_null {
                BqValue::Null
            } else {
                BqValue::Bool(negated)
            });
        }
    }
    if let Some(key) = value_to_in_key(lhs) {
        if let Some(cands) = buckets.get(&key) {
            for cand in cands {
                if matches!(ops::sql_values_equal(lhs, cand)?, Some(true)) {
                    return Ok(BqValue::Bool(!negated));
                }
            }
        }
        if discriminant(lhs) != discriminant(sample) {
            let _ = ops::sql_values_equal(lhs, sample)?;
        }
        return Ok(if has_null {
            BqValue::Null
        } else {
            BqValue::Bool(negated)
        });
    }
    let _ = ops::sql_values_equal(lhs, sample)?;
    Ok(if has_null {
        BqValue::Null
    } else {
        BqValue::Bool(negated)
    })
}

#[expect(
    clippy::cast_precision_loss,
    clippy::wildcard_enum_match_arm,
    reason = "Numeric hash buckets normalize to canonical IEEE-754 bits and verify exact equality on probe"
)]
fn value_to_in_key(val: &BqValue) -> Option<InHashKey> {
    match val {
        BqValue::Bool(b) => Some(InHashKey::Bool(*b)),
        BqValue::Int64(n) => Some(InHashKey::Num(canonical_f64_bits(*n as f64))),
        BqValue::Float64(f) => Some(InHashKey::Num(canonical_f64_bits(*f))),
        BqValue::Numeric(n) => Some(InHashKey::Num(canonical_f64_bits(n.to_f64()))),
        BqValue::BigNumeric(n) => Some(InHashKey::Num(canonical_f64_bits(n.to_f64()))),
        BqValue::String(s) => Some(InHashKey::String(s.clone())),
        BqValue::Bytes(b) => Some(InHashKey::Bytes(b.clone())),
        BqValue::Date(d) => d.and_hms_opt(0, 0, 0).map(InHashKey::Datetime),
        BqValue::Datetime(dt) => Some(InHashKey::Datetime(*dt)),
        BqValue::Time(t) => Some(InHashKey::Time(*t)),
        BqValue::Timestamp(ts) => Some(InHashKey::Timestamp(*ts)),
        _ => None,
    }
}

fn canonical_f64_bits(f: f64) -> u64 {
    let bits = f.to_bits();
    if bits == (-0.0_f64).to_bits() {
        0.0_f64.to_bits()
    } else {
        bits
    }
}
