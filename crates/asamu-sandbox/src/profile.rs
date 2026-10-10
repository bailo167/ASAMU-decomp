//! Sandbox profiles and the Sandbox's own directories.
//!
//! A profile is a small versioned JSON file: overrides on a **named base**
//! plus sticky rules. It never stores a whole parameter set, so a profile
//! written today still starts from the then-current Classic values tomorrow.
//!
//! ```json
//! {
//!   "format": "asamu-sandbox-profile",
//!   "version": 1,
//!   "name": "floaty",
//!   "description": "example values, ours, not the original's",
//!   "base": "asamu_original",
//!   "overrides": { "movement.custom_gravity_scaling": 0.5 },
//!   "rules": { "grapples": "unlimited", "rocket_boots": "on", "auto_refill": false },
//!   "time_scale": null,
//!   "extensions": {}
//! }
//! ```
//!
//! `base` is required on purpose: the default parameter set of
//! `asamu-player` is the placeholder set, so a profile must say what it
//! overrides. Built-in presets are Rust constructors whose overrides are
//! factors of the Classic value read when they are built; the factors are
//! ours.
//!
//! Everything the Sandbox writes goes under `<user data>/sandbox/`
//! ([`SandboxDirs`]): never into `saves/`, `settings.json`, the parity trace
//! directory, the converted data or the repository.
//!
//! Reading is hostile-input safe: a size limit before parsing, the format
//! marker and version checked before the rest, unknown fields refused, every
//! override checked against the parameter catalogue and the resulting set
//! validated. Nothing here panics on a malformed file.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::keys::{Catalog, TuneValue};
use crate::overlay::{BASE_CLASSIC, Overlay, OverlayError};
use crate::rules::{GrappleRule, Rules};
use crate::time::SPEED_STEPS;

/// The `format` marker of a profile file.
pub const PROFILE_FORMAT: &str = "asamu-sandbox-profile";
/// The profile schema version this build reads and writes.
pub const PROFILE_VERSION: u32 = 1;
/// A profile file larger than this is refused unread.
pub const MAX_PROFILE_BYTES: usize = 256 * 1024;

