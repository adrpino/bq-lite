use bq_lite_core::{BqErrorKind, BqValue, Engine};

#[test]
fn scripting_variables_loops_and_exception_handling() {
    let mut e = Engine::new();
    let res = e
        .exec_script(
            r"
            DECLARE total INT64 DEFAULT 0;
            DECLARE a, b INT64;
            SET (a, b) = (SELECT 10, 20);
            FOR rec IN (SELECT x FROM UNNEST([1, 2, 3, 4]) AS x) DO
                IF rec.x = 3 THEN
                    CONTINUE;
                END IF;
                SET total = total + rec.x;
            END FOR;
            ASSERT total = 7 AS 'expected 1+2+4=7';
            SELECT total + a + b AS final_sum;
            ",
        )
        .unwrap();
    assert_eq!(res.rows, vec![vec![BqValue::Int64(37)]]);

    let caught = e
        .exec_script(
            r"
            DECLARE msg STRING;
            BEGIN
                RAISE USING MESSAGE = 'boom!';
            EXCEPTION WHEN ERROR THEN
                SET msg = @@error.message;
            END;
            SELECT msg;
            ",
        )
        .unwrap();
    assert_eq!(caught.rows, vec![vec![BqValue::String("boom!".to_owned())]]);
}

#[test]
fn execute_immediate_procedures_and_transactions() {
    let mut e = Engine::new();
    e.exec("CREATE SCHEMA ds").unwrap();
    e.exec("CREATE TABLE ds.log (v INT64)").unwrap();

    e.exec_script(
        r"
        CREATE PROCEDURE ds.double_sum(IN x INT64, OUT res INT64)
        BEGIN
            SET res = x * 2;
        END;
        DECLARE ans INT64 DEFAULT 0;
        DECLARE dyn INT64 DEFAULT 0;
        CALL ds.double_sum(21, ans);
        EXECUTE IMMEDIATE 'SELECT @k + ?' INTO dyn USING 8 AS k, 2;
        INSERT INTO ds.log VALUES (ans), (dyn);

        BEGIN TRANSACTION;
            INSERT INTO ds.log VALUES (999);
        ROLLBACK;
        ",
    )
    .unwrap();

    let rows = e.query("SELECT v FROM ds.log ORDER BY v").unwrap();
    assert_eq!(
        rows.rows,
        vec![vec![BqValue::Int64(10)], vec![BqValue::Int64(42)]]
    );

    let assert_err = e.exec("ASSERT 1 = 2 AS 'math broken'").unwrap_err();
    assert_eq!(assert_err.kind, BqErrorKind::Semantic);
}

#[test]
fn information_schema_exposes_schemata_tables_columns_views_and_routines() {
    let mut e = Engine::new();
    e.exec("CREATE SCHEMA analytics OPTIONS(location='EU')")
        .unwrap();
    e.exec("CREATE TABLE analytics.users (id INT64 NOT NULL, email STRING)")
        .unwrap();
    e.exec("CREATE VIEW analytics.v_users AS SELECT id FROM analytics.users")
        .unwrap();
    e.exec("CREATE FUNCTION analytics.inc(x INT64) RETURNS INT64 AS (x + 1)")
        .unwrap();

    let schemata = e
        .query("SELECT schema_name, location FROM region-eu.INFORMATION_SCHEMA.SCHEMATA")
        .unwrap();
    assert_eq!(
        schemata.rows,
        vec![vec![
            BqValue::String("analytics".to_owned()),
            BqValue::String("EU".to_owned()),
        ]]
    );

    let tables = e
        .query(
            "SELECT table_name, table_type FROM analytics.INFORMATION_SCHEMA.TABLES ORDER BY table_name",
        )
        .unwrap();
    assert_eq!(
        tables.rows,
        vec![
            vec![
                BqValue::String("users".to_owned()),
                BqValue::String("BASE TABLE".to_owned()),
            ],
            vec![
                BqValue::String("v_users".to_owned()),
                BqValue::String("VIEW".to_owned()),
            ],
        ]
    );

    let cols = e
        .query(
            "SELECT column_name, is_nullable, data_type FROM analytics.INFORMATION_SCHEMA.COLUMNS WHERE table_name = 'users' ORDER BY ordinal_position",
        )
        .unwrap();
    assert_eq!(
        cols.rows,
        vec![
            vec![
                BqValue::String("id".to_owned()),
                BqValue::String("NO".to_owned()),
                BqValue::String("INT64".to_owned()),
            ],
            vec![
                BqValue::String("email".to_owned()),
                BqValue::String("YES".to_owned()),
                BqValue::String("STRING".to_owned()),
            ],
        ]
    );

    let routines = e
        .query("SELECT routine_name, routine_type FROM analytics.INFORMATION_SCHEMA.ROUTINES")
        .unwrap();
    assert_eq!(
        routines.rows,
        vec![vec![
            BqValue::String("inc".to_owned()),
            BqValue::String("FUNCTION".to_owned()),
        ]]
    );
}

#[test]
fn script_parenthesized_set_unpacks_select_as_struct_single_and_multi() {
    let mut e = Engine::new();
    let res = e
        .exec_script(
            "DECLARE parts ARRAY<DATE>;
             DECLARE a INT64;
             DECLARE b STRING;
             DECLARE raw STRUCT<x INT64>;
             SET (parts) = (
                 SELECT AS STRUCT ARRAY_AGG(DISTINCT d ORDER BY d)
                 FROM UNNEST([DATE '2026-01-01', DATE '2026-01-02', DATE '2026-01-01']) AS d
             );
             SET (a, b) = (SELECT AS STRUCT 7, 'ok');
             SET raw = STRUCT(99 AS x);
             SELECT
               ARRAY_LENGTH(parts) AS n,
               DATE '2026-01-02' IN UNNEST(parts) AS has_d2,
               a,
               b,
               raw.x AS rx;",
        )
        .unwrap();
    assert_eq!(
        res.rows,
        vec![vec![
            BqValue::Int64(2),
            BqValue::Bool(true),
            BqValue::Int64(7),
            BqValue::String("ok".to_owned()),
            BqValue::Int64(99),
        ]]
    );
}
