//! Replays a trace's inputs through our simulation and records our trace.
//!
//! The replay builds an [`asamu_game::Game`] on the graybox level or on a
//! level converted by `asamu-import` (optionally with the map's Kismet,
//! [`asamu_game::load_level_with_kismet`]), puts the pawn in the trace's
//! initial state (sample 0: position, velocity, yaw, pitch, walking or
//! falling), applies the recorded initial script state from the `init:` note
//! (grapple capacity and used count, rocket boots) and then feeds every
//! sample's input, one fixed tick each, at the trace's tick rate. It stops at
//! the first tick gap. The result is a runtime trace aligned tick for tick
//! with the input trace, ready for [`crate::compare`].
//!
//! If the game's own spawn state already equals sample 0 (a runtime trace
//! that started at the spawn point), the spawn state is kept unchanged, so
//! replaying a runtime recording reproduces it exactly.
//!
//! Limits (schema v1 carries no more state): a replay that starts while the
//! grapple is attached does not recreate the attachment; script state other
//! than the `init:` fields (jump damping, power-jump charge, boost timeline,
//! sprint latch) starts fresh, which is why scenarios start standing still.
//! With `--kismet` the map's Kismet always starts from level start, also for
//! a segment replay (`start_tick`), and its level-start actions may override
//! the `init:` grapple and boots state; the notes say so.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use asamu_core::DEFAULT_TICK_RATE_HZ;
use asamu_core::rotator::wrap_radians;
use asamu_game::{Game, LevelScript, load_level_with_kismet};
use asamu_player::trace::{TraceGrappleState, TraceSource};
use asamu_player::{
    MovementModelKind, PawnPhysicsState, PlayerParams, Trace, grapple_gun, rocket_boots,
};

use crate::convert::InitState;

/// Which level the replay runs on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayLevel {
    /// The hand-made graybox level (`asamu_world::graybox_test_level`).
    Graybox,
    /// A level converted by `asamu-import`.
    Converted {
        /// The converted output directory.
        dir: PathBuf,
        /// Map name (e.g. `AG-Workshop`).
        map: String,
        /// Run the map's Kismet ([`load_level_with_kismet`]); 60 Hz only.
        kismet: bool,
    },
}

/// Replay options.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayOptions {
    /// Level.
    pub level: ReplayLevel,
    /// Tick rate override, Hz (required for variable-rate traces).
    pub tick_rate: Option<f64>,
    /// Apply the trace's `init:` note.
    pub use_init: bool,
    /// Grapple capacity override (`SetMaxGrapples`).
    pub max_grapples: Option<i32>,
    /// Use the placeholder parameters and model (debugging).
    pub placeholder: bool,
    /// Start from the sample with this tick instead of the first one
    /// (segment replays: separates local error from accumulated drift).
    pub start_tick: Option<u64>,
    /// Replay at most this many ticks.
    pub max_ticks: Option<u64>,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            level: ReplayLevel::Graybox,
            tick_rate: None,
            use_init: true,
            max_grapples: None,
            placeholder: false,
            start_tick: None,
            max_ticks: None,
        }
    }
}

/// Result of [`replay`].
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayResult {
    /// Our trace, with the input trace's tick numbers.
    pub trace: Trace,
    /// The input trace's tick after which a gap stopped the replay.
    pub stopped_at_gap: Option<u64>,
    /// Respawns during the replay.
    pub respawns: u32,
}

fn params(placeholder: bool) -> PlayerParams {
    if placeholder {
        PlayerParams::placeholder()
    } else {
        PlayerParams::asamu_original()
    }
}

