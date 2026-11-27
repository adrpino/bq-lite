use crate::ast::{
    Cte, Expr, GroupByClause, GroupByItem, Ident, NamedWindowDef, PipeOp, Query, ReplaceItem,
    Select, SelectAsKind, SelectItem, SetExpr, SetOpKind, SetQuantifier, WithClause,
};
use crate::error::{BqError, BqResult};

use super::cursor::Parser;
use super::expr::parse_expr;
use super::expr_postfix::{parse_order_by_expr, parse_window_spec};
use super::query_from::parse_from_list;
use super::token::TokenKind;

pub(super) fn parse_query(p: &mut Parser) -> BqResult<Query> {
    p.with_depth(parse_query_inner)
}

fn parse_query_inner(p: &mut Parser) -> BqResult<Query> {
    let with = if p.is_kw("WITH") {
        Some(parse_with_clause(p)?)
    } else {
        None
    };
    let body = parse_set_expr(p)?;
    let order_by = if p.eat_kw("ORDER") {
        p.expect_kw("BY")?;
        p.parse_comma_list(parse_order_by_expr)?
    } else {
        Vec::new()
    };
    let (limit, offset) = parse_limit_offset(p)?;
    let mut pipe_ops = Vec::new();
    while p.eat(&TokenKind::PipeGt) {
        pipe_ops.push(parse_pipe_op(p)?);
    }
    Ok(Query {
        with,
        body,
        order_by,
        limit,
        offset,
        pipe_ops,
    })
}

fn parse_with_clause(p: &mut Parser) -> BqResult<WithClause> {
    p.expect_kw("WITH")?;
    let recursive = p.eat_kw("RECURSIVE");
    let ctes = p.parse_comma_list(parse_cte)?;
    Ok(WithClause { recursive, ctes })
}

fn parse_cte(p: &mut Parser) -> BqResult<Cte> {
    let name = p.parse_ident()?;
    let columns = if p.eat(&TokenKind::LParen) {
        let cols = p.parse_comma_list(Parser::parse_ident)?;
        p.expect(&TokenKind::RParen, "')'")?;
        cols
    } else {
        Vec::new()
    };
    p.expect_kw("AS")?;
    p.expect(&TokenKind::LParen, "'('")?;
    let query = Box::new(parse_query(p)?);
    p.expect(&TokenKind::RParen, "')'")?;
    Ok(Cte {
        name,
        columns,
        query,
    })
}

fn parse_set_expr(p: &mut Parser) -> BqResult<SetExpr> {
    let mut left = parse_set_term(p)?;
    while p.is_kw("UNION") || p.is_kw("EXCEPT") {
        let op = if p.eat_kw("UNION") {
            SetOpKind::Union
        } else {
            p.expect_kw("EXCEPT")?;
            SetOpKind::Except
        };
        let quantifier = parse_set_quantifier(p)?;
        let by_name = parse_optional_by_name(p)?;
        let right = parse_set_term(p)?;
        left = SetExpr::SetOperation {
            op,
            quantifier,
            by_name,
            left: Box::new(left),
            right: Box::new(right),
        };
    }
    Ok(left)
}

fn parse_set_term(p: &mut Parser) -> BqResult<SetExpr> {
    let mut left = parse_set_factor(p)?;
    while p.eat_kw("INTERSECT") {
        let quantifier = parse_set_quantifier(p)?;
        let by_name = parse_optional_by_name(p)?;
        let right = parse_set_factor(p)?;
        left = SetExpr::SetOperation {
            op: SetOpKind::Intersect,
            quantifier,
            by_name,
            left: Box::new(left),
            right: Box::new(right),
        };
    }
    Ok(left)
}

fn parse_set_quantifier(p: &mut Parser) -> BqResult<SetQuantifier> {
    if p.eat_kw("ALL") {
        Ok(SetQuantifier::All)
    } else if p.eat_kw("DISTINCT") {
        Ok(SetQuantifier::Distinct)
    } else {
        Err(BqError::syntax(
            "Expected ALL or DISTINCT after set operator in GoogleSQL",
            p.current_span(),
        ))
    }
}

fn parse_optional_by_name(p: &mut Parser) -> BqResult<bool> {
    if p.eat_kw("BY") {
        p.expect_kw("NAME")?;
        Ok(true)
    } else if p.eat_kw("CORRESPONDING") {
        Ok(true)
    } else if p.is_kw("STRICT")
        && matches!(&p.peek_n(1).kind, TokenKind::Ident(w) if w.eq_ignore_ascii_case("CORRESPONDING"))
    {
        p.advance();
        p.advance();
        Ok(true)
    } else {
        Ok(false)
    }
}

