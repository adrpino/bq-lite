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
}
