use crate::ast::{ArrayAccessKind, DmlReturning, Expr, Select, UpdatePathStep};
use crate::catalog::TableMeta;
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext, RowBinding};
use crate::executor::evaluator::eval_expr;
use crate::executor::query::{infer_select_columns_public, project_eval_rows_public};
use crate::executor::result::{QueryColumn, QueryResult};
use crate::types::coercion::coerce_value;
use crate::types::{BqType, BqValue};

pub(super) fn is_default_sentinel(expr: &Expr) -> bool {
    matches!(expr, Expr::SystemVar(s) if s == "__bq_default__")
}

pub(super) fn eval_update_val(
    meta: &TableMeta,
    path: &[UpdatePathStep],
    expr: &Expr,
    eval_row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    if !is_default_sentinel(expr) {
        return eval_expr(expr, eval_row, ctx);
    }
    let (col_idx, sub_steps) = resolve_assignment_root(meta, path)?;
    if !sub_steps.is_empty() {
        return Ok(BqValue::Null);
    }
    meta.columns
        .get(col_idx)
        .and_then(|c| c.default_expr.as_ref())
        .map_or(Ok(BqValue::Null), |def| {
            eval_expr(def, &EvalRow::default(), ctx)
        })
}

pub(super) struct AssignCtx<'a> {
    pub eval_row: &'a EvalRow,
    pub ctx: &'a ExecContext<'a>,
    pub touched_array_offsets: Vec<(String, usize)>,
}

impl<'a> AssignCtx<'a> {
    pub(super) const fn new(eval_row: &'a EvalRow, ctx: &'a ExecContext<'a>) -> Self {
        Self {
            eval_row,
            ctx,
            touched_array_offsets: Vec::new(),
        }
    }
}

pub(super) fn apply_assignment(
    meta: &TableMeta,
    row: &mut [BqValue],
    path: &[UpdatePathStep],
    val: BqValue,
    actx: &mut AssignCtx<'_>,
) -> BqResult<()> {
    let (idx, sub_steps) = resolve_assignment_root(meta, path)?;
    let col_schema = meta
        .columns
        .get(idx)
        .ok_or_else(|| BqError::internal("column idx"))?;
    if sub_steps.is_empty() {
        let coerced = coerce_value(val, &col_schema.ty)?;
        if !col_schema.nullable && coerced.is_null() {
            return Err(BqError::invalid_argument(format!(
                "Column '{}' cannot be NULL",
                col_schema.name
            )));
        }
        if let Some(slot) = row.get_mut(idx) {
            *slot = coerced;
        }
        return Ok(());
    }
    if let Some(slot) = row.get_mut(idx) {
        let prefix = col_schema.name.to_ascii_lowercase();
        update_nested_path(slot, &col_schema.ty, &prefix, sub_steps, val, actx)?;
    }
    Ok(())
}

fn resolve_assignment_root<'a>(
    meta: &TableMeta,
    path: &'a [UpdatePathStep],
) -> BqResult<(usize, &'a [UpdatePathStep])> {
    let [UpdatePathStep::Field(first), rest @ ..] = path else {
        return Err(BqError::query(
            "Assignment target must begin with column name",
        ));
    };
    if let Some(idx) = meta.column_index(&first.value) {
        return Ok((idx, rest));
    }
    if let [UpdatePathStep::Field(second), tail @ ..] = rest {
        if let Some(idx) = meta.column_index(&second.value) {
            return Ok((idx, tail));
        }
    }
    Err(BqError::not_found(format!(
        "Column '{}' not found",
        first.value
    )))
}

