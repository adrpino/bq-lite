use crate::ast::{
    AlterTableAction, AlterTableStmt, ColumnDef, CreateTableKind, CreateTableStmt, Ident,
    TableConstraint,
};
use crate::error::{BqError, BqResult};

use super::cursor::Parser;
use super::expr::parse_expr;
use super::query::parse_query;
use super::query_from::parse_optional_system_time;
use super::token::TokenKind;
use super::types::parse_data_type;

pub(super) fn parse_create_table_kind(
    p: &mut Parser,
    or_replace: bool,
    temp: bool,
    kind_override: Option<CreateTableKind>,
) -> BqResult<CreateTableStmt> {
    let snapshot = kind_override == Some(CreateTableKind::Snapshot);
    let external = kind_override == Some(CreateTableKind::External);
    if snapshot {
        p.expect_kw("SNAPSHOT")?;
    } else if external {
        p.expect_kw("EXTERNAL")?;
    }
    p.expect_kw("TABLE")?;
    let if_not_exists = parse_if_not_exists(p)?;
    let name = p.parse_object_name()?;
    let (columns, constraints) = if p.eat(&TokenKind::LParen) {
        parse_table_elements(p)?
    } else {
        (Vec::new(), Vec::new())
    };
    let (parsed_kind, source_table, system_time) = parse_table_source_clause(p, snapshot)?;
    let kind = if external {
        CreateTableKind::External
    } else {
        parsed_kind
    };
    let partition_by = if p.eat_kw("PARTITION") {
        p.expect_kw("BY")?;
        Some(parse_expr(p)?)
    } else {
        None
    };
    let cluster_by = if p.eat_kw("CLUSTER") {
        p.expect_kw("BY")?;
        p.parse_comma_list(Parser::parse_ident)?
    } else {
        Vec::new()
    };
    let options = p.parse_options_clause()?;
    let as_query = if p.eat_kw("AS") {
        Some(Box::new(parse_query(p)?))
    } else {
        None
    };
    Ok(CreateTableStmt {
        or_replace,
        temp,
        if_not_exists,
        kind,
        name,
        columns,
        constraints,
        source_table,
        system_time,
        partition_by,
        cluster_by,
        options,
        as_query,
    })
}

fn parse_table_source_clause(
    p: &mut Parser,
    snapshot: bool,
) -> BqResult<(
    CreateTableKind,
    Option<crate::ast::ObjectName>,
    Option<crate::ast::Expr>,
)> {
    if snapshot {
        p.expect_kw("CLONE")?;
        let src = p.parse_object_name()?;
        let st = parse_optional_system_time(p)?;
        return Ok((CreateTableKind::Snapshot, Some(src), st));
    }
    if p.eat_kw("LIKE") {
        return Ok((CreateTableKind::Like, Some(p.parse_object_name()?), None));
    }
    if p.eat_kw("COPY") {
        return Ok((CreateTableKind::Copy, Some(p.parse_object_name()?), None));
    }
    if p.eat_kw("CLONE") {
        let src = p.parse_object_name()?;
        let st = parse_optional_system_time(p)?;
        return Ok((CreateTableKind::Clone, Some(src), st));
    }
    Ok((CreateTableKind::Table, None, None))
}

fn parse_table_elements(p: &mut Parser) -> BqResult<(Vec<ColumnDef>, Vec<TableConstraint>)> {
    let mut columns = Vec::new();
    let mut constraints = Vec::new();
    if p.eat(&TokenKind::RParen) {
        return Ok((columns, constraints));
    }
    loop {
        if p.is_kw("PRIMARY") || p.is_kw("FOREIGN") || p.is_kw("CONSTRAINT") {
            constraints.push(parse_table_constraint(p)?);
        } else {
            columns.push(parse_column_def(p)?);
        }
        if !p.eat(&TokenKind::Comma) {
            break;
        }
        if p.check(&TokenKind::RParen) {
            break;
        }
    }
    p.expect(&TokenKind::RParen, "')'")?;
    Ok((columns, constraints))
}

pub(super) fn parse_column_def(p: &mut Parser) -> BqResult<ColumnDef> {
    let name = p.parse_ident()?;
    let ty = parse_data_type(p)?;
    let mut not_null = false;
    let mut default_expr = None;
    let mut options = Vec::new();
    loop {
        if p.eat_kw("NOT") {
            p.expect_kw("NULL")?;
            not_null = true;
        } else if p.eat_kw("DEFAULT") {
            default_expr = Some(parse_expr(p)?);
        } else if p.is_kw("OPTIONS") {
            options = p.parse_options_clause()?;
        } else {
            break;
        }
    }
    Ok(ColumnDef {
        name,
        ty,
        not_null,
        default_expr,
        options,
    })
}

