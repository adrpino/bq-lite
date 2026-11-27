use crate::types::{BqType, BqValue};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryColumn {
    pub name: String,
    pub ty: BqType,
}

impl QueryColumn {
    #[must_use]
    pub fn new(name: impl Into<String>, ty: BqType) -> Self {
        Self {
            name: name.into(),
            ty,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueryResult {
    pub columns: Vec<QueryColumn>,
    pub rows: Vec<Vec<BqValue>>,
    pub rows_affected: u64,
}

impl QueryResult {
    #[must_use]
    pub const fn new(columns: Vec<QueryColumn>, rows: Vec<Vec<BqValue>>) -> Self {
        Self {
            columns,
            rows,
            rows_affected: 0,
        }
    }

    #[must_use]
    pub const fn empty() -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
            rows_affected: 0,
        }
    }

    #[must_use]
    pub const fn affected(rows_affected: u64) -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
            rows_affected,
        }
    }

    #[must_use]
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// Estimates the heap footprint of this `QueryResult` in bytes for cache budgeting.
    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        let mut total = size_of::<Self>()
            + self
                .columns
                .iter()
                .map(|c| size_of::<QueryColumn>() + c.name.len())
                .sum::<usize>();
        for row in &self.rows {
            total = total.saturating_add(size_of::<Vec<BqValue>>());
            for val in row {
                total = total.saturating_add(bq_value_estimated_bytes(val));
            }
        }
        total
    }
}

fn bq_value_estimated_bytes(val: &BqValue) -> usize {
    let base = size_of::<BqValue>();
    match val {
        BqValue::Null
        | BqValue::Bool(_)
        | BqValue::Int64(_)
        | BqValue::Float64(_)
        | BqValue::Numeric(_)
        | BqValue::Date(_)
        | BqValue::Datetime(_)
        | BqValue::Time(_)
        | BqValue::Timestamp(_)
        | BqValue::Interval(_) => base,
        BqValue::BigNumeric(_) => base + 64,
        BqValue::String(s) | BqValue::Geography(s) => base.saturating_add(s.len()),
        BqValue::Bytes(b) => base.saturating_add(b.len()),
        BqValue::Json(j) => base.saturating_add(json_estimated_bytes(j)),
        BqValue::Range(r) => {
            let s = r.start.as_ref().map_or(0, bq_value_estimated_bytes);
            let e = r.end.as_ref().map_or(0, bq_value_estimated_bytes);
            base.saturating_add(s).saturating_add(e)
        }
        BqValue::Array(items) => items.iter().fold(base, |acc, v| {
            acc.saturating_add(bq_value_estimated_bytes(v))
        }),
        BqValue::Struct(fields) => fields.iter().fold(base, |acc, f| {
            let name_len = f.name.as_ref().map_or(0, String::len);
            acc.saturating_add(name_len)
                .saturating_add(bq_value_estimated_bytes(&f.value))
        }),
    }
}

fn json_estimated_bytes(v: &serde_json::Value) -> usize {
    let base = size_of::<serde_json::Value>();
    match v {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => base,
        serde_json::Value::String(s) => base.saturating_add(s.len()),
        serde_json::Value::Array(items) => items
            .iter()
            .fold(base, |acc, x| acc.saturating_add(json_estimated_bytes(x))),
        serde_json::Value::Object(map) => map.iter().fold(base, |acc, (k, x)| {
            acc.saturating_add(k.len())
                .saturating_add(json_estimated_bytes(x))
        }),
    }
}
