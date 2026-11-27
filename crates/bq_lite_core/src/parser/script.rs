use crate::ast::{
    AssertStmt, BeginEndBlock, CallStmt, CaseStmt, CaseStmtBranch, DeclareStmt,
    ExecuteImmediateStmt, ForInStmt, IfBranch, IfStmt, RaiseStmt, RepeatStmt, SetStmt, Statement,
    TransactionStmt, WhileStmt,
};
use crate::error::BqResult;

use super::cursor::Parser;
use super::expr::parse_expr;
use super::query::parse_query;
use super::token::TokenKind;
use super::types::parse_data_type;

pub(super) fn parse_block_statements(
    p: &mut Parser,
    stop_kws: &[&str],
) -> BqResult<Vec<Statement>> {
    let mut stmts = Vec::new();
    while !p.at_eof() && !stop_kws.iter().any(|kw| p.is_kw(kw)) {
        if p.eat(&TokenKind::Semicolon) {
            continue;
        }
        stmts.push(super::parse_single_statement(p)?);
        let _ = p.eat(&TokenKind::Semicolon);
    }
    Ok(stmts)
}

pub(super) fn parse_declare(p: &mut Parser) -> BqResult<DeclareStmt> {
    p.expect_kw("DECLARE")?;
    let vars = p.parse_comma_list(Parser::parse_ident)?;
    let ty = if p.is_kw("DEFAULT") || p.at_eof() || p.check(&TokenKind::Semicolon) {
        None
    } else {
        Some(parse_data_type(p)?)
    };
    let default_expr = if p.eat_kw("DEFAULT") {
        Some(parse_expr(p)?)
    } else {
        None
    };
    Ok(DeclareStmt {
        vars,
        ty,
        default_expr,
    })
}

pub(super) fn parse_set(p: &mut Parser) -> BqResult<SetStmt> {
    p.expect_kw("SET")?;
    let (targets, parenthesized) = if p.eat(&TokenKind::LParen) {
        let vars = p.parse_comma_list(Parser::parse_ident)?;
        p.expect(&TokenKind::RParen, "')'")?;
        (vars, true)
    } else {
        (vec![p.parse_ident()?], false)
    };
    p.expect(&TokenKind::Eq, "'='")?;
    let value = parse_expr(p)?;
    Ok(SetStmt {
        targets,
        parenthesized,
        value,
    })
}

pub(super) fn parse_if(p: &mut Parser) -> BqResult<IfStmt> {
    p.expect_kw("IF")?;
    let mut branches = Vec::new();
    loop {
        let condition = parse_expr(p)?;
        p.expect_kw("THEN")?;
        let body = parse_block_statements(p, &["ELSEIF", "ELSE", "END"])?;
        branches.push(IfBranch { condition, body });
        if !p.eat_kw("ELSEIF") {
            break;
        }
    }
    let else_body = if p.eat_kw("ELSE") {
        Some(parse_block_statements(p, &["END"])?)
    } else {
        None
    };
    p.expect_kw("END")?;
    p.expect_kw("IF")?;
    Ok(IfStmt {
        branches,
        else_body,
    })
}

pub(super) fn parse_case_stmt(p: &mut Parser) -> BqResult<CaseStmt> {
    p.expect_kw("CASE")?;
    let operand = if p.is_kw("WHEN") {
        None
    } else {
        Some(parse_expr(p)?)
    };
    let mut branches = Vec::new();
    while p.eat_kw("WHEN") {
        let condition = parse_expr(p)?;
        p.expect_kw("THEN")?;
        let body = parse_block_statements(p, &["WHEN", "ELSE", "END"])?;
        branches.push(CaseStmtBranch { condition, body });
    }
    let else_body = if p.eat_kw("ELSE") {
        Some(parse_block_statements(p, &["END"])?)
    } else {
        None
    };
    p.expect_kw("END")?;
    p.expect_kw("CASE")?;
    Ok(CaseStmt {
        operand,
        branches,
        else_body,
    })
}

pub(super) fn parse_loop(p: &mut Parser) -> BqResult<Vec<Statement>> {
    p.expect_kw("LOOP")?;
    let body = parse_block_statements(p, &["END"])?;
    p.expect_kw("END")?;
    p.expect_kw("LOOP")?;
    Ok(body)
}

pub(super) fn parse_while(p: &mut Parser) -> BqResult<WhileStmt> {
    p.expect_kw("WHILE")?;
    let condition = parse_expr(p)?;
    p.expect_kw("DO")?;
    let body = parse_block_statements(p, &["END"])?;
    p.expect_kw("END")?;
    p.expect_kw("WHILE")?;
    Ok(WhileStmt { condition, body })
}

