use crate::ast::{Join, JoinConstraint, JoinKind, TableFactor};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext};
use crate::executor::evaluator::{eval_expr, values_equal};
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
    if matches!(join.relation, TableFactor::Unnest { .. }) {
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
    let left_null = null_out_eval_row(left_schema);
    let right_null = null_out_eval_row(&right_schema);
    let mut right_matched = vec![false; right_rows.len()];
    let mut out = Vec::new();

    for lrow in left_rows {
        let mut l_matched = false;
        for (r_idx, rrow) in right_rows.iter().enumerate() {
            if let Some(joined) = try_join_pair(lrow, rrow, &join.constraint, ctx)? {
                l_matched = true;
                if let Some(slot) = right_matched.get_mut(r_idx) {
                    *slot = true;
                }
                out.push(joined);
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
    Ok((out, out_schema))
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
    let mut merged = Vec::with_capacity(left.cols.len() + right.cols.len());
    let mut pseudo = left.pseudo_cols.clone();
    for lcol in &left.cols {
        if is_using(&lcol.name) {
            if lcol.qualifier.is_some() {
                pseudo.push(lcol.clone());
            }
            let mut merged_col = lcol.clone();
            merged_col.qualifier = None;
            if merged_col.value.is_null() {
                if let Some(rcol) = right
                    .cols
                    .iter()
                    .find(|c| c.name.eq_ignore_ascii_case(&lcol.name))
                {
                    merged_col.value = rcol.value.clone();
                }
            }
            merged.push(merged_col);
        } else {
            merged.push(lcol.clone());
        }
    }
    for rcol in &right.cols {
        if is_using(&rcol.name) {
            if rcol.qualifier.is_some() {
                pseudo.push(rcol.clone());
            }
        } else {
            merged.push(rcol.clone());
        }
    }
    pseudo.extend(right.pseudo_cols.iter().cloned());
    EvalRow::with_pseudo(merged, pseudo)
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
            if let Some(joined) = try_join_pair(lrow, urow, &join.constraint, ctx)? {
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
    ctx: &ExecContext<'_>,
) -> BqResult<Option<EvalRow>> {
    match constraint {
        JoinConstraint::None => Ok(Some(combine_rows(lrow, rrow))),
        JoinConstraint::On(expr) => {
            let combined = combine_rows(lrow, rrow);
            let pred = eval_expr(expr, &combined, ctx)?;
            if matches!(pred, BqValue::Bool(true)) {
                Ok(Some(combined))
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
                if lv.value.is_null() || rv.value.is_null() || !values_equal(&lv.value, &rv.value)?
                {
                    return Ok(None);
                }
            }
            Ok(Some(combine_rows_for_constraint(lrow, rrow, constraint)))
        }
    }
}

pub(super) fn combine_rows(l: &EvalRow, r: &EvalRow) -> EvalRow {
    let mut cols = Vec::with_capacity(l.cols.len() + r.cols.len());
    cols.extend(l.cols.iter().cloned());
    cols.extend(r.cols.iter().cloned());
    let mut pseudo = Vec::with_capacity(l.pseudo_cols.len() + r.pseudo_cols.len());
    pseudo.extend(l.pseudo_cols.iter().cloned());
    pseudo.extend(r.pseudo_cols.iter().cloned());
    EvalRow::with_pseudo(cols, pseudo)
}
