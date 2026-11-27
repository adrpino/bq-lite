use bq_lite_core::{BqType, StructField};

#[derive(Debug, Clone)]
pub(super) struct ExpectedColumn {
    pub name: Option<String>,
    pub bq_type: BqType,
}

#[derive(Debug, Clone)]
pub(super) struct ExpectedTable {
    pub columns: Vec<ExpectedColumn>,
    pub ordered: bool,
    pub rows: Vec<Vec<ExpectedVal>>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum ExpectedVal {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    Raw(String),
    Array(Vec<Self>),
    Struct(Vec<Self>),
}

pub(super) fn parse_expected_table(raw: &str) -> Result<ExpectedTable, String> {
    let trimmed = raw.trim();
    let header_prefix = trimmed.strip_prefix("ARRAY<").ok_or_else(|| {
        format!(
            "Expected ARRAY<...>, got: {}",
            &trimmed[..trimmed.len().min(40)]
        )
    })?;
    let (elem_ty_str, rest) = split_angle_bracket(header_prefix)?;
    let columns = parse_row_struct_columns(elem_ty_str)?;
    let rest = rest.trim_start();
    let body_inner = rest
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .ok_or_else(|| "Expected [...] array body".to_owned())?
        .trim();

    let (ordered, items_src) = body_inner.strip_prefix("known order:").map_or_else(
        || {
            body_inner
                .strip_prefix("unknown order:")
                .map_or((false, body_inner), |after| (false, after.trim()))
        },
        |after| (true, after.trim()),
    );

    if items_src.is_empty() {
        return Ok(ExpectedTable {
            columns,
            ordered,
            rows: Vec::new(),
        });
    }
    let mut cursor = ValCursor::new(items_src);
    let mut rows = Vec::new();
    while !cursor.eof() {
        cursor.skip_ws();
        if cursor.eof() {
            break;
        }
        let row_val = cursor.parse_val(false)?;
        if let ExpectedVal::Struct(fields) = row_val {
            rows.push(fields);
        } else {
            rows.push(vec![row_val]);
        }
        cursor.skip_ws();
        let _ = cursor.eat(',');
    }
    Ok(ExpectedTable {
        columns,
        ordered,
        rows,
    })
}

fn split_angle_bracket(s: &str) -> Result<(&str, &str), String> {
    let mut depth = 1_usize;
    for (idx, ch) in s.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return Ok((&s[..idx], &s[idx + 1..]));
                }
            }
            _ => {}
        }
    }
    Err("Unbalanced '<' in type header".to_owned())
}

fn parse_row_struct_columns(elem_ty: &str) -> Result<Vec<ExpectedColumn>, String> {
    let t = elem_ty.trim();
    if let Some(after) = t.strip_prefix("STRUCT<") {
        let (inner, _) = split_angle_bracket(after)?;
        if inner.trim().is_empty() {
            return Ok(Vec::new());
        }
        let parts = split_top_level_commas(inner);
        let mut cols = Vec::with_capacity(parts.len());
        for part in parts {
            cols.push(parse_field_decl(&part));
        }
        return Ok(cols);
    }
    Ok(vec![ExpectedColumn {
        name: None,
        bq_type: map_type_str(t),
    }])
}

fn parse_field_decl(part: &str) -> ExpectedColumn {
    let trimmed = part.trim();
    if let Some(sp) = find_top_level_space(trimmed) {
        let name = trimmed[..sp].trim().trim_matches('`').to_owned();
        let ty_str = trimmed[sp + 1..].trim();
        return ExpectedColumn {
            name: Some(name),
            bq_type: map_type_str(ty_str),
        };
    }
    ExpectedColumn {
        name: None,
        bq_type: map_type_str(trimmed),
    }
}

