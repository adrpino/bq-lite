use serde_json::Value as JsonValue;

use crate::error::{BqError, BqResult};

pub(super) enum PathSeg {
    Key(String),
    Idx(usize),
}

pub(super) fn resolve_json_path<'a>(
    doc: &'a JsonValue,
    path: &str,
    legacy_brackets: bool,
) -> BqResult<Option<&'a JsonValue>> {
    let segs = parse_json_path(path, legacy_brackets)?;
    let mut cur = doc;
    for seg in segs {
        match (cur, seg) {
            (JsonValue::Object(m), PathSeg::Key(k)) => match m.get(&k) {
                Some(next) => cur = next,
                None => return Ok(None),
            },
            (JsonValue::Array(a), PathSeg::Idx(i)) => match a.get(i) {
                Some(next) => cur = next,
                None => return Ok(None),
            },
            _ => return Ok(None),
        }
    }
    Ok(Some(cur))
}

pub(super) fn set_json_path(doc: &mut JsonValue, path: &str, val: JsonValue) -> BqResult<()> {
    let segs = parse_json_path(path, false)?;
    if segs.is_empty() {
        *doc = val;
        return Ok(());
    }
    let mut cur = doc;
    let last_idx = segs.len() - 1;
    for (i, seg) in segs.into_iter().enumerate() {
        if i == last_idx {
            match (cur, seg) {
                (JsonValue::Object(m), PathSeg::Key(k)) => {
                    m.insert(k, val);
                }
                (JsonValue::Array(a), PathSeg::Idx(idx)) if idx < a.len() => {
                    if let Some(slot) = a.get_mut(idx) {
                        *slot = val;
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        match (cur, seg) {
            (JsonValue::Object(m), PathSeg::Key(k)) => {
                if let Some(next) = m.get_mut(&k) {
                    cur = next;
                } else {
                    return Ok(());
                }
            }
            (JsonValue::Array(a), PathSeg::Idx(idx)) => {
                if let Some(next) = a.get_mut(idx) {
                    cur = next;
                } else {
                    return Ok(());
                }
            }
            _ => return Ok(()),
        }
    }
    Ok(())
}

pub(super) fn remove_json_path(doc: &mut JsonValue, path: &str) -> BqResult<()> {
    let segs = parse_json_path(path, false)?;
    let mut cur = doc;
    let Some((last, prefix)) = segs.split_last() else {
        return Ok(());
    };
    for seg in prefix {
        match (cur, seg) {
            (JsonValue::Object(m), PathSeg::Key(k)) => {
                if let Some(next) = m.get_mut(k) {
                    cur = next;
                } else {
                    return Ok(());
                }
            }
            (JsonValue::Array(a), PathSeg::Idx(idx)) => {
                if let Some(next) = a.get_mut(*idx) {
                    cur = next;
                } else {
                    return Ok(());
                }
            }
            _ => return Ok(()),
        }
    }
    match (cur, last) {
        (JsonValue::Object(m), PathSeg::Key(k)) => {
            m.remove(k);
        }
        (JsonValue::Array(a), PathSeg::Idx(idx)) if *idx < a.len() => {
            a.remove(*idx);
        }
        _ => {}
    }
    Ok(())
}

fn parse_json_path(path: &str, legacy_brackets: bool) -> BqResult<Vec<PathSeg>> {
    let rest = path
        .strip_prefix('$')
        .ok_or_else(|| BqError::semantic(format!("JSONPath must start with '$': '{path}'")))?;
    let mut out = Vec::new();
    let mut chars = rest.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '.' => out.push(PathSeg::Key(parse_dot_key(&mut chars, path)?)),
            '[' => out.push(parse_bracket_seg(&mut chars, path, legacy_brackets)?),
            _ => return Err(BqError::semantic(format!("Invalid JSONPath '{path}'"))),
        }
    }
    Ok(out)
}

fn parse_dot_key(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    path: &str,
) -> BqResult<String> {
    if chars.peek() == Some(&'"') {
        chars.next();
        return parse_quoted_key(chars, '"', path);
    }
    let mut key = String::new();
    while let Some(&next) = chars.peek() {
        if next == '.' || next == '[' {
            break;
        }
        key.push(next);
        chars.next();
    }
    if key.is_empty() {
        return Err(BqError::semantic(format!("Empty key in JSONPath '{path}'")));
    }
    Ok(key)
}

fn parse_bracket_seg(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    path: &str,
    legacy_brackets: bool,
) -> BqResult<PathSeg> {
    while chars.peek().is_some_and(char::is_ascii_whitespace) {
        chars.next();
    }
    if chars.peek() == Some(&'\'') {
        if !legacy_brackets {
            return Err(BqError::semantic(format!(
                "Invalid token in JSONPath '{path}'"
            )));
        }
        chars.next();
        let key = parse_quoted_key(chars, '\'', path)?;
        while chars.peek().is_some_and(char::is_ascii_whitespace) {
            chars.next();
        }
        if chars.next() != Some(']') {
            return Err(BqError::semantic(format!(
                "Unterminated bracket segment in JSONPath '{path}'"
            )));
        }
        return Ok(PathSeg::Key(key));
    }
    let mut inner = String::new();
    let mut closed = false;
    for c in chars.by_ref() {
        if c == ']' {
            closed = true;
            break;
        }
        inner.push(c);
    }
    if !closed {
        return Err(BqError::semantic(format!(
            "Unterminated bracket segment in JSONPath '{path}'"
        )));
    }
    let idx = inner
        .trim()
        .parse::<usize>()
        .map_err(|_| BqError::semantic(format!("Invalid array index in JSONPath '{path}'")))?;
    Ok(PathSeg::Idx(idx))
}

fn parse_quoted_key(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    quote: char,
    path: &str,
) -> BqResult<String> {
    let mut key = String::new();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let escaped = chars.next().ok_or_else(|| {
                BqError::semantic(format!("Unterminated escape in JSONPath '{path}'"))
            })?;
            key.push(escaped);
        } else if c == quote {
            return Ok(key);
        } else {
            key.push(c);
        }
    }
    Err(BqError::semantic(format!(
        "Unterminated quoted key in JSONPath '{path}'"
    )))
}
