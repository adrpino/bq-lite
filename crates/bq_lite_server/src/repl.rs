use std::io::{BufRead, Write};
use std::path::Path;

use bq_lite_core::catalog::ColumnSchema;
use bq_lite_core::io::csv_json::export_csv;
use bq_lite_core::parser::parse_script;
use bq_lite_core::{BqValue, Engine, QueryResult};
use serde_json::{json, Value as JsonValue};

/// Output format for one-shot `exec` queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
    Csv,
}

/// Runs a one-shot SQL script or query and writes results in `format`.
pub fn run_exec(
    project: &str,
    data_dir: Option<&Path>,
    sql: &str,
    format: OutputFormat,
    out: &mut dyn Write,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut eng = open_engine(project, data_dir)?;
    let qr = exec_sql_smart(&mut eng, sql)?;
    if let Some(dir) = data_dir {
        eng.save_persistent(&dir.join("bq_lite.redb"))?;
    }
    match format {
        OutputFormat::Table => {
            if qr.columns.is_empty() {
                writeln!(out, "OK ({} rows affected)", qr.rows_affected)?;
            } else {
                render_table(&qr, out)?;
            }
        }
        OutputFormat::Json => {
            let rows_json: Vec<JsonValue> = qr
                .rows
                .iter()
                .map(|r| {
                    let mut map = serde_json::Map::new();
                    for (c, v) in qr.columns.iter().zip(r.iter()) {
                        map.insert(c.name.clone(), bq_value_to_cli_json(v));
                    }
                    JsonValue::Object(map)
                })
                .collect();
            writeln!(out, "{}", serde_json::to_string_pretty(&rows_json)?)?;
        }
        OutputFormat::Csv => {
            let cols: Vec<ColumnSchema> = qr
                .columns
                .iter()
                .map(|c| ColumnSchema::new(&c.name, c.ty.clone()))
                .collect();
            let bytes = export_csv(&cols, &qr.rows)?;
            out.write_all(&bytes)?;
        }
    }
    Ok(())
}

fn bq_value_to_cli_json(val: &BqValue) -> JsonValue {
    match val {
        BqValue::Null => JsonValue::Null,
        BqValue::Bool(b) => JsonValue::Bool(*b),
        BqValue::Int64(n) => json!(*n),
        BqValue::Float64(f) => {
            serde_json::Number::from_f64(*f).map_or_else(|| json!(f.to_string()), JsonValue::Number)
        }
        BqValue::Json(j) => (**j).clone(),
        BqValue::Array(items) => JsonValue::Array(items.iter().map(bq_value_to_cli_json).collect()),
        BqValue::Struct(fields) => {
            let mut obj = serde_json::Map::new();
            for (idx, field) in fields.iter().enumerate() {
                let key = field.name.clone().unwrap_or_else(|| format!("f{idx}"));
                obj.insert(key, bq_value_to_cli_json(&field.value));
            }
            JsonValue::Object(obj)
        }
        BqValue::Numeric(_)
        | BqValue::BigNumeric(_)
        | BqValue::String(_)
        | BqValue::Bytes(_)
        | BqValue::Date(_)
        | BqValue::Datetime(_)
        | BqValue::Time(_)
        | BqValue::Timestamp(_)
        | BqValue::Interval(_)
        | BqValue::Geography(_)
        | BqValue::Range(_) => json!(val.to_string()),
    }
}

pub fn run_repl(project: &str, data_dir: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    run_repl_with_io(project, data_dir, &mut stdin.lock(), &mut stdout)
}

pub fn run_repl_with_io(
    project: &str,
    data_dir: Option<&Path>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut eng = open_engine(project, data_dir)?;
    writeln!(
        out,
        "bq-lite REPL (project: {project}). Type .help, .tables, or .quit"
    )?;
    let mut buf = String::new();

    loop {
        let prompt = if buf.is_empty() { "bq> " } else { "..> " };
        write!(out, "{prompt}")?;
        out.flush()?;
        let mut line = String::new();
        let read_bytes = input.read_line(&mut line)?;
        if read_bytes == 0 {
            break;
        }
        let trimmed = line.trim();
        if buf.is_empty() && trimmed.starts_with('.') {
            if handle_dot_command(trimmed, &eng, out)? {
                break;
            }
            continue;
        }
        buf.push_str(&line);
        if trimmed.ends_with(';') {
            let sql = std::mem::take(&mut buf);
            execute_repl_sql(&mut eng, &sql, data_dir, out)?;
        }
    }
    Ok(())
}

