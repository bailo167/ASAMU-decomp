//! Sound decoding against the user's own installed game (read-only).
//!
//! Reads `ASAMU_ORIGINAL_DIR` (the folder containing
//! `A Story About My Uncle.app`) or the default macOS Steam location and
//! skips cleanly when the data is absent. Only counts and structural facts
//! are asserted; nothing is copied or written, and no subtitle text or audio
//! appears here.
//!
//! This is the acceptance test for `docs/reverse-engineering/AUDIO.md`.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use std::collections::{BTreeMap, BTreeSet};

use asamu_ue3::PackageSet;
use asamu_ue3::flags;
use asamu_ue3::level::ParamValue;
use asamu_ue3::sound::{
    CueGraph, NodeKind, PayloadFormat, SoundCoverage, SoundDecoder, SoundKind, is_localized_package,
};

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

/// Package files (cooked folder, then Maps), skipping shader caches and
/// cooker data, which hold no sound objects.
fn packages(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in [dir.to_path_buf(), dir.join("Maps")] {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut v: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .map(|x| x.to_string_lossy().to_ascii_lowercase())
                        .is_some_and(|x| matches!(x.as_str(), "u" | "upk" | "asamu"))
            })
            .filter(|p| {
                let n = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                !n.contains("shadercache") && !n.starts_with("globalpersistentcookerdata")
            })
            .collect();
        v.sort();
        out.extend(v);
    }
    out
}

/// Package files with the localized ones first (map cues import their
/// waves).
fn ordered(dir: &Path) -> Vec<PathBuf> {
    let files = packages(dir);
    let is_loc = |p: &PathBuf| p.to_string_lossy().to_ascii_lowercase().contains("_loc_");
    let mut ordered: Vec<PathBuf> = files.iter().filter(|p| is_loc(p)).cloned().collect();
    ordered.extend(files.iter().filter(|p| !is_loc(p)).cloned());
    ordered
}

fn stem(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Open every package and register it with the decoder, as the converter
/// does, so shared objects resolve deterministically.
fn open_all<'a>(set: &'a PackageSet, files: &[PathBuf]) -> SoundDecoder<'a> {
    for f in files {
        set.open_file(f).unwrap();
    }
    let dec = SoundDecoder::new(set);
    for f in files {
        dec.register_package(&stem(f));
    }
    dec
}

fn coverage(dir: &Path) -> SoundCoverage {
    let set = PackageSet::new(&[dir.to_path_buf(), dir.join("Maps")]);
    let files = ordered(dir);
    let dec = open_all(&set, &files);
    let mut cov = SoundCoverage::default();
    for f in &files {
        let lp = set.open_file(f).unwrap();
        cov.add_package(&dec, &lp);
    }
    cov.finish();
    cov
}

#[test]
fn every_sound_object_decodes_exactly() {
    let dir = require_data!();
    let cov = coverage(&dir);
    assert_eq!(cov.failure_count, 0, "{:#?}", cov.failures);
    assert!(
        cov.classes
            .values()
            .all(|c| c.failed == 0 && c.exact == c.total)
    );
    let total = |k: &str| cov.classes.get(k).map_or(0, |c| c.total);
    assert_eq!(total("SoundNodeWave"), 852);
    assert_eq!(total("SoundCue"), 605);
    assert_eq!(total("SoundClass"), 48);
    assert_eq!(total("SoundMode"), 18);
    assert_eq!(total("SoundNodeAttenuation"), 244);
    assert_eq!(total("SoundNodeModulator"), 285);
    assert_eq!(total("SoundNodeRandom"), 118);
    assert_eq!(total("SoundNodeLooping"), 105);
    assert_eq!(total("SoundNodeAmbient"), 80);
    assert_eq!(cov.sound_classes, 47);
    assert_eq!(cov.sound_modes, 17);
    // The Master class's EditorData lists every sound class.
    assert_eq!(cov.sound_class_editor_entries, 48);
}

