//! The recorder's raw format, `asamu-trace-raw` version 1.
//!
//! JSON Lines: line 1 is a [`RawHeader`], every further non-blank line one
//! [`RawRecord`]. Values are in engine units exactly as read from the game:
//! distances in UU, rotations in rotator units (65536 per turn), key names as
//! the engine names them, `Physics` as its enum number. Record `R` is taken at
//! the start of frame `R.frame` (the `UWorld::Tick` entry): its state is the
//! end of the previous frame, its keys and `pressed_jump` are the input of
//! frame `R.frame` (see [`crate::convert`]).
//!
//! A raw file never contains addresses or pointers: objects are identified by
//! class name, object name and a per-recording pawn number.
//!
//! # Optional members (recorder 0.2.0)
//!
//! The Windows recorder 0.2.0 adds members to the player and gun parts of a
//! record without raising the version (docs/TRACE_CAPTURE.md 6.10): the
//! camera's FOV lock, its cached view, the pawn's floor normal, base eye
//! height, walk bob and collision cylinder, the level of the base actor, and
//! the weapon's script state and timers. The header names them in
//! `optional_fields` ([`OPTIONAL_FIELDS`] lists every name this reader
//! knows) and the file the offsets came from in `optional_layout`.
//!
//! Every one of them is typed here and **absent by default**: a file without
//! them is a plain version-1 file and is read, and written again, exactly as
//! before. A member this reader does not know is still refused, in a record
//! as in the header's list. A record may lack a member its header announces
//! (the recorder leaves a group out of a record whose value it could not
//! read or check) and may carry one the header does not announce (a group
//! whose check failed on the first object and passed on a later one), so
//! neither is an error; [`RawFile::optional_counts`] says what is there.
//! Where the recorder writes `null` as a value (`base_level` without a base,
//! `gun.state` without a state) the member is an `Option` inside the outer
//! `Option`; everywhere else `null` is refused.
//!
//! Markers and the dropped-frame log are not in the raw file: the recorder
//! writes them to `<recording>.stats.json` ([`crate::sidecar`]).

use std::io::{BufRead, Read};

use anyhow::{Context, Result, bail};
use asamu_core::glam::Vec3;
use serde::{Deserialize, Serialize};

/// Value of [`RawHeader::format`].
pub const RAW_FORMAT: &str = "asamu-trace-raw";
/// Supported [`RawHeader::version`].
pub const RAW_VERSION: u32 = 1;
/// Longest accepted line (bytes).
pub const MAX_LINE_BYTES: usize = 1 << 20;
/// Every name a header's `optional_fields` may hold, sorted: the members
/// recorder 0.2.0 can add to a record (`Optional.MEMBERS` in
/// `tools/trace-recorder/asamu_recorder_core.py`; `tests/python_crosscheck.rs`
/// compares the two lists).
pub const OPTIONAL_FIELDS: [&str; 12] = [
    "player.base_eye_height",
    "player.base_level",
    "player.bob",
    "player.camera_pov",
    "player.cylinder",
    "player.floor",
    "player.fov_default",
    "player.fov_lock",
    "player.fov_locked",
    "player.gun.state",
    "player.gun.timers",
    "player.walk_bob",
];

/// Serde for a member that is either absent (`None`) or present with a
/// value of its type: `null` is accepted only where the type itself takes
/// it. Used with `default` and `skip_serializing_if = "Option::is_none"`, so
/// an absent member stays absent when the file is written again.
mod present {
    use serde::{Deserialize, Deserializer};

    /// # Errors
    /// Propagates deserializer errors.
    pub fn value<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        T::deserialize(d).map(Some)
    }

    /// An `f32` written as its exact `f64` value (as `asamu_core::exact_f32`).
    pub mod float {
        use serde::{Deserialize, Deserializer, Serialize, Serializer};

        /// # Errors
        /// Propagates serializer errors.
        pub fn serialize<S: Serializer>(v: &Option<f32>, s: S) -> Result<S::Ok, S::Error> {
            v.map(f64::from).serialize(s)
        }

        /// # Errors
        /// Propagates deserializer errors (`null` is one).
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f32>, D::Error> {
            f64::deserialize(d).map(|v| Some(v as f32))
        }
    }

    /// A vector written as `[x, y, z]` of exact values.
    pub mod vec3 {
        use asamu_core::glam::Vec3;
        use serde::{Deserialize, Deserializer, Serialize, Serializer};

        /// # Errors
        /// Propagates serializer errors.
        pub fn serialize<S: Serializer>(v: &Option<Vec3>, s: S) -> Result<S::Ok, S::Error> {
            v.map(|v| [f64::from(v.x), f64::from(v.y), f64::from(v.z)])
                .serialize(s)
        }

        /// # Errors
        /// Propagates deserializer errors (`null` and a wrong length are).
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec3>, D::Error> {
            let [x, y, z] = <[f64; 3]>::deserialize(d)?;
            Ok(Some(Vec3::new(x as f32, y as f32, z as f32)))
        }
    }
}

/// One key binding of the running game (`Engine.Input.Bindings`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawBinding {
    /// Key or alias name (e.g. `SpaceBar`, `GBA_Jump`).
    pub name: String,
    /// Command string (e.g. `GBA_ReleaseableJump | RocketBoostKeyDown`).
    pub command: String,
}