fn build(opts: &ReplayOptions, rate: f64) -> Result<(Game, Option<LevelScript>, String)> {
    let p = params(opts.placeholder);
    let model = if opts.placeholder {
        MovementModelKind::Placeholder
    } else {
        MovementModelKind::Ue3Pawn
    };
    Ok(match &opts.level {
        ReplayLevel::Graybox => {
            let level = Game::graybox()?.level().clone();
            let g = Game::new(level, p, rate)?.with_movement_model(model);
            (g, None, "graybox".to_owned())
        }
        ReplayLevel::Converted { dir, map, kismet } => {
            if *kismet {
                if (rate - DEFAULT_TICK_RATE_HZ).abs() > 1e-9 {
                    bail!(
                        "--kismet replays run at {DEFAULT_TICK_RATE_HZ} Hz only; the trace needs {rate} Hz"
                    );
                }
                if opts.placeholder {
                    bail!("--kismet needs the original parameters (not --placeholder)");
                }
                let (g, script) = load_level_with_kismet(dir, map)
                    .with_context(|| format!("loading {map} with Kismet from {}", dir.display()))?;
                let desc = if script.is_some() {
                    format!("converted {map} with Kismet")
                } else {
                    format!("converted {map} (no Kismet export: replayed without Kismet)")
                };
                (g, script, desc)
            } else {
                let g = Game::load_level(dir, map)
                    .with_context(|| format!("loading {map} from {}", dir.display()))?;
                let same = (rate - DEFAULT_TICK_RATE_HZ).abs() <= 1e-9 && !opts.placeholder;
                let g = if same {
                    g
                } else {
                    let m = g
                        .scene_map()
                        .cloned()
                        .context("converted game without a map")?;
                    Game::from_loaded_map(m, p, rate)?.with_movement_model(model)
                };
                (g, None, format!("converted {map}"))
            }
        }
    })
}

