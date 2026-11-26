use bq_lite_core::ast::{
    BinaryOp, CreateTableKind, Expr, FunctionBody, FunctionKind, GroupByClause, Ident, InTarget,
    InsertSource, MergeConditionKind, ObjectName, PipeOp, SelectAsKind, SelectItem, SetExpr,
    SetOpKind, SetQuantifier, Statement, TableFactor,
};
use bq_lite_core::parser::{parse_expr, parse_query, parse_script, parse_statement};
use bq_lite_core::BqErrorKind;

fn assert_all_parse(cases: &[&str]) {
    for sql in cases {
        let res = parse_statement(sql);
        assert!(res.is_ok(), "failed to parse `{sql}`: {res:?}");
    }
}

#[test]
fn parses_literals_identifiers_and_operators() {
    assert_all_parse(&[
        "SELECT 1",
        "SELECT 1 + 2 * 3, (1 + 2) * 3, -5, +4, ~1",
        "SELECT 0xFF, 1.5e-3, .25, 100.",
        "SELECT TRUE, FALSE, NULL",
        "SELECT 'hello', \"world\", '''multi\nline''', \"\"\"double\nline\"\"\"",
        "SELECT r'raw\\nstring', R\"raw\\t\", b'bytes\\x41', rb'raw\\x41', br\"raw\"",
        "SELECT DATE '2026-01-15', DATETIME '2026-01-15 10:20:30', TIME '10:20:30.123456'",
        "SELECT TIMESTAMP '2026-01-15 10:20:30 UTC', NUMERIC '123.456', BIGNUMERIC '1.234567890123456789'",
        "SELECT JSON '{\"a\": 1}'",
        "SELECT RANGE<DATE>(DATE '2026-01-01', DATE '2026-02-01')",
        "SELECT INTERVAL 1 DAY, INTERVAL '1-2 3 4:5:6' YEAR TO SECOND",
        "SELECT @named_param, ?, @@error.message",
        "SELECT * FROM `my-project.my_dataset.my_table`",
        "SELECT * FROM my_proj.my_ds.my_tbl AS t",
        "SELECT `select`, `from`, date, time, status, value, key FROM t",
        "SELECT * EXCEPT(secret, internal_id) FROM users",
        "SELECT * REPLACE(UPPER(name) AS name, price * 1.1 AS price) FROM items",
        "SELECT u.* EXCEPT(hash) REPLACE(LOWER(email) AS email) FROM users u",
        "SELECT AS STRUCT 1 AS a, 'b' AS b",
        "SELECT AS VALUE STRUCT(1 AS x, 2 AS y)",
        "SELECT DISTINCT a, b FROM t",
        "SELECT ALL a FROM t",
        "SELECT a = 1, a != 2, a <> 3, a < 4, a <= 5, a > 6, a >= 7",
        "SELECT a IS NULL, a IS NOT NULL, a IS TRUE, a IS NOT FALSE, a IS UNKNOWN, a IS NOT UNKNOWN",
        "SELECT a IS DISTINCT FROM b, a IS NOT DISTINCT FROM b",
        "SELECT a BETWEEN 1 AND 10, a NOT BETWEEN 2 AND 5",
        "SELECT a LIKE '%foo%', a NOT LIKE 'bar_'",
        "SELECT a IN (1, 2, 3), a NOT IN ('x', 'y'), a IN UNNEST(@arr)",
        "SELECT a IN (SELECT id FROM other)",
        "SELECT EXISTS(SELECT 1 FROM t WHERE id = 5)",
        "SELECT 'a' || 'b', 1 | 2, 1 & 2, 1 ^ 2, 1 << 2, 8 >> 1",
    ]);
}

