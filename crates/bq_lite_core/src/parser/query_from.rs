use crate::ast::{
    Expr, FunctionArg, Ident, Join, JoinConstraint, JoinKind, PivotSpec, Query, Select, SelectItem,
    SetExpr, TableFactor, TableWithJoins, UnpivotSpec,
};
use crate::error::{BqError, BqResult};

use super::cursor::Parser;
use super::expr::parse_expr;
use super::query::{parse_optional_alias, parse_query};
use super::token::TokenKind;

pub(super) fn parse_from_list(p: &mut Parser) -> BqResult<Vec<TableWithJoins>> {
    let mut list = vec![parse_table_with_joins(p)?];
    while p.eat(&TokenKind::Comma) {
        list.push(parse_table_with_joins(p)?);
    }
    Ok(list)
}

pub(super) fn parse_table_with_joins(p: &mut Parser) -> BqResult<TableWithJoins> {
    let relation = parse_table_factor(p)?;
    let mut joins = Vec::new();
    while let Some(kind) = try_eat_join_kind(p)? {
        let right = parse_table_factor(p)?;
        let constraint = parse_join_constraint(p, kind)?;
        joins.push(Join {
            kind,
            relation: right,
            constraint,
        });
    }
    Ok(TableWithJoins { relation, joins })
}

pub(super) fn try_eat_join_kind(p: &mut Parser) -> BqResult<Option<JoinKind>> {
    if p.eat_kw("JOIN") {
        return Ok(Some(JoinKind::Inner));
    }
    if p.eat_kw("INNER") {
        p.expect_kw("JOIN")?;
        return Ok(Some(JoinKind::Inner));
    }
    if p.eat_kw("CROSS") {
        p.expect_kw("JOIN")?;
        return Ok(Some(JoinKind::Cross));
    }
    if p.eat_kw("LEFT") {
        p.eat_kw("OUTER");
        p.expect_kw("JOIN")?;
        return Ok(Some(JoinKind::Left));
    }
    if p.eat_kw("RIGHT") {
        p.eat_kw("OUTER");
        p.expect_kw("JOIN")?;
        return Ok(Some(JoinKind::Right));
    }
    if p.eat_kw("FULL") {
        p.eat_kw("OUTER");
        p.expect_kw("JOIN")?;
        return Ok(Some(JoinKind::Full));
    }
    Ok(None)
}

pub(super) fn parse_join_constraint(p: &mut Parser, kind: JoinKind) -> BqResult<JoinConstraint> {
    if kind == JoinKind::Cross {
        return Ok(JoinConstraint::None);
    }
    if p.eat_kw("ON") {
        return Ok(JoinConstraint::On(Box::new(parse_expr(p)?)));
    }
    if p.eat_kw("USING") {
        p.expect(&TokenKind::LParen, "'('")?;
        let cols = p.parse_comma_list(Parser::parse_ident)?;
        p.expect(&TokenKind::RParen, "')'")?;
        return Ok(JoinConstraint::Using(cols));
    }
    Ok(JoinConstraint::None)
}

pub(super) fn parse_table_factor(p: &mut Parser) -> BqResult<TableFactor> {
    let mut factor = parse_base_table_factor(p)?;
    loop {
        if p.eat_kw("PIVOT") {
            let spec = parse_pivot_spec(p)?;
            let alias = parse_optional_alias(p)?;
            factor = TableFactor::Pivot {
                source: Box::new(factor),
                spec,
                alias,
            };
            continue;
        }
        if p.eat_kw("UNPIVOT") {
            let spec = parse_unpivot_spec(p)?;
            let alias = parse_optional_alias(p)?;
            factor = TableFactor::Unpivot {
                source: Box::new(factor),
                spec,
                alias,
            };
            continue;
        }
        if p.eat_kw("TABLESAMPLE") {
            let method = p.parse_ident()?.value;
            p.expect(&TokenKind::LParen, "'('")?;
            let amount = parse_expr(p)?;
            let unit = p.parse_ident()?.value.to_ascii_uppercase();
            let is_percent = match unit.as_str() {
                "PERCENT" => true,
                "ROWS" => false,
                _ => {
                    return Err(BqError::syntax(
                        "Expected PERCENT or ROWS in TABLESAMPLE",
                        p.current_span(),
                    ))
                }
            };
            p.expect(&TokenKind::RParen, "')'")?;
            if p.eat_kw("REPEATABLE") {
                p.expect(&TokenKind::LParen, "'('")?;
                let _seed = parse_expr(p)?;
                p.expect(&TokenKind::RParen, "')'")?;
            }
            factor = TableFactor::TableSample {
                source: Box::new(factor),
                method,
                amount: Box::new(amount),
                is_percent,
            };
            continue;
        }
        return Ok(factor);
    }
}

