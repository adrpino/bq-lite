use crate::ast::{
    Join, PipeGroupByItem, PipeOp, PipeOrderDir, PipeOrderedItem, SetColumnMatchMode,
    SetCorrespondence, SetOpKind, SetQuantifier,
};
use crate::error::{BqError, BqResult};

use super::cursor::Parser;
use super::expr::parse_expr;
use super::expr_postfix::parse_order_by_expr;
use super::query::{
    parse_cte, parse_group_by_item, parse_optional_alias, parse_optional_window_clause,
    parse_query, parse_select_item,
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

fn peek_set_op_prefix(p: &Parser, allowed: &[&str]) -> Option<(Option<SetColumnMatchMode>, usize)> {
    let check_op = |offset: usize| matches!(&p.peek_n(offset).kind, TokenKind::Ident(w) if allowed.iter().any(|k| w.eq_ignore_ascii_case(k)));
    if check_op(0) {
        return Some((None, 0));
    }
    let mode = if p.is_kw("INNER") {
        Some(SetColumnMatchMode::Inner)
    } else if p.is_kw("LEFT") {
        Some(SetColumnMatchMode::Left)
    } else if p.is_kw("FULL") {
        Some(SetColumnMatchMode::Full)
    } else {
        None
    }?;
    if check_op(1) {
        return Some((Some(mode), 1));
    }
    if matches!(&p.peek_n(1).kind, TokenKind::Ident(w) if w.eq_ignore_ascii_case("OUTER"))
        && check_op(2)
    {
        return Some((Some(mode), 2));
    }
    None
}

pub(super) fn try_parse_set_op_head(
    p: &mut Parser,
    allowed: &[&str],
) -> BqResult<Option<(SetOpKind, SetQuantifier, SetCorrespondence)>> {
    let Some((prefix_mode, skip)) = peek_set_op_prefix(p, allowed) else {
        return Ok(None);
    };
    for _ in 0..skip {
        p.advance();
    }
    let op = if p.eat_kw("UNION") {
        SetOpKind::Union
    } else if p.eat_kw("INTERSECT") {
        SetOpKind::Intersect
    } else {
        p.expect_kw("EXCEPT")?;
        SetOpKind::Except
    };
    let quantifier = parse_set_quantifier(p)?;
    let correspondence = parse_optional_correspondence_with_mode(p, prefix_mode)?;
    Ok(Some((op, quantifier, correspondence)))
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

fn parse_optional_correspondence_with_mode(
    p: &mut Parser,
    prefix_mode: Option<SetColumnMatchMode>,
) -> BqResult<SetCorrespondence> {
    let mode = if p.eat_kw("BY") {
        p.expect_kw("NAME")?;
        prefix_mode.unwrap_or(SetColumnMatchMode::Strict)
    } else if p.eat_kw("CORRESPONDING") {
        prefix_mode.unwrap_or(SetColumnMatchMode::Inner)
    } else if p.is_kw("STRICT")
        && matches!(&p.peek_n(1).kind, TokenKind::Ident(w) if w.eq_ignore_ascii_case("CORRESPONDING"))
    {
        p.advance();
        p.advance();
        SetColumnMatchMode::Strict
    } else {
        if prefix_mode.is_some() {
            return Err(BqError::syntax(
                "Expected BY NAME or CORRESPONDING after INNER/LEFT/FULL set operator",
                p.current_span(),
            ));
        }
        return Ok(SetCorrespondence::Positional);
    };
    let mut on_columns = Vec::new();
    if p.eat_kw("ON") || p.eat_kw("BY") {
        p.expect(&TokenKind::LParen, "'('")?;
        loop {
            on_columns.push(p.parse_ident()?.value);
            if !p.eat(&TokenKind::Comma) {
                break;
            }
        }
        p.expect(&TokenKind::RParen, "')'")?;
    }
    Ok(SetCorrespondence::ByName { mode, on_columns })
}

fn try_parse_pipe_set_op(p: &mut Parser) -> BqResult<Option<PipeOp>> {
    let Some((op, quantifier, correspondence)) =
        try_parse_set_op_head(p, &["UNION", "INTERSECT", "EXCEPT"])?
    else {
        return Ok(None);
    };
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
