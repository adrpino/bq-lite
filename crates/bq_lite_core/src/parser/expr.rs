use crate::ast::{BinaryOp, Expr, InTarget, IsTest, UnaryOp};
use crate::error::{BqError, BqResult};

use super::cursor::Parser;
use super::expr_postfix::parse_postfix;
use super::query::parse_query;
use super::token::TokenKind;

/// Parses a `GoogleSQL` expression.
pub(super) fn parse_expr(p: &mut Parser) -> BqResult<Expr> {
    p.with_depth(parse_or)
}

fn parse_or(p: &mut Parser) -> BqResult<Expr> {
    let mut left = parse_and(p)?;
    while p.eat_kw("OR") {
        let right = parse_and(p)?;
        left = Expr::Binary {
            left: Box::new(left),
            op: BinaryOp::Or,
            right: Box::new(right),
        };
    }
    Ok(left)
}

fn parse_and(p: &mut Parser) -> BqResult<Expr> {
    let mut left = parse_not(p)?;
    while p.eat_kw("AND") {
        let right = parse_not(p)?;
        left = Expr::Binary {
            left: Box::new(left),
            op: BinaryOp::And,
            right: Box::new(right),
        };
    }
    Ok(left)
}

fn parse_not(p: &mut Parser) -> BqResult<Expr> {
    if p.eat_kw("NOT") {
        let inner = parse_not(p)?;
        return Ok(Expr::Unary {
            op: UnaryOp::Not,
            expr: Box::new(inner),
        });
    }
    parse_comparison(p)
}

fn parse_comparison(p: &mut Parser) -> BqResult<Expr> {
    let mut expr = parse_bitor(p)?;
    loop {
        if let Some(op) = try_eat_cmp_op(p) {
            let right = parse_bitor(p)?;
            expr = Expr::Binary {
                left: Box::new(expr),
                op,
                right: Box::new(right),
            };
            continue;
        }
        if p.eat_kw("IS") {
            expr = parse_is_rhs(p, expr)?;
            continue;
        }
        if let Some(next) = try_parse_negated_predicate(p, expr.clone())? {
            expr = next;
            continue;
        }
        return Ok(expr);
    }
}

fn try_eat_cmp_op(p: &mut Parser) -> Option<BinaryOp> {
    let op = match p.peek().kind {
        TokenKind::Eq => Some(BinaryOp::Eq),
        TokenKind::Neq => Some(BinaryOp::Neq),
        TokenKind::Lt => Some(BinaryOp::Lt),
        TokenKind::Lte => Some(BinaryOp::Lte),
        TokenKind::Gt => Some(BinaryOp::Gt),
        TokenKind::Gte => Some(BinaryOp::Gte),
        TokenKind::Ident(_)
        | TokenKind::QuotedIdent(_)
        | TokenKind::Number(_)
        | TokenKind::String(_)
        | TokenKind::Bytes(_)
        | TokenKind::Param(_)
        | TokenKind::PositionalParam(_)
        | TokenKind::SystemVar(_)
        | TokenKind::LParen
        | TokenKind::RParen
        | TokenKind::LBracket
        | TokenKind::RBracket
        | TokenKind::LBrace
        | TokenKind::RBrace
        | TokenKind::Comma
        | TokenKind::Dot
        | TokenKind::Semicolon
        | TokenKind::Colon
        | TokenKind::Plus
        | TokenKind::Minus
        | TokenKind::Star
        | TokenKind::Slash
        | TokenKind::Percent
        | TokenKind::Pipe
        | TokenKind::PipeGt
        | TokenKind::Concat
        | TokenKind::Amp
        | TokenKind::Caret
        | TokenKind::Tilde
        | TokenKind::ShiftLeft
        | TokenKind::ShiftRight
        | TokenKind::Arrow
        | TokenKind::FatArrow
        | TokenKind::Eof => None,
    };
    if op.is_some() {
        p.advance();
    }
    op
}

fn parse_is_rhs(p: &mut Parser, left: Expr) -> BqResult<Expr> {
    let negated = p.eat_kw("NOT");
    if p.eat_kw("DISTINCT") {
        p.expect_kw("FROM")?;
        let right = parse_bitor(p)?;
        return Ok(Expr::Binary {
            left: Box::new(left),
            op: if negated {
                BinaryOp::IsNotDistinctFrom
            } else {
                BinaryOp::IsDistinctFrom
            },
            right: Box::new(right),
        });
    }
    let test = if p.eat_kw("NULL") {
        if negated {
            IsTest::NotNull
        } else {
            IsTest::Null
        }
    } else if p.eat_kw("TRUE") {
        if negated {
            IsTest::NotTrue
        } else {
            IsTest::True
        }
    } else if p.eat_kw("FALSE") {
        if negated {
            IsTest::NotFalse
        } else {
            IsTest::False
        }
    } else if p.eat_kw("UNKNOWN") {
        if negated {
            IsTest::NotUnknown
        } else {
            IsTest::Unknown
        }
    } else {
        return Err(BqError::syntax(
            "Expected NULL, TRUE, FALSE, UNKNOWN, or DISTINCT FROM after IS",
            p.current_span(),
        ));
    };
    Ok(Expr::Is {
        expr: Box::new(left),
        test,
    })
}

