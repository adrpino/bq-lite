use std::sync::Arc;

use crate::ast::{Join, JoinConstraint, JoinKind, TableFactor};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext, RowBinding, RowRef, RowSchema};
use crate::executor::evaluator::{eval_expr, eval_expr_ref, values_equal};
use crate::types::BqValue;

use super::from::{eval_correlated_cross_or_factor, eval_table_factor};
use super::joins_hash::try_hash_join;
use super::schema::{infer_table_factor_schema, null_out_eval_row};

pub(super) fn eval_join_with_schema(
    left_rows: &[EvalRow],
    left_schema: &EvalRow,
    join: &Join,
    ctx: &ExecContext<'_>,
) -> BqResult<(Vec<EvalRow>, EvalRow)> {
    if matches!(join.relation, TableFactor::Unnest { .. })
        && !matches!(join.kind, JoinKind::Right | JoinKind::Full)
    {
        let rows = eval_correlated_unnest_join(left_rows, join, ctx)?;
        let r_schema = infer_table_factor_schema(&join.relation, Some(left_schema), ctx)?;
        let schema = merged_join_schema(left_schema, &r_schema, &join.constraint);
        return Ok((rows, schema));
    }
    let right_rows = eval_table_factor(&join.relation, None, ctx)?;
    let right_schema = right_rows
        .first()
        .map(null_out_eval_row)
        .map_or_else(|| infer_table_factor_schema(&join.relation, None, ctx), Ok)?;
    let out_schema = merged_join_schema(left_schema, &right_schema, &join.constraint);

    if join.kind == JoinKind::Cross {
        let rows = eval_correlated_cross_or_factor(left_rows, &join.relation, ctx)?;
        return Ok((rows, out_schema));
    }
    if let Some(rows) = try_hash_join(
        left_rows,
        left_schema,
        &right_rows,
        &right_schema,
        join,
        ctx,
    )? {
        return Ok((rows, out_schema));
    }
    let rows = nested_loop_join(
        left_rows,
        left_schema,
        &right_rows,
        &right_schema,
        join,
        ctx,
    )?;
    Ok((rows, out_schema))
}

fn nested_loop_join(
    left_rows: &[EvalRow],
    left_schema: &EvalRow,
    right_rows: &[EvalRow],
    right_schema: &EvalRow,
    join: &Join,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    let left_null = null_out_eval_row(left_schema);
    let right_null = null_out_eval_row(right_schema);
    let mut right_matched = vec![false; right_rows.len()];
    let mut out = Vec::new();
    let shared_on_schema = matches!(join.constraint, JoinConstraint::On(_))
        .then(|| RowSchema::concat(&left_schema.schema, &right_schema.schema));

    for lrow in left_rows {
        let mut l_matched = false;
        if let JoinConstraint::On(ref expr) = join.constraint {
            for (r_idx, rrow) in right_rows.iter().enumerate() {
                let pred = eval_expr_ref(
                    expr,
                    RowRef::Joined {
                        left: lrow,
                        right: rrow,
                    },
                    ctx,
                )?;
                if matches!(pred, BqValue::Bool(true)) {
                    l_matched = true;
                    if let Some(slot) = right_matched.get_mut(r_idx) {
                        *slot = true;
                    }
                    let schema = match &shared_on_schema {
                        Some(cs)
                            if Arc::ptr_eq(&lrow.schema, &left_schema.schema)
                                && Arc::ptr_eq(&rrow.schema, &right_schema.schema) =>
                        {
                            Arc::clone(cs)
                        }
                        _ => RowSchema::concat(&lrow.schema, &rrow.schema),
                    };
                    out.push(EvalRow::combine_with_schema(lrow, rrow, schema));
                }
            }
        } else {
            for (r_idx, rrow) in right_rows.iter().enumerate() {
                if let Some(joined) = try_join_pair(lrow, rrow, &join.constraint, None, ctx)? {
                    l_matched = true;
                    if let Some(slot) = right_matched.get_mut(r_idx) {
                        *slot = true;
                    }
                    out.push(joined);
                }
            }
        }
        if !l_matched && matches!(join.kind, JoinKind::Left | JoinKind::Full) {
            out.push(combine_rows_for_constraint(
                lrow,
                &right_null,
                &join.constraint,
            ));
        }
    }

    if matches!(join.kind, JoinKind::Right | JoinKind::Full) {
        for (r_idx, rrow) in right_rows.iter().enumerate() {
            if !right_matched.get(r_idx).copied().unwrap_or(false) {
                out.push(combine_rows_for_constraint(
                    &left_null,
                    rrow,
                    &join.constraint,
                ));
            }
        }
    }
    Ok(out)
}

