//! The grapple's anchor helper on moving and turning rocks, measured on a
//! raw recording of the original (GRAPPLE.md G-AT-7 and G-AT-8;
//! KISMET_RUNTIME.md §6, "Turning actors").
//!
//! Gated: the test reads the keyboard recording `*KM1*.raw.jsonl`
//! (docs/TRACE_CAPTURE.md) in the directory `ASAMU_TRACE_RAW_DIR` names and
//! is skipped without it. Recordings never enter the repository; nothing
//! recorded is in this file.
//!
//! ```sh
//! ASAMU_TRACE_RAW_DIR=<recordings> cargo test -p asamu-player --test parity_moving_anchor_recorded -- --nocapture
//! ```
//!
//! The recorder reads two things of the gun every frame: its anchor, and the
//! location of its helper actor, which the gun bases on an `InterpActor` it
//! attaches to and which the engine then moves with that actor. So the
//! helper's recorded path is the motion the original gives an actor based on
//! a moving base, and here it is compared with the rule:
//!
//! - an anchor on an `InterpActor` is the helper's location in every frame;
//! - on a rock that turns the helper goes around a fixed axis on a circle;
//! - a rock in the rotating physics turns each frame by its `RotationRate`
//!   times the frame's time with the fraction dropped, and the helper is
//!   where [`carried_by_base`] takes it.
//!
//! The counts in the comments are those of the recording of 2026-10-12
//! (36,993 records in AG-ParadiseCave); the test prints its own. The level's
//! own numbers used here are two `RotationRate`s, named where they are used.
//! The tolerances are the recorded path's own scatter about its circle and
//! the sine table's step; the one bound of ours is on that scatter itself,
//! and is derived where it is used.

use std::path::PathBuf;

use asamu_player::grapple_gun::{BasePose, carried_by_base};
use glam::{DVec3, Vec3};
use serde_json::Value;

/// Rotator units in a radian.
const UNITS_PER_RADIAN: f64 = 65_536.0 / std::f64::consts::TAU;

/// What the test needs of one record of the level.
struct Sample {
    frame: i64,
    /// The frame's time, s (the world's delta seconds).
    dt: f32,
    attached: bool,
    /// The helper actor's location.
    helper: DVec3,
    /// The gun's anchor.
    anchor: DVec3,
}

fn vector(value: &Value) -> Option<DVec3> {
    let c = |i: usize| value.get(i)?.as_f64();
    Some(DVec3::new(c(0)?, c(1)?, c(2)?))
}

fn sample(line: &str) -> Option<Sample> {
    let record: Value = serde_json::from_str(line).ok()?;
    let world = record.get("world")?;
    if !world
        .get("map")?
        .as_str()?
        .eq_ignore_ascii_case("AG-ParadiseCave")
    {
        return None;
    }
    let gun = record.get("player")?.get("gun")?;
    Some(Sample {
        frame: record.get("frame")?.as_i64()?,
        dt: world.get("delta_seconds")?.as_f64()? as f32,
        attached: gun.get("grappling")?.as_bool()?,
        helper: vector(gun.get("anchor")?)?,
        anchor: vector(gun.get("grapple_location")?)?,
    })
}

fn recording() -> Option<Vec<Sample>> {
    let dir = PathBuf::from(std::env::var_os("ASAMU_TRACE_RAW_DIR")?);
    let path = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains("KM1") && n.ends_with(".raw.jsonl"))
        })?;
    let text = std::fs::read_to_string(path).ok()?;
    let samples: Vec<Sample> = text.lines().filter_map(sample).collect();
    (!samples.is_empty()).then_some(samples)
}

/// Eigenvectors of a symmetric 3×3 matrix by Jacobi rotations, the one of
/// the largest eigenvalue first.
#[allow(clippy::needless_range_loop)]
fn eigenvectors(mut a: [[f64; 3]; 3]) -> [DVec3; 3] {
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..64 {
        for (p, q) in [(0, 1), (0, 2), (1, 2)] {
            if a[p][q].abs() < f64::MIN_POSITIVE {
                continue;
            }
            let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
            let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            for row in &mut a {
                let (x, y) = (row[p], row[q]);
                row[p] = c * x - s * y;
                row[q] = s * x + c * y;
            }
            let (rp, rq) = (a[p], a[q]);
            for k in 0..3 {
                a[p][k] = c * rp[k] - s * rq[k];
                a[q][k] = s * rp[k] + c * rq[k];
            }
            for row in &mut v {
                let (x, y) = (row[p], row[q]);
                row[p] = c * x - s * y;
                row[q] = s * x + c * y;
            }
        }
    }
    let mut order = [0, 1, 2];
    order.sort_by(|i, j| a[*j][*j].total_cmp(&a[*i][*i]));
    order.map(|i| DVec3::new(v[0][i], v[1][i], v[2][i]))
}

