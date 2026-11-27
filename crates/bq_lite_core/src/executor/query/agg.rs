use std::collections::BTreeMap;

use crate::ast::{Expr, GroupByClause, GroupByItem, Literal, Select, SelectItem};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext, RowBinding};
use crate::executor::evaluator::{eval_expr, infer_expr_col_name, values_distinct_eq};
use crate::types::BqValue;

type BucketList = Vec<(Vec<BqValue>, Vec<EvalRow>)>;

pub(super) struct GroupedBucket {
    pub active_keys: Vec<(Expr, BqValue)>,
    pub rolled_up_keys: Vec<Expr>,
    pub rows: Vec<EvalRow>,
}

#[derive(Default)]
pub(super) struct GroupedKeys<'a> {
    pub active: &'a [(Expr, BqValue)],
    pub rolled_up: &'a [Expr],
    pub select_aliases: Option<&'a BTreeMap<String, Expr>>,
}

impl GroupedBucket {
    pub(super) fn keys(&self) -> GroupedKeys<'_> {
        GroupedKeys {
            active: &self.active_keys,
            rolled_up: &self.rolled_up_keys,
            select_aliases: None,
        }
    }
}

use super::agg_builtins::is_aggregate_name;
use super::agg_eval::eval_grouped_expr;
use super::project::{has_untyped_null, merge_inferred_type};
use super::project_eval_rows;
use super::type_infer::infer_expr_type;

#[must_use]
pub(super) fn select_uses_grouping(select: &Select) -> bool {
    if select.group_by.is_some() || select.having.is_some() {
        return true;
    }
    select.projection.iter().any(|item| match item {
        SelectItem::Expr { expr, .. } => expr_has_aggregate(expr),
        SelectItem::Wildcard { replace, .. } => replace.iter().any(|r| expr_has_aggregate(&r.expr)),
    })
}

