//! `asamu-import audio`: `SoundNodeWave` → audio files, `SoundCue` node
//! graphs → `cues.json`, subtitles → `subtitles.json`, sound classes and
//! modes → `sound_classes.json`, ambient sound actors and reverb volumes →
//! `ambient.json`, plus `manifest.json` and a coverage report.
//!
//! Everything written is derived from the user's own copy of the game: it goes
//! to the user-local output directory only (never the repository, except a
//! git-ignored `research/` subfolder, and never the install), and must not be
//! redistributed. `--check` writes nothing: it decodes every sound object,
//! loads every audio payload, verifies every Ogg page, and prints the
//! coverage report described in `docs/reverse-engineering/AUDIO.md`.
//!
//! Output layout under `<out>/audio/`:
//!
//! ```text
//! manifest.json           wave path -> file, format, duration, channels, rate, ...
//! waves/<Outer>/<Name>.ogg Ogg Vorbis exactly as stored (CompressedPCData), written
//!                         only after its page checks pass
//! waves/<Outer>/<Name>.wav RIFF/WAVE (only for raw PCM payloads; none in the Mac cook)
//! cues.json               cue path -> cue values + node graph
//! subtitles.json          wave path -> lines (text, time) in --lang, flags, cues
//! sound_classes.json      SoundClass and SoundMode values
//! ambient.json            per map: ambient sound actors and reverb volumes
//! coverage.json           the coverage report
//! ```
//!
//! Each run rewrites the JSON files from the packages it selected (audio
//! files already present are kept unless `--force`); run without `--package`
//! / `--name` filters for the complete set.
//!
//! Localization: the shipped Mac build has audio only for `INT`
//! (`*_LOC_INT.upk`); every localized wave carries the subtitles of 14
//! languages in `LocalizedSubtitles`. `--lang` picks the `_LOC_<LANG>`
//! packages when they exist (else the `INT` ones, recorded as the audio
//! language) and the subtitle language.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use asamu_ue3::PackageSet;
use asamu_ue3::model::LoadedPackage;
use asamu_ue3::sound::{
    CueGraph, MapAudio, PayloadFormat, SLOT_RAW, SoundClassData, SoundCoverage, SoundDecoder,
    SoundKind, SoundModeData, SoundWave, SubtitleCue, WAVE_BULK_SLOTS, content_hash,
    is_localized_package, package_language, parse_ogg, parse_wav, sniff_payload, wav_file,
};
use serde::Serialize;

use crate::safety;

/// Format version of every JSON file written.
const FORMAT_VERSION: u32 = 1;

const NOTICE: &str = "Converted locally from the user's own copy of A Story About My Uncle. \
                      Copyrighted game data: keep it local, never redistribute.";

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Language: selects the `_LOC_<LANG>` packages (falling back to INT
    /// audio when absent) and the subtitle language (INT, DEU, FRA, ...).
    #[arg(long, default_value = "INT")]
    lang: String,
    /// Only packages whose file name contains this text (case-insensitive;
    /// repeatable). Localized packages of the language are always loaded so
    /// that cue graphs resolve.
    #[arg(long = "package")]
    packages: Vec<String>,
    /// Only waves whose object path contains this text (case-insensitive).
    #[arg(long)]
    name: Option<String>,
    /// Stop after writing this many audio files.
    #[arg(long)]
    limit: Option<usize>,
    /// Write the JSON files only, no audio files.
    #[arg(long)]
    no_audio: bool,
    /// Overwrite audio files that already exist (otherwise they are kept).
    #[arg(long)]
    force: bool,
    /// Write nothing: decode every sound object, load every payload, verify
    /// every Ogg stream and print the coverage report.
    #[arg(long)]
    check: bool,
    /// With --check: print the report as JSON.
    #[arg(long)]
    json: bool,
    /// Convert everything in memory but write nothing.
    #[arg(long)]
    dry_run: bool,
}

// ---------------------------------------------------------------------------
// Output documents
// ---------------------------------------------------------------------------

/// One wave in `manifest.json`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WaveEntry {
    /// Package the data was taken from.
    pub package: String,
    /// Other packages holding the same object path (not written again).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also_in: Vec<String>,
    /// Copies in other packages whose audio differs from this one.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub differs_in: Vec<String>,
    /// Audio file relative to the `audio/` folder (`None` with --no-audio or
    /// when the wave stores no playable payload).
    pub file: Option<String>,
    /// Container written (`ogg`, `wav`).
    pub format: Option<String>,
    /// Bulk slot the audio came from.
    pub source_slot: Option<String>,
    /// Bytes of audio written.
    pub bytes: usize,
    /// Sample frames per channel: the final granule position of an Ogg
    /// Vorbis stream (the exact decoded length), the frame count of a WAV.
    pub samples: Option<u64>,
    /// `Duration` in seconds.
    pub duration: Option<f32>,
    /// `NumChannels`.
    pub channels: Option<i32>,
    /// `SampleRate`.
    pub sample_rate: Option<i32>,
    /// `RawPCMDataSize`.
    pub raw_pcm_bytes: Option<i32>,
    /// `bLoopingSound`.
    pub looping: Option<bool>,
    /// `Volume`.
    pub volume: Option<f32>,
    /// `Pitch`.
    pub pitch: Option<f32>,
    /// Language of a wave from a localized package (`None` otherwise).
    pub language: Option<String>,
    /// Subtitle lines available in the selected language.
    pub subtitle_lines: usize,
}

#[derive(Debug, Serialize)]
struct Manifest<'a> {
    format: &'static str,
    version: u32,
    notice: &'static str,
    language: &'a str,
    audio_language: &'a str,
    waves: &'a BTreeMap<String, WaveEntry>,
}

#[derive(Debug, Serialize)]
struct CueEntry {
    package: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    also_in: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    differs_in: Vec<String>,
    sound_class: Option<String>,
    first_node: Option<String>,
    volume_multiplier: Option<f32>,
    pitch_multiplier: Option<f32>,
    duration: Option<f32>,
    max_concurrent_play_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    face_fx_anim_set: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    face_fx_group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    face_fx_anim: Option<String>,
    nodes: Vec<asamu_ue3::sound::CueNode>,
    waves: Vec<String>,
    max_depth: usize,
    null_children: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    dangling: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unreachable: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    issues: Vec<String>,
}

impl CueEntry {
    fn from_graph(g: CueGraph) -> CueEntry {
        CueEntry {
            package: g.cue.package,
            also_in: Vec::new(),
            differs_in: Vec::new(),
            sound_class: g.cue.sound_class,
            first_node: g.cue.first_node,
            volume_multiplier: g.cue.volume_multiplier,
            pitch_multiplier: g.cue.pitch_multiplier,
            duration: g.cue.duration,
            max_concurrent_play_count: g.cue.max_concurrent_play_count,
            face_fx_anim_set: g.cue.face_fx_anim_set,
            face_fx_group: g.cue.face_fx_group,
            face_fx_anim: g.cue.face_fx_anim,
            nodes: g.nodes,
            waves: g.waves,
            max_depth: g.max_depth,
            null_children: g.null_children,
            dangling: g.dangling,
            unreachable: g.unreachable,
            issues: g.issues,
        }
    }

    /// Hash of the graph content that must agree between copies of the
    /// same cue in different packages.
    fn content_key(&self) -> u64 {
        let mut s = format!(
            "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
            self.sound_class,
            self.first_node,
            self.volume_multiplier,
            self.pitch_multiplier,
            self.duration,
            self.max_concurrent_play_count
        );
        for n in &self.nodes {
            s.push_str(&format!(
                "|{}:{}:{:?}:{:?}:{:?}",
                n.path, n.class, n.children, n.params, n.distributions
            ));
        }
        content_hash(s.as_bytes())
    }
}

