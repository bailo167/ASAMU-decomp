//! The language setting and the localized text it selects.
//!
//! - [`Locale`]: the converted tables (`<converted>/localization/`, written by
//!   `asamu-import localization` from the user's own install) and the chosen
//!   language. The start language is the first of these that is a converted
//!   language: `ASAMU_LANGUAGE` (this run only), the saved choice
//!   (`language.json` next to `settings.json`, ours), the converted default
//!   (Steam's `UserConfig.language` for the game when the install has that
//!   language), `INT`.
//! - [`apply_language`]: fills the localized layer of [`UiStrings`] (menu
//!   labels, chapter titles, tutorial pop-ups, achievement names, the title
//!   logo line and the credits' skip hint), the HUD's subtitle language and
//!   rebuilds the open menu whenever the language changes. The original
//!   applies a new language after a restart (its menu asks for one); ours
//!   applies it at once.
//! - [`localize_overlays`]: the Kismet title overlay shows the localized game
//!   title, and the credits screen scrolls the credits movie's own text.
//!
//! Lookups fall back to `INT`, then to our English defaults.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use asamu_assets::localization::{
    DEFAULT_KEY_NAMES, Localization, display_text, expand_key_placeholders, normalize_language,
};
use asamu_game::save::json::Value;
use asamu_game::save::{Achievement, ChapterId, Document, LoadOutcome, SaveError, SaveStore};
use bevy::prelude::*;

use super::UiState;
use super::menus::UiAction;
use super::strings::UiStrings;
use crate::hud::SubtitleLanguage;

/// Environment variable choosing the language for one run (not saved).
pub(crate) const LANGUAGE_ENV: &str = "ASAMU_LANGUAGE";

/// Our text keys → the original's localized properties (`Section.Key` of
/// `ASAMU.<lang>`). The pairing is ours (the menus are a functional
/// replacement); the texts are the user's own install's.
pub(crate) const LABEL_KEYS: &[(&str, &str)] = &[
    ("menu.continue", "GFxASAMUMainMenu.ContinueBtnLabel"),
    ("menu.new_game", "GFxASAMUMainMenu.startBtnLabel"),
    ("menu.chapters", "GFxASAMUMainMenu.LevelSelectLabel"),
    ("menu.time_trial", "GFxASAMUMainMenu.TimeTrialLabel"),
    ("menu.language", "GFxASAMUMainMenu.LanguageLabel"),
    (
        "menu.confirm_title",
        "GFxASAMUMainMenu.OverwritePopupTitleLabel",
    ),
    (
        "menu.confirm_text",
        "GFxASAMUMainMenu.OverwritePopupDescriptionLabel",
    ),
    ("menu.settings", "GFxASAMUMenu.OptionsLabel"),
    ("menu.quit", "GFxASAMUMenu.ExitLabel"),
    ("menu.back", "GFxASAMUMenu.BackLabel"),
    ("menu.cancel", "GFxASAMUMenu.CancelLabel"),
    ("menu.yes", "GFxASAMUMenu.YesLabel"),
    ("menu.no", "GFxASAMUMenu.NoLabel"),
    ("menu.return", "GFxASAMUPauseMenu.ResumeLabel"),
    ("menu.resume", "GFxASAMUPauseMenu.ResumeLabel"),
    ("menu.restart", "GFxASAMUPauseMenu.RestartCheckpointLabel"),
    ("menu.main_menu", "GFxASAMUPauseMenu.ReturnToMainMenuLabel"),
    ("menu.paused", "GFxASAMUPauseMenu.MenuTitlePausedLabel"),
    ("settings.fov", "GFxASAMUMenu.FOVLabel"),
    (
        "settings.mouse_sensitivity",
        "GFxASAMUMenu.MouseSensitivityLabel",
    ),
    ("settings.invert_mouse", "GFxASAMUMenu.InvertMouseLabel"),
    (
        "settings.master_volume",
        "GFxASAMUMenu.masterAudioSliderLabel",
    ),
    (
        "settings.music_volume",
        "GFxASAMUMenu.musicAudioSliderLabel",
    ),
    ("settings.sfx_volume", "GFxASAMUMenu.soundAudioSliderLabel"),
    (
        "settings.voice_volume",
        "GFxASAMUMenu.voiceAudioSliderLabel",
    ),
    ("settings.fullscreen", "GFxASAMUMenu.FullscreenLabel"),
    ("settings.resolution", "GFxASAMUMenu.ResolutionsLabel"),
    ("settings.subtitles", "GFxASAMUMenu.SubtitlesLabel"),
    ("title.logo", "GFxASAMUMainMenu.MenuTitleASAMULabel"),
    ("credits.skip", "GFxASAMUCredits.PressESCLabel"),
];

/// Chapter → its `GFxASAMUMenu.Map<Name>Name` key (level select names).
pub(crate) fn chapter_key(chapter: ChapterId) -> &'static str {
    match chapter {
        ChapterId::Workshop => "GFxASAMUMenu.MapWorkshopName",
        ChapterId::Sanctuary => "GFxASAMUMenu.MapSanctuaryName",
        ChapterId::Village => "GFxASAMUMenu.MapVillageName",
        ChapterId::DarkCave => "GFxASAMUMenu.MapDarkcaveName",
        ChapterId::StarHaven => "GFxASAMUMenu.MapStarhavenName",
        ChapterId::IceCave => "GFxASAMUMenu.MapIcecaveName",
        ChapterId::Epilogue => "GFxASAMUMenu.MapEpilogueName",
    }
}

