use chrono::{DateTime, Utc};

use crate::ast::{TableFactor, TableWithJoins};
use crate::catalog::TableKind;
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext, RowBinding};
use crate::executor::evaluator::eval_expr;
use crate::executor::query::execute_query;
use crate::types::{BqType, BqValue};

use super::governance::{
    apply_tablesample, mask_row_columns, policy_matches_session, unpack_null_struct_unnest,
    unpack_struct_unnest,
};
use super::joins::{combine_rows, eval_join_with_schema};
use super::pivot::{apply_pivot, apply_unpivot};
use super::schema::{infer_table_factor_schema, null_out_eval_row};
use super::type_infer::infer_expr_type;

pub(in crate::executor) fn eval_from_clause(
    from: &[TableWithJoins],
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    Ok(eval_from_clause_with_schema(from, ctx)?.0)
}

pub(super) fn eval_from_clause_with_schema(
    from: &[TableWithJoins],
    ctx: &ExecContext<'_>,
) -> BqResult<(Vec<EvalRow>, EvalRow)> {
    if from.is_empty() {
        return Ok((vec![EvalRow::default()], EvalRow::default()));
    }
    let (mut rows, mut schema) = eval_table_with_joins_first(from.first(), ctx)?;
    for item in from.iter().skip(1) {
        let (next_rows, next_schema) = cross_join_table_with_joins(&rows, &schema, item, ctx)?;
        rows = next_rows;
        schema = next_schema;
    }
    Ok((rows, schema))
}

fn eval_table_with_joins_first(
    twj: Option<&TableWithJoins>,
    ctx: &ExecContext<'_>,
) -> BqResult<(Vec<EvalRow>, EvalRow)> {
    let Some(item) = twj else {
        return Ok((vec![EvalRow::default()], EvalRow::default()));
    };
    let mut rows = eval_table_factor(&item.relation, None, ctx)?;
    let mut schema = rows
        .first()
        .map(null_out_eval_row)
        .map_or_else(|| infer_table_factor_schema(&item.relation, None, ctx), Ok)?;
    for join in &item.joins {
        let (next_rows, next_schema) = eval_join_with_schema(&rows, &schema, join, ctx)?;
        rows = next_rows;
        schema = next_schema;
    }
    Ok((rows, schema))
}

fn cross_join_table_with_joins(
    left_rows: &[EvalRow],
    left_schema: &EvalRow,
    twj: &TableWithJoins,
    ctx: &ExecContext<'_>,
) -> BqResult<(Vec<EvalRow>, EvalRow)> {
    let mut rows = eval_correlated_cross_or_factor(left_rows, &twj.relation, ctx)?;
    let r_schema = infer_table_factor_schema(&twj.relation, Some(left_schema), ctx)?;
    let mut schema = combine_rows(left_schema, &r_schema);
    for join in &twj.joins {
        let (next_rows, next_schema) = eval_join_with_schema(&rows, &schema, join, ctx)?;
        rows = next_rows;
        schema = next_schema;
    }
    Ok((rows, schema))
}

pub(super) fn eval_correlated_cross_or_factor(
    left_rows: &[EvalRow],
    factor: &TableFactor,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    if matches!(factor, TableFactor::Unnest { .. }) {
        let mut out = Vec::new();
        for lrow in left_rows {
            let unnested = eval_table_factor(factor, Some(lrow), ctx)?;
            for urow in unnested {
                out.push(combine_rows(lrow, &urow));
            }
        }
        return Ok(out);
    }
    let right_rows = eval_table_factor(factor, None, ctx)?;
    let mut out = Vec::with_capacity(left_rows.len() * right_rows.len());
    for lrow in left_rows {
        for rrow in &right_rows {
            out.push(combine_rows(lrow, rrow));
        }
    }
    Ok(out)
}

pub(in crate::executor) fn eval_table_factor_standalone(
    factor: &TableFactor,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    eval_table_factor(factor, None, ctx)
}

pub(super) fn eval_table_factor(
    factor: &TableFactor,
    corr_row: Option<&EvalRow>,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    match factor {
        TableFactor::Table {
            name,
            alias,
            system_time,
            tablesample_percent,
        } => {
            let rows = eval_table_ref(
                name,
                alias.as_ref().map(|a| a.value.as_str()),
                system_time.as_ref(),
                ctx,
            )?;
            match tablesample_percent {
                Some(pct) => apply_tablesample(rows, pct, ctx),
                None => Ok(rows),
            }
        }
        TableFactor::Derived { query, alias } => {
            let qr = execute_query(query, ctx)?;
            Ok(EvalRow::from_query_result(
                alias.as_ref().map(|a| a.value.as_str()),
                &qr,
            ))
        }
        TableFactor::Unnest {
            expr,
            alias,
            with_offset,
        } => eval_unnest(expr, alias.as_ref(), with_offset.as_ref(), corr_row, ctx),
        TableFactor::Pivot {
            source,
            spec,
            alias,
        } => {
            let src_rows = eval_table_factor(source, corr_row, ctx)?;
            apply_pivot(
                src_rows,
                spec,
                alias.as_ref().map(|a| a.value.as_str()),
                ctx,
            )
        }
        TableFactor::Unpivot {
            source,
            spec,
            alias,
        } => {
            let src_rows = eval_table_factor(source, corr_row, ctx)?;
            apply_unpivot(src_rows, spec, alias.as_ref().map(|a| a.value.as_str()))
        }
        TableFactor::TableFunction { name, args, alias } => {
            eval_table_function(name, args, alias.as_ref().map(|a| a.value.as_str()), ctx)
        }
    }
}