fn merged_join_schema(left: &EvalRow, right: &EvalRow, constraint: &JoinConstraint) -> EvalRow {
    combine_rows_for_constraint(left, right, constraint)
}

pub(super) fn combine_rows_for_constraint(
    left: &EvalRow,
    right: &EvalRow,
    constraint: &JoinConstraint,
) -> EvalRow {
    let JoinConstraint::Using(cols) = constraint else {
        return combine_rows(left, right);
    };
    let is_using = |name: &str| cols.iter().any(|c| c.value.eq_ignore_ascii_case(name));
    let mut merged = Vec::with_capacity(left.col_len() + right.col_len());
    let mut pseudo: Vec<RowBinding> = left
        .iter_pseudo()
        .map(crate::executor::row::BindingRef::to_owned_binding)
        .collect();

    // Record full ordered columns per qualifier for sides that lose a USING column so `t.*`
    // preserves the table's exact original column order and pre-coalesce values.
    record_using_table_cols(left, is_using, &mut pseudo);
    record_using_table_cols(right, is_using, &mut pseudo);

    // BigQuery emits merged USING columns FIRST (in USING list order), then left non-USING, then right non-USING.
    for ucol in cols {
        let lmatch = left
            .iter_cols()
            .find(|c| c.name.eq_ignore_ascii_case(&ucol.value));
        let rmatch = right
            .iter_cols()
            .find(|c| c.name.eq_ignore_ascii_case(&ucol.value));
        if let Some(base) = lmatch.or(rmatch) {
            let mut merged_col = base.to_owned_binding();
            merged_col.qualifier = None;
            if merged_col.value.is_null() {
                if let Some(rc) = rmatch {
                    merged_col.value = rc.value.clone();
                }
            }
            merged.push(merged_col);
        }
    }
    for lcol in left.iter_cols() {
        if !is_using(lcol.name) {
            merged.push(lcol.to_owned_binding());
        }
    }
    for rcol in right.iter_cols() {
        if !is_using(rcol.name) {
            merged.push(rcol.to_owned_binding());
        }
    }
    for rp in right.iter_pseudo() {
        let already = rp.qualifier.is_some_and(|rq| {
            pseudo.iter().any(|p| {
                p.qualifier
                    .as_deref()
                    .is_some_and(|pq| pq.eq_ignore_ascii_case(rq))
                    && p.name.eq_ignore_ascii_case(rp.name)
            })
        });
        if !already {
            pseudo.push(rp.to_owned_binding());
        }
    }
    EvalRow::with_pseudo(merged, pseudo)
}

fn record_using_table_cols(
    side: &EvalRow,
    is_using: impl Fn(&str) -> bool,
    pseudo: &mut Vec<RowBinding>,
) {
    let mut touched_quals: Vec<String> = Vec::new();
    for col in side.iter_cols() {
        if is_using(col.name) {
            if let Some(q) = col.qualifier {
                if !touched_quals.iter().any(|t| t.eq_ignore_ascii_case(q)) {
                    touched_quals.push(q.as_ref().to_owned());
                }
            }
        }
    }
    for q in &touched_quals {
        let already = pseudo.iter().any(|p| {
            !p.name.starts_with("_PARTITION")
                && p.qualifier
                    .as_deref()
                    .is_some_and(|pq| pq.eq_ignore_ascii_case(q))
        });
        if already {
            continue;
        }
        for col in side.iter_cols() {
            if col.qualifier.is_some_and(|cq| cq.eq_ignore_ascii_case(q)) {
                pseudo.push(col.to_owned_binding());
            }
        }
    }
}

