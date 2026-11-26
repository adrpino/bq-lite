use bq_lite_core::{BqNumeric, BqStructFieldValue, BqType, BqValue, StructField};
use bq_lite_server::wire_json::{decode_wire_fv_value, encode_wire_value};
use proptest::prelude::*;

proptest! {
    #[test]
    fn wire_fv_roundtrips_scalars_and_nested_records(
        b in any::<bool>(),
        i in any::<i64>(),
        raw_num in -1_000_000_000_000i128..1_000_000_000_000i128,
        s in ".*",
        bytes in prop::collection::vec(any::<u8>(), 0..32),
        days in 0i64..20_000i64,
    ) {
        let num = BqNumeric::from_scaled(raw_num).unwrap();
        let date = chrono::NaiveDate::from_ymd_opt(2000, 1, 1)
            .unwrap()
            .checked_add_signed(chrono::Duration::days(days))
            .unwrap();
        let ts = chrono::DateTime::from_timestamp_micros(1_700_000_000_123_456 + days * 1_000_000).unwrap();

        let struct_ty = BqType::structure(vec![
            StructField::named("b", BqType::Bool),
            StructField::named("i", BqType::Int64),
            StructField::named("n", BqType::Numeric),
            StructField::named("s", BqType::String),
            StructField::named("raw", BqType::Bytes),
            StructField::named("d", BqType::Date),
            StructField::named("ts", BqType::Timestamp),
            StructField::named("tags", BqType::array(BqType::String).unwrap()),
        ]);

        let val = BqValue::Struct(vec![
            BqStructFieldValue { name: Some("b".to_owned()), value: BqValue::Bool(b) },
            BqStructFieldValue { name: Some("i".to_owned()), value: BqValue::Int64(i) },
            BqStructFieldValue { name: Some("n".to_owned()), value: BqValue::Numeric(num) },
            BqStructFieldValue { name: Some("s".to_owned()), value: BqValue::String(s.clone()) },
            BqStructFieldValue { name: Some("raw".to_owned()), value: BqValue::Bytes(bytes) },
            BqStructFieldValue { name: Some("d".to_owned()), value: BqValue::Date(date) },
            BqStructFieldValue { name: Some("ts".to_owned()), value: BqValue::Timestamp(ts) },
            BqStructFieldValue {
                name: Some("tags".to_owned()),
                value: BqValue::Array(vec![BqValue::String(s)]),
            },
        ]);

        let encoded = encode_wire_value(&val, Some(&struct_ty));
        let decoded = decode_wire_fv_value(&encoded, &struct_ty).unwrap();
        prop_assert_eq!(decoded, val);
    }
}
