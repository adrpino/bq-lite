use crate::ast::{Expr, FunctionCall, NamedWindowDef, OrderByExpr, Select, SelectItem, WindowSpec};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext, RowBinding};
use crate::executor::evaluator::{eval_expr, values_distinct_eq};
use crate::types::BqValue;

use super::agg::{
    build_select_alias_map, execute_grouped_buckets, project_grouped_row, GroupedSelectOut,
};
use super::agg_eval::eval_grouped_expr;
use super::agg_grouping::attach_grouped_order_by;
use super::sort::sort_with_order_keys;
use super::window_fns::eval_window_partition;

#[must_use]
pub(super) fn select_has_windows(select: &Select) -> bool {
    select.qualify.as_ref().is_some_and(expr_has_window)
        || select.projection.iter().any(|item| match item {
            SelectItem::Expr { expr, .. } => expr_has_window(expr),
            SelectItem::Wildcard { replace, .. } => {
                replace.iter().any(|r| expr_has_window(&r.expr))
            }
        })
}

pub(super) fn expr_has_window(expr: &Expr) -> bool {
    match expr {
        Expr::Function(call) => {
            call.over.is_some() || call.args.iter().any(|a| expr_has_window(&a.expr))
        }
        Expr::Binary { left, right, .. } => expr_has_window(left) || expr_has_window(right),
        Expr::Unary { expr: inner, .. }
        | Expr::Grouped(inner)
        | Expr::Cast { expr: inner, .. }
        | Expr::Is { expr: inner, .. }
        | Expr::FieldAccess { expr: inner, .. } => expr_has_window(inner),
        Expr::Case {
            operand,
            whens,
            else_result,
        } => {
            operand.as_deref().is_some_and(expr_has_window)
                || whens
                    .iter()
                    .any(|w| expr_has_window(&w.condition) || expr_has_window(&w.result))
                || else_result.as_deref().is_some_and(expr_has_window)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => expr_has_window(e) || expr_has_window(low) || expr_has_window(high),
        Expr::Literal(_)
        | Expr::Ident(_)
        | Expr::CompoundIdent(_)
        | Expr::Parameter(_)
        | Expr::PositionalParam(_)
        | Expr::SystemVar(_)
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::Like { .. }
        | Expr::In { .. }
        | Expr::ArrayLiteral { .. }
        | Expr::ArraySubquery(_)
        | Expr::StructLiteral { .. }
        | Expr::Subscript { .. }
        | Expr::Extract { .. }
        | Expr::Interval { .. }
        | Expr::RangeLiteral { .. }
        | Expr::Lambda { .. }
        | Expr::Exists(_)
        | Expr::Subquery(_) => false,
    }
}

/// Evaluates all `OVER (...)` window calls in `select`, returning augmented rows and a rewritten `Select`
/// where window calls are replaced with synthetic `__win_i` identifiers.
pub(super) fn apply_windows(
    select: &Select,
    mut rows: Vec<EvalRow>,
    ctx: &ExecContext<'_>,
) -> BqResult<(Select, Vec<EvalRow>)> {
    let mut calls: Vec<FunctionCall> = Vec::new();
    let mut rewritten = select.clone();
    for item in &mut rewritten.projection {
        match item {
            SelectItem::Expr { expr, .. } => rewrite_expr_windows(expr, &mut calls),
            SelectItem::Wildcard { replace, .. } => {
                for r in replace {
                    rewrite_expr_windows(&mut r.expr, &mut calls);
                }
            }
        }
    }
    if let Some(ref mut qual) = rewritten.qualify {
        rewrite_expr_windows(qual, &mut calls);
    }

    for (idx, call) in calls.iter().enumerate() {
        let vals = eval_window_call(call, &rows, &select.window, ctx)?;
        let col_name = format!("__win_{idx}");
        for (row, val) in rows.iter_mut().zip(vals) {
            let ty = val.infer_type()?;
            row.cols.push(RowBinding {
                qualifier: None,
                name: col_name.clone(),
                ty,
                value: val,
            });
        }
    }
    Ok((rewritten, rows))
}

pub(super) fn execute_grouped_with_windows(
    select: &Select,
    order_by: &[OrderByExpr],
    rows: &[EvalRow],
    schema_row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<GroupedSelectOut> {
    let alias_map = build_select_alias_map(select);
    let mut buckets = execute_grouped_buckets(select, rows, schema_row, ctx)?;
    let mut calls: Vec<FunctionCall> = Vec::new();
    let mut rewritten = select.clone();
    for item in &mut rewritten.projection {
        match item {
            SelectItem::Expr { expr, .. } => rewrite_expr_windows(expr, &mut calls),
            SelectItem::Wildcard { replace, .. } => {
                for r in replace {
                    rewrite_expr_windows(&mut r.expr, &mut calls);
                }
            }
        }
    }
    if let Some(ref mut qual) = rewritten.qualify {
        rewrite_expr_windows(qual, &mut calls);
    }

    for (win_idx, call) in calls.iter().enumerate() {
        let vals = eval_grouped_window_call(call, &buckets, &select.window, ctx)?;
        let ident = Expr::Ident(crate::ast::Ident::unquoted(format!("__win_{win_idx}")));
        for (bucket, val) in buckets.iter_mut().zip(vals) {
            bucket.active_keys.push((ident.clone(), val));
        }
    }

    let mut projected = Vec::with_capacity(buckets.len());
    for bucket in &buckets {
        projected.push(project_grouped_row(
            &rewritten,
            &bucket.rows,
            schema_row,
            &bucket.keys(),
            ctx,
        )?);
    }
    let mut src_rows = projected.clone();
    let rewritten_order = if order_by.is_empty() {
        Vec::new()
    } else {
        attach_grouped_order_by(&buckets, &mut src_rows, order_by, &alias_map, ctx)
    };
    let (out_proj, out_src) = if let Some(ref qual) = rewritten.qualify {
        filter_grouped_qualify(qual, &buckets, projected, &src_rows, &alias_map, ctx)?
    } else {
        (projected, src_rows)
    };
    Ok((out_proj, out_src, rewritten_order))
}

fn filter_grouped_qualify(
    qualify: &Expr,
    buckets: &[super::agg::GroupedBucket],
    projected: Vec<EvalRow>,
    src_rows: &[EvalRow],
    alias_map: &std::collections::BTreeMap<String, Expr>,
    ctx: &ExecContext<'_>,
) -> BqResult<(Vec<EvalRow>, Vec<EvalRow>)> {
    let mut out_proj = Vec::new();
    let mut out_src = Vec::new();
    for ((bucket, proj_row), src_row) in buckets.iter().zip(projected).zip(src_rows.iter()) {
        let mut active = bucket.active_keys.clone();
        for col in &proj_row.cols {
            let ident = Expr::Ident(crate::ast::Ident::unquoted(col.name.clone()));
            if !active.iter().any(|(k, _)| k == &ident) {
                active.push((ident, col.value.clone()));
            }
        }
        let keys = super::agg::GroupedKeys {
            active: &active,
            rolled_up: &bucket.rolled_up_keys,
            select_aliases: Some(alias_map),
        };
        if matches!(
            eval_grouped_expr(qualify, &bucket.rows, &keys, ctx)?,
            BqValue::Bool(true)
        ) {
            out_proj.push(proj_row);
            out_src.push(src_row.clone());
        }
    }
    Ok((out_proj, out_src))
}

fn eval_grouped_window_call(
    call: &FunctionCall,
    buckets: &[super::agg::GroupedBucket],
    named_windows: &[NamedWindowDef],
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<BqValue>> {
    let raw_spec = call
        .over
        .as_ref()
        .ok_or_else(|| BqError::internal("missing OVER spec"))?;
    let spec = resolve_window_spec(raw_spec, named_windows)?;
    let mut syn_rows = vec![EvalRow::default(); buckets.len()];
    let mut syn_call = call.clone();
    for (i, arg) in syn_call.args.iter_mut().enumerate() {
        if matches!(arg.expr, Expr::Star) {
            continue;
        }
        let col_name = format!("__warg_{i}");
        for (b_idx, bucket) in buckets.iter().enumerate() {
            let val = eval_grouped_expr(&arg.expr, &bucket.rows, &bucket.keys(), ctx)?;
            let ty = val.infer_type()?;
            if let Some(sr) = syn_rows.get_mut(b_idx) {
                sr.cols.push(RowBinding {
                    qualifier: None,
                    name: col_name.clone(),
                    ty,
                    value: val,
                });
            }
        }
        arg.expr = Expr::Ident(crate::ast::Ident::unquoted(col_name));
    }
    let mut syn_spec = spec.clone();
    syn_spec.partition_by =
        rewrite_spec_exprs(&spec.partition_by, buckets, &mut syn_rows, "p", ctx)?;
    let ob_exprs: Vec<Expr> = spec.order_by.iter().map(|o| o.expr.clone()).collect();
    let rewritten_ob = rewrite_spec_exprs(&ob_exprs, buckets, &mut syn_rows, "o", ctx)?;
    for (ob, new_e) in syn_spec.order_by.iter_mut().zip(rewritten_ob) {
        ob.expr = new_e;
    }
    syn_call.over = Some(syn_spec);
    eval_window_call(&syn_call, &syn_rows, named_windows, ctx)
}

fn rewrite_spec_exprs(
    exprs: &[Expr],
    buckets: &[super::agg::GroupedBucket],
    syn_rows: &mut [EvalRow],
    prefix: &str,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<Expr>> {
    let mut out = Vec::with_capacity(exprs.len());
    for (i, expr) in exprs.iter().enumerate() {
        let col_name = format!("__w{prefix}_{i}");
        for (b_idx, bucket) in buckets.iter().enumerate() {
            let val = eval_grouped_expr(expr, &bucket.rows, &bucket.keys(), ctx)?;
            let ty = val.infer_type()?;
            if let Some(sr) = syn_rows.get_mut(b_idx) {
                sr.cols.push(RowBinding {
                    qualifier: None,
                    name: col_name.clone(),
                    ty,
                    value: val,
                });
            }
        }
        out.push(Expr::Ident(crate::ast::Ident::unquoted(col_name)));
    }
    Ok(out)
}

fn rewrite_expr_windows(expr: &mut Expr, calls: &mut Vec<FunctionCall>) {
    match expr {
        Expr::Function(call) => {
            if call.over.is_some() {
                let idx = calls.len();
                calls.push((**call).clone());
                *expr = Expr::Ident(crate::ast::Ident::unquoted(format!("__win_{idx}")));
            } else {
                for arg in &mut call.args {
                    rewrite_expr_windows(&mut arg.expr, calls);
                }
            }
        }
        Expr::Binary { left, right, .. } => {
            rewrite_expr_windows(left, calls);
            rewrite_expr_windows(right, calls);
        }
        Expr::Unary { expr: inner, .. }
        | Expr::Grouped(inner)
        | Expr::Cast { expr: inner, .. }
        | Expr::Is { expr: inner, .. }
        | Expr::FieldAccess { expr: inner, .. } => rewrite_expr_windows(inner, calls),
        Expr::Case {
            operand,
            whens,
            else_result,
        } => {
            if let Some(op) = operand {
                rewrite_expr_windows(op, calls);
            }
            for w in whens {
                rewrite_expr_windows(&mut w.condition, calls);
                rewrite_expr_windows(&mut w.result, calls);
            }
            if let Some(el) = else_result {
                rewrite_expr_windows(el, calls);
            }
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            rewrite_expr_windows(e, calls);
            rewrite_expr_windows(low, calls);
            rewrite_expr_windows(high, calls);
        }
        Expr::Literal(_)
        | Expr::Ident(_)
        | Expr::CompoundIdent(_)
        | Expr::Parameter(_)
        | Expr::PositionalParam(_)
        | Expr::SystemVar(_)
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::Like { .. }
        | Expr::In { .. }
        | Expr::ArrayLiteral { .. }
        | Expr::ArraySubquery(_)
        | Expr::StructLiteral { .. }
        | Expr::Subscript { .. }
        | Expr::Extract { .. }
        | Expr::Interval { .. }
        | Expr::RangeLiteral { .. }
        | Expr::Lambda { .. }
        | Expr::Exists(_)
        | Expr::Subquery(_) => {}
    }
}

fn eval_window_call(
    call: &FunctionCall,
    rows: &[EvalRow],
    named_windows: &[NamedWindowDef],
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<BqValue>> {
    let raw_spec = call
        .over
        .as_ref()
        .ok_or_else(|| BqError::internal("missing OVER spec"))?;
    let spec = resolve_window_spec(raw_spec, named_windows)?;
    let partitions = partition_row_indices(rows, &spec, ctx)?;
    let mut results = vec![BqValue::Null; rows.len()];

    for mut part in partitions {
        if !spec.order_by.is_empty() {
            let mut keyed: Vec<(usize, Vec<BqValue>)> = Vec::with_capacity(part.len());
            for &idx in &part {
                let row = rows.get(idx).ok_or_else(|| BqError::internal("row idx"))?;
                let keys = spec
                    .order_by
                    .iter()
                    .map(|ob| eval_expr(&ob.expr, row, ctx))
                    .collect::<BqResult<Vec<_>>>()?;
                keyed.push((idx, keys));
            }
            sort_with_order_keys(&mut keyed, &spec.order_by)?;
            part = keyed.into_iter().map(|(i, _)| i).collect();
        }
        eval_window_partition(call, &spec, &part, rows, ctx, &mut results)?;
    }
    Ok(results)
}

fn resolve_window_spec(spec: &WindowSpec, named: &[NamedWindowDef]) -> BqResult<WindowSpec> {
    let Some(ref wname) = spec.window_name else {
        return Ok(spec.clone());
    };
    let base = named
        .iter()
        .find(|w| w.name.value.eq_ignore_ascii_case(&wname.value))
        .ok_or_else(|| BqError::query(format!("Window '{}' is not defined", wname.value)))?;
    Ok(WindowSpec {
        window_name: None,
        partition_by: if spec.partition_by.is_empty() {
            base.spec.partition_by.clone()
        } else {
            spec.partition_by.clone()
        },
        order_by: if spec.order_by.is_empty() {
            base.spec.order_by.clone()
        } else {
            spec.order_by.clone()
        },
        frame: spec.frame.clone().or_else(|| base.spec.frame.clone()),
    })
}

fn partition_row_indices(
    rows: &[EvalRow],
    spec: &WindowSpec,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<Vec<usize>>> {
    let mut buckets: Vec<(Vec<BqValue>, Vec<usize>)> = Vec::new();
    for (idx, row) in rows.iter().enumerate() {
        let key = spec
            .partition_by
            .iter()
            .map(|e| eval_expr(e, row, ctx))
            .collect::<BqResult<Vec<_>>>()?;
        let found = buckets.iter_mut().find(|(existing, _)| {
            existing.len() == key.len()
                && existing
                    .iter()
                    .zip(key.iter())
                    .all(|(a, b)| values_distinct_eq(a, b).unwrap_or(false))
        });
        if let Some((_, list)) = found {
            list.push(idx);
        } else {
            buckets.push((key, vec![idx]));
        }
    }
    Ok(buckets.into_iter().map(|(_, list)| list).collect())
}
