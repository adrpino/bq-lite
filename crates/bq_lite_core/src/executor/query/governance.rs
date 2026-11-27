use std::sync::Arc;

use crate::ast::{Expr, Ident};
use crate::catalog::{ColumnSchema, RowAccessPolicyMeta};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext, RowBinding};
use crate::executor::evaluator::eval_expr;
use crate::types::{BqStructFieldValue, BqType, BqValue, StructField};

pub(super) fn unpack_struct_unnest(
    fields: &[BqStructFieldValue],
    alias: Option<&Ident>,
    whole_val: &BqValue,
    whole_ty: &BqType,
) -> (Vec<RowBinding>, Vec<RowBinding>) {
    let qual: Option<Arc<str>> = alias.map(|a| Arc::from(a.value.as_str()));
    let cols = fields
        .iter()
        .enumerate()
        .map(|(i, sf)| RowBinding {
            qualifier: qual.clone(),
            name: Arc::from(sf.name.clone().unwrap_or_else(|| format!("f{i}_"))),
            ty: sf.value.infer_type().unwrap_or(BqType::UntypedNull),
            value: sf.value.clone(),
        })
        .collect();
    let pseudo = qual.map_or_else(Vec::new, |a| {
        vec![RowBinding {
            qualifier: None,
            name: a,
            ty: whole_ty.clone(),
            value: whole_val.clone(),
        }]
    });
    (cols, pseudo)
}

pub(super) fn unpack_null_struct_unnest(
    fields: &[StructField],
    alias: Option<&Ident>,
) -> (Vec<RowBinding>, Vec<RowBinding>) {
    let qual: Option<Arc<str>> = alias.map(|a| Arc::from(a.value.as_str()));
    let cols = fields
        .iter()
        .enumerate()
        .map(|(i, sf)| RowBinding {
            qualifier: qual.clone(),
            name: Arc::from(sf.name.clone().unwrap_or_else(|| format!("f{i}_"))),
            ty: sf.ty.clone(),
            value: BqValue::Null,
        })
        .collect();
    let pseudo = qual.map_or_else(Vec::new, |a| {
        vec![RowBinding {
            qualifier: None,
            name: a,
            ty: BqType::Struct(fields.to_vec()),
            value: BqValue::Null,
        }]
    });
    (cols, pseudo)
}

pub(super) fn policy_matches_session(pol: &RowAccessPolicyMeta, ctx: &ExecContext<'_>) -> bool {
    if pol.grantees.is_empty() {
        return true;
    }
    let user = &ctx.governance.session_user;
    let user_prefixed = format!("user:{user}");
    pol.grantees.iter().any(|g| {
        g.eq_ignore_ascii_case("allUsers")
            || g.eq_ignore_ascii_case("allAuthenticatedUsers")
            || g.eq_ignore_ascii_case(user)
            || g.eq_ignore_ascii_case(&user_prefixed)
            || ctx.governance.session_roles.iter().any(|role| {
                g.eq_ignore_ascii_case(role)
                    || g.eq_ignore_ascii_case(&format!("group:{role}"))
                    || g.eq_ignore_ascii_case(&format!("role:{role}"))
            })
    })
}

pub(super) fn mask_row_columns(cols: &[ColumnSchema], row: &mut [BqValue], ctx: &ExecContext<'_>) {
    if !ctx.governance.enforce_column_masking {
        return;
    }
    let bypass = ctx.governance.session_roles.iter().any(|r| {
        r.eq_ignore_ascii_case("unmasked") || r.eq_ignore_ascii_case("fine_grained_reader")
    });
    if bypass {
        return;
    }
    for (col, val) in cols.iter().zip(row.iter_mut()) {
        if let Some(ref tag) = col.policy_tag {
            let masked = ctx
                .governance
                .masking_policies
                .get(tag)
                .and_then(Clone::clone)
                .unwrap_or(BqValue::Null);
            *val = masked;
        }
    }
}

#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::wildcard_enum_match_arm,
    reason = "TABLESAMPLE SYSTEM computes fractional sample row count"
)]
pub(super) fn apply_tablesample(
    mut rows: Vec<EvalRow>,
    pct_expr: &Expr,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    let val = eval_expr(pct_expr, &EvalRow::default(), ctx)?;
    let pct = match val {
        BqValue::Int64(n) => n as f64,
        BqValue::Float64(f) => f,
        BqValue::Numeric(n) => n.to_f64(),
        _ => {
            return Err(BqError::query(
                "TABLESAMPLE SYSTEM expects a numeric percentage",
            ))
        }
    };
    if !(0.0..=100.0).contains(&pct) {
        return Err(BqError::semantic(
            "TABLESAMPLE percentage must be between 0 and 100",
        ));
    }
    let keep = (((rows.len() as f64) * pct) / 100.0).ceil() as usize;
    rows.truncate(keep.min(rows.len()));
    Ok(rows)
}

pub(super) fn unnest_schema_row(
    elem_ty: &BqType,
    alias: Option<&str>,
    offset_name: Option<&str>,
) -> EvalRow {
    let mut cols = Vec::new();
    let mut pseudo = Vec::new();
    let qual_arc: Option<Arc<str>> = alias.map(Arc::from);
    if let BqType::Struct(fields) = elem_ty {
        for (i, sf) in fields.iter().enumerate() {
            let fname = sf.name.clone().unwrap_or_else(|| format!("f{i}_"));
            cols.push(RowBinding {
                qualifier: qual_arc.clone(),
                name: Arc::from(fname),
                ty: sf.ty.clone(),
                value: BqValue::Null,
            });
        }
        if let Some(a) = qual_arc {
            pseudo.push(RowBinding {
                qualifier: None,
                name: a,
                ty: elem_ty.clone(),
                value: BqValue::Null,
            });
        }
    } else {
        let elem_name: Arc<str> = Arc::from(alias.unwrap_or("f0_"));
        cols.push(RowBinding {
            qualifier: qual_arc,
            name: elem_name,
            ty: elem_ty.clone(),
            value: BqValue::Null,
        });
    }
    if let Some(off) = offset_name {
        cols.push(RowBinding {
            qualifier: None,
            name: Arc::from(off),
            ty: BqType::Int64,
            value: BqValue::Null,
        });
    }
    EvalRow::with_pseudo(cols, pseudo)
}