#[test]
fn every_wave_is_inline_ogg_vorbis_matching_its_properties() {
    let dir = require_data!();
    let cov = coverage(&dir);
    let w = &cov.waves;
    assert_eq!(w.decoded, 851);
    assert_eq!(w.localized, 267);
    // Only CompressedPCData holds data; the other six records are empty.
    assert_eq!(w.filled_slot_sets.len(), 1);
    assert_eq!(w.filled_slot_sets.get("CompressedPCData"), Some(&851));
    for (slot, s) in &w.slots {
        assert_eq!(s.separate_file + s.unused + s.compressed, 0, "{slot}");
        assert_eq!(s.flags.len(), 1, "{slot}");
        assert_eq!(s.flags.get("0x0"), Some(&851), "{slot}");
        if slot == "CompressedPCData" {
            assert_eq!(s.inline_filled, 851);
        } else {
            assert_eq!(s.empty, 851, "{slot}");
        }
    }
    assert_eq!(w.slots["CompressedPCData"].stored_bytes, 64_847_111);
    assert_eq!(w.payload_formats.len(), 1);
    assert_eq!(w.payload_formats.get(&PayloadFormat::OggVorbis), Some(&851));
    assert_eq!(
        (w.inline_offsets_match, w.inline_offset_mismatches),
        (851, 0)
    );
    assert_eq!(w.load_failures, 0);
    assert_eq!(counts(&w.channels), [(1, 427), (2, 424)]);
    assert_eq!(w.unique_paths, 768);
    assert_eq!((w.duplicate_copies, w.identical_duplicates), (83, 83));
    assert_eq!(w.differing_duplicates, 0);
    let tags: Vec<(&str, usize)> = w.tag_names.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    assert_eq!(
        tags,
        [
            ("Duration", 851),
            ("LocalizedSubtitles", 267),
            ("NumChannels", 851),
            ("RawPCMDataSize", 851),
            ("SampleRate", 851),
            ("SourceFilePath", 614),
            ("SourceFileTimestamp", 614),
            ("Subtitles", 161),
            ("bForceRealTimeDecompression", 75),
            ("bLoopingSound", 178),
            ("bManualWordWrap", 175),
        ]
    );

    let o = &cov.ogg;
    assert_eq!((o.parsed, o.failed), (851, 0));
    assert_eq!(o.pages, 16_640);
    assert_eq!(o.crc_mismatches + o.sequence_gaps + o.multiplexed, 0);
    assert_eq!(o.missing_bos + o.missing_eos + o.incomplete_headers, 0);
    assert_eq!(
        o.continuation_errors + o.granule_regressions + o.unterminated_packets,
        0
    );
    assert_eq!(o.valid_streams, 851);
    assert_eq!((o.channels_match, o.channel_mismatches), (851, 0));
    assert_eq!((o.rate_match, o.rate_mismatches), (851, 0));
    assert_eq!((o.pcm_size_match, o.pcm_size_mismatches), (851, 0));
    assert_eq!((o.duration_match, o.duration_mismatches), (851, 0));
    assert!(o.max_duration_error_s < 1e-5);
    assert_eq!(o.vendors.values().sum::<usize>(), 851);
    assert_eq!(o.vendors.len(), 3);
}

#[test]
fn subtitles_cover_fourteen_languages() {
    let dir = require_data!();
    let cov = coverage(&dir);
    let s = &cov.subtitles;
    assert_eq!((s.waves_with_subtitles, s.lines), (161, 519));
    assert_eq!(s.waves_with_localized, 267);
    assert_eq!(counts(&s.localized_array_lengths), [(23, 267)]);
    assert_eq!(s.slot_order_differences, 0);
    let slots: Vec<&str> = s.slot_languages.iter().map(String::as_str).collect();
    assert_eq!(
        slots,
        [
            "INT", "", "CZE", "DEU", "", "ESN", "FRA", "HUN", "ITA", "", "", "POL", "", "", "SLO",
            "", "BRA", "FIN", "NLD", "POR", "", "TUR", ""
        ]
    );
    assert_eq!(s.waves_per_language.len(), 14);
    assert!(s.waves_per_language.values().all(|&n| n == 161));
    assert_eq!(s.lines_per_language.get("INT"), Some(&519));
    assert_eq!((s.plain_equals_int, s.plain_differs_int), (267, 0));
    // Every subtitled wave lives in a localized package.
    assert_eq!(
        (s.in_localized_packages, s.outside_localized_packages),
        (161, 0)
    );
    assert_eq!(s.lines_after_duration, 0);
    assert_eq!(s.manual_word_wrap, 175);
    assert_eq!(s.mature + s.single_line + s.spoken_text + s.use_tts, 0);
}

#[test]
fn cue_graphs_and_ambient_actors_resolve() {
    let dir = require_data!();
    let cov = coverage(&dir);
    let c = &cov.cues;
    assert_eq!((c.decoded, c.graphs), (604, 604));
    assert_eq!(c.total_nodes, 1728);
    assert_eq!((c.wave_leaves, c.cross_package_leaves), (784, 198));
    assert_eq!((c.dangling, c.cycles), (0, 0), "{:?}", c.dangling_samples);
    assert_eq!(c.null_children, 76);
    assert_eq!(c.unreachable_nodes, 5);
    assert_eq!(c.without_first_node, 9);
    assert_eq!(c.editor_entries, 1707);
    assert_eq!(
        (c.editor_matches_graph, c.editor_differs, c.editor_empty),
        (504, 100, 93)
    );
    assert_eq!(c.editor_empty_by_owner.get("package"), Some(&5));
    assert_eq!((c.editor_extra_keys, c.editor_missing_nodes), (29, 0));
    assert_eq!(c.depths.keys().max(), Some(&7));
    assert_eq!(c.unresolved_sound_class, 1);
    assert!(c.issue_samples.is_empty(), "{:?}", c.issue_samples);

    let a = &cov.ambient;
    assert_eq!(a.actors.values().sum::<usize>(), 205);
    assert_eq!(a.in_level, 205);
    assert_eq!((a.with_cue, a.cue_resolved), (205, 205));
    assert_eq!(a.with_ambient_node, 82);
    assert_eq!(a.reverb_volumes, 43);
    assert!(a.issues.is_empty(), "{:?}", a.issues);
}

