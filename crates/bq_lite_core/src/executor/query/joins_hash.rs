use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, NaiveDateTime, NaiveTime, Utc};

use crate::ast::{BinaryOp, Expr, Join, JoinConstraint, JoinKind};
use crate::error::BqResult;
use crate::executor::context::{EvalRow, ExecContext, RowSchema};
use crate::executor::evaluator::eval_expr;
use crate::types::BqValue;

use super::joins::{combine_rows_for_constraint, try_join_pair};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum JoinKey {
    Bool(bool),
    Num(u64),
    String(String),
    Bytes(Vec<u8>),
    Datetime(NaiveDateTime),
    Time(NaiveTime),
    Timestamp(DateTime<Utc>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExprSide {
    Neither,
    Left,
    Right,
    Mixed,
}

impl ExprSide {
    const fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Neither, x) | (x, Self::Neither) => x,
            (Self::Left, Self::Left) => Self::Left,
            (Self::Right, Self::Right) => Self::Right,
            _ => Self::Mixed,
        }
    }
}

pub(super) fn try_hash_join(
    left_rows: &[EvalRow],
    left_schema: &EvalRow,
    right_rows: &[EvalRow],
    right_schema: &EvalRow,
    join: &Join,
    ctx: &ExecContext<'_>,
) -> BqResult<Option<Vec<EvalRow>>> {
    if left_rows.len().saturating_mul(right_rows.len()) <= 64 {
        return Ok(None);
    }
    let pairs = extract_equi_pairs(&join.constraint, left_schema, right_schema);
    if pairs.is_empty() {
        return Ok(None);
    }
    let mut index: HashMap<Vec<JoinKey>, Vec<usize>> = HashMap::with_capacity(right_rows.len());
    for (r_idx, rrow) in right_rows.iter().enumerate() {
        match eval_key_tuple(&pairs, false, rrow, ctx)? {
            KeyEval::Keys(keys) => index.entry(keys).or_default().push(r_idx),
            KeyEval::HasNull => {}
            KeyEval::Unhashable => return Ok(None),
        }
    }
    probe_hash_join(
        left_rows,
        left_schema,
        right_rows,
        right_schema,
        join,
        &pairs,
        &index,
        ctx,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "Hash probe requires both row slices, schemas, join spec, extracted key pairs, and index"
)]
fn probe_hash_join(
    left_rows: &[EvalRow],
    left_schema: &EvalRow,
    right_rows: &[EvalRow],
    right_schema: &EvalRow,
    join: &Join,
    pairs: &[(Expr, Expr)],
    index: &HashMap<Vec<JoinKey>, Vec<usize>>,
    ctx: &ExecContext<'_>,
) -> BqResult<Option<Vec<EvalRow>>> {
    let left_null = super::schema::null_out_eval_row(left_schema);
    let right_null = super::schema::null_out_eval_row(right_schema);
    let shared_schema = (!matches!(join.constraint, JoinConstraint::Using(_)))
        .then(|| RowSchema::concat(&left_schema.schema, &right_schema.schema));
    let mut right_matched = vec![false; right_rows.len()];
    let mut out = Vec::new();

    for lrow in left_rows {
        let mut l_matched = false;
        match eval_key_tuple(pairs, true, lrow, ctx)? {
            KeyEval::Keys(keys) => {
                if let Some(cands) = index.get(&keys) {
                    for &r_idx in cands {
                        let Some(rrow) = right_rows.get(r_idx) else {
                            continue;
                        };
                        let pair_schema = shared_schema.as_ref().filter(|_| {
                            Arc::ptr_eq(&lrow.schema, &left_schema.schema)
                                && Arc::ptr_eq(&rrow.schema, &right_schema.schema)
                        });
                        if let Some(joined) =
                            try_join_pair(lrow, rrow, &join.constraint, pair_schema, ctx)?
                        {
                            l_matched = true;
                            if let Some(slot) = right_matched.get_mut(r_idx) {
                                *slot = true;
                            }
                            out.push(joined);
                        }
                    }
                }
            }
            KeyEval::HasNull => {}
            KeyEval::Unhashable => return Ok(None),
        }
        if !l_matched && matches!(join.kind, JoinKind::Left | JoinKind::Full) {
            out.push(combine_rows_for_constraint(
                lrow,
                &right_null,
                &join.constraint,
            ));
        }
    }

    if matches!(join.kind, JoinKind::Right | JoinKind::Full) {
        for (r_idx, rrow) in right_rows.iter().enumerate() {
            if !right_matched.get(r_idx).copied().unwrap_or(false) {
                out.push(combine_rows_for_constraint(
                    &left_null,
                    rrow,
                    &join.constraint,
                ));
            }
        }
    }
    Ok(Some(out))
}

enum KeyEval {
    Keys(Vec<JoinKey>),
    HasNull,
    Unhashable,
}

