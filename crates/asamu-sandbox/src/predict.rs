//! A predicted arc of the player.
//!
//! [`predict`] steps a **copy** of the player with the game's own movement
//! model and parameters (`asamu_player::step_with`), holding one input, and
//! returns where the copy went. The game is not touched. The world is taken
//! as it is now: movers, crystals, attractor pads, checkpoints and Kismet do
//! not advance in the prediction, and a user interface must say so.
//!
//! Nothing here reimplements movement: the copy runs the same step function
//! the game runs, with the game's collision world.

use asamu_game::Game;
use asamu_player::{InputFrame, step_with};
use glam::Vec3;

/// Most ticks one [`predict`] call simulates (a bound on its cost, not a
/// gameplay value): one minute at the default tick rate.
pub const MAX_PREDICT_TICKS: usize = 3600;

/// Collision-centre positions (UU) of the next `ticks` ticks if `input` were
/// applied on every one of them, on a static copy of the current world.
///
/// `input` is applied verbatim each tick, so its edge flags (`jump_pressed`,
/// `use_pressed`) and look deltas repeat; pass a frame without them to
/// predict a held input. The arc ends early where the copy falls below the
/// level's kill height, and after [`MAX_PREDICT_TICKS`].
#[must_use]
pub fn predict(game: &Game, input: &InputFrame, ticks: usize) -> Vec<Vec3> {
    let ticks = ticks.min(MAX_PREDICT_TICKS);
    let model = game.movement_model();
    let params = game.params();
    let world = game.world();
    let dt = game.clock().dt();
    let kill_z = game.level().kill_z;
    let mut state = *game.player();
    let mut arc = Vec::with_capacity(ticks);
    for _ in 0..ticks {
        let events = step_with(&model, &mut state, input, params, world, dt);
        if events.non_finite_rejected {
            break;
        }
        arc.push(state.position);
        if state.position.z < kill_z {
            break;
        }
    }
    arc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicting_never_touches_the_game() {
        let mut game = Game::graybox().unwrap();
        game.start();
        let before = game.clone();
        let input = InputFrame {
            move_forward: 1.0,
            sprint_held: true,
            ..InputFrame::default()
        };
        let arc = predict(&game, &input, 120);
        assert_eq!(arc.len(), 120);
        assert_eq!(game.player(), before.player());
        assert_eq!(game.clock().tick(), before.clock().tick());
        assert_eq!(game.objects(), before.objects());
        assert!(predict(&game, &input, 0).is_empty());
    }

    #[test]
    fn the_arc_is_bounded_and_ends_below_the_kill_height() {
        let mut game = Game::graybox().unwrap();
        game.start();
        // Walking backwards off the start platform ends in the void.
        let back = InputFrame {
            move_forward: -1.0,
            ..InputFrame::default()
        };
        let arc = predict(&game, &back, usize::MAX);
        assert!(arc.len() < MAX_PREDICT_TICKS, "{}", arc.len());
        let kill_z = game.level().kill_z;
        assert!(arc.last().is_some_and(|p| p.z < kill_z));
        assert!(arc.iter().rev().skip(1).all(|p| p.z >= kill_z));
        // Standing still never ends: the bound applies.
        let still = predict(&game, &InputFrame::default(), usize::MAX);
        assert_eq!(still.len(), MAX_PREDICT_TICKS);
    }
}
