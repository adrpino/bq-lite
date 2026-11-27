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
        Expr::Extract { part, .. } => match part.value.to_ascii_uppercase().as_str() {
            "DATE" => BqType::Date,
            "TIME" => BqType::Time,
            "DATETIME" => BqType::Datetime,
            _ => BqType::Int64,
        },
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
    if let Some(fixed) = infer_fixed_fn_type(&upper) {
        return fixed;
    }
    match upper.as_str() {
        "SUM" | "MIN" | "MAX" | "ANY_VALUE" | "FIRST_VALUE" | "LAST_VALUE" | "NTH_VALUE"
        | "LAG" | "LEAD" | "ABS" | "SAFE_NEGATE" | "ARRAY_REVERSE" | "ARRAY_SLICE"
        | "ARRAY_FILTER" | "RANGE_INTERSECT" => call
            .args
            .first()
            .map_or(BqType::UntypedNull, |a| infer_expr_type(&a.expr, row, ctx)),
        "ROUND" | "TRUNC" => {
            let arg0 = call
                .args
                .first()
                .map_or(BqType::Float64, |a| infer_expr_type(&a.expr, row, ctx));
            if matches!(arg0, BqType::Numeric | BqType::BigNumeric) {
                arg0
            } else {
                BqType::Float64
            }
        }
        "IFNULL" | "NULLIF" | "IF" | "COALESCE" | "ARRAY_AGG" | "APPROX_QUANTILES" | "GREATEST"
        | "LEAST" | "SAFE_ADD" | "SAFE_SUBTRACT" | "SAFE_MULTIPLY" | "DIV" | "MOD"
        | "ARRAY_CONCAT" | "ARRAY_FIRST" | "ARRAY_LAST" | "GENERATE_ARRAY" | "RANGE_START"
        | "RANGE_END" | "JSON_QUERY" | "JSON_EXTRACT" | "JSON_QUERY_ARRAY"
        | "JSON_EXTRACT_ARRAY" | "SPLIT" => infer_polymorphic_fn_type(&upper, call, row, ctx),
        _ => lookup_udf_return_type(&call.name, ctx).unwrap_or(BqType::String),
    }
}

fn infer_fixed_fn_type(upper: &str) -> Option<BqType> {
    if let Some(ty) = infer_fixed_scalar_type(upper) {
        return Some(ty);
    }
    match upper {
        "DATE"
        | "CURRENT_DATE"
        | "DATE_ADD"
        | "DATE_SUB"
        | "DATE_TRUNC"
        | "DATE_BUCKET"
        | "LAST_DAY"
        | "PARSE_DATE"
        | "DATE_FROM_UNIX_DATE" => Some(BqType::Date),
        "DATETIME" | "CURRENT_DATETIME" | "DATETIME_ADD" | "DATETIME_SUB" | "DATETIME_TRUNC"
        | "DATETIME_BUCKET" | "PARSE_DATETIME" => Some(BqType::Datetime),
        "TIME" | "CURRENT_TIME" | "TIME_ADD" | "TIME_SUB" | "TIME_TRUNC" | "PARSE_TIME" => {
            Some(BqType::Time)
        }
        "TIMESTAMP" | "CURRENT_TIMESTAMP" | "TIMESTAMP_ADD" | "TIMESTAMP_SUB"
        | "TIMESTAMP_TRUNC" | "TIMESTAMP_BUCKET" | "PARSE_TIMESTAMP" | "TIMESTAMP_SECONDS"
        | "TIMESTAMP_MILLIS" | "TIMESTAMP_MICROS" => Some(BqType::Timestamp),
        "MAKE_INTERVAL" | "JUSTIFY_DAYS" | "JUSTIFY_HOURS" | "JUSTIFY_INTERVAL" => {
            Some(BqType::Interval)
        }
        "MD5"
        | "SHA1"
        | "SHA256"
        | "SHA512"
        | "FROM_BASE64"
        | "FROM_HEX"
        | "CODE_POINTS_TO_BYTES"
        | "NET.IP_FROM_STRING"
        | "NET.SAFE_IP_FROM_STRING" => Some(BqType::Bytes),
        "PARSE_JSON" | "TO_JSON" | "JSON_OBJECT" | "JSON_ARRAY" | "JSON_SET" | "JSON_REMOVE" => {
            Some(BqType::Json)
        }
        "ST_GEOGPOINT" | "ST_GEOGFROMTEXT" => Some(BqType::Geography),
        "JSON_KEYS" | "JSON_VALUE_ARRAY" | "JSON_EXTRACT_STRING_ARRAY" | "REGEXP_EXTRACT_ALL" => {
            BqType::array(BqType::String).ok()
        }
        "TO_CODE_POINTS" => BqType::array(BqType::Int64).ok(),
        "GENERATE_DATE_ARRAY" => BqType::array(BqType::Date).ok(),
        _ => None,
    }
}

