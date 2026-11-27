use crate::ast::{
    DeleteStmt, Ident, InsertSource, InsertStmt, MergeAction, MergeClause, MergeConditionKind,
    MergeStmt, SelectItem, TruncateStmt, UpdateAssignment, UpdateStmt,
};
use crate::error::{BqError, BqResult};

use super::cursor::Parser;
use super::expr::parse_expr;
use super::query::{parse_optional_alias, parse_query, parse_select_item};
use super::query_from::{parse_from_list, parse_table_factor};
use super::token::TokenKind;

pub(super) fn parse_insert(p: &mut Parser) -> BqResult<InsertStmt> {
    p.expect_kw("INSERT")?;
    p.eat_kw("INTO");
    let table = p.parse_object_name()?;
    let columns = if p.eat(&TokenKind::LParen) {
        let cols = p.parse_comma_list(Parser::parse_ident)?;
        p.expect(&TokenKind::RParen, "')'")?;
        cols
    } else {
        Vec::new()
    };
    let source = if p.eat_kw("VALUES") {
        let rows = p.parse_comma_list(|p| {
            p.expect(&TokenKind::LParen, "'('")?;
            let vals = p.parse_comma_list(parse_dml_value_expr)?;
            p.expect(&TokenKind::RParen, "')'")?;
            Ok(vals)
        })?;
        InsertSource::Values(rows)
    } else {
        InsertSource::Query(Box::new(parse_query(p)?))
    };
    let returning = parse_optional_then_return(p)?;
    Ok(InsertStmt {
        table,
        columns,
        source,
        returning,
    })
}

pub(super) fn parse_update(p: &mut Parser) -> BqResult<UpdateStmt> {
    p.expect_kw("UPDATE")?;
    let table = p.parse_object_name()?;
    let alias = parse_update_alias(p)?;
    p.expect_kw("SET")?;
    let assignments = p.parse_comma_list(parse_assignment)?;
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
    let returning = parse_optional_then_return(p)?;
    Ok(UpdateStmt {
        table,
        alias,
        assignments,
        from,
        where_clause,
        returning,
    })
}

fn parse_update_alias(p: &mut Parser) -> BqResult<Option<Ident>> {
    if p.is_kw("SET") {
        return Ok(None);
    }
    parse_optional_alias(p)
}

fn parse_assignment(p: &mut Parser) -> BqResult<UpdateAssignment> {
    let mut path = vec![p.parse_ident()?];
    while p.eat(&TokenKind::Dot) {
        path.push(p.parse_ident()?);
    }
    p.expect(&TokenKind::Eq, "'='")?;
    let value = parse_dml_value_expr(p)?;
    Ok(UpdateAssignment { path, value })
}

fn parse_dml_value_expr(p: &mut Parser) -> BqResult<crate::ast::Expr> {
    if p.eat_kw("DEFAULT") {
        return Ok(crate::ast::Expr::SystemVar("__bq_default__".to_owned()));
    }
    parse_expr(p)
}

pub(super) fn parse_delete(p: &mut Parser) -> BqResult<DeleteStmt> {
    p.expect_kw("DELETE")?;
    p.eat_kw("FROM");
    let table = p.parse_object_name()?;
    let alias =
        if p.is_kw("WHERE") || p.is_kw("THEN") || p.at_eof() || p.check(&TokenKind::Semicolon) {
            None
        } else {
            parse_optional_alias(p)?
        };
    let where_clause = if p.eat_kw("WHERE") {
        Some(parse_expr(p)?)
    } else {
        None
    };
    let returning = parse_optional_then_return(p)?;
    Ok(DeleteStmt {
        table,
        alias,
        where_clause,
        returning,
    })
}

pub(super) fn parse_merge(p: &mut Parser) -> BqResult<MergeStmt> {
    p.expect_kw("MERGE")?;
    p.eat_kw("INTO");
    let target = p.parse_object_name()?;
    let target_alias = if p.is_kw("USING") {
        None
    } else {
        parse_optional_alias(p)?
    };
    p.expect_kw("USING")?;
    let source = parse_table_factor(p)?;
    p.expect_kw("ON")?;
    let on = parse_expr(p)?;
    let mut clauses = Vec::new();
    while p.eat_kw("WHEN") {
        clauses.push(parse_merge_clause(p)?);
    }
    if clauses.is_empty() {
        return Err(BqError::syntax(
            "MERGE statement requires at least one WHEN clause",
            p.current_span(),
        ));
    }
    Ok(MergeStmt {
        target,
        target_alias,
        source,
        on,
        clauses,
    })
}

fn parse_merge_clause(p: &mut Parser) -> BqResult<MergeClause> {
    let kind = if p.eat_kw("MATCHED") {
        MergeConditionKind::Matched
    } else {
        p.expect_kw("NOT")?;
        p.expect_kw("MATCHED")?;
        if p.eat_kw("BY") {
            if p.eat_kw("SOURCE") {
                MergeConditionKind::NotMatchedBySource
            } else {
                p.expect_kw("TARGET")?;
                MergeConditionKind::NotMatchedByTarget
            }
        } else {
            MergeConditionKind::NotMatchedByTarget
        }
    };
    let predicate = if p.eat_kw("AND") {
        Some(parse_expr(p)?)
    } else {
        None
    };
    p.expect_kw("THEN")?;
    let action = if p.eat_kw("UPDATE") {
        p.expect_kw("SET")?;
        MergeAction::Update(p.parse_comma_list(parse_assignment)?)
    } else if p.eat_kw("DELETE") {
        MergeAction::Delete
    } else if p.eat_kw("INSERT") {
        parse_merge_insert_action(p)?
    } else {
        return Err(BqError::syntax(
            "Expected UPDATE, DELETE, or INSERT in MERGE THEN clause",
            p.current_span(),
        ));
    };
    Ok(MergeClause {
        kind,
        predicate,
        action,
    })
}

fn parse_merge_insert_action(p: &mut Parser) -> BqResult<MergeAction> {
    let columns = if p.eat(&TokenKind::LParen) {
        let cols = p.parse_comma_list(Parser::parse_ident)?;
        p.expect(&TokenKind::RParen, "')'")?;
        cols
    } else {
        Vec::new()
    };
    let values = if p.eat_kw("ROW") {
        None
    } else {
        p.expect_kw("VALUES")?;
        p.expect(&TokenKind::LParen, "'('")?;
        let vals = p.parse_comma_list(parse_expr)?;
        p.expect(&TokenKind::RParen, "')'")?;
        Some(vals)
    };
    Ok(MergeAction::Insert { columns, values })
}

pub(super) fn parse_truncate(p: &mut Parser) -> BqResult<TruncateStmt> {
    p.expect_kw("TRUNCATE")?;
    p.expect_kw("TABLE")?;
    Ok(TruncateStmt {
        table: p.parse_object_name()?,
    })
}

fn parse_optional_then_return(p: &mut Parser) -> BqResult<Option<Vec<SelectItem>>> {
    if !p.eat_kw("THEN") {
        return Ok(None);
    }
    p.expect_kw("RETURN")?;
    Ok(Some(p.parse_comma_list(parse_select_item)?))
}
