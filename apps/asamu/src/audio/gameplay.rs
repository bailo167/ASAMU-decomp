//! Gameplay sound events: the player simulation's events → the cues the
//! original plays for them (`asamu_assets::audio::gameplay_cues`, from the
//! class defaults), observed once per fixed tick.
//!
//! Which moment plays which sound comes from local reading of the ASAMU
//! script (STRONG; described in `docs/reverse-engineering/AUDIO.md`, "Audio
//! runtime"). Sounds the pawn plays itself are not spatialised (the engine
//! turns spatialisation off for the view target); sounds of the actors
//! attached to the pawn (grapple gun, power glove, rocket boots, the wind
//! component) are heard from the pawn's location.

use std::collections::BTreeMap;

use asamu_assets::audio::{
    AudioEngine, AudioLibrary, InstanceId, MaterialSounds, PlayParams, footstep_due,
    gameplay_cues as cues,
};
use asamu_core::glam::Vec3;
use asamu_game::Game;
use asamu_player::{BootsEvent, FireOutcome, PawnStateName, PowerJumpEvent, PowerJumpStateName};

/// Seconds the pawn's `Release` state waits before it hands over to
/// `FallingState` (and the wind) when still airborne and not grappling.
/// The simulation does not model that cosmetic hand-over; this emulates
/// it. STRONG for the value (script literal), TENTATIVE for the emulation.
const RELEASE_TO_FALLING_DELAY: f32 = 1.0;

/// Per-tick observer of one [`Game`].
#[derive(Debug, Default)]
pub struct GameplayAudio {
    last_tick: Option<u64>,
    prev_bob: f32,
    prev_pawn_state: Option<PawnStateName>,
    prev_power_state: Option<PowerJumpStateName>,
    prev_dying: bool,
    prev_respawns: u32,
    prev_crystal_charged: BTreeMap<u32, bool>,
    materials: MaterialSounds,
    beam: Option<InstanceId>,
    wind: Option<InstanceId>,
    wind_timer: f32,
    charge: Option<InstanceId>,
    static_loop: Option<InstanceId>,
    blast: Option<InstanceId>,
    release_timer: Option<f32>,
    death_reset: Option<f32>,
}

/// A sound of the pawn itself (not spatialised).
fn pawn_sound(engine: &mut AudioEngine, lib: &AudioLibrary, cue: &str, listener: Vec3) {
    engine.play(lib, cue, PlayParams::two_d(), listener);
}

/// A sound of an actor attached to the pawn (heard from the pawn).
fn attached_sound(
    engine: &mut AudioEngine,
    lib: &AudioLibrary,
    cue: &str,
    listener: Vec3,
) -> Option<InstanceId> {
    let id = engine.play(lib, cue, PlayParams::at(engine.player_location()), listener)?;
    engine.follow_player(id);
    Some(id)
}

/// `FadeIn` of a component attached to the pawn.
fn attached_fade_in(
    engine: &mut AudioEngine,
    lib: &AudioLibrary,
    existing: Option<InstanceId>,
    cue: &str,
    fade: (f32, f32),
    listener: Vec3,
) -> Option<InstanceId> {
    let params = PlayParams::at(engine.player_location());
    let id = engine.fade_in(lib, existing, cue, params, fade, listener)?;
    engine.follow_player(id);
    Some(id)
}

/// A sound heard from a world position (another actor).
fn world_sound(engine: &mut AudioEngine, lib: &AudioLibrary, cue: &str, at: Vec3, listener: Vec3) {
    let mut p = PlayParams::at(at);
    p.check_audible = true;
    engine.play(lib, cue, p, listener);
}

fn fade_out(engine: &mut AudioEngine, id: Option<InstanceId>, duration: f32) {
    if let Some(id) = id {
        engine.fade_out(id, duration, 0.0);
    }
}

