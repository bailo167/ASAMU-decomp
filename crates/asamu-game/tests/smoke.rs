//! End-to-end smoke run on the user's converted data (ignored by default:
//! minutes of simulation; `cargo test -p asamu-game --release --test smoke --
//! --ignored`). Skips without `ASAMU_CONVERTED_DIR` (`levels`, `meshes
//! --collision`, `kismet`, `matinee`).
//!
//! - every converted map that loads on its own runs 5,000 frames with Kismet,
//!   movers and NPCs under the deterministic input script: no panic, no
//!   non-finite value;
//! - following each story map's level transition (the player teleported into
//!   the exit trigger) reaches the next story map, Workshop → ParadiseCave →
//!   BeautifulCity → Darkcave → StarHaven → IceCave (+ TheCore streamed in,
//!   credits) → Epilogue, with every touch registered by the world's touch
//!   logic, every disabled exit enabled the way play enables it (no forced
//!   toggle) and every story interaction performed through the player's
//!   story-mode fire (none sent to Kismet directly).

use std::path::PathBuf;

use asamu_game::save::ChapterId;
use asamu_game::smoke::{self, DEFAULT_SEED};

fn converted_dir() -> Option<PathBuf> {
    let d = PathBuf::from(std::env::var_os("ASAMU_CONVERTED_DIR")?);
    (d.join("kismet").is_dir() && d.join("levels").is_dir()).then_some(d)
}

#[test]
#[ignore = "runs every converted map (seconds optimized, longer unoptimized); run with --ignored"]
fn every_converted_map_survives_5000_scripted_ticks() {
    let Some(dir) = converted_dir() else {
        eprintln!("SKIP: ASAMU_CONVERTED_DIR with converted levels and Kismet not set");
        return;
    };
    let mut ran = 0;
    for map in smoke::converted_maps(&dir) {
        let Ok(sum) = smoke::run_map(&dir, &map, 5_000, DEFAULT_SEED) else {
            eprintln!("{map}: not loadable on its own");
            continue;
        };
        ran += 1;
        eprintln!("{}", sum.line());
        assert_eq!(sum.ticks, 5_000, "{map}");
        assert!(sum.problems.is_empty(), "{map}: {:?}", sum.problems);
        assert!(sum.host_errors.is_empty(), "{map}: {:?}", sum.host_errors);
        assert!(
            sum.kismet_errors.is_empty(),
            "{map}: {:?}",
            sum.kismet_errors
        );
    }
    eprintln!("{ran} maps ran");
}

#[test]
#[ignore = "loads every story map; run with --ignored"]
fn the_story_chain_reaches_the_epilogue() {
    let Some(dir) = converted_dir() else {
        eprintln!("SKIP: ASAMU_CONVERTED_DIR with converted levels and Kismet not set");
        return;
    };
    if ChapterId::ALL.iter().any(|c| {
        !dir.join("levels")
            .join(format!("{}.scene.json", c.map_name()))
            .is_file()
    }) {
        eprintln!("SKIP: not every story map is converted");
        return;
    }
    let steps: Vec<_> = smoke::follow_story_chain(&dir, 7_200)
        .into_iter()
        .map(|s| s.expect("story map loads"))
        .collect();
    for s in &steps {
        eprintln!("{s:?}");
    }
    // Map names resolve case-insensitively (BeautifulCity opens
    // `AG-DarkCave` for the file `AG-Darkcave`).
    let path: Vec<String> = steps.iter().map(|s| s.map.to_ascii_lowercase()).collect();
    assert_eq!(
        path,
        ChapterId::ALL[..6]
            .iter()
            .map(|c| c.map_name().to_ascii_lowercase())
            .collect::<Vec<_>>()
    );
    for s in &steps {
        assert!(
            s.ok(),
            "{}: reached {:?}, expected {:?}",
            s.map,
            s.reached,
            s.expected
        );
        assert!(!s.direct_touch, "{}: a touch was injected", s.map);
        assert!(
            !s.enabled_by_toggle,
            "{}: forced toggles {:?}",
            s.map, s.enabled
        );
        assert!(
            s.interactions_injected.is_empty(),
            "{}: injected interactions {:?}",
            s.map,
            s.interactions_injected
        );
    }
    // AG-Workshop's exit waits for its story interactions; StarHaven's and
    // TheCore's exits are enabled by what an earlier trigger starts.
    assert!(!steps[0].interactions_fired.is_empty(), "{:?}", steps[0]);
    assert!(!steps[4].enabled_by_touch.is_empty(), "{:?}", steps[4]);
    let ice = steps.last().expect("six steps");
    assert!(
        ice.streamed
            .iter()
            .any(|l| l.eq_ignore_ascii_case("TheCore")),
        "IceCave streams TheCore in: {ice:?}"
    );
    assert!(ice.credits, "TheCore's credits ran");
}