fn map_type_str(raw: &str) -> BqType {
    let t = raw.trim();
    let upper = t.to_ascii_uppercase();
    if let Some(inner) = t.strip_prefix("ARRAY<").and_then(|s| s.strip_suffix('>')) {
        return BqType::array(map_type_str(inner)).unwrap_or(BqType::String);
    }
    if let Some(inner) = t.strip_prefix("STRUCT<").and_then(|s| s.strip_suffix('>')) {
        let fields = split_top_level_commas(inner)
            .into_iter()
            .filter(|p| !p.trim().is_empty())
            .map(|p| {
                let col = parse_field_decl(&p);
                col.name.map_or_else(
                    || StructField::anonymous(col.bq_type.clone()),
                    |n| StructField::named(n, col.bq_type.clone()),
                )
            })
            .collect();
        return BqType::Struct(fields);
    }
    if upper.starts_with("RANGE<") {
        if upper.contains("DATETIME") {
            return BqType::Range(bq_lite_core::RangeElementType::Datetime);
        }
        if upper.contains("TIMESTAMP") {
            return BqType::Range(bq_lite_core::RangeElementType::Timestamp);
        }
        return BqType::Range(bq_lite_core::RangeElementType::Date);
    }
    match upper.as_str() {
        "BOOL" | "BOOLEAN" => BqType::Bool,
        "INT64" | "INT32" | "UINT32" | "UINT64" => BqType::Int64,
        "DOUBLE" | "FLOAT64" | "FLOAT" => BqType::Float64,
        "NUMERIC" => BqType::Numeric,
        "BIGNUMERIC" => BqType::BigNumeric,
        "BYTES" => BqType::Bytes,
        "DATE" => BqType::Date,
        "DATETIME" => BqType::Datetime,
        "TIME" => BqType::Time,
        "TIMESTAMP" => BqType::Timestamp,
        "INTERVAL" => BqType::Interval,
        "JSON" => BqType::Json,
        "GEOGRAPHY" => BqType::Geography,
        _ => BqType::String,
    }
}

fn find_top_level_space(s: &str) -> Option<usize> {
    let mut depth = 0_usize;
    let mut in_bt = false;
    for (idx, ch) in s.char_indices() {
        match ch {
            '`' => in_bt = !in_bt,
            '<' | '(' if !in_bt => depth += 1,
            '>' | ')' if !in_bt => depth = depth.saturating_sub(1),
            c if c.is_whitespace() && depth == 0 && !in_bt => return Some(idx),
            _ => {}
        }
    }
    None
}

fn split_top_level_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0_usize;
    let mut start = 0_usize;
    for (idx, ch) in s.char_indices() {
        match ch {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(s[start..idx].to_owned());
                start = idx + 1;
            }
            _ => {}
        }
    }
    if start < s.len() {
        out.push(s[start..].to_owned());
    }
    out
}