#[derive(Debug, Serialize)]
struct Cues<'a> {
    format: &'static str,
    version: u32,
    notice: &'static str,
    cues: &'a BTreeMap<String, CueEntry>,
}

/// One wave in `subtitles.json`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SubtitleEntry {
    /// Package of the wave.
    pub package: String,
    /// Language of the lines (`LanguageExt`).
    pub language: String,
    /// True when the lines come from `LocalizedSubtitles`, false for the
    /// plain `Subtitles` property.
    pub from_localized: bool,
    /// `bMature`.
    pub mature: bool,
    /// `bManualWordWrap`.
    pub manual_word_wrap: bool,
    /// `bSingleLine`.
    pub single_line: bool,
    /// Wave `Duration` in seconds.
    pub duration: Option<f32>,
    /// Lines (text and start time in seconds), as stored.
    pub lines: Vec<SubtitleCue>,
    /// Languages with lines for this wave.
    pub languages: Vec<String>,
    /// Cues whose graph reaches this wave.
    pub cues: Vec<String>,
    /// `SoundClass` names of those cues. The wave stores no speaker; the
    /// class (e.g. narrator vs. voice) is the closest stored hint.
    pub cue_sound_classes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Subtitles<'a> {
    format: &'static str,
    version: u32,
    notice: &'static str,
    language: &'a str,
    waves: &'a BTreeMap<String, SubtitleEntry>,
}

#[derive(Debug, Serialize)]
struct SoundClasses<'a> {
    format: &'static str,
    version: u32,
    notice: &'static str,
    classes: &'a [SoundClassData],
    modes: &'a [SoundModeData],
}

#[derive(Debug, Serialize)]
struct Ambient<'a> {
    format: &'static str,
    version: u32,
    notice: &'static str,
    note: &'static str,
    maps: &'a [MapAudio],
}

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct RunStats {
    written: usize,
    kept: usize,
    duplicates: usize,
    differing_duplicates: usize,
    no_payload: usize,
    failed: usize,
}

/// Validated `--lang` value (two to four ASCII letters, upper case).
fn language(arg: &str) -> Result<String> {
    let l = arg.trim().to_ascii_uppercase();
    if !(2..=4).contains(&l.len()) || !l.chars().all(|c| c.is_ascii_alphabetic()) {
        bail!("--lang must be a language extension such as INT, DEU or FRA (got {arg:?})");
    }
    Ok(l)
}

pub fn run(ctx: &crate::Ctx, args: Args) -> Result<()> {
    let lang = language(&args.lang)?;
    let (cooked, maps, install_root) = cooked_dirs(ctx)?;
    let dirs = vec![cooked.clone(), maps.clone()];
    let all = package_files(&dirs)?;
    let (localized, audio_lang) = localized_for(&all, &lang);
    if audio_lang != lang {
        eprintln!(
            "asamu-import: no _LOC_{lang} packages; using the {audio_lang} audio (subtitles stay {lang})"
        );
    }
    let selected: Vec<PathBuf> = all
        .iter()
        .filter(|p| {
            let name = file_name_lower(p);
            let loc_ok = !is_localized_package(&name) || localized.contains(p);
            let filter_ok = args.packages.is_empty()
                || args
                    .packages
                    .iter()
                    .any(|f| name.contains(&f.to_ascii_lowercase()));
            loc_ok && filter_ok
        })
        .cloned()
        .collect();
    if selected.is_empty() {
        bail!("no package matches the --package filters");
    }
    let set = PackageSet::new(&dirs);
    // Localized packages first: cue graphs in maps import their waves.
    for f in &localized {
        set.open_file(f)
            .with_context(|| format!("opening {}", f.display()))?;
    }
    let dec = SoundDecoder::new(&set);
    let ordered = order_localized_first(&selected);
    // Every package of the run is searched (in sorted order) when a
    // reference is not local, so shared objects resolve to the same package
    // in every run.
    for f in localized.iter().chain(&ordered) {
        dec.register_package(&stem_of(f));
    }

    if args.check {
        return check(&set, &dec, &ordered, args.json);
    }

    let root = if args.dry_run {
        ctx.out.join("audio")
    } else {
        prepare_out_dir(
            &ctx.out,
            ordered.first().map(PathBuf::as_path),
            Some(&install_root),
        )?
    };
    let input = ordered.first().cloned().unwrap_or_else(|| cooked.clone());

    let mut stats = RunStats::default();
    let mut waves: BTreeMap<String, WaveEntry> = BTreeMap::new();
    let mut wave_hashes: HashMap<String, u64> = HashMap::new();
    let mut wave_props: BTreeMap<String, SoundWave> = BTreeMap::new();
    let mut cues: BTreeMap<String, CueEntry> = BTreeMap::new();
    let mut cue_keys: HashMap<String, u64> = HashMap::new();
    let mut classes: Vec<SoundClassData> = Vec::new();
    let mut modes: Vec<SoundModeData> = Vec::new();
    let mut maps_audio: Vec<MapAudio> = Vec::new();
    let mut names = OutputNames::default();
    let mut audio_budget = args.limit;

    for file in &ordered {
        let lp = set
            .open_file(file)
            .with_context(|| format!("opening {}", file.display()))?;
        for index in 0..lp.package.exports.len() {
            let Some((_, kind)) = dec.export_kind(&lp, index) else {
                continue;
            };
            let result = match kind {
                SoundKind::Node(k) if k.is_wave() => convert_wave(
                    &dec,
                    &lp,
                    index,
                    &root,
                    &input,
                    &args,
                    &mut audio_budget,
                    &mut WaveSinks {
                        waves: &mut waves,
                        hashes: &mut wave_hashes,
                        props: &mut wave_props,
                        names: &mut names,
                        stats: &mut stats,
                    },
                ),
                SoundKind::Cue => {
                    if lp.package.export(index).is_ok_and(|e| {
                        e.object_flags & asamu_ue3::flags::object::CLASS_DEFAULT_OBJECT != 0
                    }) {
                        continue;
                    }
                    dec.cue_graph(&lp, index)
                        .map(|g| {
                            let path = g.cue.path.clone();
                            record_cue(&mut cues, &mut cue_keys, path, CueEntry::from_graph(g));
                        })
                        .map_err(anyhow::Error::from)
                }
                SoundKind::SoundClass => dec
                    .decode_sound_class(&lp, index)
                    .map(|c| {
                        if !is_cdo_path(&c.path) {
                            classes.push(c);
                        }
                    })
                    .map_err(anyhow::Error::from),
                SoundKind::SoundMode => dec
                    .decode_sound_mode(&lp, index)
                    .map(|m| {
                        if !is_cdo_path(&m.path) {
                            modes.push(m);
                        }
                    })
                    .map_err(anyhow::Error::from),
                SoundKind::Node(_) => Ok(()),
            };
            if let Err(e) = result {
                stats.failed += 1;
                let path = lp.qualified(index).unwrap_or_default();
                eprintln!("asamu-import: {path} ({}): {e:#}", lp.name);
            }
        }
        let audio = dec.map_audio(&lp);
        if !audio.ambient.is_empty() || !audio.reverb.is_empty() {
            maps_audio.push(audio);
        }
    }
    for c in cues.values_mut() {
        c.also_in.sort();
        c.differs_in.sort();
    }

    let subtitles = build_subtitles(&wave_props, &cues, &lang);
    for (path, entry) in &mut waves {
        entry.subtitle_lines = subtitles.get(path).map_or(0, |s| s.lines.len());
    }

    let mut cov = SoundCoverage::default();
    for file in &ordered {
        let lp = set
            .open_file(file)
            .with_context(|| format!("opening {}", file.display()))?;
        cov.add_package(&dec, &lp);
    }
    cov.finish();

    let docs: Vec<(&str, String)> = vec![
        (
            "manifest.json",
            serde_json::to_string_pretty(&Manifest {
                format: "asamu-audio-manifest",
                version: FORMAT_VERSION,
                notice: NOTICE,
                language: &lang,
                audio_language: &audio_lang,
                waves: &waves,
            })?,
        ),
        (
            "cues.json",
            serde_json::to_string_pretty(&Cues {
                format: "asamu-audio-cues",
                version: FORMAT_VERSION,
                notice: NOTICE,
                cues: &cues,
            })?,
        ),
        (
            "subtitles.json",
            serde_json::to_string_pretty(&Subtitles {
                format: "asamu-audio-subtitles",
                version: FORMAT_VERSION,
                notice: NOTICE,
                language: &lang,
                waves: &subtitles,
            })?,
        ),
        (
            "sound_classes.json",
            serde_json::to_string_pretty(&SoundClasses {
                format: "asamu-audio-sound-classes",
                version: FORMAT_VERSION,
                notice: NOTICE,
                classes: &classes,
                modes: &modes,
            })?,
        ),
        (
            "ambient.json",
            serde_json::to_string_pretty(&Ambient {
                format: "asamu-audio-ambient",
                version: FORMAT_VERSION,
                notice: NOTICE,
                note: "export_index and name join with the level scenes written by \
                       `asamu-import levels` for the same package",
                maps: &maps_audio,
            })?,
        ),
        ("coverage.json", serde_json::to_string_pretty(&cov)?),
    ];
    if args.dry_run {
        let total: usize = docs.iter().map(|(_, j)| j.len()).sum();
        println!("dry run: nothing written ({total} JSON bytes)");
    } else {
        for (name, json) in &docs {
            let target = safety::check_output_path(&root.join(name), &input, true)?;
            safety::write_output(&target, json.as_bytes(), true)?;
        }
    }
    let lines: usize = subtitles.values().map(|s| s.lines.len()).sum();
    println!(
        "audio: {} files {}, {} kept (already present), {} duplicates of an earlier package \
         ({} with different audio), {} waves without a payload, {} failed",
        stats.written,
        if args.dry_run {
            "converted (dry run)"
        } else {
            "written"
        },
        stats.kept,
        stats.duplicates,
        stats.differing_duplicates,
        stats.no_payload,
        stats.failed
    );
    println!(
        "manifest: {} waves; cues: {}; subtitles ({lang}): {} waves, {lines} lines; \
         sound classes: {}, modes: {}; maps with ambient audio: {}; output {}",
        waves.len(),
        cues.len(),
        subtitles.len(),
        classes.len(),
        modes.len(),
        maps_audio.len(),
        root.display()
    );
    println!(
        "coverage: {} sound exports, {} failures (see coverage.json or --check)",
        cov.classes.values().map(|c| c.total).sum::<usize>(),
        cov.failure_count
    );
    println!("note: converted data is copyrighted game data; keep it local, never redistribute");
    if stats.failed > 0 {
        bail!("{} sound objects failed to convert", stats.failed);
    }
    Ok(())
}

