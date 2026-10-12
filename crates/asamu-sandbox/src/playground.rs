//! The playground: the level the Sandbox opens into.
//!
//! One purpose-built level of ours, made for this game's movement. **It
//! contains no original content and no data derived from it**: every
//! position, size, shape, count and colour role in this file was chosen by
//! us, and every shape is generated here from a few numbers. The only
//! values taken from the game are Classic *parameter* values read from
//! [`PlayerParams::asamu_original`] when the level is built (the grapple's
//! reach, for the chain whose shots are at the edge of it). It is not a
//! level of the original and nothing about it is evidence of parity.
//!
//! # How it reaches the game
//!
//! The simulation's support for turning actors, attachments and
//! Matinee-moved platforms lives in the game layer's level script
//! ([`LevelScript`]), which is built by the loader of converted levels and
//! by nothing else. So the playground is **written in the converted-data
//! layout** (a scene file, a mesh manifest with simple collision bodies, a
//! level script and its Matinee data, all generated here through
//! [`asamu_world::fixtures`], the workspace's own writer for synthetic
//! data) into a fresh directory under the system's temporary directory,
//! loaded with the same public loader Classic uses
//! ([`load_level_with_kismet_options`]), and the directory is removed again.
//! Nothing of the simulation is forked or patched: the wheel turns because
//! its hub is placed in the rotating physics with a rate, the gondolas and
//! rocks ride because they are attached to it, the ferry moves because a
//! Matinee of ours moves it, and a grapple's anchor rides all of them by
//! the game's own rules.
//!
//! One consequence, which this crate corrects wherever it prints a level's
//! name ([`level_label`]): the loader names every level it builds
//! "... (converted locally from the original)" and gives it that origin.
//! The playground is not converted from anything. A constructor for a
//! hand-made scene level is what is missing in the game layer.
//!
//! # What is in it
//!
//! | Place | What it is for |
//! |---|---|
//! | Hub | A wide paved island with lamps: the start, the only checkpoint, where every fall ends |
//! | Near chain (ahead) | Islands a sprint jump or a short grapple apart, each one higher: the warm-up |
//! | Medium chain (right) | Islands well beyond a jump, at heights that go down and up: grapple, power jump, leap |
//! | Far chain (left, past the runway) | Shots at the edge of the grapple's reach, then a stretch of hooks without a landing, with recharge crystals where the budget runs out |
//! | Yard (behind) | A ferry of ours to ride there, a carousel of rocks and a big wheel with level gondolas to grapple and ride |
//! | Tower | Ledges a jump apart up a column, a board at the top, and a long drop into the red catch, which returns you to the hub |
//! | Runway | A long flat straight between two launch ramps |
//!
//! Three routes are timed ([`Course`], [`CourseClock`]): each has an open
//! start arch and a closed finish frame.
//!
//! Reading the level by sight ([`Look`]): pale paving is built ground,
//! green-topped rock is an island, teal rocks that hang in the air are
//! hooks, magenta is whatever moves, cyan is a recharge crystal, red is the
//! catch, white arches start a route and gold frames end one. The rules
//! behind the colours are the game's own: every solid surface takes the
//! grapple (nothing is tagged), and only the catch and the depths below
//! the level kill.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use asamu_game::{Game, LevelOptions, LevelScript, load_level_with_kismet_options};
use asamu_player::PlayerParams;
use asamu_world::fixtures::{MeshFixtures, Place, SceneFixture};
use asamu_world::scene::{self, DataSource, MemorySource};
use glam::Vec3;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::command::TeleportTarget;
use thiserror::Error;

/// Id of the playground, as the stage list and the tools name it.
pub const PLAYGROUND_ID: &str = "playground";
/// Package name of the playground's generated data. Ours; no map of the
/// original has it.
pub const PLAYGROUND_MAP: &str = "SandboxPlayground";
/// What the Sandbox calls the playground wherever it prints a level's name.
pub const PLAYGROUND_LEVEL_NAME: &str = "sandbox playground (hand-made, not original content)";
/// `WorldInfo.Title` of the generated scene.
const TITLE: &str = "Sandbox playground, hand-made by this project";
/// `KillZ` of the playground, UU (ours): far below everything.
const KILL_Z: f32 = -9000.0;
/// Largest file of the generated data that is written, bytes (a bound on
/// what [`write_files`] reads back from the in-memory source).
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Grapples the playground's own level script gives at level start: the
/// highest capacity a shipped map sets (GRAPPLE.md G-CT-3), the same value
/// the graybox's test configuration uses.
pub const PLAYGROUND_GRAPPLES: i32 = 3;

// ---------------------------------------------------------------------------
// What the view needs: shapes, pieces, looks.
// ---------------------------------------------------------------------------

/// A closed convex polyhedron in its own space, UU (UE axes: X forward, Y
/// right, Z up).
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    /// Corner points.
    pub vertices: Vec<[f32; 3]>,
    /// Triangles (indices into `vertices`; either winding).
    pub triangles: Vec<[u32; 3]>,
}

/// How a piece reads at a glance. The colours are the view's; the roles are
/// what the level means by them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Look {
    /// Built ground: the hub's paving, piers, bridges, the runway.
    Paving,
    /// An island to land on.
    Island,
    /// Bare rock: spires, the tower's column and ledges.
    Rock,
    /// A rock that hangs in the air to be grappled.
    Hook,
    /// A launch ramp.
    Ramp,
    /// Something that moves: the ferry, the carousel's rocks, the gondolas.
    Rider,
    /// The frame that carries moving things (hubs and spokes).
    Machine,
    /// A recharge crystal.
    Crystal,
    /// The catch under the long drop (it kills, and the hub is where you
    /// wake up).
    Hazard,
    /// The arch a timed route starts under.
    StartGate,
    /// The frame a timed route ends in.
    FinishGate,
    /// A lamp's light (no collision).
    Lamp,
    /// The light on top of a landmark (no collision), one colour per
    /// direction from the hub: ahead, right, left, behind, the tower.
    Beacon(u8),
    /// A mark set into the hub's floor (no collision) that points at a
    /// landmark, in that landmark's colour.
    Inlay(u8),
}

/// What a piece is to the simulation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Fixed, solid.
    Fixed,
    /// A hub the engine turns by itself (rotating physics, a rate).
    Turning,
    /// Attached to a turning hub and turning with it.
    Carried,
    /// Attached to a turning hub, keeping its own rotation (a gondola).
    Gondola,
    /// Moved by the playground's Matinee.
    Ferry,
    /// A recharge crystal.
    Crystal,
    /// Drawn only: no collision, not in the level's data.
    Decor,
}

/// One thing of the playground.
#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    /// Our name for it.
    pub name: String,
    /// Index into [`Playground::shapes`].
    pub shape: usize,
    /// Where the shape's origin is, UU.
    pub location: Vec3,
    /// Its rotation (pitch, yaw, roll), rotator units.
    pub rotation: [i32; 3],
    /// How it reads.
    pub look: Look,
    /// What it is to the simulation.
    pub role: Role,
    /// Its actor id in the loaded level (`None` for [`Role::Decor`]): what
    /// the level script reports a moved actor by.
    pub actor: Option<u32>,
}

/// A place worth going to: a teleport target of the tools, and a viewpoint
/// for unattended looks.
#[derive(Clone, Debug, PartialEq)]
pub struct Station {
    /// Short id (`hub`, `near`, `medium`, `far`, `yard`, `wheel`, `tower`,
    /// `runway`).
    pub id: &'static str,
    /// The floor point to stand on, UU.
    pub feet: Vec3,
    /// View yaw there, radians (UE convention).
    pub yaw: f32,
    /// View pitch there, radians.
    pub pitch: f32,
}

impl Station {
    /// The teleport that places a pawn here, looking the station's way.
    #[must_use]
    pub fn teleport(&self) -> TeleportTarget {
        TeleportTarget::Position {
            position: (self.feet + Vec3::Z * SPAWN_LIFT).to_array(),
            yaw: Some(self.yaw),
            pitch: Some(self.pitch),
        }
    }
}

/// An axis-aligned volume a timed route starts or ends in, UU.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gate {
    /// Minimum corner.
    pub min: Vec3,
    /// Maximum corner.
    pub max: Vec3,
}

impl Gate {
    /// Whether the point `p` is inside.
    #[must_use]
    pub fn contains(&self, p: Vec3) -> bool {
        p.cmpge(self.min).all() && p.cmple(self.max).all()
    }
}

/// A timed route.
#[derive(Clone, Debug, PartialEq)]
pub struct Course {
    /// Short id.
    pub id: &'static str,
    /// What the timer calls it.
    pub title: &'static str,
    /// Under the start arch.
    pub start: Gate,
    /// Inside the finish frame.
    pub finish: Gate,
}

/// The playground: what is drawn, what is timed, and its data.
#[derive(Clone, Debug)]
pub struct Playground {
    /// The distinct shapes.
    pub shapes: Vec<Shape>,
    /// Everything in it.
    pub pieces: Vec<Piece>,
    /// The timed routes.
    pub courses: Vec<Course>,
    /// The places worth going to (the first is the hub).
    pub stations: Vec<Station>,
    /// The generated data, as files of the converted-data layout (relative
    /// path, contents).
    files: Vec<(String, Vec<u8>)>,
}

impl Playground {
    /// The generated files (relative path with `/`, contents).
    #[must_use]
    pub fn files(&self) -> &[(String, Vec<u8>)] {
        &self.files
    }

