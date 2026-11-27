use std::collections::BTreeSet;

use crate::ast::{
    Expr, GroupByClause, GroupByItem, InTarget, JoinConstraint, PipeOp, Query, Select, SelectItem,
    SetExpr, TableFactor, TableWithJoins,
};

pub(super) fn query_references_table(query: &Query, target_lower: &str) -> bool {
    let mut refs = BTreeSet::new();
    collect_query_table_refs(query, &mut refs);
    refs.contains(target_lower)
}

pub(super) fn set_expr_references_table(expr: &SetExpr, target_lower: &str) -> bool {
    let mut refs = BTreeSet::new();
    collect_set_expr_table_refs(expr, &mut refs);
    refs.contains(target_lower)
}

/// Collects all single-part table references from `query` excluding its own `WITH` definitions.
pub(super) fn collect_query_outer_refs(query: &Query, out: &mut BTreeSet<String>) {
    collect_set_expr_table_refs(&query.body, out);
    for ob in &query.order_by {
        collect_expr_table_refs(&ob.expr, out);
    }
    if let Some(ref lim) = query.limit {
        collect_expr_table_refs(lim, out);
    }
    if let Some(ref off) = query.offset {
        collect_expr_table_refs(off, out);
    }
    for op in &query.pipe_ops {
        collect_pipe_op_table_refs(op, out);
    }
}

pub(super) fn collect_query_table_refs(query: &Query, out: &mut BTreeSet<String>) {
    if let Some(ref w) = query.with {
        for c in &w.ctes {
            collect_query_table_refs(&c.query, out);
        }
    }
    collect_query_outer_refs(query, out);
}

fn collect_pipe_op_table_refs(op: &PipeOp, out: &mut BTreeSet<String>) {
    match op {
        PipeOp::Where(e) => collect_expr_table_refs(e, out),
        PipeOp::Select { items, .. }
        | PipeOp::Extend { items, .. }
        | PipeOp::Window { items, .. } => {
            for item in items {
                collect_select_item_refs(item, out);
            }
        }
        PipeOp::Set(pairs) => {
            for (_, e) in pairs {
                collect_expr_table_refs(e, out);
            }
        }
        PipeOp::Aggregate { aggs, group_by, .. } => {
            for a in aggs {
                collect_select_item_refs(&a.item, out);
            }
            for g in group_by {
                collect_group_by_refs(&g.item, out);
            }
        }
        PipeOp::OrderBy(obs) => {
            for ob in obs {
                collect_expr_table_refs(&ob.expr, out);
            }
        }
        PipeOp::Limit { limit, offset } => {
            if let Some(e) = limit {
                collect_expr_table_refs(e, out);
            }
            if let Some(e) = offset {
                collect_expr_table_refs(e, out);
            }
        }
        PipeOp::Join(j) => {
            collect_factor_table_refs(&j.relation, out);
            if let JoinConstraint::On(ref e) = j.constraint {
                collect_expr_table_refs(e, out);
            }
        }
        PipeOp::SetOp { queries, .. } => {
            for q in queries {
                collect_query_table_refs(q, out);
            }
        }
        PipeOp::With(ctes) => {
            for c in ctes {
                collect_query_table_refs(&c.query, out);
            }
        }
        PipeOp::TableSample { amount, .. } => collect_expr_table_refs(amount, out),
        PipeOp::Drop(_)
        | PipeOp::Rename(_)
        | PipeOp::As(_)
        | PipeOp::Distinct
        | PipeOp::Pivot { .. }
        | PipeOp::Unpivot { .. } => {}
    }
}

fn collect_set_expr_table_refs(expr: &SetExpr, out: &mut BTreeSet<String>) {
    match expr {
        SetExpr::Select(sel) => collect_select_table_refs(sel, out),
        SetExpr::Query(q) => collect_query_table_refs(q, out),
        SetExpr::SetOperation { left, right, .. } => {
            collect_set_expr_table_refs(left, out);
            collect_set_expr_table_refs(right, out);
        }
        SetExpr::Values(rows) => {
            for r in rows {
                for e in r {
                    collect_expr_table_refs(e, out);
                }
            }
        }
    }
}

fn collect_select_table_refs(sel: &Select, out: &mut BTreeSet<String>) {
    for twj in &sel.from {
        collect_twj_table_refs(twj, out);
    }
    for item in &sel.projection {
        collect_select_item_refs(item, out);
    }
    if let Some(ref w) = sel.where_clause {
        collect_expr_table_refs(w, out);
    }
    if let Some(GroupByClause::Items(ref items)) = sel.group_by {
        for g in items {
            collect_group_by_refs(g, out);
        }
    }
    if let Some(ref h) = sel.having {
        collect_expr_table_refs(h, out);
    }
    if let Some(ref q) = sel.qualify {
        collect_expr_table_refs(q, out);
    }
}

fn collect_select_item_refs(item: &SelectItem, out: &mut BTreeSet<String>) {
    match item {
        SelectItem::Expr { expr, .. } => collect_expr_table_refs(expr, out),
        SelectItem::Wildcard { replace, .. } => {
            for r in replace {
                collect_expr_table_refs(&r.expr, out);
            }
        }
    }
}

