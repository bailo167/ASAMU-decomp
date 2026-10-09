# Audio: SoundNodeWave, SoundCue graphs, sound classes, subtitles, ambient sounds

Evidence source: every sound export of the 38 script, content and map packages under
`~/Library/Application Support/Steam/steamapps/common/A Story About My Uncle/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac`
(and `Maps/`) of the legitimately owned Mac install (Steam build 1822049). The shader caches and
`GlobalPersistentCookerData.upk` were skipped: they hold no sound objects. Everything was read by our own code in
`crates/asamu-ue3/src/sound.rs`, on top of the object decoder (`OBJECT_FORMAT.md`) and the bulk data reader
(`TEXTURES.md`). This page is structure, names and counts only: no audio, no subtitle text, no decompiled code.

Reproduce:

```sh
cargo run --release -p asamu-import -- audio --check           # every number on this page (writes nothing)
cargo run --release -p asamu-import -- audio --check --json    # the same, machine-readable
cargo test -p asamu-ue3 --test sound_real_data                 # asserts the (T) claims; skips without data
cargo test -p asamu-ue3 --test sound                           # synthetic fixtures, known answers, hostile input
cargo run --release -p asamu-import -- --out <dir> audio       # convert (user-local output only)
```

`(T)` marks a claim asserted by `crates/asamu-ue3/tests/sound_real_data.rs` (or, for the converter, the gated test in
`tools/asamu-import/src/audio.rs`) against the install.

## Result — CONFIRMED (T)

Every sound export decodes with exact payload consumption: prelude, tagged properties and native data end at
`SerialSize`.

| Class | Exports | Default objects | Exact |
|---|---:|---:|---:|
| `SoundNodeWave` | 852 | 1 | 852 |
| `SoundCue` | 605 | 1 | 605 |
| `SoundNodeModulator` | 285 | 1 | 285 |
| `SoundNodeAttenuation` | 244 | 1 | 244 |
| `SoundNodeRandom` | 118 | 1 | 118 |
| `SoundNodeLooping` | 105 | 1 | 105 |
| `SoundNodeAmbient` | 80 | 1 | 80 |
| `SoundNodeMixer` | 78 | 1 | 78 |
| `SoundClass` | 48 | 1 | 48 |
| `SoundNodeDelay` | 35 | 1 | 35 |
| `SoundMode` | 18 | 1 | 18 |
| `SoundNodeAmbientNonLoopToggle` | 7 | 1 | 7 |
| `SoundNodeModulatorContinuous` | 5 | 1 | 5 |
| `ForcedLoopSoundNode` | 3 | 1 | 3 |
| `SoundNodeWaveParam` | 3 | 1 | 3 |
| `SoundNodeAmbientNonLoop` | 2 | 1 | 2 |
| 10 more `SoundNode*` classes (`AttenuationAndGain`, `Concatenator`, `ConcatenatorRadio`, `DistanceCrossFade`, `Doppler`, `Enveloper`, `Mature`, `Oscillator`, `WaveStreaming`, abstract `SoundNode`) | 10 | 10 | 10 (default objects only) |

Where the 851 non-default waves live: `Startup.upk` 294, the nine map packages 272, the eight `*_LOC_INT.upk`
packages plus `Startup_LOC_INT.upk` 267, `UDKBase.u` 18. 768 distinct object paths; the other 83 are copies of a wave
cooked into a second package, and every copy's audio is byte-identical to the first (FNV-1a hash) (T).

## SoundNodeWave native layout — CONFIRMED (T, exact consumption)

```text
UObject prelude, tagged properties (terminated by None)
FByteBulkData RawData
FByteBulkData CompressedPCData
FByteBulkData CompressedXbox360Data
FByteBulkData CompressedPS3Data
FByteBulkData CompressedWiiUData
FByteBulkData CompressedIPhoneData
FByteBulkData CompressedFlashData
```

- Each record is the 16-byte bulk header of `TEXTURES.md` (`flags`, `ElementCount`, `SizeOnDisk`, `OffsetInFile`)
  followed by `SizeOnDisk` inline bytes. The order equals the declaration order of the seven `UntypedBulkData_Mirror`
  properties of `Engine.SoundNodeWave`.
