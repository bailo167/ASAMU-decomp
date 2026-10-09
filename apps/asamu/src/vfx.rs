//! Gameplay visual effects: grapple beam, hit decal/light, velocity cone, speed lines, lens effects, decals.
//!
//! Status: stub — owned by its workstream. Registered in `add_default_plugins`.

use bevy::prelude::*;

/// Gameplay visual effects: grapple beam, hit decal/light, velocity cone, speed lines, lens effects, decals.
pub struct VfxPlugin;

impl Plugin for VfxPlugin {
    fn build(&self, _app: &mut App) {}
}
