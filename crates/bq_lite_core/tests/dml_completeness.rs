use bq_lite_core::{BqType, BqValue, Engine};

#[test]
fn dml_update_array_and_nested_struct_element_mutation() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA d;
         CREATE TABLE d.arrs AS
         SELECT 1 AS id, [10, 20, 30] AS nums,
                STRUCT(100 AS x, [STRUCT(1 AS a, 'one' AS b), STRUCT(2 AS a, 'two' AS b)] AS items) AS s
         UNION ALL
         SELECT 2, [40, 50, 60],
                STRUCT(200, [STRUCT(3, 'three'), STRUCT(4, 'four')]);",
    )
    .unwrap();

    eng.exec(
        "UPDATE d.arrs
         SET nums[OFFSET(0)] = 99,
             nums[ORDINAL(3)] = nums[OFFSET(1)] + 1,
             s.items[OFFSET(1)].b = 'TWO'
         WHERE id = 1",
    )
    .unwrap();

    let res = eng
        .query("SELECT nums, s.items[OFFSET(1)].b FROM d.arrs WHERE id = 1")
        .unwrap();
    assert_eq!(
        res.rows[0],
        vec![
            BqValue::Array(vec![
                BqValue::Int64(99),
                BqValue::Int64(20),
                BqValue::Int64(21),
            ]),
            BqValue::String("TWO".to_owned()),
        ]
    );

    // Duplicate element offset in single UPDATE SET must error
    let dup = eng.exec("UPDATE d.arrs SET nums[OFFSET(0)] = 1, nums[ORDINAL(1)] = 2 WHERE id = 1");
    assert!(dup.is_err());

    // Out-of-bounds offset must error
    let oob = eng.exec("UPDATE d.arrs SET nums[OFFSET(99)] = 1 WHERE id = 1");
    assert!(oob.is_err());
}

#[test]
fn dml_delete_using_and_update_from_multiplicity_and_returning() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA d;
         CREATE TABLE d.target AS
         SELECT 1 AS id, 10 AS v UNION ALL
         SELECT 2, 20 UNION ALL
         SELECT 3, 30;
         CREATE TABLE d.src AS
         SELECT 1 AS id, 100 AS delta UNION ALL
         SELECT 2, 200;
         CREATE TABLE d.dups AS
         SELECT 1 AS id, 100 AS delta UNION ALL
         SELECT 1, 999;",
    )
    .unwrap();

    // UPDATE ... FROM with 0 source rows modifies 0 rows
    let empty_upd = eng
        .exec("UPDATE d.target SET v = 999 FROM (SELECT * FROM d.src WHERE FALSE) s WHERE TRUE")
        .unwrap();
    assert_eq!(empty_upd.rows_affected, 0);

    // UPDATE ... FROM with >1 matching source row errors
    let multi_err = eng.exec("UPDATE d.target t SET v = d.delta FROM d.dups d WHERE t.id = d.id");
    assert!(multi_err.is_err());

    // UPDATE ... FROM THEN RETURN sees both updated target and source columns
    let upd_ret = eng
        .exec(
            "UPDATE d.target t
             SET v = t.v + s.delta
             FROM d.src s
             WHERE t.id = s.id
             THEN RETURN WITH ACTION t.id, t.v AS new_v, s.delta",
        )
        .unwrap();
    assert_eq!(upd_ret.rows.len(), 2);
    assert_eq!(upd_ret.columns[3].name, "ACTION");
    assert_eq!(upd_ret.rows[0][3], BqValue::String("UPDATE".to_owned()));

    // DELETE ... USING with duplicate matches deletes each target row once
    let del_ret = eng
        .exec(
            "DELETE FROM d.target AS t
             USING d.dups AS u
             WHERE t.id = u.id
             THEN RETURN WITH ACTION AS op t.id, t.v",
        )
        .unwrap();
    assert_eq!(del_ret.rows.len(), 1);
    assert_eq!(del_ret.columns[2].name, "op");
    assert_eq!(
        del_ret.rows[0],
        vec![
            BqValue::Int64(1),
            BqValue::Int64(110),
            BqValue::String("DELETE".to_owned()),
        ]
    );
}

#[test]
fn dml_assert_rows_modified_is_atomic_and_zero_row_with_action_schema() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA d;
         CREATE TABLE d.t (id INT64, v STRING);
         INSERT INTO d.t VALUES (1, 'a'), (2, 'b') ASSERT_ROWS_MODIFIED 2;",
    )
    .unwrap();

    // Failed ASSERT_ROWS_MODIFIED rolls back / does not mutate storage
    assert!(eng
        .exec("DELETE FROM d.t WHERE id = 1 ASSERT_ROWS_MODIFIED 5")
        .is_err());
    let cnt = eng.query("SELECT COUNT(*) FROM d.t").unwrap();
    assert_eq!(cnt.rows, vec![vec![BqValue::Int64(2)]]);

    // NULL / negative ASSERT_ROWS_MODIFIED errors
    assert!(eng
        .exec("DELETE FROM d.t WHERE FALSE ASSERT_ROWS_MODIFIED NULL")
        .is_err());
    assert!(eng
        .exec("DELETE FROM d.t WHERE FALSE ASSERT_ROWS_MODIFIED -1")
        .is_err());

    // 0-row THEN RETURN WITH ACTION preserves full output schema
    let zero_ret = eng
        .exec("DELETE FROM d.t WHERE FALSE THEN RETURN WITH ACTION AS act *")
        .unwrap();
    assert_eq!(zero_ret.rows.len(), 0);
    assert_eq!(zero_ret.columns.len(), 3);
    assert_eq!(zero_ret.columns[2].name, "act");
    assert_eq!(zero_ret.columns[2].ty, BqType::String);
}
