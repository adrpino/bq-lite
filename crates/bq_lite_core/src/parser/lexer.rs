use crate::error::{BqError, BqResult};

use super::lexer_strings::{scan_backtick_ident, scan_quoted_string, ScanCursor};
use super::token::{Token, TokenKind};

/// Tokenizes a `GoogleSQL` source string into a stream of [`Token`]s ending with [`TokenKind::Eof`].
pub(super) fn tokenize(src: &str) -> BqResult<Vec<Token>> {
    let mut c = ScanCursor::new(src);
    let mut tokens = Vec::new();
    let mut next_positional = 1_usize;

    loop {
        skip_ws_and_comments(&mut c)?;
        let (sp, sl, sc) = (c.pos(), c.line(), c.col());
        let Some(ch) = c.peek() else {
            tokens.push(Token {
                kind: TokenKind::Eof,
                span: c.span_from(sp, sl, sc),
            });
            return Ok(tokens);
        };
        let kind = scan_next_token(&mut c, ch, &mut next_positional, sp, sl, sc)?;
        tokens.push(Token {
            kind,
            span: c.span_from(sp, sl, sc),
        });
    }
}

fn scan_next_token(
    c: &mut ScanCursor<'_>,
    ch: char,
    next_pos: &mut usize,
    sp: usize,
    sl: usize,
    sc: usize,
) -> BqResult<TokenKind> {
    if let Some(prefixed) = try_scan_prefixed_string(c)? {
        return Ok(prefixed);
    }
    match ch {
        '`' => scan_backtick_ident(c).map(TokenKind::QuotedIdent),
        '\'' | '"' => {
            let bytes = scan_quoted_string(c, ch, false, false)?;
            let value = String::from_utf8(bytes)
                .map_err(|_| BqError::syntax("Invalid UTF-8 in string", c.span_from(sp, sl, sc)))?;
            Ok(TokenKind::String(value))
        }
        '@' => scan_at_token(c, sp, sl, sc),
        '?' => {
            c.advance();
            let idx = *next_pos;
            *next_pos += 1;
            Ok(TokenKind::PositionalParam(idx))
        }
        '0'..='9' => Ok(TokenKind::Number(scan_number(c))),
        '.' if c.peek_n(1).is_some_and(|n| n.is_ascii_digit()) => {
            Ok(TokenKind::Number(scan_number(c)))
        }
        _ if is_ident_start(ch) => Ok(TokenKind::Ident(scan_ident(c))),
        _ => scan_operator_or_punct(c, sp, sl, sc),
    }
}

fn skip_ws_and_comments(c: &mut ScanCursor<'_>) -> BqResult<()> {
    loop {
        while c.peek().is_some_and(char::is_whitespace) {
            c.advance();
        }
        if c.starts_with("--") || c.starts_with("#") {
            while let Some(ch) = c.advance() {
                if ch == '\n' {
                    break;
                }
            }
            continue;
        }
        if c.starts_with("/*") {
            skip_block_comment(c)?;
            continue;
        }
        return Ok(());
    }
}

fn skip_block_comment(c: &mut ScanCursor<'_>) -> BqResult<()> {
    let (sp, sl, sc) = (c.pos(), c.line(), c.col());
    c.advance_bytes(2);
    while !c.rest().is_empty() {
        if c.starts_with("*/") {
            c.advance_bytes(2);
            return Ok(());
        }
        c.advance();
    }
    Err(BqError::syntax(
        "Unterminated block comment",
        c.span_from(sp, sl, sc),
    ))
}

