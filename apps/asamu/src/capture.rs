//! Unattended runs: take a screenshot and/or exit after a delay
//! (`--screenshot PATH`, `--exit-after SECONDS`). Used to check locally that
//! a converted level renders; screenshots of game content stay local and are
//! never committed.
//!
//! With a converted level the screenshot waits until its assets have settled
//! (loaded or failed), then one more second (or `--screenshot-delay`
//! seconds), but never longer than the `--exit-after` deadline (default 60 s
//! when only a screenshot is asked for).
//!
//! The screenshot is rendered by a second camera into an **offscreen image**
//! (1600 × 900) that follows the player camera, not read back from the
//! window: macOS stops presenting to a window that is occluded or on a locked
//! screen, and a window read-back then comes out black. The HUD is not in the
//! image (gizmos, such as the grapple aim marker, are).
//!
//! A screenshot of a converted level is copyrighted game content, so the
//! path is checked before the window opens ([`check_screenshot_path`]): never
//! inside this repository (except its git-ignored `research/` folders), never
//! inside a game install, never through an existing symlink.

use std::path::{Path, PathBuf};

use bevy::camera::RenderTarget;
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};

use crate::PlayerCamera;
use crate::converted::ConvertedLevel;

/// Size of the offscreen screenshot image.
const SHOT_SIZE: (u32, u32) = (1600, 900);

/// What to do unattended.
#[derive(Resource, Debug, Clone, Default)]
pub struct AutoCapture {
    /// Screenshot file.
    pub screenshot: Option<PathBuf>,
    /// Exit after this many seconds.
    pub exit_after: Option<f32>,
    /// Seconds after the assets settled before the screenshot (default 1).
    pub delay: f32,
    taken_at: Option<f32>,
    settled_at: Option<f32>,
    target: Option<Handle<Image>>,
}

impl AutoCapture {
    /// A capture request.
    #[must_use]
    pub fn new(screenshot: Option<PathBuf>, exit_after: Option<f32>) -> Self {
        Self {
            screenshot,
            exit_after,
            delay: 1.0,
            taken_at: None,
            settled_at: None,
            target: None,
        }
    }

    /// Seconds after the assets settled before the screenshot.
    #[must_use]
    pub fn with_delay(mut self, delay: f32) -> Self {
        if delay.is_finite() && delay >= 0.0 {
            self.delay = delay;
        }
        self
    }

    fn active(&self) -> bool {
        self.screenshot.is_some() || self.exit_after.is_some()
    }
}

/// `research/` subdirectories the repository's `.gitignore` ignores: the
/// only places inside the repository where a screenshot may be written.
const IGNORED_RESEARCH_DIRS: &[&str] = &[
    "local",
    "raw",
    "ghidra",
    "decompiled",
    "extracted",
    "symbol-dumps",
    "decompressed",
    "strings",
];

/// Image formats the screenshot writer can encode, by extension.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "bmp", "tga"];

/// This repository's root as known at compile time (`apps/asamu/../..`).
fn compile_time_repo_root() -> Option<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent()?.parent()?.canonicalize().ok()
}

/// True if `dir` looks like the root of this repository.
fn is_repo_root(dir: &Path) -> bool {
    dir.join("Cargo.toml").is_file()
        && dir
            .join("crates")
            .join("asamu-ue3")
            .join("Cargo.toml")
            .is_file()
}

