use std::collections::BTreeMap;

use bq_lite_core::catalog::{ColumnSchema, TableKind, TableMeta, TableRef};
use bq_lite_core::{BqErrorKind, BqResult, BqType, BqValue, Engine};
use chrono::Utc;

fn setup_orders_engine() -> BqResult<Engine> {
    let mut engine = Engine::with_project("p1");
    engine.create_dataset("p1", "ds")?;
    let tref = TableRef::new("p1", "ds", "orders");
    let now = Utc::now();
    engine.create_table(
        &tref,
        TableMeta {
            name: "orders".to_owned(),
            kind: TableKind::Table,
            columns: vec![
                ColumnSchema::new("id", BqType::Int64),
                ColumnSchema::new("customer", BqType::String),
                ColumnSchema::new("amount", BqType::Int64),
                ColumnSchema::new("tags", BqType::array(BqType::String)?),
            ],
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
            primary_key: Vec::new(),
        },
    )?;
    engine.insert_rows(
        &tref,
        vec![
            vec![
                BqValue::Int64(1),
                BqValue::String("alice".to_owned()),
                BqValue::Int64(100),
                BqValue::Array(vec![
                    BqValue::String("vip".to_owned()),
                    BqValue::String("fast".to_owned()),
                ]),
            ],
            vec![
                BqValue::Int64(2),
                BqValue::String("alice".to_owned()),
                BqValue::Int64(50),
                BqValue::Array(vec![BqValue::String("vip".to_owned())]),
            ],
            vec![
                BqValue::Int64(3),
                BqValue::String("bob".to_owned()),
                BqValue::Int64(200),
                BqValue::Array(Vec::new()),
            ],
        ],
    )?;
    Ok(engine)
}

#[test]
fn executes_scalar_expressions_safe_cast_and_subscripts() {
    let engine = Engine::new();
    let res = engine
        .query(
            "SELECT 1 + 2 * 3 AS math, SAFE_CAST('nope' AS INT64) AS bad_cast, [10, 20][SAFE_OFFSET(5)] AS oob, ARRAY_LENGTH([1, 2, 3]) AS len",
        )
        .unwrap();
    assert_eq!(res.columns.len(), 4);
    assert_eq!(res.columns[0].ty, BqType::Int64);
    assert_eq!(
        res.rows,
        vec![vec![
            BqValue::Int64(7),
            BqValue::Null,
            BqValue::Null,
            BqValue::Int64(3),
        ]]
    );

    let err = engine.query("SELECT CAST('nope' AS INT64)").unwrap_err();
    assert_eq!(err.kind, BqErrorKind::Semantic);
}

#[test]
fn executes_correlated_unnest_with_offset_and_left_join() {
    let engine = setup_orders_engine().unwrap();
    let cross = engine
        .query(
            "SELECT o.id, tag, pos FROM ds.orders o, UNNEST(o.tags) AS tag WITH OFFSET AS pos ORDER BY o.id, pos",
        )
        .unwrap();
    assert_eq!(
        cross.rows,
        vec![
            vec![
                BqValue::Int64(1),
                BqValue::String("vip".to_owned()),
                BqValue::Int64(0)
            ],
            vec![
                BqValue::Int64(1),
                BqValue::String("fast".to_owned()),
                BqValue::Int64(1)
            ],
            vec![
                BqValue::Int64(2),
                BqValue::String("vip".to_owned()),
                BqValue::Int64(0)
            ],
        ]
    );

    let left = engine
        .query(
            "SELECT o.id, tag FROM ds.orders o LEFT JOIN UNNEST(o.tags) AS tag ORDER BY o.id, tag NULLS LAST",
        )
        .unwrap();
    assert_eq!(left.rows.len(), 4);
    assert_eq!(left.rows[3], vec![BqValue::Int64(3), BqValue::Null]);
}

