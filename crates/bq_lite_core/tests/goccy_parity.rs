use bq_lite_core::catalog::{ColumnSchema, TableKind, TableMeta, TableRef};
use bq_lite_core::io::csv_json::{load_csv_with_options, load_ndjson, CsvLoadOptions};
use bq_lite_core::{BqErrorKind, BqStructFieldValue, BqType, BqValue, Engine, StructField};
use chrono::Utc;

#[test]
fn bq_timestamp_utc_and_microsecond_limit() {
    let eng = Engine::new();
    let qr = eng
        .query("SELECT CAST(TIMESTAMP '2024-06-15 12:30:45.123456 UTC' AS STRING) AS s")
        .unwrap();
    assert_eq!(
        qr.rows[0][0],
        BqValue::String("2024-06-15 12:30:45.123456 UTC".to_owned())
    );

    let err = eng
        .query("SELECT CAST('2024-06-15 12:30:45.1234567 UTC' AS TIMESTAMP)")
        .unwrap_err();
    assert_eq!(err.kind, BqErrorKind::Semantic);

    let safe_qr = eng
        .query("SELECT SAFE_CAST('2024-06-15 12:30:45.1234567 UTC' AS TIMESTAMP) AS t")
        .unwrap();
    assert_eq!(safe_qr.rows[0][0], BqValue::Null);
}

#[test]
fn bq_distinct_and_group_by_reject_array_and_json() {
    let eng = Engine::new();
    let err_distinct = eng
        .query("SELECT DISTINCT [1, 2] AS arr FROM UNNEST([1, 2]) AS x")
        .unwrap_err();
    assert_eq!(err_distinct.kind, BqErrorKind::Query);

    let err_json_distinct = eng
        .query("SELECT DISTINCT PARSE_JSON('{\"a\":1}') AS j")
        .unwrap_err();
    assert_eq!(err_json_distinct.kind, BqErrorKind::Query);

    let err_group = eng
        .query("SELECT arr, COUNT(*) FROM (SELECT [1, 2] AS arr) GROUP BY arr")
        .unwrap_err();
    assert_eq!(err_group.kind, BqErrorKind::Query);
}

#[test]
fn bq_repeated_record_field_access_is_exact() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT r.id AS id, r.label AS label \
             FROM UNNEST([STRUCT(10 AS id, 'alpha' AS label), STRUCT(20 AS id, 'beta' AS label)]) AS r \
             ORDER BY r.id",
        )
        .unwrap();
    assert_eq!(qr.rows.len(), 2);
    assert_eq!(qr.rows[0][0], BqValue::Int64(10));
    assert_eq!(qr.rows[0][1], BqValue::String("alpha".to_owned()));
    assert_eq!(qr.rows[1][0], BqValue::Int64(20));
    assert_eq!(qr.rows[1][1], BqValue::String("beta".to_owned()));
}

#[test]
fn bq_ndjson_repeated_record_and_csv_null_marker() {
    let cols = vec![
        ColumnSchema::new("id", BqType::Int64),
        ColumnSchema::new("tags", BqType::array(BqType::String).unwrap()),
        ColumnSchema::new(
            "items",
            BqType::array(BqType::structure(vec![
                StructField::named("k", BqType::String),
                StructField::named("v", BqType::Int64),
            ]))
            .unwrap(),
        ),
        ColumnSchema::new("raw", BqType::Bytes),
    ];
    let ndjson = concat!(
        r#"{"id":1,"tags":["a","b"],"items":[{"k":"x","v":9}],"raw":"aGk="}"#,
        "\n",
        r#"{"id":2,"tags":[],"raw":"Ynll"}"#,
        "\n",
    );
    let rows = load_ndjson(ndjson.as_bytes(), &cols).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0][2],
        BqValue::Array(vec![BqValue::Struct(vec![
            BqStructFieldValue {
                name: Some("k".to_owned()),
                value: BqValue::String("x".to_owned()),
            },
            BqStructFieldValue {
                name: Some("v".to_owned()),
                value: BqValue::Int64(9),
            },
        ])])
    );
    assert_eq!(rows[0][3], BqValue::Bytes(b"hi".to_vec()));
    assert_eq!(rows[1][2], BqValue::Array(Vec::new()));

    let csv_cols = vec![
        ColumnSchema::new("a", BqType::Int64),
        ColumnSchema::new("b", BqType::String),
    ];
    let csv_data = "a|b\n1|\\N\n2|ok\n";
    let csv_rows = load_csv_with_options(
        csv_data.as_bytes(),
        &csv_cols,
        &CsvLoadOptions {
            skip_leading_rows: 1,
            field_delimiter: b'|',
            quote: b'"',
            null_marker: Some("\\N".to_owned()),
            ..CsvLoadOptions::default()
        },
    )
    .unwrap();
    assert_eq!(csv_rows[0][1], BqValue::Null);
    assert_eq!(csv_rows[1][1], BqValue::String("ok".to_owned()));
}

