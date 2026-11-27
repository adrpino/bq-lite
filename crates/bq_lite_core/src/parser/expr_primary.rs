use crate::ast::{DataTypeAst, Expr, Ident, Literal, ObjectName, StructFieldExpr, WhenBranch};
use crate::error::{BqError, BqResult, SourceSpan};

use super::cursor::Parser;
use super::expr::parse_expr;
use super::expr_postfix::{parse_function_call, parse_paren_or_lambda};
use super::query::parse_query;
use super::token::TokenKind;
use super::types::parse_data_type;

pub(super) fn parse_primary(p: &mut Parser) -> BqResult<Expr> {
    let span = p.current_span();
    let tok_kind = p.peek().kind.clone();
    match tok_kind {
        TokenKind::Star => {
            p.advance();
            Ok(Expr::Star)
        }
        TokenKind::Number(s) => {
            p.advance();
            parse_number_literal(&s, span)
        }
        TokenKind::String(mut s) => {
            p.advance();
            while let TokenKind::String(next_s) = &p.peek().kind {
                s.push_str(next_s);
                p.advance();
            }
            Ok(Expr::Literal(Literal::String(s)))
        }
        TokenKind::Bytes(mut b) => {
            p.advance();
            while let TokenKind::Bytes(next_b) = &p.peek().kind {
                b.extend_from_slice(next_b);
                p.advance();
            }
            Ok(Expr::Literal(Literal::Bytes(b)))
        }
        TokenKind::Param(name) => {
            p.advance();
            Ok(Expr::Parameter(name))
        }
        TokenKind::PositionalParam(idx) => {
            p.advance();
            Ok(Expr::PositionalParam(idx))
        }
        TokenKind::SystemVar(name) => {
            p.advance();
            Ok(Expr::SystemVar(name))
        }
        TokenKind::LBracket => parse_array_literal(p, None),
        TokenKind::LParen => parse_paren_or_lambda(p),
        TokenKind::QuotedIdent(s) => {
            p.advance();
            Ok(Expr::Ident(Ident::quoted(s)))
        }
        TokenKind::Ident(word) => parse_ident_primary(p, word, span),
        TokenKind::RParen
        | TokenKind::RBracket
        | TokenKind::LBrace
        | TokenKind::RBrace
        | TokenKind::Comma
        | TokenKind::Dot
        | TokenKind::Semicolon
        | TokenKind::Colon
        | TokenKind::Plus
        | TokenKind::Minus
        | TokenKind::Slash
        | TokenKind::Percent
        | TokenKind::Pipe
        | TokenKind::PipeGt
        | TokenKind::Concat
        | TokenKind::Amp
        | TokenKind::Caret
        | TokenKind::Tilde
        | TokenKind::Eq
        | TokenKind::Neq
        | TokenKind::Lt
        | TokenKind::Lte
        | TokenKind::Gt
        | TokenKind::Gte
        | TokenKind::ShiftLeft
        | TokenKind::ShiftRight
        | TokenKind::Arrow
        | TokenKind::FatArrow
        | TokenKind::Eof => Err(BqError::syntax("Expected expression", span)),
    }
}

fn parse_number_literal(s: &str, span: SourceSpan) -> BqResult<Expr> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        let val = i64::from_str_radix(hex, 16)
            .map_err(|_| BqError::syntax(format!("Invalid hex integer '{s}'"), span))?;
        return Ok(Expr::Literal(Literal::Int(val)));
    }
    if s.contains('.') || s.contains('e') || s.contains('E') {
        let val = s
            .parse::<f64>()
            .map_err(|_| BqError::syntax(format!("Invalid float literal '{s}'"), span))?;
        return Ok(Expr::Literal(Literal::Float(val)));
    }
    let val = s
        .parse::<i64>()
        .map_err(|_| BqError::syntax(format!("Integer literal out of range: '{s}'"), span))?;
    Ok(Expr::Literal(Literal::Int(val)))
}