/// Every non-default cue graph of the install, keyed by lower-case cue
/// path (first copy kept), plus every distinct wave path with its package.
fn all_graphs(dir: &Path) -> (BTreeMap<String, CueGraph>, BTreeMap<String, String>) {
    let set = PackageSet::new(&[dir.to_path_buf(), dir.join("Maps")]);
    let files = ordered(dir);
    let dec = open_all(&set, &files);
    let mut graphs = BTreeMap::new();
    let mut waves = BTreeMap::new();
    for f in &files {
        let lp = set.open_file(f).unwrap();
        for i in 0..lp.package.exports.len() {
            let cdo = lp
                .package
                .export(i)
                .is_ok_and(|e| e.object_flags & flags::object::CLASS_DEFAULT_OBJECT != 0);
            match dec.export_kind(&lp, i) {
                Some((_, SoundKind::Cue)) if !cdo => {
                    let g = dec.cue_graph(&lp, i).unwrap();
                    graphs.entry(g.cue.path.to_ascii_lowercase()).or_insert(g);
                }
                Some((_, SoundKind::Node(k))) if k.is_wave() && !cdo => {
                    let path = lp.qualified(i).unwrap().to_ascii_lowercase();
                    waves.entry(path).or_insert_with(|| lp.name.clone());
                }
                _ => {}
            }
        }
    }
    (graphs, waves)
}

fn loops_indefinitely(n: &asamu_ue3::sound::CueNode) -> Option<bool> {
    match n.kind? {
        NodeKind::Looping => Some(!matches!(
            n.params.get("bLoopIndefinitely"),
            Some(ParamValue::Bool(false))
        )),
        NodeKind::Ambient | NodeKind::ForcedLoop => Some(true),
        _ => None,
    }
}

/// `Duration` = 10000 marks the cues that loop forever (AUDIO.md, STRONG),
/// counted over distinct cue paths.
#[test]
fn looping_cues_store_duration_10000() {
    let dir = require_data!();
    let (graphs, _) = all_graphs(&dir);
    assert_eq!(graphs.len(), 572);
    let mut table: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for g in graphs.values() {
        let flags: Vec<bool> = g.nodes.iter().filter_map(loops_indefinitely).collect();
        let looping = if flags.contains(&true) {
            "indefinite"
        } else if flags.contains(&false) {
            "finite-loop"
        } else {
            "none"
        };
        let duration = match g.cue.duration {
            None => "absent",
            Some(d) if d >= 10_000.0 => "10000",
            Some(_) => "finite",
        };
        *table.entry((looping, duration)).or_insert(0) += 1;
    }
    let expect: BTreeMap<(&str, &str), usize> = [
        (("indefinite", "10000"), 154),
        // Inline cues of the two AmbientSoundSimpleToggleable actors.
        (("indefinite", "absent"), 2),
        (("finite-loop", "finite"), 2),
        (("none", "finite"), 398),
        (("none", "absent"), 16),
    ]
    .into_iter()
    .collect();
    assert_eq!(table, expect);
}

/// Cross-package wave leaves of a map's cues resolve to the map's own
/// localized companion package whenever it holds the wave, and 75 of the
/// 260 distinct localized waves are reached by no cue graph.
#[test]
fn map_cues_resolve_to_their_companion_package() {
    let dir = require_data!();
    let (graphs, waves) = all_graphs(&dir);
    let set = PackageSet::new(&[dir.to_path_buf(), dir.join("Maps")]);
    let mut cross = 0;
    for g in graphs.values() {
        let owner = &g.cue.package;
        if is_localized_package(owner) {
            continue;
        }
        let companion = set.package(&format!("{owner}_LOC_INT"));
        for n in &g.nodes {
            let Some(pkg) = &n.package else { continue };
            if pkg.eq_ignore_ascii_case(owner) || !n.kind.is_some_and(NodeKind::is_wave) {
                continue;
            }
            cross += 1;
            if let Some(c) = &companion
                && c.export_by_qualified(&n.path).is_some()
            {
                assert_eq!(pkg, &c.name, "{} in {}", n.path, g.cue.path);
            }
        }
    }
    assert!(cross > 0);
    let reached: BTreeSet<String> = graphs
        .values()
        .flat_map(|g| g.waves.iter().map(|w| w.to_ascii_lowercase()))
        .collect();
    let localized: Vec<&String> = waves
        .iter()
        .filter(|(_, pkg)| is_localized_package(pkg))
        .map(|(p, _)| p)
        .collect();
    assert_eq!(localized.len(), 260);
    assert_eq!(
        localized.iter().filter(|p| !reached.contains(**p)).count(),
        75
    );
}

fn counts<K: Ord + Clone>(m: &std::collections::BTreeMap<K, usize>) -> Vec<(K, usize)> {
    m.iter().map(|(k, v)| (k.clone(), *v)).collect()
}
