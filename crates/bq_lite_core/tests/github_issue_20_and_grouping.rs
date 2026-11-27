use bq_lite_core::types::BqValue;
use bq_lite_core::Engine;

#[test]
fn issue_20_temp_function_and_table_are_scoped_to_script() {
    let mut eng = Engine::new();

    // Job 1: CREATE TEMP FUNCTION + SELECT
    let qr1 = eng
        .exec_script("CREATE TEMP FUNCTION f(x INT64) AS (x + 1); SELECT f(1) AS v;")
        .unwrap();
    assert_eq!(qr1.rows, vec![vec![BqValue::Int64(2)]]);

    // Job 2: Identical script must succeed (no "Already Exists: Routine __temp.f").
    let qr2 = eng
        .exec_script("CREATE TEMP FUNCTION f(x INT64) AS (x + 1); SELECT f(1) AS v;")
        .unwrap();
    assert_eq!(qr2.rows, vec![vec![BqValue::Int64(2)]]);

    // Job 3: Calling f without defining it must fail.
    assert!(eng.query("SELECT f(1) AS v").is_err());

    // Temp tables are also scoped to the script, including on script failure.
    let qt1 = eng
        .exec_script("CREATE TEMP TABLE tmp AS SELECT 42 AS n; SELECT n FROM tmp;")
        .unwrap();
    assert_eq!(qt1.rows, vec![vec![BqValue::Int64(42)]]);
    assert!(eng.query("SELECT n FROM tmp").is_err());

    // Script that creates a temp function/table then errors must still clean up.
    assert!(eng
        .exec_script(
            "CREATE TEMP FUNCTION f_err(x INT64) AS (x + 1);
             CREATE TEMP TABLE tmp_err AS SELECT 1 AS n;
             SELECT 1 / 0;",
        )
        .is_err());
    assert!(eng.query("SELECT f_err(1)").is_err());
    assert!(eng.query("SELECT * FROM tmp_err").is_err());
}

#[test]
fn grouping_sets_rollup_cube_grouping_order_by_and_group_by_all() {
    let mut eng = Engine::new();
    eng.exec("CREATE SCHEMA ds").unwrap();
    eng.exec("CREATE TABLE ds.t (a INT64, b STRING, c INT64, k INT64)")
        .unwrap();
    eng.exec("INSERT INTO ds.t VALUES (1, 'x', 10, 5), (1, 'y', 20, 7), (2, 'x', 10, 3)")
        .unwrap();

    // GROUP BY () empty grouping set
    let empty_gb = eng
        .query("SELECT COUNT(*), SUM(k) FROM ds.t GROUP BY ()")
        .unwrap();
    assert_eq!(
        empty_gb.rows,
        vec![vec![BqValue::Int64(3), BqValue::Int64(15)]]
    );

    // Multi-column ROLLUP + GROUPING() + ORDER BY COUNT(*) / GROUPING()
    let rollup = eng
        .query(
            "SELECT a, UPPER(b) AS ub, c, GROUPING(a), GROUPING(UPPER(b)), GROUPING(c), SUM(k)
             FROM ds.t
             GROUP BY ROLLUP(a, (UPPER(b), c))
             ORDER BY GROUPING(a), GROUPING(UPPER(b)), a, ub, SUM(k)",
        )
        .unwrap();
    assert_eq!(rollup.rows.len(), 6);
    // Grand total row: all GROUPING() = 1, SUM(k) = 15
    assert_eq!(
        rollup.rows.last().unwrap(),
        &vec![
            BqValue::Null,
            BqValue::Null,
            BqValue::Null,
            BqValue::Int64(1),
            BqValue::Int64(1),
            BqValue::Int64(1),
            BqValue::Int64(15),
        ]
    );

    // GROUP BY ALL with window function + QUALIFY
    let gba = eng
        .query(
            "SELECT a, ROW_NUMBER() OVER (ORDER BY a) AS rn
             FROM ds.t
             GROUP BY ALL
             QUALIFY rn = 1",
        )
        .unwrap();
    assert_eq!(gba.rows, vec![vec![BqValue::Int64(1), BqValue::Int64(1)]]);

    // GROUP BY `ALL` (backticked column named ALL) + ORDER BY `ALL`
    eng.exec("CREATE TABLE ds.t_all (`ALL` INT64, v INT64)")
        .unwrap();
    eng.exec("INSERT INTO ds.t_all VALUES (1, 10), (1, 20), (2, 5)")
        .unwrap();
    let kw_col = eng
        .query("SELECT SUM(v) FROM ds.t_all GROUP BY `ALL` ORDER BY `ALL`")
        .unwrap();
    assert_eq!(
        kw_col.rows,
        vec![vec![BqValue::Int64(30)], vec![BqValue::Int64(5)]]
    );
}

#[test]
fn having_modifier_null_nan_string_agg_bytes_and_is_nan_inf() {
    let eng = Engine::new();

    // HAVING MAX / MIN returns NULL when all HAVING keys are NULL or any is NaN
    let h_null = eng
        .query(
            "SELECT MAX(id HAVING MAX k), MAX(id HAVING MIN k), ANY_VALUE(id HAVING MAX k)
             FROM (SELECT 1 AS id, CAST(NULL AS FLOAT64) AS k)",
        )
        .unwrap();
    assert_eq!(
        h_null.rows,
        vec![vec![BqValue::Null, BqValue::Null, BqValue::Null]]
    );

    let h_nan = eng
        .query(
            "SELECT MAX(id HAVING MAX k), MAX(id HAVING MIN k)
             FROM (
               SELECT 1 AS id, 1.0 AS k
               UNION ALL SELECT 2, CAST('NaN' AS FLOAT64)
             )",
        )
        .unwrap();
    assert_eq!(h_nan.rows, vec![vec![BqValue::Null, BqValue::Null]]);

    // STRING_AGG on BYTES + UPPER/LOWER on BYTES + IS_NAN / IS_INF
    let qr = eng
        .query(
            "SELECT
               STRING_AGG(b, b'-' ORDER BY b),
               UPPER(b'abc'),
               LOWER(b'XYZ'),
               IS_NAN(CAST('NaN' AS FLOAT64)),
               IS_NAN(1.5),
               IS_INF(CAST('Infinity' AS FLOAT64)),
               IS_INF(CAST('-Infinity' AS FLOAT64)),
               IS_INF(1.5),
               IS_NAN(CAST(NULL AS FLOAT64))
             FROM UNNEST([b'b', b'a']) AS b",
        )
        .unwrap();
    assert_eq!(
        qr.rows,
        vec![vec![
            BqValue::Bytes(b"a-b".to_vec()),
            BqValue::Bytes(b"ABC".to_vec()),
            BqValue::Bytes(b"xyz".to_vec()),
            BqValue::Bool(true),
            BqValue::Bool(false),
            BqValue::Bool(true),
            BqValue::Bool(true),
            BqValue::Bool(false),
            BqValue::Null,
        ]]
    );
}
