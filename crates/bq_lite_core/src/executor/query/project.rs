use std::sync::Arc;

use crate::ast::{Expr, ObjectName, Select, SelectItem};
use crate::error::{BqError, BqResult};
use crate::executor::context::{ColMeta, EvalRow, ExecContext, RowBinding, RowSchema};
use crate::executor::evaluator::{eval_expr, infer_expr_col_name};
use crate::types::{BqType, BqValue};

use super::schema::resolve_struct_type_for_name;
use super::type_infer::infer_expr_type;

pub(in crate::executor) fn project_eval_rows(
    select: &Select,
    rows: &[EvalRow],
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    let mut anon_idx = 0_usize;
    let proj_names: Vec<Option<Arc<str>>> = select
        .projection
        .iter()
        .map(|item| match item {
            SelectItem::Expr { expr, alias } => {
                let s = alias
                    .as_ref()
                    .map(|a| a.value.clone())
                    .or_else(|| infer_expr_col_name(expr))
                    .unwrap_or_else(|| {
                        let n = format!("f{anon_idx}_");
                        anon_idx += 1;
                        n
                    });
                Some(Arc::from(s))
            }
            SelectItem::Wildcard { .. } => None,
        })
        .collect();
    let has_wildcard_or_star = select.projection.iter().any(|i| match i {
        SelectItem::Wildcard { .. } => true,
        SelectItem::Expr { expr, .. } => matches!(expr.as_ref(), Expr::ExprStar(_)),
    });
    if !has_wildcard_or_star {
        return project_simple_rows(select, &proj_names, rows, ctx);
    }
    project_wildcard_rows(select, &proj_names, rows, ctx)
}

fn project_wildcard_rows(
    select: &Select,
    proj_names: &[Option<Arc<str>>],
    rows: &[EvalRow],
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    let mut out = Vec::with_capacity(rows.len());
    let mut cached_schema: Option<Arc<RowSchema>> = None;
    for row in rows {
        let cap = select.projection.len().max(row.col_len());
        let mut cols = Vec::with_capacity(cap);
        for (item, pre_name) in select.projection.iter().zip(proj_names) {
            match item {
                SelectItem::Expr { expr, .. } => {
                    if let Expr::ExprStar(inner) = expr.as_ref() {
                        let val = eval_expr(inner, row, ctx)?;
                        let mut anon_idx = cols.len();
                        expand_struct_star_value(val, &mut cols, &mut anon_idx)?;
                        continue;
                    }
                    let val = eval_expr(expr, row, ctx)?;
                    let raw_ty = val.infer_type()?;
                    let ty = if has_untyped_null(&raw_ty) {
                        merge_inferred_type(raw_ty, infer_expr_type(expr, row, ctx))
                    } else {
                        raw_ty
                    };
                    let name = pre_name.clone().unwrap_or_else(|| Arc::from("f0_"));
                    cols.push(RowBinding {
                        qualifier: None,
                        name,
                        ty,
                        value: val,
                    });
                }
                SelectItem::Wildcard {
                    qualifier,
                    except,
                    replace,
                } => {
                    expand_wildcard(row, qualifier.as_ref(), except, replace, ctx, &mut cols)?;
                }
            }
        }
        let matches_cached =
            cached_schema.as_ref().is_some_and(|cs| {
                cs.cols.len() == cols.len()
                    && cs.cols.iter().zip(&cols).all(|(m, b)| {
                        m.name == b.name && m.ty == b.ty && m.qualifier == b.qualifier
                    })
            });
        if matches_cached {
            if let Some(ref cs) = cached_schema {
                let vals = cols.into_iter().map(|b| b.value).collect();
                out.push(EvalRow::from_schema(Arc::clone(cs), vals));
                continue;
            }
        }
        let eval_row = EvalRow::new(cols);
        if cached_schema.is_none() {
            cached_schema = Some(Arc::clone(&eval_row.schema));
        }
        out.push(eval_row);
    }
    Ok(out)
}

