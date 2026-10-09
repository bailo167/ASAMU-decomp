//! Localized text export (UI, tutorials, credits, subtitles per language) from the user install. — stub, owned by its workstream.

use anyhow::{Result, bail};

#[derive(clap::Args, Debug)]
pub struct Args {}

pub fn run(_ctx: &crate::Ctx, _args: Args) -> Result<()> {
    bail!("localization is not implemented yet")
}