/// First line of a raw recording.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawHeader {
    /// Always [`RAW_FORMAT`].
    pub format: String,
    /// Always [`RAW_VERSION`].
    pub version: u32,
    /// Recorder name and version.
    pub recorder: String,
    /// Id of the layout file the recorder used.
    pub layout: String,
    /// Original game build, e.g. `steam-1822049-mac`.
    #[serde(default)]
    pub game_build: Option<String>,
    /// Where in the frame the samples were taken.
    pub sample_point: String,
    /// Scenario id (e.g. `T1`, `A4-0.2`).
    #[serde(default)]
    pub scenario: Option<String>,
    /// Launch options the user set (recorded as told, not read from the game).
    #[serde(default)]
    pub launch_options: Option<String>,
    /// `GIsBenchmarking` (fixed time step) at the start.
    #[serde(default)]
    pub benchmarking: Option<bool>,
    /// `GFixedDeltaTime` at the start, seconds.
    #[serde(default)]
    pub fixed_delta_time: Option<f64>,
    /// The game's key bindings at the start.
    #[serde(default)]
    pub bindings: Vec<RawBinding>,
    /// Free-form notes.
    #[serde(default)]
    pub notes: Vec<String>,
    /// The optional record members the recording has, sorted (each one of
    /// [`OPTIONAL_FIELDS`]); empty for a plain version-1 header.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optional_fields: Vec<String>,
    /// Id of the file the optional members' offsets came from (`null` when
    /// only members that need no offset are written); absent from a plain
    /// version-1 header.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present::value"
    )]
    pub optional_layout: Option<Option<String>>,
}

/// World timing at the start of the frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawWorld {
    /// Map (outermost package of the `WorldInfo`).
    #[serde(default)]
    pub map: Option<String>,
    /// `WorldInfo.TimeSeconds` (end of the previous frame).
    #[serde(with = "asamu_core::exact_f32")]
    pub time_seconds: f32,
    /// `WorldInfo.RealTimeSeconds`.
    #[serde(with = "asamu_core::exact_f32")]
    pub real_time_seconds: f32,
    /// `WorldInfo.DeltaSeconds`: the (dilated) length of the previous frame.
    #[serde(with = "asamu_core::exact_f32")]
    pub delta_seconds: f32,
    /// `WorldInfo.TimeDilation`.
    #[serde(with = "asamu_core::exact_f32")]
    pub time_dilation: f32,
    /// The game is paused (`WorldInfo.Pauser` set).
    #[serde(default)]
    pub paused: bool,
}

/// `PlayerInput` axes, as processed by the previous frame's controller tick.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawAxes {
    /// `aBaseY`.
    #[serde(with = "asamu_core::exact_f32")]
    pub base_y: f32,
    /// `aStrafe`.
    #[serde(with = "asamu_core::exact_f32")]
    pub strafe: f32,
    /// `aForward`.
    #[serde(with = "asamu_core::exact_f32")]
    pub forward: f32,
    /// `aTurn`.
    #[serde(with = "asamu_core::exact_f32")]
    pub turn: f32,
    /// `aLookUp`.
    #[serde(with = "asamu_core::exact_f32")]
    pub look_up: f32,
    /// `aMouseX`.
    #[serde(with = "asamu_core::exact_f32")]
    pub mouse_x: f32,
    /// `aMouseY`.
    #[serde(with = "asamu_core::exact_f32")]
    pub mouse_y: f32,
}

/// One entry of an actor's `Timers` (optional member `gun.timers`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawTimer {
    /// Name of the function the timer calls.
    pub name: String,
    /// `Rate`: the timer's period, seconds.
    #[serde(with = "asamu_core::exact_f32")]
    pub rate: f32,
    /// `Count`: the time the timer has run, seconds.
    #[serde(with = "asamu_core::exact_f32")]
    pub count: f32,
    /// `bLoop`.
    #[serde(rename = "loop")]
    pub looping: bool,
    /// `bPaused`.
    pub paused: bool,
}

/// The grapple gun (`asamu.GrappleGun`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawGun {
    /// `bIsGrappling`.
    pub grappling: bool,
    /// `bReleasedGrapple`.
    pub released: bool,
    /// `bCanGrapple`.
    pub can_grapple: bool,
    /// Location of the anchor helper (`HitLocActor`), when it exists.
    #[serde(default, with = "asamu_core::exact_f32::option_vec3")]
    pub anchor: Option<Vec3>,
    /// `vGrappleLocation`.
    #[serde(with = "asamu_core::exact_f32::vec3")]
    pub grapple_location: Vec3,
    /// `vDistance`.
    #[serde(with = "asamu_core::exact_f32")]
    pub distance: f32,
    /// `iTimesGrappled`.
    pub times_grappled: i32,
    /// `iMaxGrapples`.
    pub max_grapples: i32,
    /// Optional: name of the weapon's script state (`Active`,
    /// `WeaponFiring`, ...); `Some(None)` without a state.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present::value"
    )]
    pub state: Option<Option<String>>,
    /// Optional: the weapon's `Timers`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present::value"
    )]
    pub timers: Option<Vec<RawTimer>>,
}

impl RawGun {
    /// The weapon's refire check among its recorded timers: the first entry
    /// named [`REFIRE_TIMER`] (names compare without ASCII case, as the
    /// engine's names do). `None` when the timers were not recorded or hold
    /// no such entry.
    #[must_use]
    pub fn refire_timer(&self) -> Option<&RawTimer> {
        self.timers
            .as_ref()?
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(REFIRE_TIMER))
    }
}

/// Name of the weapon timer that is its refire check: the stock weapon sets
/// a timer of this name when it fires, with the fire interval as its period
/// (docs/TRACE_CAPTURE.md 6.10; GRAPPLE.md, "Firing state": the refire check
/// of the `WeaponFiring` state, period `FireInterval[0]` = 0.1 s).
pub const REFIRE_TIMER: &str = "RefireCheckTimer";