fn project_simple_rows(
    select: &Select,
    proj_names: &[Option<Arc<str>>],
    rows: &[EvalRow],
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    let mut out = Vec::with_capacity(rows.len());
    let mut base_schema: Option<Arc<RowSchema>> = None;
    let ncols = select.projection.len();
    for row in rows {
        let mut vals = Vec::with_capacity(ncols);
        for item in &select.projection {
            if let SelectItem::Expr { expr, .. } = item {
                vals.push(eval_expr(expr, row, ctx)?);
            }
        }
        if let Some(ref schema) = base_schema {
            if schema
                .cols
                .iter()
                .zip(&vals)
                .all(|(meta, val)| value_matches_primitive_type(val, &meta.ty))
            {
                out.push(EvalRow::from_schema(Arc::clone(schema), vals));
                continue;
            }
        }
        let mut metas = Vec::with_capacity(ncols);
        for ((item, pre_name), val) in select.projection.iter().zip(proj_names).zip(&vals) {
            if let SelectItem::Expr { expr, .. } = item {
                let raw_ty = val.infer_type()?;
                let ty = if has_untyped_null(&raw_ty) {
                    merge_inferred_type(raw_ty, infer_expr_type(expr, row, ctx))
                } else {
                    raw_ty
                };
                let name = pre_name.clone().unwrap_or_else(|| Arc::from("f0_"));
                metas.push(ColMeta {
                    qualifier: None,
                    name,
                    ty,
                });
            }
        }
        let schema = RowSchema::from_cols(metas);
        if base_schema.is_none() {
            base_schema = Some(Arc::clone(&schema));
        }
        out.push(EvalRow::from_schema(schema, vals));
    }
    Ok(out)
}

const fn value_matches_primitive_type(val: &BqValue, ty: &BqType) -> bool {
    matches!(
        (val, ty),
        (BqValue::Null, t) if !matches!(t, BqType::UntypedNull)
    ) || matches!(
        (val, ty),
        (BqValue::Bool(_), BqType::Bool)
            | (BqValue::Int64(_), BqType::Int64)
            | (BqValue::Float64(_), BqType::Float64)
            | (BqValue::Numeric(_), BqType::Numeric)
            | (BqValue::BigNumeric(_), BqType::BigNumeric)
            | (BqValue::String(_), BqType::String)
            | (BqValue::Bytes(_), BqType::Bytes)
            | (BqValue::Date(_), BqType::Date)
            | (BqValue::Datetime(_), BqType::Datetime)
            | (BqValue::Time(_), BqType::Time)
            | (BqValue::Timestamp(_), BqType::Timestamp)
            | (BqValue::Interval(_), BqType::Interval)
            | (BqValue::Json(_), BqType::Json)
            | (BqValue::Geography(_), BqType::Geography)
    )
}

pub(super) fn expand_struct_star_value(
    val: BqValue,
    cols: &mut Vec<RowBinding>,
    anon_idx: &mut usize,
) -> BqResult<()> {
    let BqValue::Struct(fields) = val else {
        return Err(BqError::query("Expression.* requires a STRUCT value"));
    };
    for field in fields {
        let ty = field.value.infer_type()?;
        let name = field.name.unwrap_or_else(|| {
            let n = format!("f{}_", *anon_idx);
            *anon_idx += 1;
            n
        });
        cols.push(RowBinding {
            qualifier: None,
            name: Arc::from(name),
            ty,
            value: field.value,
        });
    }
    Ok(())
}

pub(super) fn has_untyped_null(ty: &BqType) -> bool {
    match ty {
        BqType::UntypedNull => true,
        BqType::Array(inner) => has_untyped_null(inner),
        BqType::Struct(fields) => fields.iter().any(|f| has_untyped_null(&f.ty)),
        BqType::Bool
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
        | BqType::Range(_) => false,
    }
}

pub(super) fn merge_inferred_type(raw: BqType, inferred: BqType) -> BqType {
    if matches!(raw, BqType::UntypedNull) {
        return inferred;
    }
    if let (BqType::Array(r_el), BqType::Array(i_el)) = (&raw, &inferred) {
        return BqType::array(merge_inferred_type(
            r_el.as_ref().clone(),
            i_el.as_ref().clone(),
        ))
        .unwrap_or(raw);
    }
    if let (BqType::Struct(r_fields), BqType::Struct(i_fields)) = (&raw, &inferred) {
        if r_fields.len() == i_fields.len() {
            let merged = r_fields
                .iter()
                .zip(i_fields.iter())
                .map(|(rf, inf)| crate::types::StructField {
                    name: rf.name.clone().or_else(|| inf.name.clone()),
                    ty: merge_inferred_type(rf.ty.clone(), inf.ty.clone()),
                })
                .collect();
            return BqType::Struct(merged);
        }
    }
    raw
}

fn expand_wildcard(
    row: &EvalRow,
    qualifier: Option<&ObjectName>,
    except: &[crate::ast::Ident],
    replace: &[crate::ast::ReplaceItem],
    ctx: &ExecContext<'_>,
    out: &mut Vec<RowBinding>,
) -> BqResult<()> {
    let qual_str = qualifier.and_then(|q| q.normalized_parts().last().cloned());
    let mut matched_any = false;
    if let Some(ref qs) = qual_str {
        for col in row.iter_pseudo() {
            if col.name.starts_with("_PARTITION") {
                continue;
            }
            if col.qualifier.is_some_and(|cq| cq.eq_ignore_ascii_case(qs)) {
                matched_any = true;
                push_expanded_binding(col.name, col.ty, col.value, row, except, replace, ctx, out)?;
            }
        }
        if matched_any {
            return Ok(());
        }
    }
    for col in row.iter_cols() {
        if col.name.starts_with("__win_") {
            continue;
        }
        if let Some(ref qs) = qual_str {
            if !col.qualifier.is_some_and(|cq| cq.eq_ignore_ascii_case(qs)) {
                continue;
            }
        }
        matched_any = true;
        push_expanded_binding(col.name, col.ty, col.value, row, except, replace, ctx, out)?;
    }
    if !matched_any {
        if let Some(q) = qualifier {
            expand_struct_wildcard(row, q, except, replace, ctx, out)?;
        }
    }
    Ok(())
}