fn eval_correlated_unnest_join(
    left_rows: &[EvalRow],
    join: &Join,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    let mut out = Vec::new();
    for lrow in left_rows {
        let unnested = eval_table_factor(&join.relation, Some(lrow), ctx)?;
        let mut matched_any = false;
        for urow in &unnested {
            if let Some(joined) = try_join_pair(lrow, urow, &join.constraint, None, ctx)? {
                matched_any = true;
                out.push(joined);
            }
        }
        if !matched_any && matches!(join.kind, JoinKind::Left | JoinKind::Full) {
            let null_unnest = infer_table_factor_schema(&join.relation, Some(lrow), ctx)?;
            out.push(combine_rows_for_constraint(
                lrow,
                &null_unnest,
                &join.constraint,
            ));
        }
    }
    Ok(out)
}

pub(super) fn try_join_pair(
    lrow: &EvalRow,
    rrow: &EvalRow,
    constraint: &JoinConstraint,
    shared_schema: Option<&Arc<RowSchema>>,
    ctx: &ExecContext<'_>,
) -> BqResult<Option<EvalRow>> {
    match constraint {
        JoinConstraint::None => Ok(Some(shared_schema.map_or_else(
            || combine_rows(lrow, rrow),
            |s| EvalRow::combine_with_schema(lrow, rrow, Arc::clone(s)),
        ))),
        JoinConstraint::On(expr) => {
            let pred = eval_expr_ref(
                expr,
                RowRef::Joined {
                    left: lrow,
                    right: rrow,
                },
                ctx,
            )?;
            if matches!(pred, BqValue::Bool(true)) {
                Ok(Some(shared_schema.map_or_else(
                    || combine_rows(lrow, rrow),
                    |s| EvalRow::combine_with_schema(lrow, rrow, Arc::clone(s)),
                )))
            } else {
                Ok(None)
            }
        }
        JoinConstraint::Using(cols) => {
            for col in cols {
                let lv = lrow.resolve_ident(&col.value)?.ok_or_else(|| {
                    BqError::query(format!("USING column '{}' not found on left", col.value))
                })?;
                let rv = rrow.resolve_ident(&col.value)?.ok_or_else(|| {
                    BqError::query(format!("USING column '{}' not found on right", col.value))
                })?;
                if lv.value.is_null() || rv.value.is_null() || !values_equal(lv.value, rv.value)? {
                    return Ok(None);
                }
            }
            Ok(Some(combine_rows_for_constraint(lrow, rrow, constraint)))
        }
    }
}

pub(super) fn combine_rows(l: &EvalRow, r: &EvalRow) -> EvalRow {
    let schema = RowSchema::concat(&l.schema, &r.schema);
    EvalRow::combine_with_schema(l, r, schema)
}

/// Resolves a correlated `FROM t.arr` / `FROM t.s.arr` table factor against `ctx.outer_rows`
/// as an implicit `UNNEST(...)`.
pub(super) fn try_eval_correlated_array_ref(
    name: &crate::ast::ObjectName,
    alias: Option<&str>,
    ctx: &ExecContext<'_>,
) -> BqResult<Option<Vec<EvalRow>>> {
    if ctx.outer_rows.is_empty() || name.parts.len() < 2 {
        return Ok(None);
    }
    let expr = crate::ast::Expr::CompoundIdent(name.parts.clone());
    let empty = EvalRow::default();
    match eval_expr(&expr, &empty, ctx) {
        Ok(BqValue::Array(_) | BqValue::Null) => {
            let alias_id = alias.map(crate::ast::Ident::unquoted);
            let rows = super::from::eval_unnest(&expr, alias_id.as_ref(), None, None, ctx)?;
            Ok(Some(rows))
        }
        Ok(_) | Err(_) => Ok(None),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "FOR SYSTEM_TIME AS OF expects TIMESTAMP"
)]
pub(super) fn eval_system_time(
    expr: &crate::ast::Expr,
    ctx: &ExecContext<'_>,
) -> BqResult<chrono::DateTime<chrono::Utc>> {
    let val = eval_expr(expr, &EvalRow::default(), ctx)?;
    match val {
        BqValue::Timestamp(ts) => Ok(ts),
        _ => Err(BqError::query(
            "FOR SYSTEM_TIME AS OF requires a TIMESTAMP expression",
        )),
    }
}
