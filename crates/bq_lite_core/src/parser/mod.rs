mod cursor;
mod ddl;
mod ddl_table;
mod dml;
mod expr;
mod expr_postfix;
mod expr_primary;
mod lexer;
mod lexer_strings;
mod pipe;
mod query;
mod query_from;
mod script;
mod token;
mod types;

use crate::ast::{Expr, Query, Statement};
use crate::error::{BqError, BqResult};

use cursor::Parser;
use ddl::{
    parse_alter_schema, parse_alter_view, parse_create_stmt, parse_drop_stmt, parse_export_data,
    parse_load_data,
};
use ddl_table::parse_alter_table;
use dml::{parse_delete, parse_insert, parse_merge, parse_truncate, parse_update};
use lexer::tokenize;
use script::{
    parse_assert, parse_begin, parse_call, parse_case_stmt, parse_commit_or_rollback,
    parse_declare, parse_execute_immediate, parse_for_in, parse_if, parse_loop, parse_raise,
    parse_repeat, parse_set, parse_while,
};
use token::TokenKind;

/// Parses a multi-statement `GoogleSQL` script separated by `;`.
pub fn parse_script(sql: &str) -> BqResult<Vec<Statement>> {
    let tokens = tokenize(sql)?;
    let mut p = Parser::with_source(tokens, sql.to_owned());
    let mut stmts = Vec::new();
    while !p.at_eof() {
        if p.eat(&TokenKind::Semicolon) {
            continue;
        }
        stmts.push(parse_single_statement(&mut p)?);
        if !p.eat(&TokenKind::Semicolon) && !p.at_eof() {
            return Err(BqError::syntax(
                "Expected ';' or end of input after statement",
                p.current_span(),
            ));
        }
    }
    Ok(stmts)
}

/// Parses a single `GoogleSQL` statement.
pub fn parse_statement(sql: &str) -> BqResult<Statement> {
    let tokens = tokenize(sql)?;
    let mut p = Parser::with_source(tokens, sql.to_owned());
    let stmt = parse_single_statement(&mut p)?;
    while p.eat(&TokenKind::Semicolon) {}
    if !p.at_eof() {
        return Err(BqError::syntax(
            "Unexpected trailing tokens after statement",
            p.current_span(),
        ));
    }
    Ok(stmt)
}

/// Parses a standalone `GoogleSQL` query (`SELECT`, `WITH`, `FROM`, or parenthesized query).
pub fn parse_query(sql: &str) -> BqResult<Query> {
    let tokens = tokenize(sql)?;
    let mut p = Parser::new(tokens);
    let q = query::parse_query(&mut p)?;
    while p.eat(&TokenKind::Semicolon) {}
    if !p.at_eof() {
        return Err(BqError::syntax(
            "Unexpected trailing tokens after query",
            p.current_span(),
        ));
    }
    Ok(q)
}

/// Parses a standalone `GoogleSQL` expression.
pub fn parse_expr(sql: &str) -> BqResult<Expr> {
    let tokens = tokenize(sql)?;
    let mut p = Parser::new(tokens);
    let expr = expr::parse_expr(&mut p)?;
    if !p.at_eof() {
        return Err(BqError::syntax(
            "Unexpected trailing tokens after expression",
            p.current_span(),
        ));
    }
    Ok(expr)
}

fn parse_single_statement(p: &mut Parser) -> BqResult<Statement> {
    let TokenKind::Ident(word) = &p.peek().kind else {
        if matches!(p.peek().kind, TokenKind::LParen) {
            return Ok(Statement::Query(Box::new(query::parse_query(p)?)));
        }
        return Err(BqError::syntax("Expected SQL statement", p.current_span()));
    };
    let upper = word.to_ascii_uppercase();
    match upper.as_str() {
        "SELECT" | "WITH" | "FROM" => Ok(Statement::Query(Box::new(query::parse_query(p)?))),
        "INSERT" => Ok(Statement::Insert(Box::new(parse_insert(p)?))),
        "UPDATE" => Ok(Statement::Update(Box::new(parse_update(p)?))),
        "DELETE" => Ok(Statement::Delete(Box::new(parse_delete(p)?))),
        "MERGE" => Ok(Statement::Merge(Box::new(parse_merge(p)?))),
        "TRUNCATE" => Ok(Statement::Truncate(parse_truncate(p)?)),
        "CREATE" => parse_create_stmt(p),
        "ALTER" => parse_alter_stmt(p),
        "DROP" => Ok(Statement::Drop(parse_drop_stmt(p)?)),
        "EXPORT" => Ok(Statement::ExportData(Box::new(parse_export_data(p)?))),
        "LOAD" => Ok(Statement::LoadData(Box::new(parse_load_data(p)?))),
        "DECLARE" => Ok(Statement::Declare(Box::new(parse_declare(p)?))),
        "SET" => Ok(Statement::Set(Box::new(parse_set(p)?))),
        "IF" => Ok(Statement::If(Box::new(parse_if(p)?))),
        "CASE" => Ok(Statement::Case(Box::new(parse_case_stmt(p)?))),
        "LOOP" => Ok(Statement::Loop(parse_loop(p)?)),
        "WHILE" => Ok(Statement::While(Box::new(parse_while(p)?))),
        "REPEAT" => Ok(Statement::Repeat(Box::new(parse_repeat(p)?))),
        "FOR" => Ok(Statement::ForIn(Box::new(parse_for_in(p)?))),
        "BEGIN" => parse_begin(p),
        "EXECUTE" => Ok(Statement::ExecuteImmediate(Box::new(
            parse_execute_immediate(p)?,
        ))),
        "CALL" => Ok(Statement::Call(parse_call(p)?)),
        "RAISE" => Ok(Statement::Raise(Box::new(parse_raise(p)?))),
        "ASSERT" => Ok(Statement::Assert(Box::new(parse_assert(p)?))),
        "COMMIT" => parse_commit_or_rollback(p, true),
        "ROLLBACK" => parse_commit_or_rollback(p, false),
        "BREAK" | "LEAVE" => {
            p.advance();
            Ok(Statement::Break)
        }
        "CONTINUE" | "ITERATE" => {
            p.advance();
            Ok(Statement::Continue)
        }
        "RETURN" => {
            p.advance();
            Ok(Statement::Return)
        }
        _ => Err(BqError::syntax(
            format!("Unexpected keyword '{word}' at start of statement"),
            p.current_span(),
        )),
    }
}

fn parse_alter_stmt(p: &mut Parser) -> BqResult<Statement> {
    if p.is_kw_n(1, "SCHEMA") || p.is_kw_n(1, "DATASET") {
        return Ok(Statement::AlterSchema(parse_alter_schema(p)?));
    }
    if p.is_kw_n(1, "VIEW") || p.is_kw_n(1, "MATERIALIZED") {
        return Ok(Statement::AlterView(Box::new(parse_alter_view(p)?)));
    }
    Ok(Statement::AlterTable(Box::new(parse_alter_table(p)?)))
}
