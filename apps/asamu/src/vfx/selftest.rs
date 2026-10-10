//! Unattended check of the grapple effects (debug; off unless the
//! environment variable `ASAMU_VFX_SELFTEST` holds a number of seconds).
//!
//! Like `--walk`, this drives the game without a player so a local
//! `--screenshot` run can show the beam, the hit decal and the speed lines:
//! once the level's assets have settled it starts the game, gives the gun a
//! grapple budget when the level has none yet, turns the player on the spot
//! until the gun's trace finds an acceptable target 600–1,800 UU away, and
//! `SECONDS` after the assets settled holds the fire button (by setting the
//! mouse button's pressed state, without a click edge, so the cursor is not
//! captured) for 3 s. Use it with `--screenshot-delay SECONDS + 0.4` (and
//! mind level intros: Kismet's cinematic mode swallows the button).
//!
//! This is the only place in `vfx` that writes the simulation; it never
//! runs in normal play.

use bevy::input::InputSystems;
use bevy::prelude::*;

use crate::converted::ConvertedLevel;
use crate::{PendingInput, Sim};

/// Seconds the fire button is held.
const HOLD_SECONDS: f32 = 3.0;

#[derive(Resource, Debug)]
struct SelfTest {
    fire_after: f32,
    settled_at: Option<f32>,
    aimed: bool,
    pressed: bool,
    released: bool,
}

/// Adds the self-test when the environment asks for it.
pub(super) fn add(app: &mut App) {
    let Some(fire_after) = std::env::var("ASAMU_VFX_SELFTEST")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
    else {
        return;
    };
    info!("vfx self-test: grapple {fire_after} s after the assets settle");
    app.insert_resource(SelfTest {
        fire_after,
        settled_at: None,
        aimed: false,
        pressed: false,
        released: false,
    })
    .add_systems(PreUpdate, drive.after(InputSystems));
}

fn drive(
    time: Res<Time>,
    level: Option<Res<ConvertedLevel>>,
    sim: Option<ResMut<Sim>>,
    pending: Option<ResMut<PendingInput>>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    mut test: ResMut<SelfTest>,
) {
    if level.as_ref().is_some_and(|l| !l.assets_settled()) {
        return;
    }
    // The same clock as `--screenshot-delay`: from the frame the assets
    // settled.
    let now = time.elapsed_secs();
    let t0 = *test.settled_at.get_or_insert(now);
    let (Some(mut sim), Some(mut pending)) = (sim, pending) else {
        return;
    };
    if sim.game.state() == asamu_game::GameState::Boot {
        sim.game.start();
    }
    let fire_at = t0 + test.fire_after;
    if !test.aimed && now >= fire_at - 0.5 {
        test.aimed = true;
        if sim.game.player().script.gun.max_grapples <= 0 {
            sim.game.set_max_grapples(3);
        }
        'scan: for pitch in [15.0f32, 30.0, 45.0, 5.0, 60.0, -10.0] {
            for step in 0..72 {
                let p = sim.game.player_mut();
                p.yaw = (step as f32 * 5.0).to_radians() - std::f32::consts::PI;
                p.pitch = pitch.to_radians();
                if sim
                    .game
                    .gun_aim()
                    .is_some_and(|a| a.acceptable && (600.0..1800.0).contains(&a.distance))
                {
                    info!("vfx self-test: aiming at {:?}", sim.game.gun_aim());
                    break 'scan;
                }
            }
        }
    }
    if !test.pressed && now >= fire_at {
        test.pressed = true;
        pending.grapple_armed = true;
        mouse.press(MouseButton::Left);
        mouse.clear_just_pressed(MouseButton::Left);
    }
    if test.pressed && !test.released && now >= fire_at + HOLD_SECONDS {
        test.released = true;
        mouse.release(MouseButton::Left);
        mouse.clear_just_released(MouseButton::Left);
    }
}
