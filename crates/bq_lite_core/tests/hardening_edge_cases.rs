use bq_lite_core::{BqErrorKind, BqType, BqValue, Engine};

#[test]
fn null_valued_expressions_preserve_declared_schema_types() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA sch;
         CREATE FUNCTION sch.null_num() RETURNS NUMERIC AS (NULL);",
    )
    .unwrap();

    let qr = eng
        .query(
            "SELECT
               SAFE_CAST('bad' AS INT64) AS safe_i,
               CAST(NULL AS NUMERIC) AS cast_n,
               CAST(NULL AS DATE) AS cast_d,
               ARRAY<INT64>[] AS empty_arr,
               IFNULL(NULL, 42) AS ifnull_i,
               NULLIF(NULL, 42) AS nullif_i,
               sch.null_num() AS udf_n",
        )
        .unwrap();

    assert_eq!(qr.columns[0].ty, BqType::Int64);
    assert_eq!(qr.columns[1].ty, BqType::Numeric);
    assert_eq!(qr.columns[2].ty, BqType::Date);
    assert_eq!(qr.columns[3].ty, BqType::array(BqType::Int64).unwrap());
    assert_eq!(qr.columns[4].ty, BqType::Int64);
    assert_eq!(qr.columns[5].ty, BqType::Int64);
    assert_eq!(qr.columns[6].ty, BqType::Numeric);
    assert_eq!(qr.rows[0][0], BqValue::Null);
    assert_eq!(qr.rows[0][1], BqValue::Null);
    assert_eq!(qr.rows[0][4], BqValue::Int64(42));
    assert_eq!(qr.rows[0][5], BqValue::Null);

    let multi = eng
        .query(
            "SELECT CASE WHEN x = 1 THEN NULL ELSE x END AS v FROM UNNEST([1, 2]) AS x ORDER BY x",
        )
        .unwrap();
    assert_eq!(multi.columns[0].ty, BqType::Int64);
    assert_eq!(multi.rows[0][0], BqValue::Null);
    assert_eq!(multi.rows[1][0], BqValue::Int64(2));
}

#[test]
fn sub_microsecond_temporal_literals_are_rejected() {
    let eng = Engine::new();

    for sql in [
        "SELECT TIMESTAMP '2024-01-01 00:00:00.1234567 UTC'",
        "SELECT DATETIME '2024-01-01 00:00:00.1234567'",
        "SELECT TIME '12:30:45.1234567'",
    ] {
        let err = eng.query(sql).unwrap_err();
        assert_eq!(err.kind, BqErrorKind::Semantic, "failed for: {sql}");
    }

    let ok = eng
        .query(
            "SELECT
               TIMESTAMP '2024-01-01 00:00:00.123456 UTC',
               DATETIME '2024-01-01 00:00:00.123456',
               TIME '12:30:45.123456'",
        )
        .unwrap();
    assert_eq!(ok.rows.len(), 1);
}

#[cfg(feature = "js-udf")]
#[test]
fn js_udf_isolates_globals_preserves_return_type_and_caches() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA jiso;
         CREATE FUNCTION jiso.leak() RETURNS INT64 LANGUAGE js AS 'globalThis.c = (globalThis.c || 0) + 1; return globalThis.c;';
         CREATE FUNCTION jiso.dirty_err() RETURNS INT64 LANGUAGE js AS 'globalThis.c = 999; throw new Error(\"boom\");';
         CREATE FUNCTION jiso.undef() RETURNS INT64 LANGUAGE js AS 'let a = 1;';
         CREATE FUNCTION jiso.inc(x INT64) RETURNS INT64 LANGUAGE js AS 'return x + 1;';",
    )
    .unwrap();

    let qr = eng
        .query("SELECT jiso.leak() AS a, SAFE.jiso.dirty_err() AS b, jiso.leak() AS c, jiso.undef() AS d")
        .unwrap();
    assert_eq!(qr.columns[0].ty, BqType::Int64);
    assert_eq!(qr.columns[1].ty, BqType::Int64);
    assert_eq!(qr.columns[2].ty, BqType::Int64);
    assert_eq!(qr.columns[3].ty, BqType::Int64);
    assert_eq!(qr.rows[0][0], BqValue::Int64(1));
    assert_eq!(qr.rows[0][1], BqValue::Null);
    assert_eq!(qr.rows[0][2], BqValue::Int64(1));
    assert_eq!(qr.rows[0][3], BqValue::Null);

    let sum_qr = eng
        .query("SELECT SUM(jiso.inc(x)) FROM UNNEST(GENERATE_ARRAY(1, 500)) AS x")
        .unwrap();
    assert_eq!(sum_qr.rows[0][0], BqValue::Int64(125_750));
}