fn eval_table_function(
    name: &crate::ast::ObjectName,
    args: &[crate::ast::FunctionArg],
    alias: Option<&str>,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    let tref = ctx.catalog.resolve_table_name(name)?;
    let ds = ctx.catalog.get_dataset(&tref.project, &tref.dataset)?;
    let Some(routine) = ds.routines.get(&tref.table) else {
        return Err(BqError::unimplemented(format!(
            "Table function '{}' is not implemented",
            name.normalized_parts().join(".")
        )));
    };
    let crate::catalog::RoutineMeta::Function(cf) = routine.as_ref() else {
        return Err(BqError::query("Routine is not a table function"));
    };
    let crate::ast::FunctionBody::Query(ref q) = cf.body else {
        return Err(BqError::query("Function is not a table function"));
    };
    let empty_outer = EvalRow::default();
    let mut sub_ctx = ctx.with_outer(&empty_outer);
    for (param, arg) in cf.params.iter().zip(args.iter()) {
        let val = eval_expr(&arg.expr, &EvalRow::default(), ctx)?;
        sub_ctx
            .vars
            .insert(param.name.value.to_ascii_lowercase(), val);
    }
    let qr = execute_query(q, &sub_ctx)?;
    Ok(EvalRow::from_query_result(alias, &qr))
}

fn eval_table_ref(
    name: &crate::ast::ObjectName,
    alias: Option<&str>,
    system_time: Option<&crate::ast::Expr>,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    if let Some(info_rows) = crate::executor::metadata::try_eval_information_schema(
        name,
        alias,
        ctx.catalog,
        ctx.storage,
        ctx.governance.job_provider.as_deref(),
    )? {
        return Ok(info_rows);
    }
    let parts = name.normalized_parts();
    if let [single] = parts.as_slice() {
        if let Some(cte_res) = ctx.ctes.get(&single.to_ascii_lowercase()) {
            return Ok(EvalRow::from_query_result(
                Some(alias.unwrap_or(single)),
                cte_res,
            ));
        }
        let temp_ref =
            crate::catalog::TableRef::new(ctx.catalog.default_project(), "__temp", single);
        if ctx.catalog.get_table(&temp_ref).is_ok() {
            return eval_concrete_table(&temp_ref, alias, system_time, ctx);
        }
    }
    let tref = ctx.catalog.resolve_table_name(name)?;
    if let Some(prefix) = tref.table.strip_suffix('*') {
        return eval_wildcard_tables(&tref, prefix, alias, ctx);
    }
    eval_concrete_table(&tref, alias, system_time, ctx)
}

fn eval_wildcard_tables(
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
        let qual = alias.unwrap_or(tbl_name);
        for row in ctx.storage.read_rows(&concrete_ref) {
            let mut cols: Vec<RowBinding> = meta
                .columns
                .iter()
                .zip(row)
                .map(|(col, val)| RowBinding {
                    qualifier: Some(qual.to_owned()),
                    name: col.name.clone(),
                    ty: col.ty.clone(),
                    value: val,
                })
                .collect();
            cols.push(RowBinding {
                qualifier: Some(qual.to_owned()),
                name: "_TABLE_SUFFIX".to_owned(),
                ty: BqType::String,
                value: BqValue::String(suffix.to_owned()),
            });
            all_rows.push(EvalRow::new(cols));
        }
    }
    Ok(all_rows)
}

