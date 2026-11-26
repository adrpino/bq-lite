use bq_lite_core::{BqValue, Engine};

#[test]
fn covers_all_window_functions_and_frames() {
    let mut e = Engine::new();
    e.exec("CREATE SCHEMA ds").unwrap();
    e.exec("CREATE TABLE ds.scores (grp STRING, val INT64)")
        .unwrap();
    e.exec("INSERT INTO ds.scores VALUES ('a', 10), ('a', 10), ('a', 20), ('a', NULL), ('b', 5)")
        .unwrap();

    let res = e
        .query(
            "SELECT grp, val,
                RANK() OVER (PARTITION BY grp ORDER BY val ASC NULLS LAST) AS rnk,
                DENSE_RANK() OVER (PARTITION BY grp ORDER BY val ASC NULLS LAST) AS drnk,
                NTILE(2) OVER (PARTITION BY grp ORDER BY val ASC NULLS LAST) AS nt,
                LAG(val, 1, -1) IGNORE NULLS OVER (PARTITION BY grp ORDER BY val ASC NULLS LAST) AS lg,
                LEAD(val, 1, -1) OVER (PARTITION BY grp ORDER BY val ASC NULLS LAST) AS ld,
                FIRST_VALUE(val IGNORE NULLS) OVER (PARTITION BY grp ORDER BY val ASC NULLS LAST ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) AS fv,
                LAST_VALUE(val IGNORE NULLS) OVER (PARTITION BY grp ORDER BY val ASC NULLS LAST ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) AS lv,
                NTH_VALUE(val, 2) OVER (PARTITION BY grp ORDER BY val ASC NULLS LAST ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) AS nv,
                PERCENT_RANK() OVER (PARTITION BY grp ORDER BY val ASC NULLS LAST) AS pr,
                CUME_DIST() OVER (PARTITION BY grp ORDER BY val ASC NULLS LAST) AS cd
            FROM ds.scores
            ORDER BY grp, val NULLS LAST",
        )
        .unwrap();
    assert_eq!(res.rows.len(), 5);
    assert_eq!(res.rows[0][2], BqValue::Int64(1));
    assert_eq!(res.rows[2][2], BqValue::Int64(3));
    assert_eq!(res.rows[2][3], BqValue::Int64(2));
    assert_eq!(res.rows[2][7], BqValue::Int64(10));
    assert_eq!(res.rows[2][8], BqValue::Int64(20));
    assert_eq!(res.rows[4][10], BqValue::Float64(0.0));
}

#[test]
fn covers_all_aggregates_cube_and_grouping_sets() {
    let mut e = Engine::new();
    e.exec("CREATE SCHEMA ds").unwrap();
    e.exec("CREATE TABLE ds.t (k STRING, n INT64, flag BOOL, ts INT64)")
        .unwrap();
    e.exec("INSERT INTO ds.t VALUES ('x', 10, TRUE, 1), ('x', 20, FALSE, 2), ('y', 30, TRUE, 3)")
        .unwrap();

    let aggs = e
        .query(
            "SELECT k,
                COUNTIF(flag) AS cf,
                AVG(n) AS avg_n,
                MIN(n) AS min_n,
                MAX(n) AS max_n,
                STRING_AGG(CAST(n AS STRING), ',' ORDER BY n DESC) AS s_agg,
                ANY_VALUE(n HAVING MAX ts) AS latest_n,
                LOGICAL_AND(flag) AS l_and,
                LOGICAL_OR(flag) AS l_or,
                BIT_AND(n) AS b_and,
                BIT_OR(n) AS b_or,
                BIT_XOR(n) AS b_xor,
                STDDEV_POP(n) AS spop,
                VAR_POP(n) AS vpop
            FROM ds.t GROUP BY k ORDER BY k",
        )
        .unwrap();
    assert_eq!(aggs.rows.len(), 2);
    assert_eq!(aggs.rows[0][1], BqValue::Int64(1));
    assert_eq!(aggs.rows[0][2], BqValue::Float64(15.0));
    assert_eq!(aggs.rows[0][5], BqValue::String("20,10".to_owned()));
    assert_eq!(aggs.rows[0][6], BqValue::Int64(20));
    assert_eq!(aggs.rows[0][7], BqValue::Bool(false));
    assert_eq!(aggs.rows[0][8], BqValue::Bool(true));

    let cube = e
        .query("SELECT k, GROUPING(k) AS gk, SUM(n) FROM ds.t GROUP BY CUBE(k)")
        .unwrap();
    assert_eq!(cube.rows.len(), 3);

    let gsets = e
        .query("SELECT k, SUM(n) FROM ds.t GROUP BY GROUPING SETS((k), ())")
        .unwrap();
    assert_eq!(gsets.rows.len(), 3);
}