/// The circle that fits a path best: the plane of the path's two largest
/// principal directions, and in it the least-squares circle.
struct Circle {
    /// The circle's centre (a point of the turning axis).
    centre: DVec3,
    /// The turning axis, oriented so that the path turns about it by the
    /// right-hand rule.
    axis: DVec3,
    radius: f64,
    /// Largest distance of a point of the path from the circle's cylinder.
    radial: f64,
    /// Largest distance of a point of the path from the circle's plane.
    axial: f64,
    /// The angle of every point of the path from the first, unwrapped, in
    /// rotator units (growing).
    turned: Vec<f64>,
}

#[allow(clippy::needless_range_loop)]
fn fit_circle(path: &[DVec3]) -> Option<Circle> {
    let n = path.len() as f64;
    let mean = path.iter().sum::<DVec3>() / n;
    let mut cov = [[0.0; 3]; 3];
    for p in path {
        let d = (*p - mean).to_array();
        for i in 0..3 {
            for j in 0..3 {
                cov[i][j] += d[i] * d[j];
            }
        }
    }
    let [u, v, w] = eigenvectors(cov);
    // 2·cx·x + 2·cy·y + k = x² + y², least squares (normal equations).
    let mut ata = [[0.0; 3]; 3];
    let mut atb = [0.0; 3];
    let flat: Vec<(f64, f64)> = path
        .iter()
        .map(|p| ((*p - mean).dot(u), (*p - mean).dot(v)))
        .collect();
    for (x, y) in &flat {
        let row = [2.0 * x, 2.0 * y, 1.0];
        let b = x * x + y * y;
        for i in 0..3 {
            atb[i] += row[i] * b;
            for j in 0..3 {
                ata[i][j] += row[i] * row[j];
            }
        }
    }
    let m = glam::DMat3::from_cols_array_2d(&ata);
    if m.determinant().abs() < f64::MIN_POSITIVE {
        return None;
    }
    let solution = m.inverse() * DVec3::from_array(atb);
    let (cx, cy) = (solution.x, solution.y);
    let radius = (solution.z + cx * cx + cy * cy).max(0.0).sqrt();
    let centre = mean + u * cx + v * cy;
    let mut angles: Vec<f64> = flat.iter().map(|(x, y)| (y - cy).atan2(x - cx)).collect();
    for i in 1..angles.len() {
        while angles[i] - angles[i - 1] > std::f64::consts::PI {
            angles[i] -= std::f64::consts::TAU;
        }
        while angles[i] - angles[i - 1] < -std::f64::consts::PI {
            angles[i] += std::f64::consts::TAU;
        }
    }
    let forwards = angles.last()? >= angles.first()?;
    let axis = if forwards { u.cross(v) } else { -u.cross(v) };
    let first = *angles.first()?;
    let turned = angles
        .iter()
        .map(|a| (a - first).abs() * UNITS_PER_RADIAN)
        .collect();
    let radial = flat
        .iter()
        .map(|(x, y)| ((x - cx).hypot(y - cy) - radius).abs())
        .fold(0.0, f64::max);
    let axial = path
        .iter()
        .map(|p| (*p - mean).dot(w).abs())
        .fold(0.0, f64::max);
    Some(Circle {
        centre,
        axis,
        radius,
        radial,
        axial,
        turned,
    })
}

/// The step of a frame of `dt` seconds at `rate` units/s: the product in
/// single precision, its fraction dropped.
fn step(rate: i32, dt: f32) -> i32 {
    (rate as f32 * dt) as i32
}

