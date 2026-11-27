use std::collections::BTreeMap;

use bq_lite_core::catalog::{ColumnSchema, TableKind, TableMeta, TableRef};
use bq_lite_core::{BqType, BqValue, Engine};
use chrono::Utc;
use proptest::prelude::*;

proptest! {
    #[test]
    fn cow_forks_isolate_interleaved_mutations(
        adds_a in prop::collection::vec(1i64..1000, 0..15),
        adds_b in prop::collection::vec(1000i64..2000, 0..15),
    ) {
        let mut parent = Engine::new();
        parent.create_dataset("local", "ds").unwrap();
        let tref = TableRef::new("local", "ds", "nums");
        let now = Utc::now();
        parent.create_table(&tref, TableMeta {
            name: "nums".to_owned(),
            kind: TableKind::Table,
            columns: vec![ColumnSchema::new("v", BqType::Int64)],
            partition_by: None,
            cluster_by: Vec::new(),
            view_query: None,
            created_at: now,
            updated_at: now,
            options: BTreeMap::new(),
            description: None,
            friendly_name: None,
            labels: BTreeMap::new(),
            view_sql: None,
            expiration_ms: None,
            external_uris: Vec::new(),
            external_format: None,
            primary_key: Vec::new(),
        }).unwrap();
        parent.insert_rows(&tref, vec![vec![BqValue::Int64(0)]]).unwrap();

        let mut fork_a = parent.fork_isolated();
        let mut fork_b = parent.fork_isolated();

        for &v in &adds_a {
            fork_a.insert_rows(&tref, vec![vec![BqValue::Int64(v)]]).unwrap();
        }
        for &v in &adds_b {
            fork_b.insert_rows(&tref, vec![vec![BqValue::Int64(v)]]).unwrap();
        }

        let parent_cnt = parent.query("SELECT COUNT(*) FROM ds.nums").unwrap().rows[0][0].clone();
        let a_cnt = fork_a.query("SELECT COUNT(*) FROM ds.nums").unwrap().rows[0][0].clone();
        let b_cnt = fork_b.query("SELECT COUNT(*) FROM ds.nums").unwrap().rows[0][0].clone();

        prop_assert_eq!(parent_cnt, BqValue::Int64(1));
        prop_assert_eq!(a_cnt, BqValue::Int64(1 + i64::try_from(adds_a.len()).unwrap()));
        prop_assert_eq!(b_cnt, BqValue::Int64(1 + i64::try_from(adds_b.len()).unwrap()));
    }
}
