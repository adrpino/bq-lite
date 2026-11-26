use bq_lite_core::catalog::ColumnSchema;
use bq_lite_core::io::arrow_codec::{arrow_ipc_to_query_result, query_result_to_arrow_ipc};
use bq_lite_core::io::csv_json::{export_csv, export_ndjson, load_csv, load_ndjson};
use bq_lite_core::{BqType, BqValue, Engine};

#[cfg(feature = "persistence")]
#[test]
fn redb_persistence_roundtrips_catalog_views_udfs_and_rows() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bq_lite.redb");

    {
        let mut eng = Engine::open_persistent(&db_path, "local").unwrap();
        eng.exec("CREATE SCHEMA analytics").unwrap();
        eng.exec("CREATE TABLE analytics.users (id INT64, name STRING)")
            .unwrap();
        eng.exec("INSERT INTO analytics.users VALUES (1, 'ada'), (2, 'linus')")
            .unwrap();
        eng.exec("CREATE VIEW analytics.v_users AS SELECT name FROM analytics.users ORDER BY id")
            .unwrap();
        eng.exec("CREATE FUNCTION analytics.add1(x INT64) RETURNS INT64 AS (x + 1)")
            .unwrap();
        eng.save_persistent(&db_path).unwrap();
    }

    let reopened = Engine::open_persistent(&db_path, "local").unwrap();
    let res = reopened
        .query("SELECT analytics.add1(id) AS next_id, name FROM analytics.users ORDER BY id")
        .unwrap();
    assert_eq!(
        res.rows,
        vec![
            vec![BqValue::Int64(2), BqValue::String("ada".to_owned())],
            vec![BqValue::Int64(3), BqValue::String("linus".to_owned())],
        ]
    );
    let view_res = reopened
        .query("SELECT name FROM analytics.v_users")
        .unwrap();
    assert_eq!(view_res.rows.len(), 2);
}

#[test]
fn csv_and_ndjson_load_and_export_roundtrip() {
    let cols = vec![
        ColumnSchema::new("id", BqType::Int64),
        ColumnSchema::new("name", BqType::String),
        ColumnSchema::new("active", BqType::Bool),
    ];
    let csv_bytes = b"id,name,active\n1,alice,true\n2,,false\n";
    let loaded_csv = load_csv(csv_bytes, &cols, 1).unwrap();
    assert_eq!(
        loaded_csv,
        vec![
            vec![
                BqValue::Int64(1),
                BqValue::String("alice".to_owned()),
                BqValue::Bool(true),
            ],
            vec![BqValue::Int64(2), BqValue::Null, BqValue::Bool(false)],
        ]
    );
    let exported_csv = export_csv(&cols, &loaded_csv).unwrap();
    let reloaded_csv = load_csv(&exported_csv, &cols, 1).unwrap();
    assert_eq!(reloaded_csv, loaded_csv);

    let ndjson_bytes = b"{\"id\":10,\"name\":\"bob\",\"active\":true}\n";
    let loaded_ndjson = load_ndjson(ndjson_bytes, &cols).unwrap();
    let exported_ndjson = export_ndjson(&cols, &loaded_ndjson).unwrap();
    let reloaded_ndjson = load_ndjson(&exported_ndjson, &cols).unwrap();
    assert_eq!(reloaded_ndjson, loaded_ndjson);
}

#[test]
fn arrow_ipc_roundtrips_typed_query_result() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT 42 AS n, 2.5 AS f, TRUE AS b, 'hi' AS s, b'xyz' AS raw, DATE '2026-05-01' AS d, [10, 20] AS arr, CAST(NULL AS INT64) AS empty_col",
        )
        .unwrap();

    let ipc = query_result_to_arrow_ipc(&qr).unwrap();
    let decoded = arrow_ipc_to_query_result(&ipc).unwrap();
    assert_eq!(decoded.columns.len(), qr.columns.len());
    assert_eq!(decoded.rows, qr.rows);
}
