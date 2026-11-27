use crate::ast::{Ident, ObjectName, OptionEntry};
use crate::error::{BqError, BqResult, SourceSpan};

use super::token::{is_reserved_keyword, Token, TokenKind};

const MAX_PARSE_DEPTH: usize = 48;

pub(super) struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
    pending_gt: usize,
    source: String,
}

impl Parser {
    #[must_use]
    pub(super) const fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            pos: 0,
            depth: 0,
            pending_gt: 0,
            source: String::new(),
        }
    }

    #[must_use]
    pub(super) const fn with_source(tokens: Vec<Token>, source: String) -> Self {
        Self {
            tokens,
            pos: 0,
            depth: 0,
            pending_gt: 0,
            source,
        }
    }

    #[must_use]
    pub(super) fn slice_from(&self, start_offset: usize) -> Option<String> {
        let end = self
            .pos
            .checked_sub(1)
            .and_then(|idx| self.tokens.get(idx))
            .map_or(self.source.len(), |t| t.span.offset + t.span.length);
        self.source
            .get(start_offset..end)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }

    pub(super) fn with_depth<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> BqResult<T>,
    ) -> BqResult<T> {
        if self.depth >= MAX_PARSE_DEPTH {
            return Err(BqError::syntax(
                "Maximum query nesting depth exceeded",
                self.current_span(),
            ));
        }
        self.depth += 1;
        let res = f(self);
        self.depth -= 1;
        res
    }

    #[must_use]
    pub(super) const fn save_pos(&self) -> usize {
        self.pos
    }

    pub(super) fn restore_pos(&mut self, pos: usize) {
        self.pos = pos;
        self.pending_gt = 0;
    }

    #[must_use]
    pub(super) fn peek(&self) -> &Token {
        self.tokens
            .get(self.pos)
            .or_else(|| self.tokens.last())
            .unwrap_or(&EOF_TOKEN)
    }

    #[must_use]
    pub(super) fn peek_n(&self, n: usize) -> &Token {
        self.tokens
            .get(self.pos + n)
            .or_else(|| self.tokens.last())
            .unwrap_or(&EOF_TOKEN)
    }

    #[must_use]
    pub(super) fn current_span(&self) -> SourceSpan {
        self.peek().span
    }

    #[must_use]
    pub(super) fn at_eof(&self) -> bool {
        self.pending_gt == 0 && matches!(self.peek().kind, TokenKind::Eof)
    }

    pub(super) fn advance(&mut self) -> Token {
        let tok = self.peek().clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    #[must_use]
    pub(super) fn is_kw(&self, kw: &str) -> bool {
        self.pending_gt == 0 && self.peek().is_kw(kw)
    }

    #[must_use]
    pub(super) fn is_kw_n(&self, n: usize, kw: &str) -> bool {
        self.pending_gt == 0 && self.peek_n(n).is_kw(kw)
    }

    pub(super) fn eat_kw(&mut self, kw: &str) -> bool {
        if self.is_kw(kw) {
            self.advance();
            true
        } else {
            false
        }
    }

    pub(super) fn expect_kw(&mut self, kw: &str) -> BqResult<()> {
        if self.eat_kw(kw) {
            Ok(())
        } else {
            Err(BqError::syntax(
                format!("Expected keyword '{kw}'"),
                self.current_span(),
            ))
        }
    }

    #[must_use]
    pub(super) fn check(&self, kind: &TokenKind) -> bool {
        self.pending_gt == 0 && &self.peek().kind == kind
    }

    pub(super) fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.check(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    pub(super) fn expect(&mut self, kind: &TokenKind, label: &str) -> BqResult<Token> {
        if self.check(kind) {
            Ok(self.advance())
        } else {
            Err(BqError::syntax(
                format!("Expected {label}"),
                self.current_span(),
            ))
        }
    }

    /// Consumes a single `>` inside angle-bracket type lists, splitting `>>` if encountered.
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "Only Gt and ShiftRight can close angle-bracket type lists"
    )]
    pub(super) fn expect_gt_angle(&mut self) -> BqResult<()> {
        if self.pending_gt > 0 {
            self.pending_gt -= 1;
            return Ok(());
        }
        match self.peek().kind {
            TokenKind::Gt => {
                self.advance();
                Ok(())
            }
            TokenKind::ShiftRight => {
                self.advance();
                self.pending_gt += 1;
                Ok(())
            }
            _ => Err(BqError::syntax("Expected '>'", self.current_span())),
        }
    }

    #[must_use]
    pub(super) fn at_gt_angle(&self) -> bool {
        self.pending_gt > 0 || matches!(self.peek().kind, TokenKind::Gt | TokenKind::ShiftRight)
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "Only Ident and QuotedIdent represent identifiers"
    )]
    pub(super) fn parse_ident(&mut self) -> BqResult<Ident> {
        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::Ident(s) => {
                self.advance();
                Ok(Ident::unquoted(s))
            }
            TokenKind::QuotedIdent(s) => {
                self.advance();
                Ok(Ident::quoted(s))
            }
            _ => Err(BqError::syntax("Expected identifier", tok.span)),
        }
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "Only unreserved Ident or QuotedIdent are valid unquoted aliases"
    )]
    pub(super) fn parse_unreserved_ident(&mut self) -> BqResult<Ident> {
        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::Ident(ref s) if !is_reserved_keyword(s) => {
                let val = s.clone();
                self.advance();
                Ok(Ident::unquoted(val))
            }
            TokenKind::QuotedIdent(ref s) => {
                let val = s.clone();
                self.advance();
                Ok(Ident::quoted(val))
            }
            _ => Err(BqError::syntax("Expected unreserved identifier", tok.span)),
        }
    }

    pub(super) fn parse_object_name(&mut self) -> BqResult<ObjectName> {
        let mut parts = vec![self.parse_ident()?];
        while self.eat(&TokenKind::Dot) {
            parts.push(self.parse_ident()?);
        }
        Ok(ObjectName::new(parts))
    }

    /// Parses a table/dataset/region object name allowing unquoted hyphenated segments like `region-us` or `my-proj`.
    pub(super) fn parse_table_object_name(&mut self) -> BqResult<ObjectName> {
        let mut parts = vec![self.parse_hyphenated_ident()?];
        while self.eat(&TokenKind::Dot) {
            parts.push(self.parse_hyphenated_ident()?);
        }
        Ok(ObjectName::new(parts))
    }

    fn parse_hyphenated_ident(&mut self) -> BqResult<Ident> {
        let mut ident = self.parse_ident()?;
        if ident.quoted {
            return Ok(ident);
        }
        while self.check(&TokenKind::Minus)
            && matches!(
                self.peek_n(1).kind,
                TokenKind::Ident(_) | TokenKind::Number(_)
            )
        {
            self.advance();
            if let TokenKind::Ident(next) | TokenKind::Number(next) = self.advance().kind {
                ident.value.push('-');
                ident.value.push_str(&next);
            }
        }
        Ok(ident)
    }

    pub(super) fn parse_comma_list<T>(
        &mut self,
        mut item: impl FnMut(&mut Self) -> BqResult<T>,
    ) -> BqResult<Vec<T>> {
        let mut out = vec![item(self)?];
        while self.eat(&TokenKind::Comma) {
            out.push(item(self)?);
        }
        Ok(out)
    }

    pub(super) fn parse_options_clause(&mut self) -> BqResult<Vec<OptionEntry>> {
        if !self.eat_kw("OPTIONS") {
            return Ok(Vec::new());
        }
        self.expect(&TokenKind::LParen, "'('")?;
        if self.eat(&TokenKind::RParen) {
            return Ok(Vec::new());
        }
        let entries = self.parse_comma_list(|p| {
            let name = p.parse_ident()?;
            p.expect(&TokenKind::Eq, "'='")?;
            let value = super::expr::parse_expr(p)?;
            Ok(OptionEntry { name, value })
        })?;
        self.expect(&TokenKind::RParen, "')'")?;
        Ok(entries)
    }
}

static EOF_TOKEN: Token = Token {
    kind: TokenKind::Eof,
    span: SourceSpan::new(0, 1, 1, 0),
};
