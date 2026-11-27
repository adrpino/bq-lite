use std::collections::BTreeMap;

use crate::ast::{Expr, GroupByClause, Ident, OrderByExpr, Select, SelectItem};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext, RowBinding};
use crate::executor::evaluator::{eval_expr, infer_expr_col_name, values_distinct_eq};
use crate::types::BqValue;

type BucketList = Vec<(Vec<BqValue>, Vec<EvalRow>)>;
pub(super) type GroupedSelectOut = (Vec<EvalRow>, Vec<EvalRow>, Vec<OrderByExpr>);

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
use super::agg_grouping::{attach_grouped_order_by, expand_single_gb_item, expr_eq_gb};
use super::project::{has_untyped_null, merge_inferred_type};
use super::project_eval_rows;
use super::type_infer::infer_expr_type;
use super::window::expr_has_window;

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
            name.eq_ignore_ascii_case("GROUPING")
                || is_aggregate_name(&name)
                || call.args.iter().any(|a| expr_has_aggregate(&a.expr))
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
    let (proj, _, _) = execute_grouped_select_with_order(select, &[], rows, schema_row, ctx)?;
    Ok(proj)
}

pub(super) fn execute_grouped_select_with_order(
    select: &Select,
    order_by: &[OrderByExpr],
    rows: &[EvalRow],
    schema_row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<GroupedSelectOut> {
    let alias_map = build_select_alias_map(select);
    let buckets = execute_grouped_buckets(select, rows, schema_row, ctx)?;
    let mut projected = Vec::with_capacity(buckets.len());
    for b in &buckets {
        projected.push(project_grouped_row(
            select,
            &b.rows,
            schema_row,
            &b.keys(),
            ctx,
        )?);
    }
    let (mut src_rows, rewritten_order) = if order_by.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        let mut s = projected.clone();
        let ro = attach_grouped_order_by(&buckets, &mut s, order_by, &alias_map, ctx);
        (s, ro)
    };
    let _ = &mut src_rows;
    Ok((projected, src_rows, rewritten_order))
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
            if !all_keys.iter().any(|existing| expr_eq_gb(existing, k)) {
                all_keys.push(k.clone());
            }
        }
    }

    let mut out = Vec::new();
    for key_exprs in grouping_sets {
        let rolled_up: Vec<Expr> = all_keys
            .iter()
            .filter(|k| !key_exprs.iter().any(|ek| expr_eq_gb(ek, k)))
            .cloned()
            .collect();
        let buckets = bucket_rows(rows, &key_exprs, ctx)?;
        let buckets = if buckets.is_empty() && key_exprs.is_empty() {
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
                if rolled_up.iter().any(|r| expr_eq_gb(r, target)) {
                    rolled_full.push(Expr::Ident(Ident::unquoted(alias.clone())));
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

pub(super) fn build_select_alias_map(select: &Select) -> BTreeMap<String, Expr> {
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
        if let Some((_, val)) = active_pairs
            .iter()
            .find(|(k, _)| expr_eq_gb(k, target_expr))
        {
            let ident_expr = Expr::Ident(Ident::unquoted(alias.clone()));
            if !active_pairs.iter().any(|(k, _)| expr_eq_gb(k, &ident_expr)) {
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
                match item {
                    SelectItem::Expr { expr, .. } => {
                        if !expr_has_aggregate(expr) && !expr_has_window(expr) {
                            keys.push((**expr).clone());
                        }
                    }
                    SelectItem::Wildcard {
                        qualifier, except, ..
                    } => {
                        let qual_str = qualifier
                            .as_ref()
                            .and_then(|q| q.parts.last())
                            .map(|i| &i.value);
                        for col in &schema_row.cols {
                            if col.name.starts_with("__win_") {
                                continue;
                            }
                            if let Some(qs) = qual_str {
                                if !col
                                    .qualifier
                                    .as_deref()
                                    .is_some_and(|cq| cq.eq_ignore_ascii_case(qs))
                                {
                                    continue;
                                }
                            }
                            if except
                                .iter()
                                .any(|e| e.value.eq_ignore_ascii_case(&col.name))
                            {
                                continue;
                            }
                            keys.push(Expr::Ident(Ident::unquoted(col.name.as_ref())));
                        }
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

fn bucket_rows(
    rows: &[EvalRow],
    key_exprs: &[Expr],
    ctx: &ExecContext<'_>,
) -> BqResult<BucketList> {
    if key_exprs.is_empty() {
        return Ok(if rows.is_empty() {
            Vec::new()
        } else {
            vec![(Vec::new(), rows.to_vec())]
        });
    }
    let mut buckets: BucketList = Vec::new();
    let mut index: std::collections::HashMap<u64, Vec<usize>> = std::collections::HashMap::new();
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
        let h = super::exec_util::hash_value_slice(&key_vals);
        let cands = index.entry(h).or_default();
        let mut found_idx = None;
        for &b_idx in cands.iter() {
            if let Some((existing, _)) = buckets.get(b_idx) {
                if existing.len() == key_vals.len()
                    && existing
                        .iter()
                        .zip(key_vals.iter())
                        .all(|(a, b)| values_distinct_eq(a, b).unwrap_or(false))
                {
                    found_idx = Some(b_idx);
                    break;
                }
            }
        }
        if let Some(b_idx) = found_idx {
            if let Some((_, bucket)) = buckets.get_mut(b_idx) {
                bucket.push(row.clone());
            }
        } else {
            let new_idx = buckets.len();
            buckets.push((key_vals, vec![row.clone()]));
            cands.push(new_idx);
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
                    name: std::sync::Arc::from(name),
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
