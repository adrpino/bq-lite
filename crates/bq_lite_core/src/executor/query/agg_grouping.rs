use std::collections::BTreeMap;

use crate::ast::{
    Expr, GroupByClause, GroupByItem, Ident, Literal, OrderByExpr, Select, SelectItem,
};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext, RowBinding};
use crate::types::BqType;

use super::agg::{expr_has_aggregate, GroupedBucket, GroupedKeys};
use super::agg_eval::eval_grouped_expr;
use super::window::expr_has_window;

/// Structural AST equality for GROUP BY / ROLLUP / CUBE / `GROUPING()` key matching:
/// unwraps parenthesized `Expr::Grouped` and compares identifiers/function names case-insensitively.
#[must_use]
pub(super) fn expr_eq_gb(a: &Expr, b: &Expr) -> bool {
    let a = unwrap_grouped(a);
    let b = unwrap_grouped(b);
    match (a, b) {
        (Expr::Ident(ia), Expr::Ident(ib)) => ia.value.eq_ignore_ascii_case(&ib.value),
        (Expr::CompoundIdent(pa), Expr::CompoundIdent(pb)) => {
            pa.len() == pb.len()
                && pa
                    .iter()
                    .zip(pb.iter())
                    .all(|(x, y)| x.value.eq_ignore_ascii_case(&y.value))
        }
        (
            Expr::Binary {
                left: la,
                op: oa,
                right: ra,
            },
            Expr::Binary {
                left: lb,
                op: ob,
                right: rb,
            },
        ) => oa == ob && expr_eq_gb(la, lb) && expr_eq_gb(ra, rb),
        (Expr::Unary { op: oa, expr: ea }, Expr::Unary { op: ob, expr: eb }) => {
            oa == ob && expr_eq_gb(ea, eb)
        }
        (Expr::Function(fa), Expr::Function(fb)) => {
            fa.name
                .normalized_parts()
                .join(".")
                .eq_ignore_ascii_case(&fb.name.normalized_parts().join("."))
                && fa.distinct == fb.distinct
                && fa.safe_prefix == fb.safe_prefix
                && fa.args.len() == fb.args.len()
                && fa
                    .args
                    .iter()
                    .zip(fb.args.iter())
                    .all(|(x, y)| expr_eq_gb(&x.expr, &y.expr))
        }
        (
            Expr::FieldAccess {
                expr: ea,
                field: fa,
            },
            Expr::FieldAccess {
                expr: eb,
                field: fb,
            },
        ) => fa.value.eq_ignore_ascii_case(&fb.value) && expr_eq_gb(ea, eb),
        (
            Expr::Cast {
                expr: ea,
                ty: ta,
                safe: sa,
                ..
            },
            Expr::Cast {
                expr: eb,
                ty: tb,
                safe: sb,
                ..
            },
        ) => sa == sb && ta == tb && expr_eq_gb(ea, eb),
        _ => a == b,
    }
}

#[must_use]
pub(super) fn unwrap_grouped(mut expr: &Expr) -> &Expr {
    while let Expr::Grouped(inner) = expr {
        expr = inner;
    }
    expr
}

pub(super) type ExpandedGroupBy = (Vec<Vec<Expr>>, Vec<Expr>);

pub(super) fn expand_group_by(
    gb: &GroupByClause,
    select: &Select,
    alias_map: &BTreeMap<String, Expr>,
    schema_row: &EvalRow,
) -> BqResult<ExpandedGroupBy> {
    let proj_exprs = expand_proj_exprs_for_group_by(select, schema_row);
    let mut direct_select = Vec::new();
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
            Ok((vec![keys], direct_select))
        }
        GroupByClause::Items(items) => {
            let mut acc: Vec<Vec<Expr>> = vec![Vec::new()];
            for item in items {
                let sets = expand_single_gb_item(
                    item,
                    &proj_exprs,
                    alias_map,
                    schema_row,
                    &mut direct_select,
                )?;
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
            Ok((acc, direct_select))
        }
    }
}

