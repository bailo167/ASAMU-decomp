//! A Story About My Uncle — Rust/Bevy engine recreation.
//!
//! This is the executable shell. It does not load original game data yet.

use bevy::prelude::*;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "ASAMU-decomp (pre-alpha)".into(),
                ..default()
            }),
            ..default()
        }))
        .add_systems(Startup, setup)
        .run();
}

fn setup(mut commands: Commands) {
    commands.spawn(Camera3d::default());
}
