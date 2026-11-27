mod googlesql_compliance {
    mod compare;
    mod parser;
    mod runner;
    mod value_parse;

    use std::collections::BTreeMap;
    use std::io::Write;
    use std::path::PathBuf;

    use runner::{run_test_content, FileReport};
    use serde::{Deserialize, Serialize};

    const BUCKET_A_FILES: &[&str] = &[
        // Core expressions, literals, types, casts
        "arithmetic_functions.test",
        "bytes.test",
        "case_statement_queries.test",
        "cast_function.test",
        "cast_function_to_json.test",
        "civil_time.test",
        "comparison_functions.test",
        "concat_function.test",
        "conditional_evaluation.test",
        "constant_queries.test",
        "date.test",
        "additional_date_time_functions.test",
        "generate_uuid.test",
        "hash.test",
        "interval.test",
        "is_unknown.test",
        "json_queries.test",
        "logical_functions.test",
        "math_functions.test",
        "numeric.test",
        "rand.test",
        "range.test",
        "range_constructors.test",
        "range_functions.test",
        "regexp_functions.test",
        "round.test",
        "safe_function.test",
        "strings.test",
        "timezones.test",
        // Arrays & Structs
        "array_constructors.test",
        "array_functions.test",
        "array_functions_with_lambda.test",
        "array_joins.test",
        "array_of_arrays.test",
        "array_queries.test",
        "struct_queries.test",
        "struct_positional_accessor.test",
        "unnest_queries.test",
        // Queries, Joins, Set Ops, CTEs, Subqueries, Pivot/Unpivot, Pipe
        "in_queries.test",
        "join_queries.test",
        "limit_queries.test",
        "orderby_queries.test",
        "orderby_numeric_queries.test",
        "select_distinct.test",
        "unionall_queries.test",
        "union_distinct_queries.test",
        "except_intersect_queries.test",
        "set_operation_by_name.test",
        "with_queries.test",
        "with_recursive.test",
        "subquery_expression_queries.test",
        "pivot.test",
        "unpivot.test",
        "qualify.test",
        "pipe_syntax.test",
        "pipe_operators.test",
        "tablesample.test",
        // Aggregations & Analytic/Window Functions
        "aggregation_queries.test",
        "aggregation_distinct_queries.test",
        "aggregation_having_modifier_queries.test",
        "aggregation_order_by_queries.test",
        "any_value_aggregation.test",
        "array_aggregation.test",
        "group_by_all.test",
        "groupby_queries.test",
        "groupby_queries_2.test",
        "grouping_sets_queries.test",
        "having_queries.test",
        "numeric_aggregation_queries.test",
        "analytic_row_number.test",
        "analytic_rank.test",
        "analytic_dense_rank.test",
        "analytic_ntile.test",
        "analytic_percent_rank.test",
        "analytic_cume_dist.test",
        "analytic_first_value.test",
        "analytic_last_value.test",
        "analytic_nth_value.test",
        "analytic_lag.test",
        "analytic_lead.test",
        "analytic_count.test",
        "analytic_count_if.test",
        "analytic_sum.test",
        "analytic_avg.test",
        "analytic_min_max.test",
        "analytic_bit_and.test",
        "analytic_bit_or.test",
        "analytic_bit_xor.test",
        "analytic_logical_and.test",
        "analytic_logical_or.test",
        "analytic_any_value.test",
        "analytic_array_aggregation.test",
        "analytic_string_aggregation.test",
        "analytic_partitionby_orderby.test",
        "analytic_window_frames.test",
        "analytic_null_handling_modifier_queries.test",
        // DML & Routines
        "dml_insert.test",
        "dml_update.test",
        "dml_update_struct.test",
        "dml_delete.test",
        "dml_returning.test",
        "call_sql_udf.test",
        "call_sql_tvf.test",
    ];

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct BaselineEntry {
        passed: usize,
        failed: usize,
        skipped: usize,
    }

    #[test]
    fn harness_self_test_runs_offline() {
        let sample = r#"
[prepare_database]
CREATE TABLE T AS
SELECT CAST(1 AS INT64) AS id, 'ada' AS name, CAST(NULL AS INT32) AS extra
UNION ALL
SELECT 2, 'grace', 5
--
ARRAY<STRUCT<id INT64, name STRING, extra INT32>>[
  {1, "ada", NULL},
  {2, "grace", 5}
]
==
[name=ordered_query]
SELECT id, UPPER(name) FROM T ORDER BY id
--
ARRAY<STRUCT<id INT64, STRING>>[known order:
  {1, "ADA"},
  {2, "GRACE"}
]
==
[name=unordered_query]
SELECT id FROM T
--
ARRAY<STRUCT<id INT64>>[unknown order:
  {2},
  {1}
]
==
[name=numeric_and_json]
SELECT NUMERIC '1.2500', JSON '{"a": 1}', [1, 2], STRUCT(1 AS x, true AS y)
--
ARRAY<STRUCT<NUMERIC, JSON, ARRAY<INT64>, STRUCT<x INT64, y BOOL>>>[
  {1.25, {"a":1}, [1, 2], {1, true}}
]
==
[name=expected_error]
SELECT CAST('not_a_number' AS INT64)
--
ERROR: generic::out_of_range: Bad int64 value
==
[name=dml_isolation_1]
INSERT INTO T VALUES (3, 'linus', 7)
--
STRUCT<
  num_rows_modified INT64,
  all_rows ARRAY<>
>{1, ARRAY<STRUCT<id INT64, name STRING, extra INT64>>[{1, "ada", NULL}, {2, "grace", 5}, {3, "linus", 7}]}
==
[name=dml_isolation_2]
SELECT COUNT(*) FROM T
--
ARRAY<STRUCT<INT64>>[{2}]
==
[name=tz_scoped_skip]
[default_time_zone=America/Los_Angeles]
SELECT 1
--
ARRAY<STRUCT<INT64>>[{1}]
==
[name=after_tz_still_runs]
SELECT 42
--
ARRAY<STRUCT<INT64>>[{42}]
==
"#;
        let report = run_test_content("self_test.test", sample);
        assert_eq!(
            report.failed, 0,
            "self_test failures: {:?}",
            report.sample_failures
        );
        assert_eq!(report.passed, 7);
        assert_eq!(report.skipped, 1);
    }

    #[test]
    fn googlesql_bucket_a_compliance_ratchet() {
        let Ok(testdata_dir) = std::env::var("BQ_LITE_GOOGLESQL_DIR") else {
            return;
        };
        let root = PathBuf::from(testdata_dir);
        assert!(
            root.is_dir(),
            "BQ_LITE_GOOGLESQL_DIR '{}' does not exist",
            root.display()
        );

        let reports: Vec<FileReport> = BUCKET_A_FILES
            .iter()
            .map(|&fname| {
                let p = root.join(fname);
                assert!(
                    p.is_file(),
                    "Required Bucket A test file '{}' is missing",
                    p.display()
                );
                let content = std::fs::read_to_string(&p).unwrap();
                run_test_content(fname, &content)
            })
            .collect();

        print_scorecard(&reports);

        let baseline_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/googlesql_compliance/baseline.json");
        if std::env::var("BQ_LITE_UPDATE_BASELINE").as_deref() == Ok("1") {
            let map: BTreeMap<String, BaselineEntry> = reports
                .iter()
                .map(|r| {
                    (
                        r.file_name.clone(),
                        BaselineEntry {
                            passed: r.passed,
                            failed: r.failed,
                            skipped: r.skipped,
                        },
                    )
                })
                .collect();
            let json = serde_json::to_string_pretty(&map).unwrap();
            std::fs::write(&baseline_path, format!("{json}\n")).unwrap();
            println!("Updated baseline at {}", baseline_path.display());
            return;
        }

        if baseline_path.exists() {
            let raw = std::fs::read_to_string(&baseline_path).unwrap();
            let baseline: BTreeMap<String, BaselineEntry> = serde_json::from_str(&raw).unwrap();
            let mut regressions = Vec::new();
            for r in &reports {
                if let Some(prev) = baseline.get(&r.file_name) {
                    if r.passed < prev.passed {
                        regressions.push(format!(
                            "{}: passed dropped {} -> {} (samples: {:?})",
                            r.file_name, prev.passed, r.passed, r.sample_failures
                        ));
                    }
                }
            }
            assert!(
                regressions.is_empty(),
                "GoogleSQL compliance regressions detected:\n{}",
                regressions.join("\n")
            );
        }
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "Scorecard percentage display only"
    )]
    fn print_scorecard(reports: &[FileReport]) {
        let mut out = std::io::stdout();
        let mut tot_pass = 0_usize;
        let mut tot_fail = 0_usize;
        let mut tot_skip = 0_usize;
        let mut all_buckets: BTreeMap<String, usize> = BTreeMap::new();

        let _ = writeln!(out, "\n=== GoogleSQL Bucket A Compliance Scorecard ===");
        for r in reports {
            tot_pass += r.passed;
            tot_fail += r.failed;
            tot_skip += r.skipped;
            for (k, v) in &r.failure_buckets {
                *all_buckets.entry(k.clone()).or_insert(0) += v;
            }
            let run = r.passed + r.failed;
            let pct = if run == 0 {
                100.0
            } else {
                (r.passed as f64) * 100.0 / (run as f64)
            };
            let _ = writeln!(
                out,
                "  {:46} {:4}/{:<4} ({:5.1}%)  [skip {:3}]",
                r.file_name, r.passed, run, pct, r.skipped
            );
        }

        let tot_run = tot_pass + tot_fail;
        let tot_pct = if tot_run == 0 {
            100.0
        } else {
            (tot_pass as f64) * 100.0 / (tot_run as f64)
        };
        let _ = writeln!(
        out,
        "\nTOTAL: {tot_pass}/{tot_run} passed ({tot_pct:.1}%), {tot_fail} failed, {tot_skip} skipped across {} files",
        reports.len()
    );

        let mut sorted_buckets: Vec<(String, usize)> = all_buckets.into_iter().collect();
        sorted_buckets.sort_by_key(|a| std::cmp::Reverse(a.1));
        let _ = writeln!(out, "\nTop 25 Failure Root Causes:");
        for (bucket, cnt) in sorted_buckets.into_iter().take(25) {
            let _ = writeln!(out, "  {cnt:5}  {bucket}");
        }
    }
}