#[test]
fn the_recorded_helper_rides_its_rock_rigidly() {
    let Some(samples) = recording() else {
        eprintln!("SKIP: ASAMU_TRACE_RAW_DIR with the recording KM1 not set");
        return;
    };
    // --- Attachments: maximal runs of attached frames.
    let mut attachments: Vec<(usize, usize)> = Vec::new();
    for (i, s) in samples.iter().enumerate() {
        if !s.attached {
            continue;
        }
        match attachments.last_mut() {
            Some(last) if last.1 + 1 == i => last.1 = i,
            _ => attachments.push((i, i)),
        }
    }
    // G-AT-7/8: on an `InterpActor` the anchor is the helper's location in
    // every frame; on anything else the helper is left where it is, far
    // away. (KM1: 13 and 53 of 66.)
    let riding: Vec<usize> = attachments
        .iter()
        .filter(|(a, b)| samples[*a..=*b].iter().all(|s| s.helper == s.anchor))
        .map(|(a, _)| *a)
        .collect();
    let mut nearest = f64::INFINITY;
    for (a, b) in &attachments {
        if riding.contains(a) {
            continue;
        }
        // Never a mixture: the anchor is on the helper for a whole
        // attachment or in none of its frames.
        for s in &samples[*a..=*b] {
            assert_ne!(s.helper, s.anchor, "frame {}", s.frame);
            nearest = nearest.min((s.helper - s.anchor).length());
        }
    }
    eprintln!(
        "{} attachments, {} with the anchor on the helper; in the others the helper is at \
         least {nearest:.0} uu from the anchor",
        attachments.len(),
        riding.len()
    );
    assert!(riding.len() >= 10, "the recording has the wheels");

    // --- Stretches: from one attach on an `InterpActor` to the next (the
    // helper keeps riding after the release).
    let mut circles = 0;
    let mut frames = 0;
    let mut steps_on_the_table = (0usize, 0usize);
    let mut truncated = 0;
    for (k, start) in riding.iter().enumerate() {
        let end = riding.get(k + 1).map_or(samples.len(), |next| *next);
        let stretch = &samples[*start..end];
        let path: Vec<DVec3> = stretch.iter().map(|s| s.helper).collect();
        let length: f64 = path.windows(2).map(|w| (w[1] - w[0]).length()).sum();
        let Some(circle) = fit_circle(&path) else {
            continue;
        };
        // A lift or a bobbing platform, not a wheel: its path is a line
        // (the best circle is far larger than the level) or short.
        if circle.radius > 100_000.0 || length < 5000.0 {
            eprintln!(
                "frame {}: {} frames, {length:.0} uu of path, not a turn",
                stretch[0].frame,
                stretch.len()
            );
            continue;
        }
        circles += 1;
        frames += stretch.len();
        let turned = *circle.turned.last().expect("a path has an end");
        // The scatter of the recorded path about its own circle.
        let scatter = circle.radial + circle.axial;
        eprintln!(
            "frame {}: {} frames, radius {:.1} uu, {:.2} turns, up to {:.3} uu off the \
             cylinder and {:.3} uu off the plane, axis ({:.4}, {:.4}, {:.4})",
            stretch[0].frame,
            stretch.len(),
            circle.radius,
            turned / 65_536.0,
            circle.radial,
            circle.axial,
            circle.axis.x,
            circle.axis.y,
            circle.axis.z
        );
        // A circle: the helper stays within 3 uu of it on radii of 2,000 to
        // 5,000 uu over up to 4.5 turns (KM1: at most 1.35 uu off the
        // cylinder; the bound is the size of single-precision rounding at
        // 150,000 uu, 1/64 uu a frame, added up over a turn of 560 to 1,120
        // frames in a random walk: √1,120 / 64 × a handful of operations).
        assert!(
            circle.radial < 3.0 && circle.axial < 3.0,
            "frame {}: not a circle ({} and {} uu off)",
            stretch[0].frame,
            circle.radial,
            circle.axial
        );
        // The sine table's steps: the helper's angle changes by whole
        // multiples of 4 units from frame to frame.
        let single: Vec<(f64, &Sample)> = (1..stretch.len())
            .filter(|i| stretch[*i].frame == stretch[i - 1].frame + 1)
            .map(|i| (circle.turned[i] - circle.turned[i - 1], &stretch[i]))
            .collect();
        steps_on_the_table.1 += single.len();
        steps_on_the_table.0 += single
            .iter()
            .filter(|(s, _)| (s - 4.0 * (s / 4.0).round()).abs() <= 0.5)
            .count();

        // --- A rock in the rotating physics: the frame's step is the rate
        // times the frame's time, truncated. The level's two rates of such
        // rocks on the route: 9102 units/s of yaw (`InterpActor_51`, bound
        // to no Matinee) and 4551 units/s of roll (`InterpActor_25` and
        // `_101`, which a Matinee group names). A rock that follows a
        // Matinee track of one turn instead runs ahead of these sums (by
        // 37 to 735 units on KM1's stretches).
        let rate_of = |units: f64| {
            let per_second = units / f64::from(stretch.iter().skip(1).map(|s| s.dt).sum::<f32>());
            [9102, 4551]
                .into_iter()
                .find(|r| (per_second - f64::from(*r)).abs() < 0.015 * f64::from(*r))
        };
        let Some(rate) = rate_of(turned) else {
            continue;
        };
        let complete = stretch.windows(2).all(|w| w[1].frame == w[0].frame + 1);
        let mut sum = 0i64;
        let mut worst = 0.0_f64;
        for (i, s) in stretch.iter().enumerate().skip(1) {
            sum += i64::from(step(rate, s.dt));
            worst = worst.max((circle.turned[i] - sum as f64).abs());
        }
        // What the path can tell of an angle: its scatter on its radius,
        // and the table's step of 4 units.
        let resolution = 2.0 * scatter / circle.radius * UNITS_PER_RADIAN + 4.0;
        let exact = f64::from(rate) * f64::from(stretch.iter().skip(1).map(|s| s.dt).sum::<f32>());
        eprintln!(
            "    rate {rate}: turned {turned:.1} units; the truncated steps add up to {sum} \
             (at most {worst:.1} apart, resolution {resolution:.1}); rate × time is {exact:.1}"
        );
        if !complete || worst > resolution {
            // Follows a track (or a frame is missing): not this rule.
            continue;
        }
        assert!(
            exact - turned > 10.0 * resolution,
            "the stretch does not tell truncated steps from the rate itself"
        );
        truncated += 1;
        if rate != 9102 {
            continue;
        }
        // --- The helper's place, frame by frame, by our rule: the rock
        // turns about the vertical through its location by the truncated
        // step of its yaw rate (a negative one: the path turns about −Z),
        // and the helper is carried. Any point of the axis serves as the
        // rock's location for a turn in place.
        assert!(circle.axis.z < -0.999, "a yaw: {:?}", circle.axis);
        let location = circle.centre.as_vec3();
        let mut yaw = 0i32;
        let mut helper: Vec3 = stretch[0].helper.as_vec3();
        let mut furthest = 0.0_f64;
        for s in stretch.iter().skip(1) {
            let next = yaw - step(rate, s.dt);
            helper = carried_by_base(
                helper,
                BasePose {
                    location,
                    rotation: [0, yaw, 0],
                },
                BasePose {
                    location,
                    rotation: [0, next, 0],
                },
            );
            yaw = next;
            furthest = furthest.max((helper.as_dvec3() - s.helper).length());
        }
        // Within the recorded path's own scatter about a circle, twice (ours
        // is on the circle; the recorded helper wanders about it), and the
        // table's step on this radius.
        let room = 2.0 * scatter + 4.0 / UNITS_PER_RADIAN * circle.radius;
        eprintln!(
            "    carried by our rule over {} frames: at most {furthest:.3} uu from the \
             recorded helper (room {room:.3})",
            stretch.len() - 1
        );
        assert!(
            furthest <= room,
            "the recorded helper is {furthest} uu from where the rule carries it"
        );
    }
    eprintln!(
        "{circles} stretches on turning rocks, {frames} frames; {} of {} single-frame steps \
         on the sine table's steps; {truncated} stretches turn by truncated steps",
        steps_on_the_table.0, steps_on_the_table.1
    );
    // KM1: 10 stretches (15,704 frames), 15,407 of 15,693 steps, 2 by the
    // rate (within 12.9 and 3.5 units of the truncated steps over 261,135
    // and 105,968, where rate × time is 1,554 and 639 ahead); our rule
    // carries the helper to within 3.9 uu of the recorded one over 2,251
    // frames on a radius of 2,106 uu.
    assert!(circles >= 8, "{circles}");
    assert!(
        steps_on_the_table.0 * 100 >= steps_on_the_table.1 * 95,
        "{steps_on_the_table:?}"
    );
    assert!(truncated >= 2, "{truncated}");
}
