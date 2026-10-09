//! Baked lightmap / shadow-map rendering for converted levels.
//!
//! Status: stub — owned by its workstream. Registered in `add_default_plugins`.

use bevy::prelude::*;

/// Baked lightmap / shadow-map rendering for converted levels.
pub struct LightmapPlugin;

impl Plugin for LightmapPlugin {
    fn build(&self, _app: &mut App) {}
}