fn expand_struct_wildcard(
    row: &EvalRow,
    qualifier: &ObjectName,
    except: &[crate::ast::Ident],
    replace: &[crate::ast::ReplaceItem],
    ctx: &ExecContext<'_>,
    out: &mut Vec<RowBinding>,
) -> BqResult<()> {
    let parts = qualifier.normalized_parts();
    let struct_expr = match parts.as_slice() {
        [single] => Expr::Ident(crate::ast::Ident::unquoted(single.clone())),
        many if !many.is_empty() => Expr::CompoundIdent(
            many.iter()
                .map(|p| crate::ast::Ident::unquoted(p.clone()))
                .collect(),
        ),
        _ => return Ok(()),
    };
    let Ok(val) = eval_expr(&struct_expr, row, ctx) else {
        return Ok(());
    };
    match val {
        BqValue::Struct(fields) => {
            let struct_row = build_struct_eval_row(row, &fields)?;
            for (i, sf) in fields.iter().enumerate() {
                let fname = sf.name.clone().unwrap_or_else(|| format!("f{i}_"));
                let fty = sf.value.infer_type()?;
                push_expanded_binding(
                    &fname,
                    &fty,
                    &sf.value,
                    &struct_row,
                    except,
                    replace,
                    ctx,
                    out,
                )?;
            }
        }
        BqValue::Null => {
            if let Some(BqType::Struct(fields)) = resolve_struct_type_for_name(qualifier, row) {
                for (i, sf) in fields.iter().enumerate() {
                    let fname = sf.name.clone().unwrap_or_else(|| format!("f{i}_"));
                    push_expanded_binding(
                        &fname,
                        &sf.ty,
                        &BqValue::Null,
                        row,
                        except,
                        replace,
                        ctx,
                        out,
                    )?;
                }
            }
        }
        BqValue::Bool(_)
        | BqValue::Int64(_)
        | BqValue::Float64(_)
        | BqValue::Numeric(_)
        | BqValue::BigNumeric(_)
        | BqValue::String(_)
        | BqValue::Bytes(_)
        | BqValue::Date(_)
        | BqValue::Datetime(_)
        | BqValue::Time(_)
        | BqValue::Timestamp(_)
        | BqValue::Interval(_)
        | BqValue::Json(_)
        | BqValue::Geography(_)
        | BqValue::Range(_)
        | BqValue::Array(_) => {}
    }
    Ok(())
}

fn build_struct_eval_row(
    row: &EvalRow,
    fields: &[crate::types::BqStructFieldValue],
) -> BqResult<EvalRow> {
    let mut struct_row = row.clone();
    for (i, sf) in fields.iter().enumerate() {
        let fname = sf.name.clone().unwrap_or_else(|| format!("f{i}_"));
        let fty = sf.value.infer_type()?;
        if !struct_row
            .schema
            .cols
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(&fname))
        {
            struct_row.push_binding(RowBinding {
                qualifier: None,
                name: Arc::from(fname),
                ty: fty,
                value: sf.value.clone(),
            });
        }
    }
    Ok(struct_row)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Wildcard expansion binds column metadata, exclusions, replacements, and output vector"
)]
fn push_expanded_binding(
    name: &str,
    ty: &BqType,
    val: &BqValue,
    row: &EvalRow,
    except: &[crate::ast::Ident],
    replace: &[crate::ast::ReplaceItem],
    ctx: &ExecContext<'_>,
    out: &mut Vec<RowBinding>,
) -> BqResult<()> {
    if except.iter().any(|e| e.value.eq_ignore_ascii_case(name)) {
        return Ok(());
    }
    if let Some(rep) = replace
        .iter()
        .find(|r| r.alias.value.eq_ignore_ascii_case(name))
    {
        let rval = eval_expr(&rep.expr, row, ctx)?;
        let rty = rval.infer_type()?;
        out.push(RowBinding {
            qualifier: None,
            name: Arc::from(name),
            ty: rty,
            value: rval,
        });
    } else {
        out.push(RowBinding {
            qualifier: None,
            name: Arc::from(name),
            ty: ty.clone(),
            value: val.clone(),
        });
    }
    Ok(())
}
