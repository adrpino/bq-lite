use std::collections::BTreeMap;

use bq_lite_core::catalog::TableRef;
use bq_lite_core::{BqType, BqValue, Engine};

#[test]
fn milestone_g1_parameterized_dml_ddl_and_case_insensitive_params() {
    let mut eng = Engine::new();
    let mut params = BTreeMap::new();
    params.insert("InitVal".to_owned(), BqValue::Int64(10));
    params.insert("Label".to_owned(), BqValue::String("alpha".to_owned()));
    params.insert("TargetId".to_owned(), BqValue::Int64(1));

    eng.exec_script_with_params(
        "CREATE SCHEMA gds;
         CREATE TABLE gds.items (id INT64, name STRING) AS SELECT 1 AS id, @label AS name;
         INSERT INTO gds.items VALUES (2, @LABEL);
         UPDATE gds.items SET name = 'updated' WHERE id = @targetid;
         DELETE FROM gds.items WHERE id = @initval;",
        &params,
        &[],
    )
    .unwrap();

    let qr = eng
        .query_with_params(
            "SELECT id, name, @initval + 5 AS total FROM gds.items ORDER BY id",
            &params,
            &[],
        )
        .unwrap();
    assert_eq!(qr.rows.len(), 2);
    assert_eq!(qr.rows[0][1], BqValue::String("updated".to_owned()));
    assert_eq!(qr.rows[0][2], BqValue::Int64(15));
    assert_eq!(qr.rows[1][1], BqValue::String("alpha".to_owned()));
}

#[test]
fn milestone_g2_zero_row_schema_and_empty_outer_joins() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA gds2;
         CREATE TABLE gds2.left_t (id INT64, tag STRING);
         CREATE TABLE gds2.right_t (id INT64, score FLOAT64);
         INSERT INTO gds2.left_t VALUES (1, 'a'), (2, 'b');",
    )
    .unwrap();

    // Empty SELECT retains projected column schema
    let empty_qr = eng
        .query("SELECT id, UPPER(tag) AS up FROM gds2.right_t JOIN gds2.left_t USING (id)")
        .unwrap();
    assert_eq!(empty_qr.rows.len(), 0);
    assert_eq!(empty_qr.columns.len(), 2);
    assert_eq!(empty_qr.columns[0].name, "id");
    assert_eq!(empty_qr.columns[0].ty, BqType::Int64);
    assert_eq!(empty_qr.columns[1].name, "up");

    // CTAS with WHERE FALSE preserves schema columns
    eng.exec("CREATE TABLE gds2.ctas_empty AS SELECT id, tag FROM gds2.left_t WHERE FALSE")
        .unwrap();
    let ctas_meta = eng
        .catalog()
        .get_table(&TableRef::new("local", "gds2", "ctas_empty"))
        .unwrap();
    assert_eq!(ctas_meta.columns.len(), 2);
    assert_eq!(ctas_meta.columns[0].ty, BqType::Int64);

    // LEFT JOIN against empty right table yields NULLs cleanly
    let lj = eng
        .query(
            "SELECT l.id, r.score FROM gds2.left_t l LEFT JOIN gds2.right_t r ON l.id = r.id ORDER BY l.id",
        )
        .unwrap();
    assert_eq!(lj.rows.len(), 2);
    assert_eq!(lj.rows[0], vec![BqValue::Int64(1), BqValue::Null]);
    assert_eq!(lj.rows[1], vec![BqValue::Int64(2), BqValue::Null]);

    // RIGHT JOIN when left is empty preserves left NULL columns
    let rj = eng
        .query("SELECT r.id AS rid, l.id AS lid FROM gds2.right_t r RIGHT JOIN gds2.left_t l ON r.id = l.id ORDER BY l.id")
        .unwrap();
    assert_eq!(rj.rows.len(), 2);
    assert_eq!(rj.rows[0], vec![BqValue::Null, BqValue::Int64(1)]);
}

#[test]
fn milestone_g3_grouped_windows_ordinals_aliases_and_distinct_order() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA gds3;
         CREATE TABLE gds3.sales (dept STRING, amt INT64);
         INSERT INTO gds3.sales VALUES ('eng', 10), ('eng', 20), ('ops', 5), ('hr', 100);",
    )
    .unwrap();

    // GROUP BY + Window Function + QUALIFY
    let gw = eng
        .query(
            "SELECT dept, SUM(amt) AS tot, RANK() OVER (ORDER BY SUM(amt) DESC) AS rk \
             FROM gds3.sales GROUP BY dept QUALIFY rk <= 2 ORDER BY rk",
        )
        .unwrap();
    assert_eq!(gw.rows.len(), 2);
    assert_eq!(gw.rows[0][0], BqValue::String("hr".to_owned()));
    assert_eq!(gw.rows[0][1], BqValue::Int64(100));
    assert_eq!(gw.rows[0][2], BqValue::Int64(1));
    assert_eq!(gw.rows[1][0], BqValue::String("eng".to_owned()));
    assert_eq!(gw.rows[1][2], BqValue::Int64(2));

    // GROUP BY ordinal + alias
    let ord = eng
        .query("SELECT UPPER(dept) AS d_up, COUNT(*) AS cnt FROM gds3.sales GROUP BY 1 ORDER BY 1")
        .unwrap();
    assert_eq!(ord.rows.len(), 3);
    assert_eq!(ord.rows[0][0], BqValue::String("ENG".to_owned()));

    let al = eng
        .query(
            "SELECT UPPER(dept) AS d_up, SUM(amt) AS s FROM gds3.sales GROUP BY d_up HAVING d_up = 'ENG'",
        )
        .unwrap();
    assert_eq!(al.rows.len(), 1);
    assert_eq!(al.rows[0][1], BqValue::Int64(30));

    // SELECT DISTINCT + ORDER BY alignment
    let dist = eng
        .query("SELECT DISTINCT dept FROM gds3.sales ORDER BY dept DESC")
        .unwrap();
    assert_eq!(
        dist.rows,
        vec![
            vec![BqValue::String("ops".to_owned())],
            vec![BqValue::String("hr".to_owned())],
            vec![BqValue::String("eng".to_owned())],
        ]
    );
}

