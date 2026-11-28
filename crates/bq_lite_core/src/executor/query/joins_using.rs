use std::sync::Arc;

use crate::ast::{Ident, JoinConstraint};
use crate::executor::context::{ColMeta, EvalRow, RowSchema};
use crate::types::BqValue;

use super::joins::{combine_rows, combine_rows_for_constraint};

#[derive(Debug, Clone, Copy)]
enum ValueSource {
    LeftCol(usize),
    RightCol(usize),
    LeftPseudo(usize),
    RightPseudo(usize),
    CoalesceCol {
        left: Option<usize>,
        right: Option<usize>,
    },
}

/// Precomputed schema and column-index plan for `JOIN ... USING (...)` so matched and outer
/// rows share a single `Arc<RowSchema>` without rebuilding metadata or indexes per row.
pub(super) struct UsingJoinPlan {
    left_schema: Arc<RowSchema>,
    right_schema: Arc<RowSchema>,
    out_schema: Arc<RowSchema>,
    col_sources: Vec<ValueSource>,
    pseudo_sources: Vec<ValueSource>,
}

impl UsingJoinPlan {
    pub(super) fn new(
        left_schema: &Arc<RowSchema>,
        right_schema: &Arc<RowSchema>,
        cols: &[Ident],
    ) -> Self {
        let is_using = |name: &str| cols.iter().any(|c| c.value.eq_ignore_ascii_case(name));
        let mut col_metas = Vec::with_capacity(left_schema.cols.len() + right_schema.cols.len());
        let mut col_sources = Vec::with_capacity(left_schema.cols.len() + right_schema.cols.len());
        let mut pseudo_metas: Vec<ColMeta> = left_schema.pseudo_cols.clone();
        let mut pseudo_sources: Vec<ValueSource> = (0..left_schema.pseudo_cols.len())
            .map(ValueSource::LeftPseudo)
            .collect();

        record_using_table_col_plan(
            &left_schema.cols,
            is_using,
            true,
            &mut pseudo_metas,
            &mut pseudo_sources,
        );
        record_using_table_col_plan(
            &right_schema.cols,
            is_using,
            false,
            &mut pseudo_metas,
            &mut pseudo_sources,
        );

        for ucol in cols {
            let l_idx = left_schema
                .cols
                .iter()
                .position(|c| c.name.eq_ignore_ascii_case(&ucol.value));
            let r_idx = right_schema
                .cols
                .iter()
                .position(|c| c.name.eq_ignore_ascii_case(&ucol.value));
            let base = l_idx
                .and_then(|i| left_schema.cols.get(i))
                .or_else(|| r_idx.and_then(|i| right_schema.cols.get(i)));
            if let Some(meta) = base {
                col_metas.push(ColMeta {
                    qualifier: None,
                    name: Arc::clone(&meta.name),
                    ty: meta.ty.clone(),
                });
                col_sources.push(ValueSource::CoalesceCol {
                    left: l_idx,
                    right: r_idx,
                });
            }
        }
        for (idx, lcol) in left_schema.cols.iter().enumerate() {
            if !is_using(&lcol.name) {
                col_metas.push(lcol.clone());
                col_sources.push(ValueSource::LeftCol(idx));
            }
        }
        for (idx, rcol) in right_schema.cols.iter().enumerate() {
            if !is_using(&rcol.name) {
                col_metas.push(rcol.clone());
                col_sources.push(ValueSource::RightCol(idx));
            }
        }
        for (idx, rp) in right_schema.pseudo_cols.iter().enumerate() {
            let already = rp.qualifier.as_deref().is_some_and(|rq| {
                pseudo_metas.iter().any(|p| {
                    p.qualifier
                        .as_deref()
                        .is_some_and(|pq| pq.eq_ignore_ascii_case(rq))
                        && p.name.eq_ignore_ascii_case(&rp.name)
                })
            });
            if !already {
                pseudo_metas.push(rp.clone());
                pseudo_sources.push(ValueSource::RightPseudo(idx));
            }
        }

        Self {
            left_schema: Arc::clone(left_schema),
            right_schema: Arc::clone(right_schema),
            out_schema: Arc::new(RowSchema::new(col_metas, pseudo_metas)),
            col_sources,
            pseudo_sources,
        }
    }