fn parse_ident_primary(p: &mut Parser, word: String, span: SourceSpan) -> BqResult<Expr> {
    let upper = word.to_ascii_uppercase();
    if let Some(lit) = try_parse_keyword_literal(p, &upper) {
        return Ok(lit);
    }
    if let Some(special) = try_parse_special_keyword_expr(p, &upper)? {
        return Ok(special);
    }
    if super::token::is_reserved_keyword(&word) {
        let callable_kw = matches!(
            upper.as_str(),
            "LEFT" | "RIGHT" | "GROUPING" | "IF" | "RANGE"
        ) && matches!(p.peek_n(1).kind, TokenKind::LParen);
        if !callable_kw {
            return Err(BqError::syntax(
                format!("Unexpected keyword '{word}' in expression"),
                span,
            ));
        }
    }
    p.advance();
    if p.check(&TokenKind::Arrow) {
        p.advance();
        let body = parse_expr(p)?;
        return Ok(Expr::Lambda {
            params: vec![Ident::unquoted(word)],
            body: Box::new(body),
        });
    }
    let _ = span;
    Ok(Expr::Ident(Ident::unquoted(word)))
}

fn try_parse_keyword_literal(p: &mut Parser, upper: &str) -> Option<Expr> {
    match upper {
        "NULL" => {
            p.advance();
            Some(Expr::Literal(Literal::Null))
        }
        "TRUE" => {
            p.advance();
            Some(Expr::Literal(Literal::Bool(true)))
        }
        "FALSE" => {
            p.advance();
            Some(Expr::Literal(Literal::Bool(false)))
        }
        "DATE" | "DATETIME" | "TIME" | "TIMESTAMP" | "NUMERIC" | "BIGNUMERIC" | "JSON" => {
            if matches!(p.peek_n(1).kind, TokenKind::String(_)) {
                p.advance();
                if let TokenKind::String(val) = p.advance().kind {
                    let lit = match upper {
                        "DATE" => Literal::Date(val),
                        "DATETIME" => Literal::Datetime(val),
                        "TIME" => Literal::Time(val),
                        "TIMESTAMP" => Literal::Timestamp(val),
                        "NUMERIC" => Literal::Numeric(val),
                        "BIGNUMERIC" => Literal::BigNumeric(val),
                        _ => Literal::Json(val),
                    };
                    return Some(Expr::Literal(lit));
                }
            }
            None
        }
        _ => None,
    }
}

fn try_parse_special_keyword_expr(p: &mut Parser, upper: &str) -> BqResult<Option<Expr>> {
    match upper {
        "CAST" => Ok(Some(parse_cast(p, false)?)),
        "SAFE_CAST" => Ok(Some(parse_cast(p, true)?)),
        "CASE" => Ok(Some(parse_case(p)?)),
        "EXTRACT" => Ok(Some(parse_extract(p)?)),
        "INTERVAL" => Ok(Some(parse_interval(p)?)),
        "EXISTS" => Ok(Some(parse_exists(p)?)),
        "ARRAY" => try_parse_array_expr(p),
        "STRUCT" => try_parse_struct_expr(p),
        "RANGE" if matches!(p.peek_n(1).kind, TokenKind::Lt) => Ok(Some(parse_range_literal(p)?)),
        "SAFE" if matches!(p.peek_n(1).kind, TokenKind::Dot) => Ok(Some(parse_safe_call(p)?)),
        "WITH" if matches!(p.peek_n(1).kind, TokenKind::LParen) => Ok(Some(parse_with_expr(p)?)),
        _ => Ok(None),
    }
}

fn parse_cast(p: &mut Parser, safe: bool) -> BqResult<Expr> {
    p.advance();
    p.expect(&TokenKind::LParen, "'('")?;
    let expr = parse_expr(p)?;
    p.expect_kw("AS")?;
    let ty = parse_data_type(p)?;
    let (format, time_zone) = if p.eat_kw("FORMAT") {
        let fmt_expr = parse_expr(p)?;
        let tz_expr = if p.eat_kw("AT") {
            p.expect_kw("TIME")?;
            p.expect_kw("ZONE")?;
            Some(Box::new(parse_expr(p)?))
        } else {
            None
        };
        (Some(Box::new(fmt_expr)), tz_expr)
    } else {
        (None, None)
    };
    p.expect(&TokenKind::RParen, "')'")?;
    Ok(Expr::Cast {
        expr: Box::new(expr),
        ty,
        safe,
        format,
        time_zone,
    })
}