fn expand_single_gb_item(
    item: &GroupByItem,
    proj_exprs: &[Option<Expr>],
    alias_map: &BTreeMap<String, Expr>,
    schema_row: &EvalRow,
    direct_select: &mut Vec<Expr>,
) -> BqResult<Vec<Vec<Expr>>> {
    match item {
        GroupByItem::Expr(e) => Ok(vec![vec![resolve_gb_expr(
            e,
            proj_exprs,
            alias_map,
            schema_row,
            direct_select,
        )?]]),
        GroupByItem::Tuple(exprs) => {
            let resolved = exprs
                .iter()
                .map(|e| resolve_gb_expr(e, proj_exprs, alias_map, schema_row, direct_select))
                .collect::<BqResult<Vec<_>>>()?;
            Ok(vec![resolved])
        }
        GroupByItem::Rollup(groups) => {
            let resolved_groups =
                resolve_group_tuples(groups, proj_exprs, alias_map, schema_row, direct_select)?;
            let mut sets = Vec::with_capacity(resolved_groups.len() + 1);
            for len in (0..=resolved_groups.len()).rev() {
                let flat: Vec<Expr> = resolved_groups
                    .get(..len)
                    .unwrap_or(&[])
                    .iter()
                    .flat_map(|g| g.iter().cloned())
                    .collect();
                sets.push(flat);
            }
            Ok(sets)
        }
        GroupByItem::Cube(groups) => {
            if groups.len() > 10 {
                return Err(BqError::query("CUBE supports at most 10 expressions"));
            }
            let resolved_groups =
                resolve_group_tuples(groups, proj_exprs, alias_map, schema_row, direct_select)?;
            let count = 1_usize << resolved_groups.len();
            let mut sets = Vec::with_capacity(count);
            for mask in 0..count {
                let mut subset = Vec::new();
                for (i, group) in resolved_groups.iter().enumerate() {
                    if (mask & (1_usize << i)) != 0 {
                        subset.extend(group.iter().cloned());
                    }
                }
                sets.push(subset);
            }
            Ok(sets)
        }
        GroupByItem::GroupingSets(items) => {
            let mut out = Vec::new();
            for sub in items {
                out.extend(expand_single_gb_item(
                    sub,
                    proj_exprs,
                    alias_map,
                    schema_row,
                    direct_select,
                )?);
            }
            Ok(out)
        }
    }
}

fn resolve_group_tuples(
    groups: &[Vec<Expr>],
    proj_exprs: &[Option<Expr>],
    alias_map: &BTreeMap<String, Expr>,
    schema_row: &EvalRow,
    direct_select: &mut Vec<Expr>,
) -> BqResult<Vec<Vec<Expr>>> {
    groups
        .iter()
        .map(|g| {
            g.iter()
                .map(|e| resolve_gb_expr(e, proj_exprs, alias_map, schema_row, direct_select))
                .collect::<BqResult<Vec<_>>>()
        })
        .collect()
}

fn resolve_gb_expr(
    expr: &Expr,
    proj_exprs: &[Option<Expr>],
    alias_map: &BTreeMap<String, Expr>,
    schema_row: &EvalRow,
    direct_select: &mut Vec<Expr>,
) -> BqResult<Expr> {
    let inner = unwrap_grouped(expr);
    if let Expr::Literal(Literal::Int(pos)) = inner {
        let idx = usize::try_from(*pos - 1)
            .map_err(|_| BqError::query(format!("GROUP BY position {pos} is out of range")))?;
        let target = proj_exprs
            .get(idx)
            .and_then(|opt| opt.as_ref())
            .ok_or_else(|| BqError::query(format!("GROUP BY position {pos} out of range")))?;
        let resolved = unwrap_grouped(target).clone();
        if !direct_select.iter().any(|d| expr_eq_gb(d, &resolved)) {
            direct_select.push(resolved.clone());
        }
        return Ok(resolved);
    }
    if let Expr::Ident(id) = inner {
        let unambiguous_in_from = matches!(schema_row.resolve_ident(&id.value), Ok(Some(_)));
        if !unambiguous_in_from {
            if let Some(aliased) = alias_map.get(&id.value.to_ascii_lowercase()) {
                let resolved = unwrap_grouped(aliased).clone();
                if !direct_select.iter().any(|d| expr_eq_gb(d, &resolved)) {
                    direct_select.push(resolved.clone());
                }
                return Ok(resolved);
            }
        }
    }
    Ok(inner.clone())
}

/// Returns `true` if `expr` is a compound (non-column) expression that contains at least one
/// column reference and every column reference inside it is itself a grouping column (either
/// active or rolled-up). In `GoogleSQL`, such expressions evaluate from the grouping columns
/// rather than matching a separate expression key in `GROUPING SETS`/`ROLLUP`/`CUBE`.
pub(super) fn expr_subcols_covered_by_keys(expr: &Expr, keys: &GroupedKeys<'_>) -> bool {
    let inner = unwrap_grouped(expr);
    if matches!(inner, Expr::Ident(_) | Expr::CompoundIdent(_)) {
        return false;
    }
    if keys.direct_select.iter().any(|d| expr_eq_gb(d, inner)) {
        return false;
    }
    let mut saw_col = false;
    if check_subcols_covered(inner, keys, &mut saw_col) {
        saw_col
    } else {
        false
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only simple expression trees participate in grouping-column sub-expression matching"
)]
fn check_subcols_covered(expr: &Expr, keys: &GroupedKeys<'_>, saw_col: &mut bool) -> bool {
    match unwrap_grouped(expr) {
        e @ (Expr::Ident(_) | Expr::CompoundIdent(_)) => {
            *saw_col = true;
            keys.active.iter().any(|(k, _)| expr_eq_gb(k, e))
                || keys.rolled_up.iter().any(|k| expr_eq_gb(k, e))
        }
        Expr::Literal(_) => true,
        Expr::Binary { left, right, .. } => {
            check_subcols_covered(left, keys, saw_col)
                && check_subcols_covered(right, keys, saw_col)
        }
        Expr::Unary { expr: inner, .. }
        | Expr::Cast { expr: inner, .. }
        | Expr::Is { expr: inner, .. }
        | Expr::Extract { expr: inner, .. }
        | Expr::FieldAccess { expr: inner, .. } => check_subcols_covered(inner, keys, saw_col),
        Expr::Function(call) => {
            let name = call.name.normalized_parts().join(".");
            if name.eq_ignore_ascii_case("GROUPING")
                || super::agg_builtins::is_aggregate_name(&name)
            {
                return false;
            }
            call.args
                .iter()
                .all(|a| check_subcols_covered(&a.expr, keys, saw_col))
        }
        Expr::Case {
            operand,
            whens,
            else_result,
        } => {
            operand
                .as_deref()
                .is_none_or(|o| check_subcols_covered(o, keys, saw_col))
                && whens.iter().all(|w| {
                    check_subcols_covered(&w.condition, keys, saw_col)
                        && check_subcols_covered(&w.result, keys, saw_col)
                })
                && else_result
                    .as_deref()
                    .is_none_or(|e| check_subcols_covered(e, keys, saw_col))
        }
        _ => false,
    }
}