#[test]
fn parses_expressions_joins_windows_ctes_and_pipe_syntax() {
    assert_all_parse(&[
        "SELECT CAST(x AS INT64), SAFE_CAST(y AS NUMERIC(10, 2)), CAST(d AS STRING FORMAT 'YYYY-MM-DD')",
        "SELECT CASE x WHEN 1 THEN 'one' WHEN 2 THEN 'two' ELSE 'other' END",
        "SELECT CASE WHEN x > 0 THEN 1 ELSE 0 END",
        "SELECT EXTRACT(DAY FROM d), EXTRACT(YEAR FROM ts)",
        "SELECT SAFE.PARSE_DATE('%Y-%m-%d', s), SAFE.SUBSTR(s, 1, 5)",
        "SELECT [1, 2, 3], ARRAY<INT64>[1, 2], ARRAY<STRING>[]",
        "SELECT STRUCT(1 AS a, 'hi' AS b), STRUCT<x INT64, y ARRAY<STRING>>(1, ['a'])",
        "SELECT arr[0], arr[OFFSET(0)], arr[SAFE_OFFSET(1)], arr[ORDINAL(1)], arr[SAFE_ORDINAL(9)]",
        "SELECT s.nested.field, fn().field[OFFSET(0)]",
        "SELECT ARRAY(SELECT x * 2 FROM UNNEST([1, 2, 3]) AS x)",
        "SELECT ARRAY_TRANSFORM([1, 2, 3], x -> x + 1)",
        "SELECT ARRAY_FILTER([1, 2, 3], (x, idx) -> x > idx)",
        "SELECT COUNT(*), COUNT(DISTINCT user_id), COUNTIF(active)",
        "SELECT ARRAY_AGG(DISTINCT x IGNORE NULLS ORDER BY x DESC LIMIT 5) FROM t",
        "SELECT ANY_VALUE(name HAVING MAX updated_at) FROM t GROUP BY id",
        "SELECT ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC NULLS LAST)",
        "SELECT SUM(amt) OVER (w ROWS BETWEEN 2 PRECEDING AND CURRENT ROW) FROM t WINDOW w AS (PARTITION BY k ORDER BY ts)",
        "SELECT LAG(v, 1) IGNORE NULLS OVER (ORDER BY id) FROM t",
        "SELECT FIRST_VALUE(v RESPECT NULLS) OVER (ORDER BY id RANGE BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) FROM t",
        "SELECT * FROM a INNER JOIN b ON a.id = b.id LEFT JOIN c USING (id)",
        "SELECT * FROM a FULL OUTER JOIN b ON a.k = b.k RIGHT JOIN c ON b.k = c.k",
        "SELECT * FROM a CROSS JOIN UNNEST(a.tags) AS tag WITH OFFSET AS pos",
        "SELECT * FROM a, UNNEST(a.items) AS item WITH OFFSET",
        "SELECT * FROM `proj.ds.tbl` FOR SYSTEM_TIME AS OF TIMESTAMP_SUB(CURRENT_TIMESTAMP(), INTERVAL 1 HOUR)",
        "SELECT * FROM sales PIVOT(SUM(amount) FOR quarter IN ('Q1', 'Q2' AS q2))",
        "SELECT * FROM t UNPIVOT INCLUDE NULLS (val FOR col IN (q1, q2 AS 'Q2'))",
        "SELECT * FROM MY_TVF(arg1 => 10, arg2 => 'x')",
        "SELECT a, b, SUM(c) FROM t GROUP BY ALL HAVING SUM(c) > 10",
        "SELECT a, b, SUM(c) FROM t GROUP BY ROLLUP(a, b)",
        "SELECT a, b, SUM(c) FROM t GROUP BY CUBE(a, b)",
        "SELECT a, b, SUM(c) FROM t GROUP BY GROUPING SETS((a, b), (a), ())",
        "SELECT id, ROW_NUMBER() OVER (PARTITION BY dept ORDER BY sal DESC) rn FROM emp QUALIFY rn = 1",
        "WITH RECURSIVE nums AS (SELECT 1 AS n UNION ALL SELECT n + 1 FROM nums WHERE n < 10) SELECT * FROM nums",
        "WITH a(x, y) AS (SELECT 1, 2) SELECT x, y FROM a",
        "SELECT 1 UNION ALL SELECT 2 UNION DISTINCT SELECT 3",
        "SELECT 1 AS a INTERSECT DISTINCT SELECT 1 AS a EXCEPT DISTINCT SELECT 2 AS a",
        "SELECT 1 AS a UNION ALL BY NAME SELECT 1 AS a",
        "(SELECT 1 UNION ALL SELECT 2) ORDER BY 1 DESC LIMIT 1 OFFSET 1",
        "FROM my_table |> WHERE x > 10 |> EXTEND x * 2 AS doubled |> SELECT doubled |> ORDER BY doubled DESC |> LIMIT 5",
        "SELECT 1 AS a, 2 AS b |> SET a = 10 |> DROP b |> RENAME a AS alpha |> AS res",
        "FROM orders |> AGGREGATE COUNT(*) AS cnt, SUM(total) AS rev GROUP BY customer_id",
    ]);
}

