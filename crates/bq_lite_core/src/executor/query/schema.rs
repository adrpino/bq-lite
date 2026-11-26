use crate::ast::{Query, Select, SelectAsKind, SelectItem, SetExpr, TableFactor};
use crate::catalog::TableKind;
use crate::error::BqResult;
use crate::executor::context::{EvalRow, ExecContext, RowBinding};
use crate::executor::evaluator::infer_expr_col_name;
use crate::executor::metadata::try_eval_information_schema;
use crate::executor::result::QueryColumn;
use crate::types::coercion::common_supertype;
use crate::types::{BqType, BqValue};

use super::from::eval_table_factor_standalone;
use super::type_infer::infer_expr_type;

pub(in crate::executor) fn infer_query_columns(
    query: &Query,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<QueryColumn>> {
    let empty_row = EvalRow::default();
    let mut local_ctx = ctx.with_outer(&empty_row);
    local_ctx.outer_row = ctx.outer_row;
    if let Some(ref with) = query.with {
        for cte in &with.ctes {
            let key = cte.name.value.to_ascii_lowercase();
            let mut cols = infer_query_columns(&cte.query, &local_ctx)?;
            if cte.columns.len() == cols.len() {
                for (c, alias) in cols.iter_mut().zip(&cte.columns) {
                    c.name.clone_from(&alias.value);
                }
            }
            local_ctx.ctes.insert(
                key,
                crate::executor::result::QueryResult::new(cols, Vec::new()),
            );
        }
    }
    infer_set_expr_columns(&query.body, &local_ctx)
}

fn infer_set_expr_columns(expr: &SetExpr, ctx: &ExecContext<'_>) -> BqResult<Vec<QueryColumn>> {
    match expr {
        SetExpr::Select(sel) => infer_select_columns_from_from(sel, ctx),
        SetExpr::Query(q) => infer_query_columns(q, ctx),
        SetExpr::SetOperation { left, right, .. } => {
            let lcols = infer_set_expr_columns(left, ctx)?;
            let rcols = infer_set_expr_columns(right, ctx)?;
            if lcols.len() != rcols.len() {
                return Ok(lcols);
            }
            lcols
                .into_iter()
                .zip(rcols)
                .map(|(lc, rc)| {
                    let ty = common_supertype(&[lc.ty.clone(), rc.ty]).unwrap_or(lc.ty);
                    Ok(QueryColumn::new(lc.name, ty))
                })
                .collect()
        }
        SetExpr::Values(rows) => Ok(rows
            .first()
            .map(|r| {
                r.iter()
                    .enumerate()
                    .map(|(i, e)| {
                        QueryColumn::new(
                            format!("f{i}_"),
                            infer_expr_type(e, &EvalRow::default(), ctx),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()),
    }
}

fn infer_select_columns_from_from(
    select: &Select,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<QueryColumn>> {
    let mut cols = Vec::new();
    for twj in &select.from {
        let mut part = infer_table_factor_schema(&twj.relation, None, ctx)?;
        cols.extend(part.cols);
        for j in &twj.joins {
            let r = infer_table_factor_schema(&j.relation, Some(&EvalRow::new(cols.clone())), ctx)?;
            match &j.constraint {
                crate::ast::JoinConstraint::Using(ucols) => {
                    for rc in r.cols {
                        if !ucols.iter().any(|u| u.value.eq_ignore_ascii_case(&rc.name)) {
                            cols.push(rc);
                        }
                    }
                }
                crate::ast::JoinConstraint::None | crate::ast::JoinConstraint::On(_) => {
                    cols.extend(r.cols);
                }
            }
        }
        part.pseudo_cols.clear();
    }
    let schema_row = EvalRow::new(cols);
    Ok(infer_select_columns(select, &schema_row, ctx))
}

pub(super) fn infer_select_columns(
    select: &Select,
    schema_row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> Vec<QueryColumn> {
    let mut cols = Vec::new();
    let mut anon_idx = 0_usize;
    for item in &select.projection {
        match item {
            SelectItem::Expr { expr, alias } => {
                let ty = infer_expr_type(expr, schema_row, ctx);
                let name = alias
                    .as_ref()
                    .map(|a| a.value.clone())
                    .or_else(|| infer_expr_col_name(expr))
                    .unwrap_or_else(|| {
                        let n = format!("f{anon_idx}_");
                        anon_idx += 1;
                        n
                    });
                cols.push(QueryColumn::new(name, ty));
            }
            SelectItem::Wildcard {
                qualifier,
                except,
                replace,
            } => {
                expand_schema_wildcard(
                    schema_row,
                    qualifier.as_ref(),
                    except,
                    replace,
                    ctx,
                    &mut cols,
                );
            }
        }
    }
    match select.as_kind {
        Some(SelectAsKind::Struct) => {
            let fields = cols
                .into_iter()
                .map(|c| crate::types::StructField::named(c.name, c.ty))
                .collect();
            vec![QueryColumn::new("f0_", BqType::Struct(fields))]
        }
        Some(SelectAsKind::Value) => cols
            .into_iter()
            .take(1)
            .map(|c| QueryColumn::new("f0_", c.ty))
            .collect(),
        None => cols,
    }
}

fn expand_schema_wildcard(
    schema_row: &EvalRow,
    qualifier: Option<&crate::ast::ObjectName>,
    except: &[crate::ast::Ident],
    replace: &[crate::ast::ReplaceItem],
    ctx: &ExecContext<'_>,
    out: &mut Vec<QueryColumn>,
) {
    let qual_str = qualifier.and_then(|q| q.normalized_parts().last().cloned());
    let mut matched_any = false;
    for col in &schema_row.cols {
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
        push_schema_wildcard_col((&col.name, &col.ty), schema_row, except, replace, ctx, out);
    }
    if !matched_any {
        if let Some(q) = qualifier {
            if let Some(BqType::Struct(fields)) = resolve_struct_type_for_name(q, schema_row) {
                for (i, sf) in fields.iter().enumerate() {
                    let fname = sf.name.clone().unwrap_or_else(|| format!("f{i}_"));
                    push_schema_wildcard_col(
                        (&fname, &sf.ty),
                        schema_row,
                        except,
                        replace,
                        ctx,
                        out,
                    );
                }
            }
        }
    }
}

fn push_schema_wildcard_col(
    (name, ty): (&str, &BqType),
    schema_row: &EvalRow,
    except: &[crate::ast::Ident],
    replace: &[crate::ast::ReplaceItem],
    ctx: &ExecContext<'_>,
    out: &mut Vec<QueryColumn>,
) {
    if except.iter().any(|e| e.value.eq_ignore_ascii_case(name)) {
        return;
    }
    let final_ty = replace
        .iter()
        .find(|r| r.alias.value.eq_ignore_ascii_case(name))
        .map_or_else(|| ty.clone(), |r| infer_expr_type(&r.expr, schema_row, ctx));
    out.push(QueryColumn::new(name, final_ty));
}

pub(super) fn resolve_struct_type_for_name(
    name: &crate::ast::ObjectName,
    row: &EvalRow,
) -> Option<BqType> {
    let parts = name.normalized_parts();
    let [first, rest @ ..] = parts.as_slice() else {
        return None;
    };
    if let Some(second) = rest.first() {
        if let Some(b) = row.resolve_qualified(first, second) {
            let mut ty = b.ty.clone();
            for f in rest.get(1..).unwrap_or(&[]) {
                ty = struct_field_ty(&ty, f)?;
            }
            return Some(ty);
        }
    }
    let mut cur_ty = row.resolve_ident(first).ok().flatten()?.ty.clone();
    for f in rest {
        cur_ty = struct_field_ty(&cur_ty, f)?;
    }
    Some(cur_ty)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only BqType::Struct exposes named subfields"
)]
pub(super) fn struct_field_ty(ty: &BqType, field: &str) -> Option<BqType> {
    match ty {
        BqType::Struct(fields) => fields
            .iter()
            .find(|sf| {
                sf.name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(field))
            })
            .map(|sf| sf.ty.clone()),
        _ => None,
    }
}

pub(super) fn infer_table_factor_schema(
    factor: &TableFactor,
    corr_row: Option<&EvalRow>,
    ctx: &ExecContext<'_>,
) -> BqResult<EvalRow> {
    match factor {
        TableFactor::Table { name, alias, .. } => {
            infer_table_ref_schema(name, alias.as_ref().map(|a| a.value.as_str()), ctx)
        }
        TableFactor::Derived { query, alias } => {
            let cols = infer_query_columns(query, ctx)?;
            Ok(null_eval_row_from_cols(
                alias.as_ref().map(|a| a.value.as_str()),
                &cols,
            ))
        }
        TableFactor::Unnest {
            expr,
            alias,
            with_offset,
        } => {
            let empty = EvalRow::default();
            let row_ref = corr_row.unwrap_or(&empty);
            let arr_ty = infer_expr_type(expr, row_ref, ctx);
            let inner = match arr_ty {
                BqType::Array(el) => *el,
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
                | BqType::Struct(_) => arr_ty,
            };
            let offset_name = with_offset
                .as_ref()
                .map(|o| o.as_ref().map_or("offset", |i| i.value.as_str()));
            Ok(unnest_schema_row(
                &inner,
                alias.as_ref().map(|a| a.value.as_str()),
                offset_name,
            ))
        }
        TableFactor::TableFunction { .. }
        | TableFactor::Pivot { .. }
        | TableFactor::Unpivot { .. } => {
            let sample = eval_table_factor_standalone(factor, ctx)?;
            Ok(sample
                .first()
                .map_or_else(EvalRow::default, null_out_eval_row))
        }
    }
}

pub(super) fn unnest_schema_row(
    elem_ty: &BqType,
    alias: Option<&str>,
    offset_name: Option<&str>,
) -> EvalRow {
    let mut cols = Vec::new();
    let mut pseudo = Vec::new();
    if let BqType::Struct(fields) = elem_ty {
        for (i, sf) in fields.iter().enumerate() {
            let fname = sf.name.clone().unwrap_or_else(|| format!("f{i}_"));
            cols.push(RowBinding {
                qualifier: alias.map(str::to_owned),
                name: fname,
                ty: sf.ty.clone(),
                value: BqValue::Null,
            });
        }
        if let Some(a) = alias {
            pseudo.push(RowBinding {
                qualifier: None,
                name: a.to_owned(),
                ty: elem_ty.clone(),
                value: BqValue::Null,
            });
        }
    } else {
        let elem_name = alias.map_or_else(|| "f0_".to_owned(), str::to_owned);
        cols.push(RowBinding {
            qualifier: alias.map(str::to_owned),
            name: elem_name,
            ty: elem_ty.clone(),
            value: BqValue::Null,
        });
    }
    if let Some(off) = offset_name {
        cols.push(RowBinding {
            qualifier: None,
            name: off.to_owned(),
            ty: BqType::Int64,
            value: BqValue::Null,
        });
    }
    EvalRow::with_pseudo(cols, pseudo)
}

fn infer_table_ref_schema(
    name: &crate::ast::ObjectName,
    alias: Option<&str>,
    ctx: &ExecContext<'_>,
) -> BqResult<EvalRow> {
    if let Some(info_rows) = try_eval_information_schema(
        name,
        alias,
        ctx.catalog,
        ctx.storage,
        ctx.governance.job_provider.as_deref(),
    )? {
        return Ok(info_rows
            .first()
            .map_or_else(EvalRow::default, null_out_eval_row));
    }
    let parts = name.normalized_parts();
    if let [single] = parts.as_slice() {
        if let Some(cte_res) = ctx.ctes.get(&single.to_ascii_lowercase()) {
            return Ok(null_eval_row_from_cols(
                Some(alias.unwrap_or(single)),
                &cte_res.columns,
            ));
        }
        let temp_ref =
            crate::catalog::TableRef::new(ctx.catalog.default_project(), "__temp", single);
        if let Ok(meta) = ctx.catalog.get_table(&temp_ref) {
            return infer_concrete_table_schema(meta, alias, ctx);
        }
    }
    let tref = ctx.catalog.resolve_table_name(name)?;
    if let Some(prefix) = tref.table.strip_suffix('*') {
        let ds = ctx.catalog.get_dataset(&tref.project, &tref.dataset)?;
        if let Some((tbl_name, meta)) = ds.tables.iter().find(|(k, _)| k.starts_with(prefix)) {
            let qual = alias.unwrap_or(tbl_name.as_str());
            let mut row = infer_concrete_table_schema(meta, Some(qual), ctx)?;
            row.cols.push(RowBinding {
                qualifier: Some(qual.to_owned()),
                name: "_TABLE_SUFFIX".to_owned(),
                ty: BqType::String,
                value: BqValue::Null,
            });
            return Ok(row);
        }
        return Ok(EvalRow::default());
    }
    let meta = ctx.catalog.get_table(&tref)?;
    infer_concrete_table_schema(meta, alias, ctx)
}

fn infer_concrete_table_schema(
    meta: &crate::catalog::TableMeta,
    alias: Option<&str>,
    ctx: &ExecContext<'_>,
) -> BqResult<EvalRow> {
    let qual = alias.unwrap_or(&meta.name);
    if matches!(meta.kind, TableKind::View | TableKind::MaterializedView) {
        if let Some(ref vq) = meta.view_query {
            let cols = infer_query_columns(vq, ctx)?;
            return Ok(null_eval_row_from_cols(Some(qual), &cols));
        }
    }
    let cols = meta
        .columns
        .iter()
        .map(|c| RowBinding {
            qualifier: Some(qual.to_owned()),
            name: c.name.clone(),
            ty: c.ty.clone(),
            value: BqValue::Null,
        })
        .collect();
    let pseudo = if meta.partition_by.is_some() {
        vec![
            RowBinding {
                qualifier: Some(qual.to_owned()),
                name: "_PARTITIONTIME".to_owned(),
                ty: BqType::Timestamp,
                value: BqValue::Null,
            },
            RowBinding {
                qualifier: Some(qual.to_owned()),
                name: "_PARTITIONDATE".to_owned(),
                ty: BqType::Date,
                value: BqValue::Null,
            },
        ]
    } else {
        Vec::new()
    };
    Ok(EvalRow::with_pseudo(cols, pseudo))
}

pub(super) fn null_out_eval_row(row: &EvalRow) -> EvalRow {
    EvalRow::with_pseudo(
        row.cols
            .iter()
            .map(|c| RowBinding {
                qualifier: c.qualifier.clone(),
                name: c.name.clone(),
                ty: c.ty.clone(),
                value: BqValue::Null,
            })
            .collect(),
        row.pseudo_cols
            .iter()
            .map(|c| RowBinding {
                qualifier: c.qualifier.clone(),
                name: c.name.clone(),
                ty: c.ty.clone(),
                value: BqValue::Null,
            })
            .collect(),
    )
}

fn null_eval_row_from_cols(qual: Option<&str>, cols: &[QueryColumn]) -> EvalRow {
    EvalRow::new(
        cols.iter()
            .map(|c| RowBinding {
                qualifier: qual.map(str::to_owned),
                name: c.name.clone(),
                ty: c.ty.clone(),
                value: BqValue::Null,
            })
            .collect(),
    )
}