fn eval_key_tuple(
    pairs: &[(Expr, Expr)],
    is_left: bool,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<KeyEval> {
    let mut out = Vec::with_capacity(pairs.len());
    for (le, re) in pairs {
        let expr = if is_left { le } else { re };
        let val = eval_expr(expr, row, ctx)?;
        if val.is_null() {
            return Ok(KeyEval::HasNull);
        }
        if let BqValue::Float64(f) = val {
            if f.is_nan() {
                return Ok(KeyEval::HasNull);
            }
        }
        let Some(k) = value_to_join_key(&val) else {
            return Ok(KeyEval::Unhashable);
        };
        out.push(k);
    }
    Ok(KeyEval::Keys(out))
}

#[expect(
    clippy::cast_precision_loss,
    clippy::wildcard_enum_match_arm,
    reason = "Int64/Numeric hash buckets normalize to canonical IEEE-754 bits and verify exact equality on probe"
)]
fn value_to_join_key(val: &BqValue) -> Option<JoinKey> {
    match val {
        BqValue::Bool(b) => Some(JoinKey::Bool(*b)),
        BqValue::Int64(n) => Some(JoinKey::Num(canonical_f64_bits(*n as f64))),
        BqValue::Float64(f) => Some(JoinKey::Num(canonical_f64_bits(*f))),
        BqValue::Numeric(n) => Some(JoinKey::Num(canonical_f64_bits(n.to_f64()))),
        BqValue::BigNumeric(n) => Some(JoinKey::Num(canonical_f64_bits(n.to_f64()))),
        BqValue::String(s) => Some(JoinKey::String(s.clone())),
        BqValue::Bytes(b) => Some(JoinKey::Bytes(b.clone())),
        BqValue::Date(d) => d.and_hms_opt(0, 0, 0).map(JoinKey::Datetime),
        BqValue::Datetime(dt) => Some(JoinKey::Datetime(*dt)),
        BqValue::Time(t) => Some(JoinKey::Time(*t)),
        BqValue::Timestamp(ts) => Some(JoinKey::Timestamp(*ts)),
        _ => None,
    }
}

fn canonical_f64_bits(f: f64) -> u64 {
    let bits = f.to_bits();
    if bits == (-0.0_f64).to_bits() {
        0.0_f64.to_bits()
    } else {
        bits
    }
}

fn extract_equi_pairs(
    constraint: &JoinConstraint,
    left_schema: &EvalRow,
    right_schema: &EvalRow,
) -> Vec<(Expr, Expr)> {
    match constraint {
        JoinConstraint::None => Vec::new(),
        JoinConstraint::Using(cols) => cols
            .iter()
            .map(|c| (Expr::Ident(c.clone()), Expr::Ident(c.clone())))
            .collect(),
        JoinConstraint::On(expr) => {
            let mut out = Vec::new();
            collect_conjunct_pairs(expr, left_schema, right_schema, &mut out);
            out
        }
    }
}

fn collect_conjunct_pairs(
    expr: &Expr,
    left_schema: &EvalRow,
    right_schema: &EvalRow,
    out: &mut Vec<(Expr, Expr)>,
) {
    if let Expr::Grouped(inner) = expr {
        collect_conjunct_pairs(inner, left_schema, right_schema, out);
        return;
    }
    if let Expr::Binary {
        left,
        op: BinaryOp::And,
        right,
    } = expr
    {
        collect_conjunct_pairs(left, left_schema, right_schema, out);
        collect_conjunct_pairs(right, left_schema, right_schema, out);
        return;
    }
    if let Expr::Binary {
        left,
        op: BinaryOp::Eq,
        right,
    } = expr
    {
        let ls = classify_expr_side(left, left_schema, right_schema);
        let rs = classify_expr_side(right, left_schema, right_schema);
        if ls == ExprSide::Left && rs == ExprSide::Right {
            out.push((left.as_ref().clone(), right.as_ref().clone()));
        } else if ls == ExprSide::Right && rs == ExprSide::Left {
            out.push((right.as_ref().clone(), left.as_ref().clone()));
        }
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Complex/subquery expressions default safely to Mixed so hash join falls back"
)]
fn classify_expr_side(expr: &Expr, left_schema: &EvalRow, right_schema: &EvalRow) -> ExprSide {
    match expr {
        Expr::Literal(_) => ExprSide::Neither,
        Expr::Ident(id) => {
            let in_l = left_schema
                .resolve_ident(&id.value)
                .ok()
                .flatten()
                .is_some();
            let in_r = right_schema
                .resolve_ident(&id.value)
                .ok()
                .flatten()
                .is_some();
            match (in_l, in_r) {
                (true, false) => ExprSide::Left,
                (false, true) => ExprSide::Right,
                _ => ExprSide::Mixed,
            }
        }
        Expr::CompoundIdent(parts) => classify_compound_side(parts, left_schema, right_schema),
        Expr::Grouped(inner) | Expr::Unary { expr: inner, .. } => {
            classify_expr_side(inner, left_schema, right_schema)
        }
        Expr::Cast { expr: inner, .. }
        | Expr::FieldAccess { expr: inner, .. }
        | Expr::Extract { expr: inner, .. } => classify_expr_side(inner, left_schema, right_schema),
        Expr::Binary { left, right, .. } => classify_expr_side(left, left_schema, right_schema)
            .merge(classify_expr_side(right, left_schema, right_schema)),
        Expr::Function(call) => call.args.iter().fold(ExprSide::Neither, |acc, a| {
            acc.merge(classify_expr_side(&a.expr, left_schema, right_schema))
        }),
        _ => ExprSide::Mixed,
    }
}

fn classify_compound_side(
    parts: &[crate::ast::Ident],
    left_schema: &EvalRow,
    right_schema: &EvalRow,
) -> ExprSide {
    let Some(first) = parts.first() else {
        return ExprSide::Mixed;
    };
    let in_l = parts.get(1).is_some_and(|p1| {
        left_schema
            .resolve_qualified(&first.value, &p1.value)
            .is_some()
    }) || left_schema
        .resolve_ident(&first.value)
        .ok()
        .flatten()
        .is_some();
    let in_r = parts.get(1).is_some_and(|p1| {
        right_schema
            .resolve_qualified(&first.value, &p1.value)
            .is_some()
    }) || right_schema
        .resolve_ident(&first.value)
        .ok()
        .flatten()
        .is_some();
    match (in_l, in_r) {
        (true, false) => ExprSide::Left,
        (false, true) => ExprSide::Right,
        _ => ExprSide::Mixed,
    }
}