    pub(super) fn combine(
        &self,
        lrow: &EvalRow,
        rrow: &EvalRow,
        constraint: &JoinConstraint,
    ) -> EvalRow {
        if !Arc::ptr_eq(&lrow.schema, &self.left_schema)
            || !Arc::ptr_eq(&rrow.schema, &self.right_schema)
        {
            return combine_rows_for_constraint(lrow, rrow, constraint);
        }
        let mut values = Vec::with_capacity(self.col_sources.len());
        for &src in &self.col_sources {
            values.push(read_source(src, lrow, rrow));
        }
        let mut pseudo_values = Vec::with_capacity(self.pseudo_sources.len());
        for &src in &self.pseudo_sources {
            pseudo_values.push(read_source(src, lrow, rrow));
        }
        EvalRow::from_schema_with_pseudo(Arc::clone(&self.out_schema), values, pseudo_values)
    }
}

/// Unified row combiner for both `ON` / `CROSS` (schema concat) and `USING` joins.
pub(super) enum JoinOutputPlan {
    Concat {
        left_schema: Arc<RowSchema>,
        right_schema: Arc<RowSchema>,
        out_schema: Arc<RowSchema>,
    },
    Using(UsingJoinPlan),
}

impl JoinOutputPlan {
    pub(super) fn new(
        left_schema: &Arc<RowSchema>,
        right_schema: &Arc<RowSchema>,
        constraint: &JoinConstraint,
    ) -> Self {
        match constraint {
            JoinConstraint::Using(cols) => {
                Self::Using(UsingJoinPlan::new(left_schema, right_schema, cols))
            }
            JoinConstraint::None | JoinConstraint::On(_) => Self::Concat {
                left_schema: Arc::clone(left_schema),
                right_schema: Arc::clone(right_schema),
                out_schema: RowSchema::concat(left_schema, right_schema),
            },
        }
    }

    pub(super) fn combine(
        &self,
        lrow: &EvalRow,
        rrow: &EvalRow,
        constraint: &JoinConstraint,
    ) -> EvalRow {
        match self {
            Self::Using(plan) => plan.combine(lrow, rrow, constraint),
            Self::Concat {
                left_schema,
                right_schema,
                out_schema,
            } => {
                if Arc::ptr_eq(&lrow.schema, left_schema) && Arc::ptr_eq(&rrow.schema, right_schema)
                {
                    EvalRow::combine_with_schema(lrow, rrow, Arc::clone(out_schema))
                } else {
                    combine_rows(lrow, rrow)
                }
            }
        }
    }
}

#[inline]
fn read_source(src: ValueSource, lrow: &EvalRow, rrow: &EvalRow) -> BqValue {
    match src {
        ValueSource::LeftCol(i) => lrow.values.get(i).cloned().unwrap_or(BqValue::Null),
        ValueSource::RightCol(i) => rrow.values.get(i).cloned().unwrap_or(BqValue::Null),
        ValueSource::LeftPseudo(i) => lrow.pseudo_values.get(i).cloned().unwrap_or(BqValue::Null),
        ValueSource::RightPseudo(i) => rrow.pseudo_values.get(i).cloned().unwrap_or(BqValue::Null),
        ValueSource::CoalesceCol { left, right } => {
            let lv = left
                .and_then(|i| lrow.values.get(i))
                .cloned()
                .unwrap_or(BqValue::Null);
            if lv.is_null() {
                right
                    .and_then(|i| rrow.values.get(i))
                    .cloned()
                    .unwrap_or(BqValue::Null)
            } else {
                lv
            }
        }
    }
}

fn record_using_table_col_plan(
    cols: &[ColMeta],
    is_using: impl Fn(&str) -> bool,
    is_left: bool,
    pseudo_metas: &mut Vec<ColMeta>,
    pseudo_sources: &mut Vec<ValueSource>,
) {
    let mut touched_quals: Vec<&str> = Vec::new();
    for col in cols {
        if is_using(&col.name) {
            if let Some(q) = col.qualifier.as_deref() {
                if !touched_quals.iter().any(|t| t.eq_ignore_ascii_case(q)) {
                    touched_quals.push(q);
                }
            }
        }
    }
    for q in touched_quals {
        let already = pseudo_metas.iter().any(|p| {
            !p.name.starts_with("_PARTITION")
                && p.qualifier
                    .as_deref()
                    .is_some_and(|pq| pq.eq_ignore_ascii_case(q))
        });
        if already {
            continue;
        }
        for (idx, col) in cols.iter().enumerate() {
            if col
                .qualifier
                .as_deref()
                .is_some_and(|cq| cq.eq_ignore_ascii_case(q))
            {
                pseudo_metas.push(col.clone());
                pseudo_sources.push(if is_left {
                    ValueSource::LeftCol(idx)
                } else {
                    ValueSource::RightCol(idx)
                });
            }
        }
    }
}
