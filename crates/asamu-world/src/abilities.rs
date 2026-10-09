//! Ability unlock state that level data (Kismet, in the original) sets.
//!
//! In the original the grapple budget and the rocket boots are switched by
//! Kismet actions of each map (`docs/reverse-engineering/KISMET.md`,
//! `LEVELS.md`, GRAPPLE.md G-CT-2/3, ABILITIES.md A-RB-1):
//! `SeqAct_SetMaxGrapples` (`Grapples`), `SeqAct_ToggleGrapple` (a one-shot
//! latch, G-IN-4) and `SeqAct_ToggleRocketBoots` (`Enable`). The gun starts
//! every level with capacity 0 and the boots disabled (class defaults).
//!
//! [`LevelAbilities`] is what a level applies when it starts. There is no
//! Kismet runtime yet, so a level states it directly: the hand-made graybox
//! uses [`LevelAbilities::graybox_test`] (everything on, 3 grapples) — a
//! **test configuration**, not an original level's state. Which of a map's
//! Kismet actions fire at level start is per path and partly TENTATIVE
//! (LEVELS.md), so no per-map start state is derived here; the actions
//! themselves are listed in [`ORIGINAL_ABILITY_ACTIONS`] (CONFIRMED values).

use serde::{Deserialize, Serialize};

/// Ability state a level applies at its start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LevelAbilities {
    /// `SetMaxGrapples(n)` at level start (`None`: no action, the gun keeps
    /// its initial capacity 0). Negative = unlimited (32 767).
    pub max_grapples: Option<i32>,
    /// `EnableRocketBoots(enable)` at level start (`None`: no action, the
    /// boots stay disabled).
    pub rocket_boots: Option<bool>,
}

impl Default for LevelAbilities {
    /// No Kismet action at level start (the original's class defaults:
    /// capacity 0, boots disabled).
    fn default() -> Self {
        Self {
            max_grapples: None,
            rocket_boots: None,
        }
    }
}

impl LevelAbilities {
    /// The graybox test configuration: every ability available, 3 grapples
    /// (the highest capacity a shipped map sets; G-CT-3). Not an original
    /// level's state.
    #[must_use]
    pub fn graybox_test() -> Self {
        Self {
            max_grapples: Some(3),
            rocket_boots: Some(true),
        }
    }
}

/// The ability-related Kismet actions of one shipped map (structure and
/// values from the map decode; CONFIRMED in KISMET.md / GRAPPLE.md G-CT-3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KismetAbilityActions {
    /// Map package name.
    pub map: &'static str,
    /// `WorldInfo.Title` (empty for TheCore).
    pub title: &'static str,
    /// `Grapples` of every `SeqAct_SetMaxGrapples` instance (no linked
    /// variables, so the property value is what runs).
    pub set_max_grapples: &'static [i32],
    /// Number of `SeqAct_ToggleGrapple` instances (`Enable` true).
    pub toggle_grapple: u8,
    /// Number of `SeqAct_ToggleRocketBoots` instances that enable the boots.
    pub rocket_boots_enable: u8,
    /// Number of `SeqAct_ToggleRocketBoots` instances that disable them.
    pub rocket_boots_disable: u8,
}

/// The ability actions of the story maps, in story order (LEVELS.md).
pub const ORIGINAL_ABILITY_ACTIONS: &[KismetAbilityActions] = &[
    KismetAbilityActions {
        map: "AG-Workshop",
        title: "Workshop",
        set_max_grapples: &[0],
        toggle_grapple: 0,
        rocket_boots_enable: 0,
        rocket_boots_disable: 0,
    },
    KismetAbilityActions {
        map: "AG-ParadiseCave",
        title: "Sanctuary",
        set_max_grapples: &[1, 2],
        toggle_grapple: 1,
        rocket_boots_enable: 0,
        rocket_boots_disable: 1,
    },
    KismetAbilityActions {
        map: "AG-BeautifulCity",
        title: "Village",
        set_max_grapples: &[3, 2],
        toggle_grapple: 1,
        rocket_boots_enable: 0,
        rocket_boots_disable: 1,
    },
    KismetAbilityActions {
        map: "AG-Darkcave",
        title: "DarkCave",
        set_max_grapples: &[3],
        toggle_grapple: 1,
        rocket_boots_enable: 0,
        rocket_boots_disable: 1,
    },
    KismetAbilityActions {
        map: "AG-StarHaven",
        title: "StarHaven",
        set_max_grapples: &[3],
        toggle_grapple: 1,
        rocket_boots_enable: 3,
        rocket_boots_disable: 0,
    },
    KismetAbilityActions {
        map: "AG-IceCave",
        title: "IceCave",
        set_max_grapples: &[3],
        toggle_grapple: 1,
        rocket_boots_enable: 3,
        rocket_boots_disable: 1,
    },
    KismetAbilityActions {
        map: "TheCore",
        title: "",
        set_max_grapples: &[],
        toggle_grapple: 0,
        rocket_boots_enable: 0,
        rocket_boots_disable: 0,
    },
    KismetAbilityActions {
        map: "AG-Epilogue",
        title: "Epilogue",
        set_max_grapples: &[],
        toggle_grapple: 0,
        rocket_boots_enable: 0,
        rocket_boots_disable: 0,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graybox_is_a_test_configuration_and_default_is_no_action() {
        assert_eq!(
            LevelAbilities::default(),
            LevelAbilities {
                max_grapples: None,
                rocket_boots: None
            }
        );
        let g = LevelAbilities::graybox_test();
        assert_eq!(g.max_grapples, Some(3));
        assert_eq!(g.rocket_boots, Some(true));
    }

    #[test]
    fn kismet_table_matches_the_published_milestones() {
        let by_map = |m: &str| {
            ORIGINAL_ABILITY_ACTIONS
                .iter()
                .find(|a| a.map == m)
                .copied()
                .unwrap()
        };
        // The grapple first appears in ParadiseCave (first ToggleGrapple and
        // first capacity >= 1); the boots are first enabled in StarHaven.
        let first_toggle = ORIGINAL_ABILITY_ACTIONS
            .iter()
            .find(|a| a.toggle_grapple > 0)
            .unwrap();
        assert_eq!(first_toggle.map, "AG-ParadiseCave");
        let first_capacity = ORIGINAL_ABILITY_ACTIONS
            .iter()
            .find(|a| a.set_max_grapples.iter().any(|&n| n >= 1))
            .unwrap();
        assert_eq!(first_capacity.map, "AG-ParadiseCave");
        let first_boots = ORIGINAL_ABILITY_ACTIONS
            .iter()
            .find(|a| a.rocket_boots_enable > 0)
            .unwrap();
        assert_eq!(first_boots.map, "AG-StarHaven");
        assert_eq!(by_map("AG-Workshop").set_max_grapples, &[0]);
        assert_eq!(by_map("AG-IceCave").set_max_grapples, &[3]);
        // Every capacity a map sets is in 0..=3.
        assert!(
            ORIGINAL_ABILITY_ACTIONS
                .iter()
                .flat_map(|a| a.set_max_grapples)
                .all(|&n| (0..=3).contains(&n))
        );
    }
}
