use serde::{Deserialize, Serialize};

use super::{Expr, Ident, ObjectName, Query};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InsertSource {
    Values(Vec<Vec<Expr>>),
    Query(Box<Query>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InsertStmt {
    pub table: ObjectName,
    pub columns: Vec<Ident>,
    pub source: InsertSource,
    pub returning: Option<Vec<super::SelectItem>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateAssignment {
    pub path: Vec<Ident>,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateStmt {
    pub table: ObjectName,
    pub alias: Option<Ident>,
    pub assignments: Vec<UpdateAssignment>,
    pub from: Vec<super::TableWithJoins>,
    pub where_clause: Option<Expr>,
    pub returning: Option<Vec<super::SelectItem>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteStmt {
    pub table: ObjectName,
    pub alias: Option<Ident>,
    pub where_clause: Option<Expr>,
    pub returning: Option<Vec<super::SelectItem>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeConditionKind {
    Matched,
    NotMatchedByTarget,
    NotMatchedBySource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MergeAction {
    Update(Vec<UpdateAssignment>),
    Delete,
    Insert {
        columns: Vec<Ident>,
        values: Option<Vec<Expr>>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeClause {
    pub kind: MergeConditionKind,
    pub predicate: Option<Expr>,
    pub action: MergeAction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeStmt {
    pub target: ObjectName,
    pub target_alias: Option<Ident>,
    pub source: super::TableFactor,
    pub on: Expr,
    pub clauses: Vec<MergeClause>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TruncateStmt {
    pub table: ObjectName,
}