/// Validates a `--screenshot` path and returns it resolved (its parent
/// canonicalized, so symlinked or `..` parents are checked where they really
/// lead).
///
/// Refused: a path without a file name or with an extension the writer
/// cannot encode, a missing parent directory, anything inside an `.app`
/// bundle, a `steamapps` tree or `install_root` (the original install, when
/// known; compared without regard to case), anything inside this repository
/// except under a git-ignored `research/` subdirectory, and an existing
/// target that is not a regular file (a symlink could redirect the write).
/// An existing regular file is overwritten.
///
/// # Errors
/// A message saying why the path was refused.
pub fn check_screenshot_path(path: &Path, install_root: Option<&Path>) -> Result<PathBuf, String> {
    let shown = path.display();
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("--screenshot {shown}: no file name"))?;
    let extension = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !IMAGE_EXTENSIONS.contains(&extension.as_str()) {
        return Err(format!(
            "--screenshot {shown}: use one of the extensions {}",
            IMAGE_EXTENSIONS.join(", ")
        ));
    }
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let parent = parent.canonicalize().map_err(|e| {
        format!(
            "--screenshot {shown}: directory {} is not usable ({e}); create it first",
            parent.display()
        )
    })?;
    let target = parent.join(file_name);
    for ancestor in parent.ancestors() {
        let name = ancestor
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if name.ends_with(".app") || name == "steamapps" {
            return Err(format!(
                "--screenshot {shown}: refusing to write inside {} (looks like a game install)",
                ancestor.display()
            ));
        }
    }
    if let Some(install) = install_root.and_then(|r| r.canonicalize().ok()) {
        let lower = |p: &Path| p.to_string_lossy().to_lowercase();
        let (t, i) = (lower(&target), lower(&install));
        if t == i || t.starts_with(&format!("{i}{}", std::path::MAIN_SEPARATOR)) {
            return Err(format!(
                "--screenshot {shown}: refusing to write inside the original install"
            ));
        }
    }
    let mut roots: Vec<PathBuf> = compile_time_repo_root().into_iter().collect();
    roots.extend(
        parent
            .ancestors()
            .filter(|a| is_repo_root(a))
            .map(Path::to_path_buf),
    );
    for root in &roots {
        let allowed = IGNORED_RESEARCH_DIRS
            .iter()
            .any(|d| target.starts_with(root.join("research").join(d)));
        if target.starts_with(root) && !allowed {
            return Err(format!(
                "--screenshot {shown}: refusing to write game content into the repository ({}); \
                 use a path outside it or under research/local/",
                root.display()
            ));
        }
    }
    match std::fs::symlink_metadata(&target) {
        Ok(m) if !m.file_type().is_file() => Err(format!(
            "--screenshot {shown}: exists and is not a regular file (symlinks are refused)"
        )),
        Ok(_) => Ok(target),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(target),
        Err(e) => Err(format!("--screenshot {shown}: {e}")),
    }
}

/// How long Bevy's teardown may take after an exit request before the
/// watchdog ends the process (an app setting, not a game value).
const TEARDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// Arms a watchdog once the app asks to exit.
///
/// Observed on macOS (Bevy 0.20, pipelined rendering; about one exit in
/// seven): the render thread drops the last reference to the window, which
/// must happen on the main thread, while the main thread waits for the
/// render thread during teardown, so the process never ends and unattended
/// runs (`--exit-after`, scripts) hang. Everything the run had to do is done
/// by then (screenshots are saved before the exit request), so after
/// [`TEARDOWN_GRACE`] the watchdog ends the process with the requested exit
/// code. A normal teardown finishes first and the watchdog never fires.
fn arm_exit_watchdog(mut exits: MessageReader<AppExit>, mut armed: Local<bool>) {
    let Some(exit) = exits.read().last() else {
        return;
    };
    if *armed {
        return;
    }
    *armed = true;
    let code = match exit {
        AppExit::Success => 0,
        AppExit::Error(code) => i32::from(code.get()),
    };
    let spawned = std::thread::Builder::new()
        .name("asamu-exit-watchdog".to_owned())
        .spawn(move || {
            std::thread::sleep(TEARDOWN_GRACE);
            eprintln!(
                "asamu: shutdown did not finish within {} s (renderer teardown stuck); exiting",
                TEARDOWN_GRACE.as_secs()
            );
            std::process::exit(code);
        });
    if let Err(e) = spawned {
        warn!("could not start the exit watchdog: {e}");
    }
}

