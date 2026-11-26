use bq_lite_core::{BqErrorKind, BqValue, Engine};

#[test]
fn ddl_create_alter_ctas_clone_snapshot_and_drop() {
    let mut e = Engine::new();
    e.exec("CREATE SCHEMA ds OPTIONS(location='EU')").unwrap();
    e.exec("CREATE TABLE ds.users (id INT64 NOT NULL, name STRING DEFAULT 'anon')")
        .unwrap();
    e.exec("INSERT INTO ds.users (id) VALUES (1)").unwrap();
    e.exec("INSERT INTO ds.users (id, name) VALUES (2, 'ada')")
        .unwrap();

    // NOT NULL violation
    let err = e
        .exec("INSERT INTO ds.users (id, name) VALUES (NULL, 'bad')")
        .unwrap_err();
    assert_eq!(err.kind, BqErrorKind::InvalidArgument);

    e.exec("ALTER TABLE ds.users ADD COLUMN role STRING DEFAULT 'member'")
        .unwrap();
    let res = e
        .query("SELECT id, name, role FROM ds.users ORDER BY id")
        .unwrap();
    assert_eq!(
        res.rows,
        vec![
            vec![
                BqValue::Int64(1),
                BqValue::String("anon".to_owned()),
                BqValue::String("member".to_owned()),
            ],
            vec![
                BqValue::Int64(2),
                BqValue::String("ada".to_owned()),
                BqValue::String("member".to_owned()),
            ],
        ]
    );

    // CTAS & Snapshot
    e.exec("CREATE TABLE ds.vips AS SELECT id, name FROM ds.users WHERE id = 2")
        .unwrap();
    e.exec("CREATE SNAPSHOT TABLE ds.users_snap CLONE ds.users")
        .unwrap();
    e.exec("DELETE FROM ds.users WHERE id = 1").unwrap();
    assert_eq!(
        e.query("SELECT COUNT(*) FROM ds.users").unwrap().rows[0][0],
        BqValue::Int64(1)
    );
    assert_eq!(
        e.query("SELECT COUNT(*) FROM ds.users_snap").unwrap().rows[0][0],
        BqValue::Int64(2)
    );
    e.exec("DROP SNAPSHOT TABLE ds.users_snap").unwrap();
    e.exec("DROP TABLE ds.vips").unwrap();
}

#[test]
fn dml_insert_deep_struct_update_delete_and_then_return() {
    let mut e = Engine::new();
    e.exec("CREATE SCHEMA ds").unwrap();
    e.exec("CREATE TABLE ds.items (id INT64, meta STRUCT<label STRING, active BOOL>)")
        .unwrap();

    let ins = e
        .exec(
            "INSERT INTO ds.items VALUES (1, STRUCT('a' AS label, TRUE AS active)), (2, STRUCT('b' AS label, TRUE AS active)) THEN RETURN id",
        )
        .unwrap();
    assert_eq!(
        ins.rows,
        vec![vec![BqValue::Int64(1)], vec![BqValue::Int64(2)]]
    );

    let upd = e
        .exec("UPDATE ds.items SET meta.active = FALSE WHERE id = 1 THEN RETURN id, meta.active")
        .unwrap();
    assert_eq!(
        upd.rows,
        vec![vec![BqValue::Int64(1), BqValue::Bool(false)]]
    );

    let del = e
        .exec("DELETE FROM ds.items WHERE NOT meta.active THEN RETURN id")
        .unwrap();
    assert_eq!(del.rows, vec![vec![BqValue::Int64(1)]]);
    assert_eq!(
        e.query("SELECT COUNT(*) FROM ds.items").unwrap().rows[0][0],
        BqValue::Int64(1)
    );

    e.exec("TRUNCATE TABLE ds.items").unwrap();
    assert_eq!(
        e.query("SELECT COUNT(*) FROM ds.items").unwrap().rows[0][0],
        BqValue::Int64(0)
    );
}

