use crate::ast::{BinaryOp, Expr, Literal, ObjectName, UnaryOp};
use crate::executor::context::ExecContext;
use crate::executor::evaluator::{infer_expr_col_name, lookup_udf_return_type};
use crate::types::coercion::common_supertype;
use crate::types::{BqType, RangeElementType, StructField};

use super::schema::{infer_query_columns, resolve_struct_type_for_name, struct_field_ty};
use crate::executor::context::EvalRow;

pub(in crate::executor) fn infer_expr_type(
    expr: &Expr,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqType {
    match expr {
        Expr::Literal(lit) => infer_literal_type(lit),
        Expr::Ident(id) => row
            .resolve_ident(&id.value)
            .ok()
            .flatten()
            .map(|b| b.ty.clone())
            .or_else(|| {
                ctx.vars
                    .get(&id.value.to_ascii_lowercase())
                    .and_then(|v| v.infer_type().ok())
            })
            .unwrap_or(BqType::UntypedNull),
        Expr::CompoundIdent(parts) => {
            let obj = ObjectName::new(parts.clone());
            resolve_struct_type_for_name(&obj, row).unwrap_or(BqType::UntypedNull)
        }
        Expr::Parameter(name) => ctx
            .resolve_param(name)
            .and_then(|v| v.infer_type().ok())
            .unwrap_or(BqType::UntypedNull),
        Expr::PositionalParam(idx) => ctx
            .positional_params
            .get(idx.saturating_sub(1))
            .and_then(|v| v.infer_type().ok())
            .unwrap_or(BqType::UntypedNull),
        Expr::SystemVar(_) => BqType::String,
        Expr::Grouped(inner) => infer_expr_type(inner, row, ctx),
        Expr::Unary { op, expr: inner } => match op {
            UnaryOp::Not => BqType::Bool,
            UnaryOp::Plus | UnaryOp::Minus | UnaryOp::BitNot => infer_expr_type(inner, row, ctx),
        },
        Expr::Binary { left, op, right } => infer_binary_type(*op, left, right, row, ctx),
        Expr::Is { .. }
        | Expr::Between { .. }
        | Expr::Like { .. }
        | Expr::In { .. }
        | Expr::Exists(_) => BqType::Bool,
        Expr::Cast { ty, .. } => ty.to_bq_type().unwrap_or(BqType::String),
        Expr::Extract { .. } => BqType::Int64,
        Expr::Interval { .. } => BqType::Interval,
        Expr::Case { .. }
        | Expr::ArrayLiteral { .. }
        | Expr::StructLiteral { .. }
        | Expr::FieldAccess { .. }
        | Expr::Subscript { .. }
        | Expr::RangeLiteral { .. }
        | Expr::Function(_)
        | Expr::Subquery(_)
        | Expr::ArraySubquery(_)
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::Lambda { .. } => infer_complex_expr_type(expr, row, ctx),
    }
}

const fn infer_literal_type(lit: &Literal) -> BqType {
    match lit {
        Literal::Null => BqType::UntypedNull,
        Literal::Bool(_) => BqType::Bool,
        Literal::Int(_) => BqType::Int64,
        Literal::Float(_) => BqType::Float64,
        Literal::Numeric(_) => BqType::Numeric,
        Literal::BigNumeric(_) => BqType::BigNumeric,
        Literal::String(_) => BqType::String,
        Literal::Bytes(_) => BqType::Bytes,
        Literal::Date(_) => BqType::Date,
        Literal::Datetime(_) => BqType::Datetime,
        Literal::Time(_) => BqType::Time,
        Literal::Timestamp(_) => BqType::Timestamp,
        Literal::Json(_) => BqType::Json,
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Simple expression variants are handled directly in infer_expr_type"
)]
fn infer_complex_expr_type(expr: &Expr, row: &EvalRow, ctx: &ExecContext<'_>) -> BqType {
    match expr {
        Expr::Case {
            whens, else_result, ..
        } => {
            let mut tys: Vec<BqType> = whens
                .iter()
                .map(|w| infer_expr_type(&w.result, row, ctx))
                .collect();
            if let Some(el) = else_result {
                tys.push(infer_expr_type(el, row, ctx));
            }
            common_supertype(&tys).unwrap_or(BqType::UntypedNull)
        }
        Expr::ArrayLiteral {
            elem_type,
            elements,
        } => {
            let el = elem_type
                .as_ref()
                .and_then(|t| t.to_bq_type().ok())
                .unwrap_or_else(|| {
                    let tys: Vec<BqType> = elements
                        .iter()
                        .map(|e| infer_expr_type(e, row, ctx))
                        .collect();
                    common_supertype(&tys).unwrap_or(BqType::UntypedNull)
                });
            BqType::array(el).unwrap_or(BqType::UntypedNull)
        }
        Expr::StructLiteral {
            explicit_type,
            fields,
        } => infer_struct_literal_type(explicit_type.as_ref(), fields, row, ctx),
        Expr::FieldAccess { expr: inner, field } => {
            struct_field_ty(&infer_expr_type(inner, row, ctx), &field.value)
                .unwrap_or(BqType::UntypedNull)
        }
        Expr::Subscript { expr: inner, .. } => match infer_expr_type(inner, row, ctx) {
            BqType::Array(el) => *el,
            BqType::Json => BqType::Json,
            _ => BqType::UntypedNull,
        },
        Expr::RangeLiteral { elem_type, .. } => match elem_type.to_bq_type() {
            Ok(BqType::Date) => BqType::Range(RangeElementType::Date),
            Ok(BqType::Datetime) => BqType::Range(RangeElementType::Datetime),
            Ok(BqType::Timestamp) => BqType::Range(RangeElementType::Timestamp),
            Ok(BqType::Range(rt)) => BqType::Range(rt),
            _ => BqType::UntypedNull,
        },
        Expr::Function(call) => infer_fn_type(call, row, ctx),
        Expr::Subquery(q) => infer_query_columns(q, ctx)
            .ok()
            .and_then(|c| c.into_iter().next().map(|x| x.ty))
            .unwrap_or(BqType::UntypedNull),
        Expr::ArraySubquery(q) => {
            let el = infer_query_columns(q, ctx)
                .ok()
                .and_then(|c| c.into_iter().next().map(|x| x.ty))
                .unwrap_or(BqType::UntypedNull);
            BqType::array(el).unwrap_or(BqType::UntypedNull)
        }
        _ => BqType::UntypedNull,
    }
}

fn infer_struct_literal_type(
    explicit_type: Option<&crate::ast::DataTypeAst>,
    fields: &[crate::ast::StructFieldExpr],
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqType {
    explicit_type
        .and_then(|t| t.to_bq_type().ok())
        .unwrap_or_else(|| {
            let sf = fields
                .iter()
                .map(|f| {
                    let fname = f
                        .name
                        .as_ref()
                        .map(|n| n.value.clone())
                        .or_else(|| infer_expr_col_name(&f.expr));
                    let fty = infer_expr_type(&f.expr, row, ctx);
                    fname.map_or_else(
                        || StructField::anonymous(fty.clone()),
                        |n| StructField::named(n, fty.clone()),
                    )
                })
                .collect();
            BqType::Struct(sf)
        })
}

fn infer_binary_type(
    op: BinaryOp,
    left: &Expr,
    right: &Expr,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqType {
    match op {
        BinaryOp::And
        | BinaryOp::Or
        | BinaryOp::Eq
        | BinaryOp::Neq
        | BinaryOp::Lt
        | BinaryOp::Lte
        | BinaryOp::Gt
        | BinaryOp::Gte
        | BinaryOp::IsDistinctFrom
        | BinaryOp::IsNotDistinctFrom => BqType::Bool,
        BinaryOp::Div => {
            let lt = infer_expr_type(left, row, ctx);
            let rt = infer_expr_type(right, row, ctx);
            if matches!(lt, BqType::Int64) && matches!(rt, BqType::Int64) {
                BqType::Float64
            } else {
                common_supertype(&[lt, rt]).unwrap_or(BqType::Float64)
            }
        }
        BinaryOp::Add
        | BinaryOp::Sub
        | BinaryOp::Mul
        | BinaryOp::Concat
        | BinaryOp::BitAnd
        | BinaryOp::BitOr
        | BinaryOp::BitXor
        | BinaryOp::ShiftLeft
        | BinaryOp::ShiftRight => {
            let lt = infer_expr_type(left, row, ctx);
            let rt = infer_expr_type(right, row, ctx);
            common_supertype(&[lt, rt]).unwrap_or(BqType::UntypedNull)
        }
    }
}

fn infer_fn_type(call: &crate::ast::FunctionCall, row: &EvalRow, ctx: &ExecContext<'_>) -> BqType {
    let upper = call.name.normalized_parts().join(".").to_ascii_uppercase();
    match upper.as_str() {
        "COUNT" | "COUNTIF" | "ROW_NUMBER" | "RANK" | "DENSE_RANK" | "NTILE" | "LENGTH"
        | "CHAR_LENGTH" | "BYTE_LENGTH" | "STRPOS" | "INSTR" | "ARRAY_LENGTH" | "UNIX_SECONDS"
        | "UNIX_MILLIS" | "UNIX_MICROS" | "UNIX_DATE" | "DATE_DIFF" | "DATETIME_DIFF"
        | "TIMESTAMP_DIFF" | "TIME_DIFF" | "FARM_FINGERPRINT" | "RANGE_BUCKET" => BqType::Int64,
        "AVG" | "PERCENT_RANK" | "CUME_DIST" | "IEEE_DIVIDE" | "SAFE_DIVIDE" | "SQRT" | "POW"
        | "POWER" | "LN" | "LOG" | "LOG10" | "EXP" | "RAND" | "STDDEV" | "STDDEV_POP"
        | "STDDEV_SAMP" | "VARIANCE" | "VAR_POP" | "VAR_SAMP" | "ST_DISTANCE" => BqType::Float64,
        "LOGICAL_AND" | "LOGICAL_OR" | "STARTS_WITH" | "ENDS_WITH" | "REGEXP_CONTAINS"
        | "RANGE_CONTAINS" | "RANGE_OVERLAPS" | "ST_CONTAINS" | "ST_WITHIN" => BqType::Bool,
        "DATE"
        | "CURRENT_DATE"
        | "DATE_ADD"
        | "DATE_SUB"
        | "DATE_TRUNC"
        | "LAST_DAY"
        | "PARSE_DATE"
        | "DATE_FROM_UNIX_DATE" => BqType::Date,
        "DATETIME" | "CURRENT_DATETIME" | "DATETIME_ADD" | "DATETIME_SUB" | "DATETIME_TRUNC"
        | "PARSE_DATETIME" => BqType::Datetime,
        "TIME" | "CURRENT_TIME" | "TIME_ADD" | "TIME_SUB" | "TIME_TRUNC" | "PARSE_TIME" => {
            BqType::Time
        }
        "TIMESTAMP" | "CURRENT_TIMESTAMP" | "TIMESTAMP_ADD" | "TIMESTAMP_SUB"
        | "TIMESTAMP_TRUNC" | "PARSE_TIMESTAMP" | "TIMESTAMP_SECONDS" | "TIMESTAMP_MILLIS"
        | "TIMESTAMP_MICROS" => BqType::Timestamp,
        "MD5"
        | "SHA1"
        | "SHA256"
        | "SHA512"
        | "FROM_BASE64"
        | "FROM_HEX"
        | "NET.IP_FROM_STRING"
        | "NET.SAFE_IP_FROM_STRING" => BqType::Bytes,
        "PARSE_JSON" | "TO_JSON" | "JSON_OBJECT" | "JSON_ARRAY" | "JSON_SET" | "JSON_REMOVE" => {
            BqType::Json
        }
        "ST_GEOGPOINT" | "ST_GEOGFROMTEXT" => BqType::Geography,
        "SUM" | "MIN" | "MAX" | "ANY_VALUE" | "FIRST_VALUE" | "LAST_VALUE" | "NTH_VALUE"
        | "LAG" | "LEAD" => call
            .args
            .first()
            .map_or(BqType::UntypedNull, |a| infer_expr_type(&a.expr, row, ctx)),
        "IFNULL" | "NULLIF" | "IF" | "COALESCE" | "ARRAY_AGG" => {
            infer_polymorphic_fn_type(&upper, call, row, ctx)
        }
        _ => lookup_udf_return_type(&call.name, ctx).unwrap_or(BqType::String),
    }
}

fn infer_polymorphic_fn_type(
    upper: &str,
    call: &crate::ast::FunctionCall,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqType {
    match upper {
        "IFNULL" | "NULLIF" => {
            let tys: Vec<BqType> = call
                .args
                .iter()
                .take(2)
                .map(|a| infer_expr_type(&a.expr, row, ctx))
                .collect();
            common_supertype(&tys).unwrap_or(BqType::UntypedNull)
        }
        "IF" => {
            let t = call
                .args
                .get(1)
                .map_or(BqType::UntypedNull, |a| infer_expr_type(&a.expr, row, ctx));
            let f = call
                .args
                .get(2)
                .map_or(BqType::UntypedNull, |a| infer_expr_type(&a.expr, row, ctx));
            common_supertype(&[t, f]).unwrap_or(BqType::UntypedNull)
        }
        "COALESCE" => {
            let tys: Vec<BqType> = call
                .args
                .iter()
                .map(|a| infer_expr_type(&a.expr, row, ctx))
                .collect();
            common_supertype(&tys).unwrap_or(BqType::UntypedNull)
        }
        "ARRAY_AGG" => {
            let el = call
                .args
                .first()
                .map_or(BqType::UntypedNull, |a| infer_expr_type(&a.expr, row, ctx));
            BqType::array(el).unwrap_or(BqType::UntypedNull)
        }
        _ => BqType::UntypedNull,
    }
}
