# Localization

Where the original keeps its localized text, how it picks a language, what `asamu-import localization` exports
from the user's own install, and how our runtime uses it. Confidence labels as in `CLAUDE.md`; "(src)" marks local
reading of the shipped script text (never copied), "(native)" local reading of the executable's disassembly (never
copied; the function is named by its symbol), "(cdo)" class default objects, "(T)" counts from our tools on the
install. **No game text appears in this document or in the repository**: the converter writes to the user-local
output directory only, and every test uses synthetic strings.

Code: `tools/asamu-import/src/localization.rs` (converter), `crates/asamu-assets/src/localization.rs` (runtime
tables), `apps/asamu/src/ui/locale.rs` (language setting), `apps/asamu/src/ui/strings.rs`, `apps/asamu/src/hud.rs`
(subtitle language).

## 1. Summary

| Text | Where it lives | Exported | Confidence |
|---|---|---|---|
| Menu labels, tooltips, pop-ups, level names, key-binding labels | `ASAMU/Localization/<LANG>/ASAMU.<lang>`, class sections (`[GFxASAMUMenu]`, `[GFxASAMUMainMenu]`, `[GFxASAMUPauseMenu]`, ...) | yes, all 14 languages | CONFIRMED (T) |
| Tutorial pop-ups (`SeqAct_ShowTutorialPopup`) | `ASAMU.<lang>` `[ASAMUHUD]`, one key per `TutorialMessageStrings` enumerator | yes | CONFIRMED (class model + file) |
| Subtitles | `<Package>.<lang>` object sections `[Group.Name SoundNodeWave]`, key `Subtitles[i]` | yes, all 14 languages | CONFIRMED (T); runtime path STRONG (§4) |
| Achievement names and descriptions | **not in the install**; Steam's local stats schema cache (5 languages) | yes, when the cache exists | CONFIRMED (T) |
| Credits list | static text fields of the GFx movie `ASAMUFrontEndFlash.asamu_credits` (same in every language) | yes (tag structure only) | CONFIRMED (T) |
| Credits skip hint | `[GFxASAMUCredits] PressESCLabel` | yes | CONFIRMED (src) |
| Title logo | an image in the HUD movie; the game title as text is `[GFxASAMUMainMenu] MenuTitleASAMULabel` | text only | TENTATIVE pairing (ours) |
| Language menu order | `asamu.ASAMUSettingsManager.LanguageCodes` | yes | CONFIRMED (cdo) |
| Font set-up per language | `GFxUI.<lang>` (`[Fonts]`, `[FontLib]`, `[IME]`; 9 languages) | exported as strings, unused | CONFIRMED (T) |
| Stock engine strings | `Engine/Localization/{DEU,ESN,FRA,INT,ITA,POL}` | no (not used by ASAMU's own UI, TENTATIVE) | — |

## 2. The localization files — CONFIRMED (T)

- 14 language folders under `ASAMU/Localization`: BRA CZE DEU ESN FIN FRA HUN INT ITA NLD POL POR SLO TUR (13 other
  languages plus INT; INVENTORY.md). Each holds `ASAMU.<lang>`, 12 subtitle files (`Workshop_Narrator`,
  `Sanctuary_Narrator`, `Village_Narrator`, `Village_Voice`, `Chasms_Narrator`, `Chasms_Voice`, `Star_Haven_Narrator`,
  `Star_Haven_Voice`, `IceCave_Narrator`, `IceCave_Voice`, `Epilogue_Narrator`, `Shared_Narrator`) and, in 9 languages
  (BRA ESN FRA HUN INT ITA NLD POR TUR), `GFxUI.<lang>`.
- Encodings: every `ASAMU.<lang>` and INT's `GFxUI.int` are UTF-16LE with a byte-order mark; the INT subtitle files
  and the other 8 `GFxUI` files are plain ASCII; the other 13 languages' subtitle files are UTF-16LE with a
  byte-order mark. No file without a byte-order mark has a byte ≥ 0x80 (T). The converter decodes by byte-order mark,
  else UTF-8, else Latin-1.
- Shape: UE3 ini — `[Section]` headers, `Key=Value` lines, `Key[i]=` for array elements; values of class sections are
  quoted strings; subtitle values are struct text `(Text="...",Time=1.500000)`. Quoted text escapes `\"`; 674 `\"`
  and 390 `\n` occur over all files, and no other escape (T). No line ends in a backslash, no line is a comment, and
  one line starts with a blank (T).

### Reading rules (what the game does, and the converter with it)

The class-section rules below were read from the game's own config reader, `FConfigFile::ProcessInputFileContents`
(the symbol is in the executable; its disassembly was read locally: the character tests, the buffers written and
the functions called). The converter follows them.

| Rule | Evidence | Confidence |
|---|---|---|
| A line ends at a line feed or a carriage return; trailing spaces and tabs are dropped | native: the line scan and the trim loop | CONFIRMED |
| A header is a line whose first character is `[` and whose last is `]`; any other line is not a header, so the keys after a malformed header stay in the previous section | native: the two character tests; §2.1 shows the outcome in shipped data | CONFIRMED |
| Lines before the first header, lines whose first character is `;` and lines without `=` are ignored; other lines split at the first `=`, with spaces and tabs trimmed around key and value | native: the tests in that order, and a search for the constant string `=` | CONFIRMED |
| A repeated header continues its section | native: the section map is searched before a section is added. That section names compare without case is stock UE3 string behaviour, not traced | STRONG |
| A key repeated in a section: the **last** value wins | §2.1 (the native section is a multi-map keyed by name: a repeated key is added, not replaced; which entry a lookup returns was not traced) | STRONG |
| A value that starts with `"` loses that quote, and one trailing `"` when it ends in one; quotes inside stay. Then `\\`, `\"` and `\n` become a backslash, a quote and a **line break**; any other escape is read as two hex digits | native: the first-character test, the call to `FString::ReplaceQuotesWithEscapedQuotes`, and the escape loop, which appends the characters 0x5C, 0x22 and 0x0A and has a hex-digit branch | CONFIRMED |
| A value that does not start with `"` (struct text included) is stored as written | native: all of the above sits behind the first-character test | CONFIRMED |
| Struct text: names and values trimmed, quoted values with `\"`, unquoted values up to `,`/`)`, text after `)` ignored | 37 shipped TUR lines end in `)"`; 18 lines in four languages have blanks next to `=`, `,` or `(` (T) | STRONG (their package data agree, §2.1; the native struct import was not read) |
| A stray `"` before `(` (`"(Text=,Time=..)`, 4 TUR lines) is dropped | the quote rule above: the config reader removes the opening quote of any value | CONFIRMED |
| Array lines are read from index 0 up to the first missing index | stock UE3 `LoadLocalizedProp`; no gap exists in the shipped files (T) | TENTATIVE |

Consequences in the shipped files (T):

- A value whose closing quote is not on its own line still loses its opening quote and has its escapes read; the rest
  is a line without `=` and is ignored. Three class values are like that: `GFxASAMUMenu.GammaLabel` in BRA is split
  over two lines (the game reads the first half only), and `GFxASAMUMenu.ResetRunTooltip` and
  `GFxASAMUMenu.ResetStrafeLeftTooltip` in ESN have their closing quote on the next line. The converter counts the
  left-over lines as ignored: BRA 2 (with the malformed header of §2.1), ESN 3, TUR 1 (one of ESN's and TUR's are lone
  quotes after a subtitle line).
- One key is written twice (`[FontLib] FontLib` in the 9 `GFxUI.<lang>` files); the last value is exported.

Not ported, because no shipped file needs it (T: no line ends in a backslash; the escape census above): a line that
ends in two backslashes continues on the next line (CONFIRMED, native); escapes other than the three above (the
converter keeps them as written and counts them); and a branch that overwrites text with the letter `X` (a constant
run of `X` characters and calls to `UObject::GetLanguage`: the stock engine's test language, STRONG).

What the verification pass changed (2026-10-10): the first version of the converter unquoted a value only when it both
started and ended with `"`, and left `\n` as written. It therefore exported the three values above with a stray
opening quote and unread escapes, and listed the reader's rules as TENTATIVE stock behaviour. The rules are now read
from the executable, and the converter's output differs from the first version in exactly those three values and in
the 374 strings whose `\n` escapes are now line breaks (390 of them).

### 2.1 Cross-check against the packages — CONFIRMED (T)

Every localized wave also stores `LocalizedSubtitles` (AUDIO.md). With the rules above, the exported subtitle lists
equal the packages' lists for that language, text and time, for **2,155 of the 2,155 waves common to both** (14
languages × 154 cooked waves, minus one; `asamu-import audio --no-audio --lang <L>` against
`asamu-import localization`, compared locally, 2026-10-10; repeated by the verification pass with its own comparison,
same result, after an independently written parser had reproduced the converter's 3,852 strings and 2,211 subtitle
lists exactly).
The exception shows two rules at work: in BRA's `Workshop_Narrator` file one header carries a trailing character after
`]`. Our reader therefore sees no such section and appends its two lines to the previous wave's section. The package
data show exactly that outcome: the previous wave's lines 0–1 are the broken section's lines, its line 2 is its own,
and the broken section's wave has its own lines only in the package (the converter's BRA table has no entry for it and
the runtime falls back to INT; the game's reader sees no such section either (§2), so there the cooked wave keeps its
INT lines). With first-wins that list differs; a strict struct parser (no text after `)`, no spaces) leaves 24 lists
different in five languages (the first converter pass's experiments; not repeated by the verification pass).

Four waves have subtitles in every language's files but are cooked into no package (T); they are exported and unused.

## 3. Class sections and what uses them

INT section sizes (T): `GFxASAMUMenu` 158 keys, `GFxASAMUMainMenu` 45, `ASAMUSettingsManager` 41, `GFxASAMUCredits`
30, `ASAMUHUD` 26, `GFxASAMUPauseMenu` 18, `GFxASAMUWorkshopMonitor` 7, `ASAMUHUDMovieTimeTrial` 5,
`GFxASAMUPauseMenuTimeTrial` 4, `ASAMUHUDMovie` 1 (335; 343 with `GFxUI`'s 8). The other languages translate 265
keys (273 with `GFxUI`): they omit 70 keys — the 29 credits role labels, the 7 workshop-monitor keys, the 32
language names (§6), `GFxASAMUMenu.VersionNumber` and `GFxASAMUMainMenu.LanguageHelpLabelExit` — which therefore come
from INT. Every key our menus use (§9) is in every language's own file (T).

- The classes declare these keys as `localized` properties (src): `GFxASAMUMenu`, `GFxASAMUMainMenu`, `ASAMUHUD`
  (26 tutorial strings), `ASAMUHUDMovie` (`MouseLabel`), `ASAMUSettingsManager` (`NoneLabel`, `XboxLabels`,
  `SupportedLanguages`, `SupportedLanguagesINT`), `GFxASAMUCredits` (`PressESCLabel` only).
- **Credits role labels are unused** (STRONG): `CrewLabel`, `ArtistLabel`, ... (29 keys) are declared by no class
  (src), are absent from `Startup.upk`'s name table (T: `asamu-inspect names Startup.upk`), and no other language
  translates them; the credits movie carries its own text (§5).

### 3.1 Tutorial pop-ups — CONFIRMED

`SeqAct_ShowTutorialPopup`'s message struct stores `tutorialStringPreset`, an `ASAMUHUD.TutorialMessageStrings`
enumerator (KISMET.md). The enum has 26 values (plus `_MAX`) whose names equal the 26 `[ASAMUHUD]` keys in the same
order (class model: `asamu-inspect class Startup.upk asamu.ASAMUHUD`; file key order; the key lists of all 14
languages equal the enumerator list, spelling included, T). The HUD binds its localized strings to a 26-entry table
in enum order and shows the preset's entry unless `tutorialStringOverride` is set (src). The shipped maps use 22
distinct presets in 29 pop-up actions on 5 maps; 4 actions set an override, a short text without placeholders or
escapes (T, Kismet census of the 12 maps).

Before showing a text the HUD movie script replaces placeholders (src, `ASAMUHUDMovie.ReplacePartWithKey`, keyboard
mode): `#MOVE#` `#SPACE#` `#SHIFT#` `#RMB#` `#LMB#` become `[<key name>]`, the first non-gamepad binding of the
command `GBA_MoveForward`, `GBA_ReleaseableJump | RocketBoostKeyDown`, `GBA_Sprint`, `GBA_PowerJump`, `GBA_Fire`;
`#MOUSE#` becomes the localized `MouseLabel`. With the shipped `DefaultInput.ini` those keys are `W`, `SpaceBar`,
`LeftShift`, `RightMouseButton`, `LeftMouseButton` (CONFIRMED (config)). Placeholder counts over the 14
`ASAMU.<lang>` files (T): `#RMB#` 69, `#SPACE#` 56, `#LMB#` 54, `#MOVE#` 28, `#SHIFT#` 28, `#MOUSE#` 14. Gamepad mode
substitutes the gamepad bindings instead (not ported). The movie replaces the placeholders in whatever text it is
handed, the preset's or the override's (src).

### 3.2 Line breaks — CONFIRMED

28 keys carry `\n` escapes: 19 tutorial texts and 9 menu texts (pop-up descriptions and help lines). 22 of them have
the escape in all 14 languages, 5 in 13 languages, and one in a single language (T). The config reader turns the
escape into a line break while it reads a quoted value (§2, native), so the strings the scripts get already hold line
breaks. The converter does the same; the runtime also converts an escape that is still written out (tables from the
first converter version, hand-made texts).

## 4. Subtitles at run time — STRONG

- `SoundNodeWave.Subtitles` (with `bManualWordWrap`, `bSingleLine`, `bMature`) is flagged `Localized`;
  `LocalizedSubtitles` is a plain (editor/cooker) array (CONFIRMED, class model of `Engine.u`).
- The executable has `UObject::LoadLocalized`, `UObject::LoadLocalizedProp`, `UObject::SetLanguage`,
  `UObject::ReloadLocalized`, `LocalizeLabel`, `LocalizeGeneral`, `appGetLanguageExt` (CONFIRMED, symbols).
- The files hold exactly the key shape `LoadLocalizedProp` reads for an array (`Subtitles[i]`) in sections named
  `<object path relative to the package> <class>`, in files named after the waves' source packages
  (`Chasms_Narrator.<lang>` ↔ wave paths `Chasms_Narrator.Group.Name`) (T).

So the game takes a wave's subtitles from the language file of the current language when its object loads, not from
the `LocalizedSubtitles` slots (STRONG; the native loader was not read). This answers AUDIO.md's open question about
how the subtitle slot is picked, and explains why only `_LOC_INT` packages ship: audio stays English, text comes from
the language files. A wave a language file lacks keeps the cooked `Subtitles`, i.e. the INT lines.

## 5. Credits — CONFIRMED (T)

- `GFxASAMUCredits` only sets the `pressescTF` widget's text to `PressESCLabel` and raises `SeqEvent_CreditsEnded`
  when the movie calls `ExitCredits` (src).
- The movie `ASAMUFrontEndFlash.asamu_credits` (`SwfMovie.RawData`, GFx signature `GFX`, version 15, uncompressed,
  68,458 bytes) has 167 `DefineEditText` tags, all with an initial text (155 of them HTML: `<p align=..><font ..>`),
  no `$` translation keys, no variable names and no other text source. 165 are placed in one sprite (the scrolling
  list), one in another sprite, and one by no timeline (the skip hint widget and a label template; which is which was
  not checked). The list is the same for every language (STRONG: nothing localizes it). The verification pass
  re-counted the tags with its own tag walker (same numbers).
- Layout in the list sprite (twips): a heading column at x = −12,750 and a names column at x = 50; a role and its
  names sit 100 twips apart vertically, rows 1,400 apart. The converter groups fields within 400 twips into a row
  (ours): 77 rows (14 one-cell, 40 two-cell, 23 three-cell).
- Not decoded: ActionScript (`DoABC`; the scroll timing lives there or in the timeline), glyph-only text
  (`DefineText`, 1 in `asamu_startup`), and the main menu / pause / HUD movies' static text fields (102, 36 and 7
  `DefineEditText`; the scripts fill most visible labels from the localized properties at run time).

## 6. Choosing a language

- Original (src + cdo): the main menu's language page lists `ASAMUSettingsManager.SupportedLanguages[i]` (localized;
  only INT ships the list, so every language shows INT's names) for the codes
  `LanguageCodes = ["---", "INT", "DEU", "FRA", "ITA", "POL", "---", "ESN", "POR", "BRA", "TUR", "FIN", "CZE", "SLO",
  "NLD", "HUN"]` (`---` = separator; CONFIRMED (cdo)); `SupportedLanguagesINT` is only referenced in a line that is
  commented out (src). It calls the native `ASAMUSystemSettingsManager.SetLanguage(code)` and asks for a restart (it
  quits at once for `RUS`/`UKR`, codes the shipped list does not contain). The shipped default is `Language=INT`
  (`DefaultEngine.ini`, CONFIRMED (config)).
- Steam: the app manifest stores the user's language for the game (`UserConfig.language`, e.g. `english`). No
  reference to Steam's game-language query was found in the executable (`nm`/`strings`, TENTATIVE: the original does
  not read it). Ours uses it as the default (below).

