use crate::ast::{Join, PipeGroupByItem, PipeOp, PipeOrderDir, PipeOrderedItem, SetOpKind};
use crate::error::{BqError, BqResult};

use super::cursor::Parser;
use super::expr::parse_expr;
use super::expr_postfix::parse_order_by_expr;
use super::query::{
    parse_cte, parse_group_by_item, parse_optional_alias, parse_optional_correspondence,
    parse_optional_window_clause, parse_query, parse_select_item, parse_set_quantifier,
};
use super::query_from::{
    parse_join_constraint, parse_pivot_spec, parse_table_factor, parse_unpivot_spec,
    try_eat_join_kind,
};
use super::token::TokenKind;

pub(super) fn parse_pipe_op(p: &mut Parser) -> BqResult<PipeOp> {
    if let Some(op) = try_parse_pipe_basic(p)? {
        return Ok(op);
    }
    if let Some(op) = try_parse_pipe_relational(p)? {
        return Ok(op);
    }
    Err(BqError::syntax(
        "Unsupported pipe operator",
        p.current_span(),
    ))
}

fn try_parse_pipe_basic(p: &mut Parser) -> BqResult<Option<PipeOp>> {
    if p.eat_kw("WHERE") {
        return Ok(Some(PipeOp::Where(Box::new(parse_expr(p)?))));
    }
    if p.eat_kw("SELECT") {
        let items = p.parse_comma_list(parse_select_item)?;
        let window = parse_optional_window_clause(p)?;
        return Ok(Some(PipeOp::Select { items, window }));
    }
    if p.eat_kw("EXTEND") {
        let items = p.parse_comma_list(parse_select_item)?;
        let window = parse_optional_window_clause(p)?;
        return Ok(Some(PipeOp::Extend { items, window }));
    }
    if p.eat_kw("WINDOW") {
        let items = p.parse_comma_list(parse_select_item)?;
        let window = parse_optional_window_clause(p)?;
        return Ok(Some(PipeOp::Window { items, window }));
    }
    if p.eat_kw("SET") {
        let items = p.parse_comma_list(|p| {
            let col = p.parse_ident()?;
            p.expect(&TokenKind::Eq, "'='")?;
            let val = parse_expr(p)?;
            Ok((col, val))
        })?;
        return Ok(Some(PipeOp::Set(items)));
    }
    if p.eat_kw("DROP") {
        return Ok(Some(PipeOp::Drop(p.parse_comma_list(Parser::parse_ident)?)));
    }
    if p.eat_kw("RENAME") {
        let items = p.parse_comma_list(|p| {
            let old = p.parse_ident()?;
            p.eat_kw("AS");
            let new = p.parse_ident()?;
            Ok((old, new))
        })?;
        return Ok(Some(PipeOp::Rename(items)));
    }
    if p.eat_kw("AS") {
        return Ok(Some(PipeOp::As(p.parse_ident()?)));
    }
    if p.eat_kw("DISTINCT") {
        return Ok(Some(PipeOp::Distinct));
    }
    Ok(None)
}

fn try_parse_pipe_relational(p: &mut Parser) -> BqResult<Option<PipeOp>> {
    if p.eat_kw("AGGREGATE") {
        return parse_pipe_aggregate(p).map(Some);
    }
    if p.eat_kw("ORDER") {
        p.expect_kw("BY")?;
        return Ok(Some(PipeOp::OrderBy(
            p.parse_comma_list(parse_order_by_expr)?,
        )));
    }
    if p.eat_kw("LIMIT") {
        let limit = if p.eat_kw("ALL") {
            None
        } else {
            Some(Box::new(parse_expr(p)?))
        };
        let offset = if p.eat_kw("OFFSET") {
            Some(Box::new(parse_expr(p)?))
        } else {
            None
        };
        return Ok(Some(PipeOp::Limit { limit, offset }));
    }
    if let Some(kind) = try_eat_join_kind(p)? {
        let relation = parse_table_factor(p)?;
        let constraint = parse_join_constraint(p, kind)?;
        return Ok(Some(PipeOp::Join(Box::new(Join {
            kind,
            relation,
            constraint,
        }))));
    }
    if let Some(op) = try_parse_pipe_set_op(p)? {
        return Ok(Some(op));
    }
    if p.eat_kw("PIVOT") {
        let spec = parse_pivot_spec(p)?;
        let alias = parse_optional_alias(p)?;
        return Ok(Some(PipeOp::Pivot { spec, alias }));
    }
    if p.eat_kw("UNPIVOT") {
        let spec = parse_unpivot_spec(p)?;
        let alias = parse_optional_alias(p)?;
        return Ok(Some(PipeOp::Unpivot { spec, alias }));
    }
    if p.eat_kw("TABLESAMPLE") {
        return parse_pipe_tablesample(p).map(Some);
    }
    if p.eat_kw("WITH") {
        let ctes = p.parse_comma_list(parse_cte)?;
        return Ok(Some(PipeOp::With(ctes)));
    }
    Ok(None)
}

