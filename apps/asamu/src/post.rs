//! Post-processing, fog and sky approximations from the original level data.
//!
//! Status: stub — owned by its workstream. Registered in `add_default_plugins`.

use bevy::prelude::*;

/// Post-processing, fog and sky approximations from the original level data.
pub struct PostPlugin;

impl Plugin for PostPlugin {
    fn build(&self, _app: &mut App) {}
}
