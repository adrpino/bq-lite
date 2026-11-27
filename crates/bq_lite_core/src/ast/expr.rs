use serde::{Deserialize, Serialize};

use super::{DataTypeAst, Ident, ObjectName, OrderByExpr, Query, WindowSpec};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Literal {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Numeric(String),
    BigNumeric(String),
    String(String),
    Bytes(Vec<u8>),
    Date(String),
    Datetime(String),
    Time(String),
    Timestamp(String),
    Json(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Concat,
    Eq,
    Neq,
    Lt,
    Lte,
    Gt,
    Gte,
    IsDistinctFrom,
    IsNotDistinctFrom,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    ShiftLeft,
    ShiftRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnaryOp {
    Plus,
    Minus,
    Not,
    BitNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IsTest {
    Null,
    NotNull,
    True,
    NotTrue,
    False,
    NotFalse,
    Unknown,
    NotUnknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NullHandling {
    IgnoreNulls,
    RespectNulls,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HavingKind {
    Max,
    Min,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionArg {
    pub name: Option<Ident>,
    pub expr: Expr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: ObjectName,
    pub safe_prefix: bool,
    pub distinct: bool,
    pub args: Vec<FunctionArg>,
    pub null_handling: Option<NullHandling>,
    pub order_by: Vec<OrderByExpr>,
    pub limit: Option<Box<Expr>>,
    pub having: Option<(HavingKind, Box<Expr>)>,
    pub over: Option<WindowSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArrayAccessKind {
    Raw,
    Offset,
    SafeOffset,
    Ordinal,
    SafeOrdinal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructFieldExpr {
    pub name: Option<Ident>,
    pub expr: Expr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WhenBranch {
    pub condition: Expr,
    pub result: Expr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InTarget {
    List(Vec<Expr>),
    Subquery(Box<Query>),
    Unnest(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Expr {
    Literal(Literal),
    Ident(Ident),
    CompoundIdent(Vec<Ident>),
    Parameter(String),
    PositionalParam(usize),
    SystemVar(String),
    Star,
    QualifiedStar(ObjectName),
    ExprStar(Box<Self>),
    WithExpr {
        bindings: Vec<(Ident, Self)>,
        body: Box<Self>,
    },
    Binary {
        left: Box<Self>,
        op: BinaryOp,
        right: Box<Self>,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Self>,
    },
    Is {
        expr: Box<Self>,
        test: IsTest,
    },
    Between {
        expr: Box<Self>,
        negated: bool,
        low: Box<Self>,
        high: Box<Self>,
    },
    Like {
        expr: Box<Self>,
        negated: bool,
        pattern: Box<Self>,
    },
    In {
        expr: Box<Self>,
        negated: bool,
        target: InTarget,
    },
    Cast {
        expr: Box<Self>,
        ty: DataTypeAst,
        safe: bool,
        format: Option<Box<Self>>,
        time_zone: Option<Box<Self>>,
    },
    Case {
        operand: Option<Box<Self>>,
        whens: Vec<WhenBranch>,
        else_result: Option<Box<Self>>,
    },
    Function(Box<FunctionCall>),
    ArrayLiteral {
        elem_type: Option<DataTypeAst>,
        elements: Vec<Self>,
    },
    ArraySubquery(Box<Query>),
    StructLiteral {
        explicit_type: Option<DataTypeAst>,
        fields: Vec<StructFieldExpr>,
    },
    Subscript {
        expr: Box<Self>,
        kind: ArrayAccessKind,
        index: Box<Self>,
    },
    FieldAccess {
        expr: Box<Self>,
        field: Ident,
    },
    Extract {
        part: Ident,
        expr: Box<Self>,
        time_zone: Option<Box<Self>>,
    },
    Interval {
        value: Box<Self>,
        unit: Ident,
        end_unit: Option<Ident>,
    },
    RangeLiteral {
        elem_type: DataTypeAst,
        start: Box<Self>,
        end: Box<Self>,
    },
    Lambda {
        params: Vec<Ident>,
        body: Box<Self>,
    },
    Exists(Box<Query>),
    Subquery(Box<Query>),
    Grouped(Box<Self>),
}