impl GameplayAudio {
    /// Observes the game after a fixed tick and plays the sounds of what
    /// happened. Repeated calls for the same tick do nothing.
    pub fn observe(
        &mut self,
        game: &Game,
        engine: &mut AudioEngine,
        lib: &AudioLibrary,
        listener: Vec3,
    ) {
        let player = game.player();
        engine.set_player_location(player.position);
        let tick = game.clock().tick();
        if self.last_tick == Some(tick) {
            return;
        }
        let first = self.last_tick.is_none();
        self.last_tick = Some(tick);
        let level = game.level();
        let crystals: BTreeMap<u32, bool> = level
            .crystals
            .iter()
            .map(|c| (c.id, game.objects().crystal_charged(level, c.id)))
            .collect();
        let script = &player.script;
        if first {
            self.prev_bob = script.bob_time;
            self.prev_pawn_state = Some(script.code.state);
            self.prev_power_state = Some(script.power_jump.state);
            self.prev_dying = game.is_dying();
            self.prev_respawns = game.respawn_count();
            self.prev_crystal_charged = crystals;
            return;
        }
        let dt = game.clock().dt();
        let ev = *game.last_events();

        // Grapple gun (GrappleGun.ProcessInstantHit / release).
        if ev.gun.fire == Some(FireOutcome::Failed) {
            attached_sound(engine, lib, cues::GRAPPLE_FAIL, listener);
        }
        if let Some(a) = ev.gun.attached {
            world_sound(engine, lib, cues::GRAPPLE_DECAL, a.anchor, listener);
            attached_sound(engine, lib, cues::GRAPPLE_START, listener);
            self.beam = attached_fade_in(
                engine,
                lib,
                self.beam,
                cues::GRAPPLE_BEAM,
                (cues::GRAPPLE_BEAM_FADE_IN, 1.0),
                listener,
            );
        }
        if ev.gun.released.is_some() {
            attached_sound(engine, lib, cues::GRAPPLE_STOP, listener);
            fade_out(engine, self.beam, cues::GRAPPLE_BEAM_FADE_OUT);
        }
        if script.gun.attached.is_some()
            && let (Some(beam), Some(gun)) = (self.beam, game.params().gun.as_ref())
        {
            let distance = player.position.distance(script.gun.grapple_location);
            engine.set_float_parameter(
                beam,
                cues::GRAPPLE_BEAM_PARAM,
                gun.max_distance.value - distance,
            );
        }

        // Crystals and glow flowers grappled (their Grappled handlers).
        for e in ev.kismet.iter() {
            if let asamu_player::SimEvent::ActorGrappled { actor } = e {
                if let Some(c) = level.crystals.iter().find(|c| c.id == actor)
                    && self
                        .prev_crystal_charged
                        .get(&actor)
                        .copied()
                        .unwrap_or(true)
                    && !crystals.get(&actor).copied().unwrap_or(true)
                {
                    attached_sound(engine, lib, cues::GRAPPLE_RECHARGED, listener);
                    world_sound(engine, lib, cues::CRYSTAL_DRAINED, c.center, listener);
                }
                if let Some(f) = level.flowers.iter().find(|f| f.id == actor) {
                    world_sound(engine, lib, cues::GLOW_FLOWER_GLOW, f.center, listener);
                }
            }
        }
        self.prev_crystal_charged = crystals;

        // Jumps (ASAMUPawn.PlayJumpingSound; the power jump plays it itself).
        let power_fired = match ev.power_jump {
            Some(PowerJumpEvent::Fired { leap, .. }) => Some(leap),
            _ => None,
        };
        if power_fired.is_some() || ev.jumped {
            if let Some(cue) = self.materials.jump(None) {
                pawn_sound(engine, lib, cue, listener);
            }
            pawn_sound(engine, lib, cues::PLAYER_JUMP_GRUNT, listener);
        }

        // Power jump (ASAMUPowerJump states).
        let power_state = script.power_jump.state;
        if power_state == PowerJumpStateName::Charging
            && self.prev_power_state != Some(PowerJumpStateName::Charging)
        {
            for id in [self.static_loop.take(), self.charge.take()]
                .into_iter()
                .flatten()
            {
                engine.stop(id);
            }
            self.charge = attached_fade_in(
                engine,
                lib,
                None,
                cues::POWER_JUMP_CHARGE,
                (cues::POWER_CHARGE_FADE_IN, 1.0),
                listener,
            );
        }
        self.prev_power_state = Some(power_state);
        match ev.power_jump {
            Some(PowerJumpEvent::Charged) => {
                attached_sound(engine, lib, cues::POWER_JUMP_LIGHT, listener);
                fade_out(engine, self.charge, cues::POWER_CHARGED_CHARGE_FADE_OUT);
                self.static_loop = attached_fade_in(
                    engine,
                    lib,
                    self.static_loop,
                    cues::POWER_JUMP_STATIC,
                    (cues::POWER_CHARGED_STATIC_FADE_IN, 1.0),
                    listener,
                );
            }
            Some(PowerJumpEvent::Fired { .. }) => {
                attached_sound(engine, lib, cues::POWER_JUMP_RELEASE, listener);
                fade_out(engine, self.static_loop, cues::POWER_FIRED_STATIC_FADE_OUT);
                if power_fired == Some(true) {
                    attached_sound(engine, lib, cues::POWER_LEAP, listener);
                }
            }
            Some(PowerJumpEvent::Canceled) => {
                fade_out(engine, self.charge, cues::POWER_CANCEL_FADE_OUT);
                fade_out(engine, self.static_loop, cues::POWER_CANCEL_FADE_OUT);
            }
            None => {}
        }

        // Landing (ASAMUPawn.PlayLandingSound).
        if let Some(l) = ev.landing
            && l.sound
        {
            if let Some(cue) = self.materials.land(None, l.hard) {
                pawn_sound(engine, lib, cue, listener);
            }
            pawn_sound(engine, lib, cues::PLAYER_LAND_GRUNT, listener);
        }

        // Rocket boots (ASAMURocketBoots).
        match ev.boots {
            Some(BootsEvent::Started) => {
                attached_sound(engine, lib, cues::BOOST_CHARGE, listener);
            }
            Some(BootsEvent::BoostBegan) => {
                // `BoostActiveSoundComponent.FadeIn(0, 1)`: reverses a
                // running fade-out, else (re)starts the blast.
                self.blast = attached_fade_in(
                    engine,
                    lib,
                    self.blast,
                    cues::BOOST_ACTIVE,
                    (0.0, 1.0),
                    listener,
                );
            }
            Some(BootsEvent::Canceled) => {
                if let Some(id) = self.blast
                    && engine.is_playing(id)
                {
                    engine.fade_out(id, cues::BOOST_CANCEL_FADE_OUT, 0.0);
                }
                attached_sound(engine, lib, cues::BOOST_INTERRUPT, listener);
            }
            Some(BootsEvent::Exhausted) => {
                attached_sound(engine, lib, cues::BOOST_EXHAUSTED, listener);
            }
            Some(BootsEvent::Finished) | None => {}
        }

        // Footsteps from the walk bob (ASAMUPawn bob update).
        let walking = player.grounded && !player.pawn.flying;
        if footstep_due(
            self.prev_bob,
            script.bob_time,
            walking,
            player.velocity.length_squared(),
        ) {
            if let Some(cue) = self.materials.footstep(None) {
                pawn_sound(engine, lib, cue, listener);
            }
            if script.sprint.active {
                pawn_sound(engine, lib, cues::SPRINTING_CLOTHES, listener);
                pawn_sound(engine, lib, cues::SPRINTING_FOOTSTEPS_THUD, listener);
            }
        }
        self.prev_bob = script.bob_time;

        // Falling wind (FallingState starts it, HasLanded stops it).
        let state = script.code.state;
        let entered = |s: PawnStateName| state == s && self.prev_pawn_state != Some(s);
        let mut start_wind = entered(PawnStateName::FallingState);
        if entered(PawnStateName::Release) {
            self.release_timer = Some(RELEASE_TO_FALLING_DELAY);
        }
        if let Some(t) = self.release_timer.as_mut() {
            *t -= dt;
            if state != PawnStateName::Release {
                self.release_timer = None;
            } else if *t <= 0.0 {
                self.release_timer = None;
                if player.velocity.z != 0.0 && script.gun.attached.is_none() {
                    start_wind = true;
                }
            }
        }
        if start_wind && !self.wind.is_some_and(|id| engine.is_playing(id)) {
            self.wind = attached_sound(engine, lib, cues::FALLING_WIND, listener);
            self.wind_timer = 0.0;
        }
        if entered(PawnStateName::HasLanded)
            && let Some(id) = self.wind.take()
        {
            engine.stop(id);
        }
        self.prev_pawn_state = Some(state);
        if let Some(id) = self.wind {
            self.wind_timer -= dt;
            if self.wind_timer <= 0.0 {
                self.wind_timer += cues::FALLING_WIND_UPDATE_INTERVAL;
                let v = player.velocity;
                engine.set_float_parameter(id, cues::FALLING_WIND_PARAM, (v.x + v.y + v.z).abs());
            }
        }

        // Death (ASAMUPawn.PlayerDied): the blackout sound and the death
        // sound mode, reset after the fade.
        let dying = game.is_dying();
        let respawns = game.respawn_count();
        let died = (dying && !self.prev_dying)
            || (respawns > self.prev_respawns && !dying && !self.prev_dying);
        if died {
            pawn_sound(engine, lib, cues::RESPAWN, listener);
            engine.set_sound_mode(lib, Some(cues::DEATH_SOUND_MODE));
            self.death_reset = Some(cues::DEATH_MODE_RESET_DELAY);
        }
        self.prev_dying = dying;
        self.prev_respawns = respawns;
        if let Some(t) = self.death_reset.as_mut() {
            *t -= dt;
            if *t <= 0.0 {
                self.death_reset = None;
                engine.set_sound_mode(lib, Some(cues::DEFAULT_SOUND_MODE));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_assets::audio::{CueDef, NodeDef, NodeKind, WaveInfo, WaveRef};
    use asamu_player::InputFrame;

    /// A library holding every gameplay cue as a one-wave cue.
    fn library() -> AudioLibrary {
        let mut lib = AudioLibrary::default();
        let mut all = vec![
            cues::PLAYER_JUMP_GRUNT,
            cues::PLAYER_LAND_GRUNT,
            cues::SPRINTING_CLOTHES,
            cues::SPRINTING_FOOTSTEPS_THUD,
            cues::RESPAWN,
            cues::FALLING_WIND,
            cues::GRAPPLE_START,
            cues::GRAPPLE_STOP,
            cues::GRAPPLE_FAIL,
            cues::GRAPPLE_RECHARGED,
            cues::GRAPPLE_DECAL,
            cues::GRAPPLE_BEAM,
            cues::POWER_JUMP_CHARGE,
            cues::POWER_JUMP_STATIC,
            cues::POWER_JUMP_RELEASE,
            cues::POWER_LEAP,
            cues::POWER_JUMP_LIGHT,
            cues::BOOST_ACTIVE,
            cues::BOOST_CHARGE,
            cues::BOOST_INTERRUPT,
            cues::BOOST_EXHAUSTED,
            cues::CRYSTAL_DRAINED,
            cues::GLOW_FLOWER_GLOW,
        ];
        for t in [
            cues::FOOTSTEP_SOUNDS,
            cues::JUMPING_SOUNDS,
            cues::LANDING_SOUNDS,
            cues::FALLING_LAND_SOUNDS,
        ] {
            all.extend(t.iter().map(|(_, c)| *c));
        }
        for path in all {
            let wave = format!("{path}.W");
            lib.add_cue(CueDef {
                path: path.to_owned(),
                sound_class: None,
                volume_multiplier: 1.0,
                pitch_multiplier: 1.0,
                duration: Some(0.3),
                max_concurrent_play_count: 0,
                first: Some(0),
                nodes: vec![NodeDef {
                    path: wave.clone(),
                    kind: NodeKind::Wave(WaveRef {
                        path: wave.clone(),
                        volume: 1.0,
                        pitch: 1.0,
                    }),
                    children: Vec::new(),
                }],
            });
            lib.add_wave(WaveInfo {
                path: wave,
                file: None,
                duration: 0.3,
                channels: 1,
                volume: 1.0,
                pitch: 1.0,
            });
        }
        lib
    }

    fn run(
        game: &mut Game,
        audio: &mut GameplayAudio,
        engine: &mut AudioEngine,
        lib: &AudioLibrary,
        input: InputFrame,
        ticks: usize,
    ) -> Vec<String> {
        let mut heard = Vec::new();
        for _ in 0..ticks {
            game.tick(&input);
            audio.observe(game, engine, lib, Vec3::ZERO);
            heard.extend(engine.playing_cues().into_iter().map(str::to_owned));
            let _ = engine.update(lib, Vec3::ZERO, game.clock().dt());
        }
        heard
    }

    #[test]
    fn walking_jumping_and_landing_play_their_cues() {
        let lib = library();
        let mut game = Game::graybox().unwrap();
        game.start();
        let mut engine = AudioEngine::new(1);
        let mut audio = GameplayAudio::default();
        // Settle on the floor.
        run(
            &mut game,
            &mut audio,
            &mut engine,
            &lib,
            InputFrame::default(),
            60,
        );
        assert!(game.player().grounded);
        // Jump: the material jump cue and the grunt.
        let jump = InputFrame {
            jump_pressed: true,
            jump_held: true,
            ..InputFrame::default()
        };
        let heard = run(&mut game, &mut audio, &mut engine, &lib, jump, 1);
        assert!(game.last_events().jumped, "the graybox pawn jumped");
        assert!(
            heard.iter().any(|c| c == "Jump.Rock.Jump_Rock_Cue"),
            "{heard:?}"
        );
        assert!(heard.iter().any(|c| c == cues::PLAYER_JUMP_GRUNT));
        // Fall back: a landing with V.z <= -500 plays the landing pair.
        let held = InputFrame {
            jump_held: true,
            ..InputFrame::default()
        };
        let mut landed_sound = false;
        let mut heard = Vec::new();
        for _ in 0..240 {
            heard.extend(run(&mut game, &mut audio, &mut engine, &lib, held, 1));
            if let Some(l) = game.last_events().landing {
                landed_sound |= l.sound;
            }
        }
        assert!(landed_sound, "a full jump lands faster than 500 UU/s");
        assert_eq!(
            heard.iter().any(|c| c == cues::PLAYER_LAND_GRUNT),
            landed_sound,
            "the grunt plays exactly when the landing has a sound"
        );
        assert_eq!(
            heard.iter().any(|c| c == "Land.Rock.Land_Rock_Cue"),
            landed_sound
        );
        // Walk forward: footsteps from the bob phase.
        let walk = InputFrame {
            move_forward: 1.0,
            ..InputFrame::default()
        };
        let heard = run(&mut game, &mut audio, &mut engine, &lib, walk, 120);
        assert!(
            heard
                .iter()
                .any(|c| c == "FootSteps.Rock.Footsteps_Rock_Cue"),
            "footsteps while walking"
        );
    }

    #[test]
    fn observing_the_same_tick_twice_plays_nothing_new() {
        let lib = library();
        let mut game = Game::graybox().unwrap();
        game.start();
        let mut engine = AudioEngine::new(1);
        let mut audio = GameplayAudio::default();
        run(
            &mut game,
            &mut audio,
            &mut engine,
            &lib,
            InputFrame::default(),
            30,
        );
        let jump = InputFrame {
            jump_pressed: true,
            ..InputFrame::default()
        };
        game.tick(&jump);
        audio.observe(&game, &mut engine, &lib, Vec3::ZERO);
        let n = engine.instance_count();
        audio.observe(&game, &mut engine, &lib, Vec3::ZERO);
        assert_eq!(engine.instance_count(), n);
    }
}
