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
//! More environment variables shape the shot (all optional, developer-only):
//!
//! - `ASAMU_VFX_SELFTEST_RANGE=MIN,MAX`: the distance of the target looked
//!   for, UU (default `600,1800`; the gun reaches 5,000);
//! - `ASAMU_VFX_SELFTEST_HOLD=SECONDS`: how long the button is held
//!   (default 3);
//! - `ASAMU_VFX_SELFTEST_TARGET=turning[,STANDOFF]`: instead of turning on
//!   the spot, the pawn is put at rest in the air `STANDOFF` UU (default
//!   1,200) from a rock that the level turns (an `InterpActor` on a wheel:
//!   an actor with a rotation rate, or attached to one), with a clear line
//!   to it, looking at it, in the frame the button goes down: a held
//!   grapple whose anchor rides a moving actor;
//! - `ASAMU_VFX_SELFTEST_PIN=1`: the pawn is kept where it was when the
//!   button went down, at rest, for as long as it is held (the pull would
//!   carry it to the anchor within a second or two and let go): the picture
//!   of a grapple held from one place for seconds.
//!
//! This is the only place in `vfx` that writes the simulation; it never
//! runs in normal play.

use asamu_core::glam as sim_glam;
use asamu_player::CollisionWorld;
use bevy::input::InputSystems;
use bevy::prelude::*;

use crate::converted::ConvertedLevel;
use crate::{PendingInput, Sim};

/// Seconds the fire button is held unless `ASAMU_VFX_SELFTEST_HOLD` says
/// otherwise.
const HOLD_SECONDS: f32 = 3.0;
/// The target distances looked for unless `ASAMU_VFX_SELFTEST_RANGE` says
/// otherwise, UU.
const RANGE_UU: (f32, f32) = (600.0, 1800.0);
/// How far from a turning rock the pawn is put unless the target option
/// says otherwise, UU.
const STANDOFF_UU: f32 = 1200.0;
/// Half sizes of the box that has to be free where the pawn is put, UU
/// (ours: a little more than the pawn's collision cylinder).
const FREE_BOX_UU: sim_glam::Vec3 = sim_glam::Vec3::new(24.0, 24.0, 48.0);

/// What the self-test fires at.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    /// Whatever acceptable surface is found by turning on the spot.
    Around,
    /// A rock the level turns, from this far away (UU).
    Turning(f32),
}

/// The options of the environment.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Options {
    fire_after: f32,
    range: (f32, f32),
    hold: f32,
    target: Target,
    pin: bool,
}

/// `MIN,MAX` as a range of distances; `None` when it is not one.
fn parse_range(text: &str) -> Option<(f32, f32)> {
    let (min, max) = text.split_once(',')?;
    let (min, max) = (
        min.trim().parse::<f32>().ok()?,
        max.trim().parse::<f32>().ok()?,
    );
    (min.is_finite() && max.is_finite() && min >= 0.0 && min < max).then_some((min, max))
}

/// `turning` or `turning,STANDOFF`; `None` for anything else.
fn parse_target(text: &str) -> Option<Target> {
    let mut parts = text.split(',').map(str::trim);
    if !parts.next()?.eq_ignore_ascii_case("turning") {
        return None;
    }
    let standoff = match parts.next() {
        Some(v) => v
            .parse::<f32>()
            .ok()
            .filter(|v| v.is_finite() && *v > 0.0)?,
        None => STANDOFF_UU,
    };
    parts.next().is_none().then_some(Target::Turning(standoff))
}

impl Options {
    /// The options from the variables' values (`get` returns a variable's
    /// value); `None` when the self-test is not asked for. A malformed
    /// optional value is ignored.
    fn read(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let fire_after = get("ASAMU_VFX_SELFTEST")?
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|v| v.is_finite() && *v >= 0.0)?;
        Some(Self {
            fire_after,
            range: get("ASAMU_VFX_SELFTEST_RANGE")
                .and_then(|v| parse_range(&v))
                .unwrap_or(RANGE_UU),
            hold: get("ASAMU_VFX_SELFTEST_HOLD")
                .and_then(|v| v.trim().parse::<f32>().ok())
                .filter(|v| v.is_finite() && *v > 0.0)
                .unwrap_or(HOLD_SECONDS),
            target: get("ASAMU_VFX_SELFTEST_TARGET")
                .and_then(|v| parse_target(&v))
                .unwrap_or(Target::Around),
            pin: get("ASAMU_VFX_SELFTEST_PIN").is_some_and(|v| v.trim() == "1"),
        })
    }
}

