#[derive(Debug, Clone)]
pub(super) struct TestSection {
    pub name: String,
    pub is_prepare_db: bool,
    pub required_features: Vec<String>,
    pub default_time_zone: Option<String>,
    pub primary_key_mode: Option<String>,
    pub has_parameters: bool,
    pub sql: String,
    pub expected: Expected,
}

#[derive(Debug, Clone)]
pub(super) enum Expected {
    Error,
    Result(String),
    Dml {
        num_rows_modified: Option<i64>,
        all_rows: Option<String>,
    },
    Empty,
}

#[derive(Debug, Default)]
pub(super) struct ParsedTestFile {
    pub sections: Vec<TestSection>,
}

pub(super) fn parse_test_file(content: &str) -> ParsedTestFile {
    let mut file = ParsedTestFile::default();
    let mut current_lines: Vec<&str> = Vec::new();
    let mut default_feats: Vec<String> = Vec::new();
    let mut default_tz: Option<String> = None;

    for raw_line in content.lines() {
        if raw_line.trim() == "==" {
            process_section(
                &current_lines,
                &mut default_feats,
                &mut default_tz,
                &mut file.sections,
            );
            current_lines.clear();
        } else {
            current_lines.push(raw_line);
        }
    }
    if !current_lines.is_empty() {
        process_section(
            &current_lines,
            &mut default_feats,
            &mut default_tz,
            &mut file.sections,
        );
    }
    file
}

fn process_section(
    lines: &[&str],
    default_feats: &mut Vec<String>,
    default_tz: &mut Option<String>,
    out: &mut Vec<TestSection>,
) {
    let mut meta = SectionMeta::new(default_feats, default_tz.as_deref());
    let mut sql_lines: Vec<&str> = Vec::new();
    let mut exp_lines: Vec<&str> = Vec::new();
    let mut in_expected = false;
    let mut seen_sql = false;
    let mut in_multiline_directive = false;

    for &line in lines {
        let trimmed = line.trim();
        if !in_expected && !seen_sql {
            if in_multiline_directive {
                if trimmed.ends_with(']') {
                    in_multiline_directive = false;
                }
                continue;
            }
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if trimmed.starts_with('[') && !trimmed.ends_with(']') {
                if trimmed.starts_with("[parameters=") {
                    meta.has_parameters = true;
                }
                in_multiline_directive = true;
                continue;
            }
            if handle_directive(trimmed, &mut meta, default_feats, default_tz) {
                continue;
            }
        }
        if !in_expected && trimmed == "--" {
            in_expected = true;
            continue;
        }
        if in_expected {
            exp_lines.push(line);
        } else {
            seen_sql = true;
            // GoogleSQL .test files escape leading `--` SQL comments as `\--`.
            let unescaped = line.strip_prefix("\\--").map_or(line, |_| &line[1..]);
            sql_lines.push(unescaped);
        }
    }

    let sql = sql_lines.join("\n").trim().to_owned();
    if sql.is_empty() {
        return;
    }
    let expected = classify_expected(&exp_lines);
    let idx = out.len() + 1;
    let name = meta.name.unwrap_or_else(|| format!("case_{idx}"));
    out.push(TestSection {
        name,
        is_prepare_db: meta.is_prepare_db,
        required_features: meta.required_features,
        default_time_zone: meta.default_time_zone,
        primary_key_mode: meta.primary_key_mode,
        has_parameters: meta.has_parameters,
        sql,
        expected,
    });
}

struct SectionMeta {
    name: Option<String>,
    is_prepare_db: bool,
    required_features: Vec<String>,
    default_time_zone: Option<String>,
    primary_key_mode: Option<String>,
    has_parameters: bool,
}

impl SectionMeta {
    fn new(default_feats: &[String], default_tz: Option<&str>) -> Self {
        Self {
            name: None,
            is_prepare_db: false,
            required_features: default_feats.to_vec(),
            default_time_zone: default_tz.map(str::to_owned),
            primary_key_mode: None,
            has_parameters: false,
        }
    }
}

fn handle_directive(
    trimmed: &str,
    meta: &mut SectionMeta,
    default_feats: &mut Vec<String>,
    default_tz: &mut Option<String>,
) -> bool {
    if !(trimmed.starts_with('[') && trimmed.ends_with(']')) {
        return false;
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    if inner == "prepare_database" {
        meta.is_prepare_db = true;
        return true;
    }
    if let Some(rest) = inner.strip_prefix("name=") {
        meta.name = Some(rest.trim().to_owned());
        return true;
    }
    if let Some(rest) = inner.strip_prefix("default required_features=") {
        *default_feats = parse_csv(rest);
        meta.required_features.clone_from(default_feats);
        return true;
    }
    if let Some(rest) = inner.strip_prefix("required_features=") {
        meta.required_features.extend(parse_csv(rest));
        return true;
    }
    if let Some(rest) = inner.strip_prefix("default default_time_zone=") {
        let tz = rest.trim().to_owned();
        *default_tz = Some(tz.clone());
        meta.default_time_zone = Some(tz);
        return true;
    }
    if let Some(rest) = inner.strip_prefix("default_time_zone=") {
        meta.default_time_zone = Some(rest.trim().to_owned());
        return true;
    }
    if let Some(rest) = inner.strip_prefix("primary_key_mode=") {
        meta.primary_key_mode = Some(rest.trim().to_owned());
        return true;
    }
    if inner.starts_with("parameters=") || inner.starts_with("default parameters=") {
        meta.has_parameters = true;
        return true;
    }
    inner.starts_with("load_proto_")
        || inner.starts_with("load_enum_")
        || inner.contains('=')
        || inner.starts_with("default ")
        || inner == "use_test_database_copy"
        || inner == "skip_required_feature_integrity_check"
        || inner == "exclude_in_sql_builder_pipe_sql_equivalence_tests"
        || inner == "reserve_match_recognize"
}

fn parse_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_owned)
        .collect()
}

fn classify_expected(exp_lines: &[&str]) -> Expected {
    let cleaned: Vec<&str> = exp_lines
        .iter()
        .copied()
        .take_while(|l| !l.trim_start().starts_with("NOTE:"))
        .collect();
    let raw = cleaned.join("\n").trim().to_owned();
    if raw.is_empty() {
        return Expected::Empty;
    }
    if raw.starts_with("ERROR:") {
        return Expected::Error;
    }
    if raw.contains("num_rows_modified") && raw.starts_with("STRUCT<") {
        let modified = extract_num_rows_modified(&raw);
        let all_rows = extract_dml_all_rows(&raw);
        return Expected::Dml {
            num_rows_modified: modified,
            all_rows,
        };
    }
    Expected::Result(raw)
}

fn extract_num_rows_modified(raw: &str) -> Option<i64> {
    let brace = raw.find('{')?;
    let after = raw[brace + 1..].trim_start();
    let num_str: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    num_str.parse().ok()
}

fn extract_dml_all_rows(raw: &str) -> Option<String> {
    let brace = raw.find('{')?;
    let body = raw[brace + 1..].trim_end().strip_suffix('}')?.trim();
    let array_pos = body.find("ARRAY<")?;
    Some(body[array_pos..].trim().to_owned())
}
