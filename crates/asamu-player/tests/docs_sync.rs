//! Keeps the "Placeholder parameters" table in `docs/PARITY.md` in sync with
//! the code. Regenerate with
//! `cargo run -p asamu-player --example provenance_table`.

use std::path::Path;

#[test]
fn parity_doc_contains_the_generated_provenance_table() {
    let doc_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/PARITY.md");
    let doc = std::fs::read_to_string(&doc_path).expect("docs/PARITY.md is readable");
    let table = asamu_player::PlayerParams::default().provenance_markdown_table();
    assert!(
        doc.contains(&table),
        "docs/PARITY.md is out of date; regenerate the table with \
         `cargo run -p asamu-player --example provenance_table`:\n{table}"
    );
    assert!(doc.contains("## Placeholder parameters"));
    assert!(doc.contains("## Trace format"));
}
