//! User settings: the model and its file live in [`asamu_game::save`]
//! ([`Settings`], `settings.json` in the user data directory); this module
//! applies them (window mode and size, master volume, subtitle visibility)
//! and offers the conversions `main.rs` uses for the camera FOV and mouse
//! look.
//!
//! The audio module applies the music / SFX / voice volumes as the sound
//! classes' own volumes (`Music`, `SFX`, `Voice`) and drops the subtitle line
//! while subtitles are off; the master volume is Bevy's global volume (here).

use std::path::{Path, PathBuf};

use asamu_game::save::{LoadOutcome, SaveStore, Settings};
use bevy::audio::{GlobalVolume, Volume};
use bevy::prelude::*;
use bevy::window::{MonitorSelection, PrimaryWindow, WindowMode};

use crate::MOUSE_RADIANS_PER_COUNT;
use crate::hud::SubtitleBox;

/// Window sizes offered by the settings menu (ours); `None` = the app's
/// default window.
pub(crate) const RESOLUTIONS: [Option<[u32; 2]>; 6] = [
    None,
    Some([1280, 720]),
    Some([1600, 900]),
    Some([1920, 1080]),
    Some([2560, 1440]),
    Some([3840, 2160]),
];

/// Audio groups of the original's settings (`*GroupVolume`, SAVE.md §7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code, reason = "integration point: read by the audio workstream")]
pub(crate) enum AudioGroup {
    /// Music.
    Music,
    /// Sound effects.
    Sfx,
    /// Voice / narration.
    Voice,
}

/// The settings in use and where they are stored.
#[derive(Resource, Debug, Clone, Default)]
pub(crate) struct UserSettings {
    /// Current values (always sanitized).
    pub settings: Settings,
    /// The store holding `settings.json` (`None`: not persisted).
    store: Option<SaveStore>,
    /// What [`apply_settings`] applied last.
    applied: Option<Settings>,
}

impl UserSettings {
    /// Loads `settings.json` from `root` (defaults when missing; an
    /// unreadable file is set aside and defaults are used).
    #[must_use]
    pub fn load(root: Option<&Path>) -> Self {
        let mut store = root.map(|r| SaveStore::new(PathBuf::from(r)));
        let settings = match store.as_ref().map(SaveStore::load::<Settings>) {
            Some(LoadOutcome::Loaded(s)) => s,
            Some(LoadOutcome::Unreadable {
                error,
                quarantined: Some(q),
            }) => {
                warn!(
                    "settings unreadable ({error}); using defaults (old file moved to {})",
                    q.display()
                );
                Settings::default()
            }
            Some(LoadOutcome::Unreadable {
                error,
                quarantined: None,
            }) => {
                // It could not be set aside: never overwrite it.
                warn!("settings unreadable ({error}); using defaults, changes are not saved");
                store = None;
                Settings::default()
            }
            Some(LoadOutcome::Missing) | None => Settings::default(),
        };
        Self {
            settings,
            store,
            applied: None,
        }
    }

    /// Replaces the settings (sanitized) and writes the file.
    pub fn set(&mut self, settings: Settings) {
        self.settings = settings.sanitized();
        if let Some(store) = &self.store
            && let Err(e) = store.save(&self.settings)
        {
            warn!("could not save the settings: {e}");
        }
    }

    /// Mouse look scale per count: (yaw, pitch); pitch is negated when the
    /// mouse is inverted.
    #[must_use]
    pub fn look_scale(&self) -> (f32, f32) {
        let s = MOUSE_RADIANS_PER_COUNT * self.settings.mouse_sensitivity;
        (s, if self.settings.invert_mouse { -s } else { s })
    }

    /// The camera's horizontal FOV: the game's run-time FOV shifted by the
    /// user's FOV setting relative to the parameter default (`base`), so the
    /// story-mode zoom keeps working (ours; the original's exact handling of
    /// a changed FOV during zoom is not traced).
    #[must_use]
    pub fn view_fov_degrees(&self, game_fov: f32, base: f32) -> f32 {
        view_fov_degrees(&self.settings, game_fov, base)
    }

    /// Effective linear volume of a group (master × group).
    #[must_use]
    #[allow(dead_code, reason = "integration point: read by the audio workstream")]
    pub fn group_volume(&self, group: AudioGroup) -> f32 {
        let s = &self.settings;
        s.master_volume
            * match group {
                AudioGroup::Music => s.music_volume,
                AudioGroup::Sfx => s.sfx_volume,
                AudioGroup::Voice => s.voice_volume,
            }
    }
}

/// See [`UserSettings::view_fov_degrees`].
#[must_use]
pub(crate) fn view_fov_degrees(settings: &Settings, game_fov: f32, base: f32) -> f32 {
    let fov = game_fov + settings.fov_degrees - base;
    if fov.is_finite() {
        fov.clamp(10.0, 170.0)
    } else {
        settings.fov_degrees
    }
}

/// `value` moved by `steps` increments of `step` on a grid of `step`
/// (avoids drifting decimals), clamped to `[lo, hi]`.
#[must_use]
pub(crate) fn step_value(value: f32, step: f32, steps: i32, [lo, hi]: [f32; 2]) -> f32 {
    let grid = (value / step).round() + steps as f32;
    (grid * step).clamp(lo, hi)
}