fn try_parse_negated_predicate(p: &mut Parser, left: Expr) -> BqResult<Option<Expr>> {
    let negated = if p.is_kw("NOT")
        && (p.is_kw_n(1, "IN") || p.is_kw_n(1, "BETWEEN") || p.is_kw_n(1, "LIKE"))
    {
        p.advance();
        true
    } else {
        false
    };

    if p.eat_kw("IN") {
        return Ok(Some(parse_in_rhs(p, left, negated)?));
    }
    if p.eat_kw("BETWEEN") {
        let low = parse_bitor(p)?;
        p.expect_kw("AND")?;
        let high = parse_bitor(p)?;
        return Ok(Some(Expr::Between {
            expr: Box::new(left),
            negated,
            low: Box::new(low),
            high: Box::new(high),
        }));
    }
    if p.eat_kw("LIKE") {
        let pattern = parse_bitor(p)?;
        return Ok(Some(Expr::Like {
            expr: Box::new(left),
            negated,
            pattern: Box::new(pattern),
        }));
    }
    Ok(None)
}

fn parse_in_rhs(p: &mut Parser, left: Expr, negated: bool) -> BqResult<Expr> {
    if p.eat_kw("UNNEST") {
        p.expect(&TokenKind::LParen, "'('")?;
        let arr_expr = parse_expr(p)?;
        p.expect(&TokenKind::RParen, "')'")?;
        return Ok(Expr::In {
            expr: Box::new(left),
            negated,
            target: InTarget::Unnest(Box::new(arr_expr)),
        });
    }
    p.expect(&TokenKind::LParen, "'('")?;
    let target = if p.is_kw("SELECT") || p.is_kw("WITH") {
        InTarget::Subquery(Box::new(parse_query(p)?))
    } else if p.check(&TokenKind::RParen) {
        InTarget::List(Vec::new())
    } else {
        InTarget::List(p.parse_comma_list(parse_expr)?)
    };
    p.expect(&TokenKind::RParen, "')'")?;
    Ok(Expr::In {
        expr: Box::new(left),
        negated,
        target,
    })
}

fn parse_bitor(p: &mut Parser) -> BqResult<Expr> {
    let mut left = parse_bitxor(p)?;
    while p.eat(&TokenKind::Pipe) {
        let right = parse_bitxor(p)?;
        left = Expr::Binary {
            left: Box::new(left),
            op: BinaryOp::BitOr,
            right: Box::new(right),
        };
    }
    Ok(left)
}

fn parse_bitxor(p: &mut Parser) -> BqResult<Expr> {
    let mut left = parse_bitand(p)?;
    while p.eat(&TokenKind::Caret) {
        let right = parse_bitand(p)?;
        left = Expr::Binary {
            left: Box::new(left),
            op: BinaryOp::BitXor,
            right: Box::new(right),
        };
    }
    Ok(left)
}

fn parse_bitand(p: &mut Parser) -> BqResult<Expr> {
    let mut left = parse_shift(p)?;
    while p.eat(&TokenKind::Amp) {
        let right = parse_shift(p)?;
        left = Expr::Binary {
            left: Box::new(left),
            op: BinaryOp::BitAnd,
            right: Box::new(right),
        };
    }
    Ok(left)
}

fn parse_shift(p: &mut Parser) -> BqResult<Expr> {
    let mut left = parse_add(p)?;
    loop {
        let op = if p.eat(&TokenKind::ShiftLeft) {
            BinaryOp::ShiftLeft
        } else if p.eat(&TokenKind::ShiftRight) {
            BinaryOp::ShiftRight
        } else {
            return Ok(left);
        };
        let right = parse_add(p)?;
        left = Expr::Binary {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
    }
}

fn parse_add(p: &mut Parser) -> BqResult<Expr> {
    let mut left = parse_mul(p)?;
    loop {
        let op = if p.eat(&TokenKind::Plus) {
            BinaryOp::Add
        } else if p.eat(&TokenKind::Minus) {
            BinaryOp::Sub
        } else if p.eat(&TokenKind::Concat) {
            BinaryOp::Concat
        } else {
            return Ok(left);
        };
        let right = parse_mul(p)?;
        left = Expr::Binary {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
    }
}

fn parse_mul(p: &mut Parser) -> BqResult<Expr> {
    let mut left = parse_unary(p)?;
    loop {
        let op = if p.eat(&TokenKind::Star) {
            BinaryOp::Mul
        } else if p.eat(&TokenKind::Slash) {
            BinaryOp::Div
        } else {
            return Ok(left);
        };
        let right = parse_unary(p)?;
        left = Expr::Binary {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
    }
}

fn parse_unary(p: &mut Parser) -> BqResult<Expr> {
    if p.eat(&TokenKind::Minus) {
        if let TokenKind::Number(raw) = &p.peek().kind {
            if raw == "9223372036854775808" || raw.eq_ignore_ascii_case("0x8000000000000000") {
                p.advance();
                return Ok(Expr::Literal(crate::ast::Literal::Int(i64::MIN)));
            }
        }
        let inner = parse_unary(p)?;
        return Ok(Expr::Unary {
            op: UnaryOp::Minus,
            expr: Box::new(inner),
        });
    }
    let op = if p.eat(&TokenKind::Plus) {
        Some(UnaryOp::Plus)
    } else if p.eat(&TokenKind::Tilde) {
        Some(UnaryOp::BitNot)
    } else {
        None
    };
    if let Some(op) = op {
        let inner = parse_unary(p)?;
        return Ok(Expr::Unary {
            op,
            expr: Box::new(inner),
        });
    }
    parse_postfix(p)
}
