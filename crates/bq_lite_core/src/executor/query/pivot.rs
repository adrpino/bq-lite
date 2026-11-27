use std::collections::BTreeSet;

use crate::ast::{Expr, PivotSpec, UnpivotSpec};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext, RowBinding};
use crate::executor::evaluator::{eval_expr, values_distinct_eq};
use crate::types::{BqType, BqValue};

use super::agg::eval_aggregate_expr;

struct PivotRowParams<'a> {
    group_cols: &'a [String],
    pivot_vals: &'a [(BqValue, String, bool)],
    spec: &'a PivotSpec,
    alias: Option<&'a str>,
}

pub(super) fn apply_pivot(
    rows: Vec<EvalRow>,
    schema_row: Option<&EvalRow>,
    spec: &PivotSpec,
    alias: Option<&str>,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    let pivot_vals = eval_pivot_values(spec, ctx)?;
    let group_cols = collect_pivot_group_cols(rows.first().or(schema_row), spec);
    let mut buckets = bucket_pivot_rows(rows, &group_cols);
    if buckets.is_empty() && group_cols.is_empty() && schema_row.is_some() {
        buckets.push((Vec::new(), Vec::new()));
    }
    let params = PivotRowParams {
        group_cols: &group_cols,
        pivot_vals: &pivot_vals,
        spec,
        alias,
    };

    let mut out = Vec::with_capacity(buckets.len());
    for (key, bucket) in buckets {
        out.push(build_pivot_row(&params, key, &bucket, ctx)?);
    }
    Ok(out)
}

