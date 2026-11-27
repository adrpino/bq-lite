use std::sync::Arc;

use crate::ast::{BinaryOp, Expr, Ident, JoinKind, TableFactor, TableWithJoins};
use crate::error::{BqError, BqResult};
use crate::executor::context::{ColMeta, EvalRow, ExecContext, RowBinding, RowSchema};
use crate::executor::evaluator::eval_expr;
use crate::types::{BqType, BqValue};

use super::governance::{unpack_null_struct_unnest, unpack_struct_unnest};
use super::type_infer::infer_expr_type;

pub(super) fn eval_wildcard_tables(
    tref: &crate::catalog::TableRef,
    prefix: &str,
    alias: Option<&str>,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    let ds = ctx.catalog.get_dataset(&tref.project, &tref.dataset)?;
    let mut all_rows = Vec::new();
    for (tbl_name, meta) in &ds.tables {
        let Some(suffix) = tbl_name.strip_prefix(prefix) else {
            continue;
        };
        let concrete_ref = crate::catalog::TableRef::new(&tref.project, &tref.dataset, tbl_name);
        let qual_arc: Option<Arc<str>> = Some(Arc::from(alias.unwrap_or(tbl_name)));
        let mut metas: Vec<ColMeta> = meta
            .columns
            .iter()
            .map(|c| ColMeta {
                qualifier: qual_arc.clone(),
                name: Arc::from(c.name.as_str()),
                ty: c.ty.clone(),
            })
            .collect();
        metas.push(ColMeta {
            qualifier: qual_arc,
            name: Arc::from("_TABLE_SUFFIX"),
            ty: BqType::String,
        });
        let schema = RowSchema::from_cols(metas);
        let (_, rows_iter) = ctx.storage.iter_rows(&concrete_ref);
        for (row, _) in rows_iter {
            let mut vals = Vec::with_capacity(row.len() + 1);
            vals.extend_from_slice(row);
            vals.push(BqValue::String(suffix.to_owned()));
            all_rows.push(EvalRow::from_schema(Arc::clone(&schema), vals));
        }
    }
    Ok(all_rows)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "UNNEST accepts Null or Array"
)]
pub(super) fn eval_unnest(
    expr: &Expr,
    alias: Option<&Ident>,
    with_offset: Option<&Option<Ident>>,
    corr_row: Option<&EvalRow>,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    let empty_row = EvalRow::default();
    let row_ref = corr_row.unwrap_or(&empty_row);
    let val = eval_expr(expr, row_ref, ctx)?;
    let items = match val {
        BqValue::Null => Vec::new(),
        BqValue::Array(items) => items,
        other => {
            return Err(BqError::query(format!(
                "UNNEST expects an ARRAY argument, got {}",
                other.infer_type()?
            )))
        }
    };
    let elem_name: Arc<str> = Arc::from(alias.map_or("f0_", |a| a.value.as_str()));
    let qual_arc: Option<Arc<str>> = alias.map(|a| Arc::from(a.value.as_str()));
    let offset_arc: Option<Arc<str>> = with_offset
        .map(|opt_alias| Arc::from(opt_alias.as_ref().map_or("offset", |a| a.value.as_str())));
    let static_elem_ty = match infer_expr_type(expr, row_ref, ctx) {
        BqType::Array(inner) => Some(*inner),
        _ => None,
    };
    if let Some(ref elem_ty) = static_elem_ty {
        if !matches!(elem_ty, BqType::Struct(_) | BqType::UntypedNull)
            && !items.iter().any(|v| matches!(v, BqValue::Struct(_)))
        {
            let mut metas = vec![ColMeta {
                qualifier: qual_arc,
                name: elem_name,
                ty: elem_ty.clone(),
            }];
            if let Some(offset_name) = offset_arc {
                metas.push(ColMeta {
                    qualifier: None,
                    name: offset_name,
                    ty: BqType::Int64,
                });
            }
            let has_off = metas.len() == 2;
            let schema = RowSchema::from_cols(metas);
            let mut out = Vec::with_capacity(items.len());
            for (idx, item) in items.into_iter().enumerate() {
                let vals = if has_off {
                    vec![item, BqValue::Int64(i64::try_from(idx).unwrap_or(i64::MAX))]
                } else {
                    vec![item]
                };
                out.push(EvalRow::from_schema(Arc::clone(&schema), vals));
            }
            return Ok(out);
        }
    }
    let mut out = Vec::with_capacity(items.len());
    for (idx, item) in items.into_iter().enumerate() {
        let ty = item.infer_type()?;
        let (mut cols, pseudo) = match (&item, &static_elem_ty) {
            (BqValue::Struct(fields), _) => unpack_struct_unnest(fields, alias, &item, &ty),
            (BqValue::Null, Some(BqType::Struct(sf))) => unpack_null_struct_unnest(sf, alias),
            _ => (
                vec![RowBinding {
                    qualifier: qual_arc.clone(),
                    name: elem_name.clone(),
                    ty,
                    value: item,
                }],
                Vec::new(),
            ),
        };
        if let Some(ref offset_name) = offset_arc {
            let off_val = i64::try_from(idx).unwrap_or(i64::MAX);
            cols.push(RowBinding {
                qualifier: None,
                name: offset_name.clone(),
                ty: BqType::Int64,
                value: BqValue::Int64(off_val),
            });
        }
        out.push(EvalRow::with_pseudo(cols, pseudo));
    }
    Ok(out)
}

