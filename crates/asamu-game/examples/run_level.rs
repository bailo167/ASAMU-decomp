//! Loads a level converted from the user's own install and walks the player
//! forward, headless (no rendering): prints position, velocity and grounded
//! state once per simulated second, world events as they happen, and the
//! tick timing.
//!
//! ```sh
//! asamu-import --out <dir> levels
//! asamu-import --out <dir> meshes --collision
//! cargo run --release -p asamu-game --example run_level -- --converted <dir> --map AG-Workshop --ticks 600
//! ```
//!
//! Options: `--converted DIR` (required), `--map NAME` (default
//! `AG-Workshop`), `--ticks N` (default 600), `--sprint`, `--jump-every N`
//! (ticks; 0 = never, the default), `--yaw DEGREES` (turn before walking).

use std::process::ExitCode;
use std::time::Instant;

use asamu_game::Game;
use asamu_player::InputFrame;

struct Args {
    converted: String,
    map: String,
    ticks: u64,
    sprint: bool,
    jump_every: u64,
    yaw: f32,
}

fn parse() -> Result<Args, String> {
    let mut args = Args {
        converted: String::new(),
        map: "AG-Workshop".to_owned(),
        ticks: 600,
        sprint: false,
        jump_every: 0,
        yaw: 0.0,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--converted" => args.converted = value("--converted")?,
            "--map" => args.map = value("--map")?,
            "--ticks" => {
                args.ticks = value("--ticks")?
                    .parse()
                    .map_err(|e| format!("--ticks: {e}"))?
            }
            "--jump-every" => {
                args.jump_every = value("--jump-every")?
                    .parse()
                    .map_err(|e| format!("--jump-every: {e}"))?;
            }
            "--yaw" => args.yaw = value("--yaw")?.parse().map_err(|e| format!("--yaw: {e}"))?,
            "--sprint" => args.sprint = true,
            "-h" | "--help" => {
                return Err("usage: run_level --converted DIR [--map NAME] [--ticks N] [--sprint] [--jump-every N] [--yaw DEG]".to_owned());
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if args.converted.is_empty() {
        return Err("--converted DIR is required (the --out directory of asamu-import)".to_owned());
    }
    Ok(args)
}

fn main() -> ExitCode {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let load = Instant::now();
    let mut game = match Game::load_level(&args.converted, &args.map) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let load_time = load.elapsed();
    if let Some(map) = game.scene_map() {
        let s = map.collision.stats();
        println!(
            "{} ({}): {} levels, {} actors, {} meshes, {} instances, {} world triangles, {} dynamic; loaded in {:.0?}",
            map.map,
            map.world.title.as_deref().unwrap_or("-"),
            map.levels.len(),
            map.stats.actors,
            s.meshes,
            s.instances,
            s.instance_triangles,
            map.dynamic.len(),
            load_time
        );
        println!(
            "KillZ {:e}; {} checkpoints, {} kill/trigger volumes, {} triggers, {} crystals, {} rocks; {} warnings",
            map.world.kill_z,
            map.actors.checkpoints.len(),
            map.actors.volumes.len(),
            map.actors.triggers.len(),
            map.actors.crystals.len(),
            map.actors.rocks.len(),
            map.warnings.len()
        );
    }
    let p = game.player();
    println!(
        "spawn {:.1?} yaw {:.1}° grounded {} | grapples {} boots {} story {}",
        p.position.to_array(),
        p.yaw.to_degrees(),
        p.grounded,
        p.script.gun.max_grapples,
        p.script.boots.enabled,
        game.in_story_mode()
    );
    game.start();
    let rate = game.clock().tick_rate_hz().round().max(1.0) as u64;
    let mut times: Vec<f64> = Vec::with_capacity(args.ticks as usize);
    for i in 0..args.ticks {
        let mut input = InputFrame {
            move_forward: 1.0,
            sprint_held: args.sprint,
            ..InputFrame::default()
        };
        if i == 0 {
            input.look_yaw_delta = args.yaw.to_radians();
        }
        if args.jump_every > 0 && i % args.jump_every == args.jump_every - 1 {
            input.jump_pressed = true;
            input.jump_held = true;
        }
        let t0 = Instant::now();
        let Some(report) = game.tick(&input) else {
            break;
        };
        times.push(t0.elapsed().as_secs_f64());
        for e in report.world.iter() {
            println!("  tick {:>5}: {e:?}", report.tick);
        }
        if let Some(l) = report.events.landing
            && l.hard
        {
            println!(
                "  tick {:>5}: hard landing at {:.0} uu/s",
                report.tick, l.velocity_z
            );
        }
        if report.tick % rate == 0 {
            let p = game.player();
            println!(
                "t={:>5.1}s pos ({:>10.1}, {:>10.1}, {:>9.1}) vel ({:>7.1}, {:>7.1}, {:>7.1}) grounded {:<5} speed {:>6.1}{}",
                game.clock().time_seconds(),
                p.position.x,
                p.position.y,
                p.position.z,
                p.velocity.x,
                p.velocity.y,
                p.velocity.z,
                p.grounded,
                p.horizontal_speed(),
                if game.is_dying() { " (dying)" } else { "" }
            );
        }
    }
    if !times.is_empty() {
        let mut sorted = times.clone();
        sorted.sort_by(f64::total_cmp);
        let avg = times.iter().sum::<f64>() / times.len() as f64;
        let p99 = sorted[(sorted.len() * 99 / 100).min(sorted.len() - 1)];
        let max = sorted[sorted.len() - 1];
        println!(
            "{} ticks: average {:.1} us, p99 {:.1} us, max {:.1} us per tick; {} respawns",
            times.len(),
            avg * 1e6,
            p99 * 1e6,
            max * 1e6,
            game.respawn_count()
        );
    }
    ExitCode::SUCCESS
}