fn parse_set_factor(p: &mut Parser) -> BqResult<SetExpr> {
    if p.eat(&TokenKind::LParen) {
        let inner = parse_query(p)?;
        p.expect(&TokenKind::RParen, "')'")?;
        return Ok(SetExpr::Query(Box::new(inner)));
    }
    if p.is_kw("SELECT") {
        return Ok(SetExpr::Select(Box::new(parse_select(p)?)));
    }
    if p.eat_kw("FROM") {
        let from = parse_from_list(p)?;
        return Ok(SetExpr::Select(Box::new(Select {
            distinct: false,
            as_kind: None,
            projection: vec![SelectItem::Wildcard {
                qualifier: None,
                except: Vec::new(),
                replace: Vec::new(),
            }],
            from,
            where_clause: None,
            group_by: None,
            having: None,
            qualify: None,
            window: Vec::new(),
        })));
    }
    Err(BqError::syntax(
        "Expected SELECT, FROM, or parenthesized query",
        p.current_span(),
    ))
}

fn parse_select(p: &mut Parser) -> BqResult<Select> {
    p.expect_kw("SELECT")?;
    let distinct = if p.eat_kw("DISTINCT") {
        true
    } else {
        p.eat_kw("ALL");
        false
    };
    let as_kind = if p.eat_kw("AS") {
        if p.eat_kw("STRUCT") {
            Some(SelectAsKind::Struct)
        } else if p.eat_kw("VALUE") {
            Some(SelectAsKind::Value)
        } else {
            return Err(BqError::syntax(
                "Expected STRUCT or VALUE after SELECT AS",
                p.current_span(),
            ));
        }
    } else {
        None
    };

    let projection = p.parse_comma_list(parse_select_item)?;
    let from = if p.eat_kw("FROM") {
        parse_from_list(p)?
    } else {
        Vec::new()
    };
    let where_clause = if p.eat_kw("WHERE") {
        Some(parse_expr(p)?)
    } else {
        None
    };
    let group_by = parse_optional_group_by(p)?;
    let having = if p.eat_kw("HAVING") {
        Some(parse_expr(p)?)
    } else {
        None
    };
    let mut qualify = None;
    let mut window = Vec::new();
    while p.is_kw("QUALIFY") || p.is_kw("WINDOW") {
        if p.eat_kw("QUALIFY") {
            qualify = Some(parse_expr(p)?);
        } else {
            window.extend(parse_optional_window_clause(p)?);
        }
    }

    Ok(Select {
        distinct,
        as_kind,
        projection,
        from,
        where_clause,
        group_by,
        having,
        qualify,
        window,
    })
}

pub(super) fn parse_select_item(p: &mut Parser) -> BqResult<SelectItem> {
    let expr = parse_expr(p)?;
    if matches!(expr, Expr::Star) {
        return parse_wildcard_modifiers(p, None);
    }
    if let Expr::QualifiedStar(q) = expr {
        return parse_wildcard_modifiers(p, Some(q));
    }
    let alias = parse_optional_alias(p)?;
    Ok(SelectItem::Expr {
        expr: Box::new(expr),
        alias,
    })
}

fn parse_wildcard_modifiers(
    p: &mut Parser,
    qualifier: Option<crate::ast::ObjectName>,
) -> BqResult<SelectItem> {
    let except = if p.eat_kw("EXCEPT") {
        p.expect(&TokenKind::LParen, "'('")?;
        let cols = p.parse_comma_list(Parser::parse_ident)?;
        p.expect(&TokenKind::RParen, "')'")?;
        cols
    } else {
        Vec::new()
    };
    let replace = if p.eat_kw("REPLACE") {
        p.expect(&TokenKind::LParen, "'('")?;
        let items = p.parse_comma_list(|p| {
            let expr = parse_expr(p)?;
            p.expect_kw("AS")?;
            let alias = p.parse_ident()?;
            Ok(ReplaceItem { expr, alias })
        })?;
        p.expect(&TokenKind::RParen, "')'")?;
        items
    } else {
        Vec::new()
    };
    Ok(SelectItem::Wildcard {
        qualifier,
        except,
        replace,
    })
}

pub(super) fn parse_optional_alias(p: &mut Parser) -> BqResult<Option<Ident>> {
    if p.eat_kw("AS") {
        return Ok(Some(p.parse_ident()?));
    }
    if matches!(
        p.peek().kind,
        TokenKind::Ident(ref s)
            if !super::token::is_reserved_keyword(s) && !is_clause_boundary_kw(s)
    ) || matches!(p.peek().kind, TokenKind::QuotedIdent(_))
    {
        return Ok(Some(p.parse_unreserved_ident()?));
    }
    Ok(None)
}

fn is_clause_boundary_kw(word: &str) -> bool {
    let upper = word.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "PIVOT"
            | "UNPIVOT"
            | "TABLESAMPLE"
            | "THEN"
            | "ELSEIF"
            | "DO"
            | "UNTIL"
            | "EXCEPTION"
            | "RETURN"
            | "MATCHED"
            | "OFFSET"
            | "OPTIONS"
            | "CLUSTER"
            | "PARTITION"
    )
}

