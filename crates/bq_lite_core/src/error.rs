use std::fmt;
use thiserror::Error;

/// Convenience result alias with boxed [`BqError`] to keep stack frames compact.
pub type BqResult<T> = Result<T, Box<BqError>>;

/// Byte offset and 1-based line/column span in a `GoogleSQL` query string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceSpan {
    pub offset: usize,
    pub line: usize,
    pub column: usize,
    pub length: usize,
}

impl SourceSpan {
    #[must_use]
    pub const fn new(offset: usize, line: usize, column: usize, length: usize) -> Self {
        Self {
            offset,
            line,
            column,
            length,
        }
    }
}

impl fmt::Display for SourceSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}:{}]", self.line, self.column)
    }
}

/// Classification of `BigQuery` engine and API errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BqErrorKind {
    Syntax,
    Query,
    Semantic,
    InvalidArgument,
    PermissionDenied,
    NotFound,
    AlreadyExists,
    Unimplemented,
    Internal,
}

impl BqErrorKind {
    /// Complete ordered list of all error variants for parity verification.
    pub const ALL: &'static [Self] = &[
        Self::Syntax,
        Self::Query,
        Self::Semantic,
        Self::InvalidArgument,
        Self::PermissionDenied,
        Self::NotFound,
        Self::AlreadyExists,
        Self::Unimplemented,
        Self::Internal,
    ];

    /// Canonical Rust variant name (matches `docs/error-parity.md`).
    pub const fn variant_name(self) -> &'static str {
        match self {
            Self::Syntax => "Syntax",
            Self::Query => "Query",
            Self::Semantic => "Semantic",
            Self::InvalidArgument => "InvalidArgument",
            Self::PermissionDenied => "PermissionDenied",
            Self::NotFound => "NotFound",
            Self::AlreadyExists => "AlreadyExists",
            Self::Unimplemented => "Unimplemented",
            Self::Internal => "Internal",
        }
    }

    /// HTTP status code returned by the `BigQuery` REST v2 API.
    pub const fn http_status(self) -> u16 {
        match self {
            Self::Syntax | Self::Query | Self::Semantic | Self::InvalidArgument => 400,
            Self::PermissionDenied => 403,
            Self::NotFound => 404,
            Self::AlreadyExists => 409,
            Self::Unimplemented => 501,
            Self::Internal => 500,
        }
    }

    /// Google Cloud API `errors[].reason` string.
    pub const fn api_reason(self) -> &'static str {
        match self {
            Self::Syntax | Self::Query | Self::Semantic => "invalidQuery",
            Self::InvalidArgument => "invalid",
            Self::PermissionDenied => "accessDenied",
            Self::NotFound => "notFound",
            Self::AlreadyExists => "duplicate",
            Self::Unimplemented => "notImplemented",
            Self::Internal => "internalError",
        }
    }

    /// Google Cloud API top-level `status` string.
    pub const fn api_status(self) -> &'static str {
        match self {
            Self::Syntax | Self::Query | Self::Semantic | Self::InvalidArgument => {
                "INVALID_ARGUMENT"
            }
            Self::PermissionDenied => "PERMISSION_DENIED",
            Self::NotFound => "NOT_FOUND",
            Self::AlreadyExists => "ALREADY_EXISTS",
            Self::Unimplemented => "UNIMPLEMENTED",
            Self::Internal => "INTERNAL",
        }
    }

    /// Whether `SAFE.` function prefix suppresses this error kind into `NULL`.
    pub const fn is_safe_suppressible(self) -> bool {
        matches!(self, Self::Semantic)
    }
}

/// Structured error returned by `bq_lite_core`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{}", self.format_display())]
pub struct BqError {
    pub kind: BqErrorKind,
    pub message: String,
    pub span: Option<SourceSpan>,
}

impl BqError {
    #[must_use]
    pub fn new(kind: BqErrorKind, message: impl Into<String>) -> Box<Self> {
        Box::new(Self {
            kind,
            message: message.into(),
            span: None,
        })
    }

    #[must_use]
    pub fn syntax(message: impl Into<String>, span: SourceSpan) -> Box<Self> {
        Box::new(Self {
            kind: BqErrorKind::Syntax,
            message: message.into(),
            span: Some(span),
        })
    }

    #[must_use]
    pub fn query(message: impl Into<String>) -> Box<Self> {
        Self::new(BqErrorKind::Query, message)
    }

    #[must_use]
    pub fn semantic(message: impl Into<String>) -> Box<Self> {
        Self::new(BqErrorKind::Semantic, message)
    }

    #[must_use]
    pub fn invalid_argument(message: impl Into<String>) -> Box<Self> {
        Self::new(BqErrorKind::InvalidArgument, message)
    }

    #[must_use]
    pub fn permission_denied(message: impl Into<String>) -> Box<Self> {
        Self::new(BqErrorKind::PermissionDenied, message)
    }

    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Box<Self> {
        Self::new(BqErrorKind::NotFound, message)
    }

    #[must_use]
    pub fn already_exists(message: impl Into<String>) -> Box<Self> {
        Self::new(BqErrorKind::AlreadyExists, message)
    }

    #[must_use]
    pub fn unimplemented(message: impl Into<String>) -> Box<Self> {
        Self::new(BqErrorKind::Unimplemented, message)
    }

    #[must_use]
    pub fn internal(message: impl Into<String>) -> Box<Self> {
        Self::new(BqErrorKind::Internal, message)
    }

    #[must_use]
    pub const fn with_span(mut self: Box<Self>, span: SourceSpan) -> Box<Self> {
        self.span = Some(span);
        self
    }

    pub const fn http_status(&self) -> u16 {
        self.kind.http_status()
    }

    pub const fn api_reason(&self) -> &'static str {
        self.kind.api_reason()
    }

    pub const fn api_status(&self) -> &'static str {
        self.kind.api_status()
    }

    pub const fn is_safe_suppressible(&self) -> bool {
        self.kind.is_safe_suppressible()
    }

    fn format_display(&self) -> String {
        self.span.map_or_else(
            || format!("{}: {}", self.kind.variant_name(), self.message),
            |span| format!("{} at {}: {}", self.kind.variant_name(), span, self.message),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_suppression_only_applies_to_semantic_errors() {
        for &kind in BqErrorKind::ALL {
            let err = BqError::new(kind, "msg");
            assert_eq!(err.is_safe_suppressible(), kind == BqErrorKind::Semantic);
        }
    }

    #[test]
    fn syntax_error_formats_span() {
        let err = BqError::syntax("unexpected token", SourceSpan::new(10, 2, 5, 3));
        assert_eq!(err.http_status(), 400);
        assert_eq!(err.api_reason(), "invalidQuery");
        assert_eq!(err.api_status(), "INVALID_ARGUMENT");
        assert_eq!(err.to_string(), "Syntax at [2:5]: unexpected token");
    }
}
