use crate::ast::{Query, Select, SelectAsKind, SelectItem, SetExpr, TableFactor};
use crate::catalog::TableKind;
use crate::error::BqResult;
use crate::executor::context::{EvalRow, ExecContext, RowBinding};
use crate::executor::evaluator::infer_expr_col_name;
use crate::executor::metadata::{try_eval_information_schema, try_infer_tables_meta_schema};
use crate::executor::result::QueryColumn;
use crate::types::coercion::common_supertype;
use crate::types::{BqType, BqValue};

use super::from::eval_table_factor_standalone;
use super::joins::combine_rows_for_constraint;
use super::tvf::eval_table_function_schema;
use super::type_infer::infer_expr_type;

pub(in crate::executor) fn infer_query_columns(
    query: &Query,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<QueryColumn>> {
    let empty_row = EvalRow::default();
    let mut local_ctx = ctx.with_outer(&empty_row);
    local_ctx.outer_rows.clone_from(&ctx.outer_rows);
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
    let mut acc = EvalRow::default();
    for twj in &select.from {
        let part = infer_table_factor_schema(&twj.relation, None, ctx)?;
        acc.cols.extend(part.cols);
        acc.pseudo_cols.extend(part.pseudo_cols);
        for j in &twj.joins {
            let r = infer_table_factor_schema(&j.relation, Some(&acc), ctx)?;
            acc = combine_rows_for_constraint(&acc, &r, &j.constraint);
        }
    }
    Ok(infer_select_columns(select, &acc, ctx))
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
    if let Some(ref qs) = qual_str {
        for col in &schema_row.pseudo_cols {
            if !col.name.starts_with("_PARTITION")
                && col
                    .qualifier
                    .as_deref()
                    .is_some_and(|cq| cq.eq_ignore_ascii_case(qs))
            {
                matched_any = true;
                push_schema_wildcard_col(
                    (&col.name, &col.ty),
                    schema_row,
                    except,
                    replace,
                    ctx,
                    out,
                );
            }
        }
    }
    if !matched_any {
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
            if name.parts.len() >= 2 && (corr_row.is_some() || !ctx.outer_rows.is_empty()) {
                let path_expr = crate::ast::Expr::CompoundIdent(name.parts.clone());
                let empty = EvalRow::default();
                let arr_ty = infer_expr_type(&path_expr, corr_row.unwrap_or(&empty), ctx);
                if let BqType::Array(el) = arr_ty {
                    return Ok(unnest_schema_row(
                        &el,
                        alias.as_ref().map(|a| a.value.as_str()),
                        None,
                    ));
                }
            }
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
            let inner = if let BqType::Array(el) = arr_ty {
                *el
            } else {
                arr_ty
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
        TableFactor::TableFunction { name, args, alias } => {
            eval_table_function_schema(name, args, alias.as_ref().map(|a| a.value.as_str()), ctx)
        }
        TableFactor::Pivot { .. } | TableFactor::Unpivot { .. } => {
            let sample = eval_table_factor_standalone(factor, ctx)?;
            Ok(sample
                .first()
                .map_or_else(EvalRow::default, null_out_eval_row))
        }
        TableFactor::NestedJoin(twj) => {
            let mut acc = infer_table_factor_schema(&twj.relation, corr_row, ctx)?;
            for j in &twj.joins {
                let r = infer_table_factor_schema(&j.relation, Some(&acc), ctx)?;
                acc = combine_rows_for_constraint(&acc, &r, &j.constraint);
            }
            Ok(acc)
        }
    }
}

pub(super) use super::governance::unnest_schema_row;

fn infer_table_ref_schema(
    name: &crate::ast::ObjectName,
    alias: Option<&str>,
    ctx: &ExecContext<'_>,
) -> BqResult<EvalRow> {
    if let Some(meta_row) = try_infer_tables_meta_schema(name, alias, ctx.catalog)? {
        return Ok(meta_row);
    }
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
    let zero = |c: &RowBinding| RowBinding {
        qualifier: c.qualifier.clone(),
        name: c.name.clone(),
        ty: c.ty.clone(),
        value: BqValue::Null,
    };
    EvalRow::with_pseudo(
        row.cols.iter().map(zero).collect(),
        row.pseudo_cols.iter().map(zero).collect(),
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