- In every one of the 851 waves: all seven records have flags `0x0` (inline, uncompressed, used). `CompressedPCData`
  holds data in all 851 (64,847,111 bytes in total); the other six records are empty (`ElementCount` = `SizeOnDisk`
  = 0) in all 851. There is no `.tfc`-style separate file and no LZO compression for sound (T).
- Every record's `OffsetInFile` equals the absolute stream position of its bytes, as for textures (T).
- The class default object has no native data.

So the Mac cook ships exactly one audio representation: the PC one. There is no raw PCM (`RawData` is stripped by
the cooker) and nothing for the console, mobile or Flash slots.

## Audio format: Ogg Vorbis — CONFIRMED (T)

Every `CompressedPCData` payload is one complete Ogg stream carrying Vorbis I. Our checker (`sound::parse_ogg`) walks
every page of all 851 streams:

- 16,640 pages; every page checksum is correct (CRC-32, polynomial 0x04C11DB7, computed over the page with the CRC
  field zeroed); no sequence gaps; one logical bitstream per payload; first page BOS, last page EOS; no bytes before
  the first or after the last page (T). Every page's continued-packet flag agrees with the page before, granule
  positions never decrease, and no stream ends inside a packet (T): all 851 pass `OggInfo::is_valid_vorbis`.
- The first three packets are the Vorbis identification, comment and setup headers in all 851 (T).
- Identification header vs. tagged properties (T, all 851):
  - `audio_channels` = `NumChannels` and `audio_sample_rate` = `SampleRate`.
  - `RawPCMDataSize` = final granule position x channels x 2. So `RawPCMDataSize` is the size of the decoded
    16-bit PCM, and the final granule is the exact sample count per channel.
  - `Duration` = final granule / sample rate, within 3.4 microseconds (float rounding).
- Three encoders appear in the comment headers' vendor strings: libVorbis of 2010-11-01 (596 streams), aoTuV b5 of
  2006-10-24 (191) and libVorbis of 2005-03-04 (64). Every stream also carries user comments; their content is not
  examined here.
- Independent re-parse (CONFIRMED, local throwaway checker in Python, not part of the test suite; 2026-10-10): its own
  summary/name/import/export reader over the uncompressed streams of all 38 packages (LZO decompression by
  `asamu-inspect decompress`, itself cross-checked against liblzo2 in `PACKAGE_ANALYSIS.md`), its own tag walker and
  bulk-record reader and its own bitwise-table Ogg CRC. It agrees with every number of this section and the two above:
  852 wave exports consumed exactly, the slot layout and flags, 64,847,111 bytes, offsets, 16,640 pages with 0 CRC
  errors, channels / rate / `RawPCMDataSize` / `Duration` (max error 3.33 microseconds), the tag census, and the 83
  copies (byte-identical by SHA-256, not only by the FNV-1a hash the library uses). Every `.ogg` file written by
  `asamu-import audio` is byte-identical (SHA-256) to the payload the checker extracted, and the manifest `samples`
  equals that payload's final granule, for all 768.
- Third-party decoder (CONFIRMED, local probe with FFmpeg, not part of the test suite): all 768 written files decode
  without a decoder error and `ffprobe` reports the manifest's channel count and sample rate for all 768. Its stream
  length (`duration_ts`) is simply the final granule, so it is not an independent length check. Decoding to PCM gives
  exactly `RawPCMDataSize` bytes for 635 files; for 133 files FFmpeg's output differs by -1,007 to +256 samples per
  channel (at most 23 ms): its demuxer assigns a duration to the first audio packet (which a Vorbis decoder turns into
  no samples) and trims the end from that shifted count. The streams themselves are consistent; the exact length is
  the final granule.
- The engine decodes with libvorbisfile: the unstripped Mac executable contains `ov_open_callbacks`, `ov_read`,
  `ov_pcm_total` and `FVorbisAudioInfo::ReadCompressedInfo` / `ReadCompressedData` (`nm`, CONFIRMED). That it plays
  exactly final-granule samples (vorbisfile trims the last packet by granule position) is TENTATIVE: library
  behaviour, not traced in the binary.

