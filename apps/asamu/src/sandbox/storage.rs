//! The Sandbox's files: user profiles and recordings, all under the
//! Sandbox's own directories (`Lab::dirs`), never among the saves.
//!
//! Layout, under the user data root (`SaveStore::default_root()`, which
//! `ASAMU_SAVE_DIR` overrides):
//!
//! ```text
//! <root>/sandbox/profiles/<name>.json            tuning profiles
//! <root>/sandbox/recordings/<name>.sbxrec.jsonl  Sandbox recordings
//! ```
//!
//! Nothing here writes to `<root>/saves/`, to `settings.json`, to
//! `$ASAMU_TRACE_DIR`, into the converted data or into the repository, and
//! nothing is created until something is saved. Every write goes through
//! [`sandbox_path`], which refuses a target outside the Sandbox's root.
//!
//! This module holds plain functions: it registers no system.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use asamu_sandbox::profile::{Profile, ProfileStore, SandboxDirs, valid_profile_name};
use asamu_sandbox::recording::{RECORDING_SUFFIX, SandboxRecording};
use bevy::prelude::*;

use super::Lab;

/// Most numbered variants of one recording name that are tried before the
/// write is given up (two recordings only share a name when they end on the
/// same tick of the same level).
const MAX_SAME_NAME: u32 = 999;

pub(super) fn build(app: &mut App) {
    // Plain functions only; nothing to register.
    let _ = app;
}

/// Names of the profiles a session can load: the built-in presets (Classic
/// first), then the user's saved ones, sorted. A saved profile that carries
/// a built-in's name is not listed: the built-in wins (see [`load_profile`]).
pub(crate) fn profile_names(lab: &Lab) -> Vec<String> {
    let mut names: Vec<String> = Profile::builtin().into_iter().map(|p| p.name).collect();
    if let Some(dirs) = &lab.dirs {
        for name in ProfileStore::new(dirs.profiles()).list() {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

/// `true` when `name` is one of the built-in presets.
pub(super) fn is_builtin_profile(name: &str) -> bool {
    Profile::builtin().iter().any(|p| p.name == name)
}

/// The profile named `name`: a built-in preset, else the user's saved
/// profile of that name.
///
/// # Errors
/// A readable sentence: no such profile, or a saved file that does not load.
pub(crate) fn load_profile(lab: &Lab, name: &str) -> Result<Profile, String> {
    if let Some(profile) = Profile::builtin().into_iter().find(|p| p.name == name) {
        return Ok(profile);
    }
    if !valid_profile_name(name) {
        return Err(format!(
            "{name:?} is not a profile name (1 to 40 characters of a-z, 0-9, _ and -)"
        ));
    }
    let Some(dirs) = &lab.dirs else {
        return Err(format!(
            "there is no profile {name:?} (and no user data directory to look for saved ones)"
        ));
    };
    ProfileStore::new(dirs.profiles())
        .load(name)
        .map_err(|e| format!("profile {name:?}: {e}"))
}

/// Saves `profile` among the user's profiles and returns the file.
///
/// # Errors
/// The name is a built-in's (it could never be loaded again) or not a
/// profile name, there is no user data directory, or the write failed.
pub(super) fn save_profile(lab: &Lab, profile: &Profile) -> Result<PathBuf, String> {
    if is_builtin_profile(&profile.name) {
        return Err(format!(
            "{:?} is a built-in profile: save under another name",
            profile.name
        ));
    }
    let Some(dirs) = &lab.dirs else {
        return Err("there is no user data directory: nothing can be saved".to_owned());
    };
    let dir = sandbox_path(dirs, &dirs.profiles())?;
    ProfileStore::new(dir)
        .save(profile)
        .map_err(|e| format!("profile {:?}: {e}", profile.name))
}

/// Writes `recording` into the Sandbox's recordings directory and returns
/// the file. A file is never replaced: when the name is taken a number is
/// added.
///
/// # Errors
/// There is no user data directory, or the write failed (nothing is left
/// behind then).
pub(super) fn write_recording(
    dirs: Option<&SandboxDirs>,
    recording: &SandboxRecording,
) -> Result<PathBuf, String> {
    let Some(dirs) = dirs else {
        return Err("there is no user data directory: the recording was not saved".to_owned());
    };
    let dir = sandbox_path(dirs, &dirs.recordings())?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    let name = recording.file_name();
    let stem = name.strip_suffix(RECORDING_SUFFIX).unwrap_or(&name);
    // Written under a temporary name and moved into place, so a reader never
    // sees half a recording.
    let temporary = dir.join(format!(".{stem}.{}.tmp", std::process::id()));
    // The temporary name must be free: what an earlier run left there goes
    // first (the entry itself: a link is removed, not followed), and the
    // file is created new, so nothing is ever written through a link.
    let _ = std::fs::remove_file(&temporary);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|e| e.to_string())
        .and_then(|file| {
            let mut out = std::io::BufWriter::new(file);
            recording
                .write_jsonl(&mut out)
                .map_err(|e| e.to_string())
                .and_then(|()| out.flush().map_err(|e| e.to_string()))
        });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("{}: {e}", dir.join(&name).display()));
    }
    for n in 1..=MAX_SAME_NAME {
        let target = if n == 1 {
            dir.join(&name)
        } else {
            dir.join(format!("{stem}-{n}{RECORDING_SUFFIX}"))
        };
        // `hard_link` fails when the target exists, so an earlier recording
        // is never replaced (a plain rename would replace it).
        match std::fs::hard_link(&temporary, &target) {
            Ok(()) => {
                let _ = std::fs::remove_file(&temporary);
                return Ok(target);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) if !target.exists() => {
                // No hard links here (some file systems): move it instead.
                return match std::fs::rename(&temporary, &target) {
                    Ok(()) => Ok(target),
                    Err(e) => {
                        let _ = std::fs::remove_file(&temporary);
                        Err(format!("{}: {e}", target.display()))
                    }
                };
            }
            Err(_) => {}
        }
    }
    let _ = std::fs::remove_file(&temporary);
    Err(format!(
        "{}: more than {MAX_SAME_NAME} recordings with this name",
        dir.join(&name).display()
    ))
}

