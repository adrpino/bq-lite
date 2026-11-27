use bq_lite_core::{BqType, BqValue, Engine};

#[test]
fn issue_18_join_using_right_and_left_qualified_columns() {
    let eng = Engine::new();

    // Exact issue #18 repro (INNER JOIN)
    let qr = eng
        .query(
            "WITH a AS (SELECT 'x' AS k, 1 AS va),
                  b AS (SELECT 'x' AS k, 'n' AS vb)
             SELECT b.vb, b.k, a.k AS ak
             FROM a JOIN b USING (k)",
        )
        .unwrap();
    assert_eq!(
        qr.rows,
        vec![vec![
            BqValue::String("n".to_owned()),
            BqValue::String("x".to_owned()),
            BqValue::String("x".to_owned()),
        ]]
    );

    // Table aliases t1, t2 with LEFT JOIN: b.k must be NULL on unmatched rows, not merged k
    let left_qr = eng
        .query(
            "WITH a AS (SELECT 'x' AS k, 1 AS va UNION ALL SELECT 'y', 2),
                  b AS (SELECT 'x' AS k, 'n' AS vb)
             SELECT k, t1.k AS lk, t2.k AS rk, t2.vb
             FROM a t1 LEFT JOIN b t2 USING (k)
             ORDER BY k",
        )
        .unwrap();
    assert_eq!(
        left_qr.rows,
        vec![
            vec![
                BqValue::String("x".to_owned()),
                BqValue::String("x".to_owned()),
                BqValue::String("x".to_owned()),
                BqValue::String("n".to_owned()),
            ],
            vec![
                BqValue::String("y".to_owned()),
                BqValue::String("y".to_owned()),
                BqValue::Null,
                BqValue::Null,
            ],
        ]
    );

    // FULL OUTER JOIN: a.k is NULL on right-only row, b.k is NULL on left-only row, k is COALESCE
    let full_qr = eng
        .query(
            "WITH a AS (SELECT 'x' AS k UNION ALL SELECT 'y'),
                  b AS (SELECT 'x' AS k UNION ALL SELECT 'z')
             SELECT k, a.k AS ak, b.k AS bk
             FROM a FULL OUTER JOIN b USING (k)
             ORDER BY k",
        )
        .unwrap();
    assert_eq!(
        full_qr.rows,
        vec![
            vec![
                BqValue::String("x".to_owned()),
                BqValue::String("x".to_owned()),
                BqValue::String("x".to_owned()),
            ],
            vec![
                BqValue::String("y".to_owned()),
                BqValue::String("y".to_owned()),
                BqValue::Null,
            ],
            vec![
                BqValue::String("z".to_owned()),
                BqValue::Null,
                BqValue::String("z".to_owned()),
            ],
        ]
    );
}

#[test]
fn issue_18_join_using_group_by_wildcards_and_empty_schema() {
    let eng = Engine::new();

    // Typical pattern from issue #18: LEFT JOIN USING + GROUP BY partner_id, t2.name
    let grp_qr = eng
        .query(
            "WITH uploads AS (
               SELECT 1 AS partner_id UNION ALL SELECT 1 UNION ALL SELECT 2
             ),
             partners AS (
               SELECT 1 AS partner_id, 'Acme' AS name
             )
             SELECT t2.partner_id, t2.name, COUNT(*) AS cnt
             FROM uploads t1
             LEFT JOIN partners t2 USING (partner_id)
             GROUP BY partner_id, t2.name
             ORDER BY partner_id",
        )
        .unwrap();
    assert_eq!(
        grp_qr.rows,
        vec![
            vec![BqValue::Null, BqValue::Null, BqValue::Int64(1)],
            vec![
                BqValue::Int64(1),
                BqValue::String("Acme".to_owned()),
                BqValue::Int64(2),
            ],
        ]
    );

    // SELECT * emits k once; SELECT b.* includes b.k and b.vb; 0-row schema preserves b.k type
    let star_qr = eng
        .query(
            "WITH a AS (SELECT 'x' AS k, 1 AS va),
                  b AS (SELECT 'x' AS k, 'n' AS vb)
             SELECT * FROM a JOIN b USING (k)",
        )
        .unwrap();
    assert_eq!(star_qr.columns.len(), 3);

    let bstar_qr = eng
        .query(
            "WITH a AS (SELECT 'x' AS k, 1 AS va),
                  b AS (SELECT 'x' AS k, 'n' AS vb)
             SELECT b.* FROM a JOIN b USING (k)",
        )
        .unwrap();
    assert_eq!(bstar_qr.columns.len(), 2);
    assert_eq!(bstar_qr.columns[0].name, "k");
    assert_eq!(bstar_qr.columns[1].name, "vb");

    let empty_qr = eng
        .query(
            "WITH a AS (SELECT CAST(NULL AS INT64) AS k WHERE FALSE),
                  b AS (SELECT CAST(NULL AS INT64) AS k, 'n' AS vb WHERE FALSE)
             SELECT b.k, b.vb FROM a LEFT JOIN b USING (k)",
        )
        .unwrap();
    assert_eq!(empty_qr.rows.len(), 0);
    assert_eq!(empty_qr.columns[0].ty, BqType::Int64);
    assert_eq!(empty_qr.columns[1].ty, BqType::String);
}