    /// The station named `id`.
    #[must_use]
    pub fn station(&self, id: &str) -> Option<&Station> {
        self.stations.iter().find(|s| s.id == id)
    }

    /// The first piece named `name`.
    #[must_use]
    pub fn piece(&self, name: &str) -> Option<&Piece> {
        self.pieces.iter().find(|p| p.name == name)
    }
}

/// The playground, built once per process (it depends on nothing but the
/// Classic set, which is a constant of the build).
#[must_use]
pub fn playground() -> &'static Playground {
    static BUILT: OnceLock<Playground> = OnceLock::new();
    BUILT.get_or_init(build)
}

/// Whether `game` runs the playground.
#[must_use]
pub fn is_playground(game: &Game) -> bool {
    game.map_name() == Some(PLAYGROUND_MAP)
}

/// The name of `game`'s level as the Sandbox prints it: the level's own
/// name, except for the playground, which the loader of converted levels
/// labels as converted from the original and which is not (module docs).
#[must_use]
pub fn level_label(game: &Game) -> String {
    if is_playground(game) {
        PLAYGROUND_LEVEL_NAME.to_owned()
    } else {
        game.level().name.clone()
    }
}

/// The Sandbox's name for a level the game's loader named `name`: `Some`
/// for the playground (whatever a trace or a banner added around it).
#[must_use]
pub fn relabel(name: &str) -> Option<&'static str> {
    name.contains(TITLE).then_some(PLAYGROUND_LEVEL_NAME)
}

// ---------------------------------------------------------------------------
// Loading.
// ---------------------------------------------------------------------------

/// Why the playground did not load.
#[derive(Debug, Error)]
pub enum PlaygroundError {
    /// No fresh directory could be made for the generated data.
    #[error("no working directory for the playground's data under {dir}: {message}")]
    WorkDir {
        /// Where it was tried.
        dir: String,
        /// The system's reason.
        message: String,
    },
    /// A generated file could not be written.
    #[error("could not write {path}: {message}")]
    Write {
        /// The file.
        path: String,
        /// The system's reason.
        message: String,
    },
    /// The game's loader refused the generated data.
    #[error("the playground's data did not load: {0}")]
    Load(String),
    /// The level loaded without its script: nothing would turn or move.
    #[error("the playground loaded without its level script")]
    NoScript,
}

/// Writes `files` (relative paths with `/`) below `dir`, which must exist.
/// Refuses a path that would leave `dir`.
///
/// # Errors
/// An unsafe path, or the system refused a write.
pub fn write_files(dir: &Path, files: &[(String, Vec<u8>)]) -> Result<(), PlaygroundError> {
    for (relative, bytes) in files {
        let mut path = dir.to_path_buf();
        for part in relative.split('/') {
            let plain = !part.is_empty()
                && part != "."
                && part != ".."
                && !part.contains(['\\', ':', '\0']);
            if !plain {
                return Err(PlaygroundError::Write {
                    path: relative.clone(),
                    message: "not a plain relative path".to_owned(),
                });
            }
            path.push(part);
        }
        let failed = |e: std::io::Error| PlaygroundError::Write {
            path: relative.clone(),
            message: e.to_string(),
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(failed)?;
        }
        std::fs::write(&path, bytes).map_err(failed)?;
    }
    Ok(())
}

/// A directory that did not exist before, below `base`: made by this call
/// (so nothing that was already there, a link included, is ever written
/// through), with a name no other call of this process uses.
fn fresh_dir(base: &Path) -> Result<PathBuf, PlaygroundError> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let failed = |message: String| PlaygroundError::WorkDir {
        dir: base.display().to_string(),
        message,
    };
    let mut last = String::from("no attempt was made");
    for _ in 0..32 {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = base.join(format!(
            "asamu-sandbox-playground-{}-{n}",
            std::process::id()
        ));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) => last = e.to_string(),
        }
    }
    Err(failed(last))
}

/// Loads the playground: a started-from-scratch game on it with the Classic
/// parameter set, in the boot state, and its level script (see the module
/// docs for the route the data takes). The generated files exist only
/// during this call.
///
/// # Errors
/// The temporary directory cannot be used, or the loader refused the data
/// (which would be a defect of this module: the data is generated).
pub fn load_playground() -> Result<(Game, LevelScript), PlaygroundError> {
    load_playground_under(&std::env::temp_dir())
}

/// [`load_playground`] with the generated files written below `base`
/// instead of the system's temporary directory.
///
/// # Errors
/// As [`load_playground`].
pub fn load_playground_under(base: &Path) -> Result<(Game, LevelScript), PlaygroundError> {
    let dir = fresh_dir(base)?;
    let loaded = write_files(&dir, playground().files()).and_then(|()| {
        // No NPC data exists for a level of ours.
        let options = LevelOptions {
            npcs: false,
            time_trial: false,
        };
        load_level_with_kismet_options(&dir, PLAYGROUND_MAP, options)
            .map_err(|e| PlaygroundError::Load(e.to_string()))
    });
    // The game holds everything it needs: the files are ours to remove.
    let _ = std::fs::remove_dir_all(&dir);
    let (game, script) = loaded?;
    let script = script.ok_or(PlaygroundError::NoScript)?;
    Ok((game, script))
}

// ---------------------------------------------------------------------------
// Timed routes.
// ---------------------------------------------------------------------------

/// A finished run of a timed route.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CourseRun {
    /// Index into [`Playground::courses`].
    pub course: usize,
    /// Its time, seconds of simulated time.
    pub seconds: f32,
    /// It is the best run of this route in the session so far.
    pub best: bool,
}

/// The clock of the playground's timed routes.
///
/// It only watches: after every tick it is told where the pawn is. The
/// clock of a route starts on the last tick the pawn is under that route's
/// start arch (standing there does not run it) and stops on the first tick
/// the pawn is inside its finish frame. A death, a respawn, a teleport or a
/// loaded save state ends the running attempt without a time. Times are
/// simulated time (ticks of the game's clock), so a frozen or slowed
/// session does not change them; a session that changed the rules or the
/// parameters times what it changed, and the read-out is a measurement of
/// this recreation in this session, never of the original.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CourseClock {
    /// The running attempt: the route and the tick its clock started on.
    running: Option<(usize, u64)>,
    /// The latest finished run.
    last: Option<CourseRun>,
    /// Best time per route, seconds.
    best: BTreeMap<usize, f32>,
}

impl CourseClock {
    /// After a tick of `game`: `died` when the tick killed or respawned the
    /// pawn. Does nothing on another level than the playground.
    pub fn observe(&mut self, game: &Game, died: bool) {
        if !is_playground(game) {
            if self.running.is_some() {
                self.running = None;
            }
            return;
        }
        let clock = game.clock();
        self.step(
            &playground().courses,
            game.player().position,
            clock.tick(),
            clock.dt(),
            died,
        );
    }

    /// One step of the clock on `courses`: the pawn's centre is at
    /// `position` after the tick `tick` of `dt` seconds.
    pub fn step(&mut self, courses: &[Course], position: Vec3, tick: u64, dt: f32, died: bool) {
        if died || !position.is_finite() {
            self.running = None;
            return;
        }
        // Under a start arch: that route's clock is (re)started.
        if let Some(index) = courses.iter().position(|c| c.start.contains(position)) {
            self.running = Some((index, tick));
            return;
        }
        let Some((index, started)) = self.running else {
            return;
        };
        let Some(course) = courses.get(index) else {
            self.running = None;
            return;
        };
        if course.finish.contains(position) {
            let seconds = tick.saturating_sub(started) as f32 * dt;
            let best = self.best.get(&index).is_none_or(|b| seconds < *b);
            if best {
                self.best.insert(index, seconds);
            }
            self.last = Some(CourseRun {
                course: index,
                seconds,
                best,
            });
            self.running = None;
        }
    }

    /// Ends the running attempt without a time (a teleport, a loaded state).
    pub fn cancel(&mut self) {
        self.running = None;
    }

    /// The running attempt: the route, and its time so far at tick `tick`.
    #[must_use]
    pub fn running(&self, tick: u64, dt: f32) -> Option<(usize, f32)> {
        self.running
            .map(|(index, started)| (index, tick.saturating_sub(started) as f32 * dt))
    }

    /// The latest finished run.
    #[must_use]
    pub fn last(&self) -> Option<CourseRun> {
        self.last
    }

    /// The best time of route `course` in this session, seconds.
    #[must_use]
    pub fn best(&self, course: usize) -> Option<f32> {
        self.best.get(&course).copied()
    }

    /// One line for the screen (`None`: nothing to say yet): the running
    /// attempt's time, or the latest finished run, with the session's best.
    #[must_use]
    pub fn line(&self, game: &Game) -> Option<String> {
        if !is_playground(game) {
            return None;
        }
        let courses = &playground().courses;
        let clock = game.clock();
        let best_of = |index: usize| {
            self.best(index)
                .map_or(String::new(), |b| format!(" (best {b:.2} s)"))
        };
        if let Some((index, seconds)) = self.running(clock.tick(), clock.dt()) {
            // Still under the arch: the clock has not left yet.
            if seconds <= 0.0 {
                return courses
                    .get(index)
                    .map(|c| format!("{}: ready{}", c.title, best_of(index)));
            }
            return courses
                .get(index)
                .map(|c| format!("{}: {seconds:.2} s{}", c.title, best_of(index)));
        }
        let run = self.last?;
        let course = courses.get(run.course)?;
        Some(if run.best {
            format!("{}: {:.2} s, the best so far", course.title, run.seconds)
        } else {
            format!(
                "{}: {:.2} s{}",
                course.title,
                run.seconds,
                best_of(run.course)
            )
        })
    }
}