fn update_nested_path(
    target: &mut BqValue,
    target_ty: &BqType,
    prefix: &str,
    steps: &[UpdatePathStep],
    val: BqValue,
    actx: &mut AssignCtx<'_>,
) -> BqResult<()> {
    let [head, tail @ ..] = steps else {
        *target = coerce_value(val, target_ty)?;
        return Ok(());
    };
    match head {
        UpdatePathStep::Field(ident) => {
            let BqValue::Struct(fields) = target else {
                return Err(BqError::semantic(format!(
                    "Cannot set field '{}' of NULL or non-STRUCT ({target_ty})",
                    ident.value
                )));
            };
            let field_ty = match target_ty {
                BqType::Struct(sf) => sf
                    .iter()
                    .find(|f| {
                        f.name
                            .as_deref()
                            .is_some_and(|n| n.eq_ignore_ascii_case(&ident.value))
                    })
                    .map_or(BqType::UntypedNull, |f| f.ty.clone()),
                BqType::UntypedNull
                | BqType::Bool
                | BqType::Int64
                | BqType::Float64
                | BqType::Numeric
                | BqType::BigNumeric
                | BqType::String
                | BqType::Bytes
                | BqType::Date
                | BqType::Datetime
                | BqType::Time
                | BqType::Timestamp
                | BqType::Interval
                | BqType::Json
                | BqType::Geography
                | BqType::Range(_)
                | BqType::Array(_) => BqType::UntypedNull,
            };
            let field = fields
                .iter_mut()
                .find(|f| {
                    f.name
                        .as_deref()
                        .is_some_and(|n| n.eq_ignore_ascii_case(&ident.value))
                })
                .ok_or_else(|| {
                    BqError::not_found(format!("STRUCT field '{}' not found", ident.value))
                })?;
            let next_prefix = format!("{prefix}.{}", ident.value.to_ascii_lowercase());
            update_nested_path(&mut field.value, &field_ty, &next_prefix, tail, val, actx)
        }
        UpdatePathStep::Index { kind, index } => {
            update_array_element(target, target_ty, prefix, (*kind, index), tail, val, actx)
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Recursive DML array-element mutation needs target value/type, prefix, index spec, remaining path, new value, and assignment context"
)]
fn update_array_element(
    target: &mut BqValue,
    target_ty: &BqType,
    prefix: &str,
    (kind, index_expr): (ArrayAccessKind, &Expr),
    tail: &[UpdatePathStep],
    val: BqValue,
    actx: &mut AssignCtx<'_>,
) -> BqResult<()> {
    let BqValue::Array(items) = target else {
        return Err(BqError::semantic(format!(
            "Cannot set element of NULL or non-ARRAY ({target_ty})"
        )));
    };
    let idx_val = eval_expr(index_expr, actx.eval_row, actx.ctx)?;
    let BqValue::Int64(raw_idx) = idx_val else {
        return Err(BqError::semantic(
            "Array subscript index in UPDATE SET must be non-NULL INT64",
        ));
    };
    let pos_i64 = match kind {
        ArrayAccessKind::Ordinal | ArrayAccessKind::SafeOrdinal => raw_idx - 1,
        ArrayAccessKind::Offset | ArrayAccessKind::SafeOffset | ArrayAccessKind::Raw => raw_idx,
    };
    let pos = usize::try_from(pos_i64).map_err(|_| {
        BqError::semantic(format!(
            "Array index {raw_idx} is out of bounds for UPDATE SET"
        ))
    })?;
    let key = (prefix.to_owned(), pos);
    if actx.touched_array_offsets.contains(&key) {
        return Err(BqError::semantic(format!(
            "Cannot perform multiple updates to offset {pos} on type {target_ty}"
        )));
    }
    actx.touched_array_offsets.push(key);
    let len = items.len();
    let elem = items.get_mut(pos).ok_or_else(|| {
        BqError::semantic(format!(
            "Array index {raw_idx} is out of bounds for ARRAY of length {len}"
        ))
    })?;
    let elem_ty = match target_ty {
        BqType::Array(inner) => inner.as_ref().clone(),
        BqType::UntypedNull
        | BqType::Bool
        | BqType::Int64
        | BqType::Float64
        | BqType::Numeric
        | BqType::BigNumeric
        | BqType::String
        | BqType::Bytes
        | BqType::Date
        | BqType::Datetime
        | BqType::Time
        | BqType::Timestamp
        | BqType::Interval
        | BqType::Json
        | BqType::Geography
        | BqType::Range(_)
        | BqType::Struct(_) => BqType::UntypedNull,
    };
    let next_prefix = format!("{prefix}[{pos}]");
    update_nested_path(elem, &elem_ty, &next_prefix, tail, val, actx)
}

pub(super) fn check_assert_rows_modified(
    assert_expr: Option<&Expr>,
    affected: u64,
    ctx: &ExecContext<'_>,
) -> BqResult<()> {
    let Some(expr) = assert_expr else {
        return Ok(());
    };
    let val = eval_expr(expr, &EvalRow::default(), ctx)?;
    let BqValue::Int64(expected) = val else {
        return Err(BqError::semantic(
            "ASSERT_ROWS_MODIFIED must have a non-NULL INT64 argument",
        ));
    };
    let actual_i64 = i64::try_from(affected).unwrap_or(i64::MAX);
    if expected != actual_i64 {
        return Err(BqError::semantic(format!(
            "ASSERT_ROWS_MODIFIED expected {expected} rows modified, but found {actual_i64}"
        )));
    }
    Ok(())
}

pub(super) fn build_returning_result(
    schema_row: &EvalRow,
    eval_rows: &[EvalRow],
    returning: &DmlReturning,
    action_label: &str,
    ctx: &ExecContext<'_>,
) -> BqResult<QueryResult> {
    let dummy_sel = Select {
        distinct: false,
        as_kind: None,
        projection: returning.items.clone(),
        from: Vec::new(),
        where_clause: None,
        group_by: None,
        having: None,
        qualify: None,
        window: Vec::new(),
    };
    let projected = project_eval_rows_public(&dummy_sel, eval_rows, ctx)?;
    let mut cols = if projected.is_empty() {
        infer_select_columns_public(&dummy_sel, schema_row, ctx)
    } else {
        projected
            .first()
            .map(|r| {
                r.cols
                    .iter()
                    .map(|c| QueryColumn::new(c.name.as_ref().to_owned(), c.ty.clone()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut out_rows: Vec<Vec<BqValue>> = projected
        .into_iter()
        .map(|r| r.cols.into_iter().map(|c| c.value).collect())
        .collect();
    if let Some(ref opt_alias) = returning.with_action {
        let col_name = opt_alias
            .as_ref()
            .map_or_else(|| "ACTION".to_owned(), |id| id.value.clone());
        cols.push(QueryColumn::new(col_name, BqType::String));
        for row in &mut out_rows {
            row.push(BqValue::String(action_label.to_owned()));
        }
    }
    let mut qr = QueryResult::new(cols, out_rows);
    qr.rows_affected = u64::try_from(qr.rows.len()).unwrap_or(u64::MAX);
    Ok(qr)
}

pub(super) fn schema_eval_row(meta: &TableMeta, qual: &str) -> EvalRow {
    let qual_arc: Option<std::sync::Arc<str>> = Some(std::sync::Arc::from(qual));
    EvalRow::new(
        meta.columns
            .iter()
            .map(|col| RowBinding {
                qualifier: qual_arc.clone(),
                name: std::sync::Arc::from(col.name.as_str()),
                ty: col.ty.clone(),
                value: BqValue::Null,
            })
            .collect(),
    )
}
