//! Keeps the generated tables in `docs/PARITY.md` ("Parameters") in sync
//! with the code. Regenerate with
//! `cargo run -p asamu-player --example provenance_table`.

use std::path::Path;

#[test]
fn parity_doc_contains_the_generated_tables() {
    let doc_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/PARITY.md");
    let doc = std::fs::read_to_string(&doc_path).expect("docs/PARITY.md is readable");
    let hint = "docs/PARITY.md is out of date; regenerate the tables with \
                `cargo run -p asamu-player --example provenance_table`";
    for (what, table) in [
        (
            "original parameters",
            asamu_player::PlayerParams::asamu_original().provenance_markdown_table(),
        ),
        (
            "script constants",
            asamu_player::pawn::script_constants_markdown_table(),
        ),
        (
            "placeholder parameters",
            asamu_player::PlayerParams::placeholder().provenance_markdown_table(),
        ),
    ] {
        assert!(doc.contains(&table), "{hint} ({what}):\n{table}");
    }
    assert!(doc.contains("## Parameters"));
    assert!(doc.contains("## Known deviations"));
    assert!(doc.contains("## Trace format"));
}
