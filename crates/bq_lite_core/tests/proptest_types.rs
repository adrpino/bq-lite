use bq_lite_core::coercion::{cast_value, common_supertype};
use bq_lite_core::{BqErrorKind, BqNumeric, BqType, BqValue};
use proptest::prelude::*;

proptest! {
    #[test]
    fn numeric_add_sub_roundtrip_matches_exact_scaled_i128(
        a in -1_000_000_000_000i128..1_000_000_000_000i128,
        b in -1_000_000_000_000i128..1_000_000_000_000i128,
    ) {
        let na = BqNumeric::from_scaled(a).unwrap();
        let nb = BqNumeric::from_scaled(b).unwrap();
        let sum = na.checked_add(nb).unwrap();
        let back = sum.checked_sub(nb).unwrap();
        prop_assert_eq!(back, na);
    }

    #[test]
    fn numeric_display_parse_roundtrips(raw in -10_000_000_000_000_000i128..10_000_000_000_000_000i128) {
        let n = BqNumeric::from_scaled(raw).unwrap();
        let parsed: BqNumeric = n.to_string().parse().unwrap();
        prop_assert_eq!(parsed, n);
    }

    #[test]
    fn arbitrary_string_cast_to_int64_never_panics(s in ".*") {
        let res = cast_value(BqValue::String(s), &BqType::Int64);
        if let Err(err) = res {
            prop_assert_eq!(err.kind, BqErrorKind::Semantic);
        }
    }

    #[test]
    fn supertype_with_untyped_null_is_identity(idx in 0usize..6) {
        let candidates = [
            BqType::Bool,
            BqType::Int64,
            BqType::Numeric,
            BqType::Float64,
            BqType::String,
            BqType::Date,
        ];
        let ty = candidates[idx].clone();
        prop_assert_eq!(
            &common_supertype(&[BqType::UntypedNull, ty.clone()]).unwrap(),
            &ty
        );
        prop_assert_eq!(
            common_supertype(&[ty.clone(), BqType::UntypedNull]).unwrap(),
            ty
        );
    }
}
