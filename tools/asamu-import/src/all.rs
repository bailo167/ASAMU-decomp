//! all — stub, owned by its workstream.

use anyhow::{Result, bail};

#[derive(clap::Args, Debug)]
pub struct Args {}

pub fn run(_ctx: &crate::Ctx, _args: Args) -> Result<()> {
    bail!("all is not implemented yet")
}