/// The tutorial pop-up texts' section; its keys are the
/// `TutorialMessageStrings` enumerators (CONFIRMED: same 26 names in the
/// same order, class model + `ASAMU.int`), which Kismet's
/// `SeqAct_ShowTutorialPopup` stores as `tutorialStringPreset`.
const TUTORIAL_SECTION: &str = "ASAMUHUD.";
/// Replaces `#MOUSE#` in the tutorials (keyboard mode).
const MOUSE_LABEL_KEY: &str = "ASAMUHUDMovie.MouseLabel";

/// The saved language choice (`language.json`, ours).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LanguagePreference {
    /// Language extension, `None` = the converted default.
    pub language: Option<String>,
}

impl Document for LanguagePreference {
    const KIND: &'static str = "language";
    const FILE: &'static str = "language.json";

    fn to_fields(&self) -> BTreeMap<String, Value> {
        let mut f = BTreeMap::new();
        f.insert(
            "language".into(),
            self.language
                .as_ref()
                .map_or(Value::Null, |l| Value::String(l.clone())),
        );
        f
    }

    fn from_fields(fields: &BTreeMap<String, Value>) -> Result<Self, SaveError> {
        let language = match fields.get("language") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => {
                Some(normalize_language(s).ok_or_else(|| SaveError::Invalid {
                    kind: Self::KIND,
                    reason: format!("{s:?} is not a language extension"),
                })?)
            }
            Some(_) => {
                return Err(SaveError::Invalid {
                    kind: Self::KIND,
                    reason: "language is not a string".into(),
                });
            }
        };
        Ok(Self { language })
    }
}

/// The converted tables and the chosen language.
#[derive(Resource, Debug)]
pub(crate) struct Locale {
    data: Option<Arc<Localization>>,
    language: String,
    store: Option<SaveStore>,
    /// The language [`apply_language`] applied last.
    applied: Option<String>,
}

impl Default for Locale {
    fn default() -> Self {
        Self {
            data: None,
            language: asamu_assets::localization::FALLBACK_LANGUAGE.to_owned(),
            store: None,
            applied: None,
        }
    }
}

impl Locale {
    /// Loads the tables of `converted_root` (when given) and picks the start
    /// language (see the module docs). `save_root` holds `language.json`.
    #[must_use]
    pub fn load(
        converted_root: Option<&Path>,
        save_root: Option<&Path>,
        env: Option<&str>,
    ) -> Self {
        let data = converted_root.and_then(|root| match Localization::load(root) {
            Ok(Some(l)) => {
                for w in l.warnings() {
                    warn!("localization: {w}");
                }
                Some(Arc::new(l))
            }
            Ok(None) => {
                info!("no converted localization (asamu-import localization): English defaults");
                None
            }
            Err(e) => {
                warn!("localization unreadable ({e}): English defaults");
                None
            }
        });
        let mut store = save_root.map(SaveStore::new);
        let saved = match store.as_ref().map(SaveStore::load::<LanguagePreference>) {
            Some(LoadOutcome::Loaded(p)) => p.language,
            Some(LoadOutcome::Unreadable { error, quarantined }) => {
                warn!("language.json unreadable ({error}); using the default");
                if quarantined.is_none() {
                    // It could not be set aside: never overwrite it.
                    store = None;
                }
                None
            }
            Some(LoadOutcome::Missing) | None => None,
        };
        // The first wish that is a converted language: an override that
        // names none must not hide the saved choice.
        let wanted = [env, saved.as_deref()]
            .into_iter()
            .flatten()
            .find(|code| data.as_ref().is_some_and(|d| d.has_language(code)));
        if let Some(code) = env
            && wanted != Some(code)
        {
            warn!("{LANGUAGE_ENV}: {code:?} is not a converted language; ignored");
        }
        Self::with_data(data, store, wanted)
    }

    /// A locale over `data` starting in `wanted` (when available).
    #[must_use]
    pub fn with_data(
        data: Option<Arc<Localization>>,
        store: Option<SaveStore>,
        wanted: Option<&str>,
    ) -> Self {
        let language = match &data {
            Some(l) => l.resolve_language(wanted),
            None => asamu_assets::localization::FALLBACK_LANGUAGE.to_owned(),
        };
        if let Some(l) = &data {
            info!(
                "language {language} ({} languages converted)",
                l.languages().len()
            );
        }
        Self {
            data,
            language,
            store,
            applied: None,
        }
    }

    /// The chosen language.
    #[must_use]
    pub fn language(&self) -> &str {
        &self.language
    }

    /// The converted tables.
    #[must_use]
    pub fn data(&self) -> Option<&Localization> {
        self.data.as_deref()
    }

    /// The chosen language's name as its own menu shows it (`None` without
    /// a choice to make: fewer than two converted languages).
    #[must_use]
    pub fn display_name(&self) -> Option<String> {
        let data = self.data.as_ref()?;
        if data.languages().len() < 2 {
            return None;
        }
        Some(
            data.language_name(&self.language, &self.language)
                .unwrap_or_else(|| self.language.clone()),
        )
    }