fn collect_group_by_refs(g: &GroupByItem, out: &mut BTreeSet<String>) {
    match g {
        GroupByItem::Expr(e) => collect_expr_table_refs(e, out),
        GroupByItem::Tuple(es) => {
            for e in es {
                collect_expr_table_refs(e, out);
            }
        }
        GroupByItem::Rollup(groups) | GroupByItem::Cube(groups) => {
            for grp in groups {
                for e in grp {
                    collect_expr_table_refs(e, out);
                }
            }
        }
        GroupByItem::GroupingSets(items) => {
            for inner in items {
                collect_group_by_refs(inner, out);
            }
        }
    }
}

fn collect_twj_table_refs(twj: &TableWithJoins, out: &mut BTreeSet<String>) {
    collect_factor_table_refs(&twj.relation, out);
    for j in &twj.joins {
        collect_factor_table_refs(&j.relation, out);
        if let JoinConstraint::On(ref e) = j.constraint {
            collect_expr_table_refs(e, out);
        }
    }
}

fn collect_factor_table_refs(factor: &TableFactor, out: &mut BTreeSet<String>) {
    match factor {
        TableFactor::Table { name, .. } => {
            let parts = name.normalized_parts();
            if let [single] = parts.as_slice() {
                out.insert(single.to_ascii_lowercase());
            }
        }
        TableFactor::Derived { query, .. } => collect_query_table_refs(query, out),
        TableFactor::NestedJoin(twj) => collect_twj_table_refs(twj, out),
        TableFactor::Pivot { source, .. }
        | TableFactor::Unpivot { source, .. }
        | TableFactor::TableSample { source, .. } => {
            collect_factor_table_refs(source, out);
        }
        TableFactor::Unnest { expr, .. } => collect_expr_table_refs(expr, out),
        TableFactor::TableFunction { args, .. } => {
            for a in args {
                collect_expr_table_refs(&a.expr, out);
            }
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Exhaustive AST walk over all Expr variants to find nested CTE table references"
)]
fn collect_expr_table_refs(expr: &Expr, out: &mut BTreeSet<String>) {
    match expr {
        Expr::Subquery(q) | Expr::ArraySubquery(q) | Expr::Exists(q) => {
            collect_query_table_refs(q, out);
        }
        Expr::In {
            expr: inner,
            target,
            ..
        } => {
            collect_expr_table_refs(inner, out);
            match target {
                InTarget::Subquery(q) => collect_query_table_refs(q, out),
                InTarget::List(es) => {
                    for e in es {
                        collect_expr_table_refs(e, out);
                    }
                }
                InTarget::Unnest(e) => collect_expr_table_refs(e, out),
            }
        }
        Expr::Binary { left, right, .. } => {
            collect_expr_table_refs(left, out);
            collect_expr_table_refs(right, out);
        }
        Expr::Between {
            expr: inner,
            low,
            high,
            ..
        } => {
            collect_expr_table_refs(inner, out);
            collect_expr_table_refs(low, out);
            collect_expr_table_refs(high, out);
        }
        Expr::Like {
            expr: inner,
            pattern,
            ..
        } => {
            collect_expr_table_refs(inner, out);
            collect_expr_table_refs(pattern, out);
        }
        Expr::Subscript {
            expr: inner, index, ..
        } => {
            collect_expr_table_refs(inner, out);
            collect_expr_table_refs(index, out);
        }
        Expr::RangeLiteral { start, end, .. } => {
            collect_expr_table_refs(start, out);
            collect_expr_table_refs(end, out);
        }
        Expr::Case {
            operand,
            whens,
            else_result,
        } => {
            if let Some(op) = operand {
                collect_expr_table_refs(op, out);
            }
            for w in whens {
                collect_expr_table_refs(&w.condition, out);
                collect_expr_table_refs(&w.result, out);
            }
            if let Some(el) = else_result {
                collect_expr_table_refs(el, out);
            }
        }
        Expr::WithExpr { bindings, body } => {
            for (_, e) in bindings {
                collect_expr_table_refs(e, out);
            }
            collect_expr_table_refs(body, out);
        }
        Expr::ArrayLiteral { elements, .. } => {
            for e in elements {
                collect_expr_table_refs(e, out);
            }
        }
        Expr::StructLiteral { fields, .. } => {
            for f in fields {
                collect_expr_table_refs(&f.expr, out);
            }
        }
        Expr::Function(call) => {
            for a in &call.args {
                collect_expr_table_refs(&a.expr, out);
            }
            for ob in &call.order_by {
                collect_expr_table_refs(&ob.expr, out);
            }
            if let Some(lim) = &call.limit {
                collect_expr_table_refs(lim, out);
            }
            if let Some((_, hav)) = &call.having {
                collect_expr_table_refs(hav, out);
            }
        }
        Expr::Unary { expr: inner, .. }
        | Expr::Grouped(inner)
        | Expr::ExprStar(inner)
        | Expr::Cast { expr: inner, .. }
        | Expr::Is { expr: inner, .. }
        | Expr::FieldAccess { expr: inner, .. }
        | Expr::Extract { expr: inner, .. }
        | Expr::Interval { value: inner, .. }
        | Expr::Lambda { body: inner, .. } => collect_expr_table_refs(inner, out),
        Expr::Literal(_)
        | Expr::Ident(_)
        | Expr::CompoundIdent(_)
        | Expr::Parameter(_)
        | Expr::PositionalParam(_)
        | Expr::SystemVar(_)
        | Expr::Star
        | Expr::QualifiedStar(_) => {}
    }
}
