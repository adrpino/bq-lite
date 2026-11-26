use crate::ast::{
    ArrayAccessKind, Expr, FunctionArg, FunctionCall, HavingKind, Ident, NullHandling, ObjectName,
    OrderByExpr, WindowFrame, WindowFrameBound, WindowFrameUnit, WindowSpec,
};
use crate::error::{BqError, BqResult};

use super::cursor::Parser;
use super::expr::parse_expr;
use super::expr_primary::parse_primary;
use super::query::parse_query;
use super::token::TokenKind;

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Ident/CompoundIdent extend path; all other Expr variants become FieldAccess"
)]
pub(super) fn parse_postfix(p: &mut Parser) -> BqResult<Expr> {
    let mut expr = parse_primary(p)?;
    loop {
        if p.eat(&TokenKind::Dot) {
            if p.eat(&TokenKind::Star) {
                expr = match expr_to_object_name(expr) {
                    Some(name) => Expr::QualifiedStar(name),
                    None => {
                        return Err(BqError::syntax(
                            "Expected qualified identifier before '.*'",
                            p.current_span(),
                        ))
                    }
                };
                continue;
            }
            let field = p.parse_ident()?;
            expr = match expr {
                Expr::Ident(first) => Expr::CompoundIdent(vec![first, field]),
                Expr::CompoundIdent(mut parts) => {
                    parts.push(field);
                    Expr::CompoundIdent(parts)
                }
                other => Expr::FieldAccess {
                    expr: Box::new(other),
                    field,
                },
            };
            continue;
        }
        if p.check(&TokenKind::LBracket) {
            expr = parse_subscript(p, expr)?;
            continue;
        }
        if p.check(&TokenKind::LParen) {
            if let Some(name) = expr_to_object_name(expr.clone()) {
                expr = parse_function_call(p, name, false)?;
                continue;
            }
        }
        return Ok(expr);
    }
}

fn expr_to_object_name(expr: Expr) -> Option<ObjectName> {
    if let Expr::Ident(id) = expr {
        Some(ObjectName::new(vec![id]))
    } else if let Expr::CompoundIdent(parts) = expr {
        Some(ObjectName::new(parts))
    } else {
        None
    }
}

fn parse_subscript(p: &mut Parser, base: Expr) -> BqResult<Expr> {
    p.expect(&TokenKind::LBracket, "'['")?;
    let kind = if p.eat_kw("OFFSET") {
        ArrayAccessKind::Offset
    } else if p.eat_kw("SAFE_OFFSET") {
        ArrayAccessKind::SafeOffset
    } else if p.eat_kw("ORDINAL") {
        ArrayAccessKind::Ordinal
    } else if p.eat_kw("SAFE_ORDINAL") {
        ArrayAccessKind::SafeOrdinal
    } else {
        ArrayAccessKind::Raw
    };
    let index = if kind == ArrayAccessKind::Raw {
        parse_expr(p)?
    } else {
        p.expect(&TokenKind::LParen, "'('")?;
        let idx = parse_expr(p)?;
        p.expect(&TokenKind::RParen, "')'")?;
        idx
    };
    p.expect(&TokenKind::RBracket, "']'")?;
    Ok(Expr::Subscript {
        expr: Box::new(base),
        kind,
        index: Box::new(index),
    })
}

pub(super) fn parse_function_call(
    p: &mut Parser,
    name: ObjectName,
    safe_prefix: bool,
) -> BqResult<Expr> {
    p.expect(&TokenKind::LParen, "'('")?;
    let distinct = p.eat_kw("DISTINCT");
    let args = if p.check(&TokenKind::RParen) {
        Vec::new()
    } else {
        p.parse_comma_list(parse_function_arg)?
    };
    let null_handling = parse_null_handling(p)?;
    let order_by = if p.eat_kw("ORDER") {
        p.expect_kw("BY")?;
        p.parse_comma_list(parse_order_by_expr)?
    } else {
        Vec::new()
    };
    let limit = if p.eat_kw("LIMIT") {
        Some(Box::new(parse_expr(p)?))
    } else {
        None
    };
    let having = parse_agg_having(p)?;
    p.expect(&TokenKind::RParen, "')'")?;
    let null_handling = if null_handling.is_none() {
        parse_null_handling(p)?
    } else {
        null_handling
    };
    let over = if p.eat_kw("OVER") {
        Some(parse_window_spec(p)?)
    } else {
        None
    };
    Ok(Expr::Function(Box::new(FunctionCall {
        name,
        safe_prefix,
        distinct,
        args,
        null_handling,
        order_by,
        limit,
        having,
        over,
    })))
}

pub(super) fn parse_function_arg(p: &mut Parser) -> BqResult<FunctionArg> {
    let is_named = matches!(
        p.peek().kind,
        TokenKind::Ident(_) | TokenKind::QuotedIdent(_)
    ) && matches!(p.peek_n(1).kind, TokenKind::FatArrow);
    if is_named {
        let name = p.parse_ident()?;
        p.advance();
        let expr = parse_expr(p)?;
        return Ok(FunctionArg {
            name: Some(name),
            expr,
        });
    }
    let expr = parse_expr(p)?;
    Ok(FunctionArg { name: None, expr })
}

fn parse_null_handling(p: &mut Parser) -> BqResult<Option<NullHandling>> {
    if p.eat_kw("IGNORE") {
        p.expect_kw("NULLS")?;
        Ok(Some(NullHandling::IgnoreNulls))
    } else if p.eat_kw("RESPECT") {
        p.expect_kw("NULLS")?;
        Ok(Some(NullHandling::RespectNulls))
    } else {
        Ok(None)
    }
}