#[test]
fn milestone_g4_short_circuit_and_or_if_coalesce_ifnull() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT \
               (FALSE AND (1 / 0 = 1)) AS a_sc, \
               (TRUE OR (1 / 0 = 1)) AS o_sc, \
               IF(0 = 0, 42, 10 / 0) AS if_sc, \
               COALESCE(99, 10 / 0) AS coal_sc, \
               IFNULL(7, 10 / 0) AS ifn_sc",
        )
        .unwrap();
    assert_eq!(qr.rows[0][0], BqValue::Bool(false));
    assert_eq!(qr.rows[0][1], BqValue::Bool(true));
    assert_eq!(qr.rows[0][2], BqValue::Int64(42));
    assert_eq!(qr.rows[0][3], BqValue::Int64(99));
    assert_eq!(qr.rows[0][4], BqValue::Int64(7));
}

#[test]
fn milestone_g5_unnest_struct_unpacking_and_struct_star() {
    let eng = Engine::new();
    // Unaliased UNNEST(ARRAY<STRUCT>) unpacks fields directly
    let unaliased = eng
        .query(
            "SELECT a, b FROM UNNEST([STRUCT(1 AS a, 'x' AS b), STRUCT(2 AS a, 'y' AS b)]) ORDER BY a",
        )
        .unwrap();
    assert_eq!(unaliased.rows.len(), 2);
    assert_eq!(
        unaliased.rows[0],
        vec![BqValue::Int64(1), BqValue::String("x".to_owned())]
    );

    // Aliased UNNEST supports r.a, unqualified a, SELECT * (returns unpacked fields), and SELECT r (whole struct)
    let star_qr = eng
        .query("SELECT * FROM UNNEST([STRUCT(10 AS a, 'z' AS b)]) AS r")
        .unwrap();
    assert_eq!(star_qr.columns.len(), 2);
    assert_eq!(star_qr.columns[0].name, "a");
    assert_eq!(star_qr.columns[1].name, "b");

    // SELECT s.* struct wildcard with EXCEPT/REPLACE
    let sstar = eng
        .query(
            "SELECT s.* EXCEPT(c) REPLACE(a * 10 AS a) \
             FROM (SELECT STRUCT(3 AS a, 'hi' AS b, 99 AS c) AS s)",
        )
        .unwrap();
    assert_eq!(sstar.columns.len(), 2);
    assert_eq!(sstar.columns[0].name, "a");
    assert_eq!(sstar.columns[1].name, "b");
    assert_eq!(
        sstar.rows[0],
        vec![BqValue::Int64(30), BqValue::String("hi".to_owned())]
    );
}

#[test]
fn milestone_g6_case_insensitive_dataset_storage_and_struct_null_eq() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA ci_ds OPTIONS(is_case_insensitive = TRUE);
         CREATE TABLE ci_ds.MyTable (id INT64, label STRING);
         INSERT INTO ci_ds.mytable VALUES (1, 'ok');",
    )
    .unwrap();
    let qr = eng.query("SELECT id, label FROM ci_ds.MYTABLE").unwrap();
    assert_eq!(qr.rows.len(), 1);
    assert_eq!(qr.rows[0][1], BqValue::String("ok".to_owned()));

    let eq_qr = eng
        .query(
            "SELECT \
               (STRUCT(1, NULL) = STRUCT(1, NULL)) IS NULL AS s_null, \
               (STRUCT(1, NULL) = STRUCT(2, NULL)) AS s_false, \
               ([1, NULL] = [1, NULL]) IS NULL AS a_null, \
               ([1, NULL] = [2, NULL]) AS a_false, \
               (STRUCT(1, NULL) IS NOT DISTINCT FROM STRUCT(1, NULL)) AS s_dist",
        )
        .unwrap();
    assert_eq!(eq_qr.rows[0][0], BqValue::Bool(true));
    assert_eq!(eq_qr.rows[0][1], BqValue::Bool(false));
    assert_eq!(eq_qr.rows[0][2], BqValue::Bool(true));
    assert_eq!(eq_qr.rows[0][3], BqValue::Bool(false));
    assert_eq!(eq_qr.rows[0][4], BqValue::Bool(true));
}

#[test]
fn milestone_g7_partition_metadata_and_joined_pseudo_cols() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA pds_g;
         CREATE TABLE pds_g.events (id INT64) PARTITION BY _PARTITIONDATE;
         CREATE TABLE pds_g.dim (id INT64, name STRING);
         INSERT INTO pds_g.events VALUES (1);
         INSERT INTO pds_g.dim VALUES (1, 'one');",
    )
    .unwrap();

    let meta = eng
        .catalog()
        .get_table(&TableRef::new("local", "pds_g", "events"))
        .unwrap();
    assert_eq!(meta.partition_by.as_deref(), Some("_PARTITIONDATE"));

    let joined = eng
        .query(
            "SELECT e.id, d.name, e._PARTITIONDATE IS NOT NULL AS has_pd, e._PARTITIONTIME IS NOT NULL AS has_pt \
             FROM pds_g.events e JOIN pds_g.dim d ON e.id = d.id",
        )
        .unwrap();
    assert_eq!(joined.rows.len(), 1);
    assert_eq!(joined.rows[0][2], BqValue::Bool(true));
    assert_eq!(joined.rows[0][3], BqValue::Bool(true));
}