/// The next (`forward`) or previous entry of [`RESOLUTIONS`] after
/// `current` (an unlisted size starts from the default).
#[must_use]
pub(crate) fn cycle_resolution(current: Option<[u32; 2]>, forward: bool) -> Option<[u32; 2]> {
    let n = RESOLUTIONS.len();
    let i = RESOLUTIONS.iter().position(|r| *r == current).unwrap_or(0);
    let next = if forward {
        (i + 1) % n
    } else {
        (i + n - 1) % n
    };
    RESOLUTIONS.get(next).copied().flatten()
}

/// Applies changed settings: window mode and size, global (master) volume.
pub(crate) fn apply_settings(
    mut user: ResMut<UserSettings>,
    mut window: Query<&mut Window, With<PrimaryWindow>>,
    volume: Option<ResMut<GlobalVolume>>,
) {
    let current = user.settings;
    let previous = user.applied;
    if previous == Some(current) {
        return;
    }
    if let Ok(mut w) = window.single_mut() {
        let window_changed = previous.is_none_or(|p| {
            p.fullscreen != current.fullscreen || p.resolution != current.resolution
        });
        if window_changed {
            w.mode = if current.fullscreen {
                WindowMode::BorderlessFullscreen(MonitorSelection::Current)
            } else {
                WindowMode::Windowed
            };
            if !current.fullscreen
                && let Some([width, height]) = current.resolution
            {
                w.resolution.set(width as f32, height as f32);
            }
        }
    }
    if let Some(mut v) = volume {
        // Bevy applies the global volume to sounds that start after it
        // changes; the audio workstream mixes live from `UserSettings`.
        v.volume = Volume::Linear(current.master_volume);
    }
    user.applied = Some(current);
}

/// Hides the subtitle box while subtitles are off (the HUD shows it whenever
/// a subtitle is set).
pub(crate) fn hide_subtitles_when_off(
    user: Res<UserSettings>,
    mut boxes: Query<&mut Visibility, With<SubtitleBox>>,
) {
    if user.settings.subtitles {
        return;
    }
    for mut v in &mut boxes {
        if *v != Visibility::Hidden {
            *v = Visibility::Hidden;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fov_follows_the_setting_and_keeps_the_zoom_offset() {
        let mut s = Settings::default();
        assert_eq!(view_fov_degrees(&s, 90.0, 90.0), 90.0);
        s.fov_degrees = 100.0;
        assert_eq!(view_fov_degrees(&s, 90.0, 90.0), 100.0);
        assert_eq!(
            view_fov_degrees(&s, 50.0, 90.0),
            60.0,
            "zoom keeps its 40 degrees"
        );
        assert_eq!(view_fov_degrees(&s, f32::NAN, 90.0), 100.0);
    }

    #[test]
    fn look_scale_applies_sensitivity_and_invert() {
        let mut u = UserSettings::default();
        assert_eq!(
            u.look_scale(),
            (MOUSE_RADIANS_PER_COUNT, MOUSE_RADIANS_PER_COUNT)
        );
        u.settings.mouse_sensitivity = 2.0;
        u.settings.invert_mouse = true;
        let (yaw, pitch) = u.look_scale();
        assert_eq!(yaw, 2.0 * MOUSE_RADIANS_PER_COUNT);
        assert_eq!(pitch, -yaw);
        assert!((u.group_volume(AudioGroup::Music) - 0.8).abs() < 1e-6);
    }

    #[test]
    fn steps_stay_on_the_grid_and_in_range() {
        let mut v = 0.8;
        for _ in 0..3 {
            v = step_value(v, 0.05, 1, [0.0, 1.0]);
        }
        assert!((v - 0.95).abs() < 1e-6, "{v}");
        assert_eq!(step_value(v, 0.05, 5, [0.0, 1.0]), 1.0);
        assert_eq!(step_value(0.0, 0.05, -1, [0.0, 1.0]), 0.0);
        assert_eq!(step_value(90.0, 5.0, -1, Settings::FOV_RANGE), 85.0);
    }

    #[test]
    fn settings_persist_and_a_bad_file_is_never_overwritten_in_place() {
        let dir = std::env::temp_dir().join(format!("asamu-ui-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("settings.json");
        // Missing: defaults; a change is written and read back.
        let mut u = UserSettings::load(Some(&dir));
        assert_eq!(u.settings, Settings::default());
        let mut s = u.settings;
        s.fov_degrees = 100.0;
        u.set(s);
        assert_eq!(UserSettings::load(Some(&dir)).settings.fov_degrees, 100.0);
        // Unreadable: set aside, defaults, later changes write a new file.
        std::fs::write(&file, "{oops").unwrap();
        let mut u = UserSettings::load(Some(&dir));
        assert_eq!(u.settings, Settings::default());
        assert!(dir.join("settings.corrupt-1.json").exists());
        u.set(Settings::default());
        assert!(file.exists());
        // Unreadable and impossible to set aside: never written.
        std::fs::write(&file, "{oops").unwrap();
        for n in 1..=999 {
            std::fs::write(dir.join(format!("settings.corrupt-{n}.json")), "").unwrap();
        }
        let mut u = UserSettings::load(Some(&dir));
        u.set(Settings::default());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{oops");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolutions_cycle_both_ways() {
        assert_eq!(cycle_resolution(None, true), Some([1280, 720]));
        assert_eq!(cycle_resolution(None, false), Some([3840, 2160]));
        assert_eq!(cycle_resolution(Some([3840, 2160]), true), None);
        assert_eq!(
            cycle_resolution(Some([1000, 1000]), true),
            Some([1280, 720])
        );
    }
}
