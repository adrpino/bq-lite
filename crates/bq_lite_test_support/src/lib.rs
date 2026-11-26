pub use bq_lite_core;

/// Asserts that a `BqResult` failed with the expected `BqErrorKind`.
#[macro_export]
macro_rules! assert_err_kind {
    ($res:expr, $expected:expr $(,)?) => {{
        match $res {
            Ok(val) => panic!("expected error {:?}, got Ok({val:?})", $expected),
            Err(err) => assert_eq!(
                err.kind, $expected,
                "expected error kind {:?}, got {:?} ({})",
                $expected, err.kind, err.message
            ),
        }
    }};
}
