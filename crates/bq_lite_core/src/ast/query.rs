use serde::{Deserialize, Serialize};

use super::{Expr, Ident, NamedWindowDef, ObjectName, OrderByExpr};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Query {
    pub with: Option<WithClause>,
    pub body: SetExpr,
    pub order_by: Vec<OrderByExpr>,
    pub limit: Option<Expr>,
    pub offset: Option<Expr>,
    pub pipe_ops: Vec<PipeOp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WithClause {
    pub recursive: bool,
    pub ctes: Vec<Cte>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cte {
    pub name: Ident,
    pub columns: Vec<Ident>,
    pub query: Box<Query>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetOpKind {
    Union,
    Intersect,
    Except,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetQuantifier {
    All,
    Distinct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetCorrespondence {
    Positional,
    ByName,
    Corresponding,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SetExpr {
    Select(Box<Select>),
    Query(Box<Query>),
    SetOperation {
        op: SetOpKind,
        quantifier: SetQuantifier,
        correspondence: SetCorrespondence,
        left: Box<Self>,
        right: Box<Self>,
    },
    Values(Vec<Vec<Expr>>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectAsKind {
    Struct,
    Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Select {
    pub distinct: bool,
    pub as_kind: Option<SelectAsKind>,
    pub projection: Vec<SelectItem>,
    pub from: Vec<TableWithJoins>,
    pub where_clause: Option<Expr>,
    pub group_by: Option<GroupByClause>,
    pub having: Option<Expr>,
    pub qualify: Option<Expr>,
    pub window: Vec<NamedWindowDef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SelectItem {
    Expr {
        expr: Box<Expr>,
        alias: Option<Ident>,
    },
    Wildcard {
        qualifier: Option<ObjectName>,
        except: Vec<Ident>,
        replace: Vec<ReplaceItem>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplaceItem {
    pub expr: Expr,
    pub alias: Ident,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GroupByClause {
    All,
    Items(Vec<GroupByItem>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GroupByItem {
    Expr(Box<Expr>),
    Tuple(Vec<Expr>),
    Rollup(Vec<Vec<Expr>>),
    Cube(Vec<Vec<Expr>>),
    GroupingSets(Vec<Self>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableWithJoins {
    pub relation: TableFactor,
    pub joins: Vec<Join>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TableFactor {
    Table {
        name: ObjectName,
        alias: Option<Ident>,
        system_time: Option<Expr>,
        tablesample_percent: Option<Box<Expr>>,
    },
    Derived {
        query: Box<Query>,
        alias: Option<Ident>,
    },
    Unnest {
        expr: Expr,
        alias: Option<Ident>,
        with_offset: Option<Option<Ident>>,
    },
    TableFunction {
        name: ObjectName,
        args: Vec<super::FunctionArg>,
        alias: Option<Ident>,
    },
    Pivot {
        source: Box<Self>,
        spec: PivotSpec,
        alias: Option<Ident>,
    },
    Unpivot {
        source: Box<Self>,
        spec: UnpivotSpec,
        alias: Option<Ident>,
    },
    NestedJoin(Box<TableWithJoins>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PivotSpec {
    pub aggregates: Vec<(Expr, Option<Ident>)>,
    pub for_col: Ident,
    pub in_values: Vec<(Expr, Option<Ident>)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnpivotSpec {
    pub include_nulls: bool,
    pub value_cols: Vec<Ident>,
    pub name_col: Ident,
    pub in_cols: Vec<(Vec<Ident>, Option<Expr>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinKind {
    Inner,
    Left,
    Right,
    Full,
    Cross,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JoinConstraint {
    None,
    On(Box<Expr>),
    Using(Vec<Ident>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Join {
    pub kind: JoinKind,
    pub relation: TableFactor,
    pub constraint: JoinConstraint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipeOrderDir {
    pub asc: bool,
    pub nulls_first: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipeOrderedItem {
    pub item: SelectItem,
    pub order: Option<PipeOrderDir>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipeGroupByItem {
    pub item: GroupByItem,
    pub alias: Option<Ident>,
    pub order: Option<PipeOrderDir>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PipeOp {
    Where(Box<Expr>),
    Select {
        items: Vec<SelectItem>,
        window: Vec<NamedWindowDef>,
    },
    Extend {
        items: Vec<SelectItem>,
        window: Vec<NamedWindowDef>,
    },
    Window {
        items: Vec<SelectItem>,
        window: Vec<NamedWindowDef>,
    },
    Set(Vec<(Ident, Expr)>),
    Drop(Vec<Ident>),
    Rename(Vec<(Ident, Ident)>),
    Aggregate {
        aggs: Vec<PipeOrderedItem>,
        group_by: Vec<PipeGroupByItem>,
        group_and_order_by: bool,
    },
    OrderBy(Vec<OrderByExpr>),
    Limit {
        limit: Option<Box<Expr>>,
        offset: Option<Box<Expr>>,
    },
    As(Ident),
    Distinct,
    Join(Box<Join>),
    SetOp {
        op: SetOpKind,
        quantifier: SetQuantifier,
        correspondence: SetCorrespondence,
        queries: Vec<Query>,
    },
    Pivot {
        spec: PivotSpec,
        alias: Option<Ident>,
    },
    Unpivot {
        spec: UnpivotSpec,
        alias: Option<Ident>,
    },
    TableSample {
        amount: Box<Expr>,
        is_rows: bool,
    },
    With(Vec<Cte>),
}