fn parse_case(p: &mut Parser) -> BqResult<Expr> {
    p.advance();
    let operand = if p.is_kw("WHEN") {
        None
    } else {
        Some(Box::new(parse_expr(p)?))
    };
    let mut whens = Vec::new();
    while p.eat_kw("WHEN") {
        let condition = parse_expr(p)?;
        p.expect_kw("THEN")?;
        let result = parse_expr(p)?;
        whens.push(WhenBranch { condition, result });
    }
    let else_result = if p.eat_kw("ELSE") {
        Some(Box::new(parse_expr(p)?))
    } else {
        None
    };
    p.expect_kw("END")?;
    Ok(Expr::Case {
        operand,
        whens,
        else_result,
    })
}

fn parse_extract(p: &mut Parser) -> BqResult<Expr> {
    p.advance();
    p.expect(&TokenKind::LParen, "'('")?;
    let part = p.parse_ident()?;
    p.expect_kw("FROM")?;
    let expr = parse_expr(p)?;
    let time_zone = if p.eat_kw("AT") {
        p.expect_kw("TIME")?;
        p.expect_kw("ZONE")?;
        Some(Box::new(parse_expr(p)?))
    } else {
        None
    };
    p.expect(&TokenKind::RParen, "')'")?;
    Ok(Expr::Extract {
        part,
        expr: Box::new(expr),
        time_zone,
    })
}

fn parse_interval(p: &mut Parser) -> BqResult<Expr> {
    p.advance();
    let value = parse_expr(p)?;
    let unit = p.parse_ident()?;
    let end_unit = if p.eat_kw("TO") {
        Some(p.parse_ident()?)
    } else {
        None
    };
    Ok(Expr::Interval {
        value: Box::new(value),
        unit,
        end_unit,
    })
}

fn parse_exists(p: &mut Parser) -> BqResult<Expr> {
    p.advance();
    p.expect(&TokenKind::LParen, "'('")?;
    let q = parse_query(p)?;
    p.expect(&TokenKind::RParen, "')'")?;
    Ok(Expr::Exists(Box::new(q)))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only LParen, Lt, or LBracket start ARRAY<T>[...] / ARRAY(SELECT ...)"
)]
fn try_parse_array_expr(p: &mut Parser) -> BqResult<Option<Expr>> {
    match p.peek_n(1).kind {
        TokenKind::LParen => {
            p.advance();
            p.advance();
            let q = parse_query(p)?;
            p.expect(&TokenKind::RParen, "')'")?;
            Ok(Some(Expr::ArraySubquery(Box::new(q))))
        }
        TokenKind::Lt => {
            p.advance();
            p.expect(&TokenKind::Lt, "'<'")?;
            let elem_ty = parse_data_type(p)?;
            p.expect_gt_angle()?;
            Ok(Some(parse_array_literal(p, Some(elem_ty))?))
        }
        TokenKind::LBracket => {
            p.advance();
            Ok(Some(parse_array_literal(p, None)?))
        }
        _ => Ok(None),
    }
}

pub(super) fn parse_array_literal(
    p: &mut Parser,
    elem_type: Option<DataTypeAst>,
) -> BqResult<Expr> {
    p.expect(&TokenKind::LBracket, "'['")?;
    let elements = if p.check(&TokenKind::RBracket) {
        Vec::new()
    } else {
        p.parse_comma_list(parse_expr)?
    };
    p.expect(&TokenKind::RBracket, "']'")?;
    Ok(Expr::ArrayLiteral {
        elem_type,
        elements,
    })
}

