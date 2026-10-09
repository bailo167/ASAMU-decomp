//! Prints the generated tables of `docs/PARITY.md` ("Parameters"): the
//! original parameter set with provenance, the script-code constants of the
//! ASAMU pawn script layer, and the placeholder set kept for debugging.
//!
//! `cargo run -p asamu-player --example provenance_table`

fn main() {
    let original = asamu_player::PlayerParams::asamu_original();
    println!("### Original parameter set (`PlayerParams::asamu_original`)\n");
    print!("{}", original.provenance_markdown_table());
    println!(
        "\n{} of {} parameters are placeholders.\n",
        original.placeholder_names().len(),
        original.provenance_report().len()
    );
    println!("### Script-code constants (`asamu_player::pawn`)\n");
    print!("{}", asamu_player::pawn::script_constants_markdown_table());
    let placeholder = asamu_player::PlayerParams::placeholder();
    println!("\n### Placeholder parameter set (`PlayerParams::placeholder`, debugging)\n");
    print!("{}", placeholder.provenance_markdown_table());
    println!(
        "\n{} of {} parameters are placeholders.",
        placeholder.placeholder_names().len(),
        placeholder.provenance_report().len()
    );
}
