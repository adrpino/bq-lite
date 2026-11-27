use crate::error::{BqError, BqResult, SourceSpan};

pub(super) struct ScanCursor<'a> {
    src: &'a str,
    pos: usize,
    line: usize,
    col: usize,
}

impl<'a> ScanCursor<'a> {
    #[must_use]
    pub(super) const fn new(src: &'a str) -> Self {
        Self {
            src,
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    #[must_use]
    pub(super) const fn pos(&self) -> usize {
        self.pos
    }

    #[must_use]
    pub(super) const fn line(&self) -> usize {
        self.line
    }

    #[must_use]
    pub(super) const fn col(&self) -> usize {
        self.col
    }

    #[must_use]
    pub(super) fn rest(&self) -> &'a str {
        self.src.get(self.pos..).unwrap_or("")
    }

    #[must_use]
    pub(super) fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    #[must_use]
    pub(super) fn peek_n(&self, n: usize) -> Option<char> {
        self.rest().chars().nth(n)
    }

    #[must_use]
    pub(super) fn starts_with(&self, pat: &str) -> bool {
        self.rest().starts_with(pat)
    }

    pub(super) fn advance(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        if ch == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(ch)
    }

    pub(super) fn advance_bytes(&mut self, count: usize) {
        for _ in 0..count {
            if self.advance().is_none() {
                break;
            }
        }
    }

    #[must_use]
    pub(super) const fn span_from(
        &self,
        start_pos: usize,
        start_line: usize,
        start_col: usize,
    ) -> SourceSpan {
        SourceSpan::new(
            start_pos,
            start_line,
            start_col,
            self.pos.saturating_sub(start_pos),
        )
    }
}

pub(super) fn scan_backtick_ident(c: &mut ScanCursor<'_>) -> BqResult<String> {
    let (sp, sl, sc) = (c.pos(), c.line(), c.col());
    c.advance();
    let mut out = String::new();
    while let Some(ch) = c.advance() {
        match ch {
            '`' => return Ok(out),
            '\\' => out.push(scan_escape_char(c, sp, sl, sc)?),
            other => out.push(other),
        }
    }
    Err(BqError::syntax(
        "Unterminated backtick identifier",
        c.span_from(sp, sl, sc),
    ))
}

pub(super) fn scan_quoted_string(
    c: &mut ScanCursor<'_>,
    quote: char,
    raw: bool,
    is_bytes: bool,
) -> BqResult<Vec<u8>> {
    let (sp, sl, sc) = (c.pos(), c.line(), c.col());
    let triple = c.peek_n(1) == Some(quote) && c.peek_n(2) == Some(quote);
    c.advance_bytes(if triple { 3 } else { 1 });

    let mut out = Vec::new();
    loop {
        if is_closing_quote(c, quote, triple) {
            c.advance_bytes(if triple { 3 } else { 1 });
            return Ok(out);
        }
        let Some(ch) = c.advance() else {
            return Err(BqError::syntax(
                "Unterminated string literal",
                c.span_from(sp, sl, sc),
            ));
        };
        if !triple && (ch == '\n' || ch == '\r') {
            return Err(BqError::syntax(
                "Unescaped newline in single-quoted string literal",
                c.span_from(sp, sl, sc),
            ));
        }
        if !raw && ch == '\\' {
            push_escaped_byte(&mut out, c, is_bytes, sp, sl, sc)?;
        } else {
            push_char_bytes(&mut out, ch);
        }
    }
}

fn is_closing_quote(c: &ScanCursor<'_>, quote: char, triple: bool) -> bool {
    if triple {
        c.peek() == Some(quote) && c.peek_n(1) == Some(quote) && c.peek_n(2) == Some(quote)
    } else {
        c.peek() == Some(quote)
    }
}

fn push_char_bytes(out: &mut Vec<u8>, ch: char) {
    let mut buf = [0_u8; 4];
    let encoded = ch.encode_utf8(&mut buf);
    out.extend_from_slice(encoded.as_bytes());
}

fn push_escaped_byte(
    out: &mut Vec<u8>,
    c: &mut ScanCursor<'_>,
    is_bytes: bool,
    sp: usize,
    sl: usize,
    sc: usize,
) -> BqResult<()> {
    if let (Some(d0), Some(d1), Some(d2)) = (c.peek(), c.peek_n(1), c.peek_n(2)) {
        if ('0'..='3').contains(&d0) && ('0'..='7').contains(&d1) && ('0'..='7').contains(&d2) {
            c.advance();
            c.advance();
            c.advance();
            let v = (d0 as u8 - b'0') * 64 + (d1 as u8 - b'0') * 8 + (d2 as u8 - b'0');
            out.push(v);
            return Ok(());
        }
    }
    if matches!(c.peek(), Some('x' | 'X')) {
        c.advance();
        let val = parse_hex_n(c, 2, sp, sl, sc)?;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "2 hex digits always fit in u8"
        )]
        if is_bytes {
            out.push(val as u8);
        } else {
            let ch = char::from_u32(val).ok_or_else(|| {
                BqError::syntax("Invalid \\x escape code", c.span_from(sp, sl, sc))
            })?;
            push_char_bytes(out, ch);
        }
        return Ok(());
    }
    let ch = scan_escape_char(c, sp, sl, sc)?;
    push_char_bytes(out, ch);
    Ok(())
}

fn scan_escape_char(c: &mut ScanCursor<'_>, sp: usize, sl: usize, sc: usize) -> BqResult<char> {
    let Some(esc) = c.advance() else {
        return Err(BqError::syntax(
            "Unterminated escape sequence",
            c.span_from(sp, sl, sc),
        ));
    };
    match esc {
        'a' => Ok('\x07'),
        'b' => Ok('\x08'),
        'f' => Ok('\x0c'),
        'n' => Ok('\n'),
        'r' => Ok('\r'),
        't' => Ok('\t'),
        'v' => Ok('\x0b'),
        '\\' => Ok('\\'),
        '\'' => Ok('\''),
        '"' => Ok('"'),
        '`' => Ok('`'),
        '?' => Ok('?'),
        'x' | 'X' => {
            let code = parse_hex_n(c, 2, sp, sl, sc)?;
            char::from_u32(code)
                .ok_or_else(|| BqError::syntax("Invalid \\x escape code", c.span_from(sp, sl, sc)))
        }
        'u' => {
            let code = parse_hex_n(c, 4, sp, sl, sc)?;
            char::from_u32(code)
                .ok_or_else(|| BqError::syntax("Invalid \\u escape code", c.span_from(sp, sl, sc)))
        }
        'U' => {
            let code = parse_hex_n(c, 8, sp, sl, sc)?;
            char::from_u32(code)
                .ok_or_else(|| BqError::syntax("Invalid \\U escape code", c.span_from(sp, sl, sc)))
        }
        other => Err(BqError::syntax(
            format!("Illegal escape sequence: \\{other}"),
            c.span_from(sp, sl, sc),
        )),
    }
}

fn parse_hex_n(
    c: &mut ScanCursor<'_>,
    digits: usize,
    sp: usize,
    sl: usize,
    sc: usize,
) -> BqResult<u32> {
    let mut acc = 0_u32;
    for _ in 0..digits {
        let Some(d) = c.advance().and_then(|ch| ch.to_digit(16)) else {
            return Err(BqError::syntax(
                "Invalid hex escape sequence",
                c.span_from(sp, sl, sc),
            ));
        };
        acc = (acc << 4) | d;
    }
    Ok(acc)
}
