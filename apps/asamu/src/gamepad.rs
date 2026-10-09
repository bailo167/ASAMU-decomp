//! Gamepad input per DefaultInput.ini.
//!
//! Status: stub — owned by its workstream. Registered in `add_default_plugins`.

use bevy::prelude::*;

/// Gamepad input per DefaultInput.ini.
pub struct GamepadPlugin;

impl Plugin for GamepadPlugin {
    fn build(&self, _app: &mut App) {}
}
