use crate::ast::{
    AlterSchemaStmt, AlterViewStmt, CreateFunctionStmt, CreateProcedureStmt,
    CreateRowAccessPolicyStmt, CreateSchemaStmt, CreateTableKind, CreateViewStmt, DataTypeAst,
    DropObjectKind, DropStmt, ExportDataStmt, FunctionBody, FunctionKind, LoadDataStmt, ParamMode,
    RoutineParam, Statement,
};
use crate::error::{BqError, BqResult};

use super::cursor::Parser;
use super::ddl_table::{
    parse_column_def, parse_create_table_kind, parse_if_exists, parse_if_not_exists,
    parse_paren_idents,
};
use super::expr::parse_expr;
use super::query::parse_query;
use super::script::parse_block_statements;
use super::token::TokenKind;
use super::types::parse_data_type;

pub(super) fn parse_create_stmt(p: &mut Parser) -> BqResult<Statement> {
    p.expect_kw("CREATE")?;
    let or_replace = if p.eat_kw("OR") {
        p.expect_kw("REPLACE")?;
        true
    } else {
        false
    };
    let temp = p.eat_kw("TEMP") || p.eat_kw("TEMPORARY");
    if p.is_kw("SCHEMA") || p.is_kw("DATASET") {
        return Ok(Statement::CreateSchema(parse_create_schema(p)?));
    }
    if p.is_kw("SNAPSHOT") || p.is_kw("EXTERNAL") || (p.is_kw("TABLE") && !p.is_kw_n(1, "FUNCTION"))
    {
        let kind_override = if p.is_kw("SNAPSHOT") {
            Some(CreateTableKind::Snapshot)
        } else if p.is_kw("EXTERNAL") {
            Some(CreateTableKind::External)
        } else {
            None
        };
        return Ok(Statement::CreateTable(Box::new(parse_create_table_kind(
            p,
            or_replace,
            temp,
            kind_override,
        )?)));
    }
    if p.is_kw("VIEW") || p.is_kw("MATERIALIZED") {
        return Ok(Statement::CreateView(Box::new(parse_create_view(
            p, or_replace,
        )?)));
    }
    if p.is_kw("FUNCTION") || (p.is_kw("TABLE") && p.is_kw_n(1, "FUNCTION")) {
        return Ok(Statement::CreateFunction(Box::new(parse_create_function(
            p, or_replace, temp,
        )?)));
    }
    if p.is_kw("PROCEDURE") {
        return Ok(Statement::CreateProcedure(Box::new(
            parse_create_procedure(p, or_replace)?,
        )));
    }
    if p.is_kw("ROW") {
        return Ok(Statement::CreateRowAccessPolicy(Box::new(
            parse_create_row_access_policy(p, or_replace)?,
        )));
    }
    Err(BqError::syntax(
        "Unsupported CREATE object type",
        p.current_span(),
    ))
}

fn parse_create_schema(p: &mut Parser) -> BqResult<CreateSchemaStmt> {
    if !p.eat_kw("SCHEMA") {
        p.expect_kw("DATASET")?;
    }
    let if_not_exists = parse_if_not_exists(p)?;
    let name = p.parse_object_name()?;
    let options = p.parse_options_clause()?;
    Ok(CreateSchemaStmt {
        if_not_exists,
        name,
        options,
    })
}

fn parse_create_view(p: &mut Parser, or_replace: bool) -> BqResult<CreateViewStmt> {
    let materialized = p.eat_kw("MATERIALIZED");
    p.expect_kw("VIEW")?;
    let if_not_exists = parse_if_not_exists(p)?;
    let name = p.parse_object_name()?;
    let columns = if p.check(&TokenKind::LParen) {
        parse_paren_idents(p)?
    } else {
        Vec::new()
    };
    let options = p.parse_options_clause()?;
    p.expect_kw("AS")?;
    let start_off = p.current_span().offset;
    let query = Box::new(parse_query(p)?);
    let view_sql = p.slice_from(start_off);
    Ok(CreateViewStmt {
        or_replace,
        materialized,
        if_not_exists,
        name,
        columns,
        options,
        query,
        view_sql,
    })
}

