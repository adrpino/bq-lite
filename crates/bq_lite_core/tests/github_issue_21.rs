use bq_lite_core::{BqType, BqValue, Engine, StructField};

#[test]
fn issue_21_exact_repro_table_alias_as_row_struct() {
    let eng = Engine::new();
    let res = eng
        .query(
            "WITH a AS (SELECT 1 AS id, 'x' AS v),
                  b AS (SELECT 1 AS id, 'y' AS w)
             SELECT
               TO_JSON_STRING(a) AS j1,
               (SELECT TO_JSON_STRING(b) FROM a LEFT JOIN b ON a.id = b.id) AS j2,
               (SELECT TO_JSON_STRING(IF(b.id IS NULL, NULL, b)) FROM a FULL JOIN b ON a.id = b.id) AS j3
             FROM a",
        )
        .unwrap();
    assert_eq!(res.rows.len(), 1);
    assert_eq!(
        res.rows[0],
        vec![
            BqValue::String(r#"{"id":1,"v":"x"}"#.to_owned()),
            BqValue::String(r#"{"id":1,"w":"y"}"#.to_owned()),
            BqValue::String(r#"{"id":1,"w":"y"}"#.to_owned()),
        ]
    );
}

#[test]
fn issue_21_self_full_outer_join_pairing_matched() {
    let eng = Engine::new();
    let res = eng
        .query(
            "WITH t AS (
               SELECT 'a' AS k, 1 AS i, 10 AS val UNION ALL
               SELECT 'a', 2, 20 UNION ALL
               SELECT 'a', 3, 30
             ),
             paired AS (
               SELECT
                 c AS raw_curr,
                 p AS raw_prev,
                 IF(c.k IS NULL, NULL, c) AS curr,
                 IF(p.k IS NULL, NULL, p) AS prev
               FROM (SELECT * FROM t WHERE i >= 2) c
               FULL OUTER JOIN (SELECT * FROM t WHERE i <= 2) p
                 ON p.k = c.k AND p.i + 1 = c.i
             )
             SELECT
               TO_JSON_STRING(raw_curr) AS rc,
               TO_JSON_STRING(raw_prev) AS rp,
               curr.i AS ci,
               curr.val AS cval,
               prev.i AS pi,
               prev.val AS pval
             FROM paired
             ORDER BY COALESCE(curr.i, 99), COALESCE(prev.i, 99)",
        )
        .unwrap();
    assert_eq!(res.rows.len(), 2);
    assert_eq!(
        res.rows[0],
        vec![
            BqValue::String(r#"{"k":"a","i":2,"val":20}"#.to_owned()),
            BqValue::String(r#"{"k":"a","i":1,"val":10}"#.to_owned()),
            BqValue::Int64(2),
            BqValue::Int64(20),
            BqValue::Int64(1),
            BqValue::Int64(10),
        ]
    );
}

#[test]
fn issue_21_full_outer_join_unmatched_all_null_struct_vs_if_guard() {
    let eng = Engine::new();
    let unmatched = eng
        .query(
            "WITH t AS (
               SELECT 'a' AS k, 1 AS i UNION ALL
               SELECT 'a', 3
             ),
             paired AS (
               SELECT
                 c AS raw_curr,
                 p AS raw_prev,
                 IF(c.k IS NULL, NULL, c) AS curr,
                 IF(p.k IS NULL, NULL, p) AS prev
               FROM (SELECT * FROM t WHERE i = 1) c
               FULL OUTER JOIN (SELECT * FROM t WHERE i = 3) p
                 ON p.k = c.k AND p.i + 1 = c.i
             )
             SELECT
               TO_JSON_STRING(raw_curr) AS rc,
               TO_JSON_STRING(raw_prev) AS rp,
               TO_JSON_STRING(curr) AS jc,
               TO_JSON_STRING(prev) AS jp,
               curr.i AS ci,
               prev.i AS pi
             FROM paired
             ORDER BY COALESCE(curr.i, 99), COALESCE(prev.i, 99)",
        )
        .unwrap();
    assert_eq!(unmatched.rows.len(), 2);
    assert_eq!(
        unmatched.rows[0],
        vec![
            BqValue::String(r#"{"k":"a","i":1}"#.to_owned()),
            BqValue::String(r#"{"k":null,"i":null}"#.to_owned()),
            BqValue::String(r#"{"k":"a","i":1}"#.to_owned()),
            BqValue::String("null".to_owned()),
            BqValue::Int64(1),
            BqValue::Null,
        ]
    );
    assert_eq!(
        unmatched.rows[1],
        vec![
            BqValue::String(r#"{"k":null,"i":null}"#.to_owned()),
            BqValue::String(r#"{"k":"a","i":3}"#.to_owned()),
            BqValue::String("null".to_owned()),
            BqValue::String(r#"{"k":"a","i":3}"#.to_owned()),
            BqValue::Null,
            BqValue::Int64(3),
        ]
    );
}

#[test]
fn issue_21_precedence_correlated_using_agg_and_zero_row_schema() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA s;
         CREATE TABLE s.items (id INT64, name STRING);
         INSERT INTO s.items VALUES (1, 'alpha'), (2, 'beta');",
    )
    .unwrap();

    // 1. Bare table name (without explicit AS) and column-name precedence
    let res = eng
        .query("SELECT TO_JSON_STRING(items) AS j FROM s.items ORDER BY id")
        .unwrap();
    assert_eq!(
        res.rows,
        vec![
            vec![BqValue::String(r#"{"id":1,"name":"alpha"}"#.to_owned())],
            vec![BqValue::String(r#"{"id":2,"name":"beta"}"#.to_owned())],
        ]
    );

    let prec = eng
        .query("WITH a AS (SELECT 42 AS a, 'z' AS b) SELECT a FROM a")
        .unwrap();
    assert_eq!(prec.rows, vec![vec![BqValue::Int64(42)]]);

    // 2. Correlated subquery referencing outer table alias as a value
    let corr = eng
        .query("SELECT (SELECT TO_JSON_STRING(i)) AS j FROM s.items AS i ORDER BY i.id")
        .unwrap();
    assert_eq!(
        corr.rows,
        vec![
            vec![BqValue::String(r#"{"id":1,"name":"alpha"}"#.to_owned())],
            vec![BqValue::String(r#"{"id":2,"name":"beta"}"#.to_owned())],
        ]
    );

    // 3. JOIN ... USING preserves each side's full row struct
    let using_res = eng
        .query(
            "WITH a AS (SELECT 1 AS id, 'x' AS v),
                  b AS (SELECT 1 AS id, 'y' AS w)
             SELECT TO_JSON_STRING(a) AS ja, TO_JSON_STRING(b) AS jb
             FROM a JOIN b USING (id)",
        )
        .unwrap();
    assert_eq!(
        using_res.rows,
        vec![vec![
            BqValue::String(r#"{"id":1,"v":"x"}"#.to_owned()),
            BqValue::String(r#"{"id":1,"w":"y"}"#.to_owned()),
        ]]
    );

    // 4. Aggregate over table alias (ARRAY_AGG) + 0-row static schema inference
    let agg = eng
        .query("SELECT ARRAY_LENGTH(ARRAY_AGG(i ORDER BY i.id)) AS n FROM s.items AS i")
        .unwrap();
    assert_eq!(agg.rows, vec![vec![BqValue::Int64(2)]]);

    let zero = eng
        .query(
            "SELECT i AS raw_i, IF(i.id IS NULL, NULL, i) AS maybe_i
             FROM s.items AS i
             WHERE FALSE",
        )
        .unwrap();
    assert_eq!(zero.rows.len(), 0);
    let expected_struct = BqType::Struct(vec![
        StructField::named("id", BqType::Int64),
        StructField::named("name", BqType::String),
    ]);
    assert_eq!(zero.columns[0].ty, expected_struct);
    assert_eq!(zero.columns[1].ty, expected_struct);
}