#[derive(Resource, Debug)]
struct SelfTest {
    options: Options,
    /// Where the pawn is kept while the button is held (`pin`).
    pinned: Option<sim_glam::Vec3>,
    settled_at: Option<f32>,
    aimed: bool,
    pressed: bool,
    released: bool,
}

/// Adds the self-test when the environment asks for it.
pub(super) fn add(app: &mut App) {
    let Some(options) = Options::read(|name| std::env::var(name).ok()) else {
        return;
    };
    info!("vfx self-test: {options:?}");
    app.insert_resource(SelfTest {
        options,
        pinned: None,
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
    let options = test.options;
    let fire_at = t0 + options.fire_after;
    if !test.aimed && now >= fire_at - 0.5 {
        test.aimed = true;
        if sim.game.player().script.gun.max_grapples <= 0 {
            sim.game.set_max_grapples(3);
        }
        if options.target == Target::Around {
            'scan: for pitch in [15.0f32, 30.0, 45.0, 5.0, 60.0, -10.0] {
                for step in 0..72 {
                    let p = sim.game.player_mut();
                    p.yaw = (step as f32 * 5.0).to_radians() - std::f32::consts::PI;
                    p.pitch = pitch.to_radians();
                    if sim.game.gun_aim().is_some_and(|a| {
                        a.acceptable && (options.range.0..options.range.1).contains(&a.distance)
                    }) {
                        info!("vfx self-test: aiming at {:?}", sim.game.gun_aim());
                        break 'scan;
                    }
                }
            }
        }
    }
    if !test.pressed && now >= fire_at {
        test.pressed = true;
        if let Target::Turning(standoff) = options.target {
            match view_of_a_turning_rock(&sim.game, standoff) {
                Some((name, eye, at)) => {
                    look_from(&mut sim.game, eye, at);
                    info!("vfx self-test: at {eye}, looking at the turning rock {name} at {at}");
                }
                None => {
                    warn!("vfx self-test: no turning rock with a clear view {standoff} uu away")
                }
            }
        }
        if options.pin {
            test.pinned = Some(sim.game.player().position);
        }
        pending.grapple_armed = true;
        mouse.press(MouseButton::Left);
        mouse.clear_just_pressed(MouseButton::Left);
    }
    if test.pressed && !test.released {
        if let Some(place) = test.pinned {
            let p = sim.game.player_mut();
            p.position = place;
            p.velocity = sim_glam::Vec3::ZERO;
            p.grounded = false;
            p.pawn.based = false;
        }
        if now >= fire_at + options.hold {
            test.released = true;
            test.pinned = None;
            mouse.release(MouseButton::Left);
            mouse.clear_just_released(MouseButton::Left);
        }
    }
}

/// The 26 directions of a cube's faces, edges and corners.
fn directions() -> Vec<sim_glam::Vec3> {
    let mut out = Vec::new();
    for x in [-1.0f32, 0.0, 1.0] {
        for y in [-1.0f32, 0.0, 1.0] {
            for z in [-1.0f32, 0.0, 1.0] {
                let d = sim_glam::Vec3::new(x, y, z);
                if d != sim_glam::Vec3::ZERO {
                    out.push(d.normalize());
                }
            }
        }
    }
    out
}

/// A free place `standoff` UU from a turning rock of the level with a clear
/// line to it: the rock's name, the eye, and where the line meets the rock.
/// The rocks are the level's `InterpActor`s that have a rotation rate or are
/// attached (through their bases) to an actor that has one.
fn view_of_a_turning_rock(
    game: &asamu_game::Game,
    standoff: f32,
) -> Option<(String, sim_glam::Vec3, sim_glam::Vec3)> {
    let map = game.scene_map()?;
    let scene = game.world().scene.as_ref()?;
    let eye_offset = game.player().view_location(game.params()) - game.player().position;
    for actor in &map.placed {
        let mut root = actor;
        for _ in 0..8 {
            match root.base.and_then(|b| map.placed_actor(b)) {
                Some(b) => root = b,
                None => break,
            }
        }
        if root.rotation_rate == [0; 3] || actor.kind != "interp_actor" {
            continue;
        }
        // The bounds of the rock's collision that blocks the gun's trace.
        let mut min = sim_glam::Vec3::splat(f32::INFINITY);
        let mut max = sim_glam::Vec3::splat(f32::NEG_INFINITY);
        for inst in &scene.dynamic {
            if inst.info.actor == Some(actor.id) && inst.info.blocks_traces && inst.enabled {
                min = min.min(inst.bounds.min);
                max = max.max(inst.bounds.max);
            }
        }
        if !(min.is_finite() && max.is_finite()) {
            continue;
        }
        let centre = (min + max) * 0.5;
        let range = (max - min).length() * 0.5 + standoff;
        for d in directions() {
            let eye = centre + d * range;
            if game.world().overlaps_box(eye - eye_offset, FREE_BOX_UU) {
                continue;
            }
            let Some(hit) = game.world().trace_line(eye, -d, range) else {
                continue;
            };
            if hit.hit.surface.actor == Some(actor.id) {
                return Some((actor.name.clone(), eye, hit.hit.position));
            }
        }
    }
    None
}

/// Puts the pawn at rest in the air with its eye at `eye`, looking at `at`.
fn look_from(game: &mut asamu_game::Game, eye: sim_glam::Vec3, at: sim_glam::Vec3) {
    let eye_offset = game.player().view_location(game.params()) - game.player().position;
    let aim = (at - eye).normalize_or_zero();
    let p = game.player_mut();
    p.position = eye - eye_offset;
    p.velocity = sim_glam::Vec3::ZERO;
    p.grounded = false;
    p.pawn.based = false;
    p.yaw = aim.y.atan2(aim.x);
    p.pitch = aim.z.clamp(-1.0, 1.0).asin();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn the_self_test_is_off_unless_asked_for() {
        assert_eq!(Options::read(env(&[])), None);
        assert_eq!(Options::read(env(&[("ASAMU_VFX_SELFTEST_PIN", "1")])), None);
        for bad in ["", "soon", "-1", "nan", "inf"] {
            assert_eq!(
                Options::read(env(&[("ASAMU_VFX_SELFTEST", bad)])),
                None,
                "{bad}"
            );
        }
        assert_eq!(
            Options::read(env(&[("ASAMU_VFX_SELFTEST", " 17 ")])),
            Some(Options {
                fire_after: 17.0,
                range: RANGE_UU,
                hold: HOLD_SECONDS,
                target: Target::Around,
                pin: false,
            })
        );
    }

    #[test]
    fn options_shape_the_shot_and_bad_ones_are_ignored() {
        let o = Options::read(env(&[
            ("ASAMU_VFX_SELFTEST", "5"),
            ("ASAMU_VFX_SELFTEST_RANGE", "3500, 4900"),
            ("ASAMU_VFX_SELFTEST_HOLD", "8.5"),
            ("ASAMU_VFX_SELFTEST_TARGET", "Turning,2500"),
            ("ASAMU_VFX_SELFTEST_PIN", "1"),
        ]))
        .unwrap();
        assert_eq!(o.range, (3500.0, 4900.0));
        assert_eq!(o.hold, 8.5);
        assert_eq!(o.target, Target::Turning(2500.0));
        assert!(o.pin);
        let o = Options::read(env(&[
            ("ASAMU_VFX_SELFTEST", "5"),
            ("ASAMU_VFX_SELFTEST_RANGE", "900,300"),
            ("ASAMU_VFX_SELFTEST_HOLD", "0"),
            ("ASAMU_VFX_SELFTEST_TARGET", "rock"),
            ("ASAMU_VFX_SELFTEST_PIN", "yes"),
        ]))
        .unwrap();
        assert_eq!(
            (o.range, o.hold, o.target, o.pin),
            (RANGE_UU, HOLD_SECONDS, Target::Around, false)
        );
        assert_eq!(parse_target("turning"), Some(Target::Turning(STANDOFF_UU)));
        for bad in ["turning,", "turning,-5", "turning,1,2", "turning,x", ""] {
            assert_eq!(parse_target(bad), None, "{bad}");
        }
        for bad in ["", "5", "a,b", "5,5", "-1,5", "1,inf"] {
            assert_eq!(parse_range(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_pawn_looks_at_what_it_is_put_in_front_of() {
        let mut game = asamu_game::Game::graybox().unwrap();
        let eye = sim_glam::Vec3::new(100.0, -50.0, 900.0);
        let at = eye + sim_glam::Vec3::new(-300.0, 300.0, 300.0);
        look_from(&mut game, eye, at);
        let p = game.player();
        assert!((p.view_location(game.params()) - eye).length() < 1e-3);
        assert_eq!(p.velocity, sim_glam::Vec3::ZERO);
        assert!(!p.grounded);
        assert!(
            (p.yaw.to_degrees() - 135.0).abs() < 1e-3,
            "{}",
            p.yaw.to_degrees()
        );
        assert!(
            (p.pitch.to_degrees() - 35.264).abs() < 1e-2,
            "{}",
            p.pitch.to_degrees()
        );
        assert_eq!(directions().len(), 26);
        // The graybox has no converted level: no turning rock to find.
        assert!(view_of_a_turning_rock(&game, STANDOFF_UU).is_none());
    }
}
