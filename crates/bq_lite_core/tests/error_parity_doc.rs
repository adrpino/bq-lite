use std::collections::BTreeMap;
use std::path::PathBuf;

use bq_lite_core::BqErrorKind;

#[derive(Debug, PartialEq, Eq)]
struct DocRow {
    http_status: u16,
    reason: String,
    api_status: String,
    safe_suppressible: bool,
}

#[test]
fn error_parity_doc_matches_bq_error_kind() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let doc_path = manifest_dir.join("../..").join("docs/error-parity.md");
    let content = std::fs::read_to_string(&doc_path).expect("read docs/error-parity.md");

    let mut documented = BTreeMap::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|')
            || trimmed.starts_with("| Variant")
            || trimmed.starts_with("|---")
        {
            continue;
        }
        let cols: Vec<&str> = trimmed
            .split('|')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        assert!(cols.len() >= 5, "malformed row in error-parity.md: {line}");
        let variant = cols[0].trim_matches('`').to_owned();
        let http_status: u16 = cols[1]
            .trim_matches('`')
            .parse()
            .expect("valid HTTP status");
        let reason = cols[2].trim_matches('`').to_owned();
        let api_status = cols[3].trim_matches('`').to_owned();
        let safe_suppressible: bool = cols[4].trim_matches('`').parse().expect("valid bool");
        documented.insert(
            variant,
            DocRow {
                http_status,
                reason,
                api_status,
                safe_suppressible,
            },
        );
    }

    assert_eq!(
        documented.len(),
        BqErrorKind::ALL.len(),
        "docs/error-parity.md row count must match BqErrorKind::ALL"
    );

    for &kind in BqErrorKind::ALL {
        let row = documented.get(kind.variant_name()).unwrap_or_else(|| {
            panic!(
                "missing variant {} in docs/error-parity.md",
                kind.variant_name()
            )
        });
        assert_eq!(row.http_status, kind.http_status());
        assert_eq!(row.reason, kind.api_reason());
        assert_eq!(row.api_status, kind.api_status());
        assert_eq!(row.safe_suppressible, kind.is_safe_suppressible());
    }
}
