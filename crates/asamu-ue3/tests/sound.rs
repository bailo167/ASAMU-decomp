//! Sound decoding against synthetic fixtures written byte by byte here (no
//! original game data): `SoundNodeWave` native bulk records with a hand-made
//! Ogg Vorbis stream and a hand-made WAV file, subtitles, a `SoundCue` graph
//! with an import into a localized package, an unreachable node and a cyclic
//! cue, `SoundClass` / `SoundMode`, an ambient sound actor, the Ogg checker,
//! the WAV writer and parser, and hostile-input (corruption/truncation)
//! sweeps that must never panic.
//!
//! Packages:
//! - `SndPkg`: `Wave_A` (Ogg in `CompressedPCData`, subtitles), `Wave_B`
//!   (WAV in `RawData`), `Cue_A` -> `Atten_0` -> `Random_0` -> {`Wave_A`,
//!   empty input, import `OtherLoc.Wave_C`}, `Cue_A.Orphan_0` (unreachable),
//!   `Class_A`, `Mode_A`, `AmbientSound_0` + `AudioComponent_0` (plays
//!   `Cue_A`), `Cue_Loop` -> `Rnd_1` <-> `Rnd_2` (cycle).
//! - `OtherLoc_LOC_INT`: `OtherLoc.Wave_C` (Ogg).

#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use asamu_ue3::bulkdata::BulkStorage;
use asamu_ue3::schema::{PropertyDef, PropertyType, Schema, StructDef, StructKind};
use asamu_ue3::sound::{
    self, AmbientKind, NodeKind, PayloadFormat, SLOT_COMPRESSED_PC, SLOT_RAW, SoundCoverage,
    SoundDecoder, SoundError, SoundKind, WAVE_BULK_SLOTS, content_hash, ogg_crc, parse_ogg,
    parse_wav, read_wave_native, sniff_payload, wav_file,
};
use asamu_ue3::{Package, PackageSet};
use common::{Export, Import, Synth, W};

// ---------------------------------------------------------------------------
// Names, imports, exports
// ---------------------------------------------------------------------------

const NAMES: &[&str] = &[
    "None",                 // 0
    "Core",                 // 1
    "Package",              // 2
    "Class",                // 3
    "Engine",               // 4
    "SoundNodeWave",        // 5
    "SoundCue",             // 6
    "SoundNodeRandom",      // 7
    "SoundNodeAttenuation", // 8
    "SoundClass",           // 9
    "SoundMode",            // 10
    "AmbientSound",         // 11
    "AudioComponent",       // 12
    "SndPkg",               // 13
    "OtherLoc",             // 14
    "Wave_A",               // 15
    "Wave_B",               // 16
    "Wave_C",               // 17
    "Cue_A",                // 18
    "Atten_0",              // 19
    "Random_0",             // 20
    "Orphan_0",             // 21
    "Class_A",              // 22
    "Mode_A",               // 23
    "AmbientSound_0",       // 24
    "AudioComponent_0",     // 25
    "Cue_Loop",             // 26
    "Rnd_1",                // 27
    "Rnd_2",                // 28
    "Duration",             // 29
    "FloatProperty",        // 30
    "NumChannels",          // 31
    "IntProperty",          // 32
    "SampleRate",           // 33
    "RawPCMDataSize",       // 34
    "bLoopingSound",        // 35
    "BoolProperty",         // 36
    "Subtitles",            // 37
    "ArrayProperty",        // 38
    "Text",                 // 39
    "StrProperty",          // 40
    "Time",                 // 41
    "LocalizedSubtitles",   // 42
    "LanguageExt",          // 43
    "bManualWordWrap",      // 44
    "FirstNode",            // 45
    "ObjectProperty",       // 46
    "VolumeMultiplier",     // 47
    "NameProperty",         // 48
    "ASAMU_Narrator",       // 49
    "ChildNodes",           // 50
    "Weights",              // 51
    "RadiusMax",            // 52
    "bIsChild",             // 53
    "ChildClassNames",      // 54
    "SFX",                  // 55
    "FadeInTime",           // 56
    "Location",             // 57
    "StructProperty",       // 58
    "Vector",               // 59
    "CueTemplate",          // 60
    "Cue_B",                // 61
];

fn n(s: &str) -> i32 {
    NAMES.iter().position(|x| *x == s).unwrap() as i32
}

/// Import package indices.
const IMP_WAVE: i32 = -3;
const IMP_CUE: i32 = -4;
const IMP_RANDOM: i32 = -5;
const IMP_ATTEN: i32 = -6;
const IMP_PACKAGE: i32 = -7;
const IMP_SOUND_CLASS: i32 = -8;
const IMP_SOUND_MODE: i32 = -9;
const IMP_AMBIENT: i32 = -10;
const IMP_AUDIO_COMPONENT: i32 = -11;
const IMP_WAVE_C: i32 = -13;
const IMP_CUE_TEMPLATE: i32 = -14;

/// Export positions (0-based) in `SndPkg`; package index = position + 1.
const E_WAVE_A: usize = 1;
const E_WAVE_B: usize = 2;
const E_CUE_A: usize = 3;
const E_ATTEN: usize = 4;
const E_RANDOM: usize = 5;
const E_ORPHAN: usize = 6;
const E_CLASS_A: usize = 7;
const E_MODE_A: usize = 8;
const E_AMBIENT: usize = 9;
const E_AUDIO: usize = 10;
const E_CUE_LOOP: usize = 11;
const E_RND_1: usize = 12;
const E_RND_2: usize = 13;
const E_CUE_B: usize = 14;
const SND_EXPORTS: usize = 15;

fn pi(e: usize) -> i32 {
    e as i32 + 1
}

fn imports() -> Vec<Import> {
    let imp = |cp, cn, outer, name| Import {
        class_package: cp,
        class_name: cn,
        outer,
        name,
        number: 0,
    };
    vec![
        imp(1, 2, 0, 1),    // -1 Core
        imp(1, 2, 0, 4),    // -2 Engine
        imp(1, 3, -2, 5),   // -3 Engine.SoundNodeWave
        imp(1, 3, -2, 6),   // -4 Engine.SoundCue
        imp(1, 3, -2, 7),   // -5 Engine.SoundNodeRandom
        imp(1, 3, -2, 8),   // -6 Engine.SoundNodeAttenuation
        imp(1, 3, -1, 2),   // -7 Core.Package
        imp(1, 3, -2, 9),   // -8 Engine.SoundClass
        imp(1, 3, -2, 10),  // -9 Engine.SoundMode
        imp(1, 3, -2, 11),  // -10 Engine.AmbientSound
        imp(1, 3, -2, 12),  // -11 Engine.AudioComponent
        imp(1, 2, 0, 14),   // -12 OtherLoc (package)
        imp(4, 5, -12, 17), // -13 OtherLoc.Wave_C (SoundNodeWave)
        imp(4, 6, -12, 60), // -14 OtherLoc.CueTemplate (SoundCue)
    ]
}

// ---------------------------------------------------------------------------
// Payload writers
// ---------------------------------------------------------------------------

fn fname(w: &mut W, index: i32) {
    w.i32(index);
    w.i32(0);
}

fn tag(w: &mut W, name: &str, ty: &str, size: usize) {
    fname(w, n(name));
    fname(w, n(ty));
    w.i32(size as i32);
    w.i32(0);
}