fn try_scan_prefixed_string(c: &mut ScanCursor<'_>) -> BqResult<Option<TokenKind>> {
    let rest = c.rest();
    let (prefix_len, raw, is_bytes) = if rest.starts_with("rb'")
        || rest.starts_with("rb\"")
        || rest.starts_with("RB'")
        || rest.starts_with("RB\"")
        || rest.starts_with("rB'")
        || rest.starts_with("rB\"")
        || rest.starts_with("Rb'")
        || rest.starts_with("Rb\"")
        || rest.starts_with("br'")
        || rest.starts_with("br\"")
        || rest.starts_with("BR'")
        || rest.starts_with("BR\"")
        || rest.starts_with("bR'")
        || rest.starts_with("bR\"")
        || rest.starts_with("Br'")
        || rest.starts_with("Br\"")
    {
        (2, true, true)
    } else if rest.starts_with("r'")
        || rest.starts_with("r\"")
        || rest.starts_with("R'")
        || rest.starts_with("R\"")
    {
        (1, true, false)
    } else if rest.starts_with("b'")
        || rest.starts_with("b\"")
        || rest.starts_with("B'")
        || rest.starts_with("B\"")
    {
        (1, false, true)
    } else {
        return Ok(None);
    };

    let (sp, sl, sc) = (c.pos(), c.line(), c.col());
    c.advance_bytes(prefix_len);
    let quote = c.peek().unwrap_or('\'');
    let bytes = scan_quoted_string(c, quote, raw, is_bytes)?;
    if is_bytes {
        Ok(Some(TokenKind::Bytes(bytes)))
    } else {
        let value = String::from_utf8(bytes)
            .map_err(|_| BqError::syntax("Invalid UTF-8 in string", c.span_from(sp, sl, sc)))?;
        Ok(Some(TokenKind::String(value)))
    }
}

fn scan_at_token(c: &mut ScanCursor<'_>, sp: usize, sl: usize, sc: usize) -> BqResult<TokenKind> {
    c.advance();
    if c.peek() == Some('@') {
        c.advance();
        let name = scan_dotted_ident(c, sp, sl, sc)?;
        return Ok(TokenKind::SystemVar(name));
    }
    let name = scan_ident_nonempty(c, sp, sl, sc)?;
    Ok(TokenKind::Param(name))
}

fn scan_dotted_ident(c: &mut ScanCursor<'_>, sp: usize, sl: usize, sc: usize) -> BqResult<String> {
    let mut name = scan_ident_nonempty(c, sp, sl, sc)?;
    while c.peek() == Some('.') && c.peek_n(1).is_some_and(is_ident_start) {
        c.advance();
        name.push('.');
        name.push_str(&scan_ident(c));
    }
    Ok(name)
}

fn scan_ident_nonempty(
    c: &mut ScanCursor<'_>,
    sp: usize,
    sl: usize,
    sc: usize,
) -> BqResult<String> {
    if !c.peek().is_some_and(is_ident_start) {
        return Err(BqError::syntax(
            "Expected identifier after '@'",
            c.span_from(sp, sl, sc),
        ));
    }
    Ok(scan_ident(c))
}

fn scan_ident(c: &mut ScanCursor<'_>) -> String {
    let mut out = String::new();
    while let Some(ch) = c.peek() {
        if is_ident_continue(ch) {
            out.push(ch);
            c.advance();
        } else {
            break;
        }
    }
    out
}

fn scan_number(c: &mut ScanCursor<'_>) -> String {
    let mut out = String::new();
    if (c.starts_with("0x") || c.starts_with("0X")) && c.peek_n(2).is_some() {
        out.push_str("0x");
        c.advance_bytes(2);
        while let Some(ch) = c.peek() {
            if ch.is_ascii_hexdigit() {
                out.push(ch);
                c.advance();
            } else {
                break;
            }
        }
        return out;
    }
    consume_digits(c, &mut out);
    if c.peek() == Some('.') && c.peek_n(1) != Some('.') {
        out.push('.');
        c.advance();
        consume_digits(c, &mut out);
    }
    if matches!(c.peek(), Some('e' | 'E')) {
        out.push('e');
        c.advance();
        if matches!(c.peek(), Some('+' | '-')) {
            if let Some(sign) = c.advance() {
                out.push(sign);
            }
        }
        consume_digits(c, &mut out);
    }
    out
}