#[test]
fn parses_dml_ddl_and_scripting_statements() {
    assert_all_parse(&[
        "INSERT INTO ds.tbl (a, b) VALUES (1, 'x'), (2, 'y')",
        "INSERT ds.tbl SELECT a, b FROM ds.other THEN RETURN *",
        "UPDATE ds.tbl t SET t.a = t.a + 1, t.meta.updated = TRUE FROM ds.src s WHERE t.id = s.id",
        "UPDATE ds.tbl SET x = 1 WHERE TRUE THEN RETURN id, x",
        "DELETE FROM ds.tbl WHERE id = 10 THEN RETURN *",
        "DELETE ds.tbl t WHERE t.expired",
        "TRUNCATE TABLE ds.tbl",
        "MERGE INTO ds.target t USING (SELECT 1 AS id, 'a' AS val) s ON t.id = s.id WHEN MATCHED AND s.val IS NOT NULL THEN UPDATE SET val = s.val WHEN NOT MATCHED BY TARGET THEN INSERT (id, val) VALUES (id, val) WHEN NOT MATCHED BY SOURCE THEN DELETE",
        "MERGE ds.target USING ds.src ON target.id = src.id WHEN NOT MATCHED THEN INSERT ROW",
        "CREATE SCHEMA IF NOT EXISTS my_ds OPTIONS(location='US')",
        "CREATE DATASET my_ds",
        "CREATE OR REPLACE TEMP TABLE tmp (id INT64 NOT NULL, tags ARRAY<STRING>, meta STRUCT<k STRING, v INT64> DEFAULT STRUCT('a', 1)) OPTIONS(expiration_timestamp=NULL)",
        "CREATE TABLE ds.events (ts TIMESTAMP, user_id INT64, PRIMARY KEY (user_id) NOT ENFORCED) PARTITION BY DATE(ts) CLUSTER BY user_id AS SELECT CURRENT_TIMESTAMP(), 1",
        "CREATE TABLE ds.copy_tbl LIKE ds.src",
        "CREATE TABLE ds.clone_tbl CLONE ds.src FOR SYSTEM_TIME AS OF TIMESTAMP '2026-01-01 00:00:00 UTC'",
        "CREATE SNAPSHOT TABLE ds.snap CLONE ds.src",
        "ALTER TABLE ds.tbl ADD COLUMN IF NOT EXISTS note STRING DEFAULT 'n/a', DROP COLUMN IF EXISTS old_col, RENAME COLUMN a TO b",
        "ALTER TABLE ds.tbl ALTER COLUMN x SET DATA TYPE NUMERIC, ALTER COLUMN x DROP NOT NULL, SET OPTIONS(description='test')",
        "ALTER TABLE ds.tbl RENAME TO new_tbl",
        "CREATE OR REPLACE VIEW ds.v (a, b) OPTIONS(description='v') AS SELECT 1, 2",
        "CREATE MATERIALIZED VIEW IF NOT EXISTS ds.mv AS SELECT k, COUNT(*) c FROM ds.t GROUP BY k",
        "CREATE OR REPLACE FUNCTION ds.add_one(x INT64) RETURNS INT64 AS (x + 1)",
        "CREATE TEMP FUNCTION ds.js_add(x INT64, y INT64) RETURNS INT64 LANGUAGE js AS r\"\"\"return x + y;\"\"\"",
        "CREATE TABLE FUNCTION ds.get_users(min_age INT64) AS SELECT * FROM ds.users WHERE age >= min_age",
        "CREATE OR REPLACE PROCEDURE ds.my_proc(IN x INT64, OUT y INT64) BEGIN SET y = x * 2; END",
        "CREATE ROW ACCESS POLICY p1 ON ds.tbl GRANT TO ('user:a@b.com') FILTER USING (region = 'US')",
        "DROP TABLE IF EXISTS ds.tbl",
        "DROP SCHEMA IF EXISTS ds CASCADE",
        "DROP VIEW ds.v",
        "DROP MATERIALIZED VIEW ds.mv",
        "DROP FUNCTION ds.add_one",
        "DROP TABLE FUNCTION ds.get_users",
        "DROP PROCEDURE ds.my_proc",
        "DROP SNAPSHOT TABLE ds.snap",
        "DROP ROW ACCESS POLICY IF EXISTS p1 ON ds.tbl",
        "DECLARE x INT64 DEFAULT 10",
        "DECLARE a, b STRING",
        "SET x = x + 1",
        "SET (a, b) = (SELECT 'x', 'y')",
        "IF x > 0 THEN SELECT 1; ELSEIF x = 0 THEN SELECT 0; ELSE SELECT -1; END IF",
        "CASE x WHEN 1 THEN SET y = 10; ELSE SET y = 20; END CASE",
        "LOOP SET i = i + 1; IF i > 5 THEN BREAK; END IF; END LOOP",
        "WHILE i < 10 DO SET i = i + 1; CONTINUE; END WHILE",
        "REPEAT SET i = i + 1; UNTIL i >= 3 END REPEAT",
        "FOR r IN (SELECT id FROM ds.t) DO CALL ds.process(r.id); END FOR",
        "BEGIN DECLARE v INT64 DEFAULT 1; SELECT v; EXCEPTION WHEN ERROR THEN SELECT @@error.message; END",
        "BEGIN TRANSACTION",
        "COMMIT TRANSACTION",
        "ROLLBACK",
        "EXECUTE IMMEDIATE 'SELECT @x + ?' INTO out_var USING 5 AS x, 10",
        "CALL ds.my_proc(1, @out)",
        "RAISE USING MESSAGE = 'boom'",
        "RAISE",
        "ASSERT 1 = 1 AS 'sanity'",
        "LEAVE",
        "ITERATE",
        "RETURN",
    ]);
}