fn parse_base_table_factor(p: &mut Parser) -> BqResult<TableFactor> {
    if p.eat_kw("UNNEST") {
        return parse_unnest_factor(p);
    }
    if p.eat(&TokenKind::LParen) {
        if p.is_kw("SELECT") || p.is_kw("WITH") || p.is_kw("FROM") {
            let query = Box::new(parse_query(p)?);
            p.expect(&TokenKind::RParen, "')'")?;
            let alias = parse_optional_alias(p)?;
            return Ok(TableFactor::Derived { query, alias });
        }
        let saved = p.save_pos();
        if let Ok(query) = parse_query(p) {
            if p.eat(&TokenKind::RParen) {
                let alias = parse_optional_alias(p)?;
                return Ok(TableFactor::Derived {
                    query: Box::new(query),
                    alias,
                });
            }
        }
        p.restore_pos(saved);
        let twj = parse_table_with_joins(p)?;
        p.expect(&TokenKind::RParen, "')'")?;
        return Ok(TableFactor::NestedJoin(Box::new(twj)));
    }
    let name = p.parse_table_object_name()?;
    if p.eat(&TokenKind::LParen) {
        let args = if p.check(&TokenKind::RParen) {
            Vec::new()
        } else {
            p.parse_comma_list(parse_tvf_arg)?
        };
        p.expect(&TokenKind::RParen, "')'")?;
        let alias = parse_optional_alias(p)?;
        return Ok(TableFactor::TableFunction { name, args, alias });
    }
    let system_time = parse_optional_system_time(p)?;
    let tablesample_percent = parse_optional_tablesample(p)?;
    let alias = parse_optional_alias(p)?;
    if p.eat_kw("WITH") {
        p.expect_kw("OFFSET")?;
        let off_alias = parse_optional_alias(p)?;
        let expr = if name.parts.len() == 1 {
            name.parts
                .into_iter()
                .next()
                .map_or(Expr::Star, Expr::Ident)
        } else {
            Expr::CompoundIdent(name.parts)
        };
        return Ok(TableFactor::Unnest {
            expr,
            alias,
            with_offset: Some(off_alias),
        });
    }
    let system_time = if system_time.is_none() {
        parse_optional_system_time(p)?
    } else {
        system_time
    };
    let tablesample_percent = if tablesample_percent.is_none() {
        parse_optional_tablesample(p)?
    } else {
        tablesample_percent
    };
    Ok(TableFactor::Table {
        name,
        alias,
        system_time,
        tablesample_percent,
    })
}

fn parse_tvf_arg(p: &mut Parser) -> BqResult<FunctionArg> {
    let is_named = matches!(
        p.peek().kind,
        TokenKind::Ident(_) | TokenKind::QuotedIdent(_)
    ) && matches!(p.peek_n(1).kind, TokenKind::FatArrow);
    let name = if is_named {
        let id = p.parse_ident()?;
        p.advance();
        Some(id)
    } else {
        None
    };
    if p.eat_kw("TABLE") {
        let tbl_name = p.parse_table_object_name()?;
        let query = Query {
            with: None,
            body: SetExpr::Select(Box::new(Select {
                distinct: false,
                as_kind: None,
                projection: vec![SelectItem::Wildcard {
                    qualifier: None,
                    except: Vec::new(),
                    replace: Vec::new(),
                }],
                from: vec![TableWithJoins {
                    relation: TableFactor::Table {
                        name: tbl_name,
                        alias: None,
                        system_time: None,
                        tablesample_percent: None,
                    },
                    joins: Vec::new(),
                }],
                where_clause: None,
                group_by: None,
                having: None,
                qualify: None,
                window: Vec::new(),
            })),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            pipe_ops: Vec::new(),
        };
        return Ok(FunctionArg {
            name,
            expr: Expr::Subquery(Box::new(query)),
        });
    }
    let expr = parse_expr(p)?;
    Ok(FunctionArg { name, expr })
}

fn parse_optional_tablesample(p: &mut Parser) -> BqResult<Option<Box<Expr>>> {
    if !p.eat_kw("TABLESAMPLE") {
        return Ok(None);
    }
    p.expect_kw("SYSTEM")?;
    p.expect(&TokenKind::LParen, "'('")?;
    let pct = parse_expr(p)?;
    p.expect_kw("PERCENT")?;
    p.expect(&TokenKind::RParen, "')'")?;
    Ok(Some(Box::new(pct)))
}

