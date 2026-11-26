use serde::{Deserialize, Serialize};

use super::{DataTypeAst, Expr, Ident, ObjectName, Statement};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeclareStmt {
    pub vars: Vec<Ident>,
    pub ty: Option<DataTypeAst>,
    pub default_expr: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetStmt {
    pub targets: Vec<Ident>,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IfBranch {
    pub condition: Expr,
    pub body: Vec<Statement>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IfStmt {
    pub branches: Vec<IfBranch>,
    pub else_body: Option<Vec<Statement>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseStmtBranch {
    pub condition: Expr,
    pub body: Vec<Statement>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseStmt {
    pub operand: Option<Expr>,
    pub branches: Vec<CaseStmtBranch>,
    pub else_body: Option<Vec<Statement>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WhileStmt {
    pub condition: Expr,
    pub body: Vec<Statement>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepeatStmt {
    pub body: Vec<Statement>,
    pub until: Expr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForInStmt {
    pub var: Ident,
    pub query: Box<super::Query>,
    pub body: Vec<Statement>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeginEndBlock {
    pub body: Vec<Statement>,
    pub exception_body: Option<Vec<Statement>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecuteImmediateStmt {
    pub sql: Expr,
    pub into: Vec<Ident>,
    pub using: Vec<(Expr, Option<Ident>)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallStmt {
    pub name: ObjectName,
    pub args: Vec<Expr>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RaiseStmt {
    pub message: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssertStmt {
    pub condition: Expr,
    pub description: Option<Expr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionStmt {
    Begin,
    Commit,
    Rollback,
}
