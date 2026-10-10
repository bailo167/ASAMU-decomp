//! Tool API: what development tooling may do to a running [`Game`] that the
//! game itself never does.
//!
//! [`Game::set_params`] exists for the Sandbox's live tuning
//! (`asamu-sandbox`). **Classic code never calls it**: the story, the menus,
//! the parity tools and every loader construct a game with its parameter set
//! and keep it. A test in `asamu-sandbox` (`tests/boundary.rs`) scans the
//! repository to keep it that way.
//!
//! Nothing here runs unless a tool calls it: the tick, the loaders and the
//! constructors are untouched, and a game that is never handed a new set
//! behaves exactly as it did before this file existed.

use asamu_player::PlayerParams;
use asamu_player::params::ParamError;
use thiserror::Error;

use crate::Game;

/// Why [`Game::set_params`] refused a parameter set.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum SetParamsError {
    /// The set fails [`PlayerParams::validate`].
    #[error(transparent)]
    Invalid(#[from] ParamError),
    /// The set would add or remove a script-layer group, which would switch
    /// the simulation pipeline under a running pawn.
    #[error(
        "the script-layer groups (pawn, gun, boots) cannot be added or removed on a running game"
    )]
    PipelineChange,
}

impl Game {
    /// Tool API (Sandbox live tuning): replaces the simulation parameters
    /// between ticks. Validates first; on error nothing changes. Classic
    /// code never calls it.
    ///
    /// The game reads its parameters by reference at every use and keeps no
    /// value derived from them, so a swap between ticks is coherent. What it
    /// does **not** do is touch the pawn: run-time values the script layer
    /// copied from the old set when the pawn started or changed mode
    /// (ground speed, air control, jump velocity, FOV, the grapple's air
    /// speed) keep their old values and are the caller's to bring up to
    /// date. Handing the game the set it already runs writes nothing at all.
    ///
    /// # Errors
    /// [`SetParamsError::Invalid`] for a set that fails validation,
    /// [`SetParamsError::PipelineChange`] when `pawn`, `gun` or `boots`
    /// would appear or disappear.
    pub fn set_params(&mut self, params: PlayerParams) -> Result<(), SetParamsError> {
        params.validate()?;
        let current = &self.params;
        if params.pawn.is_some() != current.pawn.is_some()
            || params.gun.is_some() != current.gun.is_some()
            || params.boots.is_some() != current.boots.is_some()
        {
            return Err(SetParamsError::PipelineChange);
        }
        if params == *current {
            // The same set: nothing to do, and nothing is written.
            return Ok(());
        }
        self.params = params;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use asamu_core::Provenance;
    use asamu_player::InputFrame;
    use glam::Vec3;

    use super::*;

    /// Every bit of a game's state as text: `Debug` prints each float with
    /// its shortest round-trip form (and `-0.0` as such), so two games with
    /// the same text hold the same values.
    fn fingerprint(game: &Game) -> String {
        format!("{game:?}")
    }

    /// The Classic set with one value changed and marked as not original.
    fn tuned(edit: impl FnOnce(&mut PlayerParams)) -> PlayerParams {
        let mut params = PlayerParams::asamu_original();
        edit(&mut params);
        params
    }

    fn not_original() -> Provenance {
        Provenance::placeholder("test value, not the original's")
    }

    /// A started graybox game a few ticks in, walking forward.
    fn running_game() -> Game {
        let mut game = Game::graybox().unwrap();
        game.start();
        let forward = InputFrame {
            move_forward: 1.0,
            ..InputFrame::default()
        };
        for _ in 0..20 {
            game.tick(&forward).unwrap();
        }
        game
    }

    #[test]
    fn set_params_rejects_an_invalid_set_and_changes_nothing() {
        let mut game = running_game();
        let before = fingerprint(&game);
        let bad: [fn(&mut PlayerParams); 4] = [
            |p| p.movement.capsule_radius.value = -1.0,
            |p| p.movement.custom_gravity_scaling.value = f32::NAN,
            |p| p.movement.step_height.value = 1.0e6,
            |p| {
                if let Some(pawn) = p.pawn.as_mut() {
                    pawn.zoom_duration.value = 0.0;
                }
            },
        ];
        for (i, edit) in bad.iter().enumerate() {
            let params = tuned(edit);
            // The refusal is the validation's own error (compared by name:
            // a NaN value never equals itself).
            let ParamError::OutOfRange { name: expected, .. } = params.validate().unwrap_err();
            match game.set_params(params) {
                Err(SetParamsError::Invalid(ParamError::OutOfRange { name, .. })) => {
                    assert_eq!(name, expected, "edit {i}");
                }
                other => panic!("edit {i}: expected a validation error, got {other:?}"),
            }
            assert_eq!(fingerprint(&game), before, "edit {i} changed the game");
        }
        assert_eq!(*game.params(), PlayerParams::asamu_original());
    }

    #[test]
    fn set_params_rejects_a_pipeline_change() {
        // A Classic game cannot lose a script-layer group ...
        let mut game = running_game();
        let before = fingerprint(&game);
        let removals: [fn(&mut PlayerParams); 4] = [
            |p| p.pawn = None,
            |p| p.gun = None,
            |p| p.boots = None,
            |p| *p = PlayerParams::placeholder(),
        ];
        for (i, edit) in removals.iter().enumerate() {
            assert_eq!(
                game.set_params(tuned(edit)),
                Err(SetParamsError::PipelineChange),
                "removal {i}"
            );
            assert_eq!(fingerprint(&game), before, "removal {i} changed the game");
        }
        // ... and a placeholder game cannot gain one.
        let mut placeholder = Game::graybox_placeholder().unwrap();
        placeholder.start();
        let before = fingerprint(&placeholder);
        assert_eq!(
            placeholder.set_params(PlayerParams::asamu_original()),
            Err(SetParamsError::PipelineChange)
        );
        assert_eq!(fingerprint(&placeholder), before);
        // An invalid set is reported as invalid even when it would also
        // change the pipeline: validation comes first.
        let mut invalid = PlayerParams::placeholder();
        invalid.movement.capsule_radius.value = 0.0;
        assert!(matches!(
            game.set_params(invalid),
            Err(SetParamsError::Invalid(_))
        ));
    }

    #[test]
    fn set_params_with_the_same_set_changes_no_bit() {
        // Mid-run, recording, with the pawn in a non-trivial state.
        let mut game = running_game();
        game.start_recording();
        let jump = InputFrame {
            move_forward: 1.0,
            jump_pressed: true,
            jump_held: true,
            ..InputFrame::default()
        };
        game.tick(&jump).unwrap();
        let before = fingerprint(&game);
        assert_eq!(game.set_params(PlayerParams::asamu_original()), Ok(()));
        assert_eq!(game.set_params(game.params().clone()), Ok(()));
        assert_eq!(fingerprint(&game), before);

        // And the run that follows is the run of a game that was never
        // handed a set: same reports, same recorded bytes.
        let mut control = running_game();
        control.start_recording();
        control.tick(&jump).unwrap();
        for i in 0..240 {
            let input = InputFrame {
                move_forward: 1.0,
                jump_held: i < 10,
                sprint_held: i > 60,
                ..InputFrame::default()
            };
            assert_eq!(game.set_params(PlayerParams::asamu_original()), Ok(()));
            assert_eq!(game.tick(&input), control.tick(&input), "tick {i}");
        }
        assert_eq!(fingerprint(&game), fingerprint(&control));
        let ours = game.stop_recording().unwrap().to_jsonl_string().unwrap();
        let theirs = control.stop_recording().unwrap().to_jsonl_string().unwrap();
        assert_eq!(ours, theirs);
    }

    #[test]
    fn set_params_takes_effect_on_the_next_tick() {
        // Two identical games, both falling.
        let airborne = || {
            let mut game = running_game();
            let player = game.player_mut();
            player.position += Vec3::Z * 600.0;
            player.velocity = Vec3::ZERO;
            player.grounded = false;
            for _ in 0..3 {
                game.tick(&InputFrame::default()).unwrap();
            }
            assert!(!game.player().grounded, "still falling");
            game
        };
        let mut control = airborne();
        let mut game = airborne();
        assert_eq!(game.player(), control.player());

        // Half the pawn's gravity (a test value, labelled as not original).
        let classic_scale = PlayerParams::asamu_original()
            .movement
            .custom_gravity_scaling
            .value;
        let next = tuned(|p| {
            p.movement
                .custom_gravity_scaling
                .set(classic_scale * 0.5, not_original());
        });
        let player_before = *game.player();
        assert_eq!(game.set_params(next.clone()), Ok(()));
        assert_eq!(*game.params(), next, "the set is in place");
        assert_eq!(
            *game.player(),
            player_before,
            "swapping the set does not touch the pawn"
        );
        assert_eq!(
            game.clock().tick(),
            control.clock().tick(),
            "nor does it advance the clock"
        );

        let vz_before = control.player().velocity.z;
        control.tick(&InputFrame::default()).unwrap();
        game.tick(&InputFrame::default()).unwrap();
        let classic_gain = control.player().velocity.z - vz_before;
        let tuned_gain = game.player().velocity.z - vz_before;
        assert!(classic_gain < -1.0, "the control fell: {classic_gain}");
        assert!(
            (tuned_gain - 0.5 * classic_gain).abs() < 1.0e-3 * classic_gain.abs(),
            "the very next tick ran half the gravity: {tuned_gain} vs {classic_gain}"
        );
        // The control still runs the Classic set.
        assert_eq!(*control.params(), PlayerParams::asamu_original());
    }
}