fn is_cdo_path(path: &str) -> bool {
    path.rsplit('.')
        .next()
        .is_some_and(|n| n.to_ascii_lowercase().starts_with("default__"))
}

fn record_cue(
    cues: &mut BTreeMap<String, CueEntry>,
    keys: &mut HashMap<String, u64>,
    path: String,
    e: CueEntry,
) {
    let key = e.content_key();
    if let Some(existing) = cues.get_mut(&path) {
        if !existing.also_in.contains(&e.package) {
            existing.also_in.push(e.package.clone());
        }
        if keys.get(&path).is_some_and(|k| *k != key) && !existing.differs_in.contains(&e.package) {
            existing.differs_in.push(e.package);
        }
        return;
    }
    keys.insert(path.clone(), key);
    cues.insert(path, e);
}

struct WaveSinks<'a> {
    waves: &'a mut BTreeMap<String, WaveEntry>,
    hashes: &'a mut HashMap<String, u64>,
    props: &'a mut BTreeMap<String, SoundWave>,
    names: &'a mut OutputNames,
    stats: &'a mut RunStats,
}

#[allow(clippy::too_many_arguments)]
fn convert_wave(
    dec: &SoundDecoder<'_>,
    lp: &LoadedPackage,
    index: usize,
    root: &Path,
    input: &Path,
    args: &Args,
    budget: &mut Option<usize>,
    sinks: &mut WaveSinks<'_>,
) -> Result<()> {
    let w = dec.decode_wave(lp, index)?;
    if w.is_default_object {
        return Ok(());
    }
    if let Some(filter) = &args.name
        && !w
            .path
            .to_ascii_lowercase()
            .contains(&filter.to_ascii_lowercase())
    {
        return Ok(());
    }
    let payload = lp.package.export_data(index)?;
    let audio = match w.audio_slot() {
        Some(slot) => Some((slot, w.load_slot(payload, slot)?)),
        None => None,
    };
    let hash = audio.as_ref().map_or(0, |(_, b)| content_hash(b));
    if let Some(existing) = sinks.waves.get_mut(&w.path) {
        sinks.stats.duplicates += 1;
        if !existing.also_in.contains(&lp.name) {
            existing.also_in.push(lp.name.clone());
        }
        if sinks.hashes.get(&w.path).is_some_and(|h| *h != hash) {
            sinks.stats.differing_duplicates += 1;
            existing.differs_in.push(lp.name.clone());
            eprintln!(
                "asamu-import: {} in {} differs from the copy in {} (kept the first)",
                w.path, lp.name, existing.package
            );
        }
        return Ok(());
    }
    let (file, format, bytes, source_slot, samples) = match audio {
        None => {
            sinks.stats.no_payload += 1;
            (None, None, 0, None, None)
        }
        Some((slot, data)) => {
            let (ext, out, samples) = encode_audio(&w, slot, data)?;
            let slot_name = WAVE_BULK_SLOTS.get(slot).map(|s| (*s).to_owned());
            let len = out.len();
            let stem = relative_stem(&w.path);
            sinks.names.check(&stem, &w.path)?;
            let rel = stem.with_extension(ext);
            let write = !args.no_audio && budget.is_none_or(|b| b > 0);
            let file = if write {
                match write_file(root, &rel, &out, input, args)? {
                    true => sinks.stats.written += 1,
                    false => sinks.stats.kept += 1,
                }
                if let Some(b) = budget.as_mut() {
                    *b = b.saturating_sub(1);
                }
                sinks.names.claim(&stem, &w.path);
                Some(rel_string(&rel))
            } else {
                None
            };
            (file, Some(ext.to_owned()), len, slot_name, samples)
        }
    };
    let language = is_localized_package(&lp.name)
        .then(|| package_language(&lp.name))
        .flatten();
    sinks.hashes.insert(w.path.clone(), hash);
    sinks.waves.insert(
        w.path.clone(),
        WaveEntry {
            package: lp.name.clone(),
            also_in: Vec::new(),
            differs_in: Vec::new(),
            file,
            format,
            source_slot,
            bytes,
            samples,
            duration: w.props.duration,
            channels: w.props.num_channels,
            sample_rate: w.props.sample_rate,
            raw_pcm_bytes: w.props.raw_pcm_data_size,
            looping: w.props.looping,
            volume: w.props.volume,
            pitch: w.props.pitch,
            language,
            subtitle_lines: 0,
        },
    );
    sinks.props.insert(w.path.clone(), w);
    Ok(())
}