#[test]
fn covers_subqueries_operators_numeric_and_remaining_scalars() {
    let e = Engine::new();
    let res = e
        .query(
            "SELECT
                EXISTS(SELECT 1 WHERE TRUE),
                2 IN (SELECT x FROM UNNEST([1, 2, 3]) AS x),
                2 IN UNNEST([1, 2, 3]),
                5 BETWEEN 1 AND 10,
                'foobar' LIKE 'foo%r',
                NULL IS DISTINCT FROM 1,
                NULL IS NOT DISTINCT FROM NULL,
                EXTRACT(YEAR FROM DATE '2026-07-04'),
                EXTRACT(HOUR FROM TIMESTAMP '2026-07-04 15:30:00 UTC'),
                NUMERIC '10.25' + NUMERIC '0.75',
                BIGNUMERIC '1.5' * BIGNUMERIC '2.0',
                FORMAT('%s=%d', 'k', 7),
                TO_HEX(FROM_HEX('deadbeef')),
                TO_BASE64(FROM_BASE64('aGk=')),
                TRIM('__hi__', '_'),
                SPLIT('a,b,c'),
                TRANSLATE('abc', 'ac', 'xz'),
                REPEAT('ab', 3),
                LEFT('abcdef', 2),
                RIGHT('abcdef', 2),
                STARTS_WITH('hello', 'he'),
                ENDS_WITH('hello', 'lo'),
                JSON_VALUE_ARRAY(PARSE_JSON('[\"a\",\"b\"]')),
                JSON_KEYS(JSON_OBJECT('k1', 1, 'k2', 2)),
                JSON_VALUE(JSON_SET(PARSE_JSON('{\"a\":1}'), '$.a', 9), '$.a'),
                ARRAY_FIRST(ARRAY_CONCAT([1], [2, 3])),
                ARRAY_LAST([10, 20]),
                SQRT(9),
                POW(2, 5),
                SAFE_ADD(1, 2),
                FARM_FINGERPRINT('this string is definitely longer than sixty four bytes so it hits hash_len_gt_64 in farmhash!!')",
        )
        .unwrap();
    assert_eq!(res.rows[0][0], BqValue::Bool(true));
    assert_eq!(res.rows[0][1], BqValue::Bool(true));
    assert_eq!(res.rows[0][2], BqValue::Bool(true));
    assert_eq!(res.rows[0][3], BqValue::Bool(true));
    assert_eq!(res.rows[0][4], BqValue::Bool(true));
    assert_eq!(res.rows[0][5], BqValue::Bool(true));
    assert_eq!(res.rows[0][6], BqValue::Bool(true));
    assert_eq!(res.rows[0][7], BqValue::Int64(2026));
    assert_eq!(res.rows[0][8], BqValue::Int64(15));
    assert_eq!(res.rows[0][11], BqValue::String("k=7".to_owned()));
    assert_eq!(res.rows[0][12], BqValue::String("deadbeef".to_owned()));
    assert_eq!(res.rows[0][13], BqValue::String("aGk=".to_owned()));
    assert_eq!(res.rows[0][14], BqValue::String("hi".to_owned()));
}

#[test]
fn covers_datetime_constructors_alter_table_policies_and_while_repeat() {
    let mut e = Engine::new();
    e.exec("CREATE SCHEMA ds").unwrap();
    e.exec("CREATE TABLE ds.t (a INT64, b STRING)").unwrap();
    e.exec("INSERT INTO ds.t VALUES (1, 'x')").unwrap();
    e.exec("ALTER TABLE ds.t RENAME COLUMN b TO label, ALTER COLUMN a SET DATA TYPE NUMERIC, ADD COLUMN extra INT64 DEFAULT 9")
        .unwrap();
    e.exec("CREATE ROW ACCESS POLICY p1 ON ds.t GRANT TO ('user:a@b.com') FILTER USING (a = NUMERIC '1')")
        .unwrap();
    e.exec("DROP ROW ACCESS POLICY p1 ON ds.t").unwrap();
    e.exec("ALTER TABLE ds.t RENAME TO t2").unwrap();
    let row = e.query("SELECT a, label, extra FROM ds.t2").unwrap();
    assert_eq!(
        row.rows,
        vec![vec![
            BqValue::Numeric(bq_lite_core::BqNumeric::from_i64(1)),
            BqValue::String("x".to_owned()),
            BqValue::Int64(9),
        ]]
    );

    let loop_res = e
        .exec_script(
            "
            DECLARE i INT64 DEFAULT 0;
            DECLARE acc INT64 DEFAULT 0;
            WHILE i < 3 DO
                SET i = i + 1;
                SET acc = acc + i;
            END WHILE;
            REPEAT
                SET i = i - 1;
            UNTIL i = 0 END REPEAT;
            SELECT acc,
                TIMESTAMP_SECONDS(1700000000),
                UNIX_SECONDS(TIMESTAMP_SECONDS(1700000000)),
                DATE_FROM_UNIX_DATE(UNIX_DATE(DATE '2026-01-01')),
                DATE_TRUNC(DATE '2026-05-19', MONTH),
                TIMESTAMP_TRUNC(TIMESTAMP '2026-05-19 14:25:30 UTC', HOUR);
            ",
        )
        .unwrap();
    assert_eq!(loop_res.rows[0][0], BqValue::Int64(6));
    assert_eq!(loop_res.rows[0][2], BqValue::Int64(1_700_000_000));
}