fn eval_concrete_table(
    tref: &crate::catalog::TableRef,
    alias: Option<&str>,
    system_time: Option<&crate::ast::Expr>,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    let meta = ctx.catalog.get_table(tref)?;
    let qual = alias.unwrap_or(&meta.name);
    if matches!(meta.kind, TableKind::View | TableKind::MaterializedView) {
        let Some(ref vq) = meta.view_query else {
            return Err(BqError::internal("View is missing view_query"));
        };
        let qr = execute_query(vq, ctx)?;
        return Ok(EvalRow::from_query_result(Some(qual), &qr));
    }
    let rows_with_ts: Vec<(Vec<BqValue>, DateTime<Utc>)> = if meta.kind == TableKind::External {
        let (_, ext_rows) = crate::executor::ddl::load_external_rows(meta)?;
        ext_rows.into_iter().map(|r| (r, meta.created_at)).collect()
    } else if let Some(st_expr) = system_time {
        let ts = eval_system_time(st_expr, ctx)?;
        ctx.storage
            .read_rows_as_of(tref, ts)?
            .into_iter()
            .map(|r| (r, meta.created_at))
            .collect()
    } else {
        ctx.storage.read_rows_with_times(tref, meta.created_at)
    };
    let policies = ctx
        .catalog
        .get_dataset(&tref.project, &tref.dataset)
        .ok()
        .and_then(|ds| ds.policies.get(&meta.name))
        .filter(|list| !list.is_empty());

    let mut out = Vec::with_capacity(rows_with_ts.len());
    for (row, ing_ts) in rows_with_ts {
        let eval_row = build_concrete_eval_row(meta, qual, row, ing_ts, ctx);
        if let Some(pol_list) = policies {
            let mut allowed = false;
            for pol in pol_list {
                if !policy_matches_session(pol, ctx) {
                    continue;
                }
                if matches!(
                    eval_expr(&pol.filter_expr, &eval_row, ctx)?,
                    BqValue::Bool(true)
                ) {
                    allowed = true;
                    break;
                }
            }
            if !allowed {
                continue;
            }
        }
        out.push(eval_row);
    }
    Ok(out)
}

fn build_concrete_eval_row(
    meta: &crate::catalog::TableMeta,
    qual: &str,
    mut row: Vec<BqValue>,
    ing_ts: DateTime<Utc>,
    ctx: &ExecContext<'_>,
) -> EvalRow {
    mask_row_columns(&meta.columns, &mut row, ctx);
    let part_date = ing_ts.date_naive();
    let part_ts = DateTime::<Utc>::from_naive_utc_and_offset(
        part_date.and_hms_opt(0, 0, 0).unwrap_or_default(),
        Utc,
    );
    let cols = meta
        .columns
        .iter()
        .zip(row)
        .map(|(col, val)| RowBinding {
            qualifier: Some(qual.to_owned()),
            name: col.name.clone(),
            ty: col.ty.clone(),
            value: val,
        })
        .collect();
    let pseudo = if meta.partition_by.is_some() {
        vec![
            RowBinding {
                qualifier: Some(qual.to_owned()),
                name: "_PARTITIONTIME".to_owned(),
                ty: BqType::Timestamp,
                value: BqValue::Timestamp(part_ts),
            },
            RowBinding {
                qualifier: Some(qual.to_owned()),
                name: "_PARTITIONDATE".to_owned(),
                ty: BqType::Date,
                value: BqValue::Date(part_date),
            },
        ]
    } else {
        Vec::new()
    };
    EvalRow::with_pseudo(cols, pseudo)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "FOR SYSTEM_TIME AS OF expects TIMESTAMP"
)]
fn eval_system_time(expr: &crate::ast::Expr, ctx: &ExecContext<'_>) -> BqResult<DateTime<Utc>> {
    let val = eval_expr(expr, &EvalRow::default(), ctx)?;
    match val {
        BqValue::Timestamp(ts) => Ok(ts),
        _ => Err(BqError::query(
            "FOR SYSTEM_TIME AS OF requires a TIMESTAMP expression",
        )),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "UNNEST accepts Null or Array"
)]
fn eval_unnest(
    expr: &crate::ast::Expr,
    alias: Option<&crate::ast::Ident>,
    with_offset: Option<&Option<crate::ast::Ident>>,
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
    let elem_name = alias.map_or_else(|| "f0_".to_owned(), |a| a.value.clone());
    let static_elem_ty = match infer_expr_type(expr, row_ref, ctx) {
        BqType::Array(inner) => Some(*inner),
        _ => None,
    };
    let mut out = Vec::with_capacity(items.len());
    for (idx, item) in items.into_iter().enumerate() {
        let ty = item.infer_type()?;
        let (mut cols, pseudo) = match (&item, &static_elem_ty) {
            (BqValue::Struct(fields), _) => unpack_struct_unnest(fields, alias, &item, &ty),
            (BqValue::Null, Some(BqType::Struct(sf))) => unpack_null_struct_unnest(sf, alias),
            _ => (
                vec![RowBinding {
                    qualifier: alias.map(|a| a.value.clone()),
                    name: elem_name.clone(),
                    ty,
                    value: item,
                }],
                Vec::new(),
            ),
        };
        if let Some(opt_alias) = with_offset {
            let offset_name = opt_alias
                .as_ref()
                .map_or_else(|| "offset".to_owned(), |a| a.value.clone());
            let off_val = i64::try_from(idx).unwrap_or(i64::MAX);
            cols.push(RowBinding {
                qualifier: None,
                name: offset_name,
                ty: BqType::Int64,
                value: BqValue::Int64(off_val),
            });
        }
        out.push(EvalRow::with_pseudo(cols, pseudo));
    }
    Ok(out)
}
