//! The Sandbox's command-line rules.
//!
//! `main.rs` parses `--sandbox`, `--arena ID` and `--sandbox-profile NAME`
//! into `SandboxArgs` in every build; [`check`] decides which combinations
//! make sense. It runs at the end of `parse_cli`, before a window opens.

use asamu_sandbox::arena::builtin_arenas;
use asamu_sandbox::profile::valid_profile_name;

use crate::{Cli, Config};

/// Checks the Sandbox options against the rest of the command line.
///
/// Refused:
/// - `--arena` or `--sandbox-profile` without `--sandbox`;
/// - `--sandbox` with the placeholder configuration (`--placeholder` or
///   `ASAMU_MOVEMENT=placeholder`: a session tunes the Classic set), with
///   `--fly` (no simulation) or with `--walk` (an unattended Classic check);
/// - `--arena` together with converted data (`--converted` or `--level`):
///   arenas are hand-made and run in the graybox composition;
/// - `--sandbox --screenshot` without `--screenshot-window`: the offscreen
///   screenshot renders no UI, so it would lack the Sandbox watermark;
/// - an `--arena` that is not a built-in arena, and a `--sandbox-profile`
///   that cannot be a profile name. (Whether a saved profile of that name
///   exists is only known once the Sandbox looks for it; it says so then.)
/// - `--sandbox` while the environment variable [`MENU_ACTION_ENV`] is set:
///   that variable drives the Classic main menu unattended, and a `--sandbox`
///   process has no Classic menu (a Classic game would start behind the
///   launcher).
pub(crate) fn check(cli: &Cli) -> Result<(), String> {
    // The tests pass the environment in (`check_with`), so they do not
    // depend on the variables of the machine that runs them.
    let menu_action = !cfg!(test) && std::env::var_os(MENU_ACTION_ENV).is_some();
    check_with(cli, menu_action)
}

/// The variable `ui::setup` reads to drive the Classic main menu in
/// unattended checks.
const MENU_ACTION_ENV: &str = "ASAMU_MENU_ACTION";