    /// Picks the next (`forward`) or previous converted language, in the
    /// original's menu order, and saves the choice. `false` when nothing
    /// changed.
    pub fn cycle(&mut self, forward: bool) -> bool {
        let Some(data) = &self.data else {
            return false;
        };
        let langs = data.languages();
        if langs.len() < 2 {
            return false;
        }
        let n = langs.len();
        let i = langs.iter().position(|l| *l == self.language).unwrap_or(0);
        let next = if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        };
        let Some(code) = langs.get(next).map(|s| (*s).to_owned()) else {
            return false;
        };
        self.set_language(&code)
    }

    /// Chooses `code` (when converted) and saves the choice.
    pub fn set_language(&mut self, code: &str) -> bool {
        let Some(code) = normalize_language(code) else {
            return false;
        };
        if code == self.language || !self.data.as_ref().is_some_and(|d| d.has_language(&code)) {
            return false;
        }
        self.language = code;
        if let Some(store) = &self.store
            && let Err(e) = store.save(&LanguagePreference {
                language: Some(self.language.clone()),
            })
        {
            warn!("could not save the language choice: {e}");
        }
        true
    }

    /// The localized layer of [`UiStrings`] for the chosen language.
    #[must_use]
    pub fn texts(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let Some(data) = &self.data else {
            return out;
        };
        let lang = self.language.as_str();
        let get = |key: &str| {
            data.string(lang, key)
                .map(display_text)
                .filter(|t| !t.trim().is_empty())
        };
        for (ours, theirs) in LABEL_KEYS {
            if let Some(t) = get(theirs) {
                out.insert((*ours).to_owned(), t);
            }
        }
        for c in ChapterId::ALL {
            if let Some(t) = get(chapter_key(c)) {
                out.insert(format!("chapter.{}", c.enum_name()), t);
            }
        }
        let mouse = get(MOUSE_LABEL_KEY).unwrap_or_else(|| "Mouse".to_owned());
        for key in tutorial_keys(data, lang) {
            if let Some(t) = get(&format!("{TUTORIAL_SECTION}{key}")) {
                out.insert(
                    format!("tutorial.{key}"),
                    expand_key_placeholders(&t, &DEFAULT_KEY_NAMES, &mouse),
                );
            }
        }
        for a in Achievement::ALL {
            if let Some(text) = data.achievement(lang, a.name()) {
                if !text.name.trim().is_empty() {
                    out.insert(format!("achievement.{}", a.name()), text.name.clone());
                }
                if !text.description.trim().is_empty() {
                    out.insert(
                        format!("achievement.{}.description", a.name()),
                        text.description.clone(),
                    );
                }
            }
        }
        out
    }
}

/// The tutorial preset names the tables have (`ASAMUHUD.Tutorial_*` keys of
/// the chosen language and of `INT`).
fn tutorial_keys(data: &Localization, lang: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for code in [lang, asamu_assets::localization::FALLBACK_LANGUAGE] {
        let Some(table) = data.table(code) else {
            continue;
        };
        for k in table.keys() {
            if let Some(name) = k.strip_prefix(TUTORIAL_SECTION)
                && name.starts_with("Tutorial_")
                && !keys.iter().any(|x| x == name)
            {
                keys.push(name.to_owned());
            }
        }
    }
    keys
}

/// The settings screen's language stepper.
pub(crate) fn handle_language_actions(
    mut actions: MessageReader<UiAction>,
    mut locale: ResMut<Locale>,
) {
    for a in actions.read() {
        if let UiAction::Language(forward) = a
            && locale.cycle(*forward)
        {
            info!("language: {}", locale.language);
        }
    }
}

/// Logs each tutorial pop-up once it is on screen (`RUST_LOG=asamu=debug`):
/// where its text came from (the localized preset key, or an override /
/// the preset name), its size and the language. Never the text.
pub(crate) fn log_tutorials(
    locale: Res<Locale>,
    strings: Res<UiStrings>,
    pres: Option<Res<crate::kismet::Presentation>>,
    on_screen: Query<&Text, With<crate::kismet::TutorialText>>,
    mut logged: Local<Option<(i32, usize)>>,
) {
    let Some((id, text, seconds)) = pres.as_ref().and_then(|p| p.tutorial.as_ref()) else {
        *logged = None;
        return;
    };
    let key = Some((*id, text.len()));
    if *logged == key || !on_screen.iter().any(|t| t.0 == *text) {
        return;
    }
    *logged = key;
    debug!(
        "tutorial pop-up {id} on screen: {}, {} characters on {} lines, {seconds:?} s",
        tutorial_source(&strings, locale.language(), text),
        text.chars().count(),
        text.lines().count()
    );
}

/// Where a tutorial pop-up's text came from: the localized preset key and
/// the language, or Kismet's override / the bare preset name.
fn tutorial_source(strings: &UiStrings, language: &str, text: &str) -> String {
    strings.key_with_text("tutorial.", text).map_or_else(
        || "an override or the preset name".to_owned(),
        |key| format!("{key} in {language}"),
    )
}

/// Applies a new language: the localized text layer, the subtitle language
/// and a menu rebuild.
pub(crate) fn apply_language(
    mut locale: ResMut<Locale>,
    mut strings: ResMut<UiStrings>,
    mut state: ResMut<UiState>,
    subtitles: Option<ResMut<SubtitleLanguage>>,
) {
    if locale.applied.as_deref() == Some(locale.language.as_str()) {
        return;
    }
    strings.set_localized(locale.texts());
    if let Some(mut s) = subtitles {
        s.0 = locale
            .data
            .as_ref()
            .map(|d| Arc::new(d.subtitle_translation(&locale.language)));
    }
    state.dirty = true;
    locale.applied = Some(locale.language.clone());
}