fn open_engine(
    project: &str,
    data_dir: Option<&Path>,
) -> Result<Engine, Box<dyn std::error::Error>> {
    match data_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir)?;
            Ok(Engine::open_persistent(&dir.join("bq_lite.redb"), project)?)
        }
        None => Ok(Engine::with_project(project)),
    }
}

fn exec_sql_smart(eng: &mut Engine, sql: &str) -> bq_lite_core::BqResult<QueryResult> {
    match parse_script(sql) {
        Ok(stmts)
            if stmts.len() == 1
                && matches!(stmts.first(), Some(bq_lite_core::ast::Statement::Query(_))) =>
        {
            eng.query(sql)
        }
        Ok(_) => eng.exec_script(sql),
        Err(e) => Err(e),
    }
}

fn handle_dot_command(
    cmd: &str,
    eng: &Engine,
    out: &mut dyn Write,
) -> Result<bool, Box<dyn std::error::Error>> {
    if cmd == ".quit" || cmd == ".exit" {
        return Ok(true);
    }
    if cmd == ".help" {
        writeln!(out, ".tables          List datasets and tables")?;
        writeln!(out, ".schema <ds.tbl> Show table schema")?;
        writeln!(out, ".quit            Exit REPL")?;
        return Ok(false);
    }
    if cmd == ".tables" {
        if let Some(proj) = eng.catalog().get_project(eng.catalog().default_project()) {
            for ds in proj.datasets.values() {
                if ds.name == "__temp" {
                    continue;
                }
                for tbl in ds.tables.keys() {
                    writeln!(out, "{}.{tbl}", ds.name)?;
                }
            }
        }
        return Ok(false);
    }
    if let Some(target) = cmd.strip_prefix(".schema ") {
        let qr = eng.query(&format!(
            "SELECT column_name, data_type, is_nullable FROM INFORMATION_SCHEMA.COLUMNS WHERE CONCAT(table_schema, '.', table_name) = '{}'",
            target.trim()
        ))?;
        render_table(&qr, out)?;
        return Ok(false);
    }
    writeln!(out, "Unknown command: {cmd}")?;
    Ok(false)
}

fn execute_repl_sql(
    eng: &mut Engine,
    sql: &str,
    data_dir: Option<&Path>,
    out: &mut dyn Write,
) -> Result<(), Box<dyn std::error::Error>> {
    match exec_sql_smart(eng, sql) {
        Ok(qr) => {
            if let Some(dir) = data_dir {
                let _ = eng.save_persistent(&dir.join("bq_lite.redb"));
            }
            if qr.columns.is_empty() {
                writeln!(out, "OK ({} rows affected)", qr.rows_affected)?;
            } else {
                render_table(&qr, out)?;
            }
        }
        Err(err) => writeln!(out, "ERROR: {}", err.message)?,
    }
    Ok(())
}

/// Renders a [`QueryResult`] as an ASCII table for CLI output.
pub fn render_table(qr: &QueryResult, out: &mut dyn Write) -> std::io::Result<()> {
    let mut widths: Vec<usize> = qr.columns.iter().map(|c| c.name.len()).collect();
    let str_rows: Vec<Vec<String>> = qr
        .rows
        .iter()
        .map(|r| {
            r.iter()
                .enumerate()
                .map(|(i, v)| {
                    let s = v.to_string();
                    if let Some(w) = widths.get_mut(i) {
                        *w = (*w).max(s.len());
                    }
                    s
                })
                .collect()
        })
        .collect();

    let mut sep = String::new();
    for w in &widths {
        sep.push_str("+-");
        for _ in 0..*w {
            sep.push('-');
        }
        sep.push('-');
    }
    sep.push('+');
    writeln!(out, "{sep}")?;
    for (i, col) in qr.columns.iter().enumerate() {
        let w = widths.get(i).copied().unwrap_or(0);
        write!(out, "| {:w$} ", col.name)?;
    }
    writeln!(out, "|")?;
    writeln!(out, "{sep}")?;
    for row in &str_rows {
        for (i, cell) in row.iter().enumerate() {
            let w = widths.get(i).copied().unwrap_or(0);
            write!(out, "| {cell:w$} ")?;
        }
        writeln!(out, "|")?;
    }
    writeln!(out, "{sep}")
}