/// [`check`], with the environment passed in (`menu_action`: the variable
/// [`MENU_ACTION_ENV`] is set).
fn check_with(cli: &Cli, menu_action: bool) -> Result<(), String> {
    let sandbox = &cli.sandbox;
    if !sandbox.enabled {
        if sandbox.arena.is_some() {
            return Err("--arena needs --sandbox".to_owned());
        }
        if sandbox.profile.is_some() {
            return Err("--sandbox-profile needs --sandbox".to_owned());
        }
        return Ok(());
    }
    if cli.config == Config::Placeholder {
        return Err(
            "--sandbox tunes the Classic parameter set: it cannot run the placeholder \
             configuration (--placeholder, ASAMU_MOVEMENT=placeholder)"
                .to_owned(),
        );
    }
    if cli.fly {
        return Err("--sandbox needs the simulation: it cannot be combined with --fly".to_owned());
    }
    if cli.walk > 0.0 {
        return Err(
            "--walk is an unattended Classic check: it cannot be combined with --sandbox"
                .to_owned(),
        );
    }
    if menu_action {
        return Err(format!(
            "{MENU_ACTION_ENV} drives the Classic main menu: it cannot be combined with \
             --sandbox (unset it)"
        ));
    }
    if sandbox.arena.is_some() && (cli.converted.is_some() || cli.level.is_some()) {
        return Err(
            "--arena starts a hand-made arena: it cannot be combined with converted data \
             (--converted, --level)"
                .to_owned(),
        );
    }
    if cli.screenshot.is_some() && !cli.screenshot_window {
        return Err(
            "--sandbox --screenshot needs --screenshot-window: the offscreen screenshot has no \
             UI, so it would lack the Sandbox watermark"
                .to_owned(),
        );
    }
    if let Some(id) = &sandbox.arena
        && !builtin_arenas().iter().any(|arena| arena.id == id)
    {
        let known: Vec<&str> = builtin_arenas().iter().map(|arena| arena.id).collect();
        return Err(format!(
            "--arena: there is no arena {id:?} (built in: {})",
            known.join(", ")
        ));
    }
    if let Some(name) = &sandbox.profile
        && !valid_profile_name(name)
    {
        return Err(format!(
            "--sandbox-profile: {name:?} is not a profile name (1 to 40 characters of a-z, \
             0-9, _ and -)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use asamu_sandbox::arena::builtin_arenas;

    use crate::{Cli, SandboxArgs, parse_cli};

    fn parse_env(list: &[&str], movement_env: Option<&str>) -> Result<Cli, String> {
        parse_cli(list.iter().map(|s| (*s).to_owned()), movement_env)
            .map(|cli| cli.expect("no --help in these tests"))
    }

    fn parse(list: &[&str]) -> Result<Cli, String> {
        parse_env(list, None)
    }

    #[test]
    fn sandbox_flags_default_off_and_parse() {
        assert_eq!(Cli::default().sandbox, SandboxArgs::default());
        let off = SandboxArgs::default();
        assert!(!off.enabled && off.arena.is_none() && off.profile.is_none());
        assert_eq!(parse(&[]).unwrap().sandbox, SandboxArgs::default());

        let cli = parse(&["--sandbox"]).unwrap();
        assert_eq!(
            cli.sandbox,
            SandboxArgs {
                enabled: true,
                arena: None,
                profile: None
            }
        );
        let cli = parse(&[
            "--sandbox",
            "--arena",
            "movement-lab",
            "--sandbox-profile",
            "floaty",
        ])
        .unwrap();
        assert_eq!(
            cli.sandbox,
            SandboxArgs {
                enabled: true,
                arena: Some("movement-lab".to_owned()),
                profile: Some("floaty".to_owned()),
            }
        );
        // The Sandbox on converted data, on one level, and with a window
        // screenshot.
        assert!(parse(&["--sandbox", "--converted", "/conv"]).is_ok());
        assert!(parse(&["--sandbox", "--level", "X", "--sandbox-profile", "moon"]).is_ok());
        assert!(parse(&["--sandbox", "--no-menu"]).is_ok());
        assert!(
            parse(&[
                "--sandbox",
                "--screenshot",
                "/tmp/shot.png",
                "--screenshot-window"
            ])
            .is_ok()
        );
        // Original parameters on the placeholder model keep the Classic set.
        assert!(parse(&["--sandbox", "--placeholder-movement"]).is_ok());
        // Every built-in arena and profile is accepted by name.
        for arena in builtin_arenas() {
            assert!(
                parse(&["--sandbox", "--arena", arena.id]).is_ok(),
                "{}",
                arena.id
            );
        }
        for profile in asamu_sandbox::profile::Profile::builtin() {
            assert!(
                parse(&["--sandbox", "--sandbox-profile", &profile.name]).is_ok(),
                "{}",
                profile.name
            );
        }
    }

    #[test]
    fn bad_combinations_are_errors() {
        for bad in [
            // Sandbox options without the Sandbox.
            &["--arena", "movement-lab"][..],
            &["--sandbox-profile", "floaty"],
            // Missing values.
            &["--sandbox", "--arena"],
            &["--sandbox", "--sandbox-profile"],
            // Not the Classic set, no simulation, unattended Classic check.
            &["--sandbox", "--placeholder"],
            &["--sandbox", "--converted", "/conv", "--fly"],
            &["--sandbox", "--walk", "2"],
            // Arenas are hand-made: not with converted data.
            &[
                "--sandbox",
                "--arena",
                "movement-lab",
                "--converted",
                "/conv",
            ],
            &["--sandbox", "--arena", "movement-lab", "--level", "X"],
            // The offscreen screenshot has no watermark.
            &["--sandbox", "--screenshot", "/tmp/shot.png"],
            // Not an arena; not a profile name.
            &["--sandbox", "--arena", "nowhere"],
            &["--sandbox", "--sandbox-profile", "Not A Name"],
            &["--sandbox", "--sandbox-profile", "../escape"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        assert!(parse_env(&["--sandbox"], Some("placeholder")).is_err());
        // The variable that drives the Classic main menu unattended: not in
        // a `--sandbox` process, which has no Classic menu. Without
        // `--sandbox` it is Classic's own business.
        let sandbox = parse(&["--sandbox"]).unwrap();
        assert!(super::check_with(&sandbox, false).is_ok());
        let refused = super::check_with(&sandbox, true).unwrap_err();
        assert!(refused.contains(super::MENU_ACTION_ENV), "{refused}");
        assert!(super::check_with(&parse(&[]).unwrap(), true).is_ok());
        // The same options without --sandbox are Classic's and stay valid.
        assert!(parse(&["--placeholder"]).is_ok());
        assert!(parse(&["--walk", "2"]).is_ok());
        assert!(parse(&["--screenshot", "/tmp/shot.png"]).is_ok());
    }
}