/// Replays `original` (see the module docs).
///
/// # Errors
/// Empty trace, no tick rate, level loading or game construction errors.
pub fn replay(original: &Trace, opts: &ReplayOptions) -> Result<ReplayResult> {
    let start = match opts.start_tick {
        None => 0,
        Some(t) => original
            .samples
            .iter()
            .position(|s| s.tick == t)
            .with_context(|| format!("the trace has no sample at tick {t}"))?,
    };
    let end = match opts.max_ticks {
        None => original.samples.len(),
        Some(n) => usize::try_from(n)
            .ok()
            .and_then(|n| start.checked_add(n)?.checked_add(1))
            .map_or(original.samples.len(), |e| e.min(original.samples.len())),
    };
    let samples = original.samples.get(start..end).unwrap_or(&[]);
    let Some(s0) = samples.first() else {
        bail!("the trace has no samples");
    };
    let rate = match (opts.tick_rate, original.meta.tick_rate) {
        (Some(r), _) => r,
        (None, Some(r)) => f64::from(r),
        (None, None) => bail!("the trace has no fixed tick rate; pass --tick-rate"),
    };
    if !(rate.is_finite() && rate > 0.0) {
        bail!("invalid tick rate {rate}");
    }
    let (mut game, mut script, level_desc) = build(opts, rate)?;
    let source = match original.meta.source {
        TraceSource::Original => "original",
        TraceSource::Runtime => "runtime",
    };
    let mut notes = vec![format!(
        "asamu-trace replay of a {source} trace on {level_desc} at {rate} Hz"
    )];
    if let Some(l) = &original.meta.level {
        notes.push(format!("input trace level: {l}"));
    }
    if script.is_some() && start > 0 {
        notes.push(format!(
            "note: Kismet starts from level start, not from the state at tick {}",
            s0.tick
        ));
    }

    // Initial state.
    {
        let p = game.player_mut();
        let yaw = wrap_radians(s0.yaw);
        let same = p.position == s0.position
            && p.velocity == s0.velocity
            && p.yaw == yaw
            && p.pitch == s0.pitch
            && p.grounded == s0.grounded;
        if same {
            notes.push("initial state: the spawn state equals sample 0 (kept)".to_owned());
        } else {
            p.position = s0.position;
            p.velocity = s0.velocity;
            p.yaw = yaw;
            p.pitch = s0.pitch;
            p.grounded = s0.grounded;
            p.pawn = PawnPhysicsState {
                force_floor_check: s0.grounded,
                ..PawnPhysicsState::default()
            };
            if p.script.started {
                p.script.pov_yaw = yaw;
                p.script.pov_pitch = s0.pitch;
            }
            notes.push(format!(
                "initial state: sample {} (position, velocity, view, {})",
                s0.tick,
                if s0.grounded { "walking" } else { "falling" }
            ));
        }
        if s0.grapple_state == TraceGrappleState::Attached {
            notes.push(
                "warning: the trace starts attached; the attachment is not recreated".to_owned(),
            );
        }
    }
    if opts.use_init
        && let Some(init) = InitState::from_notes(&original.meta.notes)
    {
        let p = game.player_mut();
        let mut applied = Vec::new();
        if s0.tick != original.samples.first().map_or(s0.tick, |f| f.tick) {
            notes.push(
                "note: the init: state belongs to the first sample, not the start tick".to_owned(),
            );
        }
        if let Some(n) = init.max_grapples {
            grapple_gun::set_max_grapples(p, n);
            applied.push(format!("max_grapples {n}"));
        }
        if let Some(n) = init.times_grappled {
            p.script.gun.times_grappled = n;
            applied.push(format!("times_grappled {n}"));
        }
        if let Some(on) = init.rocket_boots {
            rocket_boots::enable_rocket_boots(p, on);
            applied.push(format!("rocket_boots {on}"));
        }
        if !applied.is_empty() {
            notes.push(format!("init applied: {}", applied.join(", ")));
        }
    }
    if let Some(n) = opts.max_grapples {
        grapple_gun::set_max_grapples(game.player_mut(), n);
        notes.push(format!("max_grapples override {n}"));
    }

    game.start();
    game.start_recording();
    let respawns_before = game.respawn_count();
    let mut stopped_at_gap = None;
    let mut prev_tick = s0.tick;
    for s in samples.iter().skip(1) {
        if prev_tick.checked_add(1) != Some(s.tick) {
            stopped_at_gap = Some(prev_tick);
            break;
        }
        let ticked = match script.as_mut() {
            Some(sc) => sc.tick(&mut game, &s.input).is_some(),
            None => game.tick(&s.input).is_some(),
        };
        if !ticked {
            bail!("the game did not tick at input tick {}", s.tick);
        }
        prev_tick = s.tick;
    }
    let respawns = game.respawn_count().saturating_sub(respawns_before);
    let mut trace = game.stop_recording().context("recording was not running")?;
    // Our ticks count from 0; give them the input trace's numbers.
    let base = s0.tick;
    for s in &mut trace.samples {
        s.tick = s.tick.checked_add(base).context("tick numbers overflow")?;
    }
    if let Some(t) = stopped_at_gap {
        notes.push(format!("stopped at the tick gap after input tick {t}"));
    }
    if respawns > 0 {
        notes.push(format!("{respawns} respawn(s) during the replay"));
    }
    trace.meta.notes.extend(notes);
    trace.validate()?;
    Ok(ReplayResult {
        trace,
        stopped_at_gap,
        respawns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_player::InputFrame;
    use asamu_player::trace::compare;

    fn scripted_inputs(n: usize) -> Vec<InputFrame> {
        (0..n)
            .map(|i| InputFrame {
                move_forward: if i < 50 { 1.0 } else { 0.0 },
                move_right: if (20..40).contains(&i) { -1.0 } else { 0.0 },
                look_yaw_delta: if i % 7 == 0 { 0.01 } else { 0.0 },
                look_pitch_delta: if i % 11 == 0 { -0.005 } else { 0.0 },
                jump_pressed: i == 30,
                jump_held: (30..45).contains(&i),
                sprint_held: (5..25).contains(&i),
                grapple_held: (60..80).contains(&i),
                ..InputFrame::default()
            })
            .collect()
    }

    /// A runtime recording on the graybox.
    fn runtime_trace(n: usize) -> Trace {
        let mut g = Game::graybox().unwrap();
        g.start();
        g.start_recording();
        for i in scripted_inputs(n) {
            g.tick(&i).unwrap();
        }
        g.stop_recording().unwrap()
    }

    #[test]
    fn replay_reproduces_a_runtime_recording_exactly() {
        let t = runtime_trace(120);
        let r = replay(&t, &ReplayOptions::default()).unwrap();
        assert_eq!(r.stopped_at_gap, None);
        assert_eq!(r.trace.samples.len(), t.samples.len());
        let d = compare(&t, &r.trace);
        assert!(d.is_exact(), "{d:?}");
        assert!(r.trace.meta.notes.iter().any(|n| n.contains("kept")));
    }

    #[test]
    fn replay_is_deterministic_and_stops_at_gaps() {
        let mut t = runtime_trace(90);
        // Teleport the start so the replay overwrites the spawn state.
        for s in &mut t.samples {
            s.position.z += 120.0;
            s.tick += 1000;
        }
        t.samples[0].grounded = false;
        let a = replay(&t, &ReplayOptions::default()).unwrap();
        let b = replay(&t, &ReplayOptions::default()).unwrap();
        assert_eq!(
            a.trace.to_jsonl_string().unwrap(),
            b.trace.to_jsonl_string().unwrap()
        );
        assert_eq!(a.trace.samples[0].tick, 1000);
        assert_eq!(a.trace.samples[0].position, t.samples[0].position);
        assert!(!a.trace.samples[0].grounded);
        assert!(
            a.trace.samples[1].position.z < t.samples[0].position.z,
            "falls"
        );

        let mid = replay(
            &t,
            &ReplayOptions {
                start_tick: Some(1050),
                max_ticks: Some(20),
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        assert_eq!(mid.trace.samples.len(), 21);
        assert_eq!(mid.trace.samples[0].tick, 1050);
        assert_eq!(mid.trace.samples[0].position, t.samples[50].position);
        assert!(
            replay(
                &t,
                &ReplayOptions {
                    start_tick: Some(5),
                    ..ReplayOptions::default()
                }
            )
            .is_err()
        );

        t.samples.remove(40);
        let g = replay(&t, &ReplayOptions::default()).unwrap();
        assert_eq!(g.stopped_at_gap, Some(1039));
        assert_eq!(g.trace.samples.len(), 40);
        assert_eq!(g.trace.samples.last().unwrap().tick, 1039);
    }

    #[test]
    fn init_note_and_options() {
        let mut t = runtime_trace(10);
        t.meta.notes.push(
            "init: {\"air_control\":0.3,\"base\":null,\"ground_speed\":440.0,\"jump_z\":1000.0,\
             \"max_grapples\":3,\"physics\":1,\"rocket_boots\":true,\"sprinting\":false,\
             \"times_grappled\":1}"
                .to_owned(),
        );
        let r = replay(&t, &ReplayOptions::default()).unwrap();
        assert!(
            r.trace
                .meta
                .notes
                .iter()
                .any(|n| n == "init applied: max_grapples 3, times_grappled 1, rocket_boots true")
        );
        let no = replay(
            &t,
            &ReplayOptions {
                use_init: false,
                max_grapples: Some(-1),
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        assert!(
            !no.trace
                .meta
                .notes
                .iter()
                .any(|n| n.starts_with("init applied"))
        );
        assert!(
            no.trace
                .meta
                .notes
                .iter()
                .any(|n| n == "max_grapples override -1")
        );

        let mut v = t.clone();
        v.meta.tick_rate = None;
        assert!(replay(&v, &ReplayOptions::default()).is_err());
        let r30 = replay(
            &v,
            &ReplayOptions {
                tick_rate: Some(30.0),
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        assert_eq!(r30.trace.meta.tick_rate, Some(30.0));
        let p = replay(
            &t,
            &ReplayOptions {
                placeholder: true,
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        assert!(p.trace.meta.notes.iter().any(|n| n.contains("placeholder")));
        let empty = Trace::new(t.meta.clone());
        assert!(replay(&empty, &ReplayOptions::default()).is_err());
        let k = ReplayOptions {
            level: ReplayLevel::Converted {
                dir: PathBuf::from("/nonexistent"),
                map: "AG-Workshop".into(),
                kismet: true,
            },
            tick_rate: Some(30.0),
            ..ReplayOptions::default()
        };
        assert!(replay(&t, &k).is_err());
    }
}
