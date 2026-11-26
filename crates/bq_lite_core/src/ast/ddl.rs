use serde::{Deserialize, Serialize};

use super::{DataTypeAst, Expr, Ident, ObjectName, OptionEntry, Query};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnDef {
    pub name: Ident,
    pub ty: DataTypeAst,
    pub not_null: bool,
    pub default_expr: Option<Expr>,
    pub options: Vec<OptionEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TableConstraint {
    PrimaryKey {
        columns: Vec<Ident>,
        enforced: bool,
    },
    ForeignKey {
        columns: Vec<Ident>,
        ref_table: ObjectName,
        ref_columns: Vec<Ident>,
        enforced: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CreateTableKind {
    Table,
    Like,
    Copy,
    Clone,
    Snapshot,
    External,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateTableStmt {
    pub or_replace: bool,
    pub temp: bool,
    pub if_not_exists: bool,
    pub kind: CreateTableKind,
    pub name: ObjectName,
    pub columns: Vec<ColumnDef>,
    pub constraints: Vec<TableConstraint>,
    pub source_table: Option<ObjectName>,
    pub system_time: Option<Expr>,
    pub partition_by: Option<Expr>,
    pub cluster_by: Vec<Ident>,
    pub options: Vec<OptionEntry>,
    pub as_query: Option<Box<Query>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AlterTableAction {
    AddColumn {
        if_not_exists: bool,
        column: ColumnDef,
    },
    DropColumn {
        if_exists: bool,
        name: Ident,
    },
    RenameColumn {
        from: Ident,
        to: Ident,
    },
    RenameTo(ObjectName),
    SetOptions(Vec<OptionEntry>),
    AlterColumnSetDataType {
        name: Ident,
        ty: DataTypeAst,
    },
    AlterColumnSetDefault {
        name: Ident,
        expr: Expr,
    },
    AlterColumnDropDefault {
        name: Ident,
    },
    AlterColumnDropNotNull {
        name: Ident,
    },
    AlterColumnSetOptions {
        name: Ident,
        options: Vec<OptionEntry>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlterTableStmt {
    pub if_exists: bool,
    pub name: ObjectName,
    pub actions: Vec<AlterTableAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DropObjectKind {
    Table,
    ExternalTable,
    Schema,
    View,
    MaterializedView,
    Function,
    TableFunction,
    Procedure,
    SnapshotTable,
    RowAccessPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DropStmt {
    pub kind: DropObjectKind,
    pub if_exists: bool,
    pub name: ObjectName,
    pub on_table: Option<ObjectName>,
    pub cascade: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateSchemaStmt {
    pub if_not_exists: bool,
    pub name: ObjectName,
    pub options: Vec<OptionEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateViewStmt {
    pub or_replace: bool,
    pub materialized: bool,
    pub if_not_exists: bool,
    pub name: ObjectName,
    pub columns: Vec<Ident>,
    pub options: Vec<OptionEntry>,
    pub query: Box<Query>,
    #[serde(default)]
    pub view_sql: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineParam {
    pub mode: Option<ParamMode>,
    pub name: Ident,
    pub ty: DataTypeAst,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParamMode {
    In,
    Out,
    InOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FunctionKind {
    Scalar,
    Table,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FunctionBody {
    Expr(Box<Expr>),
    Query(Box<Query>),
    Code(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateFunctionStmt {
    pub or_replace: bool,
    pub temp: bool,
    pub kind: FunctionKind,
    pub if_not_exists: bool,
    pub name: ObjectName,
    pub params: Vec<RoutineParam>,
    pub returns: Option<DataTypeAst>,
    pub language: Option<String>,
    pub options: Vec<OptionEntry>,
    pub body: FunctionBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateProcedureStmt {
    pub or_replace: bool,
    pub if_not_exists: bool,
    pub name: ObjectName,
    pub params: Vec<RoutineParam>,
    pub options: Vec<OptionEntry>,
    pub body: Vec<super::Statement>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateRowAccessPolicyStmt {
    pub or_replace: bool,
    pub if_not_exists: bool,
    pub policy_name: Ident,
    pub table_name: ObjectName,
    pub grantees: Vec<String>,
    pub filter_expr: Expr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportDataStmt {
    pub options: Vec<OptionEntry>,
    pub query: Box<Query>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlterSchemaStmt {
    pub if_exists: bool,
    pub name: ObjectName,
    pub options: Vec<OptionEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlterViewStmt {
    pub if_exists: bool,
    pub materialized: bool,
    pub name: ObjectName,
    pub options: Vec<OptionEntry>,
    pub query: Option<Box<Query>>,
    pub view_sql: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadDataStmt {
    pub overwrite: bool,
    pub table: ObjectName,
    pub columns: Vec<ColumnDef>,
    pub options: Vec<OptionEntry>,
}