#[test]
fn parses_multi_statement_script_with_comments_and_empty_semicolons() {
    let script = r"
        -- leading comment
        # hash comment
        /* block comment */
        DECLARE total INT64 DEFAULT 0;;
        FOR rec IN (SELECT n FROM UNNEST([1, 2, 3]) AS n) DO
            SET total = total + rec.n;
        END FOR;
        ASSERT total = 6 AS 'sum must be 6';
    ";
    let stmts = parse_script(script).unwrap();
    assert_eq!(stmts.len(), 3);
    assert!(matches!(stmts[0], Statement::Declare(_)));
    assert!(matches!(stmts[1], Statement::ForIn(_)));
    assert!(matches!(stmts[2], Statement::Assert(_)));
}

#[test]
fn verifies_query_and_pipe_ast_structure() {
    let q = parse_query(
        "WITH cte AS (SELECT 1 AS id) SELECT * EXCEPT(secret) FROM cte |> WHERE id > 0",
    )
    .unwrap();
    assert!(q.with.is_some());
    assert_eq!(q.pipe_ops.len(), 1);
    assert!(matches!(q.pipe_ops[0], PipeOp::Where(_)));
    let SetExpr::Select(sel) = q.body else {
        panic!("expected SetExpr::Select");
    };
    assert!(matches!(
        &sel.projection[0],
        SelectItem::Wildcard { except, .. } if except.len() == 1 && except[0].value == "secret"
    ));

    let q2 = parse_query("SELECT AS VALUE 1 UNION ALL SELECT 2").unwrap();
    let SetExpr::SetOperation {
        op,
        quantifier,
        left,
        ..
    } = q2.body
    else {
        panic!("expected SetOperation");
    };
    assert_eq!(op, SetOpKind::Union);
    assert_eq!(quantifier, SetQuantifier::All);
    let SetExpr::Select(left_sel) = *left else {
        panic!("expected left Select");
    };
    assert_eq!(left_sel.as_kind, Some(SelectAsKind::Value));
}