fn try_parse_struct_expr(p: &mut Parser) -> BqResult<Option<Expr>> {
    let explicit_type = if matches!(p.peek_n(1).kind, TokenKind::Lt | TokenKind::Neq) {
        let ty = parse_data_type(p)?;
        Some(ty)
    } else if matches!(p.peek_n(1).kind, TokenKind::LParen) {
        p.advance();
        None
    } else {
        return Ok(None);
    };

    p.expect(&TokenKind::LParen, "'('")?;
    let fields = if p.check(&TokenKind::RParen) {
        Vec::new()
    } else {
        p.parse_comma_list(|p| {
            let expr = parse_expr(p)?;
            let name = if p.eat_kw("AS") {
                Some(p.parse_ident()?)
            } else {
                None
            };
            Ok(StructFieldExpr { name, expr })
        })?
    };
    p.expect(&TokenKind::RParen, "')'")?;
    Ok(Some(Expr::StructLiteral {
        explicit_type,
        fields,
    }))
}

fn parse_range_literal(p: &mut Parser) -> BqResult<Expr> {
    p.advance();
    if p.check(&TokenKind::LParen) {
        let ident = Ident::unquoted("RANGE");
        return parse_function_call(p, ObjectName::new(vec![ident]), false);
    }
    p.expect(&TokenKind::Lt, "'<'")?;
    let elem_type = parse_data_type(p)?;
    p.expect_gt_angle()?;
    if let TokenKind::String(s) = &p.peek().kind {
        let raw = s.clone();
        let span = p.current_span();
        p.advance();
        let inner = raw
            .trim()
            .strip_prefix('[')
            .and_then(|t| t.strip_suffix(')'))
            .ok_or_else(|| {
                BqError::syntax("RANGE literal string must have form '[start, end)'", span)
            })?;
        let (s_str, e_str) = inner
            .split_once(',')
            .ok_or_else(|| BqError::syntax("RANGE literal string missing ','", span))?;
        let to_bound = |part: &str| {
            let t = part.trim();
            if t.eq_ignore_ascii_case("UNBOUNDED") || t.eq_ignore_ascii_case("NULL") {
                Expr::Literal(Literal::Null)
            } else {
                Expr::Literal(Literal::String(t.to_owned()))
            }
        };
        return Ok(Expr::RangeLiteral {
            elem_type,
            start: Box::new(to_bound(s_str)),
            end: Box::new(to_bound(e_str)),
        });
    }
    p.expect(&TokenKind::LParen, "'('")?;
    let start = parse_expr(p)?;
    p.expect(&TokenKind::Comma, "','")?;
    let end = parse_expr(p)?;
    p.expect(&TokenKind::RParen, "')'")?;
    Ok(Expr::RangeLiteral {
        elem_type,
        start: Box::new(start),
        end: Box::new(end),
    })
}

fn parse_safe_call(p: &mut Parser) -> BqResult<Expr> {
    p.advance();
    p.expect(&TokenKind::Dot, "'.'")?;
    let mut parts = vec![p.parse_ident()?];
    while p.eat(&TokenKind::Dot) {
        parts.push(p.parse_ident()?);
    }
    parse_function_call(p, ObjectName::new(parts), true)
}

fn parse_with_expr(p: &mut Parser) -> BqResult<Expr> {
    p.advance();
    p.expect(&TokenKind::LParen, "'('")?;
    let mut bindings = Vec::new();
    loop {
        if matches!(&p.peek_n(1).kind, TokenKind::Ident(w) if w.eq_ignore_ascii_case("AS")) {
            let name = p.parse_ident()?;
            p.expect_kw("AS")?;
            let val = parse_expr(p)?;
            bindings.push((name, val));
            p.expect(&TokenKind::Comma, "','")?;
        } else {
            let body = parse_expr(p)?;
            p.expect(&TokenKind::RParen, "')'")?;
            return Ok(Expr::WithExpr {
                bindings,
                body: Box::new(body),
            });
        }
    }
}