#[test]
fn bq_export_data_json_serializes_array_struct_as_json_array() {
    let dir = tempfile::tempdir().unwrap();
    let uri = dir.path().join("export_*.json");
    let mut eng = Engine::new();
    let sql = format!(
        "EXPORT DATA OPTIONS(uri='{}', format='JSON') AS \
         SELECT 1 AS id, [STRUCT('a' AS k, 10 AS v), STRUCT('b' AS k, 20 AS v)] AS arr",
        uri.display()
    );
    eng.exec(&sql).unwrap();

    let actual_file = dir.path().join("export_000000000000.json");
    let content = std::fs::read_to_string(actual_file).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
    assert_eq!(parsed["id"], 1);
    assert!(parsed["arr"].is_array());
    assert_eq!(parsed["arr"][0]["k"], "a");
    assert_eq!(parsed["arr"][1]["v"], 20);
}

#[test]
fn bq_reserved_names_drop_memory_alter_qualify_and_joins() {
    let mut eng = Engine::new();
    for ds in ["ai", "ml", "obj", "mysql"] {
        eng.exec(&format!("CREATE SCHEMA {ds}")).unwrap();
        eng.exec(&format!("CREATE TABLE {ds}.t (id INT64)"))
            .unwrap();
        eng.exec(&format!("INSERT INTO {ds}.t VALUES (1)")).unwrap();
    }
    assert_eq!(
        eng.query("SELECT id FROM ml.t").unwrap().rows[0][0],
        BqValue::Int64(1)
    );

    // #481 ALTER TABLE
    eng.exec("ALTER TABLE ai.t ADD COLUMN note STRING").unwrap();
    eng.exec("INSERT INTO ai.t (id, note) VALUES (2, 'hi')")
        .unwrap();
    assert_eq!(
        eng.query("SELECT COUNT(*) FROM ai.t").unwrap().rows[0][0],
        BqValue::Int64(2)
    );

    // #313 Dropping table releases storage rows immediately
    let tref = TableRef::new("local", "ai", "t");
    assert_eq!(eng.storage().read_rows(&tref).len(), 2);
    eng.exec("DROP TABLE ai.t").unwrap();
    assert_eq!(eng.storage().read_rows(&tref).len(), 0);

    // #502 QUALIFY
    let q_res = eng
        .query(
            "SELECT x, ROW_NUMBER() OVER (ORDER BY x DESC) AS rn \
             FROM UNNEST([10, 20, 30]) AS x \
             QUALIFY rn <= 2 ORDER BY x",
        )
        .unwrap();
    assert_eq!(q_res.rows.len(), 2);
    assert_eq!(q_res.rows[0][0], BqValue::Int64(20));

    // #436 Implicit inner join vs explicit left join
    eng.create_table(
        &TableRef::new("local", "obj", "l"),
        TableMeta {
            name: "l".to_owned(),
            kind: TableKind::Table,
            columns: vec![ColumnSchema::new("id", BqType::Int64)],
            partition_by: None,
            cluster_by: Vec::new(),
            view_query: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            options: std::collections::BTreeMap::new(),
            description: None,
            friendly_name: None,
            labels: std::collections::BTreeMap::new(),
            view_sql: None,
            expiration_ms: None,
            external_uris: Vec::new(),
            external_format: None,
        },
    )
    .unwrap();
    eng.exec("INSERT INTO obj.l VALUES (1), (2)").unwrap();
    eng.exec("CREATE TABLE obj.r (id INT64)").unwrap();
    eng.exec("INSERT INTO obj.r VALUES (1)").unwrap();

    let inner = eng
        .query("SELECT l.id FROM obj.l, obj.r WHERE l.id = r.id")
        .unwrap();
    let left = eng
        .query("SELECT l.id, r.id FROM obj.l LEFT JOIN obj.r ON l.id = r.id ORDER BY l.id")
        .unwrap();
    assert_eq!(inner.rows.len(), 1);
    assert_eq!(left.rows.len(), 2);
    assert_eq!(left.rows[1][1], BqValue::Null);
}