#[test]
fn verifies_ddl_dml_and_expr_ast_structure() {
    let Statement::CreateFunction(cf) =
        parse_statement("CREATE TEMP FUNCTION f(x INT64) RETURNS INT64 LANGUAGE js AS 'return x;'")
            .unwrap()
    else {
        panic!("expected CreateFunction");
    };
    assert!(cf.temp);
    assert_eq!(cf.kind, FunctionKind::Scalar);
    assert!(matches!(cf.body, FunctionBody::Code(ref s) if s == "return x;"));

    let Statement::CreateTable(ct) =
        parse_statement("CREATE SNAPSHOT TABLE ds.s CLONE ds.t").unwrap()
    else {
        panic!("expected CreateTable");
    };
    assert_eq!(ct.kind, CreateTableKind::Snapshot);
    assert_eq!(
        ct.name,
        ObjectName::new(vec![Ident::unquoted("ds"), Ident::unquoted("s")])
    );

    let Statement::Merge(m) = parse_statement(
        "MERGE ds.t USING UNNEST([1]) s ON TRUE WHEN NOT MATCHED BY SOURCE THEN DELETE",
    )
    .unwrap() else {
        panic!("expected Merge");
    };
    assert!(matches!(m.source, TableFactor::Unnest { .. }));
    assert_eq!(m.clauses[0].kind, MergeConditionKind::NotMatchedBySource);

    let Statement::Insert(i) = parse_statement("INSERT INTO t VALUES (1), (2)").unwrap() else {
        panic!("expected Insert");
    };
    assert!(matches!(i.source, InsertSource::Values(rows) if rows.len() == 2));

    let SetExpr::Select(gb_sel) = parse_query("SELECT a FROM t GROUP BY ALL").unwrap().body else {
        panic!("expected Select");
    };
    assert_eq!(gb_sel.group_by, Some(GroupByClause::All));

    let Expr::Binary { op, left, .. } =
        parse_expr("a IN UNNEST(@arr) AND SAFE.DIVIDE(1, 0) IS NULL").unwrap()
    else {
        panic!("expected Binary");
    };
    assert_eq!(op, BinaryOp::And);
    assert!(matches!(
        *left,
        Expr::In {
            target: InTarget::Unnest(_),
            ..
        }
    ));
}

#[test]
fn malformed_sql_returns_syntax_error_with_span_and_never_panics() {
    let invalid_cases = [
        "",
        "SELECT",
        "SELECT FROM",
        "SELECT 1 +",
        "SELECT * FROM t WHERE",
        "SELECT 1 UNION SELECT 2",
        "SELECT 'unterminated",
        "SELECT `unterminated",
        "/* unclosed comment",
        "CREATE TABLE",
        "MERGE ds.t USING ds.s ON TRUE",
        "SELECT 1; SELECT 2",
        "1 + * 2",
    ];
    for bad in invalid_cases {
        let err = parse_statement(bad).unwrap_err();
        assert_eq!(err.kind, BqErrorKind::Syntax, "expected Syntax for `{bad}`");
        assert!(err.span.is_some(), "expected span for `{bad}`");
    }
}