/// `path` when it lies inside the Sandbox's own root, as every file the
/// Sandbox writes must.
///
/// # Errors
/// `path` is outside `dirs.root` (or climbs out of it with `..`).
pub(super) fn sandbox_path(dirs: &SandboxDirs, path: &Path) -> Result<PathBuf, String> {
    let inside = path.starts_with(&dirs.root)
        && !path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
    if inside {
        Ok(path.to_path_buf())
    } else {
        Err(format!(
            "{} is outside the Sandbox's directory {}",
            path.display(),
            dirs.root.display()
        ))
    }
}

#[cfg(test)]
pub(super) mod testing {
    //! Temporary user data roots for the Sandbox's tests.

    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use asamu_sandbox::profile::SandboxDirs;

    /// A fresh, empty directory under the system's temporary directory,
    /// removed again when dropped.
    pub(in crate::sandbox) struct TempRoot(pub PathBuf);

    impl TempRoot {
        pub(in crate::sandbox) fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "asamu-sandbox-app-{tag}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
            Self(dir)
        }

        /// The Sandbox's directories under this root.
        pub(in crate::sandbox) fn dirs(&self) -> SandboxDirs {
            SandboxDirs {
                root: self.0.join("sandbox"),
            }
        }

        /// Where the saves of this root would live.
        pub(in crate::sandbox) fn saves(&self) -> PathBuf {
            self.0.join("saves")
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Every file under `dir` (relative path) with its bytes; a missing
    /// directory is empty.
    pub(in crate::sandbox) fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut out = BTreeMap::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(next) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&next) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let name = path
                        .strip_prefix(dir)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.insert(name, std::fs::read(&path).unwrap_or_default());
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use asamu_game::Game;
    use asamu_game::save::{SaveSession, SaveStore};
    use asamu_player::InputFrame;
    use asamu_sandbox::command::{Command, Toggle};
    use asamu_sandbox::keys::TuneValue;
    use asamu_sandbox::session::{Session, SimCx};

    use super::testing::{TempRoot, tree};
    use super::*;

    fn lab_at(root: &TempRoot) -> Lab {
        let mut lab = Lab::new(false);
        lab.dirs = Some(root.dirs());
        lab
    }

    /// A short Sandbox recording of a tuned session on the graybox.
    fn a_recording() -> SandboxRecording {
        let mut session = Session::classic();
        let mut game = Game::graybox().unwrap();
        game.start();
        let mut script = None;
        let mut run = |session: &mut Session, game: &mut Game, cmd: Command| {
            session
                .execute(
                    cmd,
                    &mut SimCx {
                        game,
                        script: &mut script,
                    },
                )
                .unwrap()
        };
        run(
            &mut session,
            &mut game,
            Command::SetParam {
                key: "movement.custom_gravity_scaling".to_owned(),
                value: TuneValue::Float(0.5),
            },
        );
        run(&mut session, &mut game, Command::Record { on: Toggle::On });
        for _ in 0..5 {
            game.tick(&InputFrame::default()).unwrap();
        }
        run(&mut session, &mut game, Command::Record { on: Toggle::Off })
            .recording
            .expect("stopping a recording returns it")
    }