/// Zero-intermediate fast path for `FROM lhs, UNNEST(lhs.arr) AS alias [WITH OFFSET]`
/// when the array element type is a known non-struct scalar type.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "UNNEST accepts Null or Array"
)]
pub(super) fn try_eval_unnest_cross_direct(
    left_rows: &[EvalRow],
    factor: &TableFactor,
    ctx: &ExecContext<'_>,
) -> BqResult<Option<Vec<EvalRow>>> {
    let TableFactor::Unnest {
        expr,
        alias,
        with_offset,
    } = factor
    else {
        return Ok(None);
    };
    let Some(first_left) = left_rows.first() else {
        return Ok(Some(Vec::new()));
    };
    if !left_rows
        .iter()
        .skip(1)
        .all(|r| Arc::ptr_eq(&r.schema, &first_left.schema))
    {
        return Ok(None);
    }
    let BqType::Array(elem_ty) = infer_expr_type(expr, first_left, ctx) else {
        return Ok(None);
    };
    if matches!(elem_ty.as_ref(), BqType::Struct(_) | BqType::UntypedNull) {
        return Ok(None);
    }
    let mut right_metas = vec![ColMeta {
        qualifier: alias.as_ref().map(|a| Arc::from(a.value.as_str())),
        name: Arc::from(alias.as_ref().map_or("f0_", |a| a.value.as_str())),
        ty: *elem_ty,
    }];
    let has_off = with_offset.is_some();
    if let Some(opt_alias) = with_offset {
        right_metas.push(ColMeta {
            qualifier: None,
            name: Arc::from(opt_alias.as_ref().map_or("offset", |a| a.value.as_str())),
            ty: BqType::Int64,
        });
    }
    let right_schema = RowSchema::from_cols(right_metas);
    let combined_schema = RowSchema::concat(&first_left.schema, &right_schema);
    let extra_cols = 1 + usize::from(has_off);
    let mut out = Vec::with_capacity(left_rows.len());
    for lrow in left_rows {
        let items = match eval_expr(expr, lrow, ctx)? {
            BqValue::Null => continue,
            BqValue::Array(items) => items,
            other => {
                return Err(BqError::query(format!(
                    "UNNEST expects an ARRAY argument, got {}",
                    other.infer_type()?
                )))
            }
        };
        for (idx, item) in items.into_iter().enumerate() {
            let mut vals = Vec::with_capacity(lrow.values.len() + extra_cols);
            vals.extend_from_slice(&lrow.values);
            vals.push(item);
            if has_off {
                vals.push(BqValue::Int64(i64::try_from(idx).unwrap_or(i64::MAX)));
            }
            out.push(EvalRow::from_schema_with_pseudo(
                Arc::clone(&combined_schema),
                vals,
                lrow.pseudo_values.clone(),
            ));
        }
    }
    Ok(Some(out))
}

