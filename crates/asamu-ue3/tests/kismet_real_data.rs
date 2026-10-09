//! Kismet graphs of every shipped map, built from the user's own install
//! (read-only). Skips cleanly when the data is absent.
//!
//! Acceptance test for `docs/reverse-engineering/KISMET.md` and the level
//! order in `docs/reverse-engineering/LEVELS.md`: every map's graph builds
//! with zero decode failures, zero dangling links and zero unresolved
//! name-matched links, and the map transitions found in Kismet chain the
//! story maps in the order of `ASAMUGameInfo.LevelFileNames`. Only counts,
//! class names and map names are asserted; nothing is written.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use asamu_ue3::kismet::{KismetGraph, KismetSummary, build_graph_for};
use asamu_ue3::model::PackageSet;
use asamu_ue3::{Value, kismet};

const COOKED: &str = "A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac";

fn cooked_dir() -> Option<PathBuf> {
    let root = match std::env::var_os("ASAMU_ORIGINAL_DIR") {
        Some(r) => PathBuf::from(r),
        None => {
            let home = std::env::var_os("HOME")?;
            PathBuf::from(home)
                .join("Library/Application Support/Steam/steamapps/common/A Story About My Uncle")
        }
    };
    let dir = root.join(COOKED);
    dir.is_dir().then_some(dir)
}

macro_rules! require_data {
    () => {
        match cooked_dir() {
            Some(d) => d,
            None => {
                eprintln!(
                    "SKIP: original game data not found (set ASAMU_ORIGINAL_DIR to the folder \
                     containing 'A Story About My Uncle.app')"
                );
                return;
            }
        }
    };
}

fn maps(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir.join("Maps"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("asamu"))
        })
        .collect();
    v.sort();
    v
}

fn graph(dir: &Path, file: &Path) -> KismetGraph {
    // A fresh set per map keeps memory bounded.
    let set = PackageSet::new(&[dir.to_path_buf(), dir.join("Maps")]);
    let lp = set.open_file(file).unwrap();
    build_graph_for(&set, &lp)
}

/// (kismet objects, level objects, stored level links) per map.
const COUNTS: &[(&str, usize, usize, usize)] = &[
    ("AG-BeautifulCity", 581, 581, 518),
    ("AG-Darkcave", 432, 432, 407),
    ("AG-Epilogue", 80, 80, 62),
    ("AG-IceCave", 427, 427, 393),
    ("AG-ParadiseCave", 549, 549, 524),
    ("AG-StarHaven", 1074, 1051, 1088),
    ("AG-Workshop", 257, 257, 247),
    ("ASAMUEntry", 0, 0, 0),
    ("ASAMUFrontEndMap", 30, 30, 29),
    ("ASAMULegal", 5, 5, 3),
    ("Freds_place", 1, 1, 0),
    ("TheCore", 68, 68, 74),
];

/// Maps opened by `open` console commands, and levels streamed by Kismet.
const TRANSITIONS: &[(&str, &[&str], &[&str])] = &[
    (
        "AG-BeautifulCity",
        &["ASAMUFrontEndMap", "AG-DarkCave"],
        &[],
    ),
    ("AG-Darkcave", &["ASAMUFrontEndMap", "AG-StarHaven"], &[]),
    ("AG-Epilogue", &["ASAMUFrontEndMap"], &[]),
    ("AG-IceCave", &["ASAMUFrontEndMap"], &["thecore"]),
    (
        "AG-ParadiseCave",
        &["AG-BeautifulCity", "ASAMUFrontEndMap"],
        &[],
    ),
    ("AG-StarHaven", &["AG-IceCave", "ASAMUFrontEndMap"], &[]),
    ("AG-Workshop", &["AG-ParadiseCave"], &[]),
    ("ASAMUEntry", &[], &[]),
    ("ASAMUFrontEndMap", &[], &[]),
    ("ASAMULegal", &[], &[]),
    ("Freds_place", &[], &[]),
    ("TheCore", &["AG-Epilogue"], &[]),
];