#[test]
fn hash_join_handles_outer_null_cross_type_and_residual_predicates() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA hj;
         CREATE TABLE hj.l AS
           SELECT x AS id, CAST(x AS FLOAT64) AS fkey, x * 10 AS score FROM UNNEST(GENERATE_ARRAY(1, 50)) AS x
           UNION ALL SELECT NULL, NULL, 999
           UNION ALL SELECT 1, 1.0, 5;
         CREATE TABLE hj.r AS
           SELECT x AS id, CAST(x AS FLOAT64) AS fkey, x * 5 AS min_score FROM UNNEST(GENERATE_ARRAY(25, 75)) AS x
           UNION ALL SELECT NULL, NULL, 0;",
    )
    .unwrap();

    let inner = eng
        .query("SELECT COUNT(*) FROM hj.l a JOIN hj.r b ON a.id = b.id")
        .unwrap();
    assert_eq!(inner.rows[0][0], BqValue::Int64(26));

    let using_full = eng
        .query("SELECT COUNT(*) FROM hj.l FULL JOIN hj.r USING (id)")
        .unwrap();
    // 26 matched (25..=50) + 26 unmatched left (1..=24, dup id=1, NULL) + 26 unmatched right (51..=75, NULL) = 78
    assert_eq!(using_full.rows[0][0], BqValue::Int64(78));

    let cross_ty = eng
        .query("SELECT COUNT(*) FROM hj.l a JOIN hj.r b ON a.id = b.fkey AND a.score > b.min_score")
        .unwrap();
    assert_eq!(cross_ty.rows[0][0], BqValue::Int64(26));

    let date_join = eng
        .query(
            "WITH a AS (SELECT DATE '2026-01-01' AS d FROM UNNEST(GENERATE_ARRAY(1, 20)) AS x),
                  b AS (SELECT DATETIME '2026-01-01 00:00:00' AS dt FROM UNNEST(GENERATE_ARRAY(1, 10)) AS y)
             SELECT COUNT(*) FROM a JOIN b ON a.d = b.dt",
        )
        .unwrap();
    assert_eq!(date_join.rows[0][0], BqValue::Int64(200));
}

#[test]
fn using_hash_join_coalesce_precision_and_error_parity() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA ujh;
         CREATE TABLE ujh.l AS
           SELECT x AS id, x * 10 AS score FROM UNNEST(GENERATE_ARRAY(1, 50)) AS x;
         CREATE TABLE ujh.r AS
           SELECT x AS id, x * 5 AS min_score FROM UNNEST(GENERATE_ARRAY(25, 75)) AS x;",
    )
    .unwrap();

    // USING hash join preserves pre-coalesce qualified cols and coalesces outer USING key
    let using_star = eng
        .query(
            "SELECT id, a.id AS lid, b.id AS rid, a.score, b.min_score
             FROM ujh.l a FULL JOIN ujh.r b USING (id)
             WHERE id IN (2, 30, 70)
             ORDER BY id",
        )
        .unwrap();
    assert_eq!(
        using_star.rows,
        vec![
            vec![
                BqValue::Int64(2),
                BqValue::Int64(2),
                BqValue::Null,
                BqValue::Int64(20),
                BqValue::Null,
            ],
            vec![
                BqValue::Int64(30),
                BqValue::Int64(30),
                BqValue::Int64(30),
                BqValue::Int64(300),
                BqValue::Int64(150),
            ],
            vec![
                BqValue::Int64(70),
                BqValue::Null,
                BqValue::Int64(70),
                BqValue::Null,
                BqValue::Int64(350),
            ],
        ]
    );

    // Large INT64 keys (> 2^53) that collide in IEEE-754 f64 hash buckets must stay distinct
    let big_int_join = eng
        .query(
            "WITH a AS (SELECT 9007199254740992 + MOD(x, 3) AS k FROM UNNEST(GENERATE_ARRAY(1, 30)) AS x),
                  b AS (SELECT 9007199254740993 AS k FROM UNNEST(GENERATE_ARRAY(1, 5)) AS y)
             SELECT COUNT(*) FROM a JOIN b USING (k)",
        )
        .unwrap();
    assert_eq!(big_int_join.rows[0][0], BqValue::Int64(50));

    let err = eng
        .query("SELECT * FROM ujh.l JOIN ujh.r USING (no_such_col)")
        .unwrap_err();
    assert!(err.message.contains("USING column"), "got: {}", err.message);
}