struct ValCursor<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> ValCursor<'a> {
    const fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    const fn eof(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn skip_ws(&mut self) {
        while let Some(ch) = self.rest().chars().next() {
            if ch.is_whitespace() {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
    }

    fn eat(&mut self, expected: char) -> bool {
        if self.rest().starts_with(expected) {
            self.pos += expected.len_utf8();
            true
        } else {
            false
        }
    }

    fn parse_val(&mut self, in_json_or_raw: bool) -> Result<ExpectedVal, String> {
        self.skip_ws();
        let rest = self.rest();
        if rest.starts_with('{') {
            if is_json_object_ahead(rest) || in_json_or_raw {
                let raw = self.capture_balanced('{', '}')?;
                return Ok(ExpectedVal::Raw(raw));
            }
            return self.parse_struct();
        }
        if rest.starts_with('[') {
            if is_range_literal_ahead(rest) {
                let raw = self.capture_until_delim(')')?;
                return Ok(ExpectedVal::Raw(raw));
            }
            return self.parse_array();
        }
        if rest.starts_with("b\"") {
            self.pos += 1;
            let bytes = self.parse_quoted_bytes()?;
            return Ok(ExpectedVal::Bytes(bytes));
        }
        if rest.starts_with('"') {
            let s = self.parse_quoted_string()?;
            return Ok(ExpectedVal::String(s));
        }
        self.parse_bare_token()
    }

    fn parse_struct(&mut self) -> Result<ExpectedVal, String> {
        self.pos += 1;
        let mut fields = Vec::new();
        loop {
            self.skip_ws();
            if self.eat('}') {
                break;
            }
            fields.push(self.parse_val(false)?);
            self.skip_ws();
            if !self.eat(',') {
                if !self.eat('}') {
                    return Err(format!(
                        "Expected ',' or '}}' near: {}",
                        &self.rest()[..self.rest().len().min(30)]
                    ));
                }
                break;
            }
        }
        Ok(ExpectedVal::Struct(fields))
    }

    fn parse_array(&mut self) -> Result<ExpectedVal, String> {
        self.pos += 1;
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            if self.eat(']') {
                break;
            }
            items.push(self.parse_val(false)?);
            self.skip_ws();
            if !self.eat(',') {
                if !self.eat(']') {
                    return Err(format!(
                        "Expected ',' or ']' near: {}",
                        &self.rest()[..self.rest().len().min(30)]
                    ));
                }
                break;
            }
        }
        Ok(ExpectedVal::Array(items))
    }

    fn capture_balanced(&mut self, open: char, close: char) -> Result<String, String> {
        let start = self.pos;
        let mut depth = 0_usize;
        let mut in_str = false;
        let mut esc = false;
        for (off, ch) in self.rest().char_indices() {
            if in_str {
                if esc {
                    esc = false;
                } else if ch == '\\' {
                    esc = true;
                } else if ch == '"' {
                    in_str = false;
                }
                continue;
            }
            if ch == '"' {
                in_str = true;
            } else if ch == open {
                depth += 1;
            } else if ch == close {
                depth -= 1;
                if depth == 0 {
                    let end = start + off + ch.len_utf8();
                    self.pos = end;
                    return Ok(self.src[start..end].to_owned());
                }
            }
        }
        Err("Unbalanced balanced literal".to_owned())
    }

    fn capture_until_delim(&mut self, close: char) -> Result<String, String> {
        let start = self.pos;
        if let Some(off) = self.rest().find(close) {
            let end = start + off + close.len_utf8();
            self.pos = end;
            return Ok(self.src[start..end].to_owned());
        }
        Err("Unterminated range literal".to_owned())
    }

    fn parse_quoted_bytes(&mut self) -> Result<Vec<u8>, String> {
        self.pos += 1;
        let mut out = Vec::new();
        let bytes = self.src.as_bytes();
        while let Some(&b) = bytes.get(self.pos) {
            self.pos += 1;
            if b == b'"' {
                return Ok(out);
            }
            if b == b'\\' && self.pos < bytes.len() {
                decode_escape_byte(bytes, &mut self.pos, &mut out);
            } else {
                out.push(b);
            }
        }
        Err("Unterminated quoted string".to_owned())
    }

    fn parse_quoted_string(&mut self) -> Result<String, String> {
        let raw = self.parse_quoted_bytes()?;
        Ok(String::from_utf8_lossy(&raw).into_owned())
    }

    fn parse_bare_token(&mut self) -> Result<ExpectedVal, String> {
        let start = self.pos;
        while let Some(ch) = self.rest().chars().next() {
            if matches!(ch, ',' | '}' | ']') {
                break;
            }
            self.pos += ch.len_utf8();
        }
        let tok = self.src[start..self.pos].trim();
        if tok.is_empty() {
            return Err("Empty token in expected value".to_owned());
        }
        Ok(classify_bare_token(tok))
    }
}

fn decode_escape_byte(bytes: &[u8], pos: &mut usize, out: &mut Vec<u8>) {
    let Some(&esc) = bytes.get(*pos) else {
        return;
    };
    *pos += 1;
    match esc {
        b'n' => out.push(b'\n'),
        b'r' => out.push(b'\r'),
        b't' => out.push(b'\t'),
        b'a' => out.push(0x07),
        b'b' => out.push(0x08),
        b'f' => out.push(0x0C),
        b'v' => out.push(0x0B),
        b'\\' => out.push(b'\\'),
        b'"' => out.push(b'"'),
        b'\'' => out.push(b'\''),
        b'?' => out.push(b'?'),
        b'x' => {
            if let Some(hex_bytes) = bytes.get(*pos..*pos + 2) {
                if let Ok(hex_s) = std::str::from_utf8(hex_bytes) {
                    if let Ok(v) = u8::from_str_radix(hex_s, 16) {
                        out.push(v);
                        *pos += 2;
                        return;
                    }
                }
            }
            out.push(b'x');
        }
        other => out.push(other),
    }
}

fn classify_bare_token(tok: &str) -> ExpectedVal {
    match tok {
        "NULL" => ExpectedVal::Null,
        "true" => ExpectedVal::Bool(true),
        "false" => ExpectedVal::Bool(false),
        "inf" | "+inf" => ExpectedVal::Float(f64::INFINITY),
        "-inf" => ExpectedVal::Float(f64::NEG_INFINITY),
        "nan" | "-nan" => ExpectedVal::Float(f64::NAN),
        _ => {
            if let Ok(i) = tok.parse::<i64>() {
                return ExpectedVal::Int(i);
            }
            if let Ok(f) = tok.parse::<f64>() {
                return ExpectedVal::Float(f);
            }
            ExpectedVal::Raw(tok.to_owned())
        }
    }
}

fn is_json_object_ahead(rest: &str) -> bool {
    let after = rest[1..].trim_start();
    if after.starts_with('}') {
        return false;
    }
    if !after.starts_with('"') {
        return false;
    }
    let mut esc = false;
    for (idx, ch) in after[1..].char_indices() {
        if esc {
            esc = false;
            continue;
        }
        if ch == '\\' {
            esc = true;
        } else if ch == '"' {
            let tail = after[idx + 2..].trim_start();
            return tail.starts_with(':');
        }
    }
    false
}

fn is_range_literal_ahead(rest: &str) -> bool {
    let mut depth = 0_usize;
    for ch in rest.chars() {
        match ch {
            '[' => depth += 1,
            ']' => return false,
            ')' if depth == 1 => return true,
            _ => {}
        }
    }
    false
}
