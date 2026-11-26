use bq_lite_core::{BqErrorKind, BqType, BqValue, Engine};

#[test]
fn array_transform_and_array_filter_lambdas() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT \
             ARRAY_TRANSFORM([1, 2, 3], x -> x * 10) AS scaled, \
             ARRAY_TRANSFORM([10, 20, 30], (x, i) -> x + i) AS indexed, \
             ARRAY_FILTER([1, 2, 3, 4, 5], x -> MOD(x, 2) = 0) AS evens",
        )
        .unwrap();
    assert_eq!(
        qr.rows[0][0],
        BqValue::Array(vec![
            BqValue::Int64(10),
            BqValue::Int64(20),
            BqValue::Int64(30),
        ])
    );
    assert_eq!(
        qr.rows[0][1],
        BqValue::Array(vec![
            BqValue::Int64(10),
            BqValue::Int64(21),
            BqValue::Int64(32),
        ])
    );
    assert_eq!(
        qr.rows[0][2],
        BqValue::Array(vec![BqValue::Int64(2), BqValue::Int64(4)])
    );
}

#[test]
fn range_literals_and_range_functions() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT \
             RANGE_START(RANGE<DATE> '[2024-01-01, 2024-02-01)') AS r_start, \
             RANGE_END(RANGE<DATE> '[2024-01-01, 2024-02-01)') AS r_end, \
             RANGE_CONTAINS(RANGE(DATE '2024-01-01', DATE '2024-02-01'), DATE '2024-01-15') AS contains_pt, \
             RANGE_CONTAINS(RANGE(DATE '2024-01-01', DATE '2024-02-01'), DATE '2024-02-01') AS excl_end, \
             RANGE_OVERLAPS(RANGE(DATE '2024-01-01', DATE '2024-01-15'), RANGE(DATE '2024-01-10', DATE '2024-02-01')) AS ov, \
             RANGE_START(RANGE_INTERSECT(RANGE(DATE '2024-01-01', DATE '2024-01-20'), RANGE(DATE '2024-01-10', DATE '2024-02-01'))) AS inter_start, \
             SAFE.RANGE(DATE '2024-02-01', DATE '2024-01-01') AS bad_range",
        )
        .unwrap();
    assert_eq!(qr.columns[0].ty, BqType::Date);
    assert_eq!(qr.rows[0][2], BqValue::Bool(true));
    assert_eq!(qr.rows[0][3], BqValue::Bool(false));
    assert_eq!(qr.rows[0][4], BqValue::Bool(true));
    assert_eq!(qr.rows[0][6], BqValue::Null);
}

#[test]
fn net_and_geography_functions() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT \
             NET.IP_TO_STRING(NET.IP_FROM_STRING('127.0.0.1')) AS ipv4, \
             NET.SAFE_IP_FROM_STRING('not-an-ip') AS bad_ip, \
             NET.HOST('https://api.example.co.uk:8443/v1') AS host, \
             NET.REG_DOMAIN('https://api.example.com/v1') AS reg, \
             NET.PUBLIC_SUFFIX('https://api.example.com/v1') AS sfx, \
             ST_ASTEXT(ST_GEOGPOINT(0, 0)) AS pt, \
             ST_DISTANCE(ST_GEOGPOINT(0, 0), ST_GEOGPOINT(0, 1)) > 110000 AS dist_ok, \
             ST_CONTAINS(ST_GEOGFROMTEXT('POINT(1 2)'), ST_GEOGPOINT(1, 2)) AS same_pt, \
             ST_WITHIN(ST_GEOGPOINT(1, 2), ST_GEOGPOINT(1, 2)) AS within_ok",
        )
        .unwrap();
    assert_eq!(qr.rows[0][0], BqValue::String("127.0.0.1".to_owned()));
    assert_eq!(qr.rows[0][1], BqValue::Null);
    assert_eq!(
        qr.rows[0][2],
        BqValue::String("api.example.co.uk".to_owned())
    );
    assert_eq!(qr.rows[0][3], BqValue::String("example.com".to_owned()));
    assert_eq!(qr.rows[0][4], BqValue::String("com".to_owned()));
    assert_eq!(qr.rows[0][5], BqValue::String("POINT(0 0)".to_owned()));
    assert_eq!(qr.rows[0][6], BqValue::Bool(true));
    assert_eq!(qr.rows[0][7], BqValue::Bool(true));
    assert_eq!(qr.rows[0][8], BqValue::Bool(true));

    let err = eng.query("SELECT NET.IP_FROM_STRING('bad')").unwrap_err();
    assert_eq!(err.kind, BqErrorKind::Semantic);
}

#[test]
fn partition_pseudo_cols_rls_and_extra_information_schema() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA pds OPTIONS(location='US'); \
         CREATE TABLE pds.events (id INT64, region STRING, meta STRUCT<code STRING>) \
         PARTITION BY _PARTITIONDATE OPTIONS(description='events table'); \
         INSERT INTO pds.events VALUES (1, 'us', STRUCT('a')), (2, 'eu', STRUCT('b')); \
         CREATE FUNCTION pds.double_it(x INT64) RETURNS INT64 AS (x * 2);",
    )
    .unwrap();

    // SELECT * omits pseudo-columns; explicit projection exposes _PARTITIONDATE & _PARTITIONTIME
    let star_qr = eng.query("SELECT * FROM pds.events ORDER BY id").unwrap();
    assert_eq!(star_qr.columns.len(), 3);
    let pseudo_qr = eng
        .query("SELECT _PARTITIONDATE, _PARTITIONTIME FROM pds.events LIMIT 1")
        .unwrap();
    assert_eq!(pseudo_qr.columns[0].ty, BqType::Date);
    assert_eq!(pseudo_qr.columns[1].ty, BqType::Timestamp);

    // Row-Level Security enforcement
    eng.exec("CREATE ROW ACCESS POLICY us_only ON pds.events GRANT TO ('allUsers') FILTER USING (region = 'us')")
        .unwrap();
    let rls_qr = eng.query("SELECT id FROM pds.events").unwrap();
    assert_eq!(rls_qr.rows, vec![vec![BqValue::Int64(1)]]);

    // Extended INFORMATION_SCHEMA views
    assert_ne!(
        eng.query("SELECT option_name FROM pds.INFORMATION_SCHEMA.SCHEMATA_OPTIONS")
            .unwrap()
            .rows
            .len(),
        0
    );
    assert_ne!(
        eng.query("SELECT option_name FROM pds.INFORMATION_SCHEMA.TABLE_OPTIONS")
            .unwrap()
            .rows
            .len(),
        0
    );
    assert!(
        eng.query("SELECT field_path FROM pds.INFORMATION_SCHEMA.COLUMN_FIELD_PATHS")
            .unwrap()
            .rows
            .len()
            >= 4
    );
    assert_ne!(
        eng.query("SELECT parameter_name FROM pds.INFORMATION_SCHEMA.PARAMETERS")
            .unwrap()
            .rows
            .len(),
        0
    );
    assert_ne!(
        eng.query("SELECT partition_id FROM pds.INFORMATION_SCHEMA.PARTITIONS")
            .unwrap()
            .rows
            .len(),
        0
    );
    assert_eq!(
        eng.query("SELECT * FROM pds.INFORMATION_SCHEMA.JOBS")
            .unwrap()
            .rows
            .len(),
        0
    );
}
