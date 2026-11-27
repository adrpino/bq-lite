use crate::error::SourceSpan;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TokenKind {
    Ident(String),
    QuotedIdent(String),
    Number(String),
    String(String),
    Bytes(Vec<u8>),
    Param(String),
    PositionalParam(usize),
    SystemVar(String),

    // Punctuation & operators
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Dot,
    Semicolon,
    Colon,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Pipe,
    PipeGt,
    Concat,
    Amp,
    Caret,
    Tilde,
    Eq,
    Neq,
    Lt,
    Lte,
    Gt,
    Gte,
    ShiftLeft,
    ShiftRight,
    Arrow,
    FatArrow,
    Eof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Token {
    pub kind: TokenKind,
    pub span: SourceSpan,
}

impl Token {
    #[must_use]
    pub(super) fn is_kw(&self, kw: &str) -> bool {
        matches!(&self.kind, TokenKind::Ident(s) if s.eq_ignore_ascii_case(kw))
    }
}

/// Core `GoogleSQL` reserved keywords that cannot be used as unquoted aliases or column names.
const RESERVED_KEYWORDS: &[&str] = &[
    "ALL",
    "AND",
    "ANY",
    "ARRAY",
    "AS",
    "ASC",
    "ASSERT_ROWS_MODIFIED",
    "BETWEEN",
    "BY",
    "CASE",
    "CAST",
    "COLLATE",
    "CONTAINS",
    "CREATE",
    "CROSS",
    "CUBE",
    "CURRENT",
    "DEFAULT",
    "DEFINE",
    "DESC",
    "DISTINCT",
    "ELSE",
    "END",
    "ENUM",
    "ESCAPE",
    "EXCEPT",
    "EXCLUDE",
    "EXISTS",
    "EXTRACT",
    "FALSE",
    "FETCH",
    "FOLLOWING",
    "FOR",
    "FROM",
    "FULL",
    "GROUP",
    "GROUPING",
    "GROUPS",
    "HASH",
    "HAVING",
    "IF",
    "IGNORE",
    "IN",
    "INNER",
    "INTERSECT",
    "INTERVAL",
    "INTO",
    "IS",
    "JOIN",
    "LATERAL",
    "LEFT",
    "LIKE",
    "LIMIT",
    "LOOKUP",
    "MERGE",
    "NATURAL",
    "NEW",
    "NO",
    "NOT",
    "NULL",
    "NULLS",
    "OF",
    "ON",
    "OR",
    "ORDER",
    "OUTER",
    "OVER",
    "PARTITION",
    "PRECEDING",
    "PROTO",
    "QUALIFY",
    "RANGE",
    "RECURSIVE",
    "RESPECT",
    "RIGHT",
    "ROLLUP",
    "ROWS",
    "SELECT",
    "SET",
    "SOME",
    "STRUCT",
    "TABLESAMPLE",
    "THEN",
    "TO",
    "TREAT",
    "TRUE",
    "UNBOUNDED",
    "UNION",
    "UNNEST",
    "USING",
    "WHEN",
    "WHERE",
    "WINDOW",
    "WITH",
    "WITHIN",
];

#[must_use]
pub(super) fn is_reserved_keyword(word: &str) -> bool {
    let upper = word.to_ascii_uppercase();
    RESERVED_KEYWORDS.binary_search(&upper.as_str()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_keyword_list_is_sorted_and_accurate() {
        for pair in RESERVED_KEYWORDS.windows(2) {
            assert!(
                pair[0] < pair[1],
                "unsorted keyword: {} >= {}",
                pair[0],
                pair[1]
            );
        }
        assert!(is_reserved_keyword("select"));
        assert!(is_reserved_keyword("FROM"));
        assert!(!is_reserved_keyword("date"));
        assert!(!is_reserved_keyword("status"));
    }
}
