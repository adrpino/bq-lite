use bq_lite_core::error::BqErrorKind;
use bq_lite_core::types::BqValue;
use bq_lite_core::Engine;

#[test]
fn window_frames_empty_rows_range_extremes_and_frame_validation() {
    let eng = Engine::new();

    // Empty ROWS frame on leading rows returns NULL for aggregates
    let rows_qr = eng
        .query(
            "SELECT x, MIN(x) OVER (ORDER BY x ROWS BETWEEN UNBOUNDED PRECEDING AND 2 PRECEDING) AS m
             FROM UNNEST([1, 2, 3, 4]) AS x
             ORDER BY x",
        )
        .unwrap();
    assert_eq!(
        rows_qr.rows,
        vec![
            vec![BqValue::Int64(1), BqValue::Null],
            vec![BqValue::Int64(2), BqValue::Null],
            vec![BqValue::Int64(3), BqValue::Int64(1)],
            vec![BqValue::Int64(4), BqValue::Int64(1)],
        ]
    );

    // RANGE frame at i64::MIN / i64::MAX must not overflow and includes leading NULLs on UNBOUNDED PRECEDING
    let range_qr = eng
        .query(
            "SELECT id, COUNT(*) OVER (ORDER BY v ASC RANGE BETWEEN UNBOUNDED PRECEDING AND 2 PRECEDING) AS c
             FROM (
               SELECT 1 AS id, CAST(NULL AS INT64) AS v
               UNION ALL SELECT 2, -9223372036854775807
               UNION ALL SELECT 3, 9223372036854775807
             )
             ORDER BY id",
        )
        .unwrap();
    assert_eq!(
        range_qr.rows,
        vec![
            vec![BqValue::Int64(1), BqValue::Int64(1)],
            vec![BqValue::Int64(2), BqValue::Int64(1)],
            vec![BqValue::Int64(3), BqValue::Int64(2)],
        ]
    );

    // LAG / LEAD / ROW_NUMBER must reject window frames
    for bad in [
        "SELECT LEAD(x) OVER (ORDER BY x ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM UNNEST([1, 2]) x",
        "SELECT LAG(x) OVER (ORDER BY x RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM UNNEST([1, 2]) x",
        "SELECT ROW_NUMBER() OVER (ORDER BY x ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM UNNEST([1, 2]) x",
    ] {
        let err = eng.query(bad).unwrap_err();
        assert_eq!(err.kind, BqErrorKind::Query, "expected error for: {bad}");
    }

    // Analytic STRING_AGG on BYTES
    let sa = eng
        .query(
            "SELECT STRING_AGG(b, b'-') OVER (ORDER BY i ROWS BETWEEN 1 PRECEDING AND CURRENT ROW)
             FROM (SELECT 1 AS i, b'a' AS b UNION ALL SELECT 2, b'b')
             ORDER BY i",
        )
        .unwrap();
    assert_eq!(
        sa.rows,
        vec![
            vec![BqValue::Bytes(b"a".to_vec())],
            vec![BqValue::Bytes(b"a-b".to_vec())],
        ]
    );
}

#[test]
fn correlated_subqueries_unnest_array_joins_and_nested_joins() {
    let eng = Engine::new();

    // 2-level nested correlated subquery referencing grandparent outer column
    let nested = eng
        .query(
            "SELECT (SELECT (SELECT CONCAT(p, v)) FROM (SELECT '#' AS p)) AS out_s
             FROM UNNEST(['a', 'b']) AS v
             ORDER BY out_s",
        )
        .unwrap();
    assert_eq!(
        nested.rows,
        vec![
            vec![BqValue::String("#a".to_owned())],
            vec![BqValue::String("#b".to_owned())],
        ]
    );

    // RIGHT JOIN UNNEST against empty array + correlated WITH OFFSET USING (OFFSET) column order
    let rj = eng
        .query(
            "SELECT a, b
             FROM UNNEST(ARRAY<INT64>[]) AS a
             RIGHT JOIN UNNEST([10, 20]) AS b ON a = b
             ORDER BY b",
        )
        .unwrap();
    assert_eq!(
        rj.rows,
        vec![
            vec![BqValue::Null, BqValue::Int64(10)],
            vec![BqValue::Null, BqValue::Int64(20)],
        ]
    );

    let off = eng
        .query(
            "SELECT (
               SELECT AS STRUCT *
               FROM sub.a AS ea WITH OFFSET
               RIGHT JOIN sub.b AS eb WITH OFFSET USING (OFFSET)
               ORDER BY OFFSET
               LIMIT 1
             )
             FROM (SELECT [1] AS a, [2, 4] AS b) AS sub",
        )
        .unwrap();
    // USING column (OFFSET=0) must come first, then ea=1, then eb=2
    let BqValue::Struct(ref fields) = off.rows[0][0] else {
        panic!("expected struct");
    };
    let vals: Vec<&BqValue> = fields.iter().map(|f| &f.value).collect();
    assert_eq!(
        vals,
        vec![&BqValue::Int64(0), &BqValue::Int64(1), &BqValue::Int64(2)]
    );
}