fn seed_subquery_schema(eng: &mut Engine) {
    assert!(eng
        .exec_script(
            "CREATE SCHEMA sq;
             CREATE TABLE sq.a AS
               SELECT id, MOD(id, 20) AS k, CONCAT('s', CAST(MOD(id, 10) AS STRING)) AS s
               FROM UNNEST(GENERATE_ARRAY(1, 100)) AS id
               UNION ALL SELECT 101, NULL, NULL;
             CREATE TABLE sq.b AS
               SELECT k, CONCAT('s', CAST(k AS STRING)) AS s FROM UNNEST(GENERATE_ARRAY(0, 4)) AS k;",
        )
        .is_ok());
}

#[test]
fn subquery_in_cache_preserves_null_cross_type_and_error_semantics() {
    let mut eng = Engine::new();
    seed_subquery_schema(&mut eng);

    let cases = [
        (
            "SELECT COUNT(*) FROM sq.a WHERE k IN (SELECT k FROM sq.b)",
            25_i64,
        ),
        (
            "SELECT COUNT(*) FROM sq.a WHERE s IN (SELECT s FROM sq.b)",
            50,
        ),
        (
            "SELECT COUNT(*) FROM sq.a WHERE k NOT IN (SELECT k FROM sq.b)",
            75,
        ),
        (
            "SELECT COUNT(*) FROM sq.a WHERE k NOT IN (SELECT k FROM sq.b UNION ALL SELECT NULL)",
            0,
        ),
        (
            "SELECT COUNT(*) FROM sq.a WHERE k IN (SELECT k FROM sq.b UNION ALL SELECT NULL)",
            25,
        ),
        (
            "SELECT COUNT(*) FROM sq.a WHERE k NOT IN (SELECT k FROM sq.b WHERE k < 0)",
            101,
        ),
    ];
    for (sql, expected) in cases {
        let qr = eng.query(sql).unwrap();
        assert_eq!(qr.rows[0][0], BqValue::Int64(expected), "failed for: {sql}");
    }

    let nan_in = eng
        .query(
            "SELECT
               CAST('NaN' AS FLOAT64) IN (SELECT CAST('NaN' AS FLOAT64)) AS nan_eq,
               1.0 IN (SELECT k FROM sq.b) AS cross_num",
        )
        .unwrap();
    assert_eq!(
        nan_in.rows[0],
        vec![BqValue::Bool(false), BqValue::Bool(true)]
    );

    assert!(eng
        .query("SELECT COUNT(*) FROM sq.a WHERE s IN (SELECT k FROM sq.b)")
        .is_err());
    assert!(eng
        .query("SELECT COUNT(*) FROM sq.a WHERE k IN (SELECT k, s FROM sq.b)")
        .is_err());
}

#[test]
fn subquery_cache_skips_correlated_and_impure_subqueries() {
    let mut eng = Engine::new();
    seed_subquery_schema(&mut eng);

    let corr = eng
        .query(
            "SELECT a.id FROM sq.a a
             WHERE a.id <= 10 AND a.k IN (SELECT b.k FROM sq.b b WHERE b.k = a.id)
             ORDER BY a.id",
        )
        .unwrap();
    assert_eq!(
        corr.rows,
        vec![
            vec![BqValue::Int64(1)],
            vec![BqValue::Int64(2)],
            vec![BqValue::Int64(3)],
            vec![BqValue::Int64(4)],
        ]
    );

    let nested_corr = eng
        .query(
            "SELECT a.id FROM sq.a a
             WHERE a.id <= 6 AND EXISTS (SELECT 1 FROM sq.b b WHERE b.k IN (SELECT a.id))
             ORDER BY a.id",
        )
        .unwrap();
    assert_eq!(nested_corr.rows, corr.rows);

    let uuids = eng
        .query(
            "SELECT COUNT(DISTINCT u) FROM (
               SELECT (SELECT GENERATE_UUID()) AS u FROM UNNEST(GENERATE_ARRAY(1, 10)) AS x
             )",
        )
        .unwrap();
    assert_eq!(uuids.rows[0][0], BqValue::Int64(10));

    let rands = eng
        .query(
            "SELECT COUNT(DISTINCT r) FROM (
               SELECT (SELECT RAND()) AS r FROM UNNEST(GENERATE_ARRAY(1, 10)) AS x
             )",
        )
        .unwrap();
    assert!(matches!(rands.rows[0][0], BqValue::Int64(n) if n > 1));
}