fn parse_pipe_aggregate(p: &mut Parser) -> BqResult<PipeOp> {
    let aggs = if p.is_kw("GROUP") {
        Vec::new()
    } else {
        p.parse_comma_list(|p| {
            let item = parse_select_item(p)?;
            let order = parse_optional_order_dir(p)?;
            Ok(PipeOrderedItem { item, order })
        })?
    };
    let mut group_and_order_by = false;
    let group_by = if p.eat_kw("GROUP") {
        if p.eat_kw("AND") {
            p.expect_kw("ORDER")?;
            group_and_order_by = true;
        }
        p.expect_kw("BY")?;
        p.parse_comma_list(|p| {
            let item = parse_group_by_item(p)?;
            let alias = parse_optional_alias(p)?;
            let order = parse_optional_order_dir(p)?;
            Ok(PipeGroupByItem { item, alias, order })
        })?
    } else {
        Vec::new()
    };
    Ok(PipeOp::Aggregate {
        aggs,
        group_by,
        group_and_order_by,
    })
}

fn parse_optional_order_dir(p: &mut Parser) -> BqResult<Option<PipeOrderDir>> {
    let explicit_asc = if p.eat_kw("ASC") {
        Some(true)
    } else if p.eat_kw("DESC") {
        Some(false)
    } else {
        None
    };
    let nulls_first = if p.eat_kw("NULLS") {
        if p.eat_kw("FIRST") {
            Some(true)
        } else {
            p.expect_kw("LAST")?;
            Some(false)
        }
    } else {
        None
    };
    if explicit_asc.is_none() && nulls_first.is_none() {
        return Ok(None);
    }
    Ok(Some(PipeOrderDir {
        asc: explicit_asc.unwrap_or(true),
        nulls_first,
    }))
}

fn try_parse_pipe_set_op(p: &mut Parser) -> BqResult<Option<PipeOp>> {
    let op = if p.eat_kw("UNION") {
        SetOpKind::Union
    } else if p.eat_kw("INTERSECT") {
        SetOpKind::Intersect
    } else if p.eat_kw("EXCEPT") {
        SetOpKind::Except
    } else {
        return Ok(None);
    };
    let quantifier = parse_set_quantifier(p)?;
    let correspondence = parse_optional_correspondence(p)?;
    let mut queries = Vec::new();
    loop {
        p.expect(&TokenKind::LParen, "'('")?;
        let q = parse_query(p)?;
        p.expect(&TokenKind::RParen, "')'")?;
        queries.push(q);
        if !p.eat(&TokenKind::Comma) || !p.check(&TokenKind::LParen) {
            break;
        }
    }
    Ok(Some(PipeOp::SetOp {
        op,
        quantifier,
        correspondence,
        queries,
    }))
}

fn parse_pipe_tablesample(p: &mut Parser) -> BqResult<PipeOp> {
    if !p.eat_kw("SYSTEM") && !p.eat_kw("RESERVOIR") && !p.eat_kw("BERNOULLI") {
        return Err(BqError::syntax(
            "Expected SYSTEM, RESERVOIR, or BERNOULLI after TABLESAMPLE",
            p.current_span(),
        ));
    }
    p.expect(&TokenKind::LParen, "'('")?;
    let amount = Box::new(parse_expr(p)?);
    let is_rows = if p.eat_kw("ROWS") {
        true
    } else {
        p.expect_kw("PERCENT")?;
        false
    };
    p.expect(&TokenKind::RParen, "')'")?;
    Ok(PipeOp::TableSample { amount, is_rows })
}