/// A tuning profile: overrides on a named base, plus rules.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// [`PROFILE_FORMAT`].
    pub format: String,
    /// [`PROFILE_VERSION`].
    pub version: u32,
    /// File-safe name ([`valid_profile_name`]).
    pub name: String,
    /// Free text.
    #[serde(default)]
    pub description: String,
    /// The parameter set the overrides apply to. Required;
    /// [`BASE_CLASSIC`] in v1.
    pub base: String,
    /// Parameter overrides.
    #[serde(default)]
    pub overrides: Overlay,
    /// Sticky rules.
    #[serde(default)]
    pub rules: Rules,
    /// Simulation speed the profile starts with (`None`: normal speed).
    #[serde(default)]
    pub time_scale: Option<f32>,
    /// Room for later tools; kept as read, never interpreted here.
    #[serde(default)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl Profile {
    /// The profile that changes nothing: the Classic set, default rules.
    #[must_use]
    pub fn classic() -> Self {
        Self {
            format: PROFILE_FORMAT.to_owned(),
            version: PROFILE_VERSION,
            name: "classic".to_owned(),
            description: "The Classic parameter set, unchanged.".to_owned(),
            base: BASE_CLASSIC.to_owned(),
            overrides: Overlay::default(),
            rules: Rules::default(),
            time_scale: None,
            extensions: BTreeMap::new(),
        }
    }

    /// The built-in presets, `classic` first.
    ///
    /// Every preset is **ours**: each override is a factor of the Classic
    /// value read here, when the preset is built (no Classic number is
    /// written down), and the factors were picked by us for a recognisable
    /// feel. None of them is a mode of the original game.
    #[must_use]
    pub fn builtin() -> Vec<Profile> {
        let unlimited = Rules {
            grapples: GrappleRule::Unlimited,
            auto_refill: true,
            ..Rules::default()
        };
        vec![
            Self::classic(),
            preset(
                "moon",
                "Low gravity: the pawn's gravity scale at 0.35 of Classic. Ours.",
                &[("movement.custom_gravity_scaling", 0.35)],
            ),
            preset(
                "heavy",
                "High gravity: gravity scale at 1.8 of Classic, jump at 0.85. Ours.",
                &[
                    ("movement.custom_gravity_scaling", 1.8),
                    ("movement.jump_velocity", 0.85),
                ],
            ),
            preset(
                "super-jump",
                "Jump at 1.6 of Classic, power jump at 1.5. Ours.",
                &[
                    ("movement.jump_velocity", 1.6),
                    ("pawn.power_jump_strength", 1.5),
                ],
            ),
            preset(
                "ice",
                "Slippery floor: ground friction at 0.1 of Classic, acceleration at 0.4. Ours.",
                &[
                    ("movement.ground_friction", 0.1),
                    ("movement.ground_acceleration", 0.4),
                ],
            ),
            preset(
                "sprinter",
                "Walking speed at 1.5 of Classic, sprint multiplier at 1.25. Ours.",
                &[
                    ("pawn.move_speed", 1.5),
                    ("pawn.sprint_speed_multiplier", 1.25),
                ],
            ),
            preset(
                "long-reach",
                "Grapple reach at twice Classic. Ours.",
                &[("gun.max_distance", 2.0)],
            ),
            Self {
                name: "infinite-grapple".to_owned(),
                description: "Classic values; the grapple budget is unlimited and refills by \
                              itself. A rule, not a parameter. Ours."
                    .to_owned(),
                rules: unlimited,
                ..Self::classic()
            },
            Self {
                name: "bullet-time".to_owned(),
                description: "Classic values at a quarter of normal speed (the simulation step \
                              itself is unchanged). Ours."
                    .to_owned(),
                time_scale: Some(0.25),
                ..Self::classic()
            },
        ]
    }

    /// Parses and checks a profile file: size, format marker, version, name,
    /// base, time scale and that every override applies. The overrides come
    /// back in the form [`Overlay::set`] stores them.
    ///
    /// # Errors
    /// Any of those checks; never panics on malformed input.
    pub fn from_json_slice(bytes: &[u8]) -> Result<Self, ProfileError> {
        if bytes.len() > MAX_PROFILE_BYTES {
            return Err(ProfileError::TooLarge {
                bytes: bytes.len(),
                limit: MAX_PROFILE_BYTES,
            });
        }
        // The marker and the version first, so a file of another kind or a
        // later version is named as such instead of failing on a field.
        #[derive(Deserialize)]
        struct Head {
            #[serde(default)]
            format: Option<String>,
            #[serde(default)]
            version: Option<u32>,
        }
        let head: Head =
            serde_json::from_slice(bytes).map_err(|e| ProfileError::Json(e.to_string()))?;
        match head.format {
            Some(format) if format == PROFILE_FORMAT => {}
            other => return Err(ProfileError::WrongFormat(other.unwrap_or_default())),
        }
        match head.version {
            Some(PROFILE_VERSION) => {}
            Some(other) => return Err(ProfileError::UnsupportedVersion(other)),
            None => return Err(ProfileError::Json("missing field `version`".to_owned())),
        }
        // A version 1 profile is a JSON object with named fields, and so are
        // its rules. The derived readers would also take the fields in order
        // from a JSON array (`"rules": []` as the default rules, a whole
        // profile as a list), which is not the format: refused here.
        #[derive(Deserialize)]
        struct Shape {
            #[serde(default)]
            rules: Option<BTreeMap<String, serde::de::IgnoredAny>>,
        }
        if !is_json_object(bytes) {
            return Err(ProfileError::Json(
                "a profile is a JSON object with named fields".to_owned(),
            ));
        }
        // Only that it reads matters; the field names are checked below.
        let Shape { rules } = serde_json::from_slice(bytes)
            .map_err(|e| ProfileError::Json(format!("`rules` is not an object: {e}")))?;
        drop(rules);
        let mut profile: Self =
            serde_json::from_slice(bytes).map_err(|e| ProfileError::Json(e.to_string()))?;
        profile.check()?;
        profile.overrides = profile.overrides.normalized()?;
        Ok(profile)
    }

    /// The checks every profile must pass before it is used or written:
    /// marker, version, name, base, time scale, overrides.
    ///
    /// # Errors
    /// The first failing check.
    pub fn check(&self) -> Result<(), ProfileError> {
        if self.format != PROFILE_FORMAT {
            return Err(ProfileError::WrongFormat(self.format.clone()));
        }
        if self.version != PROFILE_VERSION {
            return Err(ProfileError::UnsupportedVersion(self.version));
        }
        if !valid_profile_name(&self.name) {
            return Err(ProfileError::BadName(self.name.clone()));
        }
        if self.base != BASE_CLASSIC {
            return Err(ProfileError::UnknownBase(self.base.clone()));
        }
        if let Some(scale) = self.time_scale {
            let (slowest, fastest) = speed_range();
            if !(scale.is_finite() && scale >= slowest && scale <= fastest) {
                return Err(ProfileError::Json(format!(
                    "time_scale must be between {slowest} and {fastest} (got {scale})"
                )));
            }
        }
        self.overrides.apply()?;
        Ok(())
    }

    /// The profile as pretty-printed JSON.
    ///
    /// # Errors
    /// Serialization failure.
    pub fn to_json_pretty(&self) -> Result<String, ProfileError> {
        serde_json::to_string_pretty(self).map_err(|e| ProfileError::Json(e.to_string()))
    }

    /// The profile changes nothing: Classic base, no override, default
    /// rules, normal speed.
    #[must_use]
    pub fn is_pristine(&self) -> bool {
        self.base == BASE_CLASSIC
            && self.overrides.is_empty()
            && self.rules.is_default()
            && self.time_scale.is_none()
    }
}

