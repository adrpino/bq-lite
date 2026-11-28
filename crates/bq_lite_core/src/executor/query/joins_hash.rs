use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, NaiveDateTime, NaiveTime, Utc};

use crate::ast::{BinaryOp, Expr, Join, JoinConstraint, JoinKind};
use crate::error::BqResult;
use crate::executor::context::{ColSlot, EvalRow, ExecContext, RowRef, RowSchema};
use crate::executor::evaluator::{eval_expr, eval_expr_ref, values_equal};
use crate::types::BqValue;

use super::joins_using::JoinOutputPlan;

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

enum KeyAccessor<'a> {
    ColSlot {
        schema: &'a Arc<RowSchema>,
        slot: usize,
        expr: &'a Expr,
    },
    Expr(&'a Expr),
}

impl<'a> KeyAccessor<'a> {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "Only plain column idents and table.col paths resolve to direct column slots"
    )]
    fn resolve(expr: &'a Expr, schema: &'a Arc<RowSchema>) -> Self {
        match expr {
            Expr::Ident(id) => {
                if let Ok(Some(ColSlot::Col(slot))) = schema.lookup_ident_slot(&id.value) {
                    return Self::ColSlot { schema, slot, expr };
                }
            }
            Expr::CompoundIdent(parts) => {
                if let [first, second] = parts.as_slice() {
                    if let Some(ColSlot::Col(slot)) =
                        schema.lookup_qualified_slot(&first.value, &second.value)
                    {
                        return Self::ColSlot { schema, slot, expr };
                    }
                }
            }
            _ => {}
        }
        Self::Expr(expr)
    }

    #[inline]
    fn eval_ref<'r>(&self, row: &'r EvalRow, ctx: &ExecContext<'_>) -> BqResult<Cow<'r, BqValue>> {
        match self {
            Self::ColSlot { schema, slot, expr } => {
                if Arc::ptr_eq(&row.schema, schema) {
                    if let Some(val) = row.values.get(*slot) {
                        return Ok(Cow::Borrowed(val));
                    }
                }
                Ok(Cow::Owned(eval_expr(expr, row, ctx)?))
            }
            Self::Expr(expr) => Ok(Cow::Owned(eval_expr(expr, row, ctx)?)),
        }
    }
}

enum SingleKeyEval<'r> {
    Key(JoinKey, Cow<'r, BqValue>),
    HasNull,
    Unhashable,
}

enum MultiKeyEval<'r> {
    Keys(Vec<JoinKey>, Vec<Cow<'r, BqValue>>),
    HasNull,
    Unhashable,
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
    if let JoinConstraint::Using(cols) = &join.constraint {
        for col in cols {
            if left_schema.resolve_ident(&col.value)?.is_none()
                || right_schema.resolve_ident(&col.value)?.is_none()
            {
                return Ok(None);
            }
        }
    }
    let (pairs, pure_equi) = extract_equi_pairs(&join.constraint, left_schema, right_schema);
    if pairs.is_empty() {
        return Ok(None);
    }
    let left_accs: Vec<KeyAccessor<'_>> = pairs
        .iter()
        .map(|(le, _)| KeyAccessor::resolve(le, &left_schema.schema))
        .collect();
    let right_accs: Vec<KeyAccessor<'_>> = pairs
        .iter()
        .map(|(_, re)| KeyAccessor::resolve(re, &right_schema.schema))
        .collect();
    let plan = JoinOutputPlan::new(&left_schema.schema, &right_schema.schema, &join.constraint);

    if let ([left_acc], [right_acc]) = (left_accs.as_slice(), right_accs.as_slice()) {
        run_single_key_hash_join(
            left_rows,
            left_schema,
            right_rows,
            right_schema,
            join,
            pure_equi,
            (left_acc, right_acc),
            &plan,
            ctx,
        )
    } else {
        run_multi_key_hash_join(
            left_rows,
            left_schema,
            right_rows,
            right_schema,
            join,
            pure_equi,
            (&left_accs, &right_accs),
            &plan,
            ctx,
        )
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Hash join probe needs left/right rows, schemas, join spec, accessors, output plan, and context"
)]
fn run_single_key_hash_join(
    left_rows: &[EvalRow],
    left_schema: &EvalRow,
    right_rows: &[EvalRow],
    right_schema: &EvalRow,
    join: &Join,
    pure_equi: bool,
    accs: (&KeyAccessor<'_>, &KeyAccessor<'_>),
    plan: &JoinOutputPlan,
    ctx: &ExecContext<'_>,
) -> BqResult<Option<Vec<EvalRow>>> {
    let (left_acc, right_acc) = accs;
    let mut index: HashMap<JoinKey, Vec<usize>> = HashMap::with_capacity(right_rows.len());
    for (r_idx, rrow) in right_rows.iter().enumerate() {
        match eval_single_key(right_acc, rrow, ctx)? {
            SingleKeyEval::Key(k, _) => index.entry(k).or_default().push(r_idx),
            SingleKeyEval::HasNull => {}
            SingleKeyEval::Unhashable => return Ok(None),
        }
    }

    let left_null = super::schema::null_out_eval_row(left_schema);
    let right_null = super::schema::null_out_eval_row(right_schema);
    let mut right_matched = vec![false; right_rows.len()];
    let cap = if right_rows.is_empty() {
        0
    } else {
        left_rows.len().min(65_536)
    };
    let mut out = Vec::with_capacity(cap);

    for lrow in left_rows {
        let mut l_matched = false;
        match eval_single_key(left_acc, lrow, ctx)? {
            SingleKeyEval::Key(key, lval) => {
                if let Some(cands) = index.get(&key) {
                    for &r_idx in cands {
                        let Some(rrow) = right_rows.get(r_idx) else {
                            continue;
                        };
                        let matched = if pure_equi {
                            let rval = right_acc.eval_ref(rrow, ctx)?;
                            fast_values_equal(lval.as_ref(), rval.as_ref())?
                        } else {
                            verify_on_predicate(&join.constraint, lrow, rrow, ctx)?
                        };
                        if matched {
                            l_matched = true;
                            if let Some(slot) = right_matched.get_mut(r_idx) {
                                *slot = true;
                            }
                            out.push(plan.combine(lrow, rrow, &join.constraint));
                        }
                    }
                }
            }
            SingleKeyEval::HasNull => {}
            SingleKeyEval::Unhashable => return Ok(None),
        }
        if !l_matched && matches!(join.kind, JoinKind::Left | JoinKind::Full) {
            out.push(plan.combine(lrow, &right_null, &join.constraint));
        }
    }

    append_unmatched_right(right_rows, &right_matched, &left_null, join, plan, &mut out);
    Ok(Some(out))
}

