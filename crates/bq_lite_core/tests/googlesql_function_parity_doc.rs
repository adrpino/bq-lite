use std::collections::BTreeSet;
use std::path::PathBuf;

use bq_lite_core::executor::{is_scalar_registered, registered_scalar_functions};

#[test]
fn googlesql_function_parity_doc_matches_live_engine_bidirectionally() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let doc_path = manifest_dir
        .join("../..")
        .join("docs/googlesql-function-parity.md");
    let content =
        std::fs::read_to_string(&doc_path).expect("read docs/googlesql-function-parity.md");

    let mut current_section = "";
    let mut doc_scalars = BTreeSet::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            current_section = heading.trim();
            continue;
        }
        if !trimmed.starts_with('|')
            || trimmed.starts_with("| Function")
            || trimmed.starts_with("|---")
        {
            continue;
        }
        let cols: Vec<&str> = trimmed
            .split('|')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        assert!(cols.len() >= 2, "malformed row in parity doc: {line}");
        let fn_name = cols[0].trim_matches('`').to_ascii_uppercase();
        let status = cols[1].trim_matches('`');
        if current_section == "Scalar Functions" && status == "implemented" {
            assert!(
                is_scalar_registered(&fn_name),
                "docs/googlesql-function-parity.md lists '{fn_name}' as implemented, but it is not registered"
            );
            doc_scalars.insert(fn_name);
        }
    }

    for registered in registered_scalar_functions() {
        assert!(
            doc_scalars.contains(registered),
            "engine registers scalar '{registered}', but it is missing from docs/googlesql-function-parity.md"
        );
    }
}
