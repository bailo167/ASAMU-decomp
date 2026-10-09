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

## Audio runtime

How the original plays what the sections above decode, and how our runtime reproduces it. Rules are written in our own
words; nothing decompiled or scripted is reproduced. Code: `crates/asamu-assets/src/audio.rs` (device-free evaluator,
classes and modes, narrator, subtitles, ambient actors, `AudioCommand`), `apps/asamu/src/audio.rs` with
`apps/asamu/src/audio/{backend,gameplay}.rs` (Bevy playback, gameplay events).

Evidence sources:

- **Native**: the unstripped Mac executable, read locally with the committed Ghidra script
  (`tools/ghidra-scripts/DecompileToLocal.java`, output in git-ignored `research/decompiled/`) for these symbols:
  `ParseNodes` of every `USoundNode*` class, `NotifyWaveInstanceFinished` of the looping, concatenator and ambient
  nodes, `GetDuration`, `USoundNode::CalculateAttenuatedVolume` and its helper `AttenuationEval`,
  `USoundNode::CalculateLPFComponent`, `USoundCue::CalculateMaxAudibleDistance`, `UAudioComponent::{Play, Stop,
  FadeIn, FadeOut, AdjustVolume, UpdateWaveInstances}`, `FAudioComponentSavedState::Set`,
  `FWaveInstance::NotifyFinished`, `USoundNode::ResetWaveInstances`, `UAudioDevice::{GetSortedActiveWaveInstances,
  ApplySoundMode, ApplyClassAdjusters, RecurseIntoSoundClasses, RecursiveApplyAdjuster, Interpolate}`,
  `FALSoundSource::{Init, Update}`, `FSoundSource::SetStereoBleed`, `FSubtitleManager::QueueSubtitles`,
  `AActor::{SetTimer, UpdateTimers}`. Float constants were read from the binary's data at the addresses those
  functions load (a throwaway Mach-O segment reader).
- **Native, second pass** (verify-audio-runtime, 2026-10-10; same script, local output only):
  `USoundNodeWave::HandleStart`, `UAudioComponent::Cleanup`, `UAudioDevice::StopSources`,
  `FSoundSource::Stop`, `FSubtitleManager::{FindHighestPrioritySubtitle, DisplaySubtitles}`,
  `USoundNode{Attenuation, Ambient, Looping, DistanceCrossFade}::MaxAudibleDistance`,
  `USoundCue::{IsAudible, CalculateMaxAudibleDistance, GetCueDuration}`, `UWorld::{Tick, GetAudioTimeSeconds}`; the
  `ParseNodes` of the wave, attenuation, random, mixer, modulator, looping, delay, concatenator, cross-fade, ambient
  and non-looping ambient nodes, their `NotifyWaveInstanceFinished`, `UAudioComponent::{Play, Stop, FadeIn, FadeOut,
  UpdateWaveInstances}` and `FALSoundSource::{Update, IsFinished}` re-read against our code. Every constant of the attenuation
  curves, the source update and the voice selection was re-read independently from the binary with our own
  segment reader (1, −1, 0.25, 0.02, 10, 20, 1.25, 0.4, 2.0, 1e-4 as a double, 10000, 524288): all agree.
- **Class models / defaults** (`asamu-inspect class|defaults`, CONFIRMED): property order of the sound nodes (every
  range pair is declared `Min` then `Max`), the `SoundDistanceModel` and `ESoundDistanceCalc` enum orders, struct
  defaults (`AmbientSoundSlot`, `DistanceDatum`, `SoundClassProperties`), `Engine.Default__AudioComponent` (fade stop
  times −1, targets 1, `bAllowSpatialization` true, no `SubtitlePriority`), `Engine.Default__AmbientSoundSimpleToggleable`
  (`FadeInDuration` 1, `FadeInVolumeLevel` 1, `FadeOutDuration` 1, `bAutoPlay` false), `asamu.ASAMUSoundGroup`
  (footstep / jump / landing / hard-landing tables by physical material), and the cue references of `ASAMUPawn`,
  `GrappleGun`, `ASAMUPowerJump`, `ASAMURocketBoots`, `ASAMURechargeCrystal`, `ASAMUGlowFlower`.
- **Config**: `BaseEngine.ini` `[ALAudio.ALAudioDevice] MaxChannels=32` (CONFIRMED).
- **Script** (local reading under git-ignored `research/`; STRONG): `ASAMUNarratorManager`, `SeqAct_NarratorLine`,
  `ASAMUSoundGroup`, `ASAMUPawn`, `GrappleGun`, `ASAMUPowerJump`, `ASAMURocketBoots`, `ASAMURechargeCrystal`,
  `ASAMUGlowFlower`, and the engine's `PlayerController` (`ClientHearSound`, `Kismet_ClientPlaySound`) and
  `AmbientSoundSimpleToggleable`.
- **Kismet census** (local `asamu-inspect kismet --json`, numbers only): 101 `SeqAct_NarratorLine` (delay 1.0 in 86,
  0 in 11, 1.5/2.0/2.5 in the rest; `removeAllOtherCues` never set; volume 1.0 in 83, 0.8 in 18), 214 engine
  `SeqAct_PlaySound`, 12 `SeqAct_SetSoundMode` (`ASAMU_FadeIn`, `ASAMU_SFX_Ducking`, `ASAMU_TheCore_Music`, the two
  `ASAMU_Epilogue_*`, `ASAMU_Default`).

### Component model — CONFIRMED (native)

A playing cue is an audio component. Every audio update the component adds the frame time to its playback time,
resets its working values (volume 1, pitch 1, high-frequency gain 1, not spatialised, no loop notification, no
notification hook, finished = true) and walks the cue graph from `FirstNode`. Each node multiplies or overrides the
working values and then visits its children; wave leaves create or refresh a **wave instance** keyed by the wave, the
parent node and the child index. A wave instance that has not finished takes a snapshot of the working values, is
marked started, clears its "hook already notified" flag and clears the component's "finished"; a finished one is left
alone. When the walk leaves "finished" set, the component stops. Node state that must persist (a random choice, a
modulator's draw, a loop counter, a delay's start) is kept per component and node, with a "needs initialisation" flag
set when the component starts. Stopping a component (`Cleanup`) kills its subtitles, frees its wave instances and node
state, and resets its playback time to 0 and its fades to the defaults.