#[expect(
    clippy::too_many_arguments,
    reason = "Multi-key hash join probe needs left/right rows, schemas, join spec, accessors, output plan, and context"
)]
fn run_multi_key_hash_join(
    left_rows: &[EvalRow],
    left_schema: &EvalRow,
    right_rows: &[EvalRow],
    right_schema: &EvalRow,
    join: &Join,
    pure_equi: bool,
    accs: (&[KeyAccessor<'_>], &[KeyAccessor<'_>]),
    plan: &JoinOutputPlan,
    ctx: &ExecContext<'_>,
) -> BqResult<Option<Vec<EvalRow>>> {
    let (left_accs, right_accs) = accs;
    let mut index: HashMap<Vec<JoinKey>, Vec<usize>> = HashMap::with_capacity(right_rows.len());
    for (r_idx, rrow) in right_rows.iter().enumerate() {
        match eval_multi_key(right_accs, rrow, ctx)? {
            MultiKeyEval::Keys(keys, _) => index.entry(keys).or_default().push(r_idx),
            MultiKeyEval::HasNull => {}
            MultiKeyEval::Unhashable => return Ok(None),
        }
    }

    let left_null = super::schema::null_out_eval_row(left_schema);
    let right_null = super::schema::null_out_eval_row(right_schema);
    let mut right_matched = vec![false; right_rows.len()];
    let mut out = Vec::with_capacity(if right_rows.is_empty() {
        0
    } else {
        left_rows.len().min(65_536)
    });

    for lrow in left_rows {
        let mut l_matched = false;
        match eval_multi_key(left_accs, lrow, ctx)? {
            MultiKeyEval::Keys(keys, lvals) => {
                if let Some(cands) = index.get(&keys) {
                    for &r_idx in cands {
                        let Some(rrow) = right_rows.get(r_idx) else {
                            continue;
                        };
                        let matched = if pure_equi {
                            verify_multi_equi(&lvals, right_accs, rrow, ctx)?
                        } else {
                            verify_on_predicate(&join.constraint, lrow, rrow, ctx)?
                        };
                        if matched {
                            l_matched = true;
                            if let Some(slot) = right_matched.get_mut(r_idx) {
                                *slot = true;
                            }
                            out.push(plan.combine(lrow, rrow, &join.constraint));
                        }
                    }
                }
            }
            MultiKeyEval::HasNull => {}
            MultiKeyEval::Unhashable => return Ok(None),
        }
        if !l_matched && matches!(join.kind, JoinKind::Left | JoinKind::Full) {
            out.push(plan.combine(lrow, &right_null, &join.constraint));
        }
    }

    append_unmatched_right(right_rows, &right_matched, &left_null, join, plan, &mut out);
    Ok(Some(out))
}