/// The camera's cached view (optional member `camera_pov`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawCameraPov {
    /// `CameraCache.POV.Location`, UU.
    #[serde(with = "asamu_core::exact_f32::vec3")]
    pub location: Vec3,
    /// `CameraCache.POV.Rotation` (pitch, yaw, roll), rotator units.
    pub rotation: [i32; 3],
}

/// `UTPawn` view bob values (optional member `bob`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawBob {
    /// `Bob`.
    #[serde(with = "asamu_core::exact_f32")]
    pub bob: f32,
    /// `LandBob`.
    #[serde(with = "asamu_core::exact_f32")]
    pub land: f32,
    /// `JumpBob`.
    #[serde(with = "asamu_core::exact_f32")]
    pub jump: f32,
    /// `AppliedBob`.
    #[serde(with = "asamu_core::exact_f32")]
    pub applied: f32,
    /// `BobTime`.
    #[serde(with = "asamu_core::exact_f32")]
    pub time: f32,
    /// `bJustLanded`.
    pub just_landed: bool,
    /// `bLandRecovery`.
    pub land_recovery: bool,
}

/// The pawn's `CylinderComponent` (optional member `cylinder`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawCylinder {
    /// `CollisionRadius`, UU.
    #[serde(with = "asamu_core::exact_f32")]
    pub radius: f32,
    /// `CollisionHeight` (the half height), UU.
    #[serde(with = "asamu_core::exact_f32")]
    pub half_height: f32,
    /// The component's `Translation`, UU.
    #[serde(with = "asamu_core::exact_f32::vec3")]
    pub translation: Vec3,
    /// The component is the actor's `CollisionComponent`.
    pub collision_component: bool,
}

/// Script flags of `asamu.ASAMUPawn`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawPawnFlags {
    /// `bHasJumped`.
    pub has_jumped: bool,
    /// `bPowerJumped`.
    pub power_jumped: bool,
    /// `bHasReleasedJump`.
    pub has_released_jump: bool,
    /// `bSprinting`.
    pub sprinting: bool,
    /// `bIsFalling`.
    pub is_falling: bool,
}

/// `asamu.ASAMURocketBoots` flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawBoots {
    /// `bEnabled`.
    pub enabled: bool,
    /// `bFinished`.
    pub finished: bool,
}

/// The local player's state at the start of the frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawPlayer {
    /// Class of the player controller.
    #[serde(default)]
    pub controller_class: Option<String>,
    /// Class of the pawn.
    #[serde(default)]
    pub pawn_class: Option<String>,
    /// Number of the pawn object within the recording (changes when the
    /// pawn object changes).
    pub pawn_id: u64,
    /// Pawn `Location`, UU.
    #[serde(with = "asamu_core::exact_f32::vec3")]
    pub location: Vec3,
    /// Pawn `Velocity`, UU/s.
    #[serde(with = "asamu_core::exact_f32::vec3")]
    pub velocity: Vec3,
    /// Pawn `Acceleration`, UU/s².
    #[serde(with = "asamu_core::exact_f32::vec3")]
    pub acceleration: Vec3,
    /// Pawn `Rotation` (pitch, yaw, roll), rotator units.
    pub pawn_rotation: [i32; 3],
    /// Controller `Rotation` (the view), rotator units.
    pub view_rotation: [i32; 3],
    /// Pawn `Physics` (1 walking, 2 falling, 4 flying, ...).
    pub physics: u8,
    /// Name of the pawn's `Base` actor.
    #[serde(default)]
    pub base: Option<String>,
    /// Camera `CameraCache.POV.FOV`, degrees.
    #[serde(default, with = "asamu_core::exact_f32::option")]
    pub fov_camera: Option<f32>,
    /// Controller `FOVAngle`, degrees.
    #[serde(with = "asamu_core::exact_f32")]
    pub fov_controller: f32,
    /// `PlayerInput.PressedKeys`: keys held for the coming frame.
    #[serde(default)]
    pub keys: Vec<String>,
    /// `PlayerController.bPressedJump`.
    #[serde(default)]
    pub pressed_jump: bool,
    /// Input axes of the previous frame.
    #[serde(default)]
    pub axes: Option<RawAxes>,
    /// `GroundSpeed`.
    #[serde(with = "asamu_core::exact_f32")]
    pub ground_speed: f32,
    /// `AirSpeed`.
    #[serde(with = "asamu_core::exact_f32")]
    pub air_speed: f32,
    /// `JumpZ`.
    #[serde(with = "asamu_core::exact_f32")]
    pub jump_z: f32,
    /// `AirControl`.
    #[serde(with = "asamu_core::exact_f32")]
    pub air_control: f32,
    /// `EyeHeight` (camera height above the pawn's centre, before bob).
    #[serde(default, with = "asamu_core::exact_f32::option")]
    pub eye_height: Option<f32>,
    /// The grapple gun, when the pawn's weapon is one.
    #[serde(default)]
    pub gun: Option<RawGun>,
    /// ASAMU pawn flags.
    #[serde(default)]
    pub pawn_flags: Option<RawPawnFlags>,
    /// Rocket boots.
    #[serde(default)]
    pub boots: Option<RawBoots>,
    /// Optional: name of the outermost object of the `Base` actor (the
    /// package of the level it lies in); `Some(None)` without a base.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present::value"
    )]
    pub base_level: Option<Option<String>>,
    /// Optional: camera `DefaultFOV`, degrees.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "present::float"
    )]
    pub fov_default: Option<f32>,
    /// Optional: camera `bLockedFOV`. While it is set the camera's FOV is
    /// `fov_lock`, else the cached view FOV `fov_camera`
    /// (PARITY_FINDINGS.md V19).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present::value"
    )]
    pub fov_locked: Option<bool>,
    /// Optional: camera `LockedFOV`, degrees.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "present::float"
    )]
    pub fov_lock: Option<f32>,
    /// Optional: the camera's cached view.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present::value"
    )]
    pub camera_pov: Option<RawCameraPov>,
    /// Optional: pawn `Floor`, the normal the walking physics keeps (zero
    /// until the pawn has walked).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "present::vec3"
    )]
    pub floor: Option<Vec3>,
    /// Optional: pawn `BaseEyeHeight`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "present::float"
    )]
    pub base_eye_height: Option<f32>,
    /// Optional: `UTPawn.WalkBob`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "present::vec3"
    )]
    pub walk_bob: Option<Vec3>,
    /// Optional: the other bob values.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present::value"
    )]
    pub bob: Option<RawBob>,
    /// Optional: the pawn's collision cylinder.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present::value"
    )]
    pub cylinder: Option<RawCylinder>,
}

