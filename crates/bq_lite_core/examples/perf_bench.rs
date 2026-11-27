#![expect(clippy::print_stdout, reason = "Benchmark CLI prints measurements")]
#![expect(clippy::unwrap_used, reason = "Benchmark setup uses unwrap")]

use std::time::Instant;

use bq_lite_core::catalog::{ColumnSchema, DatasetMeta, TableKind, TableMeta, TableRef};
use bq_lite_core::{BqType, BqValue, Engine};
use chrono::Utc;

fn build_wide_table(eng: &mut Engine, name: &str, num_rows: i64, num_cols: usize) {
    let _ = eng
        .catalog_mut()
        .create_dataset("local", DatasetMeta::new("bench", "US"), true);
    let mut cols = vec![
        ColumnSchema::new("id", BqType::Int64),
        ColumnSchema::new("grp", BqType::Int64),
        ColumnSchema::new("val", BqType::Int64),
    ];
    for c in 3..num_cols {
        cols.push(ColumnSchema::new(format!("col_{c:02}"), BqType::Int64));
    }
    let now = Utc::now();
    let meta = TableMeta {
        name: name.to_owned(),
        kind: TableKind::Table,
        columns: cols,
        partition_by: None,
        cluster_by: Vec::new(),
        view_query: None,
        created_at: now,
        updated_at: now,
        options: std::collections::BTreeMap::new(),
        description: None,
        friendly_name: None,
        labels: std::collections::BTreeMap::new(),
        view_sql: None,
        expiration_ms: None,
        external_uris: Vec::new(),
        external_format: None,
    };
    let tref = TableRef::new("local", "bench", name);
    eng.create_table(&tref, meta).unwrap();
    let mut rows = Vec::with_capacity(usize::try_from(num_rows).unwrap_or(0));
    for i in 0..num_rows {
        let mut r = Vec::with_capacity(num_cols);
        r.push(BqValue::Int64(i));
        r.push(BqValue::Int64(i % 2500));
        r.push(BqValue::Int64((i * 17) % 1000));
        for c in 3..num_cols {
            let ci = i64::try_from(c).unwrap_or(0);
            r.push(BqValue::Int64(i + ci));
        }
        rows.push(r);
    }
    eng.insert_rows(&tref, rows).unwrap();
}

fn time_query(eng: &Engine, label: &str, sql: &str) {
    let t0 = Instant::now();
    let qr = eng.query(sql).unwrap();
    println!(
        "{label}: rows={} elapsed_ms={:.2}",
        qr.rows.len(),
        t0.elapsed().as_secs_f64() * 1000.0
    );
}

fn run_workload(name: &str) {
    let mut eng = Engine::new();
    match name {
        "scan" => {
            build_wide_table(&mut eng, "wide", 20_000, 15);
            time_query(
                &eng,
                "scan",
                "SELECT id, grp, val, col_03, col_04, col_05, col_06, col_07, col_08, col_09
                 FROM bench.wide
                 WHERE val >= 100",
            );
        }
        "window" => {
            build_wide_table(&mut eng, "win_t", 4_000, 6);
            time_query(
                &eng,
                "window",
                "SELECT id,
                        SUM(val) OVER (PARTITION BY MOD(id, 4) ORDER BY id) AS running_sum,
                        COUNT(*) OVER (ORDER BY id RANGE BETWEEN 50 PRECEDING AND 50 FOLLOWING) AS rng_cnt
                 FROM bench.win_t",
            );
        }
        "groupby" => {
            build_wide_table(&mut eng, "gb_t", 15_000, 6);
            time_query(
                &eng,
                "groupby",
                "SELECT DISTINCT grp, SUM(val) AS s, COUNT(*) AS c
                 FROM bench.gb_t
                 GROUP BY grp",
            );
        }
        "cte_corr" => {
            build_wide_table(&mut eng, "big_cte", 10_000, 8);
            build_wide_table(&mut eng, "outer_t", 300, 4);
            time_query(
                &eng,
                "cte_corr",
                "WITH big AS (SELECT * FROM bench.big_cte)
                 SELECT o.id,
                        (SELECT COUNT(*) FROM UNNEST([o.id, o.val]) AS x WHERE x > 10) AS cnt
                 FROM bench.outer_t o
                 WHERE EXISTS (SELECT 1 FROM (SELECT o.id AS k) WHERE k >= 0)",
            );
        }
        "join_ineq" => {
            build_wide_table(&mut eng, "jl", 500, 6);
            build_wide_table(&mut eng, "jr", 500, 6);
            time_query(
                &eng,
                "join_ineq",
                "SELECT l.id, r.id
                 FROM bench.jl l
                 JOIN bench.jr r ON l.id = r.id AND l.val < r.val + 10",
            );
        }
        _ => {
            for w in ["scan", "window", "groupby", "cte_corr", "join_ineq"] {
                run_workload(w);
            }
        }
    }
}

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_else(|| "all".to_owned());
    run_workload(&arg);
}