Format facts (counts of the 851 waves): mono 427, stereo 424 (T). Sample rates: 44,100 Hz 417, 48,000 Hz 197,
22,050 Hz 137, 27,700 Hz 46, 27,777 Hz 29, 29,866 Hz 10, 32,000 Hz 5, 36,000 Hz 5, 38,000 Hz 2, 96,000 Hz 3. Total
duration 6,590.7 s over the 851 (5,806.6 s over the 768 distinct paths). Nominal bitrates range from 32,888 to 104,000 bit/s; 3 streams store -2 instead of a rate.

Consequence for the runtime: the converter can pass the streams through unchanged (`.ogg`); no decoding or
re-encoding is needed, and nothing is lost.

## SoundNodeWave properties — CONFIRMED (T where marked)

Tags stored on the 851 waves (how many waves store each): `Duration`, `NumChannels`, `SampleRate`, `RawPCMDataSize`
(851 each), `SourceFilePath` and `SourceFileTimestamp` (614; editor-only, a developer machine path, never exported by
the converter), `LocalizedSubtitles` (267), `bLoopingSound` (178), `bManualWordWrap` (175), `Subtitles` (161),
`bForceRealTimeDecompression` (75) (T). Nothing else is stored, so everything else is the class default.

Class defaults (`Engine.Default__SoundNodeWave`, CONFIRMED, cdo): `Volume` 0.75, `Pitch` 1.0, `CompressionQuality` 40,
`bLoopingSound` true. The converter reports values merged over these defaults.

- `bLoopingSound` is in the editor category `Compression` (CONFIRMED from the class model). It is true for 673 waves
  only because the default is true; whether the engine loops a wave is decided by the cue graph (see `Duration` below).
  Its exact effect on PC playback: TENTATIVE (likely only a compression/streaming hint).
- `bMature`, `bSingleLine`, `bUseTTS` and `SpokenText` are never set (T).

## Subtitles and localization — CONFIRMED (T)

- A subtitle line (`SubtitleCue`) has exactly two members, `Text` and `Time` (start in seconds from the start of the
  wave). There is no speaker field in the wave data. The converter reports, per subtitled wave, the `SoundClass` of
  the cues that play it: narrator lines are `ASAMU_Narrator` (74 waves), spoken character lines `ASAMU_Voice` (77),
  plus 3 `ASAMU_MenuSFX` waves (counts over distinct paths). That class is the only speaker hint stored.
- 161 waves have subtitle lines (519 lines). All of them live in localized `*_LOC_INT.upk` packages (T).
- Every wave of a localized package (267) stores `LocalizedSubtitles`, an array of 23 entries
  (`LanguageExt`, `Subtitles`, `bMature`, `bManualWordWrap`, `bSingleLine`). The slot order is the same in all 267
  arrays (T): `INT, -, CZE, DEU, -, ESN, FRA, HUN, ITA, -, -, POL, -, -, SLO, -, BRA, FIN, NLD, POR, -, TUR, -`, where
  `-` is an empty slot with an empty `LanguageExt`. The names of the 9 empty slots are not stored (UNKNOWN; a search
  of the shipped config files and of NUL-terminated ASCII/UTF-16/UTF-32 strings in the executable found no language
  code list).
- 14 languages have lines, each for all 161 subtitled waves (T): BRA, CZE, DEU, ESN, FIN, FRA, HUN, INT, ITA, NLD,
  POL, POR, SLO, TUR. Line counts are 519 for most languages; DEU 523, FRA 528, ITA 520, POL 522 split some lines
  differently.
- The plain `Subtitles` property equals the `INT` entry in all 267 waves (T).
- Times: no line starts after the wave's `Duration` (T); 2 line lists are not in ascending time order.
- Audio exists only for `INT`: the Mac depot ships `*_LOC_INT.upk` packages only (directory listing; the
  `PCTOC_<LANG>.txt` files list other languages, but no `_LOC_<LANG>` package of another language is installed).
  So a non-English player hears the English recording with subtitles from `LocalizedSubtitles`. `asamu-import audio
  --lang DEU` does exactly that: it falls back to the INT audio (recorded as `audio_language` in the manifest) and
  writes the DEU lines (154 distinct waves, 508 lines).
