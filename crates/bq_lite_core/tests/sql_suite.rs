use std::path::{Path, PathBuf};

use bq_lite_core::Engine;

#[test]
fn runs_declarative_slt_golden_suites() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let suite_dir = manifest_dir.join("tests/sql_suite");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&suite_dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("slt"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "expected at least one .slt file in {suite_dir:?}"
    );
    for file in files {
        run_slt_file(&file).unwrap();
    }
}

fn run_slt_file(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = content.lines().collect();
    let mut engine = Engine::new();
    let mut idx = 0_usize;

    while let Some(&raw) = lines.get(idx) {
        let line = raw.trim();
        idx += 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "statement ok" {
            let sql = collect_until_blank(&lines, &mut idx);
            let res = engine.exec(&sql);
            assert!(
                res.is_ok(),
                "file {:?} failed `{sql}`: {res:?}",
                path.file_name()
            );
        } else if line == "script ok" {
            let sql = collect_until_blank(&lines, &mut idx);
            let res = engine.exec_script(&sql);
            assert!(
                res.is_ok(),
                "file {:?} script failed `{sql}`: {res:?}",
                path.file_name()
            );
        } else if line == "statement error" {
            let sql = collect_until_blank(&lines, &mut idx);
            assert!(
                engine.exec(&sql).is_err(),
                "file {:?} expected error for `{sql}`",
                path.file_name()
            );
        } else if line.starts_with("query ") {
            run_slt_query(path, &engine, &lines, &mut idx)?;
        }
    }
    Ok(())
}

fn run_slt_query(
    path: &Path,
    engine: &Engine,
    lines: &[&str],
    idx: &mut usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let sql = collect_until_sep(lines, idx);
    let expected = collect_lines_until_blank(lines, idx);
    let res = engine.query(&sql)?;
    let actual: Vec<String> = res
        .rows
        .iter()
        .map(|r| {
            r.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    assert_eq!(
        actual,
        expected,
        "file {:?} mismatch for `{sql}`",
        path.file_name()
    );
    Ok(())
}

fn collect_until_blank(lines: &[&str], idx: &mut usize) -> String {
    let mut out = Vec::new();
    while let Some(&line) = lines.get(*idx) {
        *idx += 1;
        if line.trim().is_empty() {
            break;
        }
        out.push(line);
    }
    out.join("\n")
}

fn collect_until_sep(lines: &[&str], idx: &mut usize) -> String {
    let mut out = Vec::new();
    while let Some(&line) = lines.get(*idx) {
        *idx += 1;
        if line.trim() == "----" {
            break;
        }
        out.push(line);
    }
    out.join("\n")
}

fn collect_lines_until_blank(lines: &[&str], idx: &mut usize) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(&line) = lines.get(*idx) {
        *idx += 1;
        if line.trim().is_empty() {
            break;
        }
        out.push(line.to_owned());
    }
    out
}