impl RawPlayer {
    /// The names ([`OPTIONAL_FIELDS`]) of the optional members this player
    /// part carries, sorted.
    #[must_use]
    pub fn optional_members(&self) -> Vec<&'static str> {
        let gun = self.gun.as_ref();
        [
            ("player.base_eye_height", self.base_eye_height.is_some()),
            ("player.base_level", self.base_level.is_some()),
            ("player.bob", self.bob.is_some()),
            ("player.camera_pov", self.camera_pov.is_some()),
            ("player.cylinder", self.cylinder.is_some()),
            ("player.floor", self.floor.is_some()),
            ("player.fov_default", self.fov_default.is_some()),
            ("player.fov_lock", self.fov_lock.is_some()),
            ("player.fov_locked", self.fov_locked.is_some()),
            ("player.gun.state", gun.is_some_and(|g| g.state.is_some())),
            ("player.gun.timers", gun.is_some_and(|g| g.timers.is_some())),
            ("player.walk_bob", self.walk_bob.is_some()),
        ]
        .into_iter()
        .filter(|(_, there)| *there)
        .map(|(name, _)| name)
        .collect()
    }
}

/// One frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRecord {
    /// `GFrameCounter` at the sample.
    pub frame: u64,
    /// The `DeltaSeconds` argument of `UWorld::Tick` for this frame
    /// (undilated), when the recorder could read it.
    #[serde(default, with = "asamu_core::exact_f32::option")]
    pub dt_arg: Option<f32>,
    /// World timing (`None`: no world).
    #[serde(default)]
    pub world: Option<RawWorld>,
    /// Player state (`None`: no player this frame).
    #[serde(default)]
    pub player: Option<RawPlayer>,
}

impl RawRecord {
    /// `true` if the frame has a player and an unpaused world.
    #[must_use]
    pub fn usable(&self) -> bool {
        self.player.is_some() && self.world.as_ref().is_some_and(|w| !w.paused)
    }
}

/// A whole raw recording.
#[derive(Clone, Debug, PartialEq)]
pub struct RawFile {
    /// Line 1.
    pub header: RawHeader,
    /// Frames in file order.
    pub records: Vec<RawRecord>,
}

impl RawFile {
    /// Reads a raw recording. Blank lines are skipped; `\r\n` is accepted.
    ///
    /// # Errors
    /// I/O, UTF-8, line length, JSON or header errors, with line numbers.
    pub fn read<R: BufRead>(mut r: R) -> Result<Self> {
        let mut header: Option<RawHeader> = None;
        let mut records = Vec::new();
        let mut buf = Vec::new();
        let mut number = 0_usize;
        loop {
            buf.clear();
            let limit = u64::try_from(MAX_LINE_BYTES + 2).unwrap_or(u64::MAX);
            let n = (&mut r).take(limit).read_until(b'\n', &mut buf)?;
            if n == 0 {
                break;
            }
            number += 1;
            let mut bytes = buf.as_slice();
            if let Some(rest) = bytes.strip_suffix(b"\n") {
                bytes = rest;
            } else if buf.len() > MAX_LINE_BYTES {
                bail!("line {number}: longer than {MAX_LINE_BYTES} bytes");
            }
            if let Some(rest) = bytes.strip_suffix(b"\r") {
                bytes = rest;
            }
            if bytes.len() > MAX_LINE_BYTES {
                bail!("line {number}: longer than {MAX_LINE_BYTES} bytes");
            }
            let line = std::str::from_utf8(bytes)
                .with_context(|| format!("line {number}: not valid UTF-8"))?;
            if line.trim().is_empty() {
                continue;
            }
            if header.is_none() {
                let h: RawHeader = serde_json::from_str(line)
                    .with_context(|| format!("line {number}: invalid raw header"))?;
                if h.format != RAW_FORMAT {
                    bail!(
                        "line {number}: not a raw recording (format {:?}, expected {RAW_FORMAT:?})",
                        h.format
                    );
                }
                if h.version != RAW_VERSION {
                    bail!(
                        "line {number}: unsupported raw version {} (supported: {RAW_VERSION})",
                        h.version
                    );
                }
                if let Some(unknown) = h
                    .optional_fields
                    .iter()
                    .find(|f| !OPTIONAL_FIELDS.contains(&f.as_str()))
                {
                    bail!(
                        "line {number}: the header announces an optional field this reader does \
                         not know: {unknown:?} (known: {})",
                        OPTIONAL_FIELDS.join(", ")
                    );
                }
                header = Some(h);
            } else {
                let rec: RawRecord = serde_json::from_str(line)
                    .with_context(|| format!("line {number}: invalid raw record"))?;
                records.push(rec);
            }
        }
        let header = header.context("empty raw recording (missing header)")?;
        Ok(Self { header, records })
    }