fn parse_table_constraint(p: &mut Parser) -> BqResult<TableConstraint> {
    if p.eat_kw("CONSTRAINT") {
        let _ = p.parse_ident()?;
    }
    if p.eat_kw("PRIMARY") {
        p.expect_kw("KEY")?;
        let columns = parse_paren_idents(p)?;
        let enforced = parse_not_enforced(p)?;
        return Ok(TableConstraint::PrimaryKey { columns, enforced });
    }
    p.expect_kw("FOREIGN")?;
    p.expect_kw("KEY")?;
    let columns = parse_paren_idents(p)?;
    p.expect_kw("REFERENCES")?;
    let ref_table = p.parse_object_name()?;
    let ref_columns = parse_paren_idents(p)?;
    let enforced = parse_not_enforced(p)?;
    Ok(TableConstraint::ForeignKey {
        columns,
        ref_table,
        ref_columns,
        enforced,
    })
}

fn parse_not_enforced(p: &mut Parser) -> BqResult<bool> {
    if p.eat_kw("NOT") {
        p.expect_kw("ENFORCED")?;
        Ok(false)
    } else {
        Ok(true)
    }
}

pub(super) fn parse_alter_table(p: &mut Parser) -> BqResult<AlterTableStmt> {
    p.expect_kw("ALTER")?;
    p.expect_kw("TABLE")?;
    let if_exists = parse_if_exists(p)?;
    let name = p.parse_object_name()?;
    let actions = p.parse_comma_list(parse_alter_action)?;
    Ok(AlterTableStmt {
        if_exists,
        name,
        actions,
    })
}

fn parse_alter_action(p: &mut Parser) -> BqResult<AlterTableAction> {
    if p.eat_kw("ADD") {
        p.expect_kw("COLUMN")?;
        let if_not_exists = parse_if_not_exists(p)?;
        return Ok(AlterTableAction::AddColumn {
            if_not_exists,
            column: parse_column_def(p)?,
        });
    }
    if p.eat_kw("DROP") {
        p.expect_kw("COLUMN")?;
        let if_exists = parse_if_exists(p)?;
        return Ok(AlterTableAction::DropColumn {
            if_exists,
            name: p.parse_ident()?,
        });
    }
    if p.eat_kw("RENAME") {
        if p.eat_kw("COLUMN") {
            let from = p.parse_ident()?;
            p.expect_kw("TO")?;
            return Ok(AlterTableAction::RenameColumn {
                from,
                to: p.parse_ident()?,
            });
        }
        p.expect_kw("TO")?;
        return Ok(AlterTableAction::RenameTo(p.parse_object_name()?));
    }
    if p.is_kw("SET") {
        p.advance();
        return Ok(AlterTableAction::SetOptions(p.parse_options_clause()?));
    }
    if p.eat_kw("ALTER") {
        p.expect_kw("COLUMN")?;
        return parse_alter_column_action(p);
    }
    Err(BqError::syntax(
        "Unsupported ALTER TABLE action",
        p.current_span(),
    ))
}

fn parse_alter_column_action(p: &mut Parser) -> BqResult<AlterTableAction> {
    let name = p.parse_ident()?;
    if p.eat_kw("SET") {
        if p.eat_kw("DATA") {
            p.expect_kw("TYPE")?;
            return Ok(AlterTableAction::AlterColumnSetDataType {
                name,
                ty: parse_data_type(p)?,
            });
        }
        if p.is_kw("OPTIONS") {
            return Ok(AlterTableAction::AlterColumnSetOptions {
                name,
                options: p.parse_options_clause()?,
            });
        }
        p.expect_kw("DEFAULT")?;
        return Ok(AlterTableAction::AlterColumnSetDefault {
            name,
            expr: parse_expr(p)?,
        });
    }
    if p.eat_kw("DROP") {
        if p.eat_kw("DEFAULT") {
            return Ok(AlterTableAction::AlterColumnDropDefault { name });
        }
        p.expect_kw("NOT")?;
        p.expect_kw("NULL")?;
        return Ok(AlterTableAction::AlterColumnDropNotNull { name });
    }
    Err(BqError::syntax(
        "Expected SET or DROP in ALTER COLUMN",
        p.current_span(),
    ))
}

pub(super) fn parse_if_not_exists(p: &mut Parser) -> BqResult<bool> {
    if p.is_kw("IF") && p.is_kw_n(1, "NOT") {
        p.expect_kw("IF")?;
        p.expect_kw("NOT")?;
        p.expect_kw("EXISTS")?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub(super) fn parse_if_exists(p: &mut Parser) -> BqResult<bool> {
    if p.is_kw("IF") && p.is_kw_n(1, "EXISTS") {
        p.expect_kw("IF")?;
        p.expect_kw("EXISTS")?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub(super) fn parse_paren_idents(p: &mut Parser) -> BqResult<Vec<Ident>> {
    p.expect(&TokenKind::LParen, "'('")?;
    let cols = if p.check(&TokenKind::RParen) {
        Vec::new()
    } else {
        p.parse_comma_list(Parser::parse_ident)?
    };
    p.expect(&TokenKind::RParen, "')'")?;
    Ok(cols)
}
