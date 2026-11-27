use bq_lite_core::{BqType, BqValue, Engine};

#[test]
fn issue_22_to_json_string_whole_float64_without_trailing_dot_zero() {
    let eng = Engine::new();
    let res = eng
        .query(
            "SELECT TO_JSON_STRING(STRUCT(
               0.0 AS lit0,
               CAST(0 AS FLOAT64) AS cast0,
               100.0 AS lit100,
               ROUND(100.0, 2) AS round100,
               1.5 AS f,
               CAST(1e20 AS FLOAT64) AS big
             )) AS j",
        )
        .unwrap();
    assert_eq!(
        res.rows,
        vec![vec![BqValue::String(
            r#"{"lit0":0,"cast0":0,"lit100":100,"round100":100,"f":1.5,"big":1e+20}"#.to_owned()
        )]]
    );

    // Negative whole floats, -0.0, and TO_JSON consistency
    let neg = eng
        .query(
            "SELECT
               TO_JSON_STRING(-42.0) AS n42,
               TO_JSON_STRING(-0.0) AS n0,
               TO_JSON_STRING(TO_JSON(100.0)) AS tj",
        )
        .unwrap();
    assert_eq!(
        neg.rows,
        vec![vec![
            BqValue::String("-42".to_owned()),
            BqValue::String("0".to_owned()),
            BqValue::String("100".to_owned()),
        ]]
    );
}

#[test]
fn issue_23_coalesce_ifnull_if_case_nullif_common_supertype() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA ds;
         CREATE TABLE ds.t AS
         SELECT
           COALESCE(CAST(NULL AS FLOAT64), 0) AS c,
           IFNULL(CAST(NULL AS FLOAT64), 0)   AS i,
           IF(TRUE, 0, 1.5)                   AS f,
           CASE WHEN TRUE THEN 0 ELSE 1.5 END AS k,
           NULLIF(0, 1.5)                     AS n,
           LEAST(0, 1.5)                      AS l,
           GREATEST(0, 1.5)                   AS g;",
    )
    .unwrap();

    let cols = eng
        .query(
            "SELECT column_name, data_type
             FROM ds.INFORMATION_SCHEMA.COLUMNS
             WHERE table_name = 't'
             ORDER BY ordinal_position",
        )
        .unwrap();
    assert_eq!(
        cols.rows,
        vec![
            vec![
                BqValue::String("c".to_owned()),
                BqValue::String("FLOAT64".to_owned())
            ],
            vec![
                BqValue::String("i".to_owned()),
                BqValue::String("FLOAT64".to_owned())
            ],
            vec![
                BqValue::String("f".to_owned()),
                BqValue::String("FLOAT64".to_owned())
            ],
            vec![
                BqValue::String("k".to_owned()),
                BqValue::String("FLOAT64".to_owned())
            ],
            vec![
                BqValue::String("n".to_owned()),
                BqValue::String("FLOAT64".to_owned())
            ],
            vec![
                BqValue::String("l".to_owned()),
                BqValue::String("FLOAT64".to_owned())
            ],
            vec![
                BqValue::String("g".to_owned()),
                BqValue::String("FLOAT64".to_owned())
            ],
        ]
    );

    // Runtime values are Float64 and short-circuiting is preserved
    let sc = eng
        .query(
            "SELECT
               IF(TRUE, 0, 1 / 0) AS sc_if,
               COALESCE(CAST(NULL AS NUMERIC), 5) AS coal_num,
               COALESCE(CAST(NULL AS FLOAT64), 0) AS coal_f64",
        )
        .unwrap();
    assert_eq!(sc.columns[0].ty, BqType::Float64);
    assert_eq!(sc.columns[1].ty, BqType::Numeric);
    assert_eq!(sc.columns[2].ty, BqType::Float64);
    assert_eq!(sc.rows[0][0], BqValue::Float64(0.0));
    assert_eq!(sc.rows[0][2], BqValue::Float64(0.0));
}

#[test]
fn issue_23_grouped_and_union_supertype_and_json_consistency() {
    let eng = Engine::new();
    let grouped = eng
        .query(
            "WITH data AS (
               SELECT 'a' AS k, CAST(NULL AS FLOAT64) AS x UNION ALL
               SELECT 'b', 2.5
             )
             SELECT
               k,
               COALESCE(SUM(x), 0) AS s,
               TO_JSON_STRING(COALESCE(SUM(x), 0)) AS js
             FROM data
             GROUP BY k
             ORDER BY k",
        )
        .unwrap();
    assert_eq!(grouped.columns[1].ty, BqType::Float64);
    assert_eq!(
        grouped.rows,
        vec![
            vec![
                BqValue::String("a".to_owned()),
                BqValue::Float64(0.0),
                BqValue::String("0".to_owned()),
            ],
            vec![
                BqValue::String("b".to_owned()),
                BqValue::Float64(2.5),
                BqValue::String("2.5".to_owned()),
            ],
        ]
    );

    let union_res = eng
        .query("SELECT 0 AS v UNION ALL SELECT 1.5 AS v ORDER BY v")
        .unwrap();
    assert_eq!(union_res.columns[0].ty, BqType::Float64);
    assert_eq!(
        union_res.rows,
        vec![vec![BqValue::Float64(0.0)], vec![BqValue::Float64(1.5)]]
    );
}
