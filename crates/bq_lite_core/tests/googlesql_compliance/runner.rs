use std::collections::BTreeMap;

use bq_lite_core::catalog::{ColumnSchema, DatasetMeta, TableKind, TableMeta, TableRef};
use bq_lite_core::{BqErrorKind, BqType, BqValue, Engine};
use chrono::Utc;

use super::compare::{expected_val_to_bq, matches_query_result};
use super::parser::{parse_test_file, Expected, TestSection};
use super::value_parse::parse_expected_table;

#[derive(Debug, Default)]
pub(super) struct FileReport {
    pub file_name: String,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub failure_buckets: BTreeMap<String, usize>,
    pub sample_failures: Vec<String>,
}

const OUT_OF_SCOPE_FEATURES: &[&str] = &[
    "SQL_GRAPH",
    "SQL_GRAPH_ADVANCED_QUERY",
    "SQL_GRAPH_BOUNDED_PATH_QUANTIFICATION",
    "SQL_GRAPH_PATH_TYPE",
    "SQL_GRAPH_PATH_MODE",
    "SQL_GRAPH_EXPOSE_GRAPH_ELEMENT",
    "SQL_GRAPH_CHEAPEST_PATH",
    "COLLATION_SUPPORT",
    "ANNOTATION_FRAMEWORK",
    "DIFFERENTIAL_PRIVACY",
    "DIFFERENTIAL_PRIVACY_REPORT_FUNCTIONS",
    "ANONYMIZATION",
    "MAP_TYPE",
    "UUID_TYPE",
    "VECTOR_TYPE",
    "ENCRYPTION",
    "MATCH_RECOGNIZE",
    "MEASURES",
    "TIMESTAMP_NANOS",
    "TIMESTAMP_PICOS",
    "DISALLOW_NULL_PRIMARY_KEYS",
    "REPLACE_FIELDS",
    "PROTO",
    "ENUM",
    "VALUE_TABLES",
    "NAMED_ARGUMENTS",
    "TEMPLATE_FUNCTIONS",
];

pub(super) fn run_test_content(file_name: &str, content: &str) -> FileReport {
    let parsed = parse_test_file(content);
    let mut report = FileReport {
        file_name: file_name.to_owned(),
        ..FileReport::default()
    };
    let mut engine = new_compliance_engine();

    for sec in &parsed.sections {
        if sec.is_prepare_db {
            prepare_db_section(&mut engine, sec);
            continue;
        }
        if should_skip_section(sec) {
            report.skipped += 1;
            continue;
        }
        run_single_case(&engine, sec, &mut report);
    }
    report
}

fn new_compliance_engine() -> Engine {
    let mut eng = Engine::new();
    let _ = eng
        .catalog_mut()
        .create_dataset("local", DatasetMeta::new("compliance", "US"), true);
    eng.catalog_mut()
        .set_default_dataset(Some("compliance".to_owned()));
    eng
}

fn should_skip_section(sec: &TestSection) -> bool {
    if sec.has_parameters || sec.default_time_zone.is_some() {
        return true;
    }
    if sec.required_features.iter().any(|f| {
        OUT_OF_SCOPE_FEATURES
            .iter()
            .any(|oof| f.eq_ignore_ascii_case(oof) || f.starts_with("TEST_ONLY"))
    }) {
        return true;
    }
    let upper = sec.sql.to_ascii_uppercase();
    if upper.contains("PROTO")
        || upper.contains("TESTENUM")
        || upper.contains("KITCHENSINK")
        || upper.contains("GOOGLESQL_TEST.")
        || upper.contains("ENUM_VAL")
        || upper.contains("PROTO_VAL")
        || has_non_bq_zetasql_type(&upper)
    {
        return true;
    }
    if let Expected::Result(ref raw) = sec.expected {
        if raw.contains("PROTO<")
            || raw.contains("ENUM<")
            || raw.contains("googlesql_test.")
            || has_non_bq_zetasql_type(&raw.to_ascii_uppercase())
        {
            return true;
        }
    }
    false
}

