//! Prints the parameter provenance report as a Markdown table (the
//! "Placeholder parameters" table in `docs/PARITY.md`).
//!
//! `cargo run -p asamu-player --example provenance_table`

fn main() {
    let params = asamu_player::PlayerParams::default();
    print!("{}", params.provenance_markdown_table());
    if params.all_placeholders() {
        println!(
            "\nAll {} parameters are placeholders.",
            params.provenance_report().len()
        );
    }
}
