//! Sticky cheats that are not parameters.
//!
//! A level sets the player's abilities (grapple budget, rocket boots)
//! through its start table or its Kismet. [`Rules`] lets a session pin them
//! instead; [`Rules::enforce`] runs before every tick because a scripted
//! level's start events overwrite abilities inside its first tick. The
//! default rules change nothing: enforcing them reads the game and writes
//! not one bit.
//!
//! Every correction goes through the calls the level's own Kismet actions
//! use (`Game::set_max_grapples`, `Game::enable_rocket_boots`) and the
//! gun's own refill; nothing here reaches into the simulation another way.

use asamu_game::Game;
use asamu_player::grapple_gun;
use serde::{Deserialize, Serialize};

/// The grapple budget.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrappleRule {
    /// Whatever the level sets.
    #[default]
    Level,
    /// This many grapples per landing (a negative number means no limit,
    /// as it does for the level's own action).
    Fixed(i32),
    /// No limit.
    Unlimited,
}

/// An ability that can be forced on or off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Switch {
    /// Whatever the level sets.
    #[default]
    Level,
    /// Forced on.
    On,
    /// Forced off.
    Off,
}

/// The session's sticky rules.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Rules {
    /// Grapple budget.
    pub grapples: GrappleRule,
    /// Rocket boots.
    pub rocket_boots: Switch,
    /// Refill the grapple budget whenever it is used up (no landing needed).
    pub auto_refill: bool,
}

impl Rules {
    /// The rules that change nothing.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Makes `game` follow the rules, writing only where it differs. Returns
    /// the number of corrections (0 for the default rules, and 0 whenever
    /// the game already follows them).
    ///
    /// A game without the script layer's gun or boots (the placeholder
    /// pipeline) has nothing to pin and is left alone.
    pub fn enforce(&self, game: &mut Game) -> u32 {
        if self.is_default() {
            return 0;
        }
        let script = game.player().script;
        if !script.started {
            return 0;
        }
        let mut corrections = 0;

        if script.gun.spawned {
            // The capacity the level's own action would store for `n`.
            let wanted = match self.grapples {
                GrappleRule::Level => None,
                GrappleRule::Fixed(n) if n >= 0 => Some((n, n)),
                GrappleRule::Fixed(_) | GrappleRule::Unlimited => {
                    Some((-1, grapple_gun::UNLIMITED_GRAPPLES))
                }
            };
            if let Some((argument, capacity)) = wanted
                && script.gun.max_grapples != capacity
            {
                game.set_max_grapples(argument);
                corrections += 1;
            }
            // Used up: nothing left although the pawn may grapple at all.
            let gun = game.player().script.gun;
            if self.auto_refill && gun.times_grappled > 0 && gun.grapples_left() == 0 {
                grapple_gun::reset_grapple_amount(game.player_mut());
                corrections += 1;
            }
        }

        if script.boots.spawned {
            let wanted = match self.rocket_boots {
                Switch::Level => None,
                Switch::On => Some(true),
                Switch::Off => Some(false),
            };
            if let Some(enable) = wanted
                && script.boots.enabled != enable
            {
                game.enable_rocket_boots(enable);
                corrections += 1;
            }
        }
        corrections
    }
}

#[cfg(test)]
mod tests {
    use asamu_player::InputFrame;

    use super::*;

    fn game() -> Game {
        let mut game = Game::graybox().unwrap();
        game.start();
        game
    }

    fn fingerprint(game: &Game) -> String {
        format!("{game:?}")
    }

    #[test]
    fn the_default_rules_write_nothing() {
        let mut game = game();
        // Put the game in a state every rule would have something to say
        // about.
        game.set_max_grapples(1);
        game.player_mut().script.gun.times_grappled = 1;
        game.enable_rocket_boots(false);
        let before = fingerprint(&game);
        let rules = Rules::default();
        assert!(rules.is_default());
        for _ in 0..3 {
            assert_eq!(rules.enforce(&mut game), 0);
        }
        assert_eq!(fingerprint(&game), before);
    }

    #[test]
    fn rules_correct_once_and_then_leave_the_game_alone() {
        let mut game = game();
        let rules = Rules {
            grapples: GrappleRule::Fixed(2),
            rocket_boots: Switch::On,
            auto_refill: false,
        };
        assert!(!rules.is_default());
        game.set_max_grapples(0);
        game.enable_rocket_boots(false);
        assert_eq!(rules.enforce(&mut game), 2);
        assert_eq!(game.player().script.gun.max_grapples, 2);
        assert!(game.player().script.boots.enabled);
        // Already followed: no write.
        let settled = fingerprint(&game);
        assert_eq!(rules.enforce(&mut game), 0);
        assert_eq!(fingerprint(&game), settled);

        // The same result as the level's own actions.
        let mut direct = self::game();
        direct.set_max_grapples(2);
        direct.enable_rocket_boots(true);
        assert_eq!(game.player(), direct.player());
    }