fn append_unmatched_right(
    right_rows: &[EvalRow],
    right_matched: &[bool],
    left_null: &EvalRow,
    join: &Join,
    plan: &JoinOutputPlan,
    out: &mut Vec<EvalRow>,
) {
    if matches!(join.kind, JoinKind::Right | JoinKind::Full) {
        for (r_idx, rrow) in right_rows.iter().enumerate() {
            if !right_matched.get(r_idx).copied().unwrap_or(false) {
                out.push(plan.combine(left_null, rrow, &join.constraint));
            }
        }
    }
}

fn verify_multi_equi(
    lvals: &[Cow<'_, BqValue>],
    right_accs: &[KeyAccessor<'_>],
    rrow: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<bool> {
    for (lv, racc) in lvals.iter().zip(right_accs) {
        let rv = racc.eval_ref(rrow, ctx)?;
        if !fast_values_equal(lv.as_ref(), rv.as_ref())? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn verify_on_predicate(
    constraint: &JoinConstraint,
    lrow: &EvalRow,
    rrow: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<bool> {
    let JoinConstraint::On(expr) = constraint else {
        return Ok(true);
    };
    let pred = eval_expr_ref(
        expr,
        RowRef::Joined {
            left: lrow,
            right: rrow,
        },
        ctx,
    )?;
    Ok(matches!(pred, BqValue::Bool(true)))
}

#[inline]
fn fast_values_equal(a: &BqValue, b: &BqValue) -> BqResult<bool> {
    match (a, b) {
        (BqValue::Int64(x), BqValue::Int64(y)) => Ok(x == y),
        (BqValue::String(x), BqValue::String(y)) => Ok(x == y),
        (BqValue::Bool(x), BqValue::Bool(y)) => Ok(x == y),
        (BqValue::Date(x), BqValue::Date(y)) => Ok(x == y),
        (BqValue::Timestamp(x), BqValue::Timestamp(y)) => Ok(x == y),
        _ => values_equal(a, b),
    }
}

#[inline]
fn eval_single_key<'r>(
    acc: &KeyAccessor<'_>,
    row: &'r EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<SingleKeyEval<'r>> {
    let val = acc.eval_ref(row, ctx)?;
    if val.is_null() {
        return Ok(SingleKeyEval::HasNull);
    }
    if let BqValue::Float64(f) = val.as_ref() {
        if f.is_nan() {
            return Ok(SingleKeyEval::HasNull);
        }
    }
    Ok(value_to_join_key(val.as_ref())
        .map_or(SingleKeyEval::Unhashable, |k| SingleKeyEval::Key(k, val)))
}

fn eval_multi_key<'r>(
    accs: &[KeyAccessor<'_>],
    row: &'r EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<MultiKeyEval<'r>> {
    let mut keys = Vec::with_capacity(accs.len());
    let mut vals = Vec::with_capacity(accs.len());
    for acc in accs {
        let val = acc.eval_ref(row, ctx)?;
        if val.is_null() {
            return Ok(MultiKeyEval::HasNull);
        }
        if let BqValue::Float64(f) = val.as_ref() {
            if f.is_nan() {
                return Ok(MultiKeyEval::HasNull);
            }
        }
        let Some(k) = value_to_join_key(val.as_ref()) else {
            return Ok(MultiKeyEval::Unhashable);
        };
        keys.push(k);
        vals.push(val);
    }
    Ok(MultiKeyEval::Keys(keys, vals))
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
) -> (Vec<(Expr, Expr)>, bool) {
    match constraint {
        JoinConstraint::None => (Vec::new(), false),
        JoinConstraint::Using(cols) => (
            cols.iter()
                .map(|c| (Expr::Ident(c.clone()), Expr::Ident(c.clone())))
                .collect(),
            true,
        ),
        JoinConstraint::On(expr) => {
            let mut out = Vec::new();
            let pure = collect_conjunct_pairs(expr, left_schema, right_schema, &mut out);
            (out, pure)
        }
    }
}

fn collect_conjunct_pairs(
    expr: &Expr,
    left_schema: &EvalRow,
    right_schema: &EvalRow,
    out: &mut Vec<(Expr, Expr)>,
) -> bool {
    if let Expr::Grouped(inner) = expr {
        return collect_conjunct_pairs(inner, left_schema, right_schema, out);
    }
    if let Expr::Binary {
        left,
        op: BinaryOp::And,
        right,
    } = expr
    {
        let lp = collect_conjunct_pairs(left, left_schema, right_schema, out);
        let rp = collect_conjunct_pairs(right, left_schema, right_schema, out);
        return lp && rp;
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
            return true;
        }
        if ls == ExprSide::Right && rs == ExprSide::Left {
            out.push((right.as_ref().clone(), left.as_ref().clone()));
            return true;
        }
    }
    false
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