fn parse_unnest_factor(p: &mut Parser) -> BqResult<TableFactor> {
    p.expect(&TokenKind::LParen, "'('")?;
    let expr = parse_expr(p)?;
    p.expect(&TokenKind::RParen, "')'")?;
    let alias = parse_optional_alias(p)?;
    let with_offset = if p.eat_kw("WITH") {
        p.expect_kw("OFFSET")?;
        let offset_alias = parse_optional_alias(p)?;
        Some(offset_alias)
    } else {
        None
    };
    Ok(TableFactor::Unnest {
        expr,
        alias,
        with_offset,
    })
}

pub(super) fn parse_optional_system_time(p: &mut Parser) -> BqResult<Option<Expr>> {
    if !p.is_kw("FOR") || !p.is_kw_n(1, "SYSTEM_TIME") {
        return Ok(None);
    }
    p.expect_kw("FOR")?;
    p.expect_kw("SYSTEM_TIME")?;
    p.expect_kw("AS")?;
    p.expect_kw("OF")?;
    Ok(Some(parse_expr(p)?))
}

pub(super) fn parse_pivot_spec(p: &mut Parser) -> BqResult<PivotSpec> {
    p.expect(&TokenKind::LParen, "'('")?;
    let aggregates = p.parse_comma_list(|p| {
        let expr = parse_expr(p)?;
        let alias = parse_optional_alias(p)?;
        Ok((expr, alias))
    })?;
    p.expect_kw("FOR")?;
    let for_expr = super::expr_primary::parse_primary(p)?;
    p.expect_kw("IN")?;
    p.expect(&TokenKind::LParen, "'('")?;
    let in_values = p.parse_comma_list(|p| {
        let expr = parse_expr(p)?;
        let alias = parse_optional_alias(p)?;
        Ok((expr, alias))
    })?;
    p.expect(&TokenKind::RParen, "')'")?;
    p.expect(&TokenKind::RParen, "')'")?;
    Ok(PivotSpec {
        aggregates,
        for_expr,
        in_values,
    })
}

pub(super) fn parse_unpivot_spec(p: &mut Parser) -> BqResult<UnpivotSpec> {
    let include_nulls = if p.eat_kw("INCLUDE") {
        p.expect_kw("NULLS")?;
        true
    } else if p.eat_kw("EXCLUDE") {
        p.expect_kw("NULLS")?;
        false
    } else {
        false
    };
    p.expect(&TokenKind::LParen, "'('")?;
    let value_cols = parse_ident_or_tuple(p)?;
    p.expect_kw("FOR")?;
    let name_col = p.parse_ident()?;
    p.expect_kw("IN")?;
    p.expect(&TokenKind::LParen, "'('")?;
    let in_cols = p.parse_comma_list(|p| {
        let cols = parse_unpivot_col_or_tuple(p)?;
        let alias = if p.eat_kw("AS") {
            Some(parse_expr(p)?)
        } else {
            None
        };
        Ok((cols, alias))
    })?;
    p.expect(&TokenKind::RParen, "')'")?;
    p.expect(&TokenKind::RParen, "')'")?;
    Ok(UnpivotSpec {
        include_nulls,
        value_cols,
        name_col,
        in_cols,
    })
}

fn parse_unpivot_col_ident(p: &mut Parser) -> BqResult<Ident> {
    let obj = p.parse_object_name()?;
    obj.parts
        .into_iter()
        .last()
        .ok_or_else(|| BqError::syntax("Expected column name in UNPIVOT", p.current_span()))
}

fn parse_unpivot_col_or_tuple(p: &mut Parser) -> BqResult<Vec<Ident>> {
    if p.eat(&TokenKind::LParen) {
        let cols = p.parse_comma_list(parse_unpivot_col_ident)?;
        p.expect(&TokenKind::RParen, "')'")?;
        Ok(cols)
    } else if matches!(
        p.peek().kind,
        TokenKind::Ident(_) | TokenKind::QuotedIdent(_)
    ) {
        Ok(vec![parse_unpivot_col_ident(p)?])
    } else {
        Err(BqError::syntax(
            "Expected column name or parenthesized column list",
            p.current_span(),
        ))
    }
}

fn parse_ident_or_tuple(p: &mut Parser) -> BqResult<Vec<Ident>> {
    if p.eat(&TokenKind::LParen) {
        let cols = p.parse_comma_list(Parser::parse_ident)?;
        p.expect(&TokenKind::RParen, "')'")?;
        Ok(cols)
    } else if matches!(
        p.peek().kind,
        TokenKind::Ident(_) | TokenKind::QuotedIdent(_)
    ) {
        Ok(vec![p.parse_ident()?])
    } else {
        Err(BqError::syntax(
            "Expected column name or parenthesized column list",
            p.current_span(),
        ))
    }
}