- Struct elements of arrays store every member that is not `Transient`, including members equal to their defaults:
  first subtitle lines store `Time` = 0.0, the empty `LocalizedSubtitles` slots store an empty `LanguageExt` and an
  empty line list, `SoundClassAdjuster` elements store `PitchAdjuster` = 1.0 and `AmbientSoundSlot` elements
  `Weight` = 1.0 (both equal to the declared struct defaults). The one member never seen, `SoundClassAdjuster`'s
  `SoundClassName`, is flagged `Transient`. So array elements need no default merging. STRONG (every element
  inspected agrees; not counted exhaustively).

## SoundCue — CONFIRMED (T, exact consumption)

```text
UObject prelude, tagged properties
TMap<SoundNode, SoundNodeEditorData> EditorData:
    i32 Count, Count x { i32 Node (object reference), i32 NodePosX, i32 NodePosY }
```

- 604 non-default cues, 1,707 `EditorData` entries (T). 93 maps are empty (T): the 88 inline cues owned by ambient
  sound actors, their class default objects and the simple spline audio components, plus 5 cues that sit directly in
  a package. In 504 cues the map's keys are exactly the nodes reachable from `FirstNode` (T). The other 7 non-empty
  maps hold 29 extra keys that are not reachable nodes (positions of disconnected or deleted nodes), and no reachable
  node anywhere lacks an entry (T). The map is editor layout only; the runtime needs only `FirstNode`.
- Tags used: `SoundClass` (a name), `SoundClassName` (where stored, the unrepresentable `None` value, see
  `OBJECT_FORMAT.md`), `FirstNode`, `VolumeMultiplier`, `PitchMultiplier`, `Duration`, `MaxConcurrentPlayCount`.
  Defaults (`Engine.Default__SoundCue`, CONFIRMED, cdo): `VolumeMultiplier` 0.75, `PitchMultiplier` 1.0,
  `MaxConcurrentPlayCount` 16.
- `SoundClass` names an object of class `SoundClass` by name for every cue but one (T: 1 unresolved, `Announcer`,
  stock UDK content). Usage: `ASAMU_Game_SFX` 102, `ASAMU_Voice` 100, `Ambient` 88, `ASAMU_Narrator` 78,
  `Character` 59, `ASAMU_Music` 51, `ASAMU_Ambient` 38, 23 without a class, the rest under 20 each.
- `Duration` is a cooker-computed value. 10000 marks a cue that loops forever. Of the 572 distinct cues (T):
  156 contain an indefinitely looping node (`SoundNodeLooping` with `bLoopIndefinitely`, `SoundNodeAmbient`,
  `ForcedLoopSoundNode`); 154 of them store 10000 and 2 (the inline cues of the two `AmbientSoundSimpleToggleable`
  actors) store no `Duration` at all. No cue without such a node stores 10000; the 2 cues whose looping node has a
  finite count store a finite duration; 398 others store a finite value and 16 none. STRONG (complete agreement
  wherever a value is stored; the computing code was not read). Correction (verify-audio, 2026-10-10): an earlier
  version said all cues with such a node store 10000; the 2 toggleable inline cues do not.

## Cue graphs — CONFIRMED (T)

`SoundDecoder::cue_graph` walks `FirstNode` and every node's `ChildNodes`:

- 604 graphs, 1,728 nodes (T). Leaves: 784 waves (T), 2 `SoundNodeWaveParam` (stock content, waves supplied at run
  time by parameter name).
- 198 wave leaves are imports resolved in another package (T): map cues (narration, character lines) reference the
  waves of the map's `_LOC_INT` package by object path. The converter therefore loads the localized packages first.
- Some of those waves are cooked into several localized packages (the 83 copies), so a reference can match more than
  one loaded package. `SoundDecoder::locate_from` resolves a reference in a fixed order: an export of the holding
  package; its registered companions `<package>_LOC_*`; the package named by the path's first component; the
  `Startup*` packages; every registered package by name; only then the package set's own search. Every
  cross-package wave leaf of a map cue whose companion holds the wave resolves to that companion (T). Before this
  rule (found by verify-audio, 2026-10-10) the choice came from hash order: two identical runs of the converter wrote
  different `package` fields for 4 nodes of `cues.json`.
- No dangling reference and no cycle (T). 76 empty child inputs (T): `SoundNodeRandom` inputs left empty (each with a
  weight) and similar. 9 cues have no `FirstNode`. 5 node exports inside cues are not reachable from `FirstNode` (T).
