use std::sync::Arc;

use bq_lite_core::catalog::{JobInfoProvider, JobMetadataEntry};
use bq_lite_core::{BqValue, Engine};
use chrono::Utc;

#[derive(Debug)]
struct MockJobs;

impl JobInfoProvider for MockJobs {
    fn list_jobs(&self, project_id: &str) -> Vec<JobMetadataEntry> {
        vec![JobMetadataEntry {
            project_id: project_id.to_owned(),
            job_id: "job_live_1".to_owned(),
            job_type: "QUERY".to_owned(),
            state: "DONE".to_owned(),
            query: Some("SELECT 1".to_owned()),
            creation_time: Utc::now(),
            total_bytes_processed: 128,
            error_message: None,
        }]
    }
}

#[test]
fn milestone_j_information_schema_depth_and_live_jobs() {
    let mut eng = Engine::new();
    eng.set_job_provider(Arc::new(MockJobs));
    eng.exec_script(
        "CREATE SCHEMA jds;
         CREATE TABLE jds.orders (id INT64 NOT NULL, region STRING DEFAULT 'us', dt DATE)
           PARTITION BY dt CLUSTER BY region;
         INSERT INTO jds.orders VALUES (1, 'eu', DATE '2025-01-01'), (2, 'us', DATE '2025-01-02');
         CREATE VIEW jds.orders_v AS SELECT id FROM jds.orders;
         CREATE ROW ACCESS POLICY p_eu ON jds.orders GRANT TO ('user:alice@example.com') FILTER USING (region = 'eu');",
    )
    .unwrap();

    let tbls = eng
        .query("SELECT table_name, ddl, creation_time IS NOT NULL FROM jds.INFORMATION_SCHEMA.TABLES ORDER BY table_name")
        .unwrap();
    assert_eq!(tbls.rows.len(), 2);
    assert!(tbls.rows[0][1].to_string().contains("CREATE TABLE"));
    assert_eq!(tbls.rows[0][2], BqValue::Bool(true));

    let views = eng
        .query("SELECT table_name, view_definition FROM jds.INFORMATION_SCHEMA.VIEWS")
        .unwrap();
    assert_eq!(
        views.rows[0][1],
        BqValue::String("SELECT id FROM jds.orders".to_owned())
    );

    let cols = eng
        .query("SELECT column_name, is_partitioning_column, clustering_ordinal_position, column_default FROM jds.INFORMATION_SCHEMA.COLUMNS WHERE table_name = 'orders' ORDER BY ordinal_position")
        .unwrap();
    assert_eq!(cols.rows[1][2], BqValue::Int64(1));
    assert!(cols.rows[1][3].to_string().contains("us"));
    assert_eq!(cols.rows[2][1], BqValue::String("YES".to_owned()));

    let parts = eng
        .query("SELECT total_rows, total_logical_bytes FROM jds.INFORMATION_SCHEMA.PARTITIONS WHERE table_name = 'orders'")
        .unwrap();
    assert_eq!(parts.rows[0][0], BqValue::Int64(2));
    assert_eq!(parts.rows[0][1], BqValue::Int64(128));

    let pols = eng
        .query("SELECT policy_name, ARRAY_LENGTH(grantee_list) FROM jds.INFORMATION_SCHEMA.ROW_ACCESS_POLICIES")
        .unwrap();
    assert_eq!(pols.rows[0][0], BqValue::String("p_eu".to_owned()));
    assert_eq!(pols.rows[0][1], BqValue::Int64(1));

    let jobs = eng
        .query("SELECT job_id, job_type, state FROM region-us.INFORMATION_SCHEMA.JOBS_BY_PROJECT")
        .unwrap();
    assert_eq!(jobs.rows.len(), 1);
    assert_eq!(jobs.rows[0][0], BqValue::String("job_live_1".to_owned()));
}

#[test]
fn milestone_j_rls_grantees_and_column_masking() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA sec;
         CREATE TABLE sec.users (id INT64, email STRING OPTIONS(policy_tag='pii'), region STRING);
         INSERT INTO sec.users VALUES (1, 'a@x.com', 'eu'), (2, 'b@x.com', 'us');
         CREATE ROW ACCESS POLICY r_eu ON sec.users GRANT TO ('user:alice@example.com') FILTER USING (region = 'eu');
         CREATE ROW ACCESS POLICY r_admin ON sec.users GRANT TO ('group:admins') FILTER USING (TRUE);",
    )
    .unwrap();

    // Default user matches neither policy -> 0 rows visible
    let none_qr = eng.query("SELECT id FROM sec.users").unwrap();
    assert_eq!(none_qr.rows.len(), 0);

    // Alice sees only EU row + SESSION_USER()
    eng.set_session_user("alice@example.com");
    let alice_qr = eng
        .query("SELECT id, email, SESSION_USER() FROM sec.users")
        .unwrap();
    assert_eq!(alice_qr.rows.len(), 1);
    assert_eq!(alice_qr.rows[0][0], BqValue::Int64(1));
    assert_eq!(
        alice_qr.rows[0][2],
        BqValue::String("alice@example.com".to_owned())
    );

    // Enable column masking with custom mask string -> Alice sees masked email
    eng.set_enforce_column_masking(true);
    eng.set_masking_policy("pii", Some(BqValue::String("***".to_owned())));
    let masked = eng.query("SELECT email FROM sec.users").unwrap();
    assert_eq!(masked.rows[0][0], BqValue::String("***".to_owned()));

    // Admin with fine_grained_reader sees all rows unmasked
    eng.set_session_roles(vec!["admins".to_owned(), "fine_grained_reader".to_owned()]);
    let admin = eng
        .query("SELECT id, email FROM sec.users ORDER BY id")
        .unwrap();
    assert_eq!(admin.rows.len(), 2);
    assert_eq!(admin.rows[1][1], BqValue::String("b@x.com".to_owned()));
}