// ---------------------------------------------------------------------------
// Shapes.
// ---------------------------------------------------------------------------

fn v(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x, y, z)
}

/// A value in `[0, 1)` from two numbers (ours; only makes rocks differ).
fn noise(seed: u32, index: u32) -> f32 {
    let mut h = seed
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(index.wrapping_mul(0x85EB_CA6B))
        ^ 0x5BD1_E995;
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

impl Shape {
    /// A box centred on its origin with the half sizes `half`.
    fn block(half: Vec3) -> Self {
        let (vertices, triangles) = asamu_world::fixtures::box_mesh(-half, half);
        Self {
            vertices,
            triangles,
        }
    }

    /// A solid of rings: a polygon of `sides` corners (radii `rx`, `ry`,
    /// first corner at the angle `turn`) repeated at each `(z, scale)` of
    /// `profile`, top to bottom, the corners joined ring to ring. A scale
    /// of zero is a single point (a tip). `rough` is how much each corner's
    /// distance from the axis varies (the same for every ring, so the sides
    /// stay flat); with a profile whose scale is concave in `z` the solid
    /// is convex as long as `rough` is small against the polygon's corner
    /// angle.
    fn rings(
        sides: u32,
        rx: f32,
        ry: f32,
        turn: f32,
        rough: f32,
        seed: u32,
        profile: &[(f32, f32)],
    ) -> Self {
        let sides = sides.max(3);
        let mut vertices: Vec<[f32; 3]> = Vec::new();
        let mut triangles: Vec<[u32; 3]> = Vec::new();
        // Start index of each level and whether it is a ring.
        let mut levels: Vec<(u32, bool)> = Vec::new();
        for (z, scale) in profile {
            let start = vertices.len() as u32;
            if *scale > 0.0 {
                for i in 0..sides {
                    let angle = turn + std::f32::consts::TAU * i as f32 / sides as f32;
                    let factor = 1.0 + rough * (2.0 * noise(seed, i) - 1.0);
                    vertices.push([
                        factor * scale * rx * angle.cos(),
                        factor * scale * ry * angle.sin(),
                        *z,
                    ]);
                }
                levels.push((start, true));
            } else {
                vertices.push([0.0, 0.0, *z]);
                levels.push((start, false));
            }
        }
        let cap = |start: u32, triangles: &mut Vec<[u32; 3]>| {
            for i in 1..sides - 1 {
                triangles.push([start, start + i, start + i + 1]);
            }
        };
        if let Some((start, true)) = levels.first() {
            cap(*start, &mut triangles);
        }
        for pair in levels.windows(2) {
            let (a, a_ring) = pair[0];
            let (b, b_ring) = pair[1];
            for i in 0..sides {
                let j = (i + 1) % sides;
                match (a_ring, b_ring) {
                    (true, true) => {
                        triangles.push([a + i, a + j, b + j]);
                        triangles.push([a + i, b + j, b + i]);
                    }
                    (true, false) => triangles.push([a + i, a + j, b]),
                    (false, true) => triangles.push([a, b + i, b + j]),
                    (false, false) => {}
                }
            }
        }
        if levels.len() > 1
            && let Some((start, true)) = levels.last()
        {
            cap(*start, &mut triangles);
        }
        Self {
            vertices,
            triangles,
        }
    }

    /// A ramp: `length` along +X from its low edge at the origin, `width`
    /// across (centred), rising to `height` at the far end.
    fn ramp(length: f32, width: f32, height: f32) -> Self {
        let w = width * 0.5;
        Self {
            vertices: vec![
                [0.0, -w, 0.0],
                [length, -w, 0.0],
                [length, -w, height],
                [0.0, w, 0.0],
                [length, w, 0.0],
                [length, w, height],
            ],
            triangles: vec![
                [0, 1, 2],
                [3, 5, 4],
                [0, 3, 4],
                [0, 4, 1],
                [1, 4, 5],
                [1, 5, 2],
                [0, 2, 5],
                [0, 5, 3],
            ],
        }
    }

    /// The centre of the corner points.
    #[must_use]
    pub fn centre(&self) -> Vec3 {
        let sum: Vec3 = self.vertices.iter().map(|p| Vec3::from_array(*p)).sum();
        sum / self.vertices.len().max(1) as f32
    }

    /// How far outside one of its own faces a corner lies, UU: zero (up to
    /// rounding) for a convex solid.
    #[must_use]
    pub fn convexity_error(&self) -> f32 {
        let centre = self.centre();
        let point = |i: u32| {
            self.vertices
                .get(i as usize)
                .map_or(Vec3::ZERO, |p| Vec3::from_array(*p))
        };
        let mut worst = 0.0f32;
        for t in &self.triangles {
            let (a, b, c) = (point(t[0]), point(t[1]), point(t[2]));
            let Some(mut normal) = (b - a).cross(c - a).try_normalize() else {
                continue;
            };
            if normal.dot(a - centre) < 0.0 {
                normal = -normal;
            }
            for p in &self.vertices {
                worst = worst.max(normal.dot(Vec3::from_array(*p) - a));
            }
        }
        worst
    }

    /// Whether every edge is shared by exactly two triangles (a closed
    /// surface).
    #[must_use]
    pub fn is_closed(&self) -> bool {
        let mut edges: BTreeMap<(u32, u32), u32> = BTreeMap::new();
        for t in &self.triangles {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                if a == b {
                    return false;
                }
                *edges.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
        !edges.is_empty() && edges.values().all(|n| *n == 2)
    }
}

// ---------------------------------------------------------------------------
// The builder.
// ---------------------------------------------------------------------------

/// How a solid piece is placed in the level's data.
#[derive(Clone, Debug, PartialEq)]
enum Solid {
    Fixed,
    Turning {
        /// `RotationRate` (pitch, yaw, roll), rotator units per second.
        rate: [i32; 3],
    },
    Attached {
        /// Scene slot of the hub.
        hub: usize,
        /// Keeps its own rotation.
        level: bool,
    },
    Ferry,
    Crystal,
}

struct Builder {
    scene: SceneFixture,
    meshes: MeshFixtures,
    shapes: Vec<Shape>,
    keys: Vec<String>,
    pieces: Vec<Piece>,
    courses: Vec<Course>,
    stations: Vec<Station>,
    /// Scene slot and placed location of the ferry.
    ferry: Option<(usize, Vec3)>,
}

impl Builder {
    fn new() -> Self {
        Self {
            scene: SceneFixture::new(PLAYGROUND_MAP, KILL_Z).with_title(TITLE),
            meshes: MeshFixtures::new(),
            shapes: Vec::new(),
            keys: Vec::new(),
            pieces: Vec::new(),
            courses: Vec::new(),
            stations: Vec::new(),
            ferry: None,
        }
    }

    fn mesh_path(index: usize) -> String {
        format!("{PLAYGROUND_MAP}.Shape{index:03}")
    }

    /// The index of the shape `key` names, made by `make` the first time.
    fn shape(&mut self, key: String, make: impl FnOnce() -> Shape) -> usize {
        if let Some(index) = self.keys.iter().position(|k| *k == key) {
            return index;
        }
        let shape = make();
        let index = self.shapes.len();
        // The solid is its own simple body: the pawn collides with the same
        // corners the grapple's trace hits.
        let path = Self::mesh_path(index);
        self.meshes
            .add_convex(&path, &shape.vertices, &shape.triangles);
        self.meshes.add(
            &path,
            PLAYGROUND_MAP,
            shape.vertices.clone(),
            shape.triangles.clone(),
        );
        self.shapes.push(shape);
        self.keys.push(key);
        index
    }

    /// Adds a piece; a solid one also goes into the level's data.
    fn add(
        &mut self,
        name: &str,
        shape: usize,
        location: Vec3,
        rotation: [i32; 3],
        look: Look,
        solid: Option<Solid>,
    ) -> Option<usize> {
        let place = Place {
            location,
            rotation,
            scale: Vec3::ONE,
        };
        let mesh = Self::mesh_path(shape);
        let (role, slot) = match solid {
            None => (Role::Decor, None),
            Some(Solid::Fixed) => {
                let slot = self.scene.static_mesh(&mesh, place);
                (Role::Fixed, Some(slot))
            }
            Some(Solid::Crystal) => {
                let slot = self.scene.mesh_actor(
                    "asamu.ASAMURechargeCrystal",
                    "recharge_crystal",
                    &mesh,
                    place,
                    "COLLIDE_BlockAll",
                    None,
                    json!({}),
                );
                (Role::Crystal, Some(slot))
            }
            Some(moving) => {
                let slot = self.scene.mesh_actor(
                    "Engine.InterpActor",
                    "interp_actor",
                    &mesh,
                    place,
                    "COLLIDE_BlockAll",
                    None,
                    json!({}),
                );
                // What `mesh_actor` stored, plus what makes it move.
                let component =
                    format!("{PLAYGROUND_MAP}.InterpActor_{slot}.StaticMeshComponent_0");
                self.scene
                    .set_actor_value(slot, "collide_actors", json!(true));
                self.scene
                    .set_actor_value(slot, "block_actors", json!(true));
                let role = match moving {
                    Solid::Turning { rate } => {
                        self.scene
                            .set_actor_value(slot, "physics", json!("PHYS_Rotating"));
                        self.scene.set_actor_value(
                            slot,
                            "instance",
                            json!({
                                "CollisionComponent": component,
                                "RotationRate": {"Pitch": rate[0], "Yaw": rate[1], "Roll": rate[2]},
                            }),
                        );
                        Role::Turning
                    }
                    Solid::Attached { hub, level } => {
                        let base = self.scene.path(hub);
                        self.scene
                            .set_actor_value(slot, "physics", json!("PHYS_Interpolating"));
                        self.scene.set_actor_value(slot, "base", json!(base));
                        self.scene.set_actor_value(slot, "hard_attach", json!(true));
                        self.scene.set_actor_value(
                            slot,
                            "instance",
                            json!({
                                "CollisionComponent": component,
                                "bIgnoreBaseRotation": level,
                            }),
                        );
                        if level { Role::Gondola } else { Role::Carried }
                    }
                    Solid::Ferry | Solid::Fixed | Solid::Crystal => {
                        self.scene
                            .set_actor_value(slot, "physics", json!("PHYS_Interpolating"));
                        self.ferry = Some((slot, location));
                        Role::Ferry
                    }
                };
                (role, Some(slot))
            }
        };
        self.pieces.push(Piece {
            name: name.to_owned(),
            shape,
            location,
            rotation,
            look,
            role,
            actor: slot.and_then(|slot| scene::actor_id(0, slot)),
        });
        slot
    }

    /// A box from two corners.
    fn slab(&mut self, name: &str, min: Vec3, max: Vec3, look: Look, solid: Option<Solid>) {
        let half = (max - min) * 0.5;
        let shape = self.shape(format!("block {half:?}"), || Shape::block(half));
        self.add(name, shape, (min + max) * 0.5, [0; 3], look, solid);
    }

    /// An island whose flat top (radius about `radius`) is centred on `top`,
    /// a little narrower one way than the other.
    fn island(&mut self, name: &str, top: Vec3, radius: f32, look: Look, seed: u32) {
        let squash = 0.86 + 0.12 * noise(seed, 91);
        self.island_shaped(name, top, radius, squash, look, seed);
    }

    /// An island whose flat top is centred on `top`: `radius` along X and
    /// `radius * squash` along Y.
    fn island_shaped(
        &mut self,
        name: &str,
        top: Vec3,
        radius: f32,
        squash: f32,
        look: Look,
        seed: u32,
    ) {
        let sides = if radius > 1000.0 { 14 } else { 10 };
        let turn = noise(seed, 92) * std::f32::consts::TAU;
        let shape = self.shape(format!("island {radius} {squash} {seed}"), || {
            Shape::rings(
                sides,
                radius,
                radius * squash,
                turn,
                0.03,
                seed,
                &[
                    (0.0, 1.0),
                    (-0.18 * radius, 1.08),
                    (-0.9 * radius, 0.72),
                    (-1.7 * radius, 0.0),
                ],
            )
        });
        self.add(name, shape, top, [0; 3], look, Some(Solid::Fixed));
    }

    /// A rock hanging in the air, about `size` across, centred on `at`.
    fn hook(&mut self, name: &str, at: Vec3, size: f32, seed: u32) {
        let shape = self.hook_shape(size, seed);
        self.add(name, shape, at, [0; 3], Look::Hook, Some(Solid::Fixed));
    }

    fn hook_shape(&mut self, size: f32, seed: u32) -> usize {
        let turn = noise(seed, 93) * std::f32::consts::TAU;
        self.shape(format!("hook {size} {seed}"), || {
            Shape::rings(
                7,
                size * 0.5,
                size * (0.4 + 0.1 * noise(seed, 94)),
                turn,
                0.06,
                seed,
                &[
                    (0.45 * size, 0.0),
                    (0.12 * size, 0.8),
                    (-0.1 * size, 1.0),
                    (-0.6 * size, 0.0),
                ],
            )
        })
    }

    /// A standing rock of height `height` with its foot centred on `foot`.
    fn spire(&mut self, name: &str, foot: Vec3, radius: f32, height: f32, look: Look, seed: u32) {
        let turn = noise(seed, 95) * std::f32::consts::TAU;
        let shape = self.shape(format!("spire {radius} {height} {seed}"), || {
            Shape::rings(
                6,
                radius,
                radius * 0.85,
                turn,
                0.05,
                seed,
                &[(height, 0.0), (0.35 * height, 0.75), (0.0, 1.0)],
            )
        });
        self.add(name, shape, foot, [0; 3], look, Some(Solid::Fixed));
    }

    /// A small light without collision, centred on `at`.
    fn light(&mut self, name: &str, at: Vec3, size: f32, look: Look) {
        let shape = self.shape(format!("light {size}"), || {
            Shape::rings(
                4,
                size,
                size,
                0.0,
                0.0,
                0,
                &[(size * 1.4, 0.0), (0.0, 1.0), (-size * 1.4, 0.0)],
            )
        });
        self.add(name, shape, at, [0; 3], look, None);
    }

    /// A landmark: a tall rock on `foot` with a light of colour `beacon` on
    /// top.
    fn landmark(&mut self, name: &str, foot: Vec3, height: f32, beacon: u8, seed: u32) {
        self.spire(name, foot, height * 0.12, height, Look::Rock, seed);
        self.light(
            &format!("{name} light"),
            foot + Vec3::Z * (height + 110.0),
            90.0,
            Look::Beacon(beacon),
        );
    }

    /// A gate across the travel direction: `along_x` when the route runs
    /// along X (the posts stand apart in Y). `closed` adds the sill that
    /// makes a finish frame. Returns the volume between the posts.
    fn gate(&mut self, name: &str, at: Vec3, along_x: bool, width: f32, closed: bool) -> Gate {
        const POST: f32 = 34.0;
        const HEIGHT: f32 = 430.0;
        let look = if closed {
            Look::FinishGate
        } else {
            Look::StartGate
        };
        let across = if along_x { Vec3::Y } else { Vec3::X };
        let ahead = if along_x { Vec3::X } else { Vec3::Y };
        let half = width * 0.5;
        let post = across * POST + ahead * POST;
        for (side, label) in [(-1.0, "left"), (1.0, "right")] {
            let foot = at + across * (side * half);
            self.slab(
                &format!("{name} {label} post"),
                foot - post,
                foot + post + Vec3::Z * HEIGHT,
                look,
                Some(Solid::Fixed),
            );
        }
        let top = at + Vec3::Z * HEIGHT;
        self.slab(
            &format!("{name} lintel"),
            top - across * (half + POST) - ahead * POST,
            top + across * (half + POST) + ahead * POST + Vec3::Z * (POST * 2.0),
            look,
            Some(Solid::Fixed),
        );
        if closed {
            // A finish reads as a closed frame: a second bar under the
            // lintel, and a light on top.
            let bar = at + Vec3::Z * (HEIGHT - 110.0);
            self.slab(
                &format!("{name} bar"),
                bar - across * half - ahead * (POST * 0.5),
                bar + across * half + ahead * (POST * 0.5) + Vec3::Z * 30.0,
                look,
                None,
            );
            self.light(
                &format!("{name} light"),
                top + Vec3::Z * 170.0,
                60.0,
                Look::FinishGate,
            );
        }
        Gate {
            min: at - across * (half - POST) - ahead * 140.0,
            max: at + across * (half - POST) + ahead * 140.0 + Vec3::Z * HEIGHT,
        }
    }

    fn station(&mut self, id: &'static str, feet: Vec3, yaw_degrees: f32, pitch_degrees: f32) {
        self.stations.push(Station {
            id,
            feet,
            yaw: yaw_degrees.to_radians(),
            pitch: pitch_degrees.to_radians(),
        });
    }
}

// ---------------------------------------------------------------------------
// The level. Every number below is ours unless it says otherwise.
// ---------------------------------------------------------------------------

/// Rotator units in a full turn.
const TURN: i32 = 65_536;

/// Radius of the hub's paved top, UU.
pub const HUB_RADIUS: f32 = 1500.0;
/// Where the pawn starts and wakes up: the floor point, UU.
pub const HUB_START: Vec3 = Vec3::new(-300.0, 0.0, 0.0);
/// Height of the pawn's centre above the floor point it is placed on, UU
/// (ours: a little above the Classic collision half height, so the first
/// tick's floor check stands it up).
const SPAWN_LIFT: f32 = 50.0;

/// A landing of a chain: the centre of its flat top, UU, and about how far
/// the top reaches from it.
pub type Landing = (Vec3, f32);

/// How far above a landing's top its hook hangs, UU: a pull to the hook
/// ends under it, and the pawn drops onto the landing.
pub const HOOK_LIFT: f32 = 1000.0;

/// The near chain, ahead of the start: gaps a walking jump crosses, each
/// landing a little higher, then one gap for the leap and a last step for
/// the power jump.
pub const NEAR_CHAIN: [Landing; 5] = [
    (Vec3::new(2520.0, 0.0, 120.0), 500.0),
    (Vec3::new(3990.0, 300.0, 260.0), 480.0),
    (Vec3::new(5410.0, -120.0, 400.0), 480.0),
    (Vec3::new(8090.0, 210.0, 520.0), 520.0),
    (Vec3::new(9680.0, 0.0, 1400.0), 820.0),
];
/// The medium chain, to the right of the start: every gap is far beyond a
/// jump, and the landings go down and up.
pub const MEDIUM_CHAIN: [Landing; 5] = [
    (Vec3::new(0.0, 4800.0, 200.0), 580.0),
    (Vec3::new(700.0, 8400.0, -600.0), 580.0),
    (Vec3::new(-500.0, 12_000.0, 300.0), 580.0),
    (Vec3::new(300.0, 15_600.0, 1300.0), 580.0),
    (Vec3::new(0.0, 19_400.0, 700.0), 900.0),
];
/// Each long shot of the far chain as a fraction of the Classic grapple
/// range (ours): from the eye of a pawn standing where the route leaves the
/// ground to the hook it is meant to take.
pub const FAR_SHOT_FRACTION: f32 = 0.93;
/// Shots of that kind at the start of the far chain.
pub const FAR_SHOTS: u32 = 2;
/// What hangs in the far chain's stretch without a landing, in the order it
/// is taken: `false` a hook, `true` a recharge crystal. The playground's
/// budget is [`PLAYGROUND_GRAPPLES`], and each crystal is the grapple that
/// would use the last of it.
pub const FAR_AIR: [bool; 7] = [false, false, true, false, false, true, false];
/// Spacing of that stretch's hooks, UU.
pub const FAR_AIR_SPACING: f32 = 2400.0;
/// How far that stretch swings to either side of its line, UU: from under
/// one hook the next must not be hidden behind it.
pub const FAR_AIR_SWAY: f32 = 900.0;

/// Centre of the carousel's hub, UU.
pub const CAROUSEL_CENTRE: Vec3 = Vec3::new(-7000.0, -3600.0, 900.0);
/// Distance of the carousel's rocks from its axis, UU.
pub const CAROUSEL_RADIUS: f32 = 2000.0;
/// Rocks on the carousel.
pub const CAROUSEL_ROCKS: u32 = 6;
/// The carousel's rate, rotator units per second about the vertical: one
/// turn in 40 s.
pub const CAROUSEL_RATE: i32 = 1638;
/// Centre of the big wheel's hub, UU.
pub const WHEEL_CENTRE: Vec3 = Vec3::new(-7000.0, 3400.0, 2700.0);
/// Distance of the wheel's gondolas from its axis, UU.
pub const WHEEL_RADIUS: f32 = 2100.0;
/// Gondolas on the wheel.
pub const WHEEL_GONDOLAS: u32 = 6;
/// The wheel's rate, rotator units per second about its axle (roll): one
/// turn in 48 s.
pub const WHEEL_RATE: i32 = 1365;

/// Where the ferry rests at the hub: the centre of its deck, UU.
pub const FERRY_REST: Vec3 = Vec3::new(-2020.0, 0.0, 0.0);
/// How far the ferry travels to the yard, UU.
pub const FERRY_TRAVEL: Vec3 = Vec3::new(-3580.0, 0.0, 600.0);
/// The ferry's timetable, seconds: wait, cross, wait, cross back.
pub const FERRY_WAIT: f32 = 3.0;
/// See [`FERRY_WAIT`].
pub const FERRY_CROSSING: f32 = 9.0;
/// Centre of the yard's top, UU.
pub const YARD: Vec3 = Vec3::new(-7000.0, 0.0, 600.0);

/// Where the tower stands: the centre of its island's top, UU.
pub const TOWER_BASE: Vec3 = Vec3::new(2300.0, 2300.0, 0.0);
/// Heights of the tower's ledges above its island, UU: a jump apart.
pub const TOWER_LEDGES: [f32; 7] = [400.0, 800.0, 1200.0, 1600.0, 2000.0, 2400.0, 2800.0];
/// Top of the tower above its island, UU: a power jump above the last
/// ledge.
pub const TOWER_HEIGHT: f32 = 3600.0;
/// Where the tower's own hook hangs, from the centre of its island's top,
/// UU: beside the top, towards the hub, within the Classic reach of the
/// bridge below.
pub const TOWER_HOOK: Vec3 = Vec3::new(-330.0, -330.0, TOWER_HEIGHT + HOOK_LIFT);
/// How far above a carousel rock or a gondola its own hook rides, UU.
pub const RIDER_HOOK_LIFT: f32 = 850.0;
/// Centre of the catch under the tower's board, UU.
pub const CATCH_CENTRE: Vec3 = Vec3::new(5200.0, 2600.0, -2600.0);
/// Half the catch's width, UU.
pub const CATCH_HALF: f32 = 1900.0;

/// `y` of the runway's centre line, UU.
pub const RUNWAY_Y: f32 = -3200.0;
/// Where the far chain leaves the ground: the floor point at the runway's
/// edge, UU.
pub const FAR_EDGE: Vec3 = Vec3::new(-1500.0, RUNWAY_Y - 320.0, 0.0);

/// Where the hook of a landing hangs: [`HOOK_LIFT`] above its top, a little
/// past the centre as seen from `from`.
#[must_use]
pub fn hook_over(landing: Vec3, from: Vec3) -> Vec3 {
    let mut ahead = landing - from;
    ahead.z = 0.0;
    landing + ahead.normalize_or_zero() * 100.0 + Vec3::Z * HOOK_LIFT
}

fn build() -> Playground {
    let params = PlayerParams::asamu_original();
    // The Classic grapple range (the debug rope's range if the script layer
    // is ever absent: only so this stays total).
    let reach = params
        .gun
        .as_ref()
        .map_or(params.grapple.max_range.value, |gun| gun.max_distance.value);
    let eye = params.movement.capsule_half_height.value + params.camera.eye_height.value;

    let mut b = Builder::new();
    let fixed = || Some(Solid::Fixed);

    // --- The hub -----------------------------------------------------------
    b.island_shaped("hub", Vec3::ZERO, HUB_RADIUS, 1.0, Look::Paving, 1);
    for (i, (x, y)) in [(1.0, 1.0), (1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0)]
        .into_iter()
        .enumerate()
    {
        let foot = v(x * 800.0, y * 800.0, 0.0);
        b.slab(
            &format!("hub lamp {i}"),
            foot - v(22.0, 22.0, 0.0),
            foot + v(22.0, 22.0, 300.0),
            Look::Rock,
            fixed(),
        );
        b.light(
            &format!("hub lamp {i} light"),
            foot + Vec3::Z * 360.0,
            34.0,
            Look::Lamp,
        );
    }
    // The floor says where things are: a ring, and an arrow towards each
    // landmark in that landmark's colour (ahead, right, left, behind, and
    // the tower between ahead and right).
    for (name, radius, lift, look) in [
        ("hub ring", 640.0, 2.0, Look::Rock),
        ("hub disc", 560.0, 4.0, Look::Paving),
    ] {
        let shape = b.shape(format!("disc {radius}"), || {
            Shape::rings(28, radius, radius, 0.0, 0.0, 0, &[(lift, 1.0), (0.0, 1.0)])
        });
        b.add(name, shape, Vec3::ZERO, [0; 3], look, None);
    }
    let arrow = b.shape("arrow".to_owned(), || {
        Shape::rings(3, 190.0, 110.0, 0.0, 0.0, 0, &[(5.0, 1.0), (0.0, 1.0)])
    });
    for (beacon, degrees) in [(0u8, 0.0f32), (1, 90.0), (2, -90.0), (3, 180.0), (4, 45.0)] {
        let angle = degrees.to_radians();
        let yaw = (degrees / 360.0 * TURN as f32).round() as i32;
        b.add(
            &format!("hub arrow {beacon}"),
            arrow,
            v(1000.0 * angle.cos(), 1000.0 * angle.sin(), 0.0),
            [0, yaw, 0],
            Look::Inlay(beacon),
            None,
        );
    }
    b.scene.player_start(HUB_START + Vec3::Z * SPAWN_LIFT, 0);
    // The only checkpoint that can be activated: every death ends here.
    b.scene.checkpoint(
        Place::at(HUB_START + Vec3::Z * SPAWN_LIFT),
        HUB_RADIUS,
        400.0,
        0,
        json!({"bRotatePlayerToSpawnPointRot": false}),
    );
    b.station("hub", HUB_START, 0.0, 4.0);

    // --- Near chain, ahead (+X) ---------------------------------------------
    let near_start = b.gate("near start", v(1250.0, 0.0, 0.0), true, 520.0, false);
    let mut previous = v(HUB_RADIUS, 0.0, 0.0);
    for (i, (top, radius)) in NEAR_CHAIN.into_iter().enumerate() {
        b.island(
            &format!("near island {}", i + 1),
            top,
            radius,
            Look::Island,
            10 + i as u32,
        );
        b.hook(
            &format!("near hook {}", i + 1),
            hook_over(top, previous),
            230.0,
            20 + i as u32,
        );
        previous = top;
    }
    let lookout = NEAR_CHAIN[4].0;
    let near_finish = b.gate(
        "near finish",
        lookout + v(420.0, 0.0, 0.0),
        true,
        520.0,
        true,
    );
    b.landmark(
        "near landmark",
        lookout + v(-80.0, 560.0, 0.0),
        2000.0,
        0,
        30,
    );
    b.station("near", v(1150.0, 0.0, 0.0), 0.0, 8.0);
    b.courses.push(Course {
        id: "near",
        title: "Near chain",
        start: near_start,
        finish: near_finish,
    });

    // --- Medium chain, to the right (+Y) ------------------------------------
    let medium_start = b.gate("medium start", v(0.0, 1250.0, 0.0), false, 520.0, false);
    let mut previous = v(0.0, HUB_RADIUS, 0.0);
    for (i, (top, radius)) in MEDIUM_CHAIN.into_iter().enumerate() {
        b.island(
            &format!("medium island {}", i + 1),
            top,
            radius,
            Look::Island,
            40 + i as u32,
        );
        b.hook(
            &format!("medium hook {}", i + 1),
            hook_over(top, previous),
            260.0,
            50 + i as u32,
        );
        previous = top;
    }
    let medium_end = MEDIUM_CHAIN[4].0;
    let medium_finish = b.gate(
        "medium finish",
        medium_end + v(0.0, 470.0, 0.0),
        false,
        520.0,
        true,
    );
    b.landmark(
        "medium landmark",
        medium_end + v(620.0, -100.0, 0.0),
        2200.0,
        1,
        70,
    );
    b.station("medium", v(0.0, 1150.0, 0.0), 90.0, 10.0);
    b.courses.push(Course {
        id: "medium",
        title: "Medium chain",
        start: medium_start,
        finish: medium_finish,
    });

    // --- Runway and ramps, to the left (-Y) ---------------------------------
    b.slab(
        "runway bridge",
        v(-260.0, RUNWAY_Y + 340.0, -70.0),
        v(260.0, -1380.0, 0.0),
        Look::Paving,
        fixed(),
    );
    b.slab(
        "runway",
        v(-3600.0, RUNWAY_Y - 350.0, -90.0),
        v(7000.0, RUNWAY_Y + 350.0, 0.0),
        Look::Paving,
        fixed(),
    );
    // A ramp at each end: 800 long, rising 380 (25 degrees, walkable).
    let ramp = b.shape("ramp".to_owned(), || Shape::ramp(800.0, 600.0, 380.0));
    b.add(
        "ramp ahead",
        ramp,
        v(6200.0, RUNWAY_Y, 0.0),
        [0; 3],
        Look::Ramp,
        fixed(),
    );
    b.add(
        "ramp behind",
        ramp,
        v(-2800.0, RUNWAY_Y, 0.0),
        [0, TURN / 2, 0],
        Look::Ramp,
        fixed(),
    );
    b.island(
        "runway landing ahead",
        v(8700.0, RUNWAY_Y, -150.0),
        900.0,
        Look::Island,
        80,
    );
    b.island(
        "runway landing behind",
        v(-5300.0, RUNWAY_Y, -150.0),
        900.0,
        Look::Island,
        81,
    );
    let sprint_start = b.gate("sprint start", v(600.0, RUNWAY_Y, 0.0), true, 620.0, false);
    let sprint_finish = b.gate("sprint finish", v(5900.0, RUNWAY_Y, 0.0), true, 620.0, true);
    b.station("runway", v(0.0, RUNWAY_Y, 0.0), 0.0, 0.0);
    b.courses.push(Course {
        id: "sprint",
        title: "Runway sprint",
        start: sprint_start,
        finish: sprint_finish,
    });

    // --- Far chain, beyond the runway (-Y) ----------------------------------
    // Shots at the edge of the Classic reach, measured from the eye of a
    // pawn standing where the route leaves the ground; the landing of each
    // lies under its hook.
    let shot = FAR_SHOT_FRACTION * reach;
    let far_start = b.gate(
        "far start",
        FAR_EDGE + v(0.0, 100.0, 0.0),
        false,
        520.0,
        false,
    );
    let mut stand = FAR_EDGE;
    for i in 0..FAR_SHOTS {
        // The hook: `shot` from the eye, about 24 degrees up.
        let rise = 0.4 * shot;
        let run = (shot * shot - rise * rise).max(0.0).sqrt();
        let hook = stand + Vec3::Z * eye + v(0.0, -run, rise);
        b.hook(&format!("far hook {}", i + 1), hook, 300.0, 100 + i);
        let top = hook - v(0.0, 100.0, HOOK_LIFT);
        b.island(
            &format!("far island {}", i + 1),
            top,
            680.0,
            Look::Island,
            110 + i,
        );
        stand = top + v(0.0, -480.0, 0.0);
    }
    b.station("crystals", stand, -90.0, 14.0);
    // The stretch without a landing: hooks in a row, and a crystal wherever
    // the budget would run out.
    let crystal = b.shape("crystal".to_owned(), || {
        Shape::rings(
            6,
            90.0,
            90.0,
            0.0,
            0.0,
            0,
            &[(240.0, 0.0), (50.0, 1.0), (-50.0, 1.0), (-240.0, 0.0)],
        )
    });
    let air_from = stand + Vec3::Z * 900.0;
    let mut air_end = air_from;
    let mut hooks = 0u32;
    let mut sway = -FAR_AIR_SWAY;
    for (i, is_crystal) in FAR_AIR.into_iter().enumerate() {
        if is_crystal {
            // A crystal does not pull (the gun lets go of it at once): it
            // hangs just past the hook before it, towards the next one, to
            // be taken in passing.
            b.add(
                &format!("far crystal {}", i + 1),
                crystal,
                air_end + v(-sway * 0.6, -500.0, 420.0),
                [0; 3],
                Look::Crystal,
                Some(Solid::Crystal),
            );
        } else {
            hooks += 1;
            sway = -sway;
            let n = hooks as f32;
            air_end = air_from + v(sway, -n * FAR_AIR_SPACING, 150.0 * n);
            b.hook(
                &format!("far air hook {}", i + 1),
                air_end,
                280.0,
                120 + i as u32,
            );
        }
    }
    let far_end = air_end - v(0.0, 100.0, HOOK_LIFT);
    b.island("far end", far_end, 950.0, Look::Island, 130);
    let far_finish = b.gate(
        "far finish",
        far_end + v(0.0, -480.0, 0.0),
        false,
        560.0,
        true,
    );
    b.landmark(
        "far landmark",
        far_end + v(640.0, 100.0, 0.0),
        2400.0,
        2,
        131,
    );
    b.station("far", FAR_EDGE + v(0.0, 420.0, 0.0), -90.0, 18.0);
    b.courses.push(Course {
        id: "far",
        title: "Far chain",
        start: far_start,
        finish: far_finish,
    });

    // --- The ferry and the yard, behind (-X) --------------------------------
    b.slab(
        "hub pier",
        v(-1700.0, -260.0, -70.0),
        v(-1350.0, 260.0, 0.0),
        Look::Paving,
        fixed(),
    );
    b.slab(
        "ferry",
        FERRY_REST - v(300.0, 300.0, 60.0),
        FERRY_REST + v(300.0, 300.0, 0.0),
        Look::Rider,
        Some(Solid::Ferry),
    );
    b.island("yard", YARD, 900.0, Look::Island, 140);
    b.slab(
        "yard pier",
        v(-6200.0, -260.0, YARD.z - 70.0),
        v(-5920.0, 260.0, YARD.z),
        Look::Paving,
        fixed(),
    );
    b.station("ferry", v(-1500.0, 0.0, 0.0), 180.0, 0.0);
    b.station("yard", YARD + v(300.0, 0.0, 0.0), 180.0, 12.0);

    // The carousel: a hub the engine turns about the vertical, rocks
    // attached to it at two heights.
    let carousel_hub = b.shape("carousel hub".to_owned(), || {
        Shape::rings(
            8,
            260.0,
            260.0,
            0.0,
            0.0,
            0,
            &[(500.0, 0.0), (120.0, 1.0), (-120.0, 1.0), (-500.0, 0.0)],
        )
    });
    let carousel = b.add(
        "carousel hub",
        carousel_hub,
        CAROUSEL_CENTRE,
        [0; 3],
        Look::Machine,
        Some(Solid::Turning {
            rate: [0, CAROUSEL_RATE, 0],
        }),
    );
    if let Some(hub) = carousel {
        let rock = b.shape("carousel rock".to_owned(), || {
            Shape::rings(
                9,
                330.0,
                300.0,
                0.4,
                0.03,
                150,
                &[(0.0, 1.0), (-70.0, 1.06), (-300.0, 0.6), (-520.0, 0.0)],
            )
        });
        for i in 0..CAROUSEL_ROCKS {
            let angle = std::f32::consts::TAU * i as f32 / CAROUSEL_ROCKS as f32;
            let lift = if i % 2 == 0 { -500.0 } else { 100.0 };
            let top = CAROUSEL_CENTRE
                + v(
                    CAROUSEL_RADIUS * angle.cos(),
                    CAROUSEL_RADIUS * angle.sin(),
                    lift,
                );
            b.add(
                &format!("carousel rock {}", i + 1),
                rock,
                top,
                [0; 3],
                Look::Rider,
                Some(Solid::Attached { hub, level: false }),
            );
            // Its own hook rides above it: a pull ends over the rock.
            let hook = b.hook_shape(200.0, 151);
            b.add(
                &format!("carousel hook {}", i + 1),
                hook,
                top + Vec3::Z * RIDER_HOOK_LIFT,
                [0; 3],
                Look::Hook,
                Some(Solid::Attached { hub, level: false }),
            );
        }
    }
    b.station("carousel", YARD + v(0.0, -650.0, 0.0), -90.0, 14.0);

    // The big wheel: a hub turned about its axle (the X axis), gondolas
    // attached to it that keep level.
    let wheel_hub = b.shape("wheel hub".to_owned(), || {
        Shape::block(v(420.0, 230.0, 230.0))
    });
    let wheel = b.add(
        "wheel hub",
        wheel_hub,
        WHEEL_CENTRE,
        [0; 3],
        Look::Machine,
        Some(Solid::Turning {
            rate: [0, 0, WHEEL_RATE],
        }),
    );
    if let Some(hub) = wheel {
        let gondola = b.shape("gondola".to_owned(), || {
            Shape::rings(
                8,
                340.0,
                340.0,
                0.39,
                0.0,
                0,
                &[(0.0, 1.0), (-60.0, 1.05), (-260.0, 0.0)],
            )
        });
        for i in 0..WHEEL_GONDOLAS {
            // The first gondola starts at the bottom, level with the yard.
            let angle = std::f32::consts::TAU * i as f32 / WHEEL_GONDOLAS as f32;
            let top =
                WHEEL_CENTRE + v(0.0, WHEEL_RADIUS * angle.sin(), -WHEEL_RADIUS * angle.cos());
            b.add(
                &format!("gondola {}", i + 1),
                gondola,
                top,
                [0; 3],
                Look::Rider,
                Some(Solid::Attached { hub, level: true }),
            );
            let hook = b.hook_shape(200.0, 152);
            b.add(
                &format!("gondola hook {}", i + 1),
                hook,
                top + Vec3::Z * RIDER_HOOK_LIFT,
                [0; 3],
                Look::Hook,
                Some(Solid::Attached { hub, level: true }),
            );
        }
    }
    // Where to board: a step out from the yard towards the wheel's lowest
    // point.
    b.island(
        "wheel step",
        v(YARD.x, 2050.0, YARD.z),
        360.0,
        Look::Island,
        160,
    );
    b.landmark("yard landmark", YARD + v(-620.0, 0.0, 0.0), 2200.0, 3, 161);
    b.station("wheel", v(YARD.x, 2050.0, YARD.z), 90.0, 24.0);

    // --- The tower and the long drop ----------------------------------------
    b.slab(
        "tower bridge",
        v(1000.0, 1000.0, -70.0),
        v(2050.0, 2050.0, 0.0),
        Look::Paving,
        fixed(),
    );
    b.island("tower island", TOWER_BASE, 620.0, Look::Island, 170);
    b.slab(
        "tower column",
        TOWER_BASE + v(-110.0, -110.0, 0.0),
        TOWER_BASE + v(110.0, 110.0, TOWER_HEIGHT),
        Look::Rock,
        fixed(),
    );
    // Ledges around the column.
    let ledge = b.shape("tower ledge".to_owned(), || {
        Shape::rings(
            6,
            260.0,
            260.0,
            0.0,
            0.0,
            0,
            &[(0.0, 1.0), (-50.0, 1.0), (-170.0, 0.0)],
        )
    });
    for (i, height) in TOWER_LEDGES.into_iter().enumerate() {
        let angle = 1.31 * (i as f32 + 1.0);
        b.add(
            &format!("tower ledge {}", i + 1),
            ledge,
            TOWER_BASE + v(400.0 * angle.cos(), 400.0 * angle.sin(), height),
            [0; 3],
            Look::Rock,
            fixed(),
        );
    }
    // The top, the board that points away from the hub over the catch, and
    // a hook above for those who would rather be pulled up (beside the top,
    // so that the pull from a ledge passes it and does not end under it).
    b.slab(
        "tower top",
        TOWER_BASE + v(-330.0, -330.0, TOWER_HEIGHT - 60.0),
        TOWER_BASE + v(330.0, 330.0, TOWER_HEIGHT),
        Look::Paving,
        fixed(),
    );
    b.slab(
        "tower board",
        TOWER_BASE + v(330.0, 150.0, TOWER_HEIGHT - 40.0),
        TOWER_BASE + v(1250.0, 450.0, TOWER_HEIGHT),
        Look::Ramp,
        fixed(),
    );
    b.hook("tower hook", TOWER_BASE + TOWER_HOOK, 240.0, 171);
    b.light(
        "tower light",
        TOWER_BASE + TOWER_HOOK + Vec3::Z * 260.0,
        80.0,
        Look::Beacon(4),
    );
    // The catch: a kill zone of the game's own kind, drawn as a red sheet.
    b.scene.kill_zone(
        CATCH_CENTRE - v(CATCH_HALF, CATCH_HALF, 150.0),
        CATCH_CENTRE + v(CATCH_HALF, CATCH_HALF, 150.0),
    );
    b.slab(
        "catch",
        CATCH_CENTRE - v(CATCH_HALF, CATCH_HALF, 20.0),
        CATCH_CENTRE + v(CATCH_HALF, CATCH_HALF, 20.0),
        Look::Hazard,
        None,
    );
    b.station(
        "tower",
        TOWER_BASE + v(250.0, 250.0, TOWER_HEIGHT),
        0.0,
        -30.0,
    );

    // Stations become teleport targets of the tools: checkpoints that are
    // switched off (a touch never activates them, so the hub stays the place
    // every fall ends).
    let stations = b.stations.clone();
    for (i, station) in stations.iter().enumerate().skip(1) {
        let yaw = (station.yaw / std::f32::consts::TAU * TURN as f32).round() as i32;
        b.scene.checkpoint(
            Place {
                location: station.feet + Vec3::Z * SPAWN_LIFT,
                rotation: [0, yaw, 0],
                scale: Vec3::ONE,
            },
            60.0,
            60.0,
            i32::try_from(i).unwrap_or(i32::MAX),
            json!({"bEnabled": false, "bRotatePlayerToSpawnPointRot": false}),
        );
    }

    let files = data_files(&b);
    Playground {
        shapes: b.shapes,
        pieces: b.pieces,
        courses: b.courses,
        stations: b.stations,
        files,
    }
}

// ---------------------------------------------------------------------------
// The generated data.
// ---------------------------------------------------------------------------

/// The playground's level script (ours): at level start it gives the grapple
/// budget and the rocket boots, the way a level's own start events do, and
/// starts the ferry's Matinee, which loops.
fn kismet_document(ferry: Option<(usize, Vec3)>) -> String {
    let ferry_path =
        |slot: usize| format!("{PLAYGROUND_MAP}.TheWorld.PersistentLevel.InterpActor_{slot}");
    let mut links = vec![json!({"op": 2, "input": 0})];
    let mut members = vec![1, 2, 3];
    let mut nodes = vec![
        json!({"id": 1, "class": "Engine.SeqEvent_LevelLoaded", "kind": "event", "parent": 0,
               "outputs": [{"desc": "Loaded and Visible", "links": []}],
               "event": {"max_trigger_count": 1}}),
        json!({"id": 2, "class": "asamu.SeqAct_SetMaxGrapples", "kind": "action", "parent": 0,
               "inputs": [{"desc": "In"}],
               "outputs": [{"desc": "Out", "links": [{"op": 3, "input": 0}]}],
               "params": {"Grapples": PLAYGROUND_GRAPPLES}, "auto_activate_outputs": true}),
        json!({"id": 3, "class": "asamu.SeqAct_ToggleRocketBoots", "kind": "action", "parent": 0,
               "inputs": [{"desc": "In"}],
               "outputs": [{"desc": "Out", "links": []}],
               "params": {"Enable": true}, "auto_activate_outputs": true}),
    ];
    let mut actors = Vec::new();
    if let Some((slot, location)) = ferry {
        links.push(json!({"op": 4, "input": 0}));
        members.extend([4, 5, 6]);
        nodes.push(json!({
            "id": 4, "class": "Engine.SeqAct_Interp", "kind": "action", "parent": 0,
            "inputs": [{"desc": "Play"}, {"desc": "Reverse"}, {"desc": "Stop"},
                       {"desc": "Pause"}, {"desc": "Change Dir"}],
            "outputs": [{"desc": "Completed", "links": []}, {"desc": "Reversed", "links": []}],
            "variables": [{"desc": "Data", "vars": [5]}, {"desc": "Ferry", "vars": [6]}],
            "auto_activate_outputs": true, "latent": true, "latent_base": true}));
        nodes.push(json!({
            "id": 5, "path": format!("{PLAYGROUND_MAP}.FerryData"),
            "class": "Engine.InterpData", "kind": "variable", "parent": 0}));
        nodes.push(json!({
            "id": 6, "class": "Engine.SeqVar_Object", "kind": "variable", "parent": 0,
            "var": {"value": {"$obj": ferry_path(slot)}}}));
        actors.push(json!({
            "path": ferry_path(slot), "name": format!("InterpActor_{slot}"),
            "class": "Engine.InterpActor", "kind": "interp_actor",
            "package": PLAYGROUND_MAP, "slot": slot,
            "location": location.to_array()}));
    }
    if let Some(outputs) = nodes
        .first_mut()
        .and_then(|n| n.get_mut("outputs"))
        .and_then(|o| o.get_mut(0))
        .and_then(|o| o.get_mut("links"))
    {
        *outputs = json!(links);
    }
    let mut all = vec![json!({
        "id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": members})];
    all.extend(nodes);
    json!({
        "format": asamu_game::asamu_kismet::RUNTIME_FORMAT,
        "version": asamu_game::asamu_kismet::RUNTIME_VERSION,
        "package": PLAYGROUND_MAP,
        "nodes": all,
        "actors": actors,
    })
    .to_string()
}

/// The ferry's Matinee data (ours): one movement track relative to where
/// the ferry is placed, there and back with a wait at each end, looping.
fn matinee_document(slot: usize) -> String {
    let key = |time: f32, offset: Vec3| {
        json!({"in": time, "out": offset.to_array(), "arrive": [0.0, 0.0, 0.0],
               "leave": [0.0, 0.0, 0.0], "mode": "linear"})
    };
    let (wait, crossing) = (FERRY_WAIT, FERRY_CROSSING);
    let length = 2.0 * (wait + crossing);
    let object = format!("{PLAYGROUND_MAP}.TheWorld.PersistentLevel.InterpActor_{slot}");
    json!({
        "format": "asamu-matinee", "version": 1, "package": PLAYGROUND_MAP,
        "actions": [{
            "path": format!("{PLAYGROUND_MAP}.FerryInterp"), "node": 4, "scope": "level",
            "interp_data": format!("{PLAYGROUND_MAP}.FerryData"),
            "settings": {"looping": true},
            "bindings": [{"link": 1, "label": "Ferry", "group": "Ferry",
                          "targets": [{"variable": format!("{PLAYGROUND_MAP}.FerryVar"),
                                       "object": object}]}],
        }],
        "interp_data": [{
            "path": format!("{PLAYGROUND_MAP}.FerryData"), "length": length,
            "groups": [{"kind": "group", "name": "Ferry", "tracks": [{
                "class": "Engine.InterpTrackMove",
                "data": {"type": "move", "move_frame": "relative_to_initial",
                         "pos": {"points": [
                             key(0.0, Vec3::ZERO),
                             key(wait, Vec3::ZERO),
                             key(wait + crossing, FERRY_TRAVEL),
                             key(2.0 * wait + crossing, FERRY_TRAVEL),
                             key(length, Vec3::ZERO),
                         ]},
                         "euler": {"points": [key(0.0, Vec3::ZERO)]}},
            }]}],
        }],
    })
    .to_string()
}

/// Everything the loader reads, as files of the converted-data layout.
fn data_files(b: &Builder) -> Vec<(String, Vec<u8>)> {
    let mut source = MemorySource::new();
    b.scene.write(&mut source);
    b.meshes.write_v2(&mut source, true);
    let mut files = Vec::new();
    // The writer's own layout: the scene, the manifest, and the meshes of
    // our one package.
    for dir in ["levels", "meshes", &format!("meshes/{PLAYGROUND_MAP}")] {
        for name in source.list(dir).unwrap_or_default() {
            let path = format!("{dir}/{name}");
            if let Ok(bytes) = source.read(&path, MAX_FILE_BYTES) {
                files.push((path, bytes));
            }
        }
    }
    files.push((
        format!("kismet/{PLAYGROUND_MAP}.kismet.json"),
        kismet_document(b.ferry).into_bytes(),
    ));
    if let Some((slot, _)) = b.ferry {
        files.push((
            format!("matinee/{PLAYGROUND_MAP}.matinee.json"),
            matinee_document(slot).into_bytes(),
        ));
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shape_is_a_closed_convex_solid() {
        let p = playground();
        assert!(p.shapes.len() > 20, "{}", p.shapes.len());
        for (index, shape) in p.shapes.iter().enumerate() {
            assert!(shape.is_closed(), "shape {index} is not closed");
            let error = shape.convexity_error();
            assert!(error < 0.05, "shape {index} is not convex: {error} uu");
            assert!(
                shape.vertices.iter().flatten().all(|c| c.is_finite()),
                "shape {index}"
            );
        }
        // Every piece names a shape that exists, and only decor has no actor.
        for piece in &p.pieces {
            assert!(piece.shape < p.shapes.len(), "{}", piece.name);
            assert_eq!(
                piece.actor.is_none(),
                piece.role == Role::Decor,
                "{}",
                piece.name
            );
            assert!(piece.location.is_finite(), "{}", piece.name);
        }
    }

    #[test]
    fn the_far_shots_are_at_the_edge_of_the_classic_reach() {
        let params = PlayerParams::asamu_original();
        let reach = params.gun.as_ref().unwrap().max_distance.value;
        let eye = params.movement.capsule_half_height.value + params.camera.eye_height.value;
        let hook = playground().piece("far hook 1").unwrap();
        let distance = (hook.location - (FAR_EDGE + Vec3::Z * eye)).length();
        assert!(
            (distance - FAR_SHOT_FRACTION * reach).abs() < 1.0,
            "{distance}"
        );
        assert!(distance < reach);
        // The stretch without a landing needs its crystals: more grapples
        // than the budget, and never more than the budget between two.
        let hooks = FAR_AIR.iter().filter(|c| !**c).count();
        assert!(hooks > PLAYGROUND_GRAPPLES as usize);
        let mut used = 0;
        for is_crystal in FAR_AIR {
            used += 1;
            assert!(
                used <= PLAYGROUND_GRAPPLES,
                "the budget runs out before a crystal"
            );
            if is_crystal {
                used = 0;
            }
        }
    }

    #[test]
    fn the_noise_stays_in_range_and_differs() {
        let values: Vec<f32> = (0..64).map(|i| noise(7, i)).collect();
        assert!(values.iter().all(|x| (0.0..1.0).contains(x)));
        assert!(values.iter().any(|x| *x < 0.25) && values.iter().any(|x| *x > 0.75));
        assert_eq!(noise(7, 3), noise(7, 3));
        assert_ne!(noise(7, 3), noise(8, 3));
    }

    fn course() -> Vec<Course> {
        vec![Course {
            id: "t",
            title: "Test route",
            start: Gate {
                min: v(0.0, 0.0, 0.0),
                max: v(10.0, 10.0, 10.0),
            },
            finish: Gate {
                min: v(100.0, 0.0, 0.0),
                max: v(110.0, 10.0, 10.0),
            },
        }]
    }

    #[test]
    fn the_clock_starts_on_leaving_the_arch_and_stops_in_the_frame() {
        let courses = course();
        let mut clock = CourseClock::default();
        let dt = 0.5;
        let inside = v(5.0, 5.0, 5.0);
        let between = v(50.0, 5.0, 5.0);
        let finish = v(105.0, 5.0, 5.0);
        // Nothing runs before the arch.
        clock.step(&courses, between, 1, dt, false);
        assert_eq!(clock.running(1, dt), None);
        // Standing under the arch keeps the clock at zero.
        clock.step(&courses, inside, 2, dt, false);
        clock.step(&courses, inside, 3, dt, false);
        assert_eq!(clock.running(3, dt), Some((0, 0.0)));
        // It counts from the last tick under the arch.
        clock.step(&courses, between, 4, dt, false);
        clock.step(&courses, between, 5, dt, false);
        assert_eq!(clock.running(5, dt), Some((0, 1.0)));
        clock.step(&courses, finish, 7, dt, false);
        assert_eq!(clock.running(7, dt), None);
        assert_eq!(
            clock.last(),
            Some(CourseRun {
                course: 0,
                seconds: 2.0,
                best: true
            })
        );
        assert_eq!(clock.best(0), Some(2.0));
        // A slower run is not the best; a faster one is.
        clock.step(&courses, inside, 10, dt, false);
        clock.step(&courses, finish, 20, dt, false);
        assert_eq!(
            clock.last().map(|r| (r.seconds, r.best)),
            Some((5.0, false))
        );
        assert_eq!(clock.best(0), Some(2.0));
        clock.step(&courses, inside, 30, dt, false);
        clock.step(&courses, finish, 32, dt, false);
        assert_eq!(clock.last().map(|r| (r.seconds, r.best)), Some((1.0, true)));
        assert_eq!(clock.best(0), Some(1.0));
    }

    #[test]
    fn a_death_a_cancel_and_a_bad_position_end_the_attempt_without_a_time() {
        let courses = course();
        let dt = 0.5;
        let inside = v(5.0, 5.0, 5.0);
        let finish = v(105.0, 5.0, 5.0);
        for end in 0..3 {
            let mut clock = CourseClock::default();
            clock.step(&courses, inside, 1, dt, false);
            match end {
                0 => clock.step(&courses, v(50.0, 5.0, 5.0), 2, dt, true),
                1 => clock.cancel(),
                _ => clock.step(&courses, Vec3::NAN, 2, dt, false),
            }
            assert_eq!(clock.running(2, dt), None, "{end}");
            clock.step(&courses, finish, 3, dt, false);
            assert_eq!(clock.last(), None, "{end}");
        }
        // A finish without a start is nothing.
        let mut clock = CourseClock::default();
        clock.step(&courses, finish, 1, dt, false);
        assert_eq!(clock.last(), None);
    }

    #[test]
    fn files_are_written_below_the_directory_and_nowhere_else() {
        let base = std::env::temp_dir().join(format!(
            "asamu-sandbox-playground-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        for bad in ["../x", "a/../../x", "/x", "a//b", "a\\b", "c:x", ""] {
            let files = vec![(bad.to_owned(), vec![1u8])];
            assert!(write_files(&base, &files).is_err(), "{bad:?}");
        }
        write_files(&base, &[("a/b/c.txt".to_owned(), b"ours".to_vec())]).unwrap();
        assert_eq!(
            std::fs::read(base.join("a").join("b").join("c.txt")).unwrap(),
            b"ours"
        );
        // A fresh directory is new every time and is the caller's alone.
        let first = fresh_dir(&base).unwrap();
        let second = fresh_dir(&base).unwrap();
        assert_ne!(first, second);
        assert!(first.is_dir() && second.is_dir());
        // A base that is not a directory is an error, not a panic.
        assert!(fresh_dir(&base.join("a").join("b").join("c.txt")).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