/// A profile name is 1 to 40 characters of `a-z`, `0-9`, `_` and `-`, so it
/// is always a safe file stem.
#[must_use]
pub fn valid_profile_name(name: &str) -> bool {
    (1..=40).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// The first byte of `bytes` that is not JSON white space opens an object.
fn is_json_object(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .find(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        .is_some_and(|b| *b == b'{')
}

/// The slowest and fastest speed factor a profile may start with: the ends
/// of the time control's speed steps.
fn speed_range() -> (f32, f32) {
    SPEED_STEPS
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), s| {
            (lo.min(*s), hi.max(*s))
        })
}

/// A built-in preset: each `(key, factor)` overrides the key with `factor`
/// times its Classic value, read from the catalogue now and rounded to the
/// key's nudge grid. A factor that no longer gives a valid set (because a
/// Classic value changed) is left out rather than guessed at; the tests
/// notice a preset that lost an override.
fn preset(name: &str, description: &str, factors: &[(&str, f64)]) -> Profile {
    let catalog = Catalog::shared();
    let mut overrides = Overlay::default();
    for (key, factor) in factors {
        let Some(info) = catalog.get(key) else {
            continue;
        };
        let TuneValue::Float(classic) = info.classic else {
            continue;
        };
        let grid = info.step / 1000.0;
        let target = classic * factor;
        let rounded = if grid.is_finite() && grid > 0.0 {
            // Print-and-parse leaves the round decimal, not its binary noise.
            let snapped = (target / grid).round() * grid;
            format!("{snapped:.9}").parse::<f64>().unwrap_or(snapped)
        } else {
            target
        };
        // Refused values are skipped (see above).
        let _ = overrides.set(key, TuneValue::Float(rounded));
    }
    Profile {
        name: name.to_owned(),
        description: description.to_owned(),
        overrides,
        ..Profile::classic()
    }
}

/// Why a profile could not be read or written.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ProfileError {
    /// The file is larger than [`MAX_PROFILE_BYTES`].
    #[error("the profile is {bytes} bytes (at most {limit})")]
    TooLarge {
        /// Size found.
        bytes: usize,
        /// The limit.
        limit: usize,
    },
    /// Not valid JSON for a profile.
    #[error("not a valid profile: {0}")]
    Json(String),
    /// Wrong `format` marker.
    #[error("not a sandbox profile (format {0:?}, expected {PROFILE_FORMAT:?})")]
    WrongFormat(String),
    /// A schema version this build does not read.
    #[error("unsupported profile version {0} (supported: {PROFILE_VERSION})")]
    UnsupportedVersion(u32),
    /// A base this build does not know.
    #[error("unknown base {0:?} (supported: {BASE_CLASSIC:?})")]
    UnknownBase(String),
    /// The name is not a [`valid_profile_name`].
    #[error("{0:?} is not a profile name (1 to 40 of a-z, 0-9, _ and -)")]
    BadName(String),
    /// An override does not apply.
    #[error(transparent)]
    Overlay(#[from] OverlayError),
    /// No profile of that name.
    #[error("no profile named {0:?}")]
    NotFound(String),
    /// Reading or writing failed.
    #[error("{action} {path}: {message}")]
    Io {
        /// What was attempted, e.g. `write`.
        action: &'static str,
        /// The file or directory.
        path: String,
        /// The system's message.
        message: String,
    },
}