#[test]
fn every_map_graph_builds_without_dangling_links() {
    let dir = require_data!();
    let files = maps(&dir);
    assert_eq!(files.len(), 12);
    let mut summaries: BTreeMap<String, KismetSummary> = BTreeMap::new();
    for f in &files {
        let g = graph(&dir, f);
        let name = g.package.clone();
        assert_eq!(g.stats.decode_failures, 0, "{name}");
        assert_eq!(g.stats.decode_warnings, 0, "{name}");
        assert_eq!(g.stats.unresolved_classes, 0, "{name}: {:?}", g.warnings);
        assert_eq!(g.stats.parent_mismatches, 0, "{name}");
        assert_eq!(g.stats.unlisted_members, 0, "{name}");
        assert!(g.dangling.is_empty(), "{name}: {:#?}", g.dangling);
        assert!(g.unresolved.is_empty(), "{name}: {:#?}", g.unresolved);
        assert!(g.warnings.is_empty(), "{name}: {:?}", g.warnings);
        // Stored links never leave their sequence; only remote events do.
        assert!(
            g.edges
                .iter()
                .filter(|e| !e.derived)
                .all(|e| !e.cross_sequence),
            "{name}"
        );
        // Every node outside the level belongs to a prefab archetype.
        assert!(
            g.nodes
                .iter()
                .all(|n| n.scope != kismet::NodeScope::Detached),
            "{name}"
        );
        summaries.insert(name, g.summary());
    }
    for &(map, objects, level, stored) in COUNTS {
        let s = &summaries[map];
        assert_eq!(
            (s.kismet_objects, s.level_objects, s.links.stored),
            (objects, level, stored),
            "{map}"
        );
    }
    for &(map, opens, streams) in TRANSITIONS {
        let s = &summaries[map];
        assert_eq!(s.map_transitions, opens, "{map}");
        assert_eq!(s.streamed_levels, streams, "{map}");
    }
    let sh = &summaries["AG-StarHaven"];
    assert_eq!(sh.sequences.prefab_instance, 10);
    assert_eq!(sh.sequences.prefab_archetype, 3);
    assert_eq!(sh.prefab_archetype_objects, 23);
    assert_eq!(sh.sequences.max_depth, 2);

    // Ability and checkpoint usage per story map:
    // (map, TriggerCheckpoint, ToggleGrapple, SetMaxGrapples, ToggleRocketBoots, NarratorLine).
    let usage: &[(&str, usize, usize, usize, usize, usize)] = &[
        ("AG-Workshop", 0, 0, 1, 0, 6),
        ("AG-ParadiseCave", 1, 1, 2, 1, 10),
        ("AG-BeautifulCity", 1, 1, 2, 1, 11),
        ("AG-Darkcave", 2, 1, 1, 1, 22),
        ("AG-StarHaven", 3, 1, 1, 3, 30),
        ("AG-IceCave", 2, 1, 1, 4, 13),
        ("TheCore", 0, 0, 0, 0, 5),
        ("AG-Epilogue", 0, 0, 0, 0, 4),
    ];
    for &(map, chk, grapple, max, rocket, narrator) in usage {
        let f = &summaries[map].features;
        assert_eq!(
            (
                f.checkpoint_triggers,
                f.grapple_toggles,
                f.max_grapple_sets,
                f.rocket_boot_toggles,
                f.narrator_lines
            ),
            (chk, grapple, max, rocket, narrator),
            "{map}"
        );
    }
}

#[test]
fn kismet_transitions_follow_the_level_table() {
    let dir = require_data!();
    // The level table in the game-info class defaults.
    let set = PackageSet::new(std::slice::from_ref(&dir));
    let d = set.inherited_defaults("asamu.ASAMUGameInfo").unwrap();
    let table: Vec<String> = d
        .values
        .iter()
        .find(|v| v.name == "LevelFileNames")
        .map(|v| match &v.value {
            Value::Array(items) => items
                .iter()
                .filter_map(|i| match i {
                    Value::Str(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        })
        .unwrap();
    assert_eq!(
        table,
        [
            "ASAMUFrontEndMap",
            "AG-Workshop",
            "AG-ParadiseCave",
            "AG-BeautifulCity",
            "AG-Darkcave",
            "AG-StarHaven",
            "AG-IceCave",
            "AG-Epilogue"
        ]
    );
    drop(set);

    // Follow the non-menu transition of each map from the first story map.
    let mut next: BTreeMap<String, String> = BTreeMap::new();
    for f in maps(&dir) {
        let s = graph(&dir, &f).summary();
        let story: Vec<String> = s
            .map_transitions
            .iter()
            .filter(|m| !m.eq_ignore_ascii_case("ASAMUFrontEndMap"))
            .cloned()
            .chain(s.streamed_levels.iter().cloned())
            .collect();
        assert!(story.len() <= 1, "{}: {story:?}", s.package);
        if let Some(t) = story.into_iter().next() {
            next.insert(s.package.to_ascii_lowercase(), t);
        }
    }
    let mut chain = vec!["AG-Workshop".to_owned()];
    while let Some(t) = next.get(&chain.last().unwrap().to_ascii_lowercase()) {
        assert!(chain.len() < 16, "transition cycle: {chain:?}");
        chain.push(t.clone());
    }
    let lower: Vec<String> = chain.iter().map(|c| c.to_ascii_lowercase()).collect();
    assert_eq!(
        lower,
        [
            "ag-workshop",
            "ag-paradisecave",
            "ag-beautifulcity",
            "ag-darkcave",
            "ag-starhaven",
            "ag-icecave",
            "thecore",
            "ag-epilogue"
        ]
    );
    // The same order as the level table, with TheCore streamed into IceCave.
    let mut expected: Vec<String> = table[1..].iter().map(|t| t.to_ascii_lowercase()).collect();
    expected.insert(6, "thecore".to_owned());
    assert_eq!(lower, expected);
}

#[test]
fn startup_package_holds_no_kismet_instances() {
    let dir = require_data!();
    let set = PackageSet::new(std::slice::from_ref(&dir));
    let lp = set.package("Startup").unwrap();
    let g = build_graph_for(&set, &lp);
    assert!(g.nodes.is_empty(), "{} nodes", g.nodes.len());
}