fn parse_create_function(
    p: &mut Parser,
    or_replace: bool,
    temp: bool,
) -> BqResult<CreateFunctionStmt> {
    let table_function = p.eat_kw("TABLE");
    p.expect_kw("FUNCTION")?;
    let if_not_exists = parse_if_not_exists(p)?;
    let name = p.parse_object_name()?;
    let params = parse_routine_params(p)?;
    let returns = if p.eat_kw("RETURNS") {
        Some(parse_data_type(p)?)
    } else {
        None
    };
    let language = if p.eat_kw("LANGUAGE") {
        Some(p.parse_ident()?.value)
    } else {
        None
    };
    let options = p.parse_options_clause()?;
    p.expect_kw("AS")?;
    let body = parse_function_body(p, table_function, language.as_deref())?;
    Ok(CreateFunctionStmt {
        or_replace,
        temp,
        kind: if table_function {
            FunctionKind::Table
        } else {
            FunctionKind::Scalar
        },
        if_not_exists,
        name,
        params,
        returns,
        language,
        options,
        body,
    })
}

fn parse_function_body(
    p: &mut Parser,
    table_function: bool,
    language: Option<&str>,
) -> BqResult<FunctionBody> {
    if language.is_some_and(|l| l.eq_ignore_ascii_case("js")) {
        if let TokenKind::String(code) = p.advance().kind {
            return Ok(FunctionBody::Code(code));
        }
        return Err(BqError::syntax(
            "Expected JavaScript body string after AS",
            p.current_span(),
        ));
    }
    if table_function {
        return Ok(FunctionBody::Query(Box::new(parse_query(p)?)));
    }
    p.expect(&TokenKind::LParen, "'('")?;
    let expr = parse_expr(p)?;
    p.expect(&TokenKind::RParen, "')'")?;
    Ok(FunctionBody::Expr(Box::new(expr)))
}

fn parse_create_procedure(p: &mut Parser, or_replace: bool) -> BqResult<CreateProcedureStmt> {
    p.expect_kw("PROCEDURE")?;
    let if_not_exists = parse_if_not_exists(p)?;
    let name = p.parse_object_name()?;
    let params = parse_routine_params(p)?;
    let options = p.parse_options_clause()?;
    p.expect_kw("BEGIN")?;
    let body = parse_block_statements(p, &["END"])?;
    p.expect_kw("END")?;
    Ok(CreateProcedureStmt {
        or_replace,
        if_not_exists,
        name,
        params,
        options,
        body,
    })
}

fn parse_create_row_access_policy(
    p: &mut Parser,
    or_replace: bool,
) -> BqResult<CreateRowAccessPolicyStmt> {
    p.expect_kw("ROW")?;
    p.expect_kw("ACCESS")?;
    p.expect_kw("POLICY")?;
    let if_not_exists = parse_if_not_exists(p)?;
    let policy_name = p.parse_ident()?;
    p.expect_kw("ON")?;
    let table_name = p.parse_object_name()?;
    let mut grantees = Vec::new();
    if p.eat_kw("GRANT") {
        p.expect_kw("TO")?;
        p.expect(&TokenKind::LParen, "'('")?;
        grantees = p.parse_comma_list(|p| {
            if let TokenKind::String(s) = p.advance().kind {
                Ok(s)
            } else {
                Err(BqError::syntax("Expected grantee string", p.current_span()))
            }
        })?;
        p.expect(&TokenKind::RParen, "')'")?;
    }
    p.expect_kw("FILTER")?;
    p.expect_kw("USING")?;
    p.expect(&TokenKind::LParen, "'('")?;
    let filter_expr = parse_expr(p)?;
    p.expect(&TokenKind::RParen, "')'")?;
    Ok(CreateRowAccessPolicyStmt {
        or_replace,
        if_not_exists,
        policy_name,
        table_name,
        grantees,
        filter_expr,
    })
}

fn parse_routine_params(p: &mut Parser) -> BqResult<Vec<RoutineParam>> {
    p.expect(&TokenKind::LParen, "'('")?;
    if p.eat(&TokenKind::RParen) {
        return Ok(Vec::new());
    }
    let params = p.parse_comma_list(|p| {
        let mode = if p.eat_kw("INOUT") {
            Some(ParamMode::InOut)
        } else if p.eat_kw("OUT") {
            Some(ParamMode::Out)
        } else if p.eat_kw("IN") {
            Some(ParamMode::In)
        } else {
            None
        };
        let name = p.parse_ident()?;
        let ty = if p.eat_kw("ANY") {
            p.expect_kw("TABLE")?;
            DataTypeAst::Table(Vec::new())
        } else {
            parse_data_type(p)?
        };
        Ok(RoutineParam { mode, name, ty })
    })?;
    p.expect(&TokenKind::RParen, "')'")?;
    Ok(params)
}