/// The Sandbox's directories under the user data directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxDirs {
    /// `<user data>/sandbox`.
    pub root: PathBuf,
}

impl SandboxDirs {
    /// `<SaveStore::default_root()>/sandbox` (`ASAMU_SAVE_DIR` overrides the
    /// root, as for saves). `None` without a user data directory. Creates
    /// nothing.
    #[must_use]
    pub fn default_location() -> Option<Self> {
        asamu_game::save::SaveStore::default_root().map(|root| Self {
            root: root.join("sandbox"),
        })
    }

    /// Where user profiles are stored.
    #[must_use]
    pub fn profiles(&self) -> PathBuf {
        self.root.join("profiles")
    }

    /// Where Sandbox recordings are written.
    #[must_use]
    pub fn recordings(&self) -> PathBuf {
        self.root.join("recordings")
    }

    /// Where inspection dumps are written.
    #[must_use]
    pub fn dumps(&self) -> PathBuf {
        self.root.join("dumps")
    }
}

/// File-name suffix of a stored profile.
const PROFILE_FILE_SUFFIX: &str = ".json";

/// Creates `path` holding `text` and a final newline, flushed to disk. The
/// path must be free: a file, a directory or a link that is already there is
/// an error, so nothing is ever written through a link.
fn write_new_file(path: &Path, text: &str) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(text.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()
}

/// Most profiles [`ProfileStore::list`] returns (a bound against a directory
/// someone filled with files, not a feature limit).
const MAX_LISTED_PROFILES: usize = 512;

/// User profiles in one directory (`<name>.json`).
#[derive(Clone, Debug)]
pub struct ProfileStore {
    dir: PathBuf,
}

