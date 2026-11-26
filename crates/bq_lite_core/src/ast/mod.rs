use serde::{Deserialize, Serialize};

pub mod common;
pub mod ddl;
pub mod dml;
pub mod expr;
pub mod query;
pub mod script;

pub use common::{
    DataTypeAst, Ident, NamedWindowDef, ObjectName, OptionEntry, OrderByExpr, StructFieldAst,
    WindowFrame, WindowFrameBound, WindowFrameUnit, WindowSpec,
};
pub use ddl::{
    AlterSchemaStmt, AlterTableAction, AlterTableStmt, AlterViewStmt, ColumnDef,
    CreateFunctionStmt, CreateProcedureStmt, CreateRowAccessPolicyStmt, CreateSchemaStmt,
    CreateTableKind, CreateTableStmt, CreateViewStmt, DropObjectKind, DropStmt, ExportDataStmt,
    FunctionBody, FunctionKind, LoadDataStmt, ParamMode, RoutineParam, TableConstraint,
};
pub use dml::{
    DeleteStmt, InsertSource, InsertStmt, MergeAction, MergeClause, MergeConditionKind, MergeStmt,
    TruncateStmt, UpdateAssignment, UpdateStmt,
};
pub use expr::{
    ArrayAccessKind, BinaryOp, Expr, FunctionArg, FunctionCall, HavingKind, InTarget, IsTest,
    Literal, NullHandling, StructFieldExpr, UnaryOp, WhenBranch,
};
pub use query::{
    Cte, GroupByClause, GroupByItem, Join, JoinConstraint, JoinKind, PipeOp, PivotSpec, Query,
    ReplaceItem, Select, SelectAsKind, SelectItem, SetExpr, SetOpKind, SetQuantifier, TableFactor,
    TableWithJoins, UnpivotSpec, WithClause,
};
pub use script::{
    AssertStmt, BeginEndBlock, CallStmt, CaseStmt, CaseStmtBranch, DeclareStmt,
    ExecuteImmediateStmt, ForInStmt, IfBranch, IfStmt, RaiseStmt, RepeatStmt, SetStmt,
    TransactionStmt, WhileStmt,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Statement {
    Query(Box<Query>),
    Insert(Box<InsertStmt>),
    Update(Box<UpdateStmt>),
    Delete(Box<DeleteStmt>),
    Merge(Box<MergeStmt>),
    Truncate(TruncateStmt),
    CreateTable(Box<CreateTableStmt>),
    AlterTable(Box<AlterTableStmt>),
    AlterSchema(AlterSchemaStmt),
    AlterView(Box<AlterViewStmt>),
    Drop(DropStmt),
    CreateSchema(CreateSchemaStmt),
    CreateView(Box<CreateViewStmt>),
    CreateFunction(Box<CreateFunctionStmt>),
    CreateProcedure(Box<CreateProcedureStmt>),
    CreateRowAccessPolicy(Box<CreateRowAccessPolicyStmt>),
    ExportData(Box<ExportDataStmt>),
    LoadData(Box<LoadDataStmt>),
    Declare(Box<DeclareStmt>),
    Set(Box<SetStmt>),
    If(Box<IfStmt>),
    Case(Box<CaseStmt>),
    Loop(Vec<Self>),
    While(Box<WhileStmt>),
    Repeat(Box<RepeatStmt>),
    ForIn(Box<ForInStmt>),
    BeginEnd(Box<BeginEndBlock>),
    ExecuteImmediate(Box<ExecuteImmediateStmt>),
    Call(CallStmt),
    Raise(Box<RaiseStmt>),
    Assert(Box<AssertStmt>),
    Transaction(TransactionStmt),
    Break,
    Continue,
    Return,
}