pub(super) fn expand_proj_exprs_for_group_by(
    select: &Select,
    schema_row: &EvalRow,
) -> Vec<Option<Expr>> {
    let mut out = Vec::new();
    for item in &select.projection {
        match item {
            SelectItem::Expr { expr, .. } => out.push(Some((**expr).clone())),
            SelectItem::Wildcard {
                qualifier, except, ..
            } => {
                if let Some(q) = qualifier {
                    if q.parts.len() == 1 {
                        if let Some(first) = q.parts.first() {
                            if let Ok(Some(binding)) = schema_row.resolve_ident(&first.value) {
                                if let BqType::Struct(fields) = &binding.ty {
                                    for f in fields {
                                        if let Some(ref fname) = f.name {
                                            if !except
                                                .iter()
                                                .any(|e| e.value.eq_ignore_ascii_case(fname))
                                            {
                                                out.push(Some(Expr::CompoundIdent(vec![
                                                    first.clone(),
                                                    Ident::unquoted(fname.clone()),
                                                ])));
                                            }
                                        }
                                    }
                                    continue;
                                }
                            }
                        }
                    }
                }
                out.push(None);
            }
        }
    }
    out
}

/// Evaluates grouped `ORDER BY` expressions against each `GroupedBucket`, storing the results
/// as synthetic `__grp_ord_{i}` columns on `src_rows` and returning rewritten `OrderByExpr`s.
pub(super) fn attach_grouped_order_by(
    buckets: &[GroupedBucket<'_>],
    src_rows: &mut [EvalRow],
    order_by: &[OrderByExpr],
    alias_map: &BTreeMap<String, Expr>,
    ctx: &ExecContext<'_>,
) -> Vec<OrderByExpr> {
    let mut rewritten = Vec::with_capacity(order_by.len());
    for (idx, ob) in order_by.iter().enumerate() {
        let is_ordinal = matches!(unwrap_grouped(&ob.expr), Expr::Literal(Literal::Int(_)));
        let is_select_alias = if let Expr::Ident(id) = unwrap_grouped(&ob.expr) {
            alias_map.contains_key(&id.value.to_ascii_lowercase())
        } else {
            false
        };
        if is_ordinal || is_select_alias || expr_has_window(&ob.expr) {
            rewritten.push(ob.clone());
            continue;
        }
        let col_name = format!("__grp_ord_{idx}");
        let mut evaluated_all = true;
        let mut vals = Vec::with_capacity(buckets.len());
        for bucket in buckets {
            let keys = GroupedKeys {
                active: &bucket.active_keys,
                rolled_up: &bucket.rolled_up_keys,
                direct_select: &bucket.direct_select_keys,
                select_aliases: Some(alias_map),
            };
            if let Ok(val) = eval_grouped_expr(&ob.expr, bucket.rows.as_slice(), &keys, ctx) {
                vals.push(val);
            } else {
                evaluated_all = false;
                break;
            }
        }
        if evaluated_all {
            let col_arc: std::sync::Arc<str> = std::sync::Arc::from(col_name.as_str());
            for (srow, val) in src_rows.iter_mut().zip(vals) {
                let ty = val.infer_type().unwrap_or(BqType::UntypedNull);
                srow.cols.push(RowBinding {
                    qualifier: None,
                    name: col_arc.clone(),
                    ty,
                    value: val,
                });
            }
            rewritten.push(OrderByExpr {
                expr: Expr::Ident(Ident::unquoted(col_name)),
                asc: ob.asc,
                nulls_first: ob.nulls_first,
            });
        } else {
            rewritten.push(ob.clone());
        }
    }
    rewritten
}