pub(super) fn parse_drop_stmt(p: &mut Parser) -> BqResult<DropStmt> {
    p.expect_kw("DROP")?;
    let kind = if p.eat_kw("TABLE") {
        if p.eat_kw("FUNCTION") {
            DropObjectKind::TableFunction
        } else {
            DropObjectKind::Table
        }
    } else if p.eat_kw("SNAPSHOT") {
        p.expect_kw("TABLE")?;
        DropObjectKind::SnapshotTable
    } else if p.eat_kw("EXTERNAL") {
        p.expect_kw("TABLE")?;
        DropObjectKind::ExternalTable
    } else if p.eat_kw("SCHEMA") || p.eat_kw("DATASET") {
        DropObjectKind::Schema
    } else if p.eat_kw("VIEW") {
        DropObjectKind::View
    } else if p.eat_kw("MATERIALIZED") {
        p.expect_kw("VIEW")?;
        DropObjectKind::MaterializedView
    } else if p.eat_kw("FUNCTION") {
        DropObjectKind::Function
    } else if p.eat_kw("PROCEDURE") {
        DropObjectKind::Procedure
    } else if p.eat_kw("ROW") {
        p.expect_kw("ACCESS")?;
        p.expect_kw("POLICY")?;
        DropObjectKind::RowAccessPolicy
    } else {
        return Err(BqError::syntax(
            "Unsupported DROP object type",
            p.current_span(),
        ));
    };
    let if_exists = parse_if_exists(p)?;
    let name = p.parse_object_name()?;
    let on_table = if kind == DropObjectKind::RowAccessPolicy {
        p.expect_kw("ON")?;
        Some(p.parse_object_name()?)
    } else {
        None
    };
    let cascade = p.eat_kw("CASCADE");
    let _ = p.eat_kw("RESTRICT");
    Ok(DropStmt {
        kind,
        if_exists,
        name,
        on_table,
        cascade,
    })
}

pub(super) fn parse_export_data(p: &mut Parser) -> BqResult<ExportDataStmt> {
    p.expect_kw("EXPORT")?;
    p.expect_kw("DATA")?;
    let options = p.parse_options_clause()?;
    p.expect_kw("AS")?;
    let query = Box::new(parse_query(p)?);
    Ok(ExportDataStmt { options, query })
}

pub(super) fn parse_alter_schema(p: &mut Parser) -> BqResult<AlterSchemaStmt> {
    p.expect_kw("ALTER")?;
    if !p.eat_kw("SCHEMA") {
        p.expect_kw("DATASET")?;
    }
    let if_exists = parse_if_exists(p)?;
    let name = p.parse_object_name()?;
    p.expect_kw("SET")?;
    let options = p.parse_options_clause()?;
    Ok(AlterSchemaStmt {
        if_exists,
        name,
        options,
    })
}

pub(super) fn parse_alter_view(p: &mut Parser) -> BqResult<AlterViewStmt> {
    p.expect_kw("ALTER")?;
    let materialized = p.eat_kw("MATERIALIZED");
    p.expect_kw("VIEW")?;
    let if_exists = parse_if_exists(p)?;
    let name = p.parse_object_name()?;
    let options = if p.eat_kw("SET") {
        p.parse_options_clause()?
    } else {
        Vec::new()
    };
    let (query, view_sql) = if p.eat_kw("AS") {
        let start_off = p.current_span().offset;
        let q = Box::new(parse_query(p)?);
        let vsql = p.slice_from(start_off);
        (Some(q), vsql)
    } else {
        (None, None)
    };
    Ok(AlterViewStmt {
        if_exists,
        materialized,
        name,
        options,
        query,
        view_sql,
    })
}

pub(super) fn parse_load_data(p: &mut Parser) -> BqResult<LoadDataStmt> {
    p.expect_kw("LOAD")?;
    p.expect_kw("DATA")?;
    let overwrite = if p.eat_kw("OVERWRITE") {
        true
    } else {
        let _ = p.eat_kw("INTO");
        false
    };
    let table = p.parse_object_name()?;
    let columns = if p.eat(&TokenKind::LParen) {
        if p.eat(&TokenKind::RParen) {
            Vec::new()
        } else {
            let cols = p.parse_comma_list(parse_column_def)?;
            p.expect(&TokenKind::RParen, "')'")?;
            cols
        }
    } else {
        Vec::new()
    };
    p.expect_kw("FROM")?;
    p.expect_kw("FILES")?;
    p.expect(&TokenKind::LParen, "'('")?;
    let options = if p.eat(&TokenKind::RParen) {
        Vec::new()
    } else {
        let opts = p.parse_comma_list(|p| {
            let name = p.parse_ident()?;
            p.expect(&TokenKind::Eq, "'='")?;
            let value = parse_expr(p)?;
            Ok(crate::ast::OptionEntry { name, value })
        })?;
        p.expect(&TokenKind::RParen, "')'")?;
        opts
    };
    Ok(LoadDataStmt {
        overwrite,
        table,
        columns,
        options,
    })
}