    /// Parses a raw recording from a string.
    ///
    /// # Errors
    /// See [`Self::read`].
    pub fn from_str_lines(s: &str) -> Result<Self> {
        Self::read(s.as_bytes())
    }

    /// Writes JSON Lines (header first).
    ///
    /// # Errors
    /// Serialization or I/O errors.
    pub fn write<W: std::io::Write>(&self, mut w: W) -> Result<()> {
        serde_json::to_writer(&mut w, &self.header)?;
        w.write_all(b"\n")?;
        for r in &self.records {
            serde_json::to_writer(&mut w, r)?;
            w.write_all(b"\n")?;
        }
        w.flush()?;
        Ok(())
    }

    /// How many records carry each optional member (by its name in
    /// [`OPTIONAL_FIELDS`]); empty for a plain version-1 recording.
    #[must_use]
    pub fn optional_counts(&self) -> std::collections::BTreeMap<&'static str, usize> {
        let mut counts = std::collections::BTreeMap::new();
        for p in self.records.iter().filter_map(|r| r.player.as_ref()) {
            for name in p.optional_members() {
                *counts.entry(name).or_insert(0) += 1;
            }
        }
        counts
    }

    /// JSON Lines as a string.
    ///
    /// # Errors
    /// Serialization errors.
    pub fn to_jsonl_string(&self) -> Result<String> {
        let mut buf = Vec::new();
        self.write(&mut buf)?;
        Ok(String::from_utf8(buf)?)
    }
}

