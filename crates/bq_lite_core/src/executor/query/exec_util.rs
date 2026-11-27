use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::executor::context::EvalRow;
use crate::types::BqValue;

/// Zero-copy view over a slice of rows or an indexed subset of a larger row buffer.
#[derive(Clone, Copy)]
pub(super) enum RowSlice<'a> {
    Owned(&'a [EvalRow]),
    Indexed {
        rows: &'a [EvalRow],
        indices: &'a [usize],
    },
}

impl<'a> RowSlice<'a> {
    #[must_use]
    pub(super) const fn len(self) -> usize {
        match self {
            Self::Owned(s) => s.len(),
            Self::Indexed { indices, .. } => indices.len(),
        }
    }

    #[must_use]
    pub(super) fn first(self) -> Option<&'a EvalRow> {
        match self {
            Self::Owned(s) => s.first(),
            Self::Indexed { rows, indices } => indices.first().and_then(|&i| rows.get(i)),
        }
    }

    #[must_use]
    pub(super) const fn iter(self) -> RowSliceIter<'a> {
        RowSliceIter {
            slice: self,
            pos: 0,
        }
    }
}

pub(super) struct RowSliceIter<'a> {
    slice: RowSlice<'a>,
    pos: usize,
}

impl<'a> Iterator for RowSliceIter<'a> {
    type Item = &'a EvalRow;

    fn next(&mut self) -> Option<Self::Item> {
        let item = match self.slice {
            RowSlice::Owned(s) => s.get(self.pos),
            RowSlice::Indexed { rows, indices } => {
                indices.get(self.pos).and_then(|&idx| rows.get(idx))
            }
        };
        if item.is_some() {
            self.pos += 1;
        }
        item
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let rem = self.slice.len().saturating_sub(self.pos);
        (rem, Some(rem))
    }
}

impl ExactSizeIterator for RowSliceIter<'_> {}

/// Computes a canonical hash for a slice of `BqValue`s compatible with `values_distinct_eq`.
/// Values equal under `values_distinct_eq` (including cross-type numerics, `-0.0 == +0.0`,
/// and `NaN == NaN`) are guaranteed to hash identically; callers verify bucket collisions
/// with exact `values_distinct_eq`.
#[must_use]
pub(super) fn hash_value_slice(vals: &[BqValue]) -> u64 {
    let mut h = DefaultHasher::new();
    vals.len().hash(&mut h);
    for v in vals {
        hash_bq_value(v, &mut h);
    }
    h.finish()
}

#[must_use]
pub(super) fn hash_single_value(val: &BqValue) -> u64 {
    let mut h = DefaultHasher::new();
    hash_bq_value(val, &mut h);
    h.finish()
}

#[expect(
    clippy::cast_precision_loss,
    reason = "Canonical numeric hash projects INT64/NUMERIC/BIGNUMERIC to IEEE-754 f64; exact equality resolves collisions"
)]
fn hash_bq_value(val: &BqValue, h: &mut DefaultHasher) {
    match val {
        BqValue::Null => 0_u8.hash(h),
        BqValue::Bool(b) => {
            1_u8.hash(h);
            b.hash(h);
        }
        BqValue::Int64(n) => hash_canonical_num(*n as f64, h),
        BqValue::Float64(f) => hash_canonical_num(*f, h),
        BqValue::Numeric(n) => hash_canonical_num(n.to_f64(), h),
        BqValue::BigNumeric(bn) => hash_canonical_num(bn.to_f64(), h),
        BqValue::String(s) => {
            3_u8.hash(h);
            s.hash(h);
        }
        BqValue::Bytes(b) => {
            4_u8.hash(h);
            b.hash(h);
        }
        BqValue::Date(d) => {
            5_u8.hash(h);
            d.hash(h);
        }
        BqValue::Datetime(dt) => {
            6_u8.hash(h);
            dt.hash(h);
        }
        BqValue::Time(t) => {
            7_u8.hash(h);
            t.hash(h);
        }
        BqValue::Timestamp(ts) => {
            8_u8.hash(h);
            ts.hash(h);
        }
        BqValue::Interval(iv) => {
            9_u8.hash(h);
            iv.months.hash(h);
            iv.days.hash(h);
            iv.micros.hash(h);
        }
        BqValue::Json(j) => {
            10_u8.hash(h);
            j.to_string().hash(h);
        }
        BqValue::Geography(g) => {
            11_u8.hash(h);
            g.hash(h);
        }
        BqValue::Range(r) => {
            12_u8.hash(h);
            if let Some(ref s) = r.start {
                1_u8.hash(h);
                hash_bq_value(s, h);
            } else {
                0_u8.hash(h);
            }
            if let Some(ref e) = r.end {
                1_u8.hash(h);
                hash_bq_value(e, h);
            } else {
                0_u8.hash(h);
            }
        }
        BqValue::Array(items) => {
            13_u8.hash(h);
            items.len().hash(h);
            for item in items {
                hash_bq_value(item, h);
            }
        }
        BqValue::Struct(fields) => {
            14_u8.hash(h);
            fields.len().hash(h);
            for f in fields {
                hash_bq_value(&f.value, h);
            }
        }
    }
}

fn hash_canonical_num(f: f64, h: &mut DefaultHasher) {
    2_u8.hash(h);
    if f.is_nan() {
        u64::MAX.hash(h);
    } else if f == 0.0 {
        0.0_f64.to_bits().hash(h);
    } else {
        f.to_bits().hash(h);
    }
}
