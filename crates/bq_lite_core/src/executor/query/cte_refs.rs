use std::collections::BTreeSet;

use crate::ast::{Expr, PipeOp, Query, Select, SelectItem, SetExpr, TableFactor, TableWithJoins};

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

pub(super) fn collect_query_table_refs(query: &Query, out: &mut BTreeSet<String>) {
    if let Some(ref w) = query.with {
        for c in &w.ctes {
            collect_query_table_refs(&c.query, out);
        }
    }
    collect_set_expr_table_refs(&query.body, out);
    for op in &query.pipe_ops {
        if let PipeOp::SetOp { queries, .. } = op {
            for q in queries {
                collect_query_table_refs(q, out);
            }
        }
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
        SetExpr::Values(_) => {}
    }
}

fn collect_select_table_refs(sel: &Select, out: &mut BTreeSet<String>) {
    for twj in &sel.from {
        collect_twj_table_refs(twj, out);
    }
    for item in &sel.projection {
        if let SelectItem::Expr { expr, .. } = item {
            collect_expr_table_refs(expr, out);
        }
    }
    if let Some(ref w) = sel.where_clause {
        collect_expr_table_refs(w, out);
    }
    if let Some(ref h) = sel.having {
        collect_expr_table_refs(h, out);
    }
}

fn collect_twj_table_refs(twj: &TableWithJoins, out: &mut BTreeSet<String>) {
    collect_factor_table_refs(&twj.relation, out);
    for j in &twj.joins {
        collect_factor_table_refs(&j.relation, out);
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
    clippy::wildcard_enum_match_arm,
    reason = "Only subquery-bearing expression variants can contain CTE table references"
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
            if let crate::ast::InTarget::Subquery(q) = target {
                collect_query_table_refs(q, out);
            }
        }
        Expr::Binary { left, right, .. } => {
            collect_expr_table_refs(left, out);
            collect_expr_table_refs(right, out);
        }
        Expr::Unary { expr: inner, .. }
        | Expr::Grouped(inner)
        | Expr::Cast { expr: inner, .. }
        | Expr::Is { expr: inner, .. }
        | Expr::FieldAccess { expr: inner, .. } => collect_expr_table_refs(inner, out),
        Expr::Function(call) => {
            for a in &call.args {
                collect_expr_table_refs(&a.expr, out);
            }
        }
        _ => {}
    }
}