pub(super) fn parse_repeat(p: &mut Parser) -> BqResult<RepeatStmt> {
    p.expect_kw("REPEAT")?;
    let body = parse_block_statements(p, &["UNTIL"])?;
    p.expect_kw("UNTIL")?;
    let until = parse_expr(p)?;
    p.expect_kw("END")?;
    p.expect_kw("REPEAT")?;
    Ok(RepeatStmt { body, until })
}

pub(super) fn parse_for_in(p: &mut Parser) -> BqResult<ForInStmt> {
    p.expect_kw("FOR")?;
    let var = p.parse_ident()?;
    p.expect_kw("IN")?;
    p.expect(&TokenKind::LParen, "'('")?;
    let query = Box::new(parse_query(p)?);
    p.expect(&TokenKind::RParen, "')'")?;
    p.expect_kw("DO")?;
    let body = parse_block_statements(p, &["END"])?;
    p.expect_kw("END")?;
    p.expect_kw("FOR")?;
    Ok(ForInStmt { var, query, body })
}

pub(super) fn parse_begin(p: &mut Parser) -> BqResult<Statement> {
    p.expect_kw("BEGIN")?;
    if p.eat_kw("TRANSACTION") {
        return Ok(Statement::Transaction(TransactionStmt::Begin));
    }
    let body = parse_block_statements(p, &["EXCEPTION", "END"])?;
    let exception_body = if p.eat_kw("EXCEPTION") {
        p.expect_kw("WHEN")?;
        p.expect_kw("ERROR")?;
        p.expect_kw("THEN")?;
        Some(parse_block_statements(p, &["END"])?)
    } else {
        None
    };
    p.expect_kw("END")?;
    Ok(Statement::BeginEnd(Box::new(BeginEndBlock {
        body,
        exception_body,
    })))
}

pub(super) fn parse_execute_immediate(p: &mut Parser) -> BqResult<ExecuteImmediateStmt> {
    p.expect_kw("EXECUTE")?;
    p.expect_kw("IMMEDIATE")?;
    let sql = parse_expr(p)?;
    let into = if p.eat_kw("INTO") {
        p.parse_comma_list(Parser::parse_ident)?
    } else {
        Vec::new()
    };
    let using = if p.eat_kw("USING") {
        p.parse_comma_list(|p| {
            let expr = parse_expr(p)?;
            let alias = if p.eat_kw("AS") {
                Some(p.parse_ident()?)
            } else {
                None
            };
            Ok((expr, alias))
        })?
    } else {
        Vec::new()
    };
    Ok(ExecuteImmediateStmt { sql, into, using })
}

pub(super) fn parse_call(p: &mut Parser) -> BqResult<CallStmt> {
    p.expect_kw("CALL")?;
    let name = p.parse_object_name()?;
    p.expect(&TokenKind::LParen, "'('")?;
    let args = if p.check(&TokenKind::RParen) {
        Vec::new()
    } else {
        p.parse_comma_list(parse_expr)?
    };
    p.expect(&TokenKind::RParen, "')'")?;
    Ok(CallStmt { name, args })
}

pub(super) fn parse_raise(p: &mut Parser) -> BqResult<RaiseStmt> {
    p.expect_kw("RAISE")?;
    let message = if p.eat_kw("USING") {
        p.expect_kw("MESSAGE")?;
        p.expect(&TokenKind::Eq, "'='")?;
        Some(parse_expr(p)?)
    } else {
        None
    };
    Ok(RaiseStmt { message })
}

pub(super) fn parse_assert(p: &mut Parser) -> BqResult<AssertStmt> {
    p.expect_kw("ASSERT")?;
    let condition = parse_expr(p)?;
    let description = if p.eat_kw("AS") {
        Some(parse_expr(p)?)
    } else {
        None
    };
    Ok(AssertStmt {
        condition,
        description,
    })
}

pub(super) fn parse_commit_or_rollback(p: &mut Parser, is_commit: bool) -> BqResult<Statement> {
    if is_commit {
        p.expect_kw("COMMIT")?;
        p.eat_kw("TRANSACTION");
        Ok(Statement::Transaction(TransactionStmt::Commit))
    } else {
        p.expect_kw("ROLLBACK")?;
        p.eat_kw("TRANSACTION");
        Ok(Statement::Transaction(TransactionStmt::Rollback))
    }
}