/// Screenshot / exit automation.
pub struct CapturePlugin;

impl Plugin for CapturePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AutoCapture>()
            .add_systems(PostStartup, setup_offscreen)
            .add_systems(Update, auto_capture)
            .add_systems(Last, arm_exit_watchdog);
    }
}

/// Adds the offscreen screenshot camera as a child of the player camera.
fn setup_offscreen(
    mut commands: Commands,
    mut capture: ResMut<AutoCapture>,
    mut images: ResMut<Assets<Image>>,
    camera: Query<Entity, With<PlayerCamera>>,
) {
    if capture.screenshot.is_none() {
        return;
    }
    let Some(parent) = camera.iter().next() else {
        warn!("no player camera: the screenshot will be read back from the window");
        return;
    };
    let image = images.add(Image::new_target_texture(
        SHOT_SIZE.0,
        SHOT_SIZE.1,
        TextureFormat::Bgra8UnormSrgb,
        None,
    ));
    let aspect = SHOT_SIZE.0 as f32 / SHOT_SIZE.1 as f32;
    commands.entity(parent).with_child((
        Camera3d::default(),
        Camera {
            order: -1,
            ..default()
        },
        RenderTarget::Image(image.clone().into()),
        Projection::from(PerspectiveProjection {
            // 90 degrees horizontal (the original's default FOV).
            fov: 2.0 * ((90.0_f32.to_radians() * 0.5).tan() / aspect).atan(),
            near: 0.05,
            far: 4000.0,
            ..default()
        }),
        Transform::IDENTITY,
    ));
    capture.target = Some(image);
}

