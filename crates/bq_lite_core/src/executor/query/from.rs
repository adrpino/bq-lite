use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::ast::{TableFactor, TableWithJoins};
use crate::catalog::TableKind;
use crate::error::{BqError, BqResult};
use crate::executor::context::{ColMeta, EvalRow, ExecContext, RowRef, RowSchema};
use crate::executor::evaluator::{eval_expr, eval_expr_ref};
use crate::executor::query::execute_query;
use crate::types::{BqType, BqValue};

pub(super) use super::from_unnest::eval_unnest;
use super::from_unnest::{
    can_push_where_to_first_table, eval_wildcard_tables, split_pushable_where,
    try_eval_unnest_cross_direct,
};
use super::governance::{
    apply_tablesample, derived_query_to_eval_rows, mask_row_columns, policy_matches_session,
};
use super::joins::{combine_rows, eval_join_with_schema};
use super::pivot::{apply_pivot, apply_unpivot};
use super::schema::{infer_table_factor_schema, null_out_eval_row};
use super::tvf::eval_table_function;

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
    let (rows, schema, _) = eval_from_clause_with_filter(from, None, None, ctx)?;
    Ok((rows, schema))
}

type FromFilterOut<'a> = (Vec<EvalRow>, EvalRow, Vec<&'a crate::ast::Expr>);
type CachedPairSchema = Option<(Arc<RowSchema>, Arc<RowSchema>, Arc<RowSchema>)>;

pub(super) fn eval_from_clause_with_filter<'a>(
    from: &[TableWithJoins],
    where_clause: Option<&'a crate::ast::Expr>,
    scan_limit: Option<usize>,
    ctx: &ExecContext<'_>,
) -> BqResult<FromFilterOut<'a>> {
    if from.is_empty() {
        let rem = where_clause.into_iter().collect();
        return Ok((vec![EvalRow::default()], EvalRow::default(), rem));
    }
    if let [only] = from {
        if only.joins.is_empty() {
            if let Some((rows, schema)) =
                try_scan_single_concrete_table(&only.relation, where_clause, scan_limit, ctx)?
            {
                return Ok((rows, schema, Vec::new()));
            }
        }
    }
    let push_where = where_clause.filter(|_| can_push_where_to_first_table(from));
    let (mut rows, mut schema, remaining) =
        eval_table_with_joins_first(from.first(), push_where, where_clause, ctx)?;
    for item in from.iter().skip(1) {
        let (next_rows, next_schema) = cross_join_table_with_joins(&rows, &schema, item, ctx)?;
        rows = next_rows;
        schema = next_schema;
    }
    Ok((rows, schema, remaining))
}