fn infer_fixed_scalar_type(upper: &str) -> Option<BqType> {
    match upper {
        "COUNT"
        | "COUNTIF"
        | "APPROX_COUNT_DISTINCT"
        | "BIT_AND"
        | "BIT_OR"
        | "BIT_XOR"
        | "GROUPING"
        | "ROW_NUMBER"
        | "RANK"
        | "DENSE_RANK"
        | "NTILE"
        | "LENGTH"
        | "CHAR_LENGTH"
        | "BYTE_LENGTH"
        | "OCTET_LENGTH"
        | "ASCII"
        | "UNICODE"
        | "STRPOS"
        | "INSTR"
        | "REGEXP_INSTR"
        | "ARRAY_LENGTH"
        | "UNIX_SECONDS"
        | "UNIX_MILLIS"
        | "UNIX_MICROS"
        | "UNIX_DATE"
        | "DATE_DIFF"
        | "DATETIME_DIFF"
        | "TIMESTAMP_DIFF"
        | "TIME_DIFF"
        | "FARM_FINGERPRINT"
        | "RANGE_BUCKET"
        | "SIGN"
        | "INT64" => Some(BqType::Int64),
        "AVG" | "PERCENT_RANK" | "CUME_DIST" | "IEEE_DIVIDE" | "SAFE_DIVIDE" | "SQRT" | "CBRT"
        | "CEIL" | "CEILING" | "FLOOR" | "POW" | "POWER" | "LN" | "LOG" | "LOG10" | "EXP"
        | "RAND" | "STDDEV" | "STDDEV_POP" | "STDDEV_SAMP" | "VARIANCE" | "VAR_POP"
        | "VAR_SAMP" | "CORR" | "COVAR_POP" | "COVAR_SAMP" | "ST_DISTANCE" | "FLOAT64" => {
            Some(BqType::Float64)
        }
        "LOGICAL_AND" | "LOGICAL_OR" | "STARTS_WITH" | "ENDS_WITH" | "REGEXP_CONTAINS"
        | "RANGE_CONTAINS" | "RANGE_OVERLAPS" | "ST_CONTAINS" | "ST_WITHIN" | "SEARCH" | "BOOL" => {
            Some(BqType::Bool)
        }
        _ => None,
    }
}

fn infer_polymorphic_fn_type(
    upper: &str,
    call: &crate::ast::FunctionCall,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqType {
    match upper {
        "IFNULL" | "NULLIF" | "DIV" | "MOD" | "SAFE_ADD" | "SAFE_SUBTRACT" | "SAFE_MULTIPLY" => {
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
        "COALESCE" | "GREATEST" | "LEAST" | "ARRAY_CONCAT" => {
            let tys: Vec<BqType> = call
                .args
                .iter()
                .map(|a| infer_expr_type(&a.expr, row, ctx))
                .collect();
            common_supertype(&tys).unwrap_or(BqType::UntypedNull)
        }
        "ARRAY_AGG" | "APPROX_QUANTILES" => {
            let el = call
                .args
                .first()
                .map_or(BqType::UntypedNull, |a| infer_expr_type(&a.expr, row, ctx));
            BqType::array(el).unwrap_or(BqType::UntypedNull)
        }
        "ARRAY_FIRST" | "ARRAY_LAST" => {
            let arg0 = call
                .args
                .first()
                .map_or(BqType::UntypedNull, |a| infer_expr_type(&a.expr, row, ctx));
            if let BqType::Array(el) = arg0 {
                *el
            } else {
                BqType::UntypedNull
            }
        }
        "GENERATE_ARRAY" => {
            let tys: Vec<BqType> = call
                .args
                .iter()
                .take(2)
                .map(|a| infer_expr_type(&a.expr, row, ctx))
                .collect();
            let el = common_supertype(&tys).unwrap_or(BqType::Int64);
            let elem = if matches!(el, BqType::UntypedNull) {
                BqType::Int64
            } else {
                el
            };
            BqType::array(elem).unwrap_or(BqType::UntypedNull)
        }
        "RANGE_START" | "RANGE_END" => {
            let arg0 = call
                .args
                .first()
                .map_or(BqType::UntypedNull, |a| infer_expr_type(&a.expr, row, ctx));
            if let BqType::Range(r) = arg0 {
                r.to_bq_type()
            } else {
                BqType::UntypedNull
            }
        }
        "JSON_QUERY" | "JSON_EXTRACT" | "JSON_QUERY_ARRAY" | "JSON_EXTRACT_ARRAY" | "SPLIT" => {
            infer_json_or_split_type(upper, call, row, ctx)
        }
        _ => BqType::UntypedNull,
    }
}

fn infer_json_or_split_type(
    upper: &str,
    call: &crate::ast::FunctionCall,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqType {
    let arg0 = call
        .args
        .first()
        .map_or(BqType::String, |a| infer_expr_type(&a.expr, row, ctx));
    match upper {
        "JSON_QUERY" | "JSON_EXTRACT" => {
            if matches!(arg0, BqType::Json) {
                BqType::Json
            } else {
                BqType::String
            }
        }
        "JSON_QUERY_ARRAY" | "JSON_EXTRACT_ARRAY" => {
            let elem = if matches!(arg0, BqType::Json) {
                BqType::Json
            } else {
                BqType::String
            };
            BqType::array(elem).unwrap_or(BqType::UntypedNull)
        }
        "SPLIT" => {
            let elem = if matches!(arg0, BqType::Bytes) {
                BqType::Bytes
            } else {
                BqType::String
            };
            BqType::array(elem).unwrap_or(BqType::UntypedNull)
        }
        _ => BqType::UntypedNull,
    }
}