    #[test]
    fn sandbox_profiles_list_builtins_first_and_load_by_name() {
        let root = TempRoot::new("profiles");
        let lab = lab_at(&root);
        let builtin: Vec<String> = Profile::builtin().into_iter().map(|p| p.name).collect();
        assert_eq!(builtin.first().map(String::as_str), Some("classic"));
        // Nothing saved yet: the built-ins, and no directory was created.
        assert_eq!(profile_names(&lab), builtin);
        assert!(!root.dirs().root.exists());

        let mut mine = Profile::classic();
        mine.name = "my-tune".to_owned();
        let path = save_profile(&lab, &mine).unwrap();
        assert!(path.starts_with(root.dirs().profiles()), "{path:?}");
        let mut expected = builtin.clone();
        expected.push("my-tune".to_owned());
        assert_eq!(profile_names(&lab), expected);
        assert_eq!(load_profile(&lab, "my-tune").unwrap(), mine);
        assert_eq!(load_profile(&lab, "moon").unwrap().name, "moon");

        // A built-in's name is refused (the built-in would shadow the file),
        // and so are names that are not file stems.
        let mut shadow = Profile::classic();
        shadow.name = "moon".to_owned();
        assert!(save_profile(&lab, &shadow).is_err());
        let mut bad = Profile::classic();
        bad.name = "../escape".to_owned();
        assert!(save_profile(&lab, &bad).is_err());
        assert!(load_profile(&lab, "../escape").is_err());
        assert!(load_profile(&lab, "nobody").is_err());
        assert_eq!(profile_names(&lab), expected);

        // Without a user data directory: the built-ins still load, nothing
        // can be saved.
        let mut homeless = Lab::new(false);
        homeless.dirs = None;
        assert_eq!(profile_names(&homeless), builtin);
        assert!(load_profile(&homeless, "heavy").is_ok());
        assert!(load_profile(&homeless, "my-tune").is_err());
        assert!(save_profile(&homeless, &mine).is_err());
    }

    #[test]
    fn sandbox_files_stay_out_of_the_saves() {
        let root = TempRoot::new("files");
        // Saves on disk next to the Sandbox's directory, as in a real user
        // data root.
        let mut saves = SaveSession::open(SaveStore::new(root.saves()));
        saves.new_game().unwrap();
        drop(saves);
        let before = tree(&root.saves());
        assert!(!before.is_empty(), "the save files exist");

        let lab = lab_at(&root);
        let recording = a_recording();
        let first = write_recording(lab.dirs.as_ref(), &recording).unwrap();
        let second = write_recording(lab.dirs.as_ref(), &recording).unwrap();
        let mut mine = Profile::classic();
        mine.name = "kept".to_owned();
        save_profile(&lab, &mine).unwrap();

        // The saves are byte-identical, and everything new is the Sandbox's.
        assert_eq!(tree(&root.saves()), before);
        let all = tree(&root.0);
        let new: Vec<&String> = all
            .keys()
            .filter(|name| !name.starts_with("saves/"))
            .collect();
        assert_eq!(new.len(), 3, "{new:?}");
        assert!(
            new.iter().all(|name| name.starts_with("sandbox/")),
            "{new:?}"
        );
        assert!(first.starts_with(root.dirs().recordings()));
        assert!(
            first
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("asamu-sandbox-") && n.ends_with(RECORDING_SUFFIX))
        );
        // The second write did not replace the first.
        assert_ne!(first, second);
        assert!(
            second
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&format!("-2{RECORDING_SUFFIX}")))
        );

        // The file is a Sandbox recording, and the parity reader refuses it.
        let text = std::fs::read_to_string(&first).unwrap();
        let read = SandboxRecording::read_jsonl(text.as_bytes()).unwrap();
        assert_eq!(read, recording);
        assert_eq!(read.header.param_set, "modified");
        assert!(asamu_player::Trace::from_jsonl_str(&text).is_err());

        // No user data directory: an error, not a file somewhere else.
        assert!(write_recording(None, &recording).is_err());
    }

    /// A link lying in wait under the temporary name a recording is written
    /// to first is removed, not written through.
    #[cfg(unix)]
    #[test]
    fn sandbox_recordings_are_not_written_through_a_link() {
        let root = TempRoot::new("link");
        let dirs = root.dirs();
        let recording = a_recording();
        let outside = root.0.join("outside.txt");
        std::fs::write(&outside, "not the Sandbox's").unwrap();
        std::fs::create_dir_all(dirs.recordings()).unwrap();
        let name = recording.file_name();
        let stem = name.strip_suffix(RECORDING_SUFFIX).unwrap();
        let trap = dirs
            .recordings()
            .join(format!(".{stem}.{}.tmp", std::process::id()));
        std::os::unix::fs::symlink(&outside, &trap).unwrap();
        // The final name taken by a link too: the recording gets the next
        // free name instead of following it.
        std::os::unix::fs::symlink(&outside, dirs.recordings().join(&name)).unwrap();

        let written = write_recording(Some(&dirs), &recording).unwrap();
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "not the Sandbox's"
        );
        assert!(std::fs::symlink_metadata(&trap).is_err());
        assert!(std::fs::symlink_metadata(&written).unwrap().is_file());
        assert_ne!(written, dirs.recordings().join(&name));
        let text = std::fs::read_to_string(&written).unwrap();
        assert_eq!(
            SandboxRecording::read_jsonl(text.as_bytes()).unwrap(),
            recording
        );
    }

    #[test]
    fn sandbox_paths_outside_the_root_are_refused() {
        let root = TempRoot::new("paths");
        let dirs = root.dirs();
        assert!(sandbox_path(&dirs, &dirs.profiles()).is_ok());
        assert!(sandbox_path(&dirs, &dirs.recordings().join("x.sbxrec.jsonl")).is_ok());
        assert!(sandbox_path(&dirs, &root.saves()).is_err());
        assert!(sandbox_path(&dirs, &root.0).is_err());
        assert!(sandbox_path(&dirs, &dirs.root.join("..").join("saves")).is_err());
    }
}