fn t_float(w: &mut W, name: &str, v: f32) {
    tag(w, name, "FloatProperty", 4);
    w.bytes(&v.to_le_bytes());
}

fn t_int(w: &mut W, name: &str, v: i32) {
    tag(w, name, "IntProperty", 4);
    w.i32(v);
}

fn t_bool(w: &mut W, name: &str, v: bool) {
    tag(w, name, "BoolProperty", 0);
    w.bytes(&[u8::from(v)]);
}

fn t_object(w: &mut W, name: &str, index: i32) {
    tag(w, name, "ObjectProperty", 4);
    w.i32(index);
}

fn t_name(w: &mut W, name: &str, value: &str) {
    tag(w, name, "NameProperty", 8);
    fname(w, n(value));
}

fn t_str(w: &mut W, name: &str, value: &str) {
    let mut v = W::default();
    v.fstring(value);
    tag(w, name, "StrProperty", v.len());
    w.bytes(&v.0);
}

/// Array tag whose value bytes are `body` (count included).
fn t_array(w: &mut W, name: &str, body: &[u8]) {
    tag(w, name, "ArrayProperty", body.len());
    w.bytes(body);
}

fn none(w: &mut W) {
    fname(w, 0);
}

fn objects(items: &[i32]) -> Vec<u8> {
    let mut v = W::default();
    v.i32(items.len() as i32);
    for i in items {
        v.i32(*i);
    }
    v.0
}

fn floats(items: &[f32]) -> Vec<u8> {
    let mut v = W::default();
    v.i32(items.len() as i32);
    for f in items {
        v.bytes(&f.to_le_bytes());
    }
    v.0
}

/// Array of tagged `SubtitleCue` structs (test text, not game text).
fn subtitle_array(lines: &[(&str, f32)]) -> Vec<u8> {
    let mut v = W::default();
    v.i32(lines.len() as i32);
    for (text, time) in lines {
        t_str(&mut v, "Text", text);
        t_float(&mut v, "Time", *time);
        none(&mut v);
    }
    v.0
}

fn bulk(w: &mut W, flags: u32, count: i32, size: i32, offset: i32) {
    w.u32(flags);
    w.i32(count);
    w.i32(size);
    w.i32(offset);
}

/// Inline record (+ data); `base` is the payload's absolute stream offset.
fn bulk_inline(w: &mut W, base: usize, data: &[u8]) {
    let at = base + w.len() + 16;
    bulk(w, 0, data.len() as i32, data.len() as i32, at as i32);
    w.bytes(data);
}

/// The seven wave records: `pc` in CompressedPCData, `raw` in RawData.
fn wave_tail(w: &mut W, base: usize, raw: &[u8], pc: &[u8]) {
    bulk_inline(w, base, raw);
    bulk_inline(w, base, pc);
    for _ in 2..7 {
        bulk_inline(w, base, &[]);
    }
}

// ---------------------------------------------------------------------------
// Ogg and WAV fixtures (independent implementations)
// ---------------------------------------------------------------------------