/// Our credits list inside the Kismet credits screen.
#[derive(Component)]
pub(crate) struct LocalizedCredits {
    /// The language it was built for.
    language: String,
}

/// The credits text rows: the movie's rows (cells joined, font size from the
/// fields' heights), then the localized skip hint. `(text, font px)`.
#[must_use]
pub(crate) fn credits_lines(data: &Localization, strings: &UiStrings) -> Vec<(String, f32)> {
    let Some(credits) = data.credits() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for row in &credits.rows {
        let text = row.line("   |   ");
        if text.is_empty() {
            continue;
        }
        // Movie font heights are twips at the movie's own scale; half of
        // their pixel size reads well on our screen (ours).
        let twips = row.cells.iter().filter_map(|c| c.size).max().unwrap_or(640);
        let px = (f32::from(twips) / 20.0 * 0.5).clamp(12.0, 44.0);
        out.push((text, px));
    }
    if let Some(skip) = strings.lookup("credits.skip") {
        out.push((String::new(), 16.0));
        out.push((skip.to_owned(), 16.0));
    }
    out
}

/// The title overlay's line and the credits screen's text follow the
/// language; the credits scroll while the screen shows.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(crate) fn localize_overlays(
    mut commands: Commands,
    locale: Res<Locale>,
    strings: Res<UiStrings>,
    pres: Option<Res<crate::kismet::Presentation>>,
    titles: Query<&Children, With<crate::kismet::TitleOverlay>>,
    credit_roots: Query<(Entity, &Children, &ComputedNode), With<crate::kismet::CreditsOverlay>>,
    mut texts: Query<&mut Text, Without<LocalizedCredits>>,
    mut visibility: Query<&mut Visibility, Without<LocalizedCredits>>,
    mut ours: Query<(Entity, &LocalizedCredits, &mut Node, &ComputedNode)>,
) {
    if let Some(title) = strings.lookup("title.logo") {
        for children in &titles {
            for child in children.iter() {
                if let Ok(mut t) = texts.get_mut(child)
                    && t.0 != title
                {
                    title.clone_into(&mut t.0);
                }
            }
        }
    }
    let Some(data) = locale.data() else {
        return;
    };
    if data.credits().is_none() {
        return;
    }
    for (root, children, root_node) in &credit_roots {
        let mut have = None;
        for child in children.iter() {
            if let Ok((e, c, ..)) = ours.get(child) {
                have = Some((e, c.language == locale.language()));
            } else if let Ok(mut v) = visibility.get_mut(child) {
                // The stand-in text of the credits screen.
                if *v != Visibility::Hidden {
                    *v = Visibility::Hidden;
                }
            }
        }
        match have {
            Some((_, true)) => {}
            Some((e, false)) => {
                commands.entity(e).despawn();
                spawn_credits(&mut commands, root, data, &strings, locale.language());
            }
            None => spawn_credits(&mut commands, root, data, &strings, locale.language()),
        }
        // Scroll: from below the screen to above it over the screen's time.
        let progress = pres.as_ref().and_then(|p| p.credits).map_or(0.0, |left| {
            (1.0 - left / crate::kismet::CREDITS_SECONDS).clamp(0.0, 1.0)
        });
        for child in children.iter() {
            if let Ok((_, _, mut node, computed)) = ours.get_mut(child) {
                let inv = root_node.inverse_scale_factor();
                let screen = root_node.size().y * inv;
                let content = computed.size().y * inv;
                let top = screen - progress * (screen + content);
                if node.top != px(top) {
                    node.top = px(top);
                }
            }
        }
    }
}