#[test]
fn milestone_j_load_data_external_table_alter_schema_view_tablesample_search_geo() {
    let dir = tempfile::tempdir().unwrap();
    let csv_path = dir.path().join("items.csv");
    std::fs::write(&csv_path, "id,name\n1,alpha\n2,beta\n3,gamma\n4,delta\n").unwrap();

    let mut eng = Engine::new();
    let sql = format!(
        "CREATE SCHEMA ext_ds;
         ALTER SCHEMA ext_ds SET OPTIONS(description='updated');
         CREATE EXTERNAL TABLE ext_ds.ext_items (id INT64, name STRING)
           OPTIONS(format='CSV', uris=['{}'], skip_leading_rows=1);
         CREATE TABLE ext_ds.loaded (id INT64, name STRING);
         LOAD DATA OVERWRITE ext_ds.loaded FROM FILES(format='CSV', uris=['{}'], skip_leading_rows=1);
         CREATE VIEW ext_ds.v AS SELECT id FROM ext_ds.loaded;
         ALTER VIEW ext_ds.v AS SELECT id, name FROM ext_ds.loaded;",
        csv_path.display(),
        csv_path.display()
    );
    eng.exec_script(&sql).unwrap();

    let ext_qr = eng.query("SELECT COUNT(*) FROM ext_ds.ext_items").unwrap();
    assert_eq!(ext_qr.rows[0][0], BqValue::Int64(4));

    let samp = eng
        .query("SELECT id FROM ext_ds.loaded TABLESAMPLE SYSTEM (50 PERCENT)")
        .unwrap();
    assert_eq!(samp.rows.len(), 2);

    let vqr = eng
        .query("SELECT id, name FROM ext_ds.v ORDER BY id LIMIT 1")
        .unwrap();
    assert_eq!(vqr.columns.len(), 2);

    let sg = eng
        .query(
            "SELECT \
               SEARCH(STRUCT('Hello BigQuery World' AS txt, ['Rust'] AS tags), 'bigquery rust') AS s1, \
               SEARCH('just text', 'missing') AS s2, \
               ST_CONTAINS(ST_GEOGFROMTEXT('POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))'), ST_GEOGPOINT(5, 5)) AS in_poly, \
               ST_WITHIN(ST_GEOGPOINT(20, 20), ST_GEOGFROMTEXT('POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))')) AS out_poly",
        )
        .unwrap();
    assert_eq!(sg.rows[0][0], BqValue::Bool(true));
    assert_eq!(sg.rows[0][1], BqValue::Bool(false));
    assert_eq!(sg.rows[0][2], BqValue::Bool(true));
    assert_eq!(sg.rows[0][3], BqValue::Bool(false));
}

#[cfg(feature = "persistence")]
#[test]
fn milestone_j_incremental_redb_persistence_handles_updates_and_drops() {
    use bq_lite_core::storage::persist::PersistCache;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("inc.redb");
    let mut cache = PersistCache::default();

    let mut eng = Engine::open_persistent(&db, "local").unwrap();
    eng.exec_script(
        "CREATE SCHEMA pds;
         CREATE TABLE pds.t1 (id INT64);
         CREATE TABLE pds.t2 (id INT64);
         INSERT INTO pds.t1 VALUES (10);
         INSERT INTO pds.t2 VALUES (20);",
    )
    .unwrap();
    eng.save_persistent_incremental(&db, &mut cache).unwrap();

    // Modify only t1 and drop t2; save incrementally
    eng.exec_script("INSERT INTO pds.t1 VALUES (11); DROP TABLE pds.t2;")
        .unwrap();
    eng.save_persistent_incremental(&db, &mut cache).unwrap();

    let reloaded = Engine::open_persistent(&db, "local").unwrap();
    let qr = reloaded.query("SELECT id FROM pds.t1 ORDER BY id").unwrap();
    assert_eq!(qr.rows.len(), 2);
    assert_eq!(qr.rows[1][0], BqValue::Int64(11));
    assert!(reloaded.query("SELECT * FROM pds.t2").is_err());
}

#[cfg(feature = "js-udf")]
#[test]
fn milestone_j_js_udf_execution() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA judf;
         CREATE FUNCTION judf.calc(x FLOAT64, y FLOAT64) RETURNS FLOAT64 LANGUAGE js AS r\"\"\"
           let bonus = x > 5 ? 10 : 0;
           return x * y + bonus;
         \"\"\";
         CREATE FUNCTION judf.shout(s STRING) RETURNS STRING LANGUAGE js AS r\"\"\"
           return s.trim().toUpperCase() + '!';
         \"\"\";",
    )
    .unwrap();
    let qr = eng
        .query("SELECT judf.calc(6.0, 2.0), judf.shout('  hi  ')")
        .unwrap();
    assert_eq!(qr.rows[0][0], BqValue::Float64(22.0));
    assert_eq!(qr.rows[0][1], BqValue::String("HI!".to_owned()));
}
