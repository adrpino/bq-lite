use std::time::Instant;

use bq_lite_core::catalog::{ColumnSchema, DatasetMeta, TableKind, TableMeta, TableRef};
use bq_lite_core::{BqType, BqValue, Engine};
use chrono::Utc;

fn seed_bench_table(eng: &mut Engine, name: &str, num_rows: i64, num_groups: i64) {
    let _ = eng
        .catalog_mut()
        .create_dataset("local", DatasetMeta::new("perf", "US"), true);
    let now = Utc::now();
    let meta = TableMeta {
        name: name.to_owned(),
        kind: TableKind::Table,
        columns: vec![
            ColumnSchema::new("id", BqType::Int64),
            ColumnSchema::new("grp", BqType::Int64),
            ColumnSchema::new("val", BqType::Int64),
        ],
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
    let tref = TableRef::new("local", "perf", name);
    assert!(eng.create_table(&tref, meta).is_ok());
    let rows: Vec<Vec<BqValue>> = (0..num_rows)
        .map(|i| {
            vec![
                BqValue::Int64(i),
                BqValue::Int64(i % num_groups),
                BqValue::Int64((i * 13) % 100),
            ]
        })
        .collect();
    assert!(eng.insert_rows(&tref, rows).is_ok());
}

#[test]
fn non_quadratic_window_groupby_distinct_and_cte_correlated_guard() {
    let mut eng = Engine::new();
    seed_bench_table(&mut eng, "t_win", 2_000, 4);
    seed_bench_table(&mut eng, "t_gb", 4_000, 1_000);
    seed_bench_table(&mut eng, "t_cte", 2_000, 10);
    seed_bench_table(&mut eng, "t_outer", 150, 5);

    // Cumulative SUM + RANGE frame over 2,000 rows (would take seconds in debug if O(N^2))
    let t0 = Instant::now();
    let wqr = eng
        .query(
            "SELECT id,
                    SUM(val) OVER (PARTITION BY grp ORDER BY id) AS rsum,
                    COUNT(*) OVER (ORDER BY id RANGE BETWEEN 20 PRECEDING AND 20 FOLLOWING) AS rcnt
             FROM perf.t_win",
        )
        .unwrap();
    assert_eq!(wqr.rows.len(), 2_000);
    assert!(
        t0.elapsed().as_secs() < 5,
        "window regression: took {:?}",
        t0.elapsed()
    );

    // GROUP BY + DISTINCT over 4,000 rows / 1,000 groups (hashed O(N))
    let t1 = Instant::now();
    let gqr = eng
        .query("SELECT DISTINCT grp, SUM(val) AS s, COUNT(*) AS c FROM perf.t_gb GROUP BY grp")
        .unwrap();
    assert_eq!(gqr.rows.len(), 1_000);
    assert!(
        t1.elapsed().as_secs() < 5,
        "groupby/distinct regression: took {:?}",
        t1.elapsed()
    );

    // Correlated subquery inside query with large CTE (Arc<QueryResult> shared, no deep clone)
    let t2 = Instant::now();
    let cqr = eng
        .query(
            "WITH big AS (SELECT * FROM perf.t_cte)
             SELECT o.id, (SELECT COUNT(*) FROM UNNEST([o.id, o.val]) AS x WHERE x >= 0) AS n
             FROM perf.t_outer o
             WHERE EXISTS (SELECT 1 FROM (SELECT o.id AS k) WHERE k >= 0)",
        )
        .unwrap();
    assert_eq!(cqr.rows.len(), 150);
    assert!(
        t2.elapsed().as_secs() < 5,
        "correlated CTE regression: took {:?}",
        t2.elapsed()
    );
}