    #[test]
    fn unlimited_and_negative_budgets_are_the_levels_unlimited() {
        for rule in [
            GrappleRule::Unlimited,
            GrappleRule::Fixed(-1),
            GrappleRule::Fixed(-7),
        ] {
            let mut game = game();
            let rules = Rules {
                grapples: rule,
                ..Rules::default()
            };
            assert_eq!(rules.enforce(&mut game), 1, "{rule:?}");
            let mut direct = self::game();
            direct.set_max_grapples(-1);
            assert_eq!(game.player(), direct.player(), "{rule:?}");
            assert_eq!(rules.enforce(&mut game), 0, "{rule:?}");
        }
    }

    #[test]
    fn boots_can_be_forced_off_and_the_level_rule_follows_the_level() {
        let mut game = game();
        game.enable_rocket_boots(true);
        let off = Rules {
            rocket_boots: Switch::Off,
            ..Rules::default()
        };
        assert_eq!(off.enforce(&mut game), 1);
        assert!(!game.player().script.boots.enabled);
        // `Level` never overrides what the level (here: the test) set.
        game.enable_rocket_boots(true);
        let level = Rules {
            grapples: GrappleRule::Fixed(3),
            rocket_boots: Switch::Level,
            auto_refill: false,
        };
        level.enforce(&mut game);
        assert!(game.player().script.boots.enabled);
    }

    #[test]
    fn auto_refill_refills_only_a_used_up_budget() {
        let rules = Rules {
            auto_refill: true,
            ..Rules::default()
        };
        // Budget left: nothing to do.
        let mut game = game();
        game.set_max_grapples(2);
        game.player_mut().script.gun.times_grappled = 1;
        assert_eq!(rules.enforce(&mut game), 0);
        assert_eq!(game.player().script.gun.times_grappled, 1);
        // Used up: refilled, exactly as the gun's own refill does.
        game.player_mut().script.gun.times_grappled = 2;
        let mut direct = game.clone();
        assert_eq!(rules.enforce(&mut game), 1);
        grapple_gun::reset_grapple_amount(direct.player_mut());
        assert_eq!(game.player(), direct.player());
        assert_eq!(game.player().script.gun.grapples_left(), 2);
        assert_eq!(rules.enforce(&mut game), 0);
        // A level that allows no grapple at all stays that way.
        let mut none = self::game();
        none.set_max_grapples(0);
        assert_eq!(rules.enforce(&mut none), 0);
        assert_eq!(none.player().script.gun.max_grapples, 0);
    }

    #[test]
    fn a_game_without_the_script_layer_is_left_alone() {
        let mut game = Game::graybox_placeholder().unwrap();
        game.start();
        game.tick(&InputFrame::default()).unwrap();
        let before = fingerprint(&game);
        let rules = Rules {
            grapples: GrappleRule::Unlimited,
            rocket_boots: Switch::On,
            auto_refill: true,
        };
        assert_eq!(rules.enforce(&mut game), 0);
        assert_eq!(fingerprint(&game), before);
    }

    #[test]
    fn rules_have_the_documented_file_shape() {
        let rules = Rules {
            grapples: GrappleRule::Fixed(2),
            rocket_boots: Switch::Off,
            auto_refill: true,
        };
        let json = serde_json::to_string(&rules).unwrap();
        assert_eq!(
            json,
            r#"{"grapples":{"fixed":2},"rocket_boots":"off","auto_refill":true}"#
        );
        assert_eq!(serde_json::from_str::<Rules>(&json).unwrap(), rules);
        // Missing fields are the defaults; unknown ones are refused.
        assert_eq!(
            serde_json::from_str::<Rules>("{}").unwrap(),
            Rules::default()
        );
        assert_eq!(
            serde_json::from_str::<Rules>(r#"{"grapples":"unlimited"}"#).unwrap(),
            Rules {
                grapples: GrappleRule::Unlimited,
                ..Rules::default()
            }
        );
        assert!(serde_json::from_str::<Rules>(r#"{"god_mode":true}"#).is_err());
    }
}