## 7. Achievement names — CONFIRMED (T)

The install has only the enumerator names (`EASAMUAchievements` in `Startup.upk`, also strings in
`OnlineSubsystemSteamworks.u`); the display names come from Steam. Steam's local cache
`<Steam>/appcache/stats/UserGameStatsSchema_278360.bin` (binary KeyValues) lists 15 achievements under
`stats/1/bits/1..15` with `name` = the enumerator names in enum order (bit = index + 1, as SAVE.md §6.4) and
`display/name` + `display/desc` in english, german, french, italian and polish. The converter reads it when present
(only the schema; the per-account `UserGameStats_*` file is never opened).

## 8. Converter: `asamu-import localization`

```sh
asamu-import --out <dir> localization [--lang INT,DEU] [--steam-stats PATH] [--no-steam] [--no-credits] [--dry-run]
```

Writes `<out>/localization/` (user-local; refused inside the repository except git-ignored `research/`, inside an
`.app`/`steamapps` tree and inside the install; JSON replaced atomically):

| File | Content |
|---|---|
| `manifest.json` | languages (code, file, counts), `language_menu` (cdo), `steam_language`, `default_language` (Steam's when the install has it, else INT), credits file, warnings |
| `<LANG>.json` | `strings` (`Section.Key` → text as the game reads it: unquoted, escapes read, line breaks as line breaks), `subtitles` (wave path → lines, flags), `achievements` (API name → name, description, hidden), files read with encodings, issue counters |
| `credits.json` | the credits rows (cells with x, font height, alignment, text) |

Steam mapping (ours): english→INT, german→DEU, french→FRA, italian→ITA, polish→POL, spanish/latam→ESN,
portuguese→POR, brazilian→BRA, turkish→TUR, finnish→FIN, czech→CZE, dutch→NLD, hungarian→HUN (Steam has no Slovak).

Local run (2026-10-10, nothing committed): 14 languages; strings INT 343, others 265 or 273; subtitled waves 158 per
language (BRA 157), lines BRA 512, DEU 518, FRA 523, ITA 515, POL 517, the rest 514; 15 achievements for INT, DEU,
FRA, ITA, POL; credits 165 fields in 77 rows; language menu 16 entries; Steam language `english` → default INT;
0 malformed lines, 0 lines after gaps, 0 unknown escapes; 1 repeated key in each of the 9 languages with `GFxUI`;
ignored lines BRA 2, ESN 3, TUR 1 (§2); in 0.2 s. The verification pass got the same counts from the converter and
from an independent census of the files.

## 9. Runtime (ours)

- `asamu_assets::localization::Localization` loads the tables. File sizes, the number of strings, waves, lines (per
  wave and in all), achievements, credits rows and cells, and key and text lengths are bounded; a table or credits
  file beyond a bound, or malformed, is a warning and is left out. Every lookup falls back to INT (TENTATIVE for menu
  strings; for subtitles it is the original's outcome, §4). String keys and wave paths compare without case.
- Start language: the first of these that is a converted language — `ASAMU_LANGUAGE` (one run, not saved), the saved
  choice (`language.json` beside `settings.json`), the converted default (Steam's) — else INT. Settings → Language
  cycles the converted languages in `LanguageCodes` order and applies at once (the original needs a restart).
- Menu labels, chapter titles (`Map<Name>Name`), the confirmation pop-up, settings labels, achievement toasts, the
  title overlay (`MenuTitleASAMULabel`) and the credits skip hint use the chosen language through `UiStrings` (keys in
  `ui::locale::LABEL_KEYS`; the pairing of our screens with the original's keys is ours). Kismet's tutorial pop-up
  reads `tutorial.<preset>`: the `[ASAMUHUD]` text with placeholders replaced by the default key names (§3.1).
  With `RUST_LOG=asamu=debug` each pop-up is logged once it is on screen: the key it came from, the language and its
  size, never its text.
- The credits screen scrolls the movie's rows (font size from the field heights, ours) for `kismet::CREDITS_SECONDS`.
- Subtitles: the audio module shows the converted audio's lines (usually INT); the HUD maps each line to the chosen
  language's line of the same wave (`SubtitleTranslation`). Lists of equal length pair line by line; the differently
  split lists (17 cooked waves: FRA 9, DEU 4, POL 3, ITA 1) map by time window, an approximation of their timing. The
  map goes from text to text, INT lines first, then the chosen language's own lines (never changed), then the other
  languages. Measured on the shipped data with INT audio (T): of the 500 non-empty cooked INT lines, 494–500 per
  language change, and every line of an equally split wave gets its own translation except one in SLO, where the same
  INT sentence is translated in two ways and the first shows for both. Exact text and timing need the audio module to
  swap its tracks with `Localization::localize_tracks` (integration point, not wired: `audio.rs` belongs to the audio
  workstream).

## 10. Open items

- `UObject::LoadLocalizedProp` and the struct text import were not read; the struct and array rules marked
  STRONG/TENTATIVE in §2 come from data agreement. The config reader itself is read (§2).
- A tutorial override text is shown as written: `kismet.rs` does not pass it through the placeholder replacement,
  which the original applies to overrides too (§3.1). The four shipped overrides have no placeholder, so nothing
  differs today. A pop-up that is on screen when the language changes keeps its text until it is shown again.
- Gamepad placeholder names, rebinding-aware placeholders.
- The credits movie's scroll speed and duration (ActionScript), the title logo image, the menus' static movie text.
- Subtitle timing of a non-INT language through the audio engine (`localize_tracks`), and the one SLO line of §9.

## 11. Verification pass (2026-10-10)

Independent checks on the same install, all local (scratch under ignored `research/local/`, deleted afterwards;
counts only):

| Claim | Check | Result |
|---|---|---|
| File census, encodings, escape and placeholder counts (§2, §3.1) | a parser written for the check, not sharing code with the converter | same numbers; 3,852 strings and 2,211 subtitle lists equal to the converter's output |
| Reading rules (§2) | disassembly of `FConfigFile::ProcessInputFileContents`, read locally | rules confirmed; quote and escape handling corrected (three values, and where `\n` becomes a line break) |
| Subtitle lists equal the packages' lists (§2.1) | own comparison of both exports, 14 languages | 2,155 of 2,155 |
| Tutorial keys = enumerators (§3.1); presets in the maps | class model against every language's key order; converted Kismet graphs | 26 keys in enum order in 14 languages; 29 actions, 22 presets, 5 maps, 4 overrides |
| Placeholder replacement and default keys (§3.1) | the movie script (src) and `DefaultInput.ini` | as described |
| `LanguageCodes`, `Language=INT` (§6) | class defaults, `DefaultEngine.ini` | as described |
| Credits movie (§5) | own tag walker over the movie's bytes | 167 text fields, 155 HTML, 165 in the list sprite |
| Achievement schema (§7) | own reader of Steam's cached schema | 15 entries in enum order, 5 languages |
| No game text in the repository | every converted string, subtitle line, credits cell and achievement text (10,619 fragments of 4 characters or more) searched in all tracked and new files | no sentence, tutorial, tooltip or subtitle found. What matches is the game's title and its studio's name in project documents, single common words, key and file names, and short generic labels of our own English menu (such as the volume and sensitivity labels) that read like the original's |
| The app shows the chosen language | gated tests on a conversion: the main menu the app builds shows five labels from each language's own table in all 14 languages, and every preset the converted maps use resolves to that language's text. A run of the app in DEU on AG-Workshop logged the first pop-up on screen as the DEU text of its preset (57 characters on 2 lines; the INT text has 44) | as claimed. The menu and pop-up were not looked at by eye |