The engine walks a node once per path that reaches it (no shared-node bookkeeping), so a crafted graph that shares
nodes costs 2^depth visits; no shipped cue shares a node. Our evaluator stops after 4096 node visits per instance and
update (a guard for crafted data only).

Per node:

| Node | Behaviour |
|---|---|
| Attenuation | Only if the component allows spatialisation: distance from the source to the listener (full 3-D, or one axis for the three `InfiniteXX` distance types); `bAttenuate` multiplies the volume by the curve below; `bAttenuateWithLPF` sets the high-frequency gain (1 inside `LPFRadiusMin`, 0 from `LPFRadiusMax`, linear between); `bSpatialize` is OR-ed in; `OmniRadius` copied. Without spatialisation it only clears the spatialise flag. Then all children. |
| Random | Once per play (or after a loop re-initialises it): sum the weights (without replacement: of the inputs not used yet), draw `r = frand · sum`, walk the inputs subtracting each weight until `r ≤ weight` (without replacement the input must also be unused; it is then marked used). The used list lives **on the node**, shared by every component, and resets (keeping only the latest choice) when every input has been used. Quirk kept: used inputs leave the sum but are still subtracted during the walk, which biases the next choice. An empty input chosen plays nothing (the grunt cues rely on this). Clears loop notification. |
| Mixer | Every non-empty input with the volume times `InputVolume[i]`; the working values are saved and restored around each input. Clears loop notification. |
| Modulator | Once per play: volume, then pitch, each `f · (Min − Max) + Max`; multiplies both. |
| ModulatorContinuous | Every update: volume and pitch from the distributions (a named float parameter of the component mapped through `MinInput/MaxInput → MinOutput/MaxOutput`; mapping details TENTATIVE). |
| Looping | Once per play: a remaining count `trunc(f · (Min − Max) + Max)`; while indefinite or the count is positive it becomes the notification hook and sets loop notification. |
| Delay | Clears loop notification; once per play draws the delay and remembers the playback time; children only once the delay has passed, otherwise it keeps the component alive. |
| Concatenator | Plays input `index` (starting at 0) with its `InputVolume`; becomes the hook unless on the last input; each finished wave advances the index. |
| DistanceCrossFade | Every input with a gain from its distance window: linear fade-in between the two fade-in distances, full `Volume` between fade-in end and fade-out start, linear fade-out after, silence outside. Clears loop notification. |
| WaveParam | The wave set on the component under `WaveParameterName`, else the children. |
| Wave | Multiplies the wave's own `Volume` and `Pitch`, then the wave instance as above. The wave's `bLoopingSound` is **not read**: looping comes only from the graph (STRONG; the wave parse ignores it, other readers were not searched). |
| Ambient (inline node of `AmbientSoundSimple*`) | Once per play: volume, pitch in their ranges. Every update: always the 3-D distance (it ignores the component's spatialisation switch), the curve, LPF, spatialise; it becomes the hook and plays **every** slot at once with `VolumeScale` / `PitchScale`, each looping forever at the source. Slot weights are unused. |
| AmbientNonLoop | Once per play and after each sound: volume, pitch, a delay, then one slot by weight (cumulative weight ≥ `f · sum`, else the last). Keeps the component alive; plays the slot's wave when the delay has passed. The toggle variant stops the component after one sound. |

Attenuation curves (`AttenuationEval`, constants read from the binary; re-verified by the second pass, and by
hand-computed values in `attenuation_curves_match_hand_computed_values`): silence from `RadiusMax` on, full volume up
to `RadiusMin`; between them, with `t = (d − min)/(max − min)`: linear `1 − t`; logarithmic `ln(d/max)/ln(min/max)`
(scale 0.25 instead when `min` = 0, i.e. `−0.25 · ln(d/max)`), capped at 1; inverse `0.02 · (max/d) · (max/min)`
(`max/min` read as 1 when `min` = 0), capped at 1; log-reverse `1 − ln(1/(1 − d/max)) / −ln(min/max)` (the same 0.25
scale when `min` = 0), floored at 0; natural sound `10^(t · dBAttenuationAtMax / 20)`. An unknown enum value applies
no curve. (This settles the open question above about the curves.)

Audible distance (`USoundCue::CalculateMaxAudibleDistance`, CONFIRMED): the largest value any node of the graph
reports; attenuation and ambient nodes report `RadiusMax`, a cross-fade node its largest `FadeInDistanceEnd` /
`FadeOutDistanceEnd`, and a **looping node `WORLD_MAX` (524288)**, so a cue containing a looping node is always
audible; 0 becomes `WORLD_MAX`. `IsAudible` (sounds played by other actors) compares the squared distance with it.
Correction (verify-audio-runtime): the first version of the runtime ignored looping nodes and used
`FadeInDistanceStart`.

Loops and wave ends (`NotifyFinished` and the hooks): a wave instance that finishes is marked finished and its hook
node is told. A looping node (count left) waits until every started wave under it has finished, then counts down,
re-initialises the nodes it currently reaches (new random choice, new modulator draw, new delay) and restarts those
waves; a finite loop therefore plays `count + 1` times, matching the cooked duration rule `(LoopCountMax + 1) ·
child`. A wave reached from a looping node through only attenuation/modulator nodes loops seamlessly at the source and
notifies at each wrap. The ambient node keeps its slots looping.

Component volume and voices: final gain = node-chain volume × component `VolumeMultiplier` × cue `VolumeMultiplier` ×
fade-in × fade-out × adjust-volume × sound-class volume, then ×1.25 for waves of exactly two channels of a class with
non-zero `StereoBleed` (every class), clamped to [0, 1] by the source; final pitch = node-chain pitch × component and
cue pitch multipliers × class pitch, clamped to **[0.4, 2.0]** by the source. A wave instance takes a voice only if
its priority (node-chain volume, +1 for `bAlwaysPlay` classes, + the class's radio-filter volume, which is non-zero
only for the unused `DialogRadio` class) exceeds 0.0001; the 32 highest priorities play. A component whose cue
duration (`GetCueDuration`: the cooked `Duration`, or the graph's when none is stored) is below 10000 is stopped once
it has played longer than duration / 0.4. `Play` is refused when the cue already plays `MaxConcurrentPlayCount`
times. Stereo waves are never spatialised (the wave parse warns about it).

Losing a voice (CONFIRMED, `StopSources` and `FSoundSource::Stop`): a source whose wave instance is no longer among
the voices is stopped, and stopping a source marks its wave instance finished and notifies the hook node (unless it
was already notified), so a one-shot that drops out (its channel taken, attenuated to nothing, its branch no longer
parsed) does not come back; a looping node restarts its subtree, an ambient node keeps its slot alive. Wave instances
that did not make the 32-channel cut are marked finished at once (without notification) unless they loop forever at
the source (ambient slots) or their component has `bShouldRemainActiveIfDropped`, which is set in the
`AmbientSound` component template (CONFIRMED, cdo; inherited by the subclasses' components, STRONG). Correction
(verify-audio-runtime): the first version of the runtime let every dropped wave wait for a channel and restart.

Fades: `FadeIn(d, v)` ramps the fade-in multiplier to `v` over `d` and calls `Play`, unless a fade-out is running,
which it reverses from the current level without restarting. `Play` on a component that is still playing restarts it
**and resets the fade values**, so `FadeIn` on a playing component (no fade-out running) restarts at full volume
(CONFIRMED, `Play`); on a stopped component the fade applies from playback time 0 (`Cleanup` reset it). `FadeOut(d,
v)` ramps down (or, while fading in, from the current level) and the component **stops** when the fade-out ends; a
negative duration stops at once.

Sound classes and modes: effective class values multiply volume and pitch down the tree from `Master` (`SFX`, `Music`,
`Voice` at 0.8 each in the game) and inherit `bIsUISound` / `bIsMusic`; a class outside the tree gives no class factor.
`SetSoundMode` with a name that is not a loaded mode changes nothing (CONFIRMED); setting the mode already active
changes nothing.
A mode applies its adjusters (multiplying; `bApplyToChildren` reaches every descendant) and interpolates linearly
from the current values, starting after `InitialDelay` over `FadeInTime`; a mode with a non-negative `Duration`
returns to the base mode after it, over its `FadeOutTime`; a mode with a negative duration becomes the base. Which
mode is the base at start-up is not set in the shipped config (our runtime starts with none: TENTATIVE). While the
game is paused only `bIsUISound` classes advance.

Subtitles (CONFIRMED, `USoundNodeWave::HandleStart`, `FSubtitleManager::{QueueSubtitles,
FindHighestPrioritySubtitle}`): when a **wave instance is created** (whether or not it gets a voice) for a wave with
lines, on a component with a non-zero `SubtitlePriority` and subtitles not suppressed, its lines are queued **keyed by
that wave instance**, with start times offset by the audio clock and the wave's own `Duration` (not scaled by pitch;
a line later than that is clamped to it), plus an empty line at the end; lines with a negative time are kept as
stored. The audio clock (`GetAudioTimeSeconds`) is a `WorldInfo` time that `UWorld::Tick` advances by the raw frame
time **only while the game is not paused**. Each entry keeps a cursor that moves on when the next line has started (so
in the 2 shipped lists that are not in time order, a line followed by an earlier one is skipped); an entry is dropped
when its cursor reaches the end marker; the shown line is the highest-priority entry whose current line has started;
a stopping component kills its entries. Ties resolve to the most recent queue in our runtime (TENTATIVE: the engine
keeps whichever entry its hash set visits last). Only the narrator component (priority 10000, script) and Kismet
`SeqAct_PlaySound` components (10000, engine script) have a priority; ambient sounds and gameplay sounds keep 0 and
show no subtitles. Corrections (verify-audio-runtime): the first version queued when a voice started (again after a
lost voice), keyed by component, scaled the duration by the pitch, timed lines on real time (they ran on while
paused) and could pick a higher-priority entry whose line had not started.

Spatialisation of gameplay sounds (engine script, STRONG): a sound the pawn plays on itself (it is the view target)
is not spatialised at all, so attenuation nodes do nothing for it; sounds of other actors — including the grapple
gun, the power-jump and rocket-boots actors attached to the pawn — are heard from their location.

### Narrator — STRONG (script) with native timer rules

The narrator keeps a FIFO of lines (id, cue, volume, delay, Kismet node). Adding to an empty queue fires
`SeqEvent_NarratorEvents` "StartedNarrating" and plays the line at once — its delay is unused. A line ends on a timer
of the cue's cooked `Duration` (not the audio): its action's "FinishedLine" output fires, the line is removed, and
the next line starts after **its** delay; when none is left "FinishedNarrating" fires. Quirks kept: a timer of 0
seconds never fires (CONFIRMED, native timers drop rate-0 timers), so a queued line with delay 0 stalls the queue;
removing the playing line stops its audio at once but not its end timer, which later ends the next line before it
played; "remove all other cues" skips the playing line (its loop stops before index 0); removing a line removes
**every queued line equal to it** (id, cue, volume, delay and node: the same Kismet action fired twice); the delayed
start plays the cue noted when the previous line ended, at the volume of the line first in the queue by then, with an
end timer of that line's cue duration (an emptied queue gives volume 0 and no end timer).

Timers (CONFIRMED, `AActor::UpdateTimers`): every count grows by the actor's frame time first (after world time
dilation, which the runtime does not model), then in order a rate-0 timer is removed unfired and a timer fires once its count **exceeds** its rate; a timer set while another
fires starts at 0 and is not advanced in that tick. Correction (verify-audio-runtime): the first version fired at
equality and advanced a delayed start in the tick that set it (each delay ran one frame short).

The Kismet runtime (`asamu-kismet`, `narrator.rs`) has its own port of this queue on the simulation tick; with it the
audio side only plays and stops the narrator sound (`NarratorPlay` / `NarratorStop`, below). The two queues must not
both run.

### Gameplay sounds — STRONG (script), cues CONFIRMED (cdo)

| Moment | Cue(s) (`asamu_assets::audio::gameplay_cues`) |
|---|---|
| Jump from the ground (not a power jump) | material jump cue + `TheHand_Jump_Grunt_Cue` |
| Landing with `V.z ≤ −500` (`normalLandSoundVelocityThreshold`) | material landing cue (hard-landing table when `V.z < −2000`) + `TheHand_Land_Grunt_Cue` |
| Footstep: walk-bob phase `trunc(π/2 + 9·BobTime/π)` changes while walking faster than 10 UU/s | material footstep cue; sprinting adds `Sprinting_Rustle_Cue` and `Footsteps_Rock_Srpinting_Cue` |
| Pawn enters `FallingState` / `HasLanded` | wind loop `Player_Falling_Wind_Cue` starts / stops; float parameter `FallingWindParam = abs(V.x + V.y + V.z)` every 0.1 s (until the parameter is set the cue's continuous modulator reads its constant, 0: silent) |
| Grapple fire fails | `GrapplingGun_Fail_Cue` |
| Grapple attaches | `GrapplingGun_Decal_Cue` at the hit, `GrapplingGun_Beam_Start_Cue`, beam loop `GrapplingGun_Beam_Cue` faded in over 0.5 s; while attached `GrapplingBeamParam = fMaxDistance − distance` |
| Grapple releases | `GrapplingGun_Beam_Stop_Cue`, beam faded out over 0.2 s |
| Charged crystal grappled | `GrapplingGun_Recharged_Cue` and `Crystal_Drained_Cue` at the crystal |
| Power jump: charging / charged / fired / cancelled | charge loop in (0.1 s) / light cue, charge out (0.2 s), static loop in (0.1 s) / jump sounds + `PowerJump_Jump_Cue` (+ `PowerLeap_Jump_Cue` for a leap), static out (0.6 s) / both out (0.3 s) |
| Rocket boots: start / boost / landing cancels / exhausted | `RocketBoots_Charge_Cue` / `RocketBoots_Blast_Cue` (`FadeIn(0, 1)` on its component: reverses a running fade-out) / blast out (0.2 s) + `RocketBoots_Stopped_Cue` / `RocketBoots_Exhausted_Cue` |
| Player dies | `Death_Blackout_Cue`, sound mode `ASAMU_Death`, `ASAMU_Default` again 0.3 + 0.6 s later |

Material tables: a known physical material selects its entry and is remembered; an unknown or empty one reuses the
remembered entry (rock at start). Landing and hard landing share one remembered index (an index valid only in the
longer table gives no hard-landing sound; kept). The converted collision has no physical materials yet, so the
runtime always uses the remembered (rock) entries. Goat-mode variants and the workshop-mode jump suppression are not
ported. The pawn's `Release` → `FallingState` hand-over after 1 s is not modelled by the simulation; the runtime
emulates it for the wind (TENTATIVE).

### Runtime (our implementation)

- `AudioLibrary::load(<converted>/audio)` reads the five documents (bounded, format/version checked, manifest file
  paths validated); `read_wave_file` reads one `.ogg` and validates the whole container (`validate_ogg_vorbis`: every
  page in bounds with a correct checksum, one logical stream from BOS to EOS with consecutive sequence numbers, the
  three Vorbis headers with a sane identification header) before Bevy decodes it.
- `AudioEngine::update(&lib, listener_uu, dt)` evaluates every instance and returns the voices (loudest first, at
  most 32) with final gain, pitch, spatial flag, position, loop flag; plus Kismet feedback and the subtitle line.
  Deterministic for a seed (`UeRand`, the engine's LCG).
- Ambient actors (`ambient.json`): `load_ambient` starts the auto-playing actors of the map; spline actors are heard
  from the closest point of their polyline (TENTATIVE: the engine's virtual speakers are not ported); multi-cue
  splines play their cue with the slot's volume scale (TENTATIVE); toggleable actors follow Kismet `SeqAct_Toggle`
  (on / off / toggle, with their fades; addressed by object name or path; the plain `AmbientSound` has no toggle
  handler). Ambient components remain active when they lose their channel (above). The toggleable actors' checkpoint
  record (`bCurrentlyPlaying`) is not saved yet.
- Bevy: distance attenuation is the original's; Bevy only pans (voices placed half a unit from the listener, ears one
  unit apart, so Bevy's own falloff never applies). The distance low-pass filter is computed but not applied.
  Without an audio device Bevy plays nothing (`bevy_audio` warns and its playback system does not run; our voice
  entities simply never get sinks); the engine, subtitles and Kismet feedback still run.
- Bevy unwraps the decoder it creates (`bevy_audio` 0.20 `AudioSource::decoder`), so the IO task also opens the
  decoder once inside `catch_unwind` and refuses a stream it cannot open. A voice whose audio finishes loading late
  starts part-way in, except looping voices (Bevy applies the start offset inside the repeat, which would cut every
  repetition).
- Measured (local, all 768 converted waves through Bevy's decoder without an audio device): 625 decode to exactly the
  final-granule frame count, the other 143 differ by at most 34.5 ms (the same files and cause as the FFmpeg note
  above, STRONG). This replaces the UNKNOWN above about Bevy's trimming.
- Kismet API: `apps/asamu/src/audio.rs` `AudioCommandMessage(asamu_assets::audio::AudioCommand)` in,
  `AudioFeedbackMessage(asamu_assets::audio::AudioFeedback)` out; commands: `PlaySound` / `StopSound` (optional
  `node`: the `SeqAct_PlaySound` action, so a stop affects exactly that action's sounds), `NarratorAddLine` /
  `NarratorRemoveLine` (our queue; optional `node`), `NarratorPlay` / `NarratorStop` (one line now, for the Kismet
  runtime's narrator queue), `SetSoundMode`, `ToggleAmbient`, `StopAll`.

Tests: `cargo test -p asamu-assets --lib audio` (67: synthetic cues for every node, curves, fades, voice limit,
modes, narrator quirks, subtitles, ambient actors, hostile graphs, Ogg validation, and hand-computed cases whose
expected values were worked out independently from the formulas and the engine's random generator — random choices
with and without replacement, modulator draws, loop counts, delays, ambient slot choice, attenuation values; plus
`real_converted_audio_evaluates`, which validates every converted audio file and runs every converted cue and ambient
set when `ASAMU_CONVERTED_DIR` points at a converted directory and skips otherwise) and `cargo test -p asamu audio`
(gameplay events on the graybox; `real_converted_waves_decode` decodes converted `.ogg` files through the runtime's
load path and Bevy's decoder, the first 64 or every file with `ASAMU_AUDIO_DECODE_ALL=1`).

Real-data result (local, 2026-10-10): 768 files validated; all 572 cues evaluate; 504 produce voices within 4 s at
300 UU and 510 within 12 s. The silent ones: 8 cues without a first node (the 4 class-default inline cues, the 3
worm loops of `Dark_Cave_worm` and the front end's `NoSoundCue`); 43
ambient nodes and 2 attenuation nodes whose `RadiusMax` is under 300 UU; 2 `SoundNodeWaveParam` cues with no wave
set; delays longer than the run; the continuous modulator of the falling wind before its parameter is set; and cues
whose random choice picked an empty input (the grunts by design). Looping over a random node with empty inputs stops
the component for good when an empty input is chosen (no wave clears "finished" and nothing notifies the loop): the
Workshop `Wood_Creak_Cue` (4 of 7 inputs empty) and the epilogue tuba/cello music (1 of 2) — the original's behaviour
under the confirmed node rules (STRONG), kept.

Updates to the open questions above: the attenuation curves are now CONFIRMED (this section); `bLoopingSound` is not
read when a wave plays (STRONG); Bevy's decoded lengths are measured (above); a music system exists in script
(`ASAMUAdaptiveMusicManager`, `ASAMUAdaptiveMusicTrack`, Kismet `SeqAct_AddAdaptiveTracks` /
`SeqAct_SetAdaptiveTrackVolumeMultiplier` / `SeqEvent_TrackBeat`) and is **not** implemented yet; music placed as
ambient sounds plays. Not ported: reverb volumes and interior settings, occlusion, the radio filter, Doppler /
oscillator / enveloper nodes (no shipped cue reaches them), FaceFX, time dilation of the narrator timers, the
graph-computed duration for the 16 cues that store none (no safety stop for them), and the `OnQueueSubtitles`
delegate (not bound by the ASAMU script read). Not known: whether `FindHighestPrioritySubtitle` also moves its cursor
past a line that has not started yet (the decompiled condition reads that way; our cursor waits, TENTATIVE).

## Adaptive music

How the original layers and mixes its level music, and our port. Rules are in our own words; nothing scripted or
decompiled is reproduced. Code: `apps/asamu/src/audio/music.rs`. This supersedes the "not implemented yet" note at
the end of "Audio runtime".

Evidence:

- **Script** (src): local reading, under git-ignored `research/local/`, of the `ScriptText` of
  `ASAMUAdaptiveMusicManager`, `ASAMUAdaptiveMusicTrack`, `ASMAMUAdaptiveVolumeMultiplier`, `ASAMUBeatEventActor`,
  `SeqAct_AddAdaptiveTracks`, `SeqAct_SetAdaptiveTrack`, `SeqAct_SetAdaptiveTrackVolumeMultiplier`,
  `SeqAct_EditMultiplierForAllTracks`, `SeqEvent_TrackBeat`, `SeqVar_ASAMUAdaptiveMusicTrack`, `ASAMUGameInfo` and its
  subclasses (`asamu-inspect scripttext`), plus a search of all 172 `asamu` classes for callers of the manager.
- **Class defaults** (cdo): `asamu-inspect props <Startup.upk> asamu.Default__<class>` (and the component
  `asamu.Default__ASAMUAdaptiveMusicTrack.AudioComponent`).
- **Native**: `UObject::execEqualEqual_StrStr` (UnrealScript string `==`) calls `wcscmp`, while
  `execComplementEqual_StrStr` (`~=`) calls a case-insensitive compare
  (`otool -tV -p __ZN7UObject21execEqualEqual_StrStrER6FFramePv <ASAMU executable>`); the audio component rules of
  "Audio runtime" above (`AdjustVolume`, `GetAdjustVolumeOnFlyMultiplier`, `Play`).
- **Census**: local `asamu-inspect kismet --json` of the maps and the converted `cues.json` (names and numbers only).
- **Verification pass** (verify-music, 2026-10-10; every claim of this section re-derived independently, local output
  only): the same `ScriptText`s; `asamu-inspect calls` over all 172 `asamu` classes (bytecode; identifiers only):
  `GetTrackFromID`, `GetVolumeMultiplier` and `RemoveVolumeMultiplier` compare with the native `EqualEqual_StrStr`
  (#122), and no class calls `AddMultiplierToTrack`, `RemoveMultiplierFromTrack` or `AddQueuedTracks`; `otool -tV` of
  `USequence::FindSeqObjectsByClass` and `USequence::execFindSeqObjectsByClass`; the cdos above; a fresh kismet census
  of all 12 maps; a `--no-audio` conversion for the cue shapes; the converted Kismet graphs of the three music levels run
  through `asamu-kismet` (test `real_converted_kismet_adds_the_shipped_tracks`). No discrepancy with the section as
  first written; additions are marked "(verify)".

### Lifecycle — CONFIRMED (src)

- The game info owns one music manager, a plain object created in `InitGame`. The manager is initialised twice: in
  `InitGame`, and in `SpawnDefaultPawnFor` right after the player pawn is spawned. Initialising runs, over every root
  sequence of the loaded levels (nested sequences searched), the activation of each `SeqAct_AddAdaptiveTracks`
  directly, then initialises each `SeqEvent_TrackBeat`.
- (verify) The search results accumulate across root sequences: `FindSeqObjectsByClass` appends to the caller's array
  without clearing it, and the exec thunk hands it the script's own local by reference (CONFIRMED native); the script
  keeps one local for the whole loop and re-walks all of it after each root (CONFIRMED src). With n root sequences, an
  action under the i-th (counting from 1) runs n − i + 1 times per pass, adding its tracks that often. No shipped
  effect: every map with Kismet has exactly one root sequence (census of the 12 maps), and the one streamed sub-level
  (TheCore, streamed into IceCave by a trigger later) has no adaptive objects, so each action runs once per pass
  (STRONG: that no other level is in the world when the pawn spawns is inferred from the data, not traced). Beat
  events are walked the same way, but "no beat actor yet" keeps one actor per event.
- Adding a track needs the pawn. In the `InitGame` pass there is none, so the track specs go to a waiting list that
  the shipped script never drains (the drain call is disabled); that pass matters only for the forced outputs below.
  The pawn pass adds the tracks. Death does not respawn the pawn (ABILITIES.md A-DT-2), so a level adds its tracks
  exactly once. The menu game info overrides `SpawnDefaultPawnFor`; the front-end map has no adaptive tracks anyway.
- `SeqAct_AddAdaptiveTracks` declares no input links (cdo `InputLinks` empty; none of the 4 shipped instances adds
  one), so only the manager runs it. Its activation adds each `TrackHolder` of `tracksToAdd` in order, then forces
  its output 0 ("Out").
- Nothing removes a track while the level runs.

### Tracks — CONFIRMED (src) unless noted

- A track is an actor spawned at the pawn and based on it, owning one audio component. It takes the holder's `ID`,
  sets its component's cue to `trackSoundCue`, keeps `numberOfTracks` as a "number of bars" that nothing reads, and
  plays the component at once. An ID already in use is not refused: both tracks play, and lookups find the first.
- Component: the class's component template stores no properties (cdo), so the engine's `AudioComponent` defaults
  apply: spatialisation allowed, no subtitle priority, not kept active when it loses its channel. The template is not
  in the actor's component list (the class default stores only `audioComp`), so where the sound is placed is
  TENTATIVE; it is never heard, because none of the 14 shipped track cues contains an attenuation node (CONFIRMED,
  cue graphs).
- All stems of a level start in the same frame. A muted stem keeps its voice (voice priority uses the node-chain
  volume, not the component volume; "Audio runtime"), so the stems stay in step and a fade-in joins in time. There is
  no other synchronisation: no tempo, bar or playback position is computed anywhere.

### Volume multipliers — CONFIRMED (src; native where marked)

- A track holds an ordered list of named multipliers (`ASMAMUAdaptiveVolumeMultiplier`: an ID and a factor; the
  class default stores `"NONE"` and −1.0, both overwritten when one is created, cdo) and a current adjust time
  (initially 0).
- *Add* appends a record, even when the ID is already present. *Edit* gives the first record with that ID the new
  value, or adds one when there is none. *Remove* drops the first record with that ID, if any. Each then stores its
  adjust time as the current one and re-applies the volume; remove re-applies even when it removed nothing.
- Re-applying: the volume is 1 multiplied by every factor in list order (single precision) and goes to the component
  as `AdjustVolume(current adjust time, volume)`: a linear ramp on the component's playback clock from the last
  completed target (initially 1) to the new one. A zero time takes effect at the next audio update; a change during
  a ramp restarts from the last completed target, not from the level reached (CONFIRMED native, "Audio runtime"). The
  factor multiplies with the cue's `VolumeMultiplier` (0.75 for the IceCave drums, 1.0 for the others), the sound
  class (`ASAMU_Music` for every track) and the rest of the gain chain.
- Track and multiplier IDs compare exactly, case included (string `==` is a `wcscmp`, CONFIRMED native).
- Manager functions: edit by track ID (first track with that ID; with none, a log line and no change), edit on every
  track in list order, and add / remove by track ID (silently nothing for an unknown track). No class calls add or
  remove (search of all 172 `asamu` classes): only edits are reachable from the game.

### Kismet actions — CONFIRMED (src, cdo, census)

| Class (editor name) | Properties | Behaviour |
|---|---|---|
| `SeqAct_AddAdaptiveTracks` ("Add track") | `tracksToAdd` (`TrackHolder`: `ID`, `trackSoundCue`, `numberOfTracks`) | adds the tracks (above); forces "Out" |
| `SeqAct_SetAdaptiveTrackVolumeMultiplier` ("Set track multiplier") | `trackID`, `multiplierID`, `Multiplier`, `adjustTime`, `trackVar` | edit by track ID; with a track variable, edit that track instead. The class declares no variable links (cdo `VariableLinks` empty) and all 28 shipped instances leave `trackVar` empty, so only the ID path runs |
| `SeqAct_EditMultiplierForAllTracks` ("Set all tracks multiplier") | `multiplierID`, `Multiplier`, `adjustTime` | edit on every track |
| `SeqAct_SetAdaptiveTrack` ("Adaptive Track") | variable links `Value`, `Target` (`SeqVar_ASAMUAdaptiveMusicTrack`, which holds a track) | copies a track reference from one variable to another; neither class occurs in a shipped level sequence |

The two multiplier actions finish like plain actions; only `AddAdaptiveTracks` forces an output.

### Beat events — CONFIRMED (src) unless noted

- `SeqEvent_TrackBeat` (`trackID`, `beatAmount`; one output "Beat"; no variable links, cdo): when the manager
  initialises it, a track with exactly that ID exists and it has no beat actor yet, it spawns an
  `ASAMUBeatEventActor` whose period is `beatAmount` seconds. In the `InitGame` pass no track exists, so only the
  pawn pass spawns beat actors.
- The beat actor's state code loops forever: a latent sleep of the period (frame-quantised, GRAPPLE.md G-TM-3), then
  the event's output 0 is forced (no activation check). The period is unrelated to the track's audio: beats continue
  while the track is muted, after a one-shot track has finished, and for a track without a cue. A period of 0 pulses
  every tick.
- (verify) Frame quantisation (arithmetic from G-TM-3, CONFIRMED rules; checked in single precision): at a constant
  tick `dt` a sleep of `p` ends on the n-th tick after the one that issued it, the smallest n with p − n·dt < dt/2, and
  the overshoot is dropped, so the beat period is n·dt rather than p. At 60 Hz: 0.5 s → 30 ticks, 1.0 s → 60 (both
  exact), 0.48 s → 29 ticks = 0.4833 s (120 Hz: 58 ticks, the same), 0 → every tick. The Darkcave beats therefore run
  0.7 % slower than their 0.48 s stems (the `Sleeping` waves are 15.36 s = 32 × 0.48 s; the heartbeat 5.76 s = 12 ×
  0.48 s): about 0.11 s of drift per `Sleeping` loop. The time-trial (4 s, beat 1.0 s), FallingRocks (96 s, 0.5 s) and
  `MusicPart2` (102 s, 0.5 s) periods are whole numbers of ticks at 60 Hz. At the original's variable frame rate the
  rounding varies per beat.

### Census (local, all maps) — CONFIRMED

Three levels use the system; every multiplier action uses the ID `Mute`, so each track ends up with one record and
its volume is that record's value.

| Level | Tracks: ID → cue (all class `ASAMU_Music`) | Multiplier actions | Beat events |
|---|---|---|---|
| AG-ParadiseCave | `MusicPart2` → `Sanctuary_Music.Sanctuary_Music_Part2_Cue` (one 102 s wave, no loop); `TimeTrialStart`, `TimeTrial1`…`TimeTrial5` → `Music.ASAMU_TimeTrial_Start_Cue`, `Music.ASAMU_TimeTrial_1_Cue`…`_5_Cue` (looping; `TimeTrial1` picks one of 5 waves) | 2 mute-all; 7 by ID on the time-trial stems (0 or 1, time 0) | `MusicPart2` 0.5 s, `TimeTrialStart` 1.0 s |
| AG-Darkcave | `HeartBeat`, `Sleeping1`, `Sleeping2`, `Searching`, `Sleeping3` → `Chasms_Music.Worm.Chasms_Worm_Music_HeartBeat_Cue`, `…_Sleeping_Cue`, `…_Sleeping_Cue_2`, `…_Searching_Cue`, `…_Sleeping_Cue_3` (looping; `Searching` has a modulator) | 1 mute-all; 17 by ID (0, 0.7 or 0.8, times 0–5 s) | `Sleeping1` ×4 (0.48 s ×3, 0 ×1) |
| AG-IceCave | `FallingRocks_Music` → `IceCave_Music.FallingRocks.FallingRocks_Music_Drums_Cue`, `FallingRocks_Inst` → `…FallingRocks_Music_Inst_Cue` (looping) | 1 mute-all; 4 by ID (0 over 1.5 s, 0.75 or 1.0 over 13 s) | `FallingRocks_Music` 0.5 s |

14 tracks (13 loop forever, cooked `Duration` 10000; `MusicPart2` does not loop), 4 `AddAdaptiveTracks`, 4
`EditMultiplierForAllTracks`, 28 `SetAdaptiveTrackVolumeMultiplier`, 7 `TrackBeat`; `numberOfTracks` is 0 in all 14
holders. Consequences:

- Every stem starts muted: each `AddAdaptiveTracks` "Out" is linked to a mute-all (`Mute`, 0, time 0), which runs in
  the first Kismet update, before the stems' first audio update.
- `MusicPart2` is never unmuted by any action: it plays silently for 102 s and only drives its beat event.
- One Darkcave action names the track `Suspense`, which no action adds: it logs and changes nothing.

### Our implementation

- `MusicManager` (device-free, deterministic): tracks, waiting list, multipliers and the manager functions; it
  returns `MusicEffect`s (`Play`, `AdjustVolume`, `Stop`, `TrackNotFound`, `TrackRefused`). `run_effects` plays them
  on the `AudioEngine`: a track's cue starts as a spatialisable component at the pawn that follows it, and volume
  changes go to `AudioEngine::adjust_volume`.
- Guard (ours, not the original's): at most `MAX_TRACKS` = 256 tracks, and as many parked specs; a further track is
  refused and logged (`TrackRefused`). The original appends without limit; only crafted data that re-runs
  `AddAdaptiveTracks` could reach it (shipped maximum: 7 tracks in a level).
- Bevy: `MusicPlugin` (registered by `AudioPlugin`) reads `MusicCommandMessage(MusicCommand)`: `AddTracks`,
  `EditMultiplier { target: Id | All | Handle }`, `Reset`. While the converted audio loads, commands wait, at most
  `MAX_QUEUED_COMMANDS` = 4096 (ours; beyond it the oldest multiplier edit is dropped, track additions are kept, and a
  queued `Reset` drops what came before it). With no documents and no load running they are dropped: a load starts
  again only with a new game, whose `StopAll` resets the music anyway, so a silent game no longer queues a level's
  edits forever. Once the documents are in, every command of a frame applies, in order, without a cap.
  `AudioCommand::StopAll` (the level-change command) forgets every track and waiting command; it is handled before
  that frame's music commands, so the next level's tracks survive it. Tracks are always added with a pawn: our Kismet
  runtime emits only the pawn-pass activation (the `InitGame` pass has no audible effect).
- Kismet mapping: `MusicCommand::from_output` turns `Output::AdaptiveTracks { tracks }` into `AddTracks`
  (`TrackSpec::from_holder`: each `TrackHolder`'s `ID`, `trackSoundCue`, `numberOfTracks`; the converted graphs
  store the field as `Id`, and struct fields are looked up case-insensitively like UE3 names) and
  `Output::AdaptiveMultiplier { track, .. }` into `MusicCommand::edit` (`track` `None` = every track); any other
  output is not music. The existing `Output` variants suffice. `apps/asamu/src/kismet.rs` (`route_output`) still
  carries an equivalent inline copy of this mapping; calling `from_output` there would leave one source.
- Beat events need no call from the app: `asamu-kismet`'s `Runtime::tick` (`tick_beats`) produces them.
- Not ported: the track-variable path and `SeqAct_SetAdaptiveTrack` (unreachable in the shipped data);
  `SeqAct_PlayMusicTrack` (front end) is the engine's separate music-track path and is not handled here.

Tests: `cargo test -p asamu audio::music` (24: 23 run, 1 ignored). Model: IDs compare exactly and the first match
wins, edit/add/remove rules and re-application, the single-precision product order, mute-all order and unknown IDs,
edits without tracks and stale handles, the waiting list, the track guard, the Kismet field mapping
(`kismet_outputs_become_commands`, `Id` spelling included). Through the device-free engine (values worked out by
hand): stems start together muted and ramp on the component clock; an edit during a ramp starts from the last
completed target; the IceCave fade shape (mute, 15 s, drums to 1.0 with the cue's 0.75 and instruments to 0.75 over
13 s, then both to 0 over 1.5 s: 0.375 / 0.375 half-way, 0.75 / 0.75 at the end, 0.375 / 0.375 half-way out, then 0);
non-finite and extreme multipliers and times keep every gain finite in [0, 1]; the same command script replays
bit-identically; tracks follow the pawn and unknown cues stay silent. Bevy: commands wait for a running load, are
dropped when no audio can arrive, the wait queue is bounded and keeps track additions, a queued reset drops earlier
commands, a loaded frame applies more than the queue bound, `StopAll` resets (also in the same frame as new tracks).
Real data (`ASAMU_CONVERTED_DIR`, skip otherwise): `real_converted_adaptive_tracks_play` starts the 14 shipped tracks
from the converted `cues.json`; `real_converted_kismet_adds_the_shipped_tracks` runs the first Kismet update of
ParadiseCave, Darkcave and IceCave through `asamu-kismet` and checks the tracks and cues against the census;
`real_converted_kismet_starts_the_stems_muted` (ignored until cross-check 1 is fixed) asserts that every stem is muted
after that update. Local result (2026-10-10, kismet + `--no-audio` audio JSON converted under ignored `research/`,
deleted afterwards): all 14 cues resolve, are `ASAMU_Music` without attenuation and still play after 2 s; the three
levels yield exactly the census tracks (ParadiseCave in the order `MusicPart2`, then the six time-trial stems); the
ignored test fails on `AG-ParadiseCave` `MusicPart2` (volume 1: the first update emits the two `AddTracks` and no
edit), which is cross-check 1.

### Cross-check of the Kismet runtime (`asamu-kismet`)

1. CONFIRMED (src, census; reproduced through our runtime on the converted graphs): `init_adaptive_music` emits
   `AdaptiveTracks` but does not force output 0 of the actions it runs. In the original the forced "Out" runs the
   linked mute-all in all 4 shipped actions; without it every stem plays at full volume from level start
   (ParadiseCave: `MusicPart2` and the six time-trial stems at once; Darkcave: all five worm stems; IceCave: both
   FallingRocks stems). After the first update of each level our runtime has emitted only the `AdaptiveTracks`
   outputs (2, 1, 1), no multiplier edit. Fix: force output 0 of each action after emitting (the original forces it
   in both passes; the second impulse repeats an idempotent mute). Then un-ignore
   `real_converted_kismet_starts_the_stems_muted`.
2. CONFIRMED (native): beat registration matches `trackID` case-insensitively; the original compares exactly. No
   shipped difference (every beat event's `trackID` equals a track ID exactly).
3. TENTATIVE: the first beat comes one tick early. The beat actor exists before the first world tick; its state code
   first runs in its first tick, which only issues the sleep, and the countdown starts with the next tick
   (G-TM-3), while `tick_beats` counts the first tick too. Later beats have the same period in both. Hand-computed
   (single precision, constant 60 Hz; the tick in which the sleep ends): 0.5 s: ours 30, 60, 90, original 31, 61,
   91; 1.0 s: 60, 120 vs 61, 121; 0.48 s: 29, 58, 87 vs 30, 59, 88; 0 s: every tick from 1 vs from 2 (120 Hz: the same
   one-tick offset). The forced output then runs in the next Kismet update in both.
4. TENTATIVE (robustness): a `SetAdaptiveTrackVolumeMultiplier` whose `trackID` is not a string becomes `track: None`,
   which means "every track"; the original would look up the empty ID. Every shipped instance stores a string.
5. (verify) No shipped difference: `Graph::find_by_class` visits each action once, while the original activates the
   actions of earlier root sequences again for every later one (Lifecycle above). Equal while a single root sequence
   is in the world at level start, which holds for every shipped map.
6. (verify) TENTATIVE (robustness): the runtime records a track for beat registration only when its holder has a
   string `ID`; the original adds the track with the empty ID, which a beat event with an empty `trackID` would find.
   Every shipped holder and beat event stores a non-empty ID.