fn has_non_bq_zetasql_type(upper: &str) -> bool {
    const ZETASQL_ONLY_TYPES: &[&str] = &["INT32", "UINT32", "UINT64", "FLOAT", "DOUBLE"];
    ZETASQL_ONLY_TYPES.iter().any(|ty| contains_word(upper, ty))
}

fn contains_word(haystack: &str, word: &str) -> bool {
    let bytes = haystack.as_bytes();
    let wlen = word.len();
    let mut start = 0;
    while let Some(rel) = haystack[start..].find(word) {
        let idx = start + rel;
        let before_ok = idx == 0
            || !bytes
                .get(idx - 1)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
        let after_idx = idx + wlen;
        let after_ok = after_idx >= bytes.len()
            || !bytes
                .get(after_idx)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
        if before_ok && after_ok {
            return true;
        }
        start = idx + wlen;
    }
    false
}

fn prepare_db_section(engine: &mut Engine, sec: &TestSection) {
    if engine.exec(&sec.sql).is_ok() {
        return;
    }
    if let Expected::Result(ref raw) = sec.expected {
        if let (Some(tbl_name), Ok(expected_tbl)) = (
            extract_create_table_name(&sec.sql),
            parse_expected_table(raw),
        ) {
            let cols: Vec<ColumnSchema> = expected_tbl
                .columns
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let name = c.name.clone().unwrap_or_else(|| format!("col_{i}"));
                    ColumnSchema::new(name, c.bq_type.clone())
                })
                .collect();
            let now = Utc::now();
            let meta = TableMeta {
                name: tbl_name.clone(),
                kind: TableKind::Table,
                columns: cols.clone(),
                partition_by: None,
                cluster_by: Vec::new(),
                view_query: None,
                created_at: now,
                updated_at: now,
                options: BTreeMap::new(),
                description: None,
                friendly_name: None,
                labels: BTreeMap::new(),
                view_sql: None,
                expiration_ms: None,
                external_uris: Vec::new(),
                external_format: None,
            };
            let tref = TableRef::new("local", "compliance", &tbl_name);
            let _ = engine.catalog_mut().put_table(&tref, meta, true, false);
            let rows: Vec<Vec<BqValue>> = expected_tbl
                .rows
                .iter()
                .map(|erow| {
                    erow.iter()
                        .enumerate()
                        .map(|(i, ev)| {
                            let ty = cols.get(i).map_or(&BqType::String, |c| &c.ty);
                            expected_val_to_bq(ev, ty)
                        })
                        .collect()
                })
                .collect();
            let _ = engine.insert_rows(&tref, rows);
        }
    }
}

fn extract_create_table_name(sql: &str) -> Option<String> {
    let mut tokens = sql.split_whitespace();
    while let Some(tok) = tokens.next() {
        if tok.eq_ignore_ascii_case("TABLE") {
            let next = tokens.next()?;
            let clean = next
                .split('(')
                .next()
                .unwrap_or(next)
                .trim_matches('`')
                .to_owned();
            if !clean.is_empty() {
                return Some(clean);
            }
        }
    }
    None
}

fn run_single_case(base_engine: &Engine, sec: &TestSection, report: &mut FileReport) {
    match &sec.expected {
        Expected::Empty => {
            report.skipped += 1;
        }
        Expected::Error => {
            let mut fork = base_engine.fork_isolated();
            match fork.exec_script(&sec.sql) {
                Err(err) if err.kind != BqErrorKind::Unimplemented => {
                    report.passed += 1;
                }
                Err(err) => {
                    let bucket = classify_bq_error(&err);
                    record_failure(report, &sec.name, &bucket, &err.message);
                }
                Ok(_) => {
                    record_failure(
                        report,
                        &sec.name,
                        "expected_error_got_ok",
                        "Expected error, got Ok",
                    );
                }
            }
        }
        Expected::Result(raw) => {
            let expected_tbl = match parse_expected_table(raw) {
                Ok(t) => t,
                Err(e) => {
                    report.skipped += 1;
                    let _ = e;
                    return;
                }
            };
            let res = base_engine.query(&sec.sql);
            match res {
                Ok(qr) => match matches_query_result(&expected_tbl, &qr) {
                    Ok(()) => report.passed += 1,
                    Err(msg) => {
                        let bucket = if msg.starts_with("row count") {
                            "mismatch:row_count"
                        } else {
                            "mismatch:value"
                        };
                        record_failure(report, &sec.name, bucket, &msg);
                    }
                },
                Err(err) => {
                    let bucket = classify_bq_error(&err);
                    record_failure(report, &sec.name, &bucket, &err.message);
                }
            }
        }
        Expected::Dml {
            num_rows_modified,
            all_rows,
        } => run_dml_case(
            base_engine,
            sec,
            *num_rows_modified,
            all_rows.as_deref(),
            report,
        ),
    }
}

