use std::sync::Arc;

use crate::ast::{Expr, ObjectName, Select, SelectItem};
use crate::error::BqResult;
use crate::executor::context::{EvalRow, ExecContext, RowBinding};
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
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut cols = Vec::with_capacity(select.projection.len());
        for (item, pre_name) in select.projection.iter().zip(&proj_names) {
            match item {
                SelectItem::Expr { expr, .. } => {
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
        out.push(EvalRow::new(cols));
    }
    Ok(out)
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
        for col in &row.pseudo_cols {
            if col.name.starts_with("_PARTITION") {
                continue;
            }
            if col
                .qualifier
                .as_deref()
                .is_some_and(|cq| cq.eq_ignore_ascii_case(qs))
            {
                matched_any = true;
                push_expanded_binding(
                    &col.name, &col.ty, &col.value, row, except, replace, ctx, out,
                )?;
            }
        }
        if matched_any {
            return Ok(());
        }
    }
    for col in &row.cols {
        if col.name.starts_with("__win_") {
            continue;
        }
        if let Some(ref qs) = qual_str {
            if !col
                .qualifier
                .as_deref()
                .is_some_and(|cq| cq.eq_ignore_ascii_case(qs))
            {
                continue;
            }
        }
        matched_any = true;
        push_expanded_binding(
            &col.name, &col.ty, &col.value, row, except, replace, ctx, out,
        )?;
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
            .cols
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(&fname))
        {
            struct_row.cols.push(RowBinding {
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