/// The file to write for a wave's payload and its length in sample frames:
/// Ogg and RIFF payloads as stored, after checking that the container is
/// sound (an Ogg stream must pass [`OggInfo::is_valid_vorbis`]-style page
/// checks, a RIFF file must parse with a consistent format); a headerless
/// `RawData` payload as 16-bit PCM wrapped in a WAV header built from
/// `NumChannels` and `SampleRate` (TENTATIVE: no such payload exists in the
/// Mac cook, where `RawData` is always empty). Damaged payloads are refused
/// rather than written.
///
/// [`OggInfo::is_valid_vorbis`]: asamu_ue3::sound::OggInfo::is_valid_vorbis
fn encode_audio(
    w: &SoundWave,
    slot: usize,
    data: Vec<u8>,
) -> Result<(&'static str, Vec<u8>, Option<u64>)> {
    let fmt = sniff_payload(&data);
    let slot_name = WAVE_BULK_SLOTS.get(slot).copied().unwrap_or("?");
    match fmt {
        PayloadFormat::OggVorbis | PayloadFormat::OggOther => {
            let info = parse_ogg(&data)
                .with_context(|| format!("{}: Ogg stream in {slot_name}", w.path))?;
            let structural = info.crc_mismatches == 0
                && info.streams == 1
                && info.sequence_gaps == 0
                && info.continuation_errors == 0
                && info.granule_regressions == 0
                && !info.unterminated_packet
                && info.first_page_bos
                && info.last_page_eos;
            if !structural || (fmt == PayloadFormat::OggVorbis && !info.is_valid_vorbis()) {
                bail!(
                    "{}: damaged Ogg stream in {slot_name} ({} CRC mismatches, {} streams, \
                     {} sequence gaps, {} continuation errors, {} granule regressions, \
                     unterminated packet {}, BOS {}, EOS {}, Vorbis headers {})",
                    w.path,
                    info.crc_mismatches,
                    info.streams,
                    info.sequence_gaps,
                    info.continuation_errors,
                    info.granule_regressions,
                    info.unterminated_packet,
                    info.first_page_bos,
                    info.last_page_eos,
                    info.vorbis.is_some() && info.vendor.is_some() && info.setup_header
                );
            }
            let samples = info.final_granule.and_then(|g| u64::try_from(g).ok());
            return Ok(("ogg", data, samples));
        }
        PayloadFormat::RiffWave => {
            let info = parse_wav(&data)
                .with_context(|| format!("{}: RIFF/WAVE payload in {slot_name}", w.path))?;
            let frames = u64::try_from(info.frames()).ok();
            return Ok(("wav", data, frames));
        }
        PayloadFormat::Empty | PayloadFormat::Unknown => {}
    }
    if fmt == PayloadFormat::Unknown && slot == SLOT_RAW {
        let channels = w
            .props
            .num_channels
            .and_then(|c| u16::try_from(c).ok())
            .filter(|&c| c > 0);
        let rate = w
            .props
            .sample_rate
            .and_then(|r| u32::try_from(r).ok())
            .filter(|&r| r > 0);
        if let (Some(c), Some(r)) = (channels, rate) {
            let out = wav_file(&data, c, r, 16)?;
            // Self-check: the header we wrote must parse back to the same format.
            let info = parse_wav(&out)?;
            if (info.channels, info.sample_rate, info.data_len) != (c, r, data.len())
                || !info.riff_size_matches
            {
                bail!("{}: WAV header self-check failed", w.path);
            }
            let frames = u64::try_from(info.frames()).ok();
            return Ok(("wav", out, frames));
        }
    }
    bail!(
        "{}: payload in {slot_name} is not Ogg or RIFF ({fmt:?})",
        w.path
    )
}

fn build_subtitles(
    waves: &BTreeMap<String, SoundWave>,
    cues: &BTreeMap<String, CueEntry>,
    lang: &str,
) -> BTreeMap<String, SubtitleEntry> {
    let mut reverse: HashMap<String, Vec<(String, Option<String>)>> = HashMap::new();
    for (path, c) in cues {
        for w in &c.waves {
            reverse
                .entry(w.to_ascii_lowercase())
                .or_default()
                .push((path.clone(), c.sound_class.clone()));
        }
    }
    let mut out = BTreeMap::new();
    for (path, w) in waves {
        let Some(view) = w.props.subtitle_view(lang) else {
            continue;
        };
        let refs = reverse
            .get(&path.to_ascii_lowercase())
            .cloned()
            .unwrap_or_default();
        let mut classes: Vec<String> = refs.iter().filter_map(|(_, c)| c.clone()).collect();
        classes.sort();
        classes.dedup();
        let mut languages: Vec<String> = w
            .props
            .localized_subtitles
            .iter()
            .filter(|l| !l.subtitles.is_empty() && !l.language.is_empty())
            .map(|l| l.language.clone())
            .collect();
        if languages.is_empty() && !w.props.subtitles.is_empty() {
            languages.push("INT".to_owned());
        }
        out.insert(
            path.clone(),
            SubtitleEntry {
                package: w.package.clone(),
                language: view.language.to_owned(),
                from_localized: view.from_localized,
                mature: view.mature,
                manual_word_wrap: view.manual_word_wrap,
                single_line: view.single_line,
                duration: w.props.duration,
                lines: view.lines.to_vec(),
                languages,
                cues: refs.into_iter().map(|(c, _)| c).collect(),
                cue_sound_classes: classes,
            },
        );
    }
    out
}

/// `--check`: decode everything, print the report.
fn check(set: &PackageSet, dec: &SoundDecoder<'_>, files: &[PathBuf], json: bool) -> Result<()> {
    let mut cov = SoundCoverage::default();
    for file in files {
        let lp = set
            .open_file(file)
            .with_context(|| format!("opening {}", file.display()))?;
        cov.add_package(dec, &lp);
    }
    cov.finish();
    if json {
        println!("{}", serde_json::to_string_pretty(&cov)?);
    } else {
        print_report(&cov);
    }
    if cov.failure_count > 0 {
        bail!("{} sound problems found", cov.failure_count);
    }
    Ok(())
}