- Longest path: 7 edges (T). Most cues are small: 239 distinct cues have one node, 117 two, 68 three.
- Nodes reached per class: `SoundNodeWave` 784, `SoundNodeModulator` 280, `SoundNodeAttenuation` 243,
  `SoundNodeRandom` 117, `SoundNodeLooping` 104, `SoundNodeAmbient` 77, `SoundNodeMixer` 76, `SoundNodeDelay` 34,
  `SoundNodeAmbientNonLoopToggle` 5, `SoundNodeModulatorContinuous` 4, `ForcedLoopSoundNode` 2,
  `SoundNodeWaveParam` 2.
- Every `SoundNode` subclass other than the wave stores tagged properties only (no native data, T).

Node values the runtime needs (property names from the class models in `Engine.u`; defaults are the classes' default
objects, CONFIRMED, cdo; a value absent from both the object and the defaults is zero / the enum's first entry):

| Node | Properties | Defaults (cdo) |
|---|---|---|
| `SoundNodeAttenuation` | `bAttenuate`, `bSpatialize`, `dBAttenuationAtMax`, `DistanceAlgorithm` (`SoundDistanceModel`), `DistanceType`, `RadiusMin`, `RadiusMax`, `OmniRadius`, `bAttenuateWithLPF`, `LPFRadiusMin`, `LPFRadiusMax` | `bAttenuate` true, `bSpatialize` true, `dBAttenuationAtMax` -60, `RadiusMin` 400, `RadiusMax` 4000, `LPFRadiusMin` 3000, `LPFRadiusMax` 6000 |
| `SoundNodeModulator` | `PitchMin`, `PitchMax`, `VolumeMin`, `VolumeMax` (the old distribution members are deprecated and never stored) | 0.95 / 1.05 / 0.95 / 1.05 |
| `SoundNodeModulatorContinuous` | `PitchModulation`, `VolumeModulation`: each references a `DistributionFloatSoundParameter` subobject (`ParameterName`, `MinInput`, `MaxInput`, `MinOutput`, `MaxOutput`) that maps a named run-time parameter to pitch / volume; the converter inlines those subobjects | - |
| `SoundNodeLooping` | `bLoopIndefinitely`, `LoopCountMin`, `LoopCountMax` | true, 1,000,000, 1,000,000 |
| `SoundNodeDelay` | `DelayMin`, `DelayMax` | 0, 0 |
| `SoundNodeRandom` | `Weights` (one per input), `PreselectAtLevelLoad`, `bRandomizeWithoutReplacement` | `bRandomizeWithoutReplacement` true |
| `SoundNodeMixer` | `InputVolume` (one per input) | - |
| `SoundNodeAmbient` (+ `NonLoop`, `NonLoopToggle`) | attenuation set as above with `DistanceModel`, `PitchMin/Max`, `VolumeMin/Max`, `SoundSlots` (`Wave`, `PitchScale`, `VolumeScale`, `Weight`); `NonLoop` adds `DelayMin/Max` | `RadiusMin` 2000, `RadiusMax` 5000, `LPFRadiusMin` 3500, `LPFRadiusMax` 7000, `PitchMin/Max` 1.0, `VolumeMin/Max` 0.7, plus the attenuation booleans and -60 dB |
| `SoundNodeWaveParam` | `WaveParameterName` | - |
| `ForcedLoopSoundNode` | none (an engine node used by the spline ambient sound components) | - |

`DistanceAlgorithm` values in use: `ATTENUATION_Logarithmic` (143 distinct attenuation nodes) and
`ATTENUATION_NaturalSound` (76); 7 leave it at the first entry, `ATTENUATION_Linear`. The enum's other entries are
`ATTENUATION_Inverse` and `ATTENUATION_LogReverse`.

## SoundClass and SoundMode — CONFIRMED (T)

- `SoundClass` native data is an `EditorData` map with the cue layout (`TMap<SoundClass, SoundClassEditorData>`). All
  47 non-default classes are in `Startup.upk` (package `SoundClassesAndModes`); only the root class `Master` has
  entries, 48 of them (T).
- Values: `Properties` (`SoundClassProperties`: `Volume`, `Pitch`, `StereoBleed`, `LFEBleed`,
  `VoiceCenterChannelVolume`, `RadioFilterVolume`, `RadioFilterVolumeThreshold`, `bApplyEffects`, `bAlwaysPlay`,
  `bIsUISound`, `bIsMusic`, `bReverb`, `bCenterChannelOnly`, `bApplyAmbientVolumes`), `ChildClassNames` (the class
  tree, by name; `Master` has `SFX`, `Music`, `Voice`), `bIsChild`. The default object stores the complete
  `Properties` struct (`Volume` 1.0, `Pitch` 1.0, `StereoBleed` 0.25, `LFEBleed` 0.5, `bReverb` true, the rest zero);
  each class stores only its deltas, which the converter merges member-wise.
- `SoundMode` (17 non-default, T) stores tagged properties only: `bApplyEQ`, `EQSettings`, `SoundClassEffects`
  (array of `SoundClassAdjuster`: `SoundClass` name, `VolumeAdjuster`, `PitchAdjuster`, `bApplyToChildren`,
  `VoiceCenterChannelVolumeAdjuster`), `InitialDelay`, `FadeInTime`, `Duration`, `FadeOutTime`. Kismet switches modes
  with `SeqAct_SetSoundMode` actions (present in the maps; decoding them is the Kismet workstream's).

## Ambient sound actors and reverb volumes — CONFIRMED (T)

205 ambient sound actors in the nine maps with audio (T): `AmbientSound` 119, `AmbientSoundSimple` 75,
`AmbientSoundNonLoopingToggleable` 5, `AmbientSoundSpline` 3, `AmbientSoundSimpleToggleable` 2,
`AmbientSoundSplineMultiCue` 1. All 205 are listed in their level's `ULevel::Actors`, and every one has a cue that
resolves (T).

- `AmbientSound*` (cue-based): the actor's `AudioComponent` subobject names the `SoundCue` and its
  `VolumeMultiplier` / `PitchMultiplier`. The component's values are its own tags over its template (the class
  default object's `AudioComponent0`), resolved through the export's archetype. `bAutoPlay` defaults to true
  (`Default__AmbientSound`).
- `AmbientSoundSimple*`: the actor owns an inline `SoundNodeAmbient*` (`AmbientProperties` = `SoundNodeInstance`) and an
  inline `SoundCue` (`SoundCueInstance`) whose `FirstNode` is that node. 82 actors carry such a node (T); their
  `SoundSlots` hold 81 entries naming 10 distinct waves.
- Spline variants use `SplineAudioComponent` / `MultiCueSplineAudioComponent` components whose own tags hold the
  virtual speaker `Points`, `ListenerScopeRadius` and, for the multi-cue variant, `SoundSlots` with one cue each. The
  converter keeps those component tags.
- 43 reverb volumes (T): `Settings` (`ReverbPreset` enum, volume, fade time), `AmbientZoneSettings`, `Priority`,
  `bEnabled`. Their brush geometry belongs to the level scenes (`LEVELS.md`, `LEVEL_FORMAT.md`).

Linking to the level data: each actor record carries the map package, its export index, object name, path and
`ULevel::Actors` slot, which are the keys of the level scenes written by `asamu-import levels`.

## Converter: `asamu-import audio`

Writes to `<out>/audio/` (user-local; the repository, except git-ignored `research/`, and the install are refused):

| File | Content |
|---|---|
| `waves/<Outer>/<Name>.ogg` | each distinct wave's `CompressedPCData`, byte for byte (`.wav` only for a headerless `RawData` payload, wrapped in a 44-byte RIFF header we write ourselves; no such payload exists in the Mac cook) |
| `manifest.json` | wave path -> package, copies (`also_in`, `differs_in`), file, format, source slot, bytes, `samples` (frames per channel: the stream's final granule, i.e. the exact decoded length; frames of a WAV), duration, channels, sample rate, `RawPCMDataSize`, looping flag, volume, pitch, language, subtitle line count |
| `cues.json` | cue path -> sound class, first node, multipliers, duration, max concurrency, FaceFX fields when set, nodes (path, class, kind, children, values over defaults, inlined distributions, depth; waves summarised), wave list, graph checks |
| `subtitles.json` | wave path -> lines (text, time) in `--lang`, flags, languages available, playing cues and their sound classes |
| `sound_classes.json` | the 47 sound classes (merged `Properties`, child names) and 17 modes |
| `ambient.json` | per map: ambient sound actors (placement, cue, component values, ambient node with slots) and reverb volumes |
| `coverage.json` | the coverage report (`--check` prints it without writing) |

Run on the install (local probe): 768 audio files (58 MB), 572 distinct cues, 154 subtitled waves with 504 INT lines,
47 classes, 17 modes, 9 maps with ambient audio, 0 failures, in under 2 s.

Options: `--lang` (default `INT`; validated), `--package` filters, `--name`, `--limit`, `--no-audio`, `--force`,
`--dry-run`, `--check [--json]`. Each run rewrites the JSON files from the packages it selected; existing audio files
are kept unless `--force`.

Safety and determinism of the writer (tests in `tools/asamu-import/src/audio.rs`):

- A payload is written only after its container checks pass: an Ogg stream must parse with correct checksums, one
  logical stream from BOS to EOS, no sequence/continuation errors, non-decreasing granules, no unterminated packet
  and (for Vorbis) the three header packets; a RIFF payload must have a consistent PCM format; a WAV we build is
  parsed back as a self-check. Damaged payloads are refused and counted as failures, never written.
- Output names come from object paths reduced to `[A-Za-z0-9_-]` components (Windows device names prefixed), so no
  path can climb out of `waves/`; names that collide after sanitizing or differ only in case are refused. Directories
  are created one component at a time without following links; an existing link, or a file where a directory is
  expected, is refused; existing files are kept unless `--force`, which replaces the directory entry (rename) instead
  of writing through a link. The output root is refused inside the repository (except git-ignored `research/`), inside
  an `.app` bundle or `steamapps` tree, and inside the install root.
- Two runs over the same install write byte-identical JSON (T, `json_output_is_deterministic_on_real_data`).

## Code

- `crates/asamu-ue3/src/sound.rs`: `SoundDecoder` (waves, cues, nodes, graphs, classes, modes, ambient actors and
  reverb volumes, value merging over archetypes and class defaults), `read_wave_native`, `read_editor_map`,
  `parse_ogg` / `ogg_crc` (Ogg pages, CRC, Vorbis headers), `wav_file` / `parse_wav`, `sniff_payload`,
  `SoundCoverage`.
- `tools/asamu-import/src/audio.rs`: the converter.
- Tests: `crates/asamu-ue3/tests/sound.rs` (synthetic packages written byte by byte, hand-made Ogg stream with an
  independent bitwise CRC, hand-made WAV, cross-package cue import, unreachable node, cyclic cue, Ogg continuation /
  granule / unterminated-packet / multiplexing damage, inconsistent WAV formats, deterministic resolution of an object
  held by several packages in any insertion order, corruption and truncation sweeps),
  `crates/asamu-ue3/tests/sound_real_data.rs` (the (T) claims; skips without the install), and the converter's unit
  tests in `tools/asamu-import/src/audio.rs` (payload validation, hostile object paths, links and traversal in the
  output tree, refused output roots, deterministic JSON on the install).

## Open questions

- Vorbis decoding is not implemented in this repository; the runtime is expected to play the `.ogg` files with an
  existing decoder. The workspace enables Bevy 0.20's `audio` feature, which is `bevy_audio` + `vorbis` in the Bevy
  0.20.0 manifest (CONFIRMED), so Ogg Vorbis playback is compiled in; `wav` is not enabled (no WAV is produced from
  the Mac cook). Whether Bevy's decoder trims these streams to the final granule like vorbisfile is UNKNOWN (FFmpeg
  does not for 133 of them, see above); the manifest's `samples` gives the exact length.
- 75 waves of the localized packages (distinct paths) are not reached by any cue graph in the packages. They are
  likely referenced directly from Kismet (`SeqAct_NarratorLine`, `SeqAct_PlaySound`) or Matinee sound tracks, which
  this workstream does not decode. TENTATIVE.
- How the game picks the subtitle language slot and what the 9 empty slots are: UNKNOWN.
- The meaning of `bLoopingSound` on PC and the engine's exact attenuation curves (`SoundDistanceModel`) need the
  native audio code or recorded traces: UNKNOWN / not attempted.
- Music: ASAMU plays music through cues of class `ASAMU_Music` placed as ambient sounds or triggered by Kismet; a
  dedicated music manager was not looked for here.