/// Bitwise Ogg CRC (independent of the library's table version).
fn crc_bitwise(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in data {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// One Ogg page holding `packets` (each < 255 * 255 bytes).
fn ogg_page(flags: u8, granule: i64, seq: u32, packets: &[Vec<u8>]) -> Vec<u8> {
    let mut lacing = Vec::new();
    let mut body = Vec::new();
    for p in packets {
        let mut left = p.len();
        loop {
            let l = left.min(255);
            lacing.push(l as u8);
            left -= l;
            if l < 255 {
                break;
            }
        }
        body.extend_from_slice(p);
    }
    let mut page = Vec::new();
    page.extend_from_slice(b"OggS");
    page.push(0);
    page.push(flags);
    page.extend_from_slice(&granule.to_le_bytes());
    page.extend_from_slice(&0x1234_5678u32.to_le_bytes());
    page.extend_from_slice(&seq.to_le_bytes());
    page.extend_from_slice(&[0; 4]);
    page.push(lacing.len() as u8);
    page.extend_from_slice(&lacing);
    page.extend_from_slice(&body);
    let crc = crc_bitwise(&page);
    page[22..26].copy_from_slice(&crc.to_le_bytes());
    page
}

/// A structurally complete Vorbis-in-Ogg stream (headers are real; the audio
/// packet is filler, which the container checker does not decode).
fn ogg_stream(channels: u8, rate: u32, samples: i64) -> Vec<u8> {
    let mut ident = b"\x01vorbis".to_vec();
    ident.extend_from_slice(&0u32.to_le_bytes());
    ident.push(channels);
    ident.extend_from_slice(&rate.to_le_bytes());
    ident.extend_from_slice(&0i32.to_le_bytes());
    ident.extend_from_slice(&32_000i32.to_le_bytes());
    ident.extend_from_slice(&0i32.to_le_bytes());
    ident.push(0xB8);
    ident.push(1);
    let mut comment = b"\x03vorbis".to_vec();
    let vendor = b"synthetic test encoder";
    comment.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    comment.extend_from_slice(vendor);
    comment.extend_from_slice(&0u32.to_le_bytes());
    comment.push(1);
    let mut setup = b"\x05vorbis".to_vec();
    setup.extend_from_slice(&[0xAB; 300]);
    let mut out = ogg_page(0x02, 0, 0, &[ident]);
    out.extend(ogg_page(0x00, 0, 1, &[comment, setup]));
    out.extend(ogg_page(0x04, samples, 2, &[vec![0x55; 40]]));
    out
}

/// Canonical 44-byte-header PCM WAV written independently of the library.
fn wav_by_hand(pcm: &[u8], channels: u16, rate: u32) -> Vec<u8> {
    let mut w = W::default();
    w.bytes(b"RIFF");
    w.u32(36 + pcm.len() as u32);
    w.bytes(b"WAVEfmt ");
    w.u32(16);
    w.u16(1);
    w.u16(channels);
    w.u32(rate);
    w.u32(rate * u32::from(channels) * 2);
    w.u16(channels * 2);
    w.u16(16);
    w.bytes(b"data");
    w.u32(pcm.len() as u32);
    w.bytes(pcm);
    w.0
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

fn ogg_a() -> Vec<u8> {
    ogg_stream(1, 8000, 12_000)
}

fn pcm_b() -> Vec<u8> {
    (0..4000u32)
        .flat_map(|i| (i as u16).to_le_bytes())
        .collect()
}

fn wave_a(base: usize) -> Vec<u8> {
    let mut w = W::default();
    w.i32(1); // NetIndex
    t_float(&mut w, "Duration", 1.5);
    t_int(&mut w, "NumChannels", 1);
    t_int(&mut w, "SampleRate", 8000);
    t_int(&mut w, "RawPCMDataSize", 24_000);
    t_bool(&mut w, "bLoopingSound", false);
    t_bool(&mut w, "bManualWordWrap", true);
    let lines = [("first test line", 0.0), ("second test line", 0.75)];
    t_array(&mut w, "Subtitles", &subtitle_array(&lines));
    let mut loc = W::default();
    loc.i32(3);
    // INT
    t_str(&mut loc, "LanguageExt", "INT");
    t_array(&mut loc, "Subtitles", &subtitle_array(&lines));
    t_bool(&mut loc, "bManualWordWrap", true);
    none(&mut loc);
    // empty slot
    none(&mut loc);
    // DEU
    t_str(&mut loc, "LanguageExt", "DEU");
    t_array(&mut loc, "Subtitles", &subtitle_array(&[("dritte", 0.1)]));
    none(&mut loc);
    t_array(&mut w, "LocalizedSubtitles", &loc.0);
    none(&mut w);
    wave_tail(&mut w, base, &[], &ogg_a());
    w.0
}

fn wave_b(base: usize) -> Vec<u8> {
    let mut w = W::default();
    w.i32(2);
    t_float(&mut w, "Duration", 0.5);
    t_int(&mut w, "NumChannels", 2);
    t_int(&mut w, "SampleRate", 4000);
    none(&mut w);
    wave_tail(&mut w, base, &wav_by_hand(&pcm_b(), 2, 4000), &[]);
    w.0
}

fn wave_c(base: usize) -> Vec<u8> {
    let mut w = W::default();
    w.i32(1);
    t_float(&mut w, "Duration", 1.5);
    t_int(&mut w, "NumChannels", 1);
    t_int(&mut w, "SampleRate", 8000);
    t_int(&mut w, "RawPCMDataSize", 24_000);
    none(&mut w);
    wave_tail(&mut w, base, &[], &ogg_a());
    w.0
}

fn cue_a() -> Vec<u8> {
    let mut w = W::default();
    w.i32(3);
    t_name(&mut w, "SoundClass", "ASAMU_Narrator");
    t_object(&mut w, "FirstNode", pi(E_ATTEN));
    t_float(&mut w, "VolumeMultiplier", 0.5);
    t_float(&mut w, "Duration", 1.5);
    none(&mut w);
    w.i32(4); // EditorData
    for (k, idx) in [pi(E_ATTEN), pi(E_RANDOM), pi(E_WAVE_A), IMP_WAVE_C]
        .iter()
        .enumerate()
    {
        w.i32(*idx);
        w.i32(10 * k as i32);
        w.i32(-20 * k as i32);
    }
    w.0
}

fn node(children: &[i32], extra: impl FnOnce(&mut W)) -> Vec<u8> {
    let mut w = W::default();
    w.i32(4);
    extra(&mut w);
    t_array(&mut w, "ChildNodes", &objects(children));
    none(&mut w);
    w.0
}

fn class_a() -> Vec<u8> {
    let mut w = W::default();
    w.i32(5);
    t_bool(&mut w, "bIsChild", true);
    let mut names = W::default();
    names.i32(1);
    fname(&mut names, n("SFX"));
    t_array(&mut w, "ChildClassNames", &names.0);
    none(&mut w);
    w.i32(1);
    w.i32(pi(E_CLASS_A));
    w.i32(1);
    w.i32(2);
    w.0
}

fn mode_a() -> Vec<u8> {
    let mut w = W::default();
    w.i32(6);
    t_float(&mut w, "FadeInTime", 0.25);
    none(&mut w);
    w.0
}

fn ambient() -> Vec<u8> {
    let mut w = W::default();
    w.i32(7);
    t_object(&mut w, "AudioComponent", pi(E_AUDIO));
    tag(&mut w, "Location", "StructProperty", 12);
    fname(&mut w, n("Vector"));
    for f in [1.0f32, 2.0, 3.0] {
        w.bytes(&f.to_le_bytes());
    }
    none(&mut w);
    w.0
}

fn audio_component() -> Vec<u8> {
    let mut w = W::default();
    w.i32(0); // TemplateOwnerClass (component prelude; no TemplateName)
    w.i32(8); // NetIndex
    t_object(&mut w, "SoundCue", pi(E_CUE_A));
    t_float(&mut w, "VolumeMultiplier", 0.3);
    none(&mut w);
    w.0
}

fn cue_loop() -> Vec<u8> {
    let mut w = W::default();
    w.i32(9);
    t_object(&mut w, "FirstNode", pi(E_RND_1));
    none(&mut w);
    w.i32(0);
    w.0
}

fn names() -> Vec<(String, u64)> {
    NAMES
        .iter()
        .enumerate()
        .map(|(i, s)| (s.to_string(), 0x0007_0010_0000_0000u64 + i as u64))
        .collect()
}

fn export(class: i32, outer: i32, name: &str, payload: Vec<u8>) -> Export {
    Export {
        class,
        super_: 0,
        outer,
        name: n(name),
        number: 0,
        archetype: 0,
        object_flags: 0x0007_0004_0000_0000,
        payload,
        export_flags: 1,
        net_counts: Vec::new(),
        guid: [0; 4],
        package_flags: 0,
    }
}

fn snd_payloads(bases: &[usize]) -> Vec<Vec<u8>> {
    let random_children = [pi(E_WAVE_A), 0, IMP_WAVE_C];
    vec![
        vec![0u8; 12],
        wave_a(bases[E_WAVE_A]),
        wave_b(bases[E_WAVE_B]),
        cue_a(),
        node(&[pi(E_RANDOM)], |w| t_float(w, "RadiusMax", 800.0)),
        node(&random_children, |w| {
            t_array(w, "Weights", &floats(&[1.0, 2.0, 0.5]));
        }),
        node(&[], |_| {}),
        class_a(),
        mode_a(),
        ambient(),
        audio_component(),
        cue_loop(),
        node(&[pi(E_RND_2)], |_| {}),
        node(&[pi(E_RND_1)], |_| {}),
        cue_without_tags(),
    ]
}

fn synth(exports: Vec<Export>) -> Synth {
    let mut s = Synth::sample();
    s.names = names();
    s.imports = imports();
    s.exports = exports;
    s.additional_packages = Vec::new();
    s.texture_allocations = Vec::new();
    s.depends = true;
    s.package_flags = 0x0000_0008; // cooked
    s
}

fn snd_synth(payloads: Vec<Vec<u8>>) -> Synth {
    let classes = [
        (IMP_PACKAGE, 0, "SndPkg"),
        (IMP_WAVE, 1, "Wave_A"),
        (IMP_WAVE, 1, "Wave_B"),
        (IMP_CUE, 1, "Cue_A"),
        (IMP_ATTEN, pi(E_CUE_A), "Atten_0"),
        (IMP_RANDOM, pi(E_CUE_A), "Random_0"),
        (IMP_RANDOM, pi(E_CUE_A), "Orphan_0"),
        (IMP_SOUND_CLASS, 1, "Class_A"),
        (IMP_SOUND_MODE, 1, "Mode_A"),
        (IMP_AMBIENT, 1, "AmbientSound_0"),
        (IMP_AUDIO_COMPONENT, pi(E_AMBIENT), "AudioComponent_0"),
        (IMP_CUE, 1, "Cue_Loop"),
        (IMP_RANDOM, pi(E_CUE_LOOP), "Rnd_1"),
        (IMP_RANDOM, pi(E_CUE_LOOP), "Rnd_2"),
        (IMP_CUE, 1, "Cue_B"),
    ];
    assert_eq!(classes.len(), SND_EXPORTS);
    let mut exports: Vec<Export> = classes
        .iter()
        .zip(payloads)
        .map(|((c, o, name), p)| export(*c, *o, name, p))
        .collect();
    // Cue_B inherits FirstNode from a template in the localized package: the
    // merged value carries that package's index (2 = its Wave_C), which in
    // SndPkg would be Wave_A.
    exports[E_CUE_B].archetype = IMP_CUE_TEMPLATE;
    synth(exports)
}

/// A cue that stores no tags and an empty EditorData map.
fn cue_without_tags() -> Vec<u8> {
    let mut w = W::default();
    w.i32(10);
    none(&mut w);
    w.i32(0);
    w.0
}

/// `OtherLoc.CueTemplate`: FirstNode = its own export 2 (`Wave_C`).
fn cue_template() -> Vec<u8> {
    let mut w = W::default();
    w.i32(2);
    t_object(&mut w, "FirstNode", 2);
    none(&mut w);
    w.i32(0);
    w.0
}

fn loc_synth(payloads: Vec<Vec<u8>>) -> Synth {
    let mut it = payloads.into_iter();
    synth(vec![
        export(IMP_PACKAGE, 0, "OtherLoc", it.next().unwrap()),
        export(IMP_WAVE, 1, "Wave_C", it.next().unwrap()),
        export(IMP_CUE, 1, "CueTemplate", it.next().unwrap()),
    ])
}

/// Build with real absolute offsets (payload sizes do not depend on them).
fn build_snd() -> Vec<u8> {
    let (_, l) = snd_synth(snd_payloads(&[0; SND_EXPORTS])).build();
    let (bytes, l2) = snd_synth(snd_payloads(&l.payload_offsets)).build();
    assert_eq!(l.payload_offsets, l2.payload_offsets);
    bytes
}

fn build_loc() -> Vec<u8> {
    let p = |b: &[usize]| vec![vec![0u8; 12], wave_c(b[1]), cue_template()];
    let (_, l) = loc_synth(p(&[0, 0, 0])).build();
    let (bytes, l2) = loc_synth(p(&l.payload_offsets)).build();
    assert_eq!(l.payload_offsets, l2.payload_offsets);
    bytes
}

// ---------------------------------------------------------------------------
// Test schema (property and struct definitions the fixture needs)
// ---------------------------------------------------------------------------

struct TestSchema;

fn def(name: &str, ty: PropertyType) -> PropertyDef {
    PropertyDef {
        name: name.to_owned(),
        path: format!("Test.{name}"),
        array_dim: 1,
        flags: 0,
        category: "None".to_owned(),
        array_enum: None,
        rep_offset: None,
        ty,
    }
}

fn array_of(ty: PropertyType) -> PropertyType {
    PropertyType::Array {
        inner: Box::new(def("Inner", ty)),
    }
}

const SUBTITLE_CUE: &str = "Engine.SoundNodeWave.SubtitleCue";
const LOCALIZED_SUBTITLE: &str = "Engine.SoundNodeWave.LocalizedSubtitle";

impl Schema for TestSchema {
    fn struct_def(&self, path: &str) -> Option<Arc<StructDef>> {
        let (name, props) = if path.eq_ignore_ascii_case(SUBTITLE_CUE) {
            ("SubtitleCue", vec!["Text", "Time"])
        } else if path.eq_ignore_ascii_case(LOCALIZED_SUBTITLE) {
            (
                "LocalizedSubtitle",
                vec!["LanguageExt", "Subtitles", "bManualWordWrap"],
            )
        } else {
            return None;
        };
        Some(Arc::new(StructDef {
            path: path.to_owned(),
            name: name.to_owned(),
            kind: StructKind::ScriptStruct,
            super_path: None,
            struct_flags: 0,
            properties: props
                .iter()
                .filter_map(|p| self.find_property(path, p))
                .collect(),
        }))
    }
    fn struct_by_name(&self, name: &str) -> Option<Arc<StructDef>> {
        match name {
            "SubtitleCue" => self.struct_def(SUBTITLE_CUE),
            "LocalizedSubtitle" => self.struct_def(LOCALIZED_SUBTITLE),
            _ => None,
        }
    }
    fn find_property(&self, _owner: &str, name: &str) -> Option<Arc<PropertyDef>> {
        let obj = || PropertyType::Object {
            class: "Engine.SoundNode".to_owned(),
        };
        let ty = match name {
            "ChildNodes" => array_of(obj()),
            "Weights" => array_of(PropertyType::Float),
            "ChildClassNames" => array_of(PropertyType::Name),
            "Subtitles" => array_of(PropertyType::Struct {
                struct_path: SUBTITLE_CUE.to_owned(),
            }),
            "LocalizedSubtitles" => array_of(PropertyType::Struct {
                struct_path: LOCALIZED_SUBTITLE.to_owned(),
            }),
            "Text" | "LanguageExt" => PropertyType::Str,
            "Time" => PropertyType::Float,
            "bManualWordWrap" => PropertyType::Bool,
            _ => return None,
        };
        Some(Arc::new(def(name, ty)))
    }
    fn property_link(&self, _owner: &str) -> Vec<Arc<PropertyDef>> {
        Vec::new()
    }
    fn enum_names(&self, _path: &str) -> Option<Arc<Vec<String>>> {
        None
    }
    fn class_chain(&self, class_path: &str) -> Vec<String> {
        let name = class_path
            .rsplit('.')
            .next()
            .unwrap_or(class_path)
            .to_ascii_lowercase();
        if name == "audiocomponent" {
            return ["audiocomponent", "actorcomponent", "component", "object"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        }
        vec![name, "object".to_owned()]
    }
}

struct Fixture {
    set: PackageSet,
}

fn fixture_from(snd: Vec<u8>, loc: Vec<u8>) -> Fixture {
    let set = PackageSet::new::<&str>(&[]);
    set.insert_package("OtherLoc_LOC_INT", Package::from_bytes(loc).unwrap());
    set.insert_package("SndPkg", Package::from_bytes(snd).unwrap());
    Fixture { set }
}

fn fixture() -> Fixture {
    fixture_from(build_snd(), build_loc())
}

// ---------------------------------------------------------------------------
// End-to-end decoding
// ---------------------------------------------------------------------------

#[test]
fn waves_decode_with_exact_native_tail() {
    let f = fixture();
    let dec = SoundDecoder::with_schema(&f.set, &TestSchema);
    let lp = f.set.package("SndPkg").unwrap();
    assert!(lp.package.issues.is_empty(), "{:?}", lp.package.issues);

    let a = dec.decode_wave(&lp, E_WAVE_A).unwrap();
    assert_eq!(a.path, "SndPkg.Wave_A");
    assert!(!a.is_default_object);
    assert_eq!(a.props.duration, Some(1.5));
    assert_eq!(a.props.num_channels, Some(1));
    assert_eq!(a.props.sample_rate, Some(8000));
    assert_eq!(a.props.raw_pcm_data_size, Some(24_000));
    assert_eq!(a.props.looping, Some(false));
    assert!(a.props.manual_word_wrap);
    assert_eq!(a.props.subtitles.len(), 2);
    assert_eq!(a.props.subtitles[1].time, 0.75);
    assert_eq!(a.props.localized_subtitles.len(), 3);
    assert_eq!(a.props.localized_subtitles[1].language, "");
    assert_eq!(a.props.localized_subtitles[2].language, "DEU");
    let n = a.native.as_ref().unwrap();
    assert_eq!(n.records.len(), WAVE_BULK_SLOTS.len());
    assert_eq!(n.filled_slots(), vec![SLOT_COMPRESSED_PC]);
    assert!(a.inline_offsets_match());
    assert_eq!(a.audio_slot(), Some(SLOT_COMPRESSED_PC));
    let payload = lp.package.export_data(E_WAVE_A).unwrap();
    let ogg = a.load_slot(payload, SLOT_COMPRESSED_PC).unwrap();
    assert_eq!(ogg, ogg_a());
    assert_eq!(sniff_payload(&ogg), PayloadFormat::OggVorbis);
    let s = a.summary();
    assert_eq!(s.audio_slot, Some("CompressedPCData"));
    assert_eq!(s.audio_bytes, ogg.len());

    // Subtitle selection by language.
    let int = a.props.subtitle_view("int").unwrap();
    assert!(int.from_localized);
    assert_eq!(int.lines.len(), 2);
    assert!(int.manual_word_wrap);
    let deu = a.props.subtitle_view("DEU").unwrap();
    assert_eq!(deu.lines.len(), 1);
    assert!(a.props.subtitle_view("FRA").is_none());

    let b = dec.decode_wave(&lp, E_WAVE_B).unwrap();
    assert_eq!(b.audio_slot(), Some(SLOT_RAW));
    let raw = b
        .load_slot(lp.package.export_data(E_WAVE_B).unwrap(), SLOT_RAW)
        .unwrap();
    assert_eq!(sniff_payload(&raw), PayloadFormat::RiffWave);
    let info = parse_wav(&raw).unwrap();
    assert_eq!(
        (info.channels, info.sample_rate, info.bits_per_sample),
        (2, 4000, 16)
    );
    assert_eq!(
        &raw[info.data_offset..info.data_offset + info.data_len],
        &pcm_b()[..]
    );
    // The library writer produces the same bytes as the hand-written file.
    assert_eq!(wav_file(&pcm_b(), 2, 4000, 16).unwrap(), raw);

    assert!(matches!(
        dec.decode_wave(&lp, E_CUE_A),
        Err(SoundError::WrongClass { .. })
    ));
}

#[test]
fn cue_graph_walks_nodes_across_packages() {
    let f = fixture();
    let dec = SoundDecoder::with_schema(&f.set, &TestSchema);
    let lp = f.set.package("SndPkg").unwrap();
    let g = dec.cue_graph(&lp, E_CUE_A).unwrap();
    assert_eq!(g.cue.path, "SndPkg.Cue_A");
    assert_eq!(g.cue.sound_class.as_deref(), Some("ASAMU_Narrator"));
    assert_eq!(g.cue.first_node.as_deref(), Some("SndPkg.Cue_A.Atten_0"));
    assert_eq!(g.cue.volume_multiplier, Some(0.5));
    assert_eq!(g.cue.editor.len(), 4);
    assert_eq!(g.cue.editor[3].object.as_deref(), Some("OtherLoc.Wave_C"));
    assert_eq!((g.cue.editor[2].x, g.cue.editor[2].y), (20, -40));

    let paths: Vec<&str> = g.nodes.iter().map(|n| n.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "SndPkg.Cue_A.Atten_0",
            "SndPkg.Cue_A.Random_0",
            "SndPkg.Wave_A",
            "OtherLoc.Wave_C"
        ]
    );
    assert_eq!(g.nodes[0].kind, Some(NodeKind::Attenuation));
    assert_eq!(g.nodes[1].kind, Some(NodeKind::Random));
    assert_eq!(g.nodes[1].children.len(), 3);
    assert_eq!(g.nodes[1].children[1], None);
    assert!(g.nodes[1].params.contains_key("Weights"));
    assert!(!g.nodes[1].params.contains_key("ChildNodes"));
    assert_eq!(g.nodes[3].package.as_deref(), Some("OtherLoc_LOC_INT"));
    assert!(g.nodes.iter().all(|n| n.resolved));
    assert_eq!(
        g.nodes.iter().map(|n| n.depth).collect::<Vec<_>>(),
        [0, 1, 2, 2]
    );
    assert_eq!(g.waves, ["SndPkg.Wave_A", "OtherLoc.Wave_C"]);
    assert_eq!(g.null_children, 1);
    assert!(g.dangling.is_empty());
    assert!(!g.cycle);
    assert_eq!(g.max_depth, 2);
    assert_eq!(g.unreachable, ["SndPkg.Cue_A.Orphan_0"]);
    assert!(g.editor_matches_graph);
    assert_eq!((g.editor_extra, g.editor_missing), (0, 0));
    assert_eq!(g.owner_class, None);
    assert!(
        g.nodes[2]
            .wave
            .as_ref()
            .is_some_and(|w| w.subtitle_lines == 2)
    );

    let l = dec.cue_graph(&lp, E_CUE_LOOP).unwrap();
    assert!(l.cycle);
    assert_eq!(l.nodes.len(), 2);
    assert!(l.cue.editor.is_empty());
    assert!(!l.editor_matches_graph);
}

#[test]
fn inherited_references_resolve_by_path() {
    let f = fixture();
    let dec = SoundDecoder::with_schema(&f.set, &TestSchema);
    let lp = f.set.package("SndPkg").unwrap();
    let g = dec.cue_graph(&lp, E_CUE_B).unwrap();
    // The value comes from the template; its index (2) must not be read as
    // SndPkg's export 2 (Wave_A).
    assert_eq!(g.cue.first_node.as_deref(), Some("OtherLoc.Wave_C"));
    assert_eq!(g.nodes.len(), 1);
    assert_eq!(g.nodes[0].path, "OtherLoc.Wave_C");
    assert_eq!(g.nodes[0].package.as_deref(), Some("OtherLoc_LOC_INT"));
    assert!(g.nodes[0].resolved);
    assert_eq!(g.waves, ["OtherLoc.Wave_C"]);
}

#[test]
fn classes_modes_and_ambient_actors() {
    let f = fixture();
    let dec = SoundDecoder::with_schema(&f.set, &TestSchema);
    let lp = f.set.package("SndPkg").unwrap();
    let c = dec.decode_sound_class(&lp, E_CLASS_A).unwrap();
    assert_eq!(c.name, "Class_A");
    assert!(c.is_child);
    assert_eq!(c.child_class_names, ["SFX"]);
    assert_eq!(c.editor.len(), 1);
    assert_eq!(c.editor[0].object.as_deref(), Some("SndPkg.Class_A"));
    let m = dec.decode_sound_mode(&lp, E_MODE_A).unwrap();
    assert_eq!(m.name, "Mode_A");
    assert!(m.params.contains_key("FadeInTime"));

    assert_eq!(
        dec.export_kind(&lp, E_ORPHAN).map(|(_, k)| k),
        Some(SoundKind::Node(NodeKind::Random))
    );
    let audio = dec.map_audio(&lp);
    assert_eq!(audio.ambient.len(), 1);
    let a = &audio.ambient[0];
    assert_eq!(a.kind, AmbientKind::AmbientSound);
    assert_eq!(a.export_index, E_AMBIENT);
    assert_eq!(a.location, [1.0, 2.0, 3.0]);
    assert_eq!(a.sound_cue.as_deref(), Some("SndPkg.Cue_A"));
    assert_eq!(a.volume_multiplier, Some(0.3));
    assert_eq!(a.audio_component_class.as_deref(), Some("AudioComponent"));
    assert!(a.audio_component_params.contains_key("SoundCue"));
    assert_eq!(a.level_slot, None);
    assert!(audio.reverb.is_empty());
}

#[test]
fn coverage_counts_everything() {
    let f = fixture();
    let dec = SoundDecoder::with_schema(&f.set, &TestSchema);
    let mut cov = SoundCoverage::default();
    for name in ["OtherLoc_LOC_INT", "SndPkg"] {
        cov.add_package(&dec, &f.set.package(name).unwrap());
    }
    cov.finish();
    assert_eq!(cov.failure_count, 0, "{:?}", cov.failures);
    assert_eq!(cov.packages, 2);
    let w = &cov.waves;
    assert_eq!(w.decoded, 3);
    assert_eq!(w.localized, 1);
    assert_eq!(w.inline_offsets_match, 3);
    assert_eq!(w.payload_formats.get(&PayloadFormat::OggVorbis), Some(&2));
    assert_eq!(w.payload_formats.get(&PayloadFormat::RiffWave), Some(&1));
    assert_eq!(w.slots["CompressedPCData"].inline_filled, 2);
    assert_eq!(w.slots["RawData"].inline_filled, 1);
    assert_eq!(w.slots["CompressedFlashData"].empty, 3);
    assert_eq!(w.unique_paths, 3);
    let o = &cov.ogg;
    assert_eq!(
        (o.parsed, o.failed, o.pages, o.crc_mismatches),
        (2, 0, 6, 0)
    );
    assert_eq!((o.channels_match, o.rate_match), (2, 2));
    assert_eq!((o.pcm_size_match, o.duration_match), (2, 2));
    assert_eq!(o.vendors.get("synthetic test encoder"), Some(&2));
    let s = &cov.subtitles;
    assert_eq!((s.waves_with_subtitles, s.lines), (1, 2));
    assert_eq!(s.waves_with_localized, 1);
    assert_eq!(s.empty_slots, 1);
    assert_eq!(s.waves_per_language.get("DEU"), Some(&1));
    assert_eq!(s.plain_equals_int, 1);
    assert_eq!(s.outside_localized_packages, 1);
    let c = &cov.cues;
    assert_eq!((c.decoded, c.graphs, c.cycles), (4, 4, 1));
    assert_eq!((c.wave_leaves, c.cross_package_leaves), (4, 2));
    assert_eq!(
        (c.null_children, c.unreachable_nodes, c.dangling),
        (1, 1, 0)
    );
    assert_eq!(
        (c.editor_matches_graph, c.editor_differs, c.editor_empty),
        (1, 3, 3)
    );
    assert_eq!(c.editor_empty_by_owner.get("package"), Some(&3));
    assert_eq!((c.editor_extra_keys, c.editor_missing_nodes), (0, 0));
    assert_eq!(
        c.unresolved_sound_class_names.get("ASAMU_Narrator"),
        Some(&1)
    );
    assert_eq!((cov.sound_classes, cov.sound_modes), (1, 1));
    assert_eq!(cov.ambient.with_cue, 1);
    assert_eq!(cov.ambient.cue_resolved, 1);
}

// ---------------------------------------------------------------------------
// Ogg, WAV and helpers
// ---------------------------------------------------------------------------

#[test]
fn ogg_checker_reads_headers_and_detects_damage() {
    let s = ogg_a();
    let info = parse_ogg(&s).unwrap();
    assert_eq!(info.pages, 3);
    assert_eq!(info.crc_mismatches, 0);
    assert_eq!(info.streams, 1);
    assert!(info.first_page_bos && info.last_page_eos);
    assert_eq!(info.final_granule, Some(12_000));
    assert_eq!(info.packets, 4);
    let v = info.vorbis.as_ref().unwrap();
    assert_eq!(
        (v.channels, v.sample_rate, v.bitrate_nominal),
        (1, 8000, 32_000)
    );
    assert_eq!((v.blocksize_0, v.blocksize_1), (8, 11));
    assert_eq!(info.vendor.as_deref(), Some("synthetic test encoder"));
    assert_eq!(info.comments, Some(0));
    assert!(info.setup_header);
    assert_eq!(info.duration(), Some(1.5));
    // The library's table CRC agrees with the bitwise one.
    assert_eq!(ogg_crc(b"OggS test"), crc_bitwise(b"OggS test"));

    let mut bad = s.clone();
    let last = bad.len() - 1;
    bad[last] ^= 0xFF;
    assert_eq!(parse_ogg(&bad).unwrap().crc_mismatches, 1);
    assert!(parse_ogg(&s[..s.len() - 1]).is_err());
    let mut trailing = s.clone();
    trailing.extend_from_slice(b"junk");
    assert!(parse_ogg(&trailing).is_err());
    assert!(parse_ogg(&[]).is_err());
    let mut gap = ogg_page(0x02, 0, 0, &[vec![1]]);
    gap.extend(ogg_page(0x04, 5, 7, &[vec![2]]));
    assert_eq!(parse_ogg(&gap).unwrap().sequence_gaps, 1);
    // A packet spanning a page boundary (255-byte lacing continues).
    let long = vec![0x77u8; 600];
    let mut lacing_page = ogg_page(0x02, 0, 0, &[long]);
    assert_eq!(parse_ogg(&lacing_page).unwrap().packets, 1);
    lacing_page[5] = 0;
    assert!(!parse_ogg(&lacing_page).unwrap().first_page_bos);
}

#[test]
fn wav_writer_and_parser() {
    let pcm = [1u8, 0, 2, 0, 3, 0];
    let w = wav_file(&pcm, 1, 22_050, 16).unwrap();
    assert_eq!(w.len(), 44 + pcm.len());
    assert_eq!(&w[..4], b"RIFF");
    assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()), 36 + 6);
    let info = parse_wav(&w).unwrap();
    assert_eq!(
        (
            info.format_tag,
            info.channels,
            info.sample_rate,
            info.bits_per_sample
        ),
        (1, 1, 22_050, 16)
    );
    assert_eq!((info.data_offset, info.data_len), (44, 6));
    // Odd 8-bit data gets a pad byte counted in the RIFF size.
    let odd = wav_file(&[9, 9, 9], 1, 8000, 8).unwrap();
    assert_eq!(odd.len(), 44 + 4);
    assert_eq!(u32::from_le_bytes(odd[4..8].try_into().unwrap()), 36 + 4);
    assert_eq!(parse_wav(&odd).unwrap().data_len, 3);
    assert!(wav_file(&[1, 2, 3], 1, 8000, 16).is_err());
    assert!(wav_file(&pcm, 0, 8000, 16).is_err());
    assert!(wav_file(&pcm, 1, 0, 16).is_err());
    assert!(wav_file(&pcm, 1, 8000, 12).is_err());
    assert!(parse_wav(b"RIFF\0\0\0\0WAVE").is_err());
    assert!(parse_wav(&w[..30]).is_err());
    assert!(parse_wav(b"not a wav file at all").is_err());
}

#[test]
fn sniffing_and_hashing() {
    assert_eq!(sniff_payload(&[]), PayloadFormat::Empty);
    assert_eq!(sniff_payload(b"OggS\0\x02"), PayloadFormat::OggOther);
    assert_eq!(sniff_payload(b"\x00\x01\x02\x03"), PayloadFormat::Unknown);
    assert_eq!(PayloadFormat::OggVorbis.extension(), Some("ogg"));
    assert_eq!(PayloadFormat::RiffWave.extension(), Some("wav"));
    assert_eq!(PayloadFormat::Unknown.extension(), None);
    // FNV-1a 64 reference values.
    assert_eq!(content_hash(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(content_hash(b"a"), 0xaf63_dc4c_8601_ec8c);
}

#[test]
fn wave_native_needs_all_seven_records() {
    let mut w = W::default();
    for _ in 0..7 {
        bulk(&mut w, 0, 0, 0, 0);
    }
    let (n, end) = read_wave_native(&w.0, 0).unwrap();
    assert_eq!(end, 7 * 16);
    assert!(n.filled_slots().is_empty());
    assert!(n.records.iter().all(|r| r.storage() == BulkStorage::Inline));
    assert!(read_wave_native(&w.0[..6 * 16 + 8], 0).is_err());
    assert!(read_wave_native(&w.0, 200).is_err());
    let mut bad = w.0.clone();
    bad[16 + 8..16 + 12].copy_from_slice(&1000i32.to_le_bytes()); // inline size past the end
    assert!(read_wave_native(&bad, 0).is_err());
}

// ---------------------------------------------------------------------------
// Hostile input
// ---------------------------------------------------------------------------

/// Decode everything the importer would; results may be errors, never panics.
fn exercise(f: &Fixture) {
    let dec = SoundDecoder::with_schema(&f.set, &TestSchema);
    let mut cov = SoundCoverage::default();
    for name in ["OtherLoc_LOC_INT", "SndPkg"] {
        let Some(lp) = f.set.package(name) else {
            continue;
        };
        cov.add_package(&dec, &lp);
        for i in 0..lp.package.exports.len() {
            let _ = dec.decode_wave(&lp, i);
            let _ = dec.cue_graph(&lp, i);
            let _ = dec.decode_node(&lp, i);
            let _ = dec.decode_sound_class(&lp, i);
            let _ = dec.decode_sound_mode(&lp, i);
            if let Ok(w) = dec.decode_wave(&lp, i)
                && let Ok(payload) = lp.package.export_data(i)
            {
                for slot in 0..WAVE_BULK_SLOTS.len() {
                    if let Ok(b) = w.load_slot(payload, slot) {
                        let _ = parse_ogg(&b);
                        let _ = parse_wav(&b);
                    }
                }
            }
        }
        let _ = dec.map_audio(&lp);
    }
    cov.finish();
}

#[test]
fn corrupted_payload_bytes_never_panic() {
    let snd = build_snd();
    let loc = build_loc();
    let (_, layout) = snd_synth(snd_payloads(&[0; SND_EXPORTS])).build();
    let start = layout.header_end;
    let len = snd.len();
    // Every byte of the cue/node/class region and a stride over the waves.
    let mut positions: Vec<usize> = (start..len).step_by(7).collect();
    positions.extend(layout.payload_offsets[E_CUE_A]..layout.payload_offsets[E_CLASS_A]);
    for (k, pos) in positions.into_iter().enumerate() {
        let mut bytes = snd.clone();
        bytes[pos] = match k % 3 {
            0 => 0xFF,
            1 => 0x00,
            _ => bytes[pos].wrapping_add(0x41),
        };
        let Ok(pkg) = Package::from_bytes(bytes) else {
            continue;
        };
        let set = PackageSet::new::<&str>(&[]);
        set.insert_package(
            "OtherLoc_LOC_INT",
            Package::from_bytes(loc.clone()).unwrap(),
        );
        set.insert_package("SndPkg", pkg);
        exercise(&Fixture { set });
    }
}

#[test]
fn hostile_counts_and_streams_never_panic() {
    // Ogg: random-ish damage at every position of a small stream.
    let s = ogg_a();
    for pos in 0..s.len() {
        for v in [0x00u8, 0xFF, 0x80] {
            let mut b = s.clone();
            b[pos] = v;
            let _ = parse_ogg(&b);
        }
        let _ = parse_ogg(&s[..pos]);
    }
    // WAV: truncations and huge chunk sizes.
    let w = wav_file(&[0u8; 64], 2, 8000, 16).unwrap();
    for pos in 0..w.len() {
        let _ = parse_wav(&w[..pos]);
        let mut b = w.clone();
        b[pos] = 0xFF;
        let _ = parse_wav(&b);
    }
    // EditorData with an impossible count, truncated cue payloads.
    let f = fixture();
    let lp = f.set.package("SndPkg").unwrap();
    let payload = lp.package.export_data(E_CUE_A).unwrap().to_vec();
    let dec = SoundDecoder::with_schema(&f.set, &TestSchema);
    let obj = dec.decode_object(&lp, E_CUE_A).unwrap();
    let mut huge = payload.clone();
    huge[obj.properties_end..obj.properties_end + 4].copy_from_slice(&i32::MAX.to_le_bytes());
    assert!(sound::read_editor_map(&lp, &huge, obj.properties_end).is_err());
    let mut neg = payload.clone();
    neg[obj.properties_end..obj.properties_end + 4].copy_from_slice(&(-1i32).to_le_bytes());
    assert!(sound::read_editor_map(&lp, &neg, obj.properties_end).is_err());
    for cut in 0..payload.len() {
        let _ = sound::read_editor_map(&lp, &payload[..cut], obj.properties_end.min(cut));
    }
}

#[test]
fn truncated_packages_never_panic() {
    let snd = build_snd();
    let loc = build_loc();
    for cut in (0..snd.len()).step_by(97) {
        let Ok(pkg) = Package::from_bytes(snd[..cut].to_vec()) else {
            continue;
        };
        let set = PackageSet::new::<&str>(&[]);
        set.insert_package(
            "OtherLoc_LOC_INT",
            Package::from_bytes(loc.clone()).unwrap(),
        );
        set.insert_package("SndPkg", pkg);
        exercise(&Fixture { set });
    }
}

// ---------------------------------------------------------------------------
// Hardening: container validity, strict WAV formats, deterministic
// resolution of shared objects
// ---------------------------------------------------------------------------

/// One Ogg page with explicit lacing values (filler body), checksummed with
/// the independent bitwise CRC.
fn raw_page(flags: u8, granule: i64, serial: u32, seq: u32, lacing: &[u8]) -> Vec<u8> {
    let body_len: usize = lacing.iter().map(|&l| usize::from(l)).sum();
    let mut page = Vec::new();
    page.extend_from_slice(b"OggS");
    page.push(0);
    page.push(flags);
    page.extend_from_slice(&granule.to_le_bytes());
    page.extend_from_slice(&serial.to_le_bytes());
    page.extend_from_slice(&seq.to_le_bytes());
    page.extend_from_slice(&[0; 4]);
    page.push(lacing.len() as u8);
    page.extend_from_slice(lacing);
    page.extend(std::iter::repeat_n(0x5Au8, body_len));
    let crc = crc_bitwise(&page);
    page[22..26].copy_from_slice(&crc.to_le_bytes());
    page
}

#[test]
fn ogg_checker_flags_continuation_granule_and_unterminated_packets() {
    let good = ogg_stream(2, 44_100, 1000);
    let info = parse_ogg(&good).unwrap();
    assert!(info.is_valid_vorbis());
    assert_eq!((info.continuation_errors, info.granule_regressions), (0, 0));
    assert!(!info.unterminated_packet);
    assert!(parse_ogg(&ogg_a()).unwrap().is_valid_vorbis());

    // A packet continued on the next page with the flag set: valid.
    let mut s = raw_page(0x02, -1, 7, 0, &[255]);
    s.extend(raw_page(0x01 | 0x04, 10, 7, 1, &[3]));
    let i = parse_ogg(&s).unwrap();
    assert_eq!((i.continuation_errors, i.packets), (0, 1));
    assert!(!i.unterminated_packet);
    assert!(i.first_page_bos && i.last_page_eos);
    assert_eq!(i.final_granule, Some(10));

    // The same pages without the continued flag: one error.
    let mut s = raw_page(0x02, -1, 7, 0, &[255]);
    s.extend(raw_page(0x04, 10, 7, 1, &[3]));
    assert_eq!(parse_ogg(&s).unwrap().continuation_errors, 1);

    // A continued flag with no packet pending: one error.
    let mut s = raw_page(0x02, 0, 7, 0, &[3]);
    s.extend(raw_page(0x01 | 0x04, 10, 7, 1, &[3]));
    assert_eq!(parse_ogg(&s).unwrap().continuation_errors, 1);

    // The stream ends inside a packet.
    let i = parse_ogg(&raw_page(0x02 | 0x04, -1, 7, 0, &[255])).unwrap();
    assert!(i.unterminated_packet);
    assert!(!i.is_valid_vorbis());

    // A granule position going backwards.
    let mut s = raw_page(0x02, 100, 7, 0, &[3]);
    s.extend(raw_page(0x04, 50, 7, 1, &[3]));
    let i = parse_ogg(&s).unwrap();
    assert_eq!(i.granule_regressions, 1);
    assert_eq!(i.final_granule, Some(50));

    // A second logical stream appended: not one valid Vorbis stream.
    let mut s = good.clone();
    s.extend(raw_page(0x02 | 0x04, 0, 99, 0, &[3]));
    let i = parse_ogg(&s).unwrap();
    assert_eq!(i.streams, 2);
    assert!(!i.is_valid_vorbis());

    // One flipped bit in the audio packet: checksum mismatch, invalid.
    let mut s = good.clone();
    let last = s.len() - 1;
    s[last] ^= 1;
    let i = parse_ogg(&s).unwrap();
    assert_eq!(i.crc_mismatches, 1);
    assert!(!i.is_valid_vorbis());

    // Non-Vorbis Ogg: structurally fine, but not a Vorbis stream.
    let mut s = raw_page(0x02, 0, 7, 0, &[3]);
    s.extend(raw_page(0x04, 10, 7, 1, &[3]));
    let i = parse_ogg(&s).unwrap();
    assert!(i.vorbis.is_none());
    assert!(!i.is_valid_vorbis());
}

#[test]
fn wav_parser_rejects_inconsistent_formats() {
    let good = wav_file(&[0u8; 8], 2, 8000, 16).unwrap();
    assert_eq!(good, wav_by_hand(&[0u8; 8], 2, 8000));
    let info = parse_wav(&good).unwrap();
    assert!(info.riff_size_matches);
    assert_eq!(
        (info.block_align, info.byte_rate, info.frames()),
        (4, 32_000, 2)
    );
    // Canonical header offsets: tag 20, channels 22, rate 24, byte rate 28,
    // block align 32, bits 34, data length 40.
    let patch = |off: usize, bytes: &[u8]| {
        let mut b = good.clone();
        b[off..off + bytes.len()].copy_from_slice(bytes);
        b
    };
    assert!(parse_wav(&patch(22, &0u16.to_le_bytes())).is_err());
    assert!(parse_wav(&patch(24, &0u32.to_le_bytes())).is_err());
    assert!(parse_wav(&patch(28, &1u32.to_le_bytes())).is_err());
    assert!(parse_wav(&patch(32, &3u16.to_le_bytes())).is_err());
    assert!(parse_wav(&patch(32, &0u16.to_le_bytes())).is_err());
    assert!(parse_wav(&patch(34, &0u16.to_le_bytes())).is_err());
    assert!(parse_wav(&patch(34, &24u16.to_le_bytes())).is_err());
    // A data chunk that is not a whole number of frames.
    assert!(parse_wav(&patch(40, &6u32.to_le_bytes())).is_err());
    // Other format tags only need non-zero fields.
    assert!(parse_wav(&patch(20, &3u16.to_le_bytes())).is_ok());
    // A wrong RIFF size is reported, not fatal.
    let mut longer = good.clone();
    longer.extend_from_slice(&[0, 0]);
    assert!(!parse_wav(&longer).unwrap().riff_size_matches);
    // Every header the writer produces parses back to its own format.
    for (ch, bits) in [(1u16, 8u16), (2, 16), (3, 24), (6, 32)] {
        let frame = usize::from(ch) * usize::from(bits / 8);
        let w = wav_file(&vec![0u8; frame * 5], ch, 22_050, bits).unwrap();
        let i = parse_wav(&w).unwrap();
        assert!(i.riff_size_matches);
        assert_eq!(
            (i.channels, i.bits_per_sample, i.sample_rate, i.frames()),
            (ch, bits, 22_050, 5)
        );
    }
}

/// The wave `OtherLoc.Wave_C` that `Cue_A` imports exists in several
/// packages. Whatever the insertion order (and so the hash order inside the
/// package set), the decoder must resolve it to the same package: the
/// owner's localized companion when registered, else the first registered
/// package by name.
#[test]
fn shared_objects_resolve_to_a_fixed_package() {
    let snd = build_snd();
    let loc = build_loc();
    let orders: [[&str; 3]; 4] = [
        ["OtherLoc_LOC_INT", "ZCopy_LOC_INT", "AaaCopy_LOC_INT"],
        ["AaaCopy_LOC_INT", "OtherLoc_LOC_INT", "ZCopy_LOC_INT"],
        ["ZCopy_LOC_INT", "AaaCopy_LOC_INT", "OtherLoc_LOC_INT"],
        ["ZCopy_LOC_INT", "OtherLoc_LOC_INT", "AaaCopy_LOC_INT"],
    ];
    for companion in [false, true] {
        for round in 0..12 {
            let order = orders[round % orders.len()];
            let set = PackageSet::new::<&str>(&[]);
            for name in order {
                set.insert_package(name, Package::from_bytes(loc.clone()).unwrap());
            }
            if companion {
                set.insert_package("SndPkg_LOC_INT", Package::from_bytes(loc.clone()).unwrap());
            }
            set.insert_package("SndPkg", Package::from_bytes(snd.clone()).unwrap());
            let dec = SoundDecoder::with_schema(&set, &TestSchema);
            for name in order.iter().rev() {
                dec.register_package(name);
            }
            if companion {
                dec.register_package("SndPkg_LOC_INT");
            }
            let lp = set.package("SndPkg").unwrap();
            let g = dec.cue_graph(&lp, E_CUE_A).unwrap();
            let leaf = g
                .nodes
                .iter()
                .find(|n| n.path == "OtherLoc.Wave_C")
                .unwrap();
            let want = if companion {
                "SndPkg_LOC_INT"
            } else {
                "AaaCopy_LOC_INT"
            };
            assert_eq!(leaf.package.as_deref(), Some(want), "round {round}");
            assert!(leaf.resolved);
            assert!(g.dangling.is_empty());
            // The public resolver agrees, and local exports win.
            let (hit, _) = dec.locate_from(&lp, "OtherLoc.Wave_C").unwrap();
            assert_eq!(hit.map(|p| p.name.clone()).as_deref(), Some(want));
            let (local, i) = dec.locate_from(&lp, "SndPkg.Wave_A").unwrap();
            assert!(local.is_none());
            assert_eq!(i, E_WAVE_A);
            assert!(dec.locate_from(&lp, "Nowhere.Nothing").is_none());
        }
    }
}