#[must_use]
pub(super) fn expr_has_aggregate(expr: &Expr) -> bool {
    match expr {
        Expr::Function(call) => {
            if call.over.is_some() {
                return false;
            }
            let name = call.name.normalized_parts().join(".");
            is_aggregate_name(&name) || call.args.iter().any(|a| expr_has_aggregate(&a.expr))
        }
        Expr::Binary { left, right, .. } => expr_has_aggregate(left) || expr_has_aggregate(right),
        Expr::Unary { expr: inner, .. }
        | Expr::Grouped(inner)
        | Expr::Cast { expr: inner, .. }
        | Expr::Is { expr: inner, .. }
        | Expr::FieldAccess { expr: inner, .. }
        | Expr::Extract { expr: inner, .. } => expr_has_aggregate(inner),
        Expr::Case {
            operand,
            whens,
            else_result,
        } => {
            operand.as_deref().is_some_and(expr_has_aggregate)
                || whens
                    .iter()
                    .any(|w| expr_has_aggregate(&w.condition) || expr_has_aggregate(&w.result))
                || else_result.as_deref().is_some_and(expr_has_aggregate)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => expr_has_aggregate(e) || expr_has_aggregate(low) || expr_has_aggregate(high),
        Expr::Like {
            expr: e, pattern, ..
        } => expr_has_aggregate(e) || expr_has_aggregate(pattern),
        Expr::ArrayLiteral { elements, .. } => elements.iter().any(expr_has_aggregate),
        Expr::StructLiteral { fields, .. } => fields.iter().any(|f| expr_has_aggregate(&f.expr)),
        Expr::Subscript { expr: e, index, .. } => {
            expr_has_aggregate(e) || expr_has_aggregate(index)
        }
        Expr::Literal(_)
        | Expr::Ident(_)
        | Expr::CompoundIdent(_)
        | Expr::Parameter(_)
        | Expr::PositionalParam(_)
        | Expr::SystemVar(_)
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::In { .. }
        | Expr::Interval { .. }
        | Expr::RangeLiteral { .. }
        | Expr::Lambda { .. }
        | Expr::Exists(_)
        | Expr::Subquery(_)
        | Expr::ArraySubquery(_) => false,
    }
}

pub(super) fn eval_aggregate_expr(
    expr: &Expr,
    bucket: &[EvalRow],
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    eval_grouped_expr(expr, bucket, &GroupedKeys::default(), ctx)
}

pub(super) fn execute_grouped_select(
    select: &Select,
    rows: &[EvalRow],
    schema_row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    let buckets = execute_grouped_buckets(select, rows, schema_row, ctx)?;
    let mut result_rows = Vec::with_capacity(buckets.len());
    for b in &buckets {
        result_rows.push(project_grouped_row(
            select,
            &b.rows,
            schema_row,
            &b.keys(),
            ctx,
        )?);
    }
    Ok(result_rows)
}

pub(super) fn execute_grouped_buckets(
    select: &Select,
    rows: &[EvalRow],
    schema_row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<GroupedBucket>> {
    let alias_map = build_select_alias_map(select);
    let effective_schema = rows.first().unwrap_or(schema_row);
    let grouping_sets = match &select.group_by {
        Some(gb) => expand_group_by(gb, select, &alias_map, effective_schema)?,
        None => vec![Vec::new()],
    };
    let mut all_keys: Vec<Expr> = Vec::new();
    for set in &grouping_sets {
        for k in set {
            if !all_keys.contains(k) {
                all_keys.push(k.clone());
            }
        }
    }

    let mut out = Vec::new();
    for key_exprs in grouping_sets {
        let rolled_up: Vec<Expr> = all_keys
            .iter()
            .filter(|k| !key_exprs.contains(k))
            .cloned()
            .collect();
        let buckets = bucket_rows(rows, &key_exprs, ctx)?;
        let buckets = if buckets.is_empty() && select.group_by.is_none() {
            vec![(Vec::new(), Vec::new())]
        } else {
            buckets
        };
        for (key_vals, bucket) in buckets {
            let mut active_pairs: Vec<(Expr, BqValue)> =
                key_exprs.iter().cloned().zip(key_vals).collect();
            append_alias_keys(&alias_map, &mut active_pairs);
            let mut rolled_full = rolled_up.clone();
            for (alias, target) in &alias_map {
                if rolled_up.contains(target) {
                    rolled_full.push(Expr::Ident(crate::ast::Ident::unquoted(alias.clone())));
                }
            }
            if let Some(ref having) = select.having {
                let keys = GroupedKeys {
                    active: &active_pairs,
                    rolled_up: &rolled_full,
                    select_aliases: Some(&alias_map),
                };
                let hval = eval_grouped_expr(having, &bucket, &keys, ctx)?;
                if !matches!(hval, BqValue::Bool(true)) {
                    continue;
                }
            }
            out.push(GroupedBucket {
                active_keys: active_pairs,
                rolled_up_keys: rolled_full,
                rows: bucket,
            });
        }
    }
    Ok(out)
}

fn build_select_alias_map(select: &Select) -> BTreeMap<String, Expr> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut raw: Vec<(String, Expr)> = Vec::new();
    for item in &select.projection {
        if let SelectItem::Expr { expr, alias } = item {
            let col_name = alias
                .as_ref()
                .map(|a| a.value.clone())
                .or_else(|| infer_expr_col_name(expr));
            if let Some(name) = col_name {
                let key = name.to_ascii_lowercase();
                *counts.entry(key.clone()).or_insert(0) += 1;
                raw.push((key, (**expr).clone()));
            }
        }
    }
    let mut map = BTreeMap::new();
    for (k, e) in raw {
        if counts.get(&k).copied().unwrap_or(0) == 1 {
            map.insert(k, e);
        }
    }
    map
}

fn append_alias_keys(alias_map: &BTreeMap<String, Expr>, active_pairs: &mut Vec<(Expr, BqValue)>) {
    let mut extra = Vec::new();
    for (alias, target_expr) in alias_map {
        if let Some((_, val)) = active_pairs.iter().find(|(k, _)| k == target_expr) {
            let ident_expr = Expr::Ident(crate::ast::Ident::unquoted(alias.clone()));
            if !active_pairs.iter().any(|(k, _)| k == &ident_expr) {
                extra.push((ident_expr, val.clone()));
            }
        }
    }
    active_pairs.extend(extra);
}

fn expand_group_by(
    gb: &GroupByClause,
    select: &Select,
    alias_map: &BTreeMap<String, Expr>,
    schema_row: &EvalRow,
) -> BqResult<Vec<Vec<Expr>>> {
    let proj_exprs: Vec<Option<Expr>> = select
        .projection
        .iter()
        .map(|item| match item {
            SelectItem::Expr { expr, .. } => Some((**expr).clone()),
            SelectItem::Wildcard { .. } => None,
        })
        .collect();
    match gb {
        GroupByClause::All => {
            let mut keys = Vec::new();
            for item in &select.projection {
                if let SelectItem::Expr { expr, .. } = item {
                    if !expr_has_aggregate(expr) {
                        keys.push((**expr).clone());
                    }
                }
            }
            Ok(vec![keys])
        }
        GroupByClause::Items(items) => {
            let mut acc: Vec<Vec<Expr>> = vec![Vec::new()];
            for item in items {
                let sets = expand_single_gb_item(item, &proj_exprs, alias_map, schema_row)?;
                let mut next = Vec::with_capacity(acc.len() * sets.len());
                for base in &acc {
                    for add in &sets {
                        let mut combined = base.clone();
                        combined.extend(add.iter().cloned());
                        next.push(combined);
                    }
                }
                acc = next;
            }
            Ok(acc)
        }
    }
}

fn resolve_gb_expr(
    expr: &Expr,
    proj_exprs: &[Option<Expr>],
    alias_map: &BTreeMap<String, Expr>,
    schema_row: &EvalRow,
) -> BqResult<Expr> {
    if let Expr::Literal(Literal::Int(pos)) = expr {
        let idx = usize::try_from(pos - 1)
            .map_err(|_| BqError::query(format!("GROUP BY position {pos} is out of range")))?;
        let target = proj_exprs
            .get(idx)
            .and_then(|opt| opt.as_ref())
            .ok_or_else(|| BqError::query(format!("GROUP BY position {pos} out of range")))?;
        return Ok(target.clone());
    }
    if let Expr::Ident(id) = expr {
        let unambiguous_in_from = matches!(schema_row.resolve_ident(&id.value), Ok(Some(_)));
        if !unambiguous_in_from {
            if let Some(aliased) = alias_map.get(&id.value.to_ascii_lowercase()) {
                return Ok(aliased.clone());
            }
        }
    }
    Ok(expr.clone())
}

fn expand_single_gb_item(
    item: &GroupByItem,
    proj_exprs: &[Option<Expr>],
    alias_map: &BTreeMap<String, Expr>,
    schema_row: &EvalRow,
) -> BqResult<Vec<Vec<Expr>>> {
    match item {
        GroupByItem::Expr(e) => Ok(vec![vec![resolve_gb_expr(
            e, proj_exprs, alias_map, schema_row,
        )?]]),
        GroupByItem::Rollup(exprs) => {
            let resolved: Vec<Expr> = exprs
                .iter()
                .map(|e| resolve_gb_expr(e, proj_exprs, alias_map, schema_row))
                .collect::<BqResult<_>>()?;
            let mut sets = Vec::with_capacity(resolved.len() + 1);
            for len in (0..=resolved.len()).rev() {
                sets.push(resolved.get(..len).unwrap_or(&[]).to_vec());
            }
            Ok(sets)
        }
        GroupByItem::Cube(exprs) => {
            if exprs.len() > 10 {
                return Err(BqError::query("CUBE supports at most 10 expressions"));
            }
            let resolved: Vec<Expr> = exprs
                .iter()
                .map(|e| resolve_gb_expr(e, proj_exprs, alias_map, schema_row))
                .collect::<BqResult<_>>()?;
            let count = 1_usize << resolved.len();
            let mut sets = Vec::with_capacity(count);
            for mask in 0..count {
                let mut subset = Vec::new();
                for (i, expr) in resolved.iter().enumerate() {
                    if (mask & (1_usize << i)) != 0 {
                        subset.push(expr.clone());
                    }
                }
                sets.push(subset);
            }
            Ok(sets)
        }
        GroupByItem::GroupingSets(sets) => sets
            .iter()
            .map(|s| {
                s.iter()
                    .map(|e| resolve_gb_expr(e, proj_exprs, alias_map, schema_row))
                    .collect::<BqResult<Vec<_>>>()
            })
            .collect(),
    }
}

fn bucket_rows(
    rows: &[EvalRow],
    key_exprs: &[Expr],
    ctx: &ExecContext<'_>,
) -> BqResult<BucketList> {
    let mut buckets: BucketList = Vec::new();
    for row in rows {
        let mut key_vals = Vec::with_capacity(key_exprs.len());
        for expr in key_exprs {
            let val = eval_expr(expr, row, ctx)?;
            if matches!(val, BqValue::Array(_) | BqValue::Json(_)) {
                return Err(BqError::query(
                    "Grouping by expressions of type ARRAY/JSON is not allowed",
                ));
            }
            key_vals.push(val);
        }
        let found = buckets.iter_mut().find(|(existing, _)| {
            existing.len() == key_vals.len()
                && existing
                    .iter()
                    .zip(key_vals.iter())
                    .all(|(a, b)| values_distinct_eq(a, b).unwrap_or(false))
        });
        if let Some((_, bucket)) = found {
            bucket.push(row.clone());
        } else {
            buckets.push((key_vals, vec![row.clone()]));
        }
    }
    Ok(buckets)
}

pub(super) fn project_grouped_row(
    select: &Select,
    bucket: &[EvalRow],
    schema_row: &EvalRow,
    keys: &GroupedKeys<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<EvalRow> {
    let mut cols = Vec::new();
    let mut anon_idx = 0_usize;
    let effective_row = bucket.first().unwrap_or(schema_row);
    for item in &select.projection {
        match item {
            SelectItem::Expr { expr, alias } => {
                let val = eval_grouped_expr(expr, bucket, keys, ctx)?;
                let raw_ty = val.infer_type()?;
                let ty = if has_untyped_null(&raw_ty) {
                    merge_inferred_type(raw_ty, infer_expr_type(expr, effective_row, ctx))
                } else {
                    raw_ty
                };
                let name = alias
                    .as_ref()
                    .map(|a| a.value.clone())
                    .or_else(|| infer_expr_col_name(expr))
                    .unwrap_or_else(|| {
                        let n = format!("f{anon_idx}_");
                        anon_idx += 1;
                        n
                    });
                cols.push(RowBinding {
                    qualifier: None,
                    name,
                    ty,
                    value: val,
                });
            }
            SelectItem::Wildcard { .. } => {
                let sample = bucket.first().cloned().unwrap_or_default();
                let expanded = project_eval_rows(select, &[sample], ctx)?;
                if let Some(first) = expanded.into_iter().next() {
                    return Ok(first);
                }
            }
        }
    }
    Ok(EvalRow::new(cols))
}