fn spawn_credits(
    commands: &mut Commands,
    root: Entity,
    data: &Localization,
    strings: &UiStrings,
    language: &str,
) {
    let lines = credits_lines(data, strings);
    commands.entity(root).with_children(|parent| {
        parent
            .spawn((
                LocalizedCredits {
                    language: language.to_owned(),
                },
                Node {
                    position_type: PositionType::Absolute,
                    left: px(0),
                    right: px(0),
                    top: percent(100),
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    row_gap: px(6),
                    ..default()
                },
            ))
            .with_children(|list| {
                for (text, size) in lines {
                    list.spawn((
                        Text::new(text),
                        TextFont {
                            font_size: FontSize::Px(size),
                            ..default()
                        },
                        TextColor(Color::srgb(0.92, 0.92, 0.9)),
                        TextLayout::justify(Justify::Center),
                    ));
                }
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_assets::localization::{
        AchievementText, Credits, CreditsCell, CreditsRow, LanguageTable,
    };

    /// Synthetic tables: "INT" and "TST" (no game text).
    fn data() -> Localization {
        let mut int = LanguageTable::new("INT");
        int.insert_string("GFxASAMUMenu.ExitLabel", "Leave");
        int.insert_string("GFxASAMUMenu.MapVillageName", "The village");
        int.insert_string(
            "ASAMUHUD.Tutorial_SpaceToJump",
            "Press #SPACE# to hop\\nnow",
        );
        int.insert_string("ASAMUHUD.Tutorial_UseMouse", "Use #MOUSE#");
        int.insert_string("ASAMUHUDMovie.MouseLabel", "the rodent");
        int.insert_string("GFxASAMUCredits.PressESCLabel", "skip me");
        int.insert_string("GFxASAMUMainMenu.MenuTitleASAMULabel", "Game title");
        int.insert_string("ASAMUSettingsManager.SupportedLanguagesINT[1]", "Int-name");
        int.insert_string("ASAMUSettingsManager.SupportedLanguagesINT[2]", "Tst-name");
        int.insert_achievement(
            "FLOOR_IS_LAVA",
            AchievementText {
                name: "Hot floor".into(),
                description: "Never touch it".into(),
                hidden: false,
            },
        );
        let mut tst = LanguageTable::new("TST");
        tst.insert_string("GFxASAMUMenu.ExitLabel", "Tschuess");
        tst.insert_string("ASAMUSettingsManager.SupportedLanguages[2]", "Tst-own");
        Localization::from_tables(vec![int, tst])
            .with_language_menu(&["---", "INT", "TST"])
            .with_credits(Credits {
                rows: vec![CreditsRow {
                    y: 0,
                    cells: vec![
                        CreditsCell {
                            x: -10,
                            size: Some(840),
                            align: None,
                            text: "Role".into(),
                        },
                        CreditsCell {
                            x: 10,
                            size: Some(640),
                            align: None,
                            text: "Person".into(),
                        },
                    ],
                }],
            })
    }

    #[test]
    fn the_localized_layer_covers_labels_chapters_tutorials_and_achievements() {
        let locale = Locale::with_data(Some(Arc::new(data())), None, Some("TST"));
        assert_eq!(locale.language(), "TST");
        let t = locale.texts();
        assert_eq!(t["menu.quit"], "Tschuess");
        assert_eq!(t["chapter.Village"], "The village", "INT fallback");
        assert_eq!(
            t["tutorial.Tutorial_SpaceToJump"],
            "Press [SpaceBar] to hop\nnow"
        );
        assert_eq!(t["tutorial.Tutorial_UseMouse"], "Use the rodent");
        assert_eq!(t["achievement.FLOOR_IS_LAVA"], "Hot floor");
        assert_eq!(t["achievement.FLOOR_IS_LAVA.description"], "Never touch it");
        assert_eq!(t["title.logo"], "Game title");
        assert!(!t.contains_key("menu.continue"));
        assert_eq!(locale.display_name().as_deref(), Some("Tst-own"));
        let mut strings = UiStrings::default();
        strings.set_localized(t);
        let lines = credits_lines(locale.data().unwrap(), &strings);
        assert_eq!(lines[0], ("Role   |   Person".to_owned(), 21.0));
        assert_eq!(lines.last().unwrap().0, "skip me");
        // No data: English defaults, no language row.
        let none = Locale::with_data(None, None, Some("TST"));
        assert_eq!(none.language(), "INT");
        assert!(none.texts().is_empty());
        assert_eq!(none.display_name(), None);
    }

    /// A scratch directory (removed by the caller).
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("asamu-ui-locale-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A synthetic converted directory with "INT" and "TST" tables.
    fn converted(name: &str) -> std::path::PathBuf {
        let root = scratch(name);
        let loc = root.join("localization");
        std::fs::create_dir_all(&loc).unwrap();
        std::fs::write(
            loc.join("manifest.json"),
            r#"{"format": "asamu-localization", "version": 1,
                "languages": [{"code": "INT", "file": "INT.json"},
                              {"code": "TST", "file": "TST.json"}],
                "language_menu": ["---", "INT", "TST"], "default_language": "INT"}"#,
        )
        .unwrap();
        for code in ["INT", "TST"] {
            std::fs::write(
                loc.join(format!("{code}.json")),
                format!(
                    r#"{{"format": "asamu-localization-table", "version": 1,
                        "language": "{code}", "strings": {{"GFxASAMUMenu.ExitLabel": "x"}}}}"#
                ),
            )
            .unwrap();
        }
        root
    }

    #[test]
    fn the_choice_cycles_in_menu_order_and_persists() {
        let root = scratch("prefs");
        let store = SaveStore::new(&root);
        let shared = Arc::new(data());
        let mut locale = Locale::with_data(Some(shared.clone()), Some(store.clone()), None);
        assert_eq!(locale.language(), "INT", "no Steam default: INT");
        assert!(locale.cycle(true));
        assert_eq!(locale.language(), "TST");
        assert!(locale.cycle(true));
        assert_eq!(locale.language(), "INT", "wraps");
        assert!(locale.cycle(false));
        assert_eq!(locale.language(), "TST");
        assert!(!locale.set_language("XYZ"), "not converted");
        assert!(!locale.set_language("TST"), "unchanged");
        match store.load::<LanguagePreference>() {
            LoadOutcome::Loaded(p) => assert_eq!(p.language.as_deref(), Some("TST")),
            other => panic!("{other:?}"),
        }
        // The saved choice wins over the converted default; the environment
        // override wins over both and is not saved.
        let conv = converted("conv");
        assert_eq!(
            Locale::load(Some(&conv), Some(&root), None).language(),
            "TST"
        );
        assert_eq!(
            Locale::load(Some(&conv), Some(&root), Some("int")).language(),
            "INT"
        );
        assert_eq!(Locale::load(Some(&conv), None, None).language(), "INT");
        // An override that names no converted language does not hide the
        // saved choice.
        for bad in ["XYZ", "not a code", ""] {
            assert_eq!(
                Locale::load(Some(&conv), Some(&root), Some(bad)).language(),
                "TST",
                "{bad:?}"
            );
        }
        assert_eq!(
            Locale::load(Some(&conv), None, Some("XYZ")).language(),
            "INT",
            "no saved choice: the converted default"
        );
        assert_eq!(
            Locale::load(None, Some(&root), None).language(),
            "INT",
            "no data"
        );
        // A bad file is set aside, not trusted.
        std::fs::write(root.join("language.json"), "{\"format\": 1}").unwrap();
        let bad = Locale::load(Some(&conv), Some(&root), None);
        assert_eq!(bad.language(), "INT");
        assert!(root.join("language.corrupt-1.json").exists());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&conv);
    }

    #[test]
    fn overlays_show_the_localized_title_and_the_credits_rows() {
        let locale = Locale::with_data(Some(Arc::new(data())), None, None);
        let mut strings = UiStrings::default();
        strings.set_localized(locale.texts());
        let mut app = App::new();
        app.insert_resource(locale)
            .insert_resource(strings)
            .add_systems(Update, localize_overlays);
        let title_text = app.world_mut().spawn(Text::new("placeholder")).id();
        app.world_mut()
            .spawn((crate::kismet::TitleOverlay, Node::default()))
            .add_child(title_text);
        let stand_in = app
            .world_mut()
            .spawn((Text::new("stand-in"), Visibility::Inherited))
            .id();
        let credits = app
            .world_mut()
            .spawn((crate::kismet::CreditsOverlay, Node::default()))
            .add_child(stand_in)
            .id();
        app.update();
        app.update();
        assert_eq!(app.world().get::<Text>(title_text).unwrap().0, "Game title");
        assert_eq!(
            *app.world().get::<Visibility>(stand_in).unwrap(),
            Visibility::Hidden
        );
        let children: Vec<Entity> = app
            .world()
            .get::<Children>(credits)
            .unwrap()
            .iter()
            .collect();
        let list = children
            .iter()
            .copied()
            .find(|e| app.world().get::<LocalizedCredits>(*e).is_some())
            .expect("credits list spawned");
        let rows: Vec<String> = app
            .world()
            .get::<Children>(list)
            .unwrap()
            .iter()
            .map(|e| app.world().get::<Text>(e).unwrap().0.clone())
            .collect();
        assert_eq!(rows.first().map(String::as_str), Some("Role   |   Person"));
        assert_eq!(rows.last().map(String::as_str), Some("skip me"));
        // A language change rebuilds the list once.
        app.world_mut().resource_mut::<Locale>().set_language("TST");
        app.update();
        app.update();
        let lists = app
            .world()
            .get::<Children>(credits)
            .unwrap()
            .iter()
            .filter(|e| app.world().get::<LocalizedCredits>(*e).is_some())
            .count();
        assert_eq!(lists, 1);
    }

    #[test]
    fn tutorial_pop_ups_are_traced_to_their_localized_key() {
        let locale = Locale::with_data(Some(Arc::new(data())), None, Some("TST"));
        let mut strings = UiStrings::default();
        strings.set_localized(locale.texts());
        let shown = strings
            .lookup("tutorial.Tutorial_SpaceToJump")
            .unwrap()
            .to_owned();
        assert_eq!(
            tutorial_source(&strings, locale.language(), &shown),
            "tutorial.Tutorial_SpaceToJump in TST"
        );
        assert_eq!(
            tutorial_source(&strings, locale.language(), "SpaceToJump"),
            "an override or the preset name"
        );
        // The logging system runs with and without a pop-up on screen.
        let mut app = App::new();
        app.insert_resource(locale)
            .insert_resource(strings)
            .init_resource::<crate::kismet::Presentation>()
            .add_systems(Update, log_tutorials);
        app.update();
        app.world_mut()
            .resource_mut::<crate::kismet::Presentation>()
            .tutorial = Some((3, shown.clone(), Some(4.0)));
        app.update();
        app.world_mut()
            .spawn((crate::kismet::TutorialText, Text::new(shown)));
        app.update();
        app.world_mut()
            .resource_mut::<crate::kismet::Presentation>()
            .tutorial = None;
        app.update();
    }

    /// A headless app: the language systems and the real menu builder.
    fn menu_world(locale: Locale, screen: crate::ui::Screen) -> App {
        let mut app = App::new();
        app.add_message::<UiAction>()
            .insert_resource(locale)
            .init_resource::<UiStrings>()
            .init_resource::<UiState>()
            .init_resource::<SubtitleLanguage>()
            .init_resource::<crate::ui::Saves>()
            .init_resource::<crate::ui::UiLaunch>()
            .init_resource::<crate::ui::Play>()
            .init_resource::<crate::ui::settings::UserSettings>()
            .init_resource::<crate::ui::flow::LevelLoad>()
            .init_resource::<bevy::input_focus::InputFocus>()
            .add_systems(
                Update,
                (
                    handle_language_actions,
                    apply_language,
                    crate::ui::menus::rebuild_menu,
                )
                    .chain(),
            );
        app.world_mut().resource_mut::<UiState>().open(screen);
        app
    }

    /// Every text on screen.
    fn shown_texts(app: &mut App) -> Vec<String> {
        let mut query = app.world_mut().query::<&Text>();
        query.iter(app.world()).map(|t| t.0.clone()).collect()
    }

    #[test]
    fn the_open_menu_is_rebuilt_in_the_new_language() {
        let locale = Locale::with_data(Some(Arc::new(data())), None, None);
        let mut app = menu_world(locale, crate::ui::Screen::Main);
        app.update();
        let shown = shown_texts(&mut app);
        assert!(shown.iter().any(|t| t == "Leave"), "{shown:?}");
        assert!(
            shown.iter().any(|t| t == "Settings"),
            "our default: {shown:?}"
        );
        // The settings screen offers the language under its own name.
        app.world_mut()
            .resource_mut::<UiState>()
            .open(crate::ui::Screen::Settings);
        app.update();
        let shown = shown_texts(&mut app);
        assert!(shown.iter().any(|t| t.contains("Int-name")), "{shown:?}");
        // One step: the same screen, rebuilt in the other language.
        app.world_mut().write_message(UiAction::Language(true));
        app.update();
        let shown = shown_texts(&mut app);
        assert!(shown.iter().any(|t| t.contains("Tst-own")), "{shown:?}");
        assert!(!shown.iter().any(|t| t.contains("Int-name")), "{shown:?}");
        app.world_mut()
            .resource_mut::<UiState>()
            .open(crate::ui::Screen::Main);
        app.update();
        let shown = shown_texts(&mut app);
        assert!(shown.iter().any(|t| t == "Tschuess"), "{shown:?}");
        assert!(!shown.iter().any(|t| t == "Leave"), "{shown:?}");
    }

    /// The `tutorialStringPreset` values of the converted Kismet graphs in
    /// `dir` (a plain scan of the JSON text; preset names are identifiers).
    fn kismet_tutorial_presets(dir: &std::path::Path) -> Vec<String> {
        const KEY: &str = "\"tutorialStringPreset\"";
        let mut out: Vec<String> = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let mut rest = text.as_str();
            while let Some(at) = rest.find(KEY) {
                rest = &rest[at + KEY.len()..];
                let value = rest.trim_start().trim_start_matches(':').trim_start();
                if let Some(name) = value
                    .strip_prefix('"')
                    .and_then(|v| v.split('"').next())
                    .filter(|n| !n.is_empty() && !out.iter().any(|o| o == n))
                {
                    out.push(name.to_owned());
                }
            }
        }
        out.sort();
        out
    }

    /// Real converted data (skipped unless `ASAMU_CONVERTED_DIR` holds an
    /// `asamu-import localization` output): in every language the main menu
    /// the app builds shows that language's own labels (not the `INT`
    /// fallback, not our defaults), and every tutorial preset the converted
    /// Kismet graphs use (when `kismet/` is converted too) has that
    /// language's own text under the key `kismet.rs` reads. Prints counts
    /// only, never text.
    #[test]
    fn converted_menu_and_kismet_tutorials_show_each_language() {
        let Some(dir) = std::env::var_os("ASAMU_CONVERTED_DIR") else {
            eprintln!("skipping: ASAMU_CONVERTED_DIR not set");
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let Ok(Some(data)) = Localization::load(&dir) else {
            eprintln!("skipping: no converted localization");
            return;
        };
        let data = Arc::new(data);
        let presets = kismet_tutorial_presets(&dir.join("kismet"));
        if presets.is_empty() {
            eprintln!("no converted Kismet graphs: tutorial presets not checked");
        }
        let own = |lang: &str, key: &str| {
            data.table(lang)
                .and_then(|t| t.string(key))
                .map(display_text)
        };
        for lang in data.languages() {
            let locale = Locale::with_data(Some(data.clone()), None, Some(lang));
            assert_eq!(locale.language(), lang);
            let texts = locale.texts();
            let mut differ = 0;
            for preset in &presets {
                let key = format!("ASAMUHUD.{preset}");
                let raw = own(lang, &key).unwrap_or_else(|| panic!("{lang}: no own {key}"));
                let shown = texts
                    .get(&format!("tutorial.{preset}"))
                    .unwrap_or_else(|| panic!("{lang}: tutorial.{preset} missing"));
                assert!(!shown.trim().is_empty(), "{lang}: {preset}");
                // The shown text is the table's text but for the placeholders.
                let first = raw.split('#').next().unwrap_or("");
                assert!(shown.starts_with(first), "{lang}: {preset}");
                if own("INT", &key).as_deref() != Some(raw.as_str()) {
                    differ += 1;
                }
            }
            let mut app = menu_world(locale, crate::ui::Screen::Main);
            app.update();
            let shown = shown_texts(&mut app);
            let mut labels = 0;
            for key in [
                "GFxASAMUMainMenu.startBtnLabel",
                "GFxASAMUMainMenu.LevelSelectLabel",
                "GFxASAMUMainMenu.TimeTrialLabel",
                "GFxASAMUMenu.OptionsLabel",
                "GFxASAMUMenu.ExitLabel",
            ] {
                let want = own(lang, key).unwrap_or_else(|| panic!("{lang}: no own {key}"));
                assert!(
                    shown.contains(&want),
                    "{lang}: {key} is not on the main menu"
                );
                labels += 1;
            }
            eprintln!(
                "{lang}: {labels} main-menu labels shown from the language's own table; \
                 {} Kismet tutorial presets resolved, {differ} differ from INT",
                presets.len()
            );
        }
    }

    /// Real converted data (skipped unless `ASAMU_CONVERTED_DIR` holds an
    /// `asamu-import localization` output): in every language the menus'
    /// labels, the tutorial presets Kismet uses and the achievement names
    /// resolve without leftover placeholders or escapes, and the converted
    /// audio's subtitle lines translate. Prints counts only, never text.
    #[test]
    fn converted_localization_reaches_menus_tutorials_and_subtitles() {
        let Some(dir) = std::env::var_os("ASAMU_CONVERTED_DIR") else {
            eprintln!("skipping: ASAMU_CONVERTED_DIR not set");
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let Ok(Some(data)) = Localization::load(&dir) else {
            eprintln!("skipping: no converted localization");
            return;
        };
        let data = Arc::new(data);
        let audio = asamu_assets::audio::AudioLibrary::load(&dir.join("audio")).ok();
        for lang in data.languages() {
            let locale = Locale::with_data(Some(data.clone()), None, Some(lang));
            let texts = locale.texts();
            let labels = LABEL_KEYS
                .iter()
                .filter(|(k, _)| texts.contains_key(*k))
                .count();
            let tutorials: Vec<&String> = texts
                .iter()
                .filter(|(k, _)| k.starts_with("tutorial."))
                .map(|(_, v)| v)
                .collect();
            assert_eq!(tutorials.len(), 26, "{lang}");
            for t in &tutorials {
                assert!(!t.contains("\\n"), "{lang}: escape left");
                for p in ["#MOVE#", "#MOUSE#", "#SPACE#", "#SHIFT#", "#RMB#", "#LMB#"] {
                    assert!(!t.contains(p), "{lang}: placeholder left");
                }
            }
            let chapters = ChapterId::ALL
                .iter()
                .filter(|c| texts.contains_key(&format!("chapter.{}", c.enum_name())))
                .count();
            assert_eq!(chapters, 7, "{lang}");
            let achievements = Achievement::ALL
                .iter()
                .filter(|a| texts.contains_key(&format!("achievement.{}", a.name())))
                .count();
            assert!(locale.display_name().is_some(), "{lang}");
            let translated = audio.as_ref().map_or(0, |lib| {
                let tr = data.subtitle_translation(lang);
                lib.subtitles
                    .values()
                    .flat_map(|t| &t.lines)
                    .filter(|l| !l.text.is_empty() && tr.translate(&l.text) != l.text)
                    .count()
            });
            // Where a wave has as many lines in the language as in the audio
            // export, each shown line must be that wave's own line (a text
            // the language translates in two ways can only show one: SLO has
            // one such line in the shipped data).
            let (mut paired, mut wrong) = (0, 0);
            if let Some(lib) = &audio {
                let tr = data.subtitle_translation(lang);
                for (wave, track) in &lib.subtitles {
                    let Some(own) = data.subtitles(lang, wave) else {
                        continue;
                    };
                    if own.len() != track.lines.len() {
                        continue;
                    }
                    for (shown, want) in track.lines.iter().zip(own) {
                        if shown.text.is_empty() {
                            continue;
                        }
                        paired += 1;
                        if tr.translate(&shown.text) != want.text {
                            wrong += 1;
                        }
                    }
                }
                assert!(
                    wrong * 100 <= paired,
                    "{lang}: {wrong} of {paired} lines show another line's text"
                );
            }
            let mut strings = UiStrings::default();
            strings.set_localized(texts.clone());
            let credits = credits_lines(&data, &strings).len();
            eprintln!(
                "{lang}: {labels}/{} labels, {} tutorials, {chapters} chapters, \
                 {achievements} achievements, {translated} audio subtitle lines translated \
                 ({wrong} of {paired} line-for-line pairs show another line), {credits} credits lines",
                LABEL_KEYS.len(),
                tutorials.len()
            );
        }
    }

    #[test]
    fn preference_documents_round_trip_and_validate() {
        let p = LanguagePreference {
            language: Some("DEU".into()),
        };
        let text = asamu_game::save::encode_document(&p).unwrap();
        let back: LanguagePreference = asamu_game::save::decode_document(&text).unwrap();
        assert_eq!(back, p);
        let none: LanguagePreference = asamu_game::save::decode_document(
            r#"{"format": "asamu-decomp/language", "format_version": 1}"#,
        )
        .unwrap();
        assert_eq!(none.language, None);
        assert!(
            asamu_game::save::decode_document::<LanguagePreference>(
                r#"{"format": "asamu-decomp/language", "format_version": 1, "language": "../x"}"#
            )
            .is_err()
        );
        assert!(
            asamu_game::save::decode_document::<LanguagePreference>(
                r#"{"format": "asamu-decomp/language", "format_version": 1, "language": 3}"#
            )
            .is_err()
        );
    }

    #[test]
    fn a_language_change_refills_strings_subtitles_and_the_menu() {
        let mut app = App::new();
        app.add_message::<UiAction>()
            .insert_resource(Locale::with_data(Some(Arc::new(data())), None, None))
            .init_resource::<UiStrings>()
            .init_resource::<UiState>()
            .init_resource::<SubtitleLanguage>()
            .add_systems(Update, (handle_language_actions, apply_language).chain());
        app.update();
        assert_eq!(
            app.world().resource::<UiStrings>().get("menu.quit", "Quit"),
            "Leave"
        );
        assert!(app.world().resource::<SubtitleLanguage>().0.is_some());
        app.world_mut().resource_mut::<UiState>().dirty = false;
        app.update();
        assert!(!app.world().resource::<UiState>().dirty, "nothing changed");
        app.world_mut().write_message(UiAction::Language(true));
        app.update();
        assert_eq!(app.world().resource::<Locale>().language(), "TST");
        assert_eq!(
            app.world().resource::<UiStrings>().get("menu.quit", "Quit"),
            "Tschuess"
        );
        assert!(app.world().resource::<UiState>().dirty, "menu rebuilt");
    }
}