#[test]
fn merge_handles_matched_not_matched_and_duplicate_match_error() {
    let mut e = Engine::new();
    e.exec("CREATE SCHEMA ds").unwrap();
    e.exec("CREATE TABLE ds.target (id INT64, val STRING)")
        .unwrap();
    e.exec("CREATE TABLE ds.src (id INT64, val STRING)")
        .unwrap();
    e.exec("INSERT INTO ds.target VALUES (1, 'old1'), (2, 'keep'), (3, 'drop_me')")
        .unwrap();
    e.exec("INSERT INTO ds.src VALUES (1, 'new1'), (4, 'inserted')")
        .unwrap();

    e.exec(
        "MERGE INTO ds.target t USING ds.src s ON t.id = s.id WHEN MATCHED THEN UPDATE SET val = s.val WHEN NOT MATCHED BY TARGET THEN INSERT (id, val) VALUES (s.id, s.val) WHEN NOT MATCHED BY SOURCE AND t.id = 3 THEN DELETE",
    )
    .unwrap();

    let res = e
        .query("SELECT id, val FROM ds.target ORDER BY id")
        .unwrap();
    assert_eq!(
        res.rows,
        vec![
            vec![BqValue::Int64(1), BqValue::String("new1".to_owned())],
            vec![BqValue::Int64(2), BqValue::String("keep".to_owned())],
            vec![BqValue::Int64(4), BqValue::String("inserted".to_owned())],
        ]
    );

    // Duplicate source match must fail
    e.exec("INSERT INTO ds.src VALUES (1, 'dup')").unwrap();
    let err = e
        .exec("MERGE INTO ds.target t USING ds.src s ON t.id = s.id WHEN MATCHED THEN UPDATE SET val = s.val")
        .unwrap_err();
    assert_eq!(err.kind, BqErrorKind::InvalidArgument);
}

#[test]
fn views_sql_udfs_tvfs_temp_tables_and_wildcard_tables() {
    let mut e = Engine::new();
    e.exec("CREATE SCHEMA ds").unwrap();
    e.exec("CREATE TABLE ds.events_202601 (id INT64)").unwrap();
    e.exec("CREATE TABLE ds.events_202602 (id INT64)").unwrap();
    e.exec("INSERT INTO ds.events_202601 VALUES (1)").unwrap();
    e.exec("INSERT INTO ds.events_202602 VALUES (2)").unwrap();

    let wild = e
        .query("SELECT id, _TABLE_SUFFIX FROM `ds.events_*` ORDER BY _TABLE_SUFFIX")
        .unwrap();
    assert_eq!(
        wild.rows,
        vec![
            vec![BqValue::Int64(1), BqValue::String("202601".to_owned())],
            vec![BqValue::Int64(2), BqValue::String("202602".to_owned())],
        ]
    );

    e.exec("CREATE VIEW ds.v_events AS SELECT id FROM `ds.events_*`")
        .unwrap();
    assert_eq!(
        e.query("SELECT COUNT(*) FROM ds.v_events").unwrap().rows[0][0],
        BqValue::Int64(2)
    );

    e.exec("CREATE TEMP FUNCTION double_it(x INT64) RETURNS INT64 AS (x * 2)")
        .unwrap();
    e.exec("CREATE TABLE FUNCTION ds.above(min_id INT64) AS SELECT id FROM ds.v_events WHERE id >= min_id")
        .unwrap();
    let tvf = e
        .query("SELECT double_it(id) AS d FROM ds.above(2)")
        .unwrap();
    assert_eq!(tvf.rows, vec![vec![BqValue::Int64(4)]]);

    e.exec("CREATE TEMP TABLE tmp AS SELECT 42 AS answer")
        .unwrap();
    assert_eq!(
        e.query("SELECT answer FROM tmp").unwrap().rows[0][0],
        BqValue::Int64(42)
    );
}