/// Returns true if `FROM` joins after the first table factor allow error-safe filtering of
/// the first table before joining (no `RIGHT` or `FULL` outer joins).
pub(super) fn can_push_where_to_first_table(from: &[TableWithJoins]) -> bool {
    let Some(first) = from.first() else {
        return false;
    };
    if first.joins.is_empty() && from.len() == 1 {
        return false;
    }
    first
        .joins
        .iter()
        .all(|j| matches!(j.kind, JoinKind::Inner | JoinKind::Cross | JoinKind::Left))
        && from.iter().skip(1).all(|twj| {
            twj.joins
                .iter()
                .all(|j| matches!(j.kind, JoinKind::Inner | JoinKind::Cross | JoinKind::Left))
        })
}

/// Extracts top-level `AND` conjuncts that are guaranteed error-free and reference only
/// columns present on `first_schema`, returning `(pushable, remaining)`.
pub(super) fn split_pushable_where<'a>(
    pred: &'a Expr,
    first_schema: &EvalRow,
) -> (Vec<&'a Expr>, Vec<&'a Expr>) {
    let mut conjuncts = Vec::new();
    flatten_and(pred, &mut conjuncts);
    let mut pushable = Vec::new();
    let mut remaining = Vec::new();
    for c in conjuncts {
        if is_safe_single_table_pred(c, first_schema) {
            pushable.push(c);
        } else {
            remaining.push(c);
        }
    }
    (pushable, remaining)
}

fn flatten_and<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    if let Expr::Binary {
        left,
        op: BinaryOp::And,
        right,
    } = expr
    {
        flatten_and(left, out);
        flatten_and(right, out);
    } else if let Expr::Grouped(inner) = expr {
        flatten_and(inner, out);
    } else {
        out.push(expr);
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only provably error-free column/literal comparisons are eligible for pushdown"
)]
fn is_safe_single_table_pred(expr: &Expr, schema: &EvalRow) -> bool {
    match expr {
        Expr::Binary {
            left,
            op:
                BinaryOp::Eq
                | BinaryOp::Neq
                | BinaryOp::Lt
                | BinaryOp::Lte
                | BinaryOp::Gt
                | BinaryOp::Gte,
            right,
        } => {
            (is_col_path_in_schema(left, schema) && is_simple_literal(right))
                || (is_simple_literal(left) && is_col_path_in_schema(right, schema))
        }
        Expr::Is { expr: inner, .. } => is_col_path_in_schema(inner, schema),
        _ => false,
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only plain literals and unary +/- literals qualify as simple literals"
)]
fn is_simple_literal(expr: &Expr) -> bool {
    match expr {
        Expr::Literal(_) => true,
        Expr::Unary {
            op: crate::ast::UnaryOp::Minus | crate::ast::UnaryOp::Plus,
            expr: inner,
        } => matches!(inner.as_ref(), Expr::Literal(_)),
        _ => false,
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only simple identifier and qualified column paths are checked for schema membership"
)]
fn is_col_path_in_schema(expr: &Expr, schema: &EvalRow) -> bool {
    match expr {
        Expr::Ident(id) => matches!(schema.resolve_ident(&id.value), Ok(Some(_))),
        Expr::CompoundIdent(parts) => match parts.as_slice() {
            [first, second, ..] => {
                schema
                    .resolve_qualified(&first.value, &second.value)
                    .is_some()
                    || matches!(schema.resolve_ident(&first.value), Ok(Some(_)))
            }
            _ => false,
        },
        _ => false,
    }
}