fn parse_optional_group_by(p: &mut Parser) -> BqResult<Option<GroupByClause>> {
    if !p.eat_kw("GROUP") {
        return Ok(None);
    }
    p.expect_kw("BY")?;
    if p.eat_kw("ALL") {
        return Ok(Some(GroupByClause::All));
    }
    let items = p.parse_comma_list(parse_group_by_item)?;
    Ok(Some(GroupByClause::Items(items)))
}

fn parse_group_by_item(p: &mut Parser) -> BqResult<GroupByItem> {
    if p.eat_kw("ROLLUP") {
        p.expect(&TokenKind::LParen, "'('")?;
        let exprs = p.parse_comma_list(parse_expr)?;
        p.expect(&TokenKind::RParen, "')'")?;
        return Ok(GroupByItem::Rollup(exprs));
    }
    if p.eat_kw("CUBE") {
        p.expect(&TokenKind::LParen, "'('")?;
        let exprs = p.parse_comma_list(parse_expr)?;
        p.expect(&TokenKind::RParen, "')'")?;
        return Ok(GroupByItem::Cube(exprs));
    }
    if p.eat_kw("GROUPING") {
        p.expect_kw("SETS")?;
        p.expect(&TokenKind::LParen, "'('")?;
        let sets = p.parse_comma_list(|p| {
            if p.eat(&TokenKind::LParen) {
                if p.eat(&TokenKind::RParen) {
                    return Ok(Vec::new());
                }
                let inner = p.parse_comma_list(parse_expr)?;
                p.expect(&TokenKind::RParen, "')'")?;
                Ok(inner)
            } else {
                Ok(vec![parse_expr(p)?])
            }
        })?;
        p.expect(&TokenKind::RParen, "')'")?;
        return Ok(GroupByItem::GroupingSets(sets));
    }
    Ok(GroupByItem::Expr(Box::new(parse_expr(p)?)))
}

fn parse_optional_window_clause(p: &mut Parser) -> BqResult<Vec<NamedWindowDef>> {
    if !p.eat_kw("WINDOW") {
        return Ok(Vec::new());
    }
    p.parse_comma_list(|p| {
        let name = p.parse_ident()?;
        p.expect_kw("AS")?;
        let spec = parse_window_spec(p)?;
        Ok(NamedWindowDef { name, spec })
    })
}

fn parse_limit_offset(p: &mut Parser) -> BqResult<(Option<Expr>, Option<Expr>)> {
    if !p.eat_kw("LIMIT") {
        return Ok((None, None));
    }
    let limit = parse_expr(p)?;
    let offset = if p.eat_kw("OFFSET") {
        Some(parse_expr(p)?)
    } else {
        None
    };
    Ok((Some(limit), offset))
}

fn parse_pipe_op(p: &mut Parser) -> BqResult<PipeOp> {
    if p.eat_kw("WHERE") {
        return Ok(PipeOp::Where(Box::new(parse_expr(p)?)));
    }
    if p.eat_kw("SELECT") {
        return Ok(PipeOp::Select(p.parse_comma_list(parse_select_item)?));
    }
    if p.eat_kw("EXTEND") {
        return Ok(PipeOp::Extend(p.parse_comma_list(parse_select_item)?));
    }
    if p.eat_kw("SET") {
        let items = p.parse_comma_list(|p| {
            let col = p.parse_ident()?;
            p.expect(&TokenKind::Eq, "'='")?;
            let val = parse_expr(p)?;
            Ok((col, val))
        })?;
        return Ok(PipeOp::Set(items));
    }
    if p.eat_kw("DROP") {
        return Ok(PipeOp::Drop(p.parse_comma_list(Parser::parse_ident)?));
    }
    if p.eat_kw("RENAME") {
        let items = p.parse_comma_list(|p| {
            let old = p.parse_ident()?;
            p.eat_kw("AS");
            let new = p.parse_ident()?;
            Ok((old, new))
        })?;
        return Ok(PipeOp::Rename(items));
    }
    if p.eat_kw("AGGREGATE") {
        let aggs = p.parse_comma_list(parse_select_item)?;
        let group_by = if p.eat_kw("GROUP") {
            p.expect_kw("BY")?;
            p.parse_comma_list(parse_group_by_item)?
        } else {
            Vec::new()
        };
        return Ok(PipeOp::Aggregate { aggs, group_by });
    }
    if p.eat_kw("ORDER") {
        p.expect_kw("BY")?;
        return Ok(PipeOp::OrderBy(p.parse_comma_list(parse_order_by_expr)?));
    }
    if p.eat_kw("LIMIT") {
        let limit = Box::new(parse_expr(p)?);
        let offset = if p.eat_kw("OFFSET") {
            Some(Box::new(parse_expr(p)?))
        } else {
            None
        };
        return Ok(PipeOp::Limit { limit, offset });
    }
    if p.eat_kw("AS") {
        return Ok(PipeOp::As(p.parse_ident()?));
    }
    Err(BqError::syntax(
        "Unsupported pipe operator",
        p.current_span(),
    ))
}
