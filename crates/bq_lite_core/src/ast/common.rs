use serde::{Deserialize, Serialize};

use crate::types::BqType;

/// An identifier with whether it was backtick-quoted.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Ident {
    pub value: String,
    pub quoted: bool,
}

impl Ident {
    #[must_use]
    pub fn unquoted(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            quoted: false,
        }
    }

    #[must_use]
    pub fn quoted(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            quoted: true,
        }
    }
}

/// Qualified multi-part name (`project.dataset.table` or single backticked `proj.ds.table`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectName {
    pub parts: Vec<Ident>,
}

impl ObjectName {
    #[must_use]
    pub const fn new(parts: Vec<Ident>) -> Self {
        Self { parts }
    }

    #[must_use]
    pub fn normalized_parts(&self) -> Vec<String> {
        let mut out = Vec::new();
        for part in &self.parts {
            if part.quoted && part.value.contains('.') {
                for seg in part.value.split('.') {
                    out.push(seg.to_owned());
                }
            } else {
                out.push(part.value.clone());
            }
        }
        out
    }
}

/// Parsed `GoogleSQL` data type annotation (preserving optional length/precision constraints).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataTypeAst {
    Bool,
    Int64,
    Float64,
    Numeric {
        precision: Option<u32>,
        scale: Option<u32>,
    },
    BigNumeric {
        precision: Option<u32>,
        scale: Option<u32>,
    },
    String {
        max_len: Option<u64>,
    },
    Bytes {
        max_len: Option<u64>,
    },
    Date,
    Datetime,
    Time,
    Timestamp,
    Interval,
    Json,
    Geography,
    Range(Box<Self>),
    Array(Box<Self>),
    Struct(Vec<StructFieldAst>),
    Table(Vec<StructFieldAst>),
}

impl DataTypeAst {
    /// Converts AST type annotation into runtime [`BqType`].
    pub fn to_bq_type(&self) -> crate::error::BqResult<BqType> {
        match self {
            Self::Bool => Ok(BqType::Bool),
            Self::Int64 => Ok(BqType::Int64),
            Self::Float64 => Ok(BqType::Float64),
            Self::Numeric { .. } => Ok(BqType::Numeric),
            Self::BigNumeric { .. } => Ok(BqType::BigNumeric),
            Self::String { .. } => Ok(BqType::String),
            Self::Bytes { .. } => Ok(BqType::Bytes),
            Self::Date => Ok(BqType::Date),
            Self::Datetime => Ok(BqType::Datetime),
            Self::Time => Ok(BqType::Time),
            Self::Timestamp => Ok(BqType::Timestamp),
            Self::Interval => Ok(BqType::Interval),
            Self::Json => Ok(BqType::Json),
            Self::Geography => Ok(BqType::Geography),
            Self::Range(inner) => BqType::range(&inner.to_bq_type()?),
            Self::Array(inner) => BqType::array(inner.to_bq_type()?),
            Self::Struct(fields) => {
                let mut out = Vec::with_capacity(fields.len());
                for f in fields {
                    out.push(crate::types::StructField {
                        name: f.name.as_ref().map(|id| id.value.clone()),
                        ty: f.ty.to_bq_type()?,
                    });
                }
                Ok(BqType::Struct(out))
            }
            Self::Table(_) => Err(crate::error::BqError::query(
                "TABLE<...> type is only valid as a table-valued function parameter",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructFieldAst {
    pub name: Option<Ident>,
    pub ty: DataTypeAst,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderByExpr {
    pub expr: super::Expr,
    pub asc: Option<bool>,
    pub nulls_first: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptionEntry {
    pub name: Ident,
    pub value: super::Expr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowFrameUnit {
    Rows,
    Range,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WindowFrameBound {
    CurrentRow,
    Preceding(Option<Box<super::Expr>>),
    Following(Option<Box<super::Expr>>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowFrame {
    pub unit: WindowFrameUnit,
    pub start: WindowFrameBound,
    pub end: Option<WindowFrameBound>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowSpec {
    pub window_name: Option<Ident>,
    pub partition_by: Vec<super::Expr>,
    pub order_by: Vec<OrderByExpr>,
    pub frame: Option<WindowFrame>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedWindowDef {
    pub name: Ident,
    pub spec: WindowSpec,
}