fn print_report(cov: &SoundCoverage) {
    println!("packages examined: {}", cov.packages);
    println!("sound classes (exports, class default objects, exact, failed):");
    for (k, c) in &cov.classes {
        println!(
            "  {k:<32} {:>5} {:>3} {:>5} {:>3}",
            c.total, c.default_objects, c.exact, c.failed
        );
    }
    let w = &cov.waves;
    println!(
        "waves: {} decoded ({} localized), {} unique paths, {} duplicate copies ({} identical, {} differing)",
        w.decoded,
        w.localized,
        w.unique_paths,
        w.duplicate_copies,
        w.identical_duplicates,
        w.differing_duplicates
    );
    println!(
        "  channels {:?}; sample rates {:?}; total duration {:.1} s; looping {}",
        w.channels, w.sample_rates, w.total_duration_s, w.looping
    );
    println!("  bulk slots (empty, inline filled, separate, unused, compressed, bytes):");
    for (k, s) in &w.slots {
        println!(
            "    {k:<22} {:>5} {:>5} {:>3} {:>3} {:>3} {:>10}",
            s.empty, s.inline_filled, s.separate_file, s.unused, s.compressed, s.stored_bytes
        );
    }
    println!(
        "  payload formats {:?}; inline offsets match {} / mismatch {}; load failures {}",
        w.payload_formats, w.inline_offsets_match, w.inline_offset_mismatches, w.load_failures
    );
    let o = &cov.ogg;
    println!(
        "ogg: {} parsed, {} failed, {} pages, {} CRC mismatches, {} sequence gaps, {} multiplexed, \
         {} missing BOS, {} missing EOS, {} incomplete headers",
        o.parsed,
        o.failed,
        o.pages,
        o.crc_mismatches,
        o.sequence_gaps,
        o.multiplexed,
        o.missing_bos,
        o.missing_eos,
        o.incomplete_headers
    );
    println!(
        "  channels match {} / {}; rate match {} / {}; RawPCMDataSize = granule*ch*2 {} / {}; \
         Duration = granule/rate {} / {} (max error {:.2e} s)",
        o.channels_match,
        o.channel_mismatches,
        o.rate_match,
        o.rate_mismatches,
        o.pcm_size_match,
        o.pcm_size_mismatches,
        o.duration_match,
        o.duration_mismatches,
        o.max_duration_error_s
    );
    println!("  encoders: {:?}", o.vendors);
    let s = &cov.subtitles;
    println!(
        "subtitles: {} waves with lines ({} lines), {} with LocalizedSubtitles (lengths {:?}), \
         plain = INT entry {} / differs {}",
        s.waves_with_subtitles,
        s.lines,
        s.waves_with_localized,
        s.localized_array_lengths,
        s.plain_equals_int,
        s.plain_differs_int
    );
    println!("  waves per language: {:?}", s.waves_per_language);
    println!("  lines per language: {:?}", s.lines_per_language);
    let c = &cov.cues;
    println!(
        "cues: {} graphs, {} nodes, {} wave leaves ({} from other packages), {} dangling, \
         {} empty inputs, {} cycles, {} unreachable nodes, {} without FirstNode",
        c.graphs,
        c.total_nodes,
        c.wave_leaves,
        c.cross_package_leaves,
        c.dangling,
        c.null_children,
        c.cycles,
        c.unreachable_nodes,
        c.without_first_node
    );
    println!("  nodes per class: {:?}", c.nodes);
    println!("  depth histogram: {:?}", c.depths);
    println!(
        "  EditorData: {} entries, {} empty maps {:?}, matches graph {} / differs {}; \
         non-empty maps: {} keys not reachable, {} reachable nodes missing",
        c.editor_entries,
        c.editor_empty,
        c.editor_empty_by_owner,
        c.editor_matches_graph,
        c.editor_differs,
        c.editor_extra_keys,
        c.editor_missing_nodes
    );
    println!(
        "  sound classes used: {:?}; unresolved {:?}",
        c.sound_classes, c.unresolved_sound_class_names
    );
    println!(
        "sound classes {} ({} EditorData entries), sound modes {}",
        cov.sound_classes, cov.sound_class_editor_entries, cov.sound_modes
    );
    let a = &cov.ambient;
    println!(
        "ambient: {:?}; in level {}; with cue {} (resolved {}); inline ambient nodes {} \
         ({} slots); reverb volumes {}",
        a.actors,
        a.in_level,
        a.with_cue,
        a.cue_resolved,
        a.with_ambient_node,
        a.ambient_slots,
        a.reverb_volumes
    );
    if !cov.failures.is_empty() {
        println!("failures ({}):", cov.failure_count);
        for f in &cov.failures {
            println!("  {f}");
        }
    }
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

fn cooked_dirs(ctx: &crate::Ctx) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let install = match &ctx.original {
        Some(dir) => asamu_locate::from_original_dir(dir)?,
        None => asamu_locate::locate()?,
    };
    Ok((install.cooked_dir, install.maps_dir, install.root))
}

fn file_name_lower(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn stem_of(p: &Path) -> String {
    p.file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Package files of `dirs` (cooked folder first, then maps), without shader
/// caches and cooker data (no sound objects).
fn package_files(dirs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else {
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
                let n = file_name_lower(p);
                !n.contains("shadercache") && !n.starts_with("globalpersistentcookerdata")
            })
            .collect();
        v.sort();
        out.extend(v);
    }
    Ok(out)
}

/// The localized packages for `lang` and the language actually used for
/// audio (`INT` when the install has no `_LOC_<lang>` packages).
fn localized_for(files: &[PathBuf], lang: &str) -> (Vec<PathBuf>, String) {
    let of = |l: &str| -> Vec<PathBuf> {
        files
            .iter()
            .filter(|p| package_language(&stem_of(p)).is_some_and(|x| x.eq_ignore_ascii_case(l)))
            .cloned()
            .collect()
    };
    let wanted = of(lang);
    if !wanted.is_empty() {
        return (wanted, lang.to_owned());
    }
    (of("INT"), "INT".to_owned())
}

fn order_localized_first(files: &[PathBuf]) -> Vec<PathBuf> {
    let (mut loc, rest): (Vec<PathBuf>, Vec<PathBuf>) = files
        .iter()
        .cloned()
        .partition(|p| is_localized_package(&stem_of(p)));
    loc.extend(rest);
    loc
}

/// Refuse `dir` (canonical) when it lies inside the game install.
fn refuse_install(dir: &Path, install_root: Option<&Path>) -> Result<()> {
    let Some(root) = install_root else {
        return Ok(());
    };
    let Ok(root) = root.canonicalize() else {
        return Ok(());
    };
    if dir.starts_with(&root) {
        bail!(
            "refusing to write inside the game install {} (choose an output directory outside it)",
            root.display()
        );
    }
    Ok(())
}

/// Validate `<out>/audio` with the safety rules before creating anything,
/// then create it.
fn prepare_out_dir(
    out: &Path,
    input: Option<&Path>,
    install_root: Option<&Path>,
) -> Result<PathBuf> {
    let root = out.join("audio");
    let mut existing = root.clone();
    while !existing.exists() {
        match existing.parent() {
            Some(p) if !p.as_os_str().is_empty() => existing = p.to_path_buf(),
            _ => {
                existing = PathBuf::from(".");
                break;
            }
        }
    }
    let input = input.unwrap_or(out);
    safety::check_output_path(&existing.join(".asamu-import-audio-probe"), input, false)
        .with_context(|| format!("refusing output directory {}", out.display()))?;
    let existing = existing.canonicalize()?;
    refuse_install(&existing, install_root)
        .with_context(|| format!("refusing output directory {}", out.display()))?;
    std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    let root = root.canonicalize()?;
    refuse_install(&root, install_root)
        .with_context(|| format!("refusing output directory {}", out.display()))?;
    safety::check_output_path(&root.join("manifest.json"), input, true)
        .with_context(|| format!("refusing output directory {}", root.display()))?;
    Ok(root)
}

/// Windows device names (never usable as a file name).
const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// A file-system-safe path component: ASCII letters, digits, `-` and `_`.
fn sanitize(component: &str) -> String {
    let s: String = component
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        "_".to_owned()
    } else if RESERVED_NAMES.iter().any(|r| r.eq_ignore_ascii_case(&s)) {
        format!("_{s}")
    } else {
        s
    }
}