fn eval_pivot_values(
    spec: &PivotSpec,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<(BqValue, String, bool)>> {
    let mut out = Vec::with_capacity(spec.in_values.len());
    for (expr, col_alias) in &spec.in_values {
        let val = eval_expr(expr, &EvalRow::default(), ctx)?;
        let has_alias = col_alias.is_some();
        let label = col_alias
            .as_ref()
            .map_or_else(|| val.to_string(), |a| a.value.clone());
        out.push((val, label, has_alias));
    }
    Ok(out)
}

fn collect_pivot_group_cols(sample: Option<&EvalRow>, spec: &PivotSpec) -> Vec<String> {
    sample
        .map(|r| {
            r.cols
                .iter()
                .filter(|c| {
                    !expr_mentions(&spec.for_expr, &c.name)
                        && !agg_references_col(&spec.aggregates, &c.name)
                })
                .map(|c| c.name.as_ref().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn bucket_pivot_rows(
    rows: Vec<EvalRow>,
    group_cols: &[String],
) -> Vec<(Vec<BqValue>, Vec<EvalRow>)> {
    let mut buckets: Vec<(Vec<BqValue>, Vec<EvalRow>)> = Vec::new();
    for row in rows {
        let key: Vec<BqValue> = group_cols
            .iter()
            .map(|c| {
                row.resolve_ident(c)
                    .ok()
                    .flatten()
                    .map_or(BqValue::Null, |b| b.value.clone())
            })
            .collect();
        if let Some((_, bucket)) = buckets.iter_mut().find(|(k, _)| k == &key) {
            bucket.push(row);
        } else {
            buckets.push((key, vec![row]));
        }
    }
    buckets
}

fn build_pivot_row(
    params: &PivotRowParams<'_>,
    key: Vec<BqValue>,
    bucket: &[EvalRow],
    ctx: &ExecContext<'_>,
) -> BqResult<EvalRow> {
    let qual_arc: Option<std::sync::Arc<str>> = params.alias.map(std::sync::Arc::from);
    let mut bindings: Vec<RowBinding> = params
        .group_cols
        .iter()
        .zip(key)
        .map(|(name, val)| RowBinding {
            qualifier: qual_arc.clone(),
            name: std::sync::Arc::from(name.as_str()),
            ty: val.infer_type().unwrap_or(BqType::UntypedNull),
            value: val,
        })
        .collect();

    // Evaluate the FOR expression once per row so non-deterministic expressions assign each row cleanly.
    let row_for_vals: Vec<BqValue> = bucket
        .iter()
        .map(|r| eval_expr(&params.spec.for_expr, r, ctx))
        .collect::<BqResult<Vec<_>>>()?;

    for (pval, plabel, has_val_alias) in params.pivot_vals {
        let matching: Vec<EvalRow> = bucket
            .iter()
            .zip(row_for_vals.iter())
            .filter(|(_, fv)| values_distinct_eq(fv, pval).unwrap_or(false))
            .map(|(r, _)| r.clone())
            .collect();
        for (agg_expr, agg_alias) in &params.spec.aggregates {
            let val = if matching.is_empty() {
                eval_empty_pivot_agg(agg_expr)
            } else {
                eval_aggregate_expr(agg_expr, &matching, ctx)?
            };
            let col_name = agg_alias.as_ref().map_or_else(
                || {
                    if !has_val_alias
                        && plabel
                            .as_bytes()
                            .first()
                            .is_some_and(|b| b.is_ascii_digit() || *b == b'-')
                    {
                        format!("_{plabel}")
                    } else {
                        plabel.clone()
                    }
                },
                |a| format!("{}_{plabel}", a.value),
            );
            bindings.push(RowBinding {
                qualifier: qual_arc.clone(),
                name: std::sync::Arc::from(col_name),
                ty: val.infer_type()?,
                value: val,
            });
        }
    }
    Ok(EvalRow::new(bindings))
}

fn eval_empty_pivot_agg(expr: &Expr) -> BqValue {
    if let Expr::Function(call) = expr {
        let name = call.name.normalized_parts().join(".").to_ascii_uppercase();
        if name == "COUNT" || name == "COUNTIF" {
            return BqValue::Int64(0);
        }
    }
    BqValue::Null
}

fn agg_references_col(aggs: &[(Expr, Option<crate::ast::Ident>)], col: &str) -> bool {
    aggs.iter().any(|(e, _)| expr_mentions(e, col))
}

fn expr_mentions(expr: &Expr, col: &str) -> bool {
    match expr {
        Expr::Ident(id) => id.value.eq_ignore_ascii_case(col),
        Expr::CompoundIdent(parts) => {
            parts
                .first()
                .is_some_and(|p| p.value.eq_ignore_ascii_case(col))
                || parts
                    .last()
                    .is_some_and(|p| p.value.eq_ignore_ascii_case(col))
        }
        Expr::Function(call) => {
            call.args.iter().any(|a| expr_mentions(&a.expr, col))
                || call.order_by.iter().any(|o| expr_mentions(&o.expr, col))
                || call
                    .having
                    .as_ref()
                    .is_some_and(|(_, e)| expr_mentions(e, col))
        }
        Expr::Binary { left, right, .. } => expr_mentions(left, col) || expr_mentions(right, col),
        Expr::Grouped(inner)
        | Expr::Unary { expr: inner, .. }
        | Expr::Cast { expr: inner, .. }
        | Expr::Is { expr: inner, .. }
        | Expr::FieldAccess { expr: inner, .. }
        | Expr::Extract { expr: inner, .. }
        | Expr::ExprStar(inner) => expr_mentions(inner, col),
        Expr::Between {
            expr: e, low, high, ..
        } => expr_mentions(e, col) || expr_mentions(low, col) || expr_mentions(high, col),
        Expr::Like {
            expr: e, pattern, ..
        } => expr_mentions(e, col) || expr_mentions(pattern, col),
        Expr::In { expr: e, .. } => expr_mentions(e, col),
        Expr::Case {
            operand,
            whens,
            else_result,
        } => {
            operand.as_deref().is_some_and(|o| expr_mentions(o, col))
                || whens
                    .iter()
                    .any(|w| expr_mentions(&w.condition, col) || expr_mentions(&w.result, col))
                || else_result
                    .as_deref()
                    .is_some_and(|e| expr_mentions(e, col))
        }
        Expr::ArrayLiteral { elements, .. } => elements.iter().any(|e| expr_mentions(e, col)),
        Expr::StructLiteral { fields, .. } => fields.iter().any(|f| expr_mentions(&f.expr, col)),
        Expr::Subscript { expr: e, index, .. } => {
            expr_mentions(e, col) || expr_mentions(index, col)
        }
        Expr::Literal(_)
        | Expr::Parameter(_)
        | Expr::PositionalParam(_)
        | Expr::SystemVar(_)
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::WithExpr { .. }
        | Expr::ArraySubquery(_)
        | Expr::Interval { .. }
        | Expr::RangeLiteral { .. }
        | Expr::Lambda { .. }
        | Expr::Exists(_)
        | Expr::Subquery(_) => false,
    }
}

pub(super) fn apply_unpivot(
    rows: Vec<EvalRow>,
    spec: &UnpivotSpec,
    alias: Option<&str>,
) -> BqResult<Vec<EvalRow>> {
    let mut unpivoted_names: BTreeSet<String> = BTreeSet::new();
    for (cols, _) in &spec.in_cols {
        for c in cols {
            unpivoted_names.insert(c.value.to_ascii_lowercase());
        }
    }

    let mut out = Vec::new();
    for row in rows {
        let base_cols = base_unpivot_cols(&row, &unpivoted_names, alias);
        for (src_cols, label_expr) in &spec.in_cols {
            if let Some(unpivoted) =
                build_unpivot_row(&row, &base_cols, src_cols, label_expr.as_ref(), spec, alias)?
            {
                out.push(unpivoted);
            }
        }
    }
    Ok(out)
}

fn base_unpivot_cols(
    row: &EvalRow,
    excluded: &BTreeSet<String>,
    alias: Option<&str>,
) -> Vec<RowBinding> {
    let qual_arc: Option<std::sync::Arc<str>> = alias.map(std::sync::Arc::from);
    row.cols
        .iter()
        .filter(|c| !excluded.contains(&c.name.to_ascii_lowercase()))
        .map(|c| RowBinding {
            qualifier: qual_arc.clone(),
            name: c.name.clone(),
            ty: c.ty.clone(),
            value: c.value.clone(),
        })
        .collect()
}

fn build_unpivot_row(
    row: &EvalRow,
    base_cols: &[RowBinding],
    src_cols: &[crate::ast::Ident],
    label_expr: Option<&Expr>,
    spec: &UnpivotSpec,
    alias: Option<&str>,
) -> BqResult<Option<EvalRow>> {
    if src_cols.len() != spec.value_cols.len() {
        return Err(BqError::query("UNPIVOT IN column count mismatch"));
    }
    let mut vals = Vec::with_capacity(src_cols.len());
    for sc in src_cols {
        let b = row
            .resolve_ident(&sc.value)?
            .ok_or_else(|| BqError::query(format!("Column '{}' not found in UNPIVOT", sc.value)))?;
        vals.push(b.clone());
    }
    if !spec.include_nulls && vals.iter().all(|b| b.value.is_null()) {
        return Ok(None);
    }
    let qual_arc: Option<std::sync::Arc<str>> = alias.map(std::sync::Arc::from);
    let mut new_cols = base_cols.to_vec();
    for (dst_ident, src_binding) in spec.value_cols.iter().zip(vals) {
        new_cols.push(RowBinding {
            qualifier: qual_arc.clone(),
            name: std::sync::Arc::from(dst_ident.value.as_str()),
            ty: src_binding.ty,
            value: src_binding.value,
        });
    }
    let label = match label_expr {
        Some(Expr::Literal(crate::ast::Literal::String(s))) => s.clone(),
        Some(_) => return Err(BqError::query("UNPIVOT AS alias must be a string literal")),
        None => src_cols
            .iter()
            .map(|c| c.value.as_str())
            .collect::<Vec<_>>()
            .join("_"),
    };
    new_cols.push(RowBinding {
        qualifier: qual_arc,
        name: std::sync::Arc::from(spec.name_col.value.as_str()),
        ty: BqType::String,
        value: BqValue::String(label),
    });
    Ok(Some(EvalRow::new(new_cols)))
}

#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::wildcard_enum_match_arm,
    reason = "TABLESAMPLE computes row sample count from numeric percentage/count"
)]
pub(super) fn apply_tablesample(
    rows: Vec<EvalRow>,
    amount_expr: &Expr,
    is_percent: bool,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    let val = eval_expr(amount_expr, &EvalRow::default(), ctx)?;
    if val.is_null() {
        return Err(BqError::semantic("TABLESAMPLE size cannot be NULL"));
    }
    let amt = match val {
        BqValue::Int64(n) => n as f64,
        BqValue::Float64(f) => f,
        BqValue::Numeric(n) => n.to_f64(),
        BqValue::BigNumeric(bn) => bn.to_f64(),
        _ => return Err(BqError::query("TABLESAMPLE size must be numeric")),
    };
    if is_percent {
        if !(0.0..=100.0).contains(&amt) {
            return Err(BqError::semantic(
                "TABLESAMPLE PERCENT must be between 0 and 100",
            ));
        }
        if amt <= 0.0 {
            return Ok(Vec::new());
        }
        if amt >= 100.0 {
            return Ok(rows);
        }
        let total = rows.len();
        let target = ((total as f64) * (amt / 100.0)).round() as usize;
        Ok(rows.into_iter().take(target.min(total)).collect())
    } else {
        if amt < 0.0 {
            return Err(BqError::semantic("TABLESAMPLE ROWS cannot be negative"));
        }
        let total = rows.len();
        let cap = amt.trunc() as usize;
        Ok(rows.into_iter().take(cap.min(total)).collect())
    }
}
