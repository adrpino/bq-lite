use bq_lite_core::error::BqErrorKind;
use bq_lite_core::types::BqValue;
use bq_lite_core::Engine;

#[test]
fn issue_19_drop_view_on_table_errors_and_keeps_table() {
    let mut eng = Engine::new();
    eng.exec("CREATE SCHEMA ds").unwrap();
    eng.exec("CREATE TABLE ds.t (x INT64)").unwrap();
    eng.exec("INSERT INTO ds.t VALUES (1), (2)").unwrap();

    // DROP VIEW [IF EXISTS] on a TABLE must error and keep the table intact.
    let err_if = eng.exec("DROP VIEW IF EXISTS ds.t").unwrap_err();
    assert_eq!(err_if.kind, BqErrorKind::Query);
    let err = eng.exec("DROP VIEW ds.t").unwrap_err();
    assert_eq!(err.kind, BqErrorKind::Query);

    // Table and its 2 rows must remain intact.
    let qr = eng.exec("SELECT COUNT(*) FROM ds.t").unwrap();
    assert_eq!(qr.rows, vec![vec![BqValue::Int64(2)]]);

    // Exact migration-guard script from issue #19.
    eng.exec_script(
        "BEGIN
           EXECUTE IMMEDIATE 'DROP VIEW IF EXISTS ds.t';
         EXCEPTION WHEN ERROR THEN
           SELECT 1;
         END;
         DELETE FROM ds.t WHERE x = 1;
         MERGE ds.t AS target
         USING (SELECT 2 AS x UNION ALL SELECT 3 AS x) AS src
         ON target.x = src.x
         WHEN NOT MATCHED THEN INSERT (x) VALUES (src.x);",
    )
    .unwrap();

    let after = eng.exec("SELECT x FROM ds.t ORDER BY x").unwrap();
    assert_eq!(
        after.rows,
        vec![vec![BqValue::Int64(2)], vec![BqValue::Int64(3)]]
    );
}

#[test]
fn issue_19_drop_table_view_mv_snapshot_external_kind_enforcement() {
    let mut eng = Engine::new();
    eng.exec("CREATE SCHEMA ds").unwrap();
    eng.exec("CREATE TABLE ds.t (x INT64)").unwrap();
    eng.exec("INSERT INTO ds.t VALUES (10)").unwrap();
    eng.exec("CREATE VIEW ds.v AS SELECT x FROM ds.t").unwrap();
    eng.exec("CREATE MATERIALIZED VIEW ds.mv AS SELECT x FROM ds.t")
        .unwrap();
    eng.exec("CREATE SNAPSHOT TABLE ds.snap CLONE ds.t")
        .unwrap();
    eng.exec(
        "CREATE EXTERNAL TABLE ds.ext (a INT64)
         OPTIONS (format = 'GOOGLE_SHEETS', uris = ['https://docs.google.com/spreadsheets/d/1'])",
    )
    .unwrap();

    // Cross-kind drops must fail even with IF EXISTS and leave objects intact.
    for bad in [
        "DROP TABLE IF EXISTS ds.v",
        "DROP TABLE ds.v",
        "DROP TABLE IF EXISTS ds.mv",
        "DROP TABLE IF EXISTS ds.snap",
        "DROP VIEW IF EXISTS ds.mv",
        "DROP VIEW IF EXISTS ds.snap",
        "DROP VIEW IF EXISTS ds.ext",
        "DROP MATERIALIZED VIEW IF EXISTS ds.t",
        "DROP MATERIALIZED VIEW IF EXISTS ds.v",
        "DROP SNAPSHOT TABLE IF EXISTS ds.t",
        "DROP SNAPSHOT TABLE IF EXISTS ds.v",
        "DROP EXTERNAL TABLE IF EXISTS ds.t",
        "DROP EXTERNAL TABLE IF EXISTS ds.v",
    ] {
        let err = eng.exec(bad).unwrap_err();
        assert_eq!(err.kind, BqErrorKind::Query, "expected error for: {bad}");
    }

    // All objects still queryable.
    assert_eq!(
        eng.exec("SELECT x FROM ds.v").unwrap().rows,
        vec![vec![BqValue::Int64(10)]]
    );
    assert_eq!(
        eng.exec("SELECT x FROM ds.mv").unwrap().rows,
        vec![vec![BqValue::Int64(10)]]
    );
    assert_eq!(
        eng.exec("SELECT x FROM ds.snap").unwrap().rows,
        vec![vec![BqValue::Int64(10)]]
    );

    // Nonexistent targets with IF EXISTS succeed cleanly.
    eng.exec("DROP VIEW IF EXISTS ds.does_not_exist").unwrap();
    eng.exec("DROP TABLE IF EXISTS no_such_ds.does_not_exist")
        .unwrap();

    // Matching-kind drops succeed.
    eng.exec("DROP VIEW IF EXISTS ds.v").unwrap();
    eng.exec("DROP MATERIALIZED VIEW IF EXISTS ds.mv").unwrap();
    eng.exec("DROP SNAPSHOT TABLE IF EXISTS ds.snap").unwrap();
    eng.exec("DROP EXTERNAL TABLE IF EXISTS ds.ext").unwrap();
    eng.exec("DROP TABLE IF EXISTS ds.t").unwrap();
}

#[test]
fn issue_19_drop_routine_kind_enforcement() {
    let mut eng = Engine::new();
    eng.exec("CREATE SCHEMA ds").unwrap();
    eng.exec("CREATE FUNCTION ds.fn_scalar(x INT64) RETURNS INT64 AS (x + 1)")
        .unwrap();
    eng.exec("CREATE TABLE FUNCTION ds.fn_tvf() AS (SELECT 1 AS a)")
        .unwrap();
    eng.exec("CREATE PROCEDURE ds.proc_noop() BEGIN SELECT 1; END")
        .unwrap();

    for bad in [
        "DROP FUNCTION IF EXISTS ds.fn_tvf",
        "DROP FUNCTION IF EXISTS ds.proc_noop",
        "DROP TABLE FUNCTION IF EXISTS ds.fn_scalar",
        "DROP TABLE FUNCTION IF EXISTS ds.proc_noop",
        "DROP PROCEDURE IF EXISTS ds.fn_scalar",
        "DROP PROCEDURE IF EXISTS ds.fn_tvf",
    ] {
        let err = eng.exec(bad).unwrap_err();
        assert_eq!(err.kind, BqErrorKind::Query, "expected error for: {bad}");
    }

    eng.exec("DROP FUNCTION IF EXISTS ds.fn_scalar").unwrap();
    eng.exec("DROP TABLE FUNCTION IF EXISTS ds.fn_tvf").unwrap();
    eng.exec("DROP PROCEDURE IF EXISTS ds.proc_noop").unwrap();
}