fn auto_capture(
    mut commands: Commands,
    time: Res<Time<Real>>,
    mut capture: ResMut<AutoCapture>,
    level: Option<Res<ConvertedLevel>>,
    diagnostics: Res<bevy::diagnostic::DiagnosticsStore>,
    mut exit: MessageWriter<AppExit>,
) {
    if !capture.active() {
        return;
    }
    let now = time.elapsed_secs();
    let deadline = capture
        .exit_after
        .unwrap_or(if capture.screenshot.is_some() {
            60.0
        } else {
            f32::MAX
        });
    let settled = level.as_ref().is_none_or(|l| l.assets_settled());
    if settled && capture.settled_at.is_none() {
        capture.settled_at = Some(now);
    }
    if let Some(path) = capture.screenshot.clone()
        && capture.taken_at.is_none()
    {
        // Ready: assets settled `delay` seconds ago (or two seconds into a
        // run without a converted level), or the deadline is two seconds
        // away.
        let delay = capture.delay;
        let ready = capture
            .settled_at
            .is_some_and(|t| now >= t + delay && now >= 2.0)
            || now >= deadline - 2.0;
        if ready {
            let fps =
                crate::hud::fps(&diagnostics).map_or_else(|| "-".to_owned(), |f| format!("{f:.0}"));
            info!("taking screenshot {} ({fps} fps)", path.display());
            let shot = match &capture.target {
                Some(image) => Screenshot::image(image.clone()),
                None => Screenshot::primary_window(),
            };
            commands.spawn(shot).observe(save_to_disk(path));
            capture.taken_at = Some(now);
        }
    }
    let screenshot_done =
        capture.screenshot.is_none() || capture.taken_at.is_some_and(|t| now >= t + 1.5);
    let exit_now = match capture.exit_after {
        Some(limit) => now >= limit && screenshot_done,
        None => screenshot_done,
    };
    if exit_now || now >= deadline + 5.0 {
        info!("exiting after {now:.1} s (unattended run)");
        exit.write(AppExit::Success);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh scratch directory under the system temp dir (no extra
    /// crates), removed by [`Scratch::drop`].
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let dir = std::env::temp_dir().join(format!(
                "asamu-capture-test-{tag}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir.canonicalize().unwrap())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn accepts_plain_local_paths() {
        let s = Scratch::new("ok");
        let p = s.0.join("shot.png");
        assert_eq!(check_screenshot_path(&p, None).unwrap(), p);
        // An existing regular file is overwritten.
        std::fs::write(&p, b"old").unwrap();
        assert!(check_screenshot_path(&p, None).is_ok());
        assert!(check_screenshot_path(&s.0.join("shot.JPG"), None).is_ok());
    }

    #[test]
    fn refuses_bad_names_and_missing_directories() {
        let s = Scratch::new("names");
        for bad in ["shot", "shot.txt", "shot.png.gltf"] {
            assert!(
                check_screenshot_path(&s.0.join(bad), None).is_err(),
                "{bad}"
            );
        }
        assert!(check_screenshot_path(&s.0.join("missing").join("shot.png"), None).is_err());
        assert!(check_screenshot_path(Path::new("/"), None).is_err());
    }

    #[test]
    fn refuses_install_like_trees() {
        let s = Scratch::new("install");
        for dir in ["Game.app/Contents", "STEAMAPPS/common/Game", "x.APP"] {
            let d = s.0.join(dir);
            std::fs::create_dir_all(&d).unwrap();
            assert!(
                check_screenshot_path(&d.join("shot.png"), None).is_err(),
                "{dir}"
            );
        }
        // An install outside Steam (no .app, no steamapps), given explicitly;
        // compared without regard to case.
        let install = s.0.join("Install");
        std::fs::create_dir_all(install.join("Binaries")).unwrap();
        let inside = install.join("Binaries").join("shot.png");
        assert!(check_screenshot_path(&inside, Some(&install)).is_err());
        let upper = s.0.join("INSTALL");
        if upper.is_dir() {
            // Case-insensitive file system: the same directory.
            assert!(check_screenshot_path(&inside, Some(&upper)).is_err());
        }
        assert!(check_screenshot_path(&s.0.join("shot.png"), Some(&install)).is_ok());
        assert!(check_screenshot_path(&s.0.join("Install2.png"), Some(&install)).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinks() {
        let s = Scratch::new("links");
        let victim = s.0.join("victim.png");
        std::fs::write(&victim, b"keep").unwrap();
        let link = s.0.join("link.png");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        assert!(check_screenshot_path(&link, None).is_err());
        // A dangling link too.
        let dangling = s.0.join("dangling.png");
        std::os::unix::fs::symlink(s.0.join("nowhere.png"), &dangling).unwrap();
        assert!(check_screenshot_path(&dangling, None).is_err());
        // A symlinked parent directory is resolved before the checks.
        let app = s.0.join("Game.app");
        std::fs::create_dir_all(&app).unwrap();
        let alias = s.0.join("alias");
        std::os::unix::fs::symlink(&app, &alias).unwrap();
        assert!(check_screenshot_path(&alias.join("shot.png"), None).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
    }

    #[test]
    fn refuses_the_repository_outside_ignored_research() {
        let Some(root) = compile_time_repo_root() else {
            return;
        };
        assert!(check_screenshot_path(&root.join("shot.png"), None).is_err());
        assert!(check_screenshot_path(&root.join("docs").join("shot.png"), None).is_err());
        let research = root.join("research");
        if research.is_dir() {
            assert!(check_screenshot_path(&research.join("shot.png"), None).is_err());
            let local = research.join("local");
            if local.is_dir() {
                let name = "asamu-capture-test-nonexistent.png";
                assert!(check_screenshot_path(&local.join(name), None).is_ok());
            }
        }
        // `..` is resolved before the check.
        let sneaky = root
            .join("research")
            .join("local")
            .join("..")
            .join("shot.png");
        if root.join("research").join("local").is_dir() {
            assert!(check_screenshot_path(&sneaky, None).is_err());
        }
    }
}