fn consume_digits(c: &mut ScanCursor<'_>, out: &mut String) {
    while let Some(ch) = c.peek() {
        if ch.is_ascii_digit() {
            out.push(ch);
            c.advance();
        } else {
            break;
        }
    }
}

fn scan_operator_or_punct(
    c: &mut ScanCursor<'_>,
    sp: usize,
    sl: usize,
    sc: usize,
) -> BqResult<TokenKind> {
    let two_char = match c.rest().get(..2) {
        Some("|>") => Some(TokenKind::PipeGt),
        Some("||") => Some(TokenKind::Concat),
        Some("!=" | "<>") => Some(TokenKind::Neq),
        Some("<=") => Some(TokenKind::Lte),
        Some(">=") => Some(TokenKind::Gte),
        Some("<<") => Some(TokenKind::ShiftLeft),
        Some(">>") => Some(TokenKind::ShiftRight),
        Some("->") => Some(TokenKind::Arrow),
        Some("=>") => Some(TokenKind::FatArrow),
        _ => None,
    };
    if let Some(kind) = two_char {
        c.advance_bytes(2);
        return Ok(kind);
    }
    let Some(ch) = c.advance() else {
        return Ok(TokenKind::Eof);
    };
    single_char_token(ch).ok_or_else(|| {
        BqError::syntax(
            format!("Unexpected character '{ch}'"),
            c.span_from(sp, sl, sc),
        )
    })
}

const fn single_char_token(ch: char) -> Option<TokenKind> {
    match ch {
        '(' => Some(TokenKind::LParen),
        ')' => Some(TokenKind::RParen),
        '[' => Some(TokenKind::LBracket),
        ']' => Some(TokenKind::RBracket),
        '{' => Some(TokenKind::LBrace),
        '}' => Some(TokenKind::RBrace),
        ',' => Some(TokenKind::Comma),
        '.' => Some(TokenKind::Dot),
        ';' => Some(TokenKind::Semicolon),
        ':' => Some(TokenKind::Colon),
        '+' => Some(TokenKind::Plus),
        '-' => Some(TokenKind::Minus),
        '*' => Some(TokenKind::Star),
        '/' => Some(TokenKind::Slash),
        '%' => Some(TokenKind::Percent),
        '|' => Some(TokenKind::Pipe),
        '&' => Some(TokenKind::Amp),
        '^' => Some(TokenKind::Caret),
        '~' => Some(TokenKind::Tilde),
        '=' => Some(TokenKind::Eq),
        '<' => Some(TokenKind::Lt),
        '>' => Some(TokenKind::Gt),
        _ => None,
    }
}

const fn is_ident_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

const fn is_ident_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexes_backtick_triple_raw_bytes_and_pipe() {
        let tokens = tokenize(
            "SELECT `proj.ds.tbl`, r'a\\b', b'hi', \"\"\"multi\nline\"\"\", @p, @@error.message |> WHERE x != 1",
        )
        .unwrap();
        assert!(matches!(tokens[1].kind, TokenKind::QuotedIdent(ref s) if s == "proj.ds.tbl"));
        assert!(matches!(tokens[3].kind, TokenKind::String(ref value) if value == "a\\b"));
        assert!(matches!(tokens[5].kind, TokenKind::Bytes(ref b) if b == b"hi"));
        assert!(matches!(tokens[7].kind, TokenKind::String(ref value) if value == "multi\nline"));
        assert!(matches!(tokens[9].kind, TokenKind::Param(ref p) if p == "p"));
        assert!(matches!(tokens[11].kind, TokenKind::SystemVar(ref s) if s == "error.message"));
        assert_eq!(tokens[12].kind, TokenKind::PipeGt);
    }

    #[test]
    fn unterminated_constructs_return_syntax_errors_with_span() {
        assert!(tokenize("SELECT 'unterminated").is_err());
        assert!(tokenize("SELECT `unterminated").is_err());
        assert!(tokenize("/* unclosed").is_err());
    }
}