#[test]
fn executes_wildcard_except_replace_and_as_struct() {
    let engine = setup_orders_engine().unwrap();
    let res = engine
        .query("SELECT * EXCEPT(tags) REPLACE(amount * 2 AS amount) FROM ds.orders ORDER BY id LIMIT 1")
        .unwrap();
    assert_eq!(res.columns.len(), 3);
    assert_eq!(res.columns[0].name, "id");
    assert_eq!(res.columns[1].name, "customer");
    assert_eq!(res.columns[2].name, "amount");
    assert_eq!(
        res.rows[0],
        vec![
            BqValue::Int64(1),
            BqValue::String("alice".to_owned()),
            BqValue::Int64(200),
        ]
    );
}

#[test]
fn executes_group_by_all_rollup_and_aggregates() {
    let engine = setup_orders_engine().unwrap();
    let res = engine
        .query(
            "SELECT customer, COUNT(*) AS cnt, SUM(amount) AS total, ARRAY_AGG(id ORDER BY id DESC) AS ids FROM ds.orders GROUP BY ALL ORDER BY customer",
        )
        .unwrap();
    assert_eq!(
        res.rows,
        vec![
            vec![
                BqValue::String("alice".to_owned()),
                BqValue::Int64(2),
                BqValue::Int64(150),
                BqValue::Array(vec![BqValue::Int64(2), BqValue::Int64(1)]),
            ],
            vec![
                BqValue::String("bob".to_owned()),
                BqValue::Int64(1),
                BqValue::Int64(200),
                BqValue::Array(vec![BqValue::Int64(3)]),
            ],
        ]
    );

    let rollup = engine
        .query("SELECT customer, SUM(amount) AS total FROM ds.orders GROUP BY ROLLUP(customer)")
        .unwrap();
    assert_eq!(rollup.rows.len(), 3);
}

#[test]
fn executes_windows_named_window_and_qualify() {
    let engine = setup_orders_engine().unwrap();
    let res = engine
        .query(
            "SELECT id, customer, amount, ROW_NUMBER() OVER w AS rn, SUM(amount) OVER (PARTITION BY customer) AS cust_total FROM ds.orders WINDOW w AS (PARTITION BY customer ORDER BY amount DESC) QUALIFY rn = 1 ORDER BY id",
        )
        .unwrap();
    assert_eq!(
        res.rows,
        vec![
            vec![
                BqValue::Int64(1),
                BqValue::String("alice".to_owned()),
                BqValue::Int64(100),
                BqValue::Int64(1),
                BqValue::Int64(150),
            ],
            vec![
                BqValue::Int64(3),
                BqValue::String("bob".to_owned()),
                BqValue::Int64(200),
                BqValue::Int64(1),
                BqValue::Int64(200),
            ],
        ]
    );
}

#[test]
fn executes_recursive_cte_set_ops_and_pipe_syntax() {
    let engine = setup_orders_engine().unwrap();
    let rec = engine
        .query(
            "WITH RECURSIVE nums AS (SELECT 1 AS n UNION ALL SELECT n + 1 FROM nums WHERE n < 5) SELECT SUM(n) AS s FROM nums",
        )
        .unwrap();
    assert_eq!(rec.rows, vec![vec![BqValue::Int64(15)]]);

    let pipe = engine
        .query(
            "FROM ds.orders |> WHERE amount >= 100 |> EXTEND amount * 10 AS scaled |> SELECT id, scaled |> ORDER BY id",
        )
        .unwrap();
    assert_eq!(
        pipe.rows,
        vec![
            vec![BqValue::Int64(1), BqValue::Int64(1000)],
            vec![BqValue::Int64(3), BqValue::Int64(2000)],
        ]
    );

    let by_name = engine
        .query("SELECT 1 AS a, 'x' AS b UNION ALL BY NAME SELECT 'y' AS b, 2 AS a ORDER BY a")
        .unwrap();
    assert_eq!(
        by_name.rows,
        vec![
            vec![BqValue::Int64(1), BqValue::String("x".to_owned())],
            vec![BqValue::Int64(2), BqValue::String("y".to_owned())],
        ]
    );
}

