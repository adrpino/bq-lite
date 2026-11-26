use crate::ast::{DataTypeAst, StructFieldAst};
use crate::error::{BqError, BqResult};

use super::cursor::Parser;
use super::token::TokenKind;

pub(super) fn parse_data_type(p: &mut Parser) -> BqResult<DataTypeAst> {
    p.with_depth(parse_data_type_inner)
}

fn parse_data_type_inner(p: &mut Parser) -> BqResult<DataTypeAst> {
    let span = p.current_span();
    let ident = p.parse_ident()?;
    let upper = ident.value.to_ascii_uppercase();
    match upper.as_str() {
        "BOOL" | "BOOLEAN" => Ok(DataTypeAst::Bool),
        "INT64" | "INT" | "SMALLINT" | "INTEGER" | "BIGINT" | "TINYINT" | "BYTEINT" => {
            Ok(DataTypeAst::Int64)
        }
        "FLOAT64" => Ok(DataTypeAst::Float64),
        "NUMERIC" | "DECIMAL" => parse_precision_scale(p, false),
        "BIGNUMERIC" | "BIGDECIMAL" => parse_precision_scale(p, true),
        "STRING" => Ok(DataTypeAst::String {
            max_len: parse_optional_len(p)?,
        }),
        "BYTES" => Ok(DataTypeAst::Bytes {
            max_len: parse_optional_len(p)?,
        }),
        "DATE" => Ok(DataTypeAst::Date),
        "DATETIME" => Ok(DataTypeAst::Datetime),
        "TIME" => Ok(DataTypeAst::Time),
        "TIMESTAMP" => Ok(DataTypeAst::Timestamp),
        "INTERVAL" => Ok(DataTypeAst::Interval),
        "JSON" => Ok(DataTypeAst::Json),
        "GEOGRAPHY" => Ok(DataTypeAst::Geography),
        "RANGE" => parse_range_type(p),
        "ARRAY" => parse_array_type(p),
        "STRUCT" => parse_struct_type(p),
        _ => Err(BqError::syntax(
            format!("Unknown data type: '{}'", ident.value),
            span,
        )),
    }
}

fn parse_optional_len(p: &mut Parser) -> BqResult<Option<u64>> {
    if !p.eat(&TokenKind::LParen) {
        return Ok(None);
    }
    let len = parse_u64_token(p)?;
    p.expect(&TokenKind::RParen, "')'")?;
    Ok(Some(len))
}

fn parse_precision_scale(p: &mut Parser, is_big: bool) -> BqResult<DataTypeAst> {
    let (precision, scale) = if p.eat(&TokenKind::LParen) {
        let prec = parse_u32_token(p)?;
        let sc = if p.eat(&TokenKind::Comma) {
            Some(parse_u32_token(p)?)
        } else {
            None
        };
        p.expect(&TokenKind::RParen, "')'")?;
        (Some(prec), sc)
    } else {
        (None, None)
    };
    Ok(if is_big {
        DataTypeAst::BigNumeric { precision, scale }
    } else {
        DataTypeAst::Numeric { precision, scale }
    })
}

fn parse_range_type(p: &mut Parser) -> BqResult<DataTypeAst> {
    p.expect(&TokenKind::Lt, "'<'")?;
    let elem = parse_data_type(p)?;
    p.expect_gt_angle()?;
    Ok(DataTypeAst::Range(Box::new(elem)))
}

fn parse_array_type(p: &mut Parser) -> BqResult<DataTypeAst> {
    p.expect(&TokenKind::Lt, "'<'")?;
    let elem = parse_data_type(p)?;
    p.expect_gt_angle()?;
    Ok(DataTypeAst::Array(Box::new(elem)))
}

fn parse_struct_type(p: &mut Parser) -> BqResult<DataTypeAst> {
    p.expect(&TokenKind::Lt, "'<'")?;
    if p.at_gt_angle() {
        p.expect_gt_angle()?;
        return Ok(DataTypeAst::Struct(Vec::new()));
    }
    let mut fields = Vec::new();
    loop {
        fields.push(parse_struct_field(p)?);
        if !p.eat(&TokenKind::Comma) {
            break;
        }
    }
    p.expect_gt_angle()?;
    Ok(DataTypeAst::Struct(fields))
}

fn parse_struct_field(p: &mut Parser) -> BqResult<StructFieldAst> {
    let is_named = matches!(
        p.peek_n(1).kind,
        TokenKind::Ident(_) | TokenKind::QuotedIdent(_)
    );
    if is_named {
        let name = p.parse_ident()?;
        let ty = parse_data_type(p)?;
        Ok(StructFieldAst {
            name: Some(name),
            ty,
        })
    } else {
        let ty = parse_data_type(p)?;
        Ok(StructFieldAst { name: None, ty })
    }
}

fn parse_u32_token(p: &mut Parser) -> BqResult<u32> {
    let span = p.current_span();
    if let TokenKind::Number(s) = p.advance().kind {
        return s
            .parse::<u32>()
            .map_err(|_| BqError::syntax(format!("Invalid integer '{s}'"), span));
    }
    Err(BqError::syntax("Expected integer literal", span))
}

fn parse_u64_token(p: &mut Parser) -> BqResult<u64> {
    let span = p.current_span();
    if let TokenKind::Number(s) = p.advance().kind {
        return s
            .parse::<u64>()
            .map_err(|_| BqError::syntax(format!("Invalid integer '{s}'"), span));
    }
    Err(BqError::syntax("Expected integer literal", span))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::lexer::tokenize;

    #[test]
    fn parses_nested_struct_array_with_double_gt() {
        let toks = tokenize("ARRAY<STRUCT<id INT64, tags ARRAY<STRING>>>").unwrap();
        let mut p = Parser::new(toks);
        let ast = parse_data_type(&mut p).unwrap();
        assert!(p.at_eof());
        let bq = ast.to_bq_type().unwrap();
        assert_eq!(
            bq.to_string(),
            "ARRAY<STRUCT<id INT64, tags ARRAY<STRING>>>"
        );
    }
}
