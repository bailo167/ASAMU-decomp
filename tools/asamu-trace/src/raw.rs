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

/// The grapple gun (`asamu.GrappleGun`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
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
        }
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
}