#[test]
fn executes_pivot_and_unpivot() {
    let engine = setup_orders_engine().unwrap();
    let pivoted = engine
        .query(
            "SELECT * FROM (SELECT customer, amount FROM ds.orders) PIVOT(SUM(amount) FOR customer IN ('alice', 'bob'))",
        )
        .unwrap();
    assert_eq!(pivoted.rows.len(), 1);
    assert_eq!(
        pivoted.rows[0],
        vec![BqValue::Int64(150), BqValue::Int64(200)]
    );

    let unpivoted = engine
        .query(
            "SELECT col, val FROM (SELECT 10 AS q1, 20 AS q2) UNPIVOT (val FOR col IN (q1, q2)) ORDER BY col",
        )
        .unwrap();
    assert_eq!(
        unpivoted.rows,
        vec![
            vec![BqValue::String("q1".to_owned()), BqValue::Int64(10)],
            vec![BqValue::String("q2".to_owned()), BqValue::Int64(20)],
        ]
    );
}

#[test]
fn fork_isolated_keeps_parent_and_sibling_clean() {
    let parent = setup_orders_engine().unwrap();
    let mut fork_a = parent.fork_isolated();
    let fork_b = parent.fork_isolated();

    fork_a
        .insert_rows(
            &TableRef::new("p1", "ds", "orders"),
            vec![vec![
                BqValue::Int64(99),
                BqValue::String("charlie".to_owned()),
                BqValue::Int64(999),
                BqValue::Array(Vec::new()),
            ]],
        )
        .unwrap();

    assert_eq!(
        parent.query("SELECT COUNT(*) FROM ds.orders").unwrap().rows[0][0],
        BqValue::Int64(3)
    );
    assert_eq!(
        fork_b.query("SELECT COUNT(*) FROM ds.orders").unwrap().rows[0][0],
        BqValue::Int64(3)
    );
    assert_eq!(
        fork_a.query("SELECT COUNT(*) FROM ds.orders").unwrap().rows[0][0],
        BqValue::Int64(4)
    );
}

#[test]
fn executes_range_window_frames_with_numeric_and_interval_offsets() {
    let engine = Engine::new();
    let res = engine
        .query(
            "WITH t AS (
               SELECT 1 AS x, 10 AS v UNION ALL
               SELECT 2, 20 UNION ALL
               SELECT 5, 50 UNION ALL
               SELECT 6, 60
             )
             SELECT
               x,
               SUM(v) OVER (ORDER BY x RANGE BETWEEN 1 PRECEDING AND 1 FOLLOWING) AS win_sum,
               COUNT(*) OVER (ORDER BY x RANGE BETWEEN 10 PRECEDING AND 5 PRECEDING) AS empty_cnt,
               SUM(v) OVER (ORDER BY x DESC RANGE BETWEEN 1 PRECEDING AND CURRENT ROW) AS desc_sum
             FROM t
             ORDER BY x",
        )
        .unwrap();
    assert_eq!(
        res.rows,
        vec![
            vec![
                BqValue::Int64(1),
                BqValue::Int64(30),
                BqValue::Int64(0),
                BqValue::Int64(30),
            ],
            vec![
                BqValue::Int64(2),
                BqValue::Int64(30),
                BqValue::Int64(0),
                BqValue::Int64(20),
            ],
            vec![
                BqValue::Int64(5),
                BqValue::Int64(110),
                BqValue::Int64(0),
                BqValue::Int64(110),
            ],
            vec![
                BqValue::Int64(6),
                BqValue::Int64(110),
                BqValue::Int64(1),
                BqValue::Int64(60),
            ],
        ]
    );

    let ts_res = engine
        .query(
            "WITH events AS (
               SELECT DATE '2026-01-01' AS d, 1 AS n UNION ALL
               SELECT DATE '2026-01-02', 2 UNION ALL
               SELECT DATE '2026-01-05', 5
             )
             SELECT d, SUM(n) OVER (ORDER BY d RANGE BETWEEN INTERVAL 1 DAY PRECEDING AND CURRENT ROW) AS s
             FROM events
             ORDER BY d",
        )
        .unwrap();
    assert_eq!(ts_res.rows[0][1], BqValue::Int64(1));
    assert_eq!(ts_res.rows[1][1], BqValue::Int64(3));
    assert_eq!(ts_res.rows[2][1], BqValue::Int64(5));
}