/// `waves/<component>/.../<name>` (no extension) for an object path.
fn relative_stem(object_path: &str) -> PathBuf {
    let mut p = PathBuf::from("waves");
    for part in object_path.split('.') {
        p.push(sanitize(part));
    }
    p
}

fn rel_string(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Output stems claimed in one run (different object paths can sanitize to
/// the same stem or differ only in case).
#[derive(Debug, Default)]
struct OutputNames {
    claimed: HashMap<String, String>,
}

impl OutputNames {
    fn key(stem: &Path) -> String {
        rel_string(stem).to_ascii_lowercase()
    }

    fn check(&self, stem: &Path, path: &str) -> Result<()> {
        match self.claimed.get(&Self::key(stem)) {
            Some(owner) if owner != path => bail!(
                "output name {} is already used by {owner}",
                rel_string(stem)
            ),
            _ => Ok(()),
        }
    }

    fn claim(&mut self, stem: &Path, path: &str) {
        self.claimed.insert(Self::key(stem), path.to_owned());
    }
}

/// Create `root/rel_dir` one component at a time without following links.
fn ensure_dirs(root: &Path, rel_dir: &Path, input: &Path) -> Result<PathBuf> {
    let mut cur = root.to_path_buf();
    for comp in rel_dir.components() {
        let std::path::Component::Normal(name) = comp else {
            bail!(
                "refusing output path component {:?} in {}",
                comp.as_os_str(),
                rel_dir.display()
            );
        };
        let next = cur.join(name);
        let is_real_dir =
            |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_dir());
        match std::fs::symlink_metadata(&next) {
            Ok(m) if m.file_type().is_dir() => {}
            Ok(_) => bail!(
                "{} exists and is not a directory (links inside the output tree are refused)",
                next.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let checked = safety::check_output_path(&next, input, false)?;
                if let Err(e) = std::fs::create_dir(&checked)
                    && !(e.kind() == std::io::ErrorKind::AlreadyExists && is_real_dir(&checked))
                {
                    return Err(e).with_context(|| format!("creating {}", checked.display()));
                }
            }
            Err(e) => return Err(e).with_context(|| format!("checking {}", next.display())),
        }
        cur = next;
    }
    Ok(cur)
}

/// Write `data` to `root/rel` through the safety checks. Returns false when
/// the file exists and `--force` is off (it is kept). Writes nothing with
/// `--dry-run`.
fn write_file(root: &Path, rel: &Path, data: &[u8], input: &Path, args: &Args) -> Result<bool> {
    if args.dry_run {
        return Ok(true);
    }
    let Some(name) = rel.file_name() else {
        bail!("output path {} has no file name", rel.display());
    };
    let dir = ensure_dirs(root, rel.parent().unwrap_or(Path::new("")), input)?;
    let target = dir.join(name);
    if !args.force && std::fs::symlink_metadata(&target).is_ok() {
        return Ok(false);
    }
    let checked = safety::check_output_path(&target, input, args.force)?;
    safety::write_output(&checked, data, args.force)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_ue3::sound::NodeKind;

    #[test]
    fn language_validation() {
        assert_eq!(language("int").unwrap(), "INT");
        assert_eq!(language(" deu ").unwrap(), "DEU");
        assert!(language("").is_err());
        assert!(language("../x").is_err());
        assert!(language("ENGLISH").is_err());
    }

    #[test]
    fn stems_are_sanitized() {
        assert_eq!(
            rel_string(&relative_stem("Pkg.Group.Some Wave")),
            "waves/Pkg/Group/Some_Wave"
        );
        assert_eq!(rel_string(&relative_stem("a..CON")), "waves/a/_/_CON");
    }

    #[test]
    fn localized_selection_falls_back_to_int() {
        let files: Vec<PathBuf> = ["Startup.upk", "Startup_LOC_INT.upk", "Maps/A_LOC_INT.upk"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let (loc, audio) = localized_for(&files, "DEU");
        assert_eq!(audio, "INT");
        assert_eq!(loc.len(), 2);
        let (loc, audio) = localized_for(&files, "INT");
        assert_eq!(audio, "INT");
        assert_eq!(loc.len(), 2);
        let ordered = order_localized_first(&files);
        assert!(stem_of(&ordered[0]).contains("_LOC_"));
        assert_eq!(stem_of(&ordered[2]), "Startup");
    }

    #[test]
    fn cdo_paths() {
        assert!(is_cdo_path("Engine.Default__SoundCue"));
        assert!(!is_cdo_path("Pkg.MyCue"));
    }

    #[test]
    fn node_kind_names_round_trip() {
        assert!(NodeKind::Wave.is_wave());
        assert!(!NodeKind::Random.is_wave());
    }

    fn wave(path: &str, channels: i32, rate: i32) -> SoundWave {
        SoundWave {
            package: "Pkg_LOC_INT".to_owned(),
            export_index: 1,
            path: path.to_owned(),
            class_path: "Engine.SoundNodeWave".to_owned(),
            is_default_object: false,
            props: asamu_ue3::sound::WaveProps {
                num_channels: Some(channels),
                sample_rate: Some(rate),
                duration: Some(1.0),
                ..Default::default()
            },
            tagged: Vec::new(),
            native: None,
            serial_offset: 0,
            properties_end: 0,
            payload_size: 0,
        }
    }

    /// One Ogg page (`packets` each below 255 bytes) with a valid checksum.
    fn page(flags: u8, granule: i64, seq: u32, packets: &[&[u8]]) -> Vec<u8> {
        let mut p = b"OggS\0".to_vec();
        p.push(flags);
        p.extend_from_slice(&granule.to_le_bytes());
        p.extend_from_slice(&1u32.to_le_bytes());
        p.extend_from_slice(&seq.to_le_bytes());
        p.extend_from_slice(&[0; 4]);
        p.push(packets.len() as u8);
        p.extend(packets.iter().map(|k| k.len() as u8));
        for k in packets {
            p.extend_from_slice(k);
        }
        let crc = asamu_ue3::sound::ogg_crc(&p);
        p[22..26].copy_from_slice(&crc.to_le_bytes());
        p
    }

    /// A container-valid Vorbis stream of `samples` frames (filler audio).
    fn vorbis(samples: i64) -> Vec<u8> {
        let mut ident = b"\x01vorbis".to_vec();
        ident.extend_from_slice(&0u32.to_le_bytes());
        ident.push(1);
        ident.extend_from_slice(&8000u32.to_le_bytes());
        ident.extend_from_slice(&[0; 12]);
        ident.extend_from_slice(&[0xB8, 1]);
        let mut comment = b"\x03vorbis".to_vec();
        comment.extend_from_slice(&1u32.to_le_bytes());
        comment.push(b'x');
        comment.extend_from_slice(&0u32.to_le_bytes());
        comment.push(1);
        let setup = b"\x05vorbis-setup".to_vec();
        let mut s = page(0x02, 0, 0, &[&ident]);
        s.extend(page(0, 0, 1, &[&comment, &setup]));
        s.extend(page(0x04, samples, 2, &[&[0x55; 20]]));
        s
    }

    #[test]
    fn audio_encoding_by_payload() {
        let w = wave("Pkg.W", 1, 8000);
        let ogg = vorbis(12_000);
        assert_eq!(
            encode_audio(&w, asamu_ue3::sound::SLOT_COMPRESSED_PC, ogg.clone()).unwrap(),
            ("ogg", ogg.clone(), Some(12_000))
        );
        // Damaged or truncated Ogg streams are refused, not written.
        let mut bad = ogg.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(encode_audio(&w, asamu_ue3::sound::SLOT_COMPRESSED_PC, bad).is_err());
        assert!(
            encode_audio(
                &w,
                asamu_ue3::sound::SLOT_COMPRESSED_PC,
                ogg[..ogg.len() - 3].to_vec()
            )
            .is_err()
        );
        assert!(
            encode_audio(
                &w,
                asamu_ue3::sound::SLOT_COMPRESSED_PC,
                b"OggS\0\x02rest".to_vec()
            )
            .is_err()
        );
        let mut no_eos = ogg.clone();
        no_eos.truncate(ogg.len() - (27 + 1 + 20));
        assert!(encode_audio(&w, asamu_ue3::sound::SLOT_COMPRESSED_PC, no_eos).is_err());
        // Headerless PCM in RawData gets our own RIFF header.
        let (ext, wav, frames) = encode_audio(&w, SLOT_RAW, vec![1, 0, 2, 0]).unwrap();
        assert_eq!((ext, frames), ("wav", Some(2)));
        assert_eq!(&wav[..4], b"RIFF");
        let info = parse_wav(&wav).unwrap();
        assert_eq!(
            (info.channels, info.sample_rate, info.data_len),
            (1, 8000, 4)
        );
        // A stored RIFF/WAVE payload is kept as is when its format is sound.
        let (ext, same, frames) =
            encode_audio(&w, asamu_ue3::sound::SLOT_COMPRESSED_PC, wav.clone()).unwrap();
        assert_eq!((ext, frames), ("wav", Some(2)));
        assert_eq!(same, wav);
        let mut broken = wav.clone();
        broken[32] = 3; // block align
        assert!(encode_audio(&w, asamu_ue3::sound::SLOT_COMPRESSED_PC, broken).is_err());
        // Unknown bytes elsewhere, or PCM without a usable format, are refused.
        assert!(encode_audio(&w, asamu_ue3::sound::SLOT_COMPRESSED_PC, vec![1, 2, 3, 4]).is_err());
        assert!(encode_audio(&wave("Pkg.X", 0, 8000), SLOT_RAW, vec![1, 0]).is_err());
        assert!(encode_audio(&w, SLOT_RAW, vec![1, 0, 2]).is_err());
    }

    #[test]
    fn hostile_object_paths_map_to_safe_stems() {
        let nasty = [
            "",
            ".",
            "..",
            "../../etc/passwd",
            "Pkg/../../x",
            "Pkg\\..\\x",
            "C:\\Windows",
            "/abs/path",
            "nul",
            "Pkg.com1",
            "Pkg.LPT9",
            "Pkg. CON",
            "Pkg.\u{0}",
            "Pkg.\u{e9}t\u{e9}",
            "a..b...c",
            "~/.ssh/id",
        ];
        for path in nasty {
            let stem = relative_stem(path);
            let mut comps = stem.components();
            assert_eq!(
                comps.next(),
                Some(std::path::Component::Normal("waves".as_ref()))
            );
            for c in comps {
                let std::path::Component::Normal(name) = c else {
                    panic!("{path:?}: component {c:?}");
                };
                let name = name.to_str().unwrap();
                assert!(!name.is_empty(), "{path:?}");
                assert!(
                    name.chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'),
                    "{path:?} -> {name:?}"
                );
                assert!(
                    !RESERVED_NAMES.iter().any(|r| r.eq_ignore_ascii_case(name)),
                    "{path:?} -> {name:?}"
                );
            }
        }
        // Paths differing only in case (or sanitizing to the same stem) are
        // caught before a second file could overwrite the first.
        let mut names = OutputNames::default();
        let a = relative_stem("Pkg.Wave");
        names.check(&a, "Pkg.Wave").unwrap();
        names.claim(&a, "Pkg.Wave");
        names.check(&a, "Pkg.Wave").unwrap();
        assert!(names.check(&relative_stem("pkg.WAVE"), "pkg.WAVE").is_err());
        assert!(
            names
                .check(&relative_stem("Pkg.Wave?"), "Pkg.Wave?")
                .is_ok()
        );
        names.claim(&relative_stem("Pkg.Wave?"), "Pkg.Wave?");
        assert!(
            names
                .check(&relative_stem("Pkg.Wave*"), "Pkg.Wave*")
                .is_err()
        );
    }

    fn args(force: bool) -> Args {
        Args {
            lang: "INT".to_owned(),
            packages: Vec::new(),
            name: None,
            limit: None,
            no_audio: false,
            force,
            check: false,
            json: false,
            dry_run: false,
        }
    }

    #[test]
    fn writer_never_leaves_the_output_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("audio");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let input = tmp.path().join("in.upk");
        let rel = Path::new("waves/Pkg/W.ogg");

        // New file, then kept without --force, replaced with --force.
        assert!(write_file(&root, rel, b"one", &input, &args(false)).unwrap());
        assert!(!write_file(&root, rel, b"two", &input, &args(false)).unwrap());
        assert_eq!(std::fs::read(root.join(rel)).unwrap(), b"one");
        assert!(write_file(&root, rel, b"two", &input, &args(true)).unwrap());
        assert_eq!(std::fs::read(root.join(rel)).unwrap(), b"two");

        // Traversal components are refused before anything is created.
        assert!(ensure_dirs(&root, Path::new("waves/../../x"), &input).is_err());
        assert!(ensure_dirs(&root, Path::new("/abs"), &input).is_err());
        assert!(!tmp.path().join("x").exists());

        // A file where a directory is expected is refused.
        std::fs::write(root.join("waves/Blocked"), b"f").unwrap();
        assert!(
            write_file(
                &root,
                Path::new("waves/Blocked/W.ogg"),
                b"x",
                &input,
                &args(true)
            )
            .is_err()
        );

        // --dry-run writes nothing.
        let mut dry = args(true);
        dry.dry_run = true;
        assert!(write_file(&root, Path::new("waves/Dry/W.ogg"), b"x", &input, &dry).unwrap());
        assert!(!root.join("waves/Dry").exists());
    }

    #[cfg(unix)]
    #[test]
    fn writer_never_follows_links() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("audio");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let input = tmp.path().join("in.upk");
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let protected = outside.join("protected.ogg");
        std::fs::write(&protected, b"original").unwrap();

        // A symlinked directory inside the output tree is refused.
        std::fs::create_dir(root.join("waves")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("waves/Linked")).unwrap();
        assert!(
            write_file(
                &root,
                Path::new("waves/Linked/New.ogg"),
                b"x",
                &input,
                &args(true)
            )
            .is_err()
        );
        assert!(!outside.join("New.ogg").exists());

        // A symlinked file: kept untouched without --force, refused with it.
        std::fs::create_dir(root.join("waves/Pkg")).unwrap();
        std::os::unix::fs::symlink(&protected, root.join("waves/Pkg/W.ogg")).unwrap();
        let rel = Path::new("waves/Pkg/W.ogg");
        assert!(!write_file(&root, rel, b"derived", &input, &args(false)).unwrap());
        assert!(write_file(&root, rel, b"derived", &input, &args(true)).is_err());
        assert_eq!(std::fs::read(&protected).unwrap(), b"original");

        // A hard link is replaced, never written through.
        std::fs::hard_link(&protected, root.join("waves/Pkg/H.ogg")).unwrap();
        assert!(
            write_file(
                &root,
                Path::new("waves/Pkg/H.ogg"),
                b"derived",
                &input,
                &args(true)
            )
            .unwrap()
        );
        assert_eq!(std::fs::read(&protected).unwrap(), b"original");
        assert_eq!(
            std::fs::read(root.join("waves/Pkg/H.ogg")).unwrap(),
            b"derived"
        );

        // An output directory that is a link into a refused place is refused.
        let app = tmp.path().join("Game.app").join("Contents");
        std::fs::create_dir_all(&app).unwrap();
        let out = tmp.path().join("out");
        std::fs::create_dir(&out).unwrap();
        std::os::unix::fs::symlink(&app, out.join("audio")).unwrap();
        assert!(prepare_out_dir(&out, Some(&input), None).is_err());
    }

    #[test]
    fn output_directory_refuses_repo_and_install() {
        let tmp = tempfile::tempdir().unwrap();
        let input = tmp.path().join("in.upk");
        // A plain user directory is fine and gets an `audio/` folder.
        let ok = prepare_out_dir(&tmp.path().join("conv"), Some(&input), None).unwrap();
        assert!(ok.ends_with("audio") && ok.is_dir());
        // Inside the (pretend) install root: refused, nothing created.
        let install = tmp.path().join("Install");
        std::fs::create_dir(&install).unwrap();
        assert!(prepare_out_dir(&install.join("conv"), Some(&input), Some(&install)).is_err());
        assert!(!install.join("conv").exists());
        // Inside an app bundle or a steamapps tree: refused.
        let steam = tmp.path().join("steamapps").join("common");
        std::fs::create_dir_all(&steam).unwrap();
        assert!(prepare_out_dir(&steam.join("conv"), Some(&input), None).is_err());
        assert!(!steam.join("conv").exists());
        // Inside this repository outside research/: refused.
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        if let Ok(repo) = repo.canonicalize() {
            let target = repo.join("docs").join("asamu-audio-test-out");
            assert!(prepare_out_dir(&target, Some(&input), None).is_err());
            assert!(!target.exists());
        }
    }

    /// Two conversions of the user's install produce byte-identical JSON.
    /// Skips when the install is absent; writes only to a temporary folder.
    #[test]
    fn json_output_is_deterministic_on_real_data() {
        let original = std::env::var_os("ASAMU_ORIGINAL_DIR").map(PathBuf::from);
        let found = match &original {
            Some(d) => asamu_locate::from_original_dir(d).is_ok(),
            None => asamu_locate::locate().is_ok(),
        };
        if !found {
            eprintln!("SKIP: original game data not found");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let mut outputs = Vec::new();
        for run_index in 0..2 {
            let out = tmp.path().join(format!("run{run_index}"));
            let ctx = crate::Ctx {
                original: original.clone(),
                out: out.clone(),
            };
            let mut a = args(false);
            a.no_audio = true;
            run(&ctx, a).unwrap();
            let mut files = BTreeMap::new();
            for name in [
                "manifest.json",
                "cues.json",
                "subtitles.json",
                "sound_classes.json",
                "ambient.json",
                "coverage.json",
            ] {
                files.insert(name, std::fs::read(out.join("audio").join(name)).unwrap());
            }
            outputs.push(files);
        }
        for (name, bytes) in &outputs[0] {
            assert!(outputs[1][name] == *bytes, "{name} differs between runs");
        }
        let manifest: serde_json::Value =
            serde_json::from_slice(&outputs[0]["manifest.json"]).unwrap();
        let waves = manifest["waves"].as_object().unwrap();
        assert_eq!(waves.len(), 768);
        // Exact sample counts: RawPCMDataSize = samples x channels x 2.
        assert!(waves.values().all(|w| {
            w["samples"].as_u64().unwrap() * w["channels"].as_u64().unwrap() * 2
                == w["raw_pcm_bytes"].as_u64().unwrap()
        }));
    }

    #[test]
    fn subtitles_pick_language_and_cue_classes() {
        use asamu_ue3::sound::LocalizedSubtitle;
        let mut w = wave("Pkg.Line", 1, 8000);
        let line = |t: &str, s: f32| SubtitleCue {
            text: t.to_owned(),
            time: s,
        };
        w.props.subtitles = vec![line("a", 0.0)];
        w.props.localized_subtitles = vec![
            LocalizedSubtitle {
                slot: 0,
                language: "INT".to_owned(),
                subtitles: vec![line("a", 0.0)],
                mature: false,
                manual_word_wrap: true,
                single_line: false,
            },
            LocalizedSubtitle {
                slot: 1,
                language: "DEU".to_owned(),
                subtitles: vec![line("b", 0.0), line("c", 1.0)],
                mature: false,
                manual_word_wrap: false,
                single_line: false,
            },
        ];
        let mut waves = BTreeMap::new();
        waves.insert(w.path.clone(), w);
        waves.insert("Pkg.Silent".to_owned(), wave("Pkg.Silent", 1, 8000));
        let mut cues = BTreeMap::new();
        cues.insert(
            "Pkg.Cue".to_owned(),
            CueEntry {
                package: "Pkg".to_owned(),
                also_in: Vec::new(),
                differs_in: Vec::new(),
                sound_class: Some("ASAMU_Narrator".to_owned()),
                first_node: Some("Pkg.Line".to_owned()),
                volume_multiplier: None,
                pitch_multiplier: None,
                duration: None,
                max_concurrent_play_count: None,
                face_fx_anim_set: None,
                face_fx_group: None,
                face_fx_anim: None,
                nodes: Vec::new(),
                waves: vec!["Pkg.Line".to_owned()],
                max_depth: 0,
                null_children: 0,
                dangling: Vec::new(),
                unreachable: Vec::new(),
                issues: Vec::new(),
            },
        );
        let int = build_subtitles(&waves, &cues, "INT");
        assert_eq!(int.len(), 1);
        let e = &int["Pkg.Line"];
        assert_eq!(e.language, "INT");
        assert!(e.from_localized && e.manual_word_wrap);
        assert_eq!(e.languages, ["INT", "DEU"]);
        assert_eq!(e.cues, ["Pkg.Cue"]);
        assert_eq!(e.cue_sound_classes, ["ASAMU_Narrator"]);
        let deu = build_subtitles(&waves, &cues, "DEU");
        assert_eq!(deu["Pkg.Line"].lines.len(), 2);
        assert!(build_subtitles(&waves, &cues, "FRA").is_empty());
    }

    #[test]
    fn cue_copies_are_recorded() {
        let entry = |pkg: &str, class: &str| CueEntry {
            package: pkg.to_owned(),
            also_in: Vec::new(),
            differs_in: Vec::new(),
            sound_class: Some(class.to_owned()),
            first_node: None,
            volume_multiplier: None,
            pitch_multiplier: None,
            duration: None,
            max_concurrent_play_count: None,
            face_fx_anim_set: None,
            face_fx_group: None,
            face_fx_anim: None,
            nodes: Vec::new(),
            waves: Vec::new(),
            max_depth: 0,
            null_children: 0,
            dangling: Vec::new(),
            unreachable: Vec::new(),
            issues: Vec::new(),
        };
        let mut cues = BTreeMap::new();
        let mut keys = HashMap::new();
        record_cue(&mut cues, &mut keys, "P.C".to_owned(), entry("A", "X"));
        record_cue(&mut cues, &mut keys, "P.C".to_owned(), entry("B", "X"));
        record_cue(&mut cues, &mut keys, "P.C".to_owned(), entry("C", "Y"));
        let c = &cues["P.C"];
        assert_eq!(c.package, "A");
        assert_eq!(c.also_in, ["B", "C"]);
        assert_eq!(c.differs_in, ["C"]);
    }
}
