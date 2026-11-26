use bq_lite_core::parser::{parse_expr, parse_query, parse_script};
use bq_lite_core::BqErrorKind;
use proptest::prelude::*;

proptest! {
    #[test]
    fn arbitrary_utf8_never_panics_parser(s in ".*") {
        if let Err(err) = parse_script(&s) {
            prop_assert_eq!(err.kind, BqErrorKind::Syntax);
        }
        if let Err(err) = parse_query(&s) {
            prop_assert_eq!(err.kind, BqErrorKind::Syntax);
        }
        if let Err(err) = parse_expr(&s) {
            prop_assert_eq!(err.kind, BqErrorKind::Syntax);
        }
    }

    #[test]
    fn deeply_nested_parens_never_stack_overflow(depth in 1usize..500usize) {
        let mut sql = String::with_capacity(depth * 2 + 1);
        for _ in 0..depth {
            sql.push('(');
        }
        sql.push('1');
        for _ in 0..depth {
            sql.push(')');
        }
        let res = parse_expr(&sql);
        if depth >= 48 {
            let err = res.unwrap_err();
            prop_assert_eq!(err.kind, BqErrorKind::Syntax);
        } else {
            prop_assert!(res.is_ok());
        }
    }

    #[test]
    fn mutated_sql_tokens_never_panic(
        parts in prop::collection::vec(
            prop::sample::select(vec![
                "SELECT", "FROM", "WHERE", "GROUP", "BY", "HAVING", "QUALIFY",
                "WITH", "UNNEST", "STRUCT", "ARRAY", "CAST", "AS", "INT64",
                "SAFE.", "NULL", "TRUE", "FALSE", "|>", "(", ")", "[", "]",
                "<", ">", ",", ".", ";", "=", "+", "-", "*", "'x'", "`a.b`", "1",
            ]),
            1..40,
        )
    ) {
        let sql = parts.join(" ");
        if let Err(err) = parse_script(&sql) {
            prop_assert_eq!(err.kind, BqErrorKind::Syntax);
        }
    }
}