fn try_scan_single_concrete_table(
    factor: &TableFactor,
    where_clause: Option<&crate::ast::Expr>,
    scan_limit: Option<usize>,
    ctx: &ExecContext<'_>,
) -> BqResult<Option<(Vec<EvalRow>, EvalRow)>> {
    let TableFactor::Table {
        name,
        alias,
        system_time: None,
        tablesample_percent: None,
    } = factor
    else {
        return Ok(None);
    };
    if where_clause.is_none() && scan_limit.is_none() {
        return Ok(None);
    }
    let parts = name.normalized_parts();
    let is_meta = parts.iter().any(|p| {
        p.eq_ignore_ascii_case("INFORMATION_SCHEMA") || p.eq_ignore_ascii_case("__TABLES__")
    });
    if is_meta || (parts.len() >= 2 && !ctx.outer_rows.is_empty()) {
        return Ok(None);
    }
    let tref = if let [single] = parts.as_slice() {
        if ctx.ctes.contains_key(&single.to_ascii_lowercase()) {
            return Ok(None);
        }
        let temp_ref =
            crate::catalog::TableRef::new(ctx.catalog.default_project(), "__temp", single);
        if ctx.catalog.get_table(&temp_ref).is_ok() {
            temp_ref
        } else {
            ctx.catalog.resolve_table_name(name)?
        }
    } else {
        ctx.catalog.resolve_table_name(name)?
    };
    if tref.table.ends_with('*') {
        return Ok(None);
    }
    let meta = ctx.catalog.get_table(&tref)?;
    if meta.kind != TableKind::Table
        || meta.partition_by.is_some()
        || ctx.governance.enforce_column_masking
    {
        return Ok(None);
    }
    let has_policies = ctx
        .catalog
        .get_dataset(&tref.project, &tref.dataset)
        .ok()
        .and_then(|ds| ds.policies.get(&meta.name))
        .is_some_and(|list| !list.is_empty());
    if has_policies {
        return Ok(None);
    }
    let qual = alias
        .as_ref()
        .map_or(meta.name.as_str(), |a| a.value.as_str());
    let schema = build_concrete_schema(meta, qual);
    let schema_row =
        EvalRow::from_schema(Arc::clone(&schema), vec![BqValue::Null; schema.cols.len()]);
    let (row_count, rows_iter) = ctx.storage.iter_rows(&tref);
    let mut out = scan_limit.map_or_else(Vec::new, |cap| Vec::with_capacity(cap.min(row_count)));
    for (row, _) in rows_iter {
        if scan_limit.is_some_and(|cap| out.len() >= cap) {
            break;
        }
        if let Some(pred) = where_clause {
            let borrowed = RowRef::Borrowed {
                schema: &schema,
                values: row.as_slice(),
                pseudo_values: &[],
            };
            if !matches!(eval_expr_ref(pred, borrowed, ctx)?, BqValue::Bool(true)) {
                continue;
            }
        }
        out.push(EvalRow::from_schema(Arc::clone(&schema), row.clone()));
    }
    Ok(Some((out, schema_row)))
}

fn eval_table_with_joins_first<'a>(
    twj: Option<&TableWithJoins>,
    push_where: Option<&'a crate::ast::Expr>,
    orig_where: Option<&'a crate::ast::Expr>,
    ctx: &ExecContext<'_>,
) -> BqResult<FromFilterOut<'a>> {
    let Some(item) = twj else {
        let rem = orig_where.into_iter().collect();
        return Ok((vec![EvalRow::default()], EvalRow::default(), rem));
    };
    let mut rows = eval_table_factor(&item.relation, None, ctx)?;
    let mut schema = rows
        .first()
        .map(null_out_eval_row)
        .map_or_else(|| infer_table_factor_schema(&item.relation, None, ctx), Ok)?;
    let remaining = if let Some(pred) = push_where {
        let (pushable, rem) = split_pushable_where(pred, &schema);
        if !pushable.is_empty() {
            let mut kept = Vec::new();
            for row in rows {
                let mut ok = true;
                for p in &pushable {
                    if !matches!(eval_expr(p, &row, ctx)?, BqValue::Bool(true)) {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    kept.push(row);
                }
            }
            rows = kept;
        }
        rem
    } else {
        orig_where.into_iter().collect()
    };
    for join in &item.joins {
        let (next_rows, next_schema) = eval_join_with_schema(&rows, &schema, join, ctx)?;
        rows = next_rows;
        schema = next_schema;
    }
    Ok((rows, schema, remaining))
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
    let is_corr_path = matches!(factor, TableFactor::Unnest { .. })
        || left_rows
            .first()
            .is_some_and(|l| is_correlated_table_array_path(factor, l));
    if is_corr_path {
        if let Some(direct) = try_eval_unnest_cross_direct(left_rows, factor, ctx)? {
            return Ok(direct);
        }
        let mut out = Vec::new();
        let mut cached_pair: CachedPairSchema = None;
        for lrow in left_rows {
            let unnested = eval_table_factor(factor, Some(lrow), ctx)?;
            for urow in unnested {
                let schema = match &cached_pair {
                    Some((ls, rs, cs))
                        if Arc::ptr_eq(ls, &lrow.schema) && Arc::ptr_eq(rs, &urow.schema) =>
                    {
                        Arc::clone(cs)
                    }
                    _ => {
                        let cs = RowSchema::concat(&lrow.schema, &urow.schema);
                        cached_pair = Some((
                            Arc::clone(&lrow.schema),
                            Arc::clone(&urow.schema),
                            Arc::clone(&cs),
                        ));
                        cs
                    }
                };
                out.push(EvalRow::combine_with_schema(lrow, &urow, schema));
            }
        }
        return Ok(out);
    }
    let right_rows = eval_table_factor(factor, None, ctx)?;
    let mut out = Vec::with_capacity(left_rows.len() * right_rows.len());
    let shared_schema = left_rows
        .first()
        .zip(right_rows.first())
        .map(|(l, r)| RowSchema::concat(&l.schema, &r.schema));
    for lrow in left_rows {
        for rrow in &right_rows {
            let schema = match &shared_schema {
                Some(cs)
                    if left_rows
                        .first()
                        .is_some_and(|l0| Arc::ptr_eq(&l0.schema, &lrow.schema))
                        && right_rows
                            .first()
                            .is_some_and(|r0| Arc::ptr_eq(&r0.schema, &rrow.schema)) =>
                {
                    Arc::clone(cs)
                }
                _ => RowSchema::concat(&lrow.schema, &rrow.schema),
            };
            out.push(EvalRow::combine_with_schema(lrow, rrow, schema));
        }
    }
    Ok(out)
}