/// `true` if the first non-blank line of `text` declares the raw format.
#[must_use]
pub fn looks_raw(first_line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(first_line)
        .ok()
        .and_then(|v| {
            v.get("format")
                .and_then(|f| f.as_str())
                .map(|f| f == RAW_FORMAT)
        })
        .unwrap_or(false)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn header() -> RawHeader {
        RawHeader {
            format: RAW_FORMAT.into(),
            version: RAW_VERSION,
            recorder: "test".into(),
            layout: "mac-x86_64-steam-1822049".into(),
            game_build: Some("steam-1822049-mac".into()),
            sample_point: "test".into(),
            scenario: Some("unit".into()),
            launch_options: Some("-BENCHMARK -FPS=60".into()),
            benchmarking: Some(true),
            fixed_delta_time: Some(f64::from(1.0_f32 / 60.0)),
            bindings: [
                ("GBA_MoveForward", "Axis aBaseY Speed=1.0"),
                ("GBA_Backward", "Axis aBaseY Speed=-1.0"),
                ("GBA_StrafeLeft", "Axis aStrafe Speed=-1.0"),
                ("GBA_StrafeRight", "Axis aStrafe Speed=+1.0"),
                ("GBA_ReleaseableJump", "Jump | OnRelease ReleaseJump"),
                ("GBA_Fire", "StartFire | OnRelease StopFire"),
                ("GBA_Sprint", "StartSprinting | OnRelease StopSprinting"),
                (
                    "GBA_PowerJump",
                    "PowerJumpKeyDown | OnRelease PowerJumpKeyUp",
                ),
                ("GBA_Use", "use"),
                ("W", "GBA_MoveForward"),
                ("S", "GBA_Backward"),
                ("A", "GBA_StrafeLeft"),
                ("D", "GBA_StrafeRight"),
                ("SpaceBar", "GBA_ReleaseableJump | RocketBoostKeyDown"),
                ("LeftShift", "GBA_Sprint"),
                ("LeftMouseButton", "GBA_Fire"),
                ("RightMouseButton", "GBA_PowerJump"),
                ("E", "GBA_Use"),
            ]
            .into_iter()
            .map(|(n, c)| RawBinding {
                name: n.into(),
                command: c.into(),
            })
            .collect(),
            notes: vec![],
            optional_fields: vec![],
            optional_layout: None,
        }
    }

    pub(crate) fn player(x: f32, yaw: i32, keys: &[&str]) -> RawPlayer {
        RawPlayer {
            controller_class: Some("ASAMUPlayerController".into()),
            pawn_class: Some("ASAMUPawn".into()),
            pawn_id: 0,
            location: Vec3::new(x, -3.5, 45.05),
            velocity: Vec3::new(2.0 * x, 0.0, 0.0),
            acceleration: Vec3::ZERO,
            pawn_rotation: [0, yaw, 0],
            view_rotation: [0, yaw, 0],
            physics: 1,
            base: Some("StaticMeshActor_12".into()),
            fov_camera: Some(90.0),
            fov_controller: 90.0,
            keys: keys.iter().map(|k| (*k).to_owned()).collect(),
            pressed_jump: false,
            axes: None,
            ground_speed: 440.0,
            air_speed: 440.0,
            jump_z: 1000.0,
            air_control: 0.3,
            eye_height: Some(38.0),
            gun: Some(RawGun {
                grappling: false,
                released: false,
                can_grapple: true,
                anchor: None,
                grapple_location: Vec3::ZERO,
                distance: 0.0,
                times_grappled: 0,
                max_grapples: 2,
                state: None,
                timers: None,
            }),
            pawn_flags: Some(RawPawnFlags {
                has_jumped: false,
                power_jumped: false,
                has_released_jump: false,
                sprinting: false,
                is_falling: false,
            }),
            boots: Some(RawBoots {
                enabled: true,
                finished: false,
            }),
            base_level: None,
            fov_default: None,
            fov_locked: None,
            fov_lock: None,
            camera_pov: None,
            floor: None,
            base_eye_height: None,
            walk_bob: None,
            bob: None,
            cylinder: None,
        }
    }

    /// `p` with every optional member of recorder 0.2.0, as that recorder
    /// writes them for a pawn that stands on `base_level` with the camera's
    /// FOV lock at `locked`.
    pub(crate) fn with_optional(mut p: RawPlayer, locked: bool) -> RawPlayer {
        p.base_level = Some(Some("AG-Workshop".into()));
        p.fov_default = Some(90.0);
        p.fov_locked = Some(locked);
        p.fov_lock = Some(if locked { 50.0 } else { 0.0 });
        p.camera_pov = Some(RawCameraPov {
            location: p.location + Vec3::new(0.0, 0.0, 38.0),
            rotation: p.view_rotation,
        });
        p.floor = Some(Vec3::Z);
        p.base_eye_height = Some(38.0);
        p.walk_bob = Some(Vec3::new(0.0, 0.25, -0.5));
        p.bob = Some(RawBob {
            bob: 0.01,
            land: 0.0,
            jump: 0.0,
            applied: 0.125,
            time: 2.5,
            just_landed: false,
            land_recovery: true,
        });
        p.cylinder = Some(RawCylinder {
            radius: 21.0,
            half_height: 44.0,
            translation: Vec3::ZERO,
            collision_component: true,
        });
        if let Some(g) = p.gun.as_mut() {
            g.state = Some(Some(if locked { "WeaponFiring" } else { "Active" }.into()));
            g.timers = Some(vec![
                RawTimer {
                    name: REFIRE_TIMER.into(),
                    rate: if locked { 0.1 } else { 0.0 },
                    count: if locked { 0.0625 } else { 0.0 },
                    looping: false,
                    paused: false,
                },
                RawTimer {
                    name: "InstantReleaseTimer".into(),
                    rate: 0.05,
                    count: 0.0,
                    looping: true,
                    paused: true,
                },
            ]);
        }
        p
    }

    pub(crate) fn record(frame: u64, p: RawPlayer) -> RawRecord {
        RawRecord {
            frame,
            dt_arg: Some(1.0 / 60.0),
            world: Some(RawWorld {
                map: Some("AG-Workshop".into()),
                time_seconds: frame as f32 / 60.0,
                real_time_seconds: frame as f32 / 60.0,
                delta_seconds: 1.0 / 60.0,
                time_dilation: 1.0,
                paused: false,
            }),
            player: Some(p),
        }
    }

    #[test]
    fn round_trip_and_errors() {
        let f = RawFile {
            header: header(),
            records: vec![
                record(5, player(1.0, 0, &["W"])),
                record(6, player(2.5, 7, &[])),
            ],
        };
        let s = f.to_jsonl_string().unwrap();
        assert!(looks_raw(s.lines().next().unwrap()));
        let back = RawFile::from_str_lines(&s).unwrap();
        assert_eq!(back, f);
        let crlf = s.replace('\n', "\r\n\r\n");
        assert_eq!(RawFile::from_str_lines(&crlf).unwrap(), f);

        assert!(RawFile::from_str_lines("").is_err());
        let wrong = s.replacen("asamu-trace-raw", "asamu-trace", 1);
        assert!(!looks_raw(wrong.lines().next().unwrap()));
        assert!(RawFile::from_str_lines(&wrong).is_err());
        let v2 = s.replacen(r#""version":1"#, r#""version":2"#, 1);
        assert!(format!("{:#}", RawFile::from_str_lines(&v2).unwrap_err()).contains("version 2"));
        let extra = s.replacen(r#"{"frame":6"#, r#"{"bogus":1,"frame":6"#, 1);
        let e = format!("{:#}", RawFile::from_str_lines(&extra).unwrap_err());
        assert!(e.contains("line 3"), "{e}");
        let truncated = &s[..s.len() - 20];
        assert!(RawFile::from_str_lines(truncated).is_err());
    }

    #[test]
    fn hostile_lines() {
        let f = RawFile {
            header: header(),
            records: vec![record(1, player(1.0, 0, &[]))],
        };
        let s = f.to_jsonl_string().unwrap();
        // Blank lines only: no header.
        assert!(RawFile::from_str_lines("\n \r\n\t\n").is_err());
        // An over-long line is refused with its number, without reading on.
        let mut long = s.clone();
        long.push_str(&" ".repeat(MAX_LINE_BYTES + 1));
        long.push('\n');
        let e = format!("{:#}", RawFile::from_str_lines(&long).unwrap_err());
        assert!(e.contains("line 3") && e.contains("longer than"), "{e}");
        // An over-long last line without a newline too.
        let mut tail = s.clone();
        tail.push_str(&"x".repeat(MAX_LINE_BYTES + 5));
        assert!(RawFile::from_str_lines(&tail).is_err());
        // Exactly MAX_LINE_BYTES of blanks plus CR LF is accepted (skipped).
        let mut edge = s.clone();
        edge.push_str(&" ".repeat(MAX_LINE_BYTES));
        edge.push_str("\r\n");
        assert_eq!(RawFile::from_str_lines(&edge).unwrap(), f);
        // Invalid UTF-8.
        let mut bytes = s.into_bytes();
        bytes.extend_from_slice(b"{\"frame\":2,\xff}\n");
        let e = format!("{:#}", RawFile::read(bytes.as_slice()).unwrap_err());
        assert!(e.contains("line 3") && e.contains("UTF-8"), "{e}");
        // Wrong types and missing fields are errors, not defaults.
        let h = serde_json::to_string(&header()).unwrap();
        for rec in [
            r#"{"frame":-1}"#,
            r#"{"frame":1,"player":{"pawn_id":0}}"#,
            r#"{"frame":1,"world":{"time_seconds":"x"}}"#,
        ] {
            assert!(
                RawFile::from_str_lines(&format!("{h}\n{rec}\n")).is_err(),
                "{rec}"
            );
        }
    }

    /// The optional members of recorder 0.2.0: read as the recorder writes
    /// them, written back member for member, absent when absent.
    #[test]
    fn optional_members_round_trip() {
        let mut h = header();
        h.recorder = "asamu_win 0.2.0".into();
        h.optional_fields = OPTIONAL_FIELDS.iter().map(|s| (*s).to_owned()).collect();
        h.optional_layout = Some(Some("win-x86-steam-1822049-optional".into()));
        let mut unbased = with_optional(player(2.5, 7, &[]), true);
        unbased.base_level = Some(None);
        unbased.gun.as_mut().unwrap().state = Some(None);
        unbased.gun.as_mut().unwrap().timers = Some(vec![]);
        let f = RawFile {
            header: h,
            records: vec![
                record(5, with_optional(player(1.0, 0, &["W"]), false)),
                record(6, unbased),
                // A record may lack what the header announces.
                record(7, player(3.0, 7, &[])),
            ],
        };
        let s = f.to_jsonl_string().unwrap();
        let back = RawFile::from_str_lines(&s).unwrap();
        assert_eq!(back, f);
        let lines: Vec<&str> = s.lines().collect();
        // The members are where the recorder puts them, under its names.
        let first: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        let p = &first["player"];
        assert_eq!(p["base_level"], "AG-Workshop");
        assert_eq!(p["fov_default"], 90.0);
        assert_eq!(p["fov_locked"], false);
        assert_eq!(p["fov_lock"], 0.0);
        assert_eq!(p["camera_pov"]["rotation"], serde_json::json!([0, 0, 0]));
        assert_eq!(p["floor"], serde_json::json!([0.0, 0.0, 1.0]));
        assert_eq!(p["base_eye_height"], 38.0);
        assert_eq!(p["walk_bob"], serde_json::json!([0.0, 0.25, -0.5]));
        assert_eq!(p["bob"]["land_recovery"], true);
        assert_eq!(p["cylinder"]["half_height"], 44.0);
        assert_eq!(p["gun"]["state"], "Active");
        assert_eq!(p["gun"]["timers"][0]["name"], "RefireCheckTimer");
        assert_eq!(p["gun"]["timers"][1]["loop"], true);
        // A recorded null is a value, not an absence.
        let second: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert!(second["player"]["base_level"].is_null());
        assert!(second["player"]["gun"]["state"].is_null());
        assert_eq!(second["player"]["gun"]["timers"], serde_json::json!([]));
        // A record without them has no such member at all (a version-1 line).
        let third: serde_json::Value = serde_json::from_str(lines[3]).unwrap();
        let members = |v: &serde_json::Value| -> Vec<String> {
            v.as_object().unwrap().keys().cloned().collect()
        };
        let plain = serde_json::to_value(record(7, player(3.0, 7, &[]))).unwrap();
        assert_eq!(members(&third["player"]), members(&plain["player"]));
        assert_eq!(members(&third["player"]).len(), 23, "the version-1 members");
        assert_eq!(members(&third["player"]["gun"]).len(), 8);
        // What is there, counted.
        let counts = back.optional_counts();
        assert_eq!(counts.len(), OPTIONAL_FIELDS.len());
        assert!(counts.values().all(|n| *n == 2), "{counts:?}");
        assert_eq!(
            back.records[0].player.as_ref().unwrap().optional_members(),
            OPTIONAL_FIELDS
        );
        let gun = back.records[0].player.as_ref().unwrap().gun.as_ref();
        assert_eq!(gun.unwrap().refire_timer().unwrap().rate, 0.0);
        assert!(
            back.records[2]
                .player
                .as_ref()
                .unwrap()
                .gun
                .as_ref()
                .unwrap()
                .refire_timer()
                .is_none()
        );
        // A plain version-1 file has none, and its header stays as it was.
        let v1 = RawFile {
            header: header(),
            records: vec![record(1, player(1.0, 0, &[]))],
        };
        assert!(v1.optional_counts().is_empty());
        let text = v1.to_jsonl_string().unwrap();
        assert!(
            !text.contains("optional") && !text.contains("fov_lock"),
            "{text}"
        );
        assert_eq!(
            members(&serde_json::from_str(text.lines().next().unwrap()).unwrap()).len(),
            12
        );
    }

    /// Each optional member on its own, and what is still refused: unknown
    /// members, wrong types, and `null` where the recorder writes none.
    #[test]
    fn optional_members_are_typed_and_strict() {
        let h = serde_json::to_string(&header()).unwrap();
        let plain = serde_json::to_value(record(1, player(1.0, 0, &[]))).unwrap();
        let read = |player: &dyn Fn(&mut serde_json::Value)| {
            let mut v = plain.clone();
            player(&mut v["player"]);
            RawFile::from_str_lines(&format!("{h}\n{v}\n"))
        };
        use serde_json::json;
        let timer = json!({"name": "RefireCheckTimer", "rate": 0.125, "count": 0.0625, "loop": false, "paused": false});
        let good: Vec<(&str, serde_json::Value)> = vec![
            ("base_level", json!("freds_place")),
            ("base_level", json!(null)),
            ("fov_default", json!(90.0)),
            ("fov_locked", json!(true)),
            ("fov_lock", json!(50.0)),
            (
                "camera_pov",
                json!({"location": [1.0, 2.0, 3.0], "rotation": [-100, 16, 0]}),
            ),
            ("floor", json!([0.0, 0.0, 1.0])),
            ("base_eye_height", json!(38.0)),
            ("walk_bob", json!([0.0, 0.25, -0.5])),
            (
                "bob",
                json!({"bob": 0.015625, "land": 0.0, "jump": 0.0, "applied": 0.125, "time": 3.0, "just_landed": false, "land_recovery": true}),
            ),
            (
                "cylinder",
                json!({"radius": 21.0, "half_height": 44.0, "translation": [0.0, 0.0, 0.0], "collision_component": true}),
            ),
        ];
        for (name, value) in &good {
            let f = read(&|p| p[*name] = value.clone()).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            let p = f.records[0].player.as_ref().unwrap();
            assert_eq!(p.optional_members(), [format!("player.{name}")], "{name}");
            // Written back as read (the values here are exact in f32).
            let again = serde_json::to_value(&f.records[0]).unwrap();
            assert_eq!(&again["player"][*name], value, "{name}");
        }
        for (name, value) in [
            ("state", json!("WeaponFiring")),
            ("state", json!(null)),
            ("timers", json!([timer.clone()])),
            ("timers", json!([])),
        ] {
            let f = read(&|p| p["gun"][name] = value.clone())
                .unwrap_or_else(|e| panic!("{name}: {e:#}"));
            let p = f.records[0].player.as_ref().unwrap();
            assert_eq!(p.optional_members(), [format!("player.gun.{name}")]);
            assert_eq!(
                serde_json::to_value(&f.records[0]).unwrap()["player"]["gun"][name],
                value
            );
        }
        let f = read(&|p| p["fov_lock"] = json!(50.0)).unwrap();
        assert_eq!(f.records[0].player.as_ref().unwrap().fov_lock, Some(50.0));
        let f = read(&|p| p["floor"] = json!([0.6, 0.0, 0.8])).unwrap();
        assert_eq!(
            f.records[0].player.as_ref().unwrap().floor,
            Some(Vec3::new(0.6, 0.0, 0.8)),
            "values are narrowed to the f32 the game holds"
        );
        // Refused: a null where the recorder writes a value, a wrong type, a
        // wrong shape, a member nobody writes.
        let bad: Vec<(&str, serde_json::Value)> = vec![
            ("fov_default", json!(null)),
            ("fov_locked", json!(null)),
            ("fov_locked", json!(1)),
            ("fov_lock", json!("50")),
            ("camera_pov", json!(null)),
            ("camera_pov", json!({"location": [1.0, 2.0, 3.0]})),
            (
                "camera_pov",
                json!({"location": [1.0, 2.0, 3.0], "rotation": [0.5, 0, 0]}),
            ),
            (
                "camera_pov",
                json!({"location": [1.0, 2.0, 3.0], "rotation": [0, 0, 0], "fov": 90.0}),
            ),
            ("floor", json!(null)),
            ("floor", json!([0.0, 1.0])),
            ("floor", json!([0.0, 0.0, 1.0, 0.0])),
            ("base_eye_height", json!(null)),
            ("walk_bob", json!({"x": 0})),
            ("bob", json!({"bob": 0.01})),
            ("bob", json!(null)),
            (
                "cylinder",
                json!({"radius": 21.0, "half_height": 44.0, "translation": [0.0, 0.0, 0.0]}),
            ),
            (
                "cylinder",
                json!({"radius": 21.0, "height": 44.0, "translation": [0.0, 0.0, 0.0], "collision_component": true}),
            ),
            ("base_level", json!(12)),
            ("fov_zoom", json!(50.0)),
            ("marker", json!("x")),
        ];
        for (name, value) in &bad {
            assert!(
                read(&|p| p[*name] = value.clone()).is_err(),
                "{name} = {value}"
            );
        }
        let mut broken = timer.clone();
        broken["extra"] = json!(1);
        let mut unnamed = timer.clone();
        unnamed.as_object_mut().unwrap().remove("name");
        for (name, value) in [
            ("state", json!(3)),
            ("timers", json!(null)),
            ("timers", json!({})),
            ("timers", json!([broken])),
            ("timers", json!([unnamed])),
            ("refire", json!(0.1)),
        ] {
            assert!(
                read(&|p| p["gun"][name] = value.clone()).is_err(),
                "gun.{name} = {value}"
            );
        }
        // The header: its two members, and a name this reader does not know.
        let rec = serde_json::to_string(&plain).unwrap();
        let with = |extra: &str| {
            let line = h.replacen('{', &format!("{{{extra},"), 1);
            RawFile::from_str_lines(&format!("{line}\n{rec}\n"))
        };
        let ok = with(r#""optional_fields":["player.floor","player.gun.state"],"optional_layout":"win-x86-steam-1822049-optional""#).unwrap();
        assert_eq!(
            ok.header.optional_fields,
            ["player.floor", "player.gun.state"]
        );
        assert_eq!(
            ok.header.optional_layout,
            Some(Some("win-x86-steam-1822049-optional".into()))
        );
        // Only the member that needs no offset: the layout id is null.
        let old =
            with(r#""optional_fields":["player.base_level"],"optional_layout":null"#).unwrap();
        assert_eq!(old.header.optional_layout, Some(None));
        assert!(
            old.to_jsonl_string()
                .unwrap()
                .contains(r#""optional_layout":null"#)
        );
        let e = format!(
            "{:#}",
            with(r#""optional_fields":["player.floor","player.mood"]"#).unwrap_err()
        );
        assert!(
            e.contains("line 1") && e.contains("player.mood") && e.contains("does not know"),
            "{e}"
        );
        assert!(with(r#""optional_fields":"player.floor""#).is_err());
        assert!(with(r#""optional_fields":[1]"#).is_err());
        assert!(with(r#""optional_layout":7"#).is_err());
        assert!(
            with(r#""optional":true"#).is_err(),
            "an unknown header member"
        );
        // The list this reader knows is sorted and has no doubles.
        assert!(OPTIONAL_FIELDS.windows(2).all(|w| w[0] < w[1]));
    }
}