#[test]
fn join_star_order_nested_joins_and_temporal_range_array_ordering() {
    let mut eng = Engine::new();

    // SELECT R.*, S.* preserves per-table column order + parenthesized join
    eng.exec("CREATE SCHEMA jds").unwrap();
    eng.exec("CREATE TABLE jds.r (k INT64, id INT64, a STRING)")
        .unwrap();
    eng.exec("CREATE TABLE jds.s (k INT64, id INT64, b STRING)")
        .unwrap();
    eng.exec("CREATE TABLE jds.d (id INT64, tag STRING)")
        .unwrap();
    eng.exec("INSERT INTO jds.r VALUES (2, 10, 'a1')").unwrap();
    eng.exec("INSERT INTO jds.s VALUES (1, 10, 'b1')").unwrap();
    eng.exec("INSERT INTO jds.d VALUES (10, 't1')").unwrap();

    let stars = eng
        .query("SELECT r.*, s.* FROM jds.r JOIN jds.s USING (id)")
        .unwrap();
    assert_eq!(
        stars.rows,
        vec![vec![
            BqValue::Int64(2),
            BqValue::Int64(10),
            BqValue::String("a1".to_owned()),
            BqValue::Int64(1),
            BqValue::Int64(10),
            BqValue::String("b1".to_owned()),
        ]]
    );

    let pjoin = eng
        .query("SELECT r.a, s.b, d.tag FROM jds.r JOIN (jds.s JOIN jds.d ON s.id = d.id) ON r.id = s.id")
        .unwrap();
    assert_eq!(
        pjoin.rows,
        vec![vec![
            BqValue::String("a1".to_owned()),
            BqValue::String("b1".to_owned()),
            BqValue::String("t1".to_owned()),
        ]]
    );

    // Temporal string literal coercion in ARRAY literal + RANGE/ARRAY/INTERVAL ordering
    let arr_date = eng
        .query("SELECT x FROM UNNEST([DATE '2020-01-01', '2020-02-02']) AS x ORDER BY x")
        .unwrap();
    assert_eq!(arr_date.rows.len(), 2);
    let cmp_qr = eng
        .query("SELECT [1, 1] < [1, 2], RANGE(DATE '2020-01-01', DATE '2020-01-05') < RANGE(DATE '2020-01-02', DATE '2020-01-05'), INTERVAL 1 DAY < INTERVAL 2 DAY")
        .unwrap();
    assert_eq!(
        cmp_qr.rows,
        vec![vec![
            BqValue::Bool(true),
            BqValue::Bool(true),
            BqValue::Bool(true),
        ]]
    );
}

#[test]
fn with_recursive_forward_refs_non_recursive_union_by_name_and_empty_base() {
    let eng = Engine::new();

    // Forward reference inside WITH RECURSIVE
    let fwd = eng
        .query("WITH RECURSIVE r AS (SELECT a FROM s), s AS (SELECT 2 AS a) SELECT a FROM r")
        .unwrap();
    assert_eq!(fwd.rows, vec![vec![BqValue::Int64(2)]]);

    // Non-recursive UNION ALL CTE inside WITH RECURSIVE + cyclic UNION DISTINCT traversal
    let flights = eng
        .query(
            "WITH RECURSIVE
               edges AS (
                 SELECT 'A' AS src, 'B' AS dst UNION ALL
                 SELECT 'B', 'C' UNION ALL
                 SELECT 'C', 'A'
               ),
               reach AS (
                 SELECT src, dst FROM edges
                 UNION DISTINCT
                 SELECT r.src, e.dst FROM reach r JOIN edges e ON r.dst = e.src
               )
             SELECT COUNT(*) FROM reach",
        )
        .unwrap();
    assert_eq!(flights.rows, vec![vec![BqValue::Int64(9)]]);

    // UNION DISTINCT BY NAME recursive CTE aligns swapped columns by name
    let by_name = eng
        .query(
            "WITH RECURSIVE t AS (
               SELECT 1 AS a, 2 AS b
               UNION DISTINCT BY NAME
               SELECT b, a FROM t
             )
             SELECT a, b FROM t",
        )
        .unwrap();
    assert_eq!(
        by_name.rows,
        vec![vec![BqValue::Int64(1), BqValue::Int64(2)]]
    );

    // Empty base term still evaluates recursive term once (UNION DISTINCT terminates with 5 rows)
    let empty_base = eng
        .query(
            "WITH RECURSIVE
               t0 AS (SELECT 0 AS n),
               t AS (
                 SELECT n FROM t0 WHERE FALSE
                 UNION DISTINCT
                 SELECT * FROM (SELECT MOD(n + 1, 5) AS n FROM t UNION ALL SELECT 1 AS n)
               )
             SELECT n FROM t ORDER BY n",
        )
        .unwrap();
    assert_eq!(
        empty_base.rows,
        vec![
            vec![BqValue::Int64(0)],
            vec![BqValue::Int64(1)],
            vec![BqValue::Int64(2)],
            vec![BqValue::Int64(3)],
            vec![BqValue::Int64(4)],
        ]
    );
}