fn is_correlated_table_array_path(factor: &TableFactor, lrow: &EvalRow) -> bool {
    let TableFactor::Table { name, .. } = factor else {
        return false;
    };
    if name.parts.len() < 2 {
        return false;
    }
    let Some(first) = name.parts.first() else {
        return false;
    };
    let has_qual = lrow.schema.cols.iter().any(|c| {
        c.qualifier
            .as_deref()
            .is_some_and(|q| q.eq_ignore_ascii_case(&first.value))
    });
    has_qual || matches!(lrow.resolve_ident(&first.value), Ok(Some(_)))
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
        } => eval_table_factor_table(
            name,
            alias.as_ref(),
            system_time.as_ref(),
            tablesample_percent.as_deref(),
            corr_row,
            ctx,
        ),
        TableFactor::Derived { query, alias } => {
            let qr = execute_query(query, ctx)?;
            Ok(derived_query_to_eval_rows(
                query,
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
            let src = eval_table_factor(source, corr_row, ctx)?;
            let schema = src
                .is_empty()
                .then(|| infer_table_factor_schema(source, corr_row, ctx).ok())
                .flatten();
            apply_pivot(
                src,
                schema.as_ref(),
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
            let src = eval_table_factor(source, corr_row, ctx)?;
            apply_unpivot(src, spec, alias.as_ref().map(|a| a.value.as_str()))
        }
        TableFactor::TableSample {
            source,
            amount,
            is_percent,
            ..
        } => {
            let src = eval_table_factor(source, corr_row, ctx)?;
            super::pivot::apply_tablesample(src, amount, *is_percent, ctx)
        }
        TableFactor::TableFunction { name, args, alias } => {
            eval_table_function(name, args, alias.as_ref().map(|a| a.value.as_str()), ctx)
        }
        TableFactor::NestedJoin(twj) => {
            Ok(eval_table_with_joins_first(Some(twj), None, None, ctx)?.0)
        }
    }
}

fn eval_table_factor_table(
    name: &crate::ast::ObjectName,
    alias: Option<&crate::ast::Ident>,
    system_time: Option<&crate::ast::Expr>,
    tablesample_percent: Option<&crate::ast::Expr>,
    corr_row: Option<&EvalRow>,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    if name.parts.len() >= 2 && (corr_row.is_some() || !ctx.outer_rows.is_empty()) {
        let path_expr = crate::ast::Expr::CompoundIdent(name.parts.clone());
        let empty = EvalRow::default();
        if let Ok(BqValue::Array(_) | BqValue::Null) =
            eval_expr(&path_expr, corr_row.unwrap_or(&empty), ctx)
        {
            return eval_unnest(&path_expr, alias, None, corr_row, ctx);
        }
    }
    let rows = eval_table_ref(name, alias.map(|a| a.value.as_str()), system_time, ctx)?;
    match tablesample_percent {
        Some(pct) => apply_tablesample(rows, pct, ctx),
        None => Ok(rows),
    }
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
    if let Some(rows) = super::joins::try_eval_correlated_array_ref(name, alias, ctx)? {
        return Ok(rows);
    }
    let tref = ctx.catalog.resolve_table_name(name)?;
    if let Some(prefix) = tref.table.strip_suffix('*') {
        return eval_wildcard_tables(&tref, prefix, alias, ctx);
    }
    eval_concrete_table(&tref, alias, system_time, ctx)
}

fn build_concrete_schema(meta: &crate::catalog::TableMeta, qual: &str) -> Arc<RowSchema> {
    let qual_arc: Option<Arc<str>> = Some(Arc::from(qual));
    let cols: Vec<ColMeta> = meta
        .columns
        .iter()
        .map(|c| ColMeta {
            qualifier: qual_arc.clone(),
            name: Arc::from(c.name.as_str()),
            ty: c.ty.clone(),
        })
        .collect();
    let pseudo_cols = if meta.partition_by.is_some() {
        vec![
            ColMeta {
                qualifier: qual_arc.clone(),
                name: Arc::from("_PARTITIONTIME"),
                ty: BqType::Timestamp,
            },
            ColMeta {
                qualifier: qual_arc,
                name: Arc::from("_PARTITIONDATE"),
                ty: BqType::Date,
            },
        ]
    } else {
        Vec::new()
    };
    Arc::new(RowSchema::new(cols, pseudo_cols))
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
    let policies = ctx
        .catalog
        .get_dataset(&tref.project, &tref.dataset)
        .ok()
        .and_then(|ds| ds.policies.get(&meta.name))
        .filter(|list| !list.is_empty());
    let schema = build_concrete_schema(meta, qual);

    if meta.kind == TableKind::Table && system_time.is_none() {
        let (row_count, rows_iter) = ctx.storage.iter_rows(tref);
        let mut out = Vec::with_capacity(row_count);
        for (row, ing_ts) in rows_iter {
            let eval_row =
                build_concrete_eval_row(meta, Arc::clone(&schema), row.clone(), ing_ts, ctx);
            if check_row_policies(&eval_row, policies, ctx)? {
                out.push(eval_row);
            }
        }
        return Ok(out);
    }

    let owned_rows: Vec<Vec<BqValue>> = if meta.kind == TableKind::External {
        crate::executor::ddl::load_external_rows(meta)?.1
    } else if let Some(st_expr) = system_time {
        let ts = super::joins::eval_system_time(st_expr, ctx)?;
        ctx.storage.read_rows_as_of(tref, ts)?
    } else {
        ctx.storage.read_rows(tref)
    };
    let mut out = Vec::with_capacity(owned_rows.len());
    for row in owned_rows {
        let eval_row =
            build_concrete_eval_row(meta, Arc::clone(&schema), row, meta.created_at, ctx);
        if check_row_policies(&eval_row, policies, ctx)? {
            out.push(eval_row);
        }
    }
    Ok(out)
}

fn check_row_policies(
    eval_row: &EvalRow,
    policies: Option<&Vec<crate::catalog::RowAccessPolicyMeta>>,
    ctx: &ExecContext<'_>,
) -> BqResult<bool> {
    let Some(pol_list) = policies else {
        return Ok(true);
    };
    for pol in pol_list {
        if !policy_matches_session(pol, ctx) {
            continue;
        }
        if matches!(
            eval_expr(&pol.filter_expr, eval_row, ctx)?,
            BqValue::Bool(true)
        ) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn build_concrete_eval_row(
    meta: &crate::catalog::TableMeta,
    schema: Arc<RowSchema>,
    mut row: Vec<BqValue>,
    ing_ts: DateTime<Utc>,
    ctx: &ExecContext<'_>,
) -> EvalRow {
    if ctx.governance.enforce_column_masking {
        mask_row_columns(&meta.columns, &mut row, ctx);
    }
    let pseudo_values = if meta.partition_by.is_some() {
        let part_date = ing_ts.date_naive();
        let part_ts = DateTime::<Utc>::from_naive_utc_and_offset(
            part_date.and_hms_opt(0, 0, 0).unwrap_or_default(),
            Utc,
        );
        vec![BqValue::Timestamp(part_ts), BqValue::Date(part_date)]
    } else {
        Vec::new()
    };
    EvalRow::from_schema_with_pseudo(schema, row, pseudo_values)
}