fn run_dml_case(
    base_engine: &Engine,
    sec: &TestSection,
    num_rows_modified: Option<i64>,
    all_rows: Option<&str>,
    report: &mut FileReport,
) {
    let mut fork = base_engine.fork_isolated();
    match fork.exec(&sec.sql) {
        Ok(qr) => {
            let count_ok = num_rows_modified
                .is_none_or(|exp| i64::try_from(qr.rows_affected).unwrap_or(-1) == exp);
            if !count_ok {
                record_failure(
                    report,
                    &sec.name,
                    "dml:affected_rows",
                    &format!("expected {num_rows_modified:?}, got {}", qr.rows_affected),
                );
                return;
            }
            if let (Some(raw_rows), Some(target_tbl)) =
                (all_rows, extract_dml_target_table(&sec.sql))
            {
                if let Ok(expected_tbl) = parse_expected_table(raw_rows) {
                    match fork.query(&format!("SELECT * FROM `{target_tbl}`")) {
                        Ok(actual_tbl) => {
                            if let Err(msg) = matches_query_result(&expected_tbl, &actual_tbl) {
                                record_failure(report, &sec.name, "dml:all_rows", &msg);
                                return;
                            }
                        }
                        Err(err) => {
                            let bucket = classify_bq_error(&err);
                            record_failure(report, &sec.name, &bucket, &err.message);
                            return;
                        }
                    }
                }
            }
            report.passed += 1;
        }
        Err(err) => {
            let bucket = classify_bq_error(&err);
            record_failure(report, &sec.name, &bucket, &err.message);
        }
    }
}

fn extract_dml_target_table(sql: &str) -> Option<String> {
    let mut tokens = sql.split_whitespace();
    let first = tokens.next()?;
    let raw = if first.eq_ignore_ascii_case("INSERT") {
        let second = tokens.next()?;
        if second.eq_ignore_ascii_case("INTO") {
            tokens.next()?
        } else {
            second
        }
    } else if first.eq_ignore_ascii_case("UPDATE") {
        tokens.next()?
    } else if first.eq_ignore_ascii_case("DELETE") {
        let second = tokens.next()?;
        if second.eq_ignore_ascii_case("FROM") {
            tokens.next()?
        } else {
            second
        }
    } else {
        return None;
    };
    let clean = raw
        .split('(')
        .next()
        .unwrap_or(raw)
        .trim_matches('`')
        .to_owned();
    if clean.is_empty() {
        None
    } else {
        Some(clean)
    }
}

fn classify_bq_error(err: &bq_lite_core::BqError) -> String {
    let msg = err.message.as_str();
    let short: String = msg.chars().take(55).collect();
    format!("{:?}:{short}", err.kind)
}

fn record_failure(report: &mut FileReport, case_name: &str, bucket: &str, detail: &str) {
    report.failed += 1;
    *report.failure_buckets.entry(bucket.to_owned()).or_insert(0) += 1;
    let cap = if std::env::var_os("BQ_LITE_COMPLIANCE_FILTER").is_some() {
        60
    } else {
        5
    };
    if report.sample_failures.len() < cap {
        report
            .sample_failures
            .push(format!("{case_name}: {detail}"));
    }
}