impl ProfileStore {
    /// A store over `dir` (created on the first save).
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the profile named `name` is stored.
    fn path_of(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}{PROFILE_FILE_SUFFIX}"))
    }

    /// Names of the stored profiles, sorted: the `<name>.json` files whose
    /// name is a [`valid_profile_name`]. Unreadable entries are skipped, a
    /// missing directory lists nothing, and no file is opened (a listed
    /// file may still fail to [`ProfileStore::load`]).
    #[must_use]
    pub fn list(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|entry| {
                let file = entry.file_name();
                let name = file.to_str()?.strip_suffix(PROFILE_FILE_SUFFIX)?;
                valid_profile_name(name).then(|| name.to_owned())
            })
            .take(MAX_LISTED_PROFILES)
            .collect();
        names.sort();
        names
    }

    /// Loads the profile named `name`. The file's own `name` field is
    /// replaced by the file name, so a profile is always known by the name
    /// it was asked for.
    ///
    /// # Errors
    /// Bad name, missing file, or a file that fails
    /// [`Profile::from_json_slice`].
    pub fn load(&self, name: &str) -> Result<Profile, ProfileError> {
        if !valid_profile_name(name) {
            return Err(ProfileError::BadName(name.to_owned()));
        }
        let path = self.path_of(name);
        let io = |action: &'static str, e: std::io::Error| ProfileError::Io {
            action,
            path: path.display().to_string(),
            message: e.to_string(),
        };
        // Only a regular file in the store's own directory is a profile (a
        // link to somewhere else is not followed).
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => return Err(ProfileError::NotFound(name.to_owned())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ProfileError::NotFound(name.to_owned()));
            }
            Err(e) => return Err(io("open", e)),
        }
        let file = std::fs::File::open(&path).map_err(|e| io("open", e))?;
        // One byte past the limit is enough to know the file is too large;
        // nothing more is ever read into memory.
        let mut bytes = Vec::new();
        file.take(MAX_PROFILE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io("read", e))?;
        let mut profile = Profile::from_json_slice(&bytes)?;
        profile.name = name.to_owned();
        Ok(profile)
    }

    /// Writes `profile` as `<name>.json` (temporary file, then rename) and
    /// returns the path. Only a profile that [`Profile::check`] accepts and
    /// that reads back as itself is written.
    ///
    /// # Errors
    /// Bad name, a profile that would not load again, or the write failed.
    pub fn save(&self, profile: &Profile) -> Result<PathBuf, ProfileError> {
        profile.check()?;
        let json = profile.to_json_pretty()?;
        // Never leave a file behind that this build cannot read.
        Profile::from_json_slice(json.as_bytes())?;

        let path = self.path_of(&profile.name);
        let temporary = self.dir.join(format!(
            ".{}{PROFILE_FILE_SUFFIX}.{}.tmp",
            profile.name,
            std::process::id()
        ));
        let io = |action: &'static str, at: &Path, e: std::io::Error| ProfileError::Io {
            action,
            path: at.display().to_string(),
            message: e.to_string(),
        };
        std::fs::create_dir_all(&self.dir).map_err(|e| io("create", &self.dir, e))?;
        // What an earlier run left under the temporary name goes first (the
        // entry itself: a link is removed, not followed).
        let _ = std::fs::remove_file(&temporary);
        if let Err(e) = write_new_file(&temporary, &json) {
            let _ = std::fs::remove_file(&temporary);
            return Err(io("write", &temporary, e));
        }
        if let Err(e) = std::fs::rename(&temporary, &path) {
            let _ = std::fs::remove_file(&temporary);
            return Err(io("rename", &path, e));
        }
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::TuneValue;
    use crate::rules::{GrappleRule, Switch};

    /// The file layout documented at the top of this module.
    const EXAMPLE: &str = r#"{
      "format": "asamu-sandbox-profile",
      "version": 1,
      "name": "floaty",
      "description": "example values, ours, not the original's",
      "base": "asamu_original",
      "overrides": {
        "movement.custom_gravity_scaling": 0.5,
        "pawn.zoom_enabled": false
      },
      "rules": { "grapples": "unlimited", "rocket_boots": "on", "auto_refill": false },
      "time_scale": null,
      "extensions": {}
    }"#;

    #[test]
    fn the_documented_layout_is_the_serde_shape() {
        let profile: Profile = serde_json::from_str(EXAMPLE).unwrap();
        assert_eq!(profile.format, PROFILE_FORMAT);
        assert_eq!(profile.version, PROFILE_VERSION);
        assert_eq!(profile.name, "floaty");
        assert_eq!(profile.base, BASE_CLASSIC);
        assert_eq!(profile.overrides.len(), 2);
        assert_eq!(
            profile.overrides.get("movement.custom_gravity_scaling"),
            Some(&TuneValue::Float(0.5))
        );
        assert_eq!(
            profile.overrides.get("pawn.zoom_enabled"),
            Some(&TuneValue::Bool(false))
        );
        assert_eq!(profile.rules.grapples, GrappleRule::Unlimited);
        assert_eq!(profile.rules.rocket_boots, Switch::On);
        assert!(!profile.rules.auto_refill);
        assert_eq!(profile.time_scale, None);
        assert!(!profile.is_pristine());

        let json = serde_json::to_string(&profile).unwrap();
        assert_eq!(serde_json::from_str::<Profile>(&json).unwrap(), profile);
    }

    #[test]
    fn base_is_required_and_unknown_fields_are_refused() {
        let no_base = EXAMPLE.replace(r#""base": "asamu_original","#, "");
        assert!(serde_json::from_str::<Profile>(&no_base).is_err());
        let extra = EXAMPLE.replace(r#""version": 1,"#, r#""version": 1, "surprise": true,"#);
        assert!(serde_json::from_str::<Profile>(&extra).is_err());
        let bad_rule = EXAMPLE.replace(r#""auto_refill": false"#, r#""auto_refil": false"#);
        assert!(serde_json::from_str::<Profile>(&bad_rule).is_err());
    }

    #[test]
    fn the_classic_profile_is_pristine_and_named_safely() {
        let classic = Profile::classic();
        assert!(classic.is_pristine());
        assert!(valid_profile_name(&classic.name));
        assert_eq!(Profile::builtin().first(), Some(&classic));
    }

    #[test]
    fn profile_names_are_file_safe() {
        for good in ["a", "floaty", "super-jump", "run_2", &"x".repeat(40)] {
            assert!(valid_profile_name(good), "{good}");
        }
        for bad in [
            "",
            "Floaty",
            "has space",
            "dot.json",
            "../up",
            "a/b",
            "\u{e9}",
            &"x".repeat(41),
        ] {
            assert!(!valid_profile_name(bad), "{bad}");
        }
    }

    #[test]
    fn sandbox_directories_stay_under_their_root() {
        let dirs = SandboxDirs {
            root: PathBuf::from("data").join("sandbox"),
        };
        for dir in [dirs.profiles(), dirs.recordings(), dirs.dumps()] {
            assert!(dir.starts_with(&dirs.root), "{}", dir.display());
            assert_ne!(dir, dirs.root);
        }
    }
}