fn parse_agg_having(p: &mut Parser) -> BqResult<Option<(HavingKind, Box<Expr>)>> {
    if !p.eat_kw("HAVING") {
        return Ok(None);
    }
    let kind = if p.eat_kw("MAX") {
        HavingKind::Max
    } else if p.eat_kw("MIN") {
        HavingKind::Min
    } else {
        return Err(BqError::syntax(
            "Expected MAX or MIN after HAVING in aggregate",
            p.current_span(),
        ));
    };
    let expr = parse_expr(p)?;
    Ok(Some((kind, Box::new(expr))))
}

pub(super) fn parse_paren_or_lambda(p: &mut Parser) -> BqResult<Expr> {
    p.expect(&TokenKind::LParen, "'('")?;
    if p.is_kw("SELECT") || p.is_kw("WITH") {
        let q = parse_query(p)?;
        p.expect(&TokenKind::RParen, "')'")?;
        return Ok(Expr::Subquery(Box::new(q)));
    }
    let first = parse_expr(p)?;
    if p.eat(&TokenKind::Comma) {
        let mut params = vec![expr_to_ident(first, p)?];
        let rest = p.parse_comma_list(Parser::parse_ident)?;
        params.extend(rest);
        p.expect(&TokenKind::RParen, "')'")?;
        p.expect(&TokenKind::Arrow, "'->'")?;
        let body = parse_expr(p)?;
        return Ok(Expr::Lambda {
            params,
            body: Box::new(body),
        });
    }
    p.expect(&TokenKind::RParen, "')'")?;
    if p.eat(&TokenKind::Arrow) {
        let param = expr_to_ident(first, p)?;
        let body = parse_expr(p)?;
        return Ok(Expr::Lambda {
            params: vec![param],
            body: Box::new(body),
        });
    }
    Ok(Expr::Grouped(Box::new(first)))
}

fn expr_to_ident(expr: Expr, p: &Parser) -> BqResult<Ident> {
    if let Expr::Ident(id) = expr {
        Ok(id)
    } else {
        Err(BqError::syntax(
            "Lambda parameters must be identifiers",
            p.current_span(),
        ))
    }
}

pub(super) fn parse_order_by_expr(p: &mut Parser) -> BqResult<OrderByExpr> {
    let expr = parse_expr(p)?;
    let asc = if p.eat_kw("ASC") {
        Some(true)
    } else if p.eat_kw("DESC") {
        Some(false)
    } else {
        None
    };
    let nulls_first = if p.eat_kw("NULLS") {
        if p.eat_kw("FIRST") {
            Some(true)
        } else if p.eat_kw("LAST") {
            Some(false)
        } else {
            return Err(BqError::syntax(
                "Expected FIRST or LAST after NULLS",
                p.current_span(),
            ));
        }
    } else {
        None
    };
    Ok(OrderByExpr {
        expr,
        asc,
        nulls_first,
    })
}

pub(super) fn parse_window_spec(p: &mut Parser) -> BqResult<WindowSpec> {
    if !p.check(&TokenKind::LParen) {
        let name = p.parse_ident()?;
        return Ok(WindowSpec {
            window_name: Some(name),
            partition_by: Vec::new(),
            order_by: Vec::new(),
            frame: None,
        });
    }
    p.advance();
    let window_name = if !p.is_kw("PARTITION")
        && !p.is_kw("ORDER")
        && !p.is_kw("ROWS")
        && !p.is_kw("RANGE")
        && !p.check(&TokenKind::RParen)
    {
        Some(p.parse_ident()?)
    } else {
        None
    };
    let partition_by = if p.eat_kw("PARTITION") {
        p.expect_kw("BY")?;
        p.parse_comma_list(parse_expr)?
    } else {
        Vec::new()
    };
    let order_by = if p.eat_kw("ORDER") {
        p.expect_kw("BY")?;
        p.parse_comma_list(parse_order_by_expr)?
    } else {
        Vec::new()
    };
    let frame = parse_optional_window_frame(p)?;
    p.expect(&TokenKind::RParen, "')'")?;
    Ok(WindowSpec {
        window_name,
        partition_by,
        order_by,
        frame,
    })
}

fn parse_optional_window_frame(p: &mut Parser) -> BqResult<Option<WindowFrame>> {
    let unit = if p.eat_kw("ROWS") {
        WindowFrameUnit::Rows
    } else if p.eat_kw("RANGE") {
        WindowFrameUnit::Range
    } else {
        return Ok(None);
    };
    if p.eat_kw("BETWEEN") {
        let start = parse_frame_bound(p)?;
        p.expect_kw("AND")?;
        let end = parse_frame_bound(p)?;
        Ok(Some(WindowFrame {
            unit,
            start,
            end: Some(end),
        }))
    } else {
        let start = parse_frame_bound(p)?;
        Ok(Some(WindowFrame {
            unit,
            start,
            end: None,
        }))
    }
}

fn parse_frame_bound(p: &mut Parser) -> BqResult<WindowFrameBound> {
    if p.eat_kw("CURRENT") {
        p.expect_kw("ROW")?;
        return Ok(WindowFrameBound::CurrentRow);
    }
    let offset = if p.eat_kw("UNBOUNDED") {
        None
    } else {
        Some(Box::new(parse_expr(p)?))
    };
    if p.eat_kw("PRECEDING") {
        Ok(WindowFrameBound::Preceding(offset))
    } else if p.eat_kw("FOLLOWING") {
        Ok(WindowFrameBound::Following(offset))
    } else {
        Err(BqError::syntax(
            "Expected PRECEDING or FOLLOWING in window frame",
            p.current_span(),
        ))
    }
}
