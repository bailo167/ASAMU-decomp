//! The Sandbox's boundary, checked by reading the repository's own text.
//!
//! The Sandbox must stay *on top of* Classic and never get under it:
//!
//! - no Classic crate and no parity tool depends on the Sandbox crate;
//! - adding the Sandbox crate to a build changes nothing in how the Classic
//!   crates are compiled (it adds no dependency and no dependency feature);
//! - nothing outside the app is compiled conditionally on the Sandbox;
//! - the one tool door into a running game (`Game::set_params`) is used by
//!   the Sandbox alone;
//! - inside the app's Sandbox plugin a single file may write the simulation
//!   or the simulation's clocks.
//!
//! These are scans of source text from `CARGO_MANIFEST_DIR`, not a build
//! graph query: cheap, dependency-free and good enough to make the wrong
//! thing fail loudly in CI. Each scan first proves it can see what it is
//! looking for.

use std::path::{Path, PathBuf};

/// The repository root (this crate lives in `crates/asamu-sandbox`).
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()))
}

/// The path of `file` relative to the repository root, with `/` separators.
fn relative(file: &Path) -> String {
    let root = root().canonicalize().expect("the repository root exists");
    let file = file
        .canonicalize()
        .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    file.strip_prefix(&root)
        .unwrap_or(&file)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Every file under `dir` with one of the `extensions`, recursively, not
/// descending into build output or hidden directories.
fn files_under(dir: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let entries = std::fs::read_dir(&next)
            .unwrap_or_else(|e| panic!("{} is a readable directory: {e}", next.display()));
        for entry in entries {
            let entry = entry.expect("a directory entry");
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if name != "target" && !name.starts_with('.') {
                    pending.push(path);
                }
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| extensions.contains(&e))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// The sub-directories of `dir` that hold a crate (a `Cargo.toml`).
fn crates_in(dir: &Path) -> Vec<PathBuf> {
    let mut crates: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{} is a readable directory: {e}", dir.display()))
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.join("Cargo.toml").is_file())
        .collect();
    crates.sort();
    crates
}

/// `text` without its comments (line and block), so a scan for code is not
/// tripped by prose. String literals are kept. A heuristic, erring on the
/// side of keeping text.
fn code_only(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    let mut in_block = false;
    while let Some(c) = chars.next() {
        if in_block {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block = false;
            } else if c == '\n' {
                out.push('\n');
            }
            continue;
        }
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(escaped) = chars.next() {
                    out.push(escaped);
                }
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                // To the end of the line.
                for rest in chars.by_ref() {
                    if rest == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                in_block = true;
            }
            _ => out.push(c),
        }
    }
    out
}

/// Whether `haystack` contains `word` as a whole identifier.
fn has_identifier(haystack: &str, word: &str) -> bool {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    haystack.match_indices(word).any(|(at, _)| {
        let before = haystack[..at].chars().next_back();
        let after = haystack[at + word.len()..].chars().next();
        !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
    })
}

fn without_whitespace(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The name of the Sandbox crate, spelled so that this file does not match
/// its own scans.
const SANDBOX_CRATE: &str = concat!("asamu", "-", "sandbox");
const SANDBOX_CRATE_IDENT: &str = concat!("asamu", "_", "sandbox");

// ---------------------------------------------------------------------------

#[test]
fn no_classic_crate_or_parity_tool_depends_on_the_sandbox() {
    let root = root();
    let mut checked = Vec::new();
    for dir in crates_in(&root.join("crates"))
        .into_iter()
        .chain(crates_in(&root.join("tools")))
    {
        let name = relative(&dir);
        if name == format!("crates/{SANDBOX_CRATE}") {
            continue;
        }
        let manifest = read(&dir.join("Cargo.toml"));
        assert!(
            !manifest.contains(SANDBOX_CRATE) && !manifest.contains(SANDBOX_CRATE_IDENT),
            "{name}/Cargo.toml mentions the Sandbox crate: Classic crates and parity tools \
             must not depend on it"
        );
        for file in files_under(&dir, &["rs"]) {
            let code = code_only(&read(&file));
            assert!(
                !has_identifier(&code, SANDBOX_CRATE_IDENT),
                "{} uses the Sandbox crate",
                relative(&file)
            );
        }
        checked.push(name);
    }
    // The scan saw the crates it exists for.
    for expected in [
        "crates/asamu-core",
        "crates/asamu-player",
        "crates/asamu-world",
        "crates/asamu-kismet",
        "crates/asamu-game",
        "crates/asamu-assets",
        "crates/asamu-ue3",
        "tools/asamu-trace",
        "tools/asamu-import",
    ] {
        assert!(
            checked.iter().any(|c| c == expected),
            "{expected} was not scanned (found {checked:?})"
        );
    }
    // And it can see a dependency when there is one: the app has it.
    let app = read(&root.join("apps/asamu/Cargo.toml"));
    assert!(
        app.contains(SANDBOX_CRATE),
        "the app no longer depends on the Sandbox crate; this scan needs a new positive control"
    );
}

/// The `[section]` of a manifest: its non-empty, non-comment lines.
fn section<'a>(manifest: &'a str, name: &str) -> Vec<&'a str> {
    let mut lines = Vec::new();
    let mut inside = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            inside = line == format!("[{name}]");
            continue;
        }
        if inside && !line.is_empty() && !line.starts_with('#') {
            lines.push(line);
        }
    }
    lines
}

fn section_names(manifest: &str) -> Vec<&str> {
    manifest
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('['))
        .collect()
}

#[test]
fn the_sandbox_manifest_adds_no_dependency_features() {
    let root = root();
    let manifest = read(&root.join("crates").join(SANDBOX_CRATE).join("Cargo.toml"));

    // Only these sections: no dev-, build- or target-specific dependencies
    // and no features of its own.
    assert_eq!(
        section_names(&manifest),
        ["[package]", "[dependencies]", "[lints]"],
        "the Sandbox crate's manifest gained a section"
    );

    // Only these dependencies, each taken from the workspace as it is.
    const FLOAT_ROUNDTRIP: &str = r#"serde_json={workspace=true,features=["float_roundtrip"]}"#;
    let allowed = [
        "asamu-core",
        "asamu-game",
        "asamu-player",
        "asamu-world",
        "glam",
        "serde",
        "serde_json",
        "thiserror",
    ];
    let dependencies = section(&manifest, "dependencies");
    assert_eq!(dependencies.len(), allowed.len(), "{dependencies:?}");
    for line in &dependencies {
        let compact = without_whitespace(line);
        let name = compact
            .split(['.', '='])
            .next()
            .expect("split yields at least one piece");
        assert!(allowed.contains(&name), "unexpected dependency: {line}");
        if name == "serde_json" {
            // The one feature: the same line `asamu-player` has (checked
            // below), so it is already on in every build with the player.
            assert_eq!(compact, FLOAT_ROUNDTRIP, "{line}");
        } else {
            assert_eq!(compact, format!("{name}.workspace=true"), "{line}");
        }
    }
    let feature_lines: Vec<&str> = manifest
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter(|line| line.contains("features") || line.contains("optional"))
        .collect();
    assert_eq!(feature_lines.len(), 1, "{feature_lines:?}");

    // The premise: the Classic player crate turns that same feature on.
    let player = read(&root.join("crates/asamu-player/Cargo.toml"));
    assert!(
        section(&player, "dependencies")
            .iter()
            .any(|line| without_whitespace(line) == FLOAT_ROUNDTRIP),
        "asamu-player no longer enables serde_json's float_roundtrip, so the Sandbox crate \
         would now be the one adding it to the build"
    );

    // The workspace entry is a plain path, and the app takes the crate as an
    // optional dependency with nothing switched on.
    let workspace = read(&root.join("Cargo.toml"));
    let entry = section(&workspace, "workspace.dependencies")
        .into_iter()
        .map(without_whitespace)
        .find(|line| line.starts_with(&format!("{SANDBOX_CRATE}=")))
        .expect("the workspace lists the Sandbox crate");
    assert_eq!(
        entry,
        format!(r#"{SANDBOX_CRATE}={{path="crates/{SANDBOX_CRATE}"}}"#)
    );
    let app = read(&root.join("apps/asamu/Cargo.toml"));
    let entry = section(&app, "dependencies")
        .into_iter()
        .map(without_whitespace)
        .find(|line| line.starts_with(&format!("{SANDBOX_CRATE}=")))
        .expect("the app lists the Sandbox crate");
    assert_eq!(
        entry,
        format!("{SANDBOX_CRATE}={{workspace=true,optional=true}}")
    );
    // The app's feature switches on that dependency and nothing else.
    let feature = section(&app, "features")
        .into_iter()
        .map(without_whitespace)
        .find(|line| line.starts_with("sandbox="))
        .expect("the app has the sandbox feature");
    assert_eq!(feature, format!(r#"sandbox=["dep:{SANDBOX_CRATE}"]"#));
}

#[test]
fn no_sandbox_cfg_outside_the_app() {
    // Spelled in pieces so this file does not contain what it looks for.
    let needle = without_whitespace(concat!("feature", " = ", "\"sand", "box\""));
    let root = root();
    let mut scanned = 0;
    for top in ["crates", "tools"] {
        for file in files_under(&root.join(top), &["rs", "toml"]) {
            let text = without_whitespace(&read(&file));
            assert!(
                !text.contains(&needle),
                "{} is compiled conditionally on the app's sandbox feature; a workspace build \
                 would switch it on for the parity tools too",
                relative(&file)
            );
            scanned += 1;
        }
    }
    assert!(scanned > 50, "only {scanned} files scanned");
    // The scan can see the attribute where it is allowed: the app.
    let app_main = without_whitespace(&read(&root.join("apps/asamu/src/main.rs")));
    assert!(
        app_main.contains(&needle),
        "the app no longer has the sandbox feature attribute; this scan needs a new positive \
         control"
    );
}

#[test]
fn set_params_is_called_only_by_the_sandbox() {
    const DOOR: &str = concat!("set", "_", "params");
    let root = root();
    let tooling = "crates/asamu-game/src/tooling.rs";
    let sandbox = format!("crates/{SANDBOX_CRATE}/");
    let mut scanned = 0;
    let mut users = Vec::new();
    for top in ["crates", "apps", "tools"] {
        for file in files_under(&root.join(top), &["rs"]) {
            scanned += 1;
            if !has_identifier(&code_only(&read(&file)), DOOR) {
                continue;
            }
            let name = relative(&file);
            assert!(
                name == tooling || name.starts_with(&sandbox),
                "{name} uses Game::{DOOR}: Classic code, the app and the parity tools must \
                 never replace a running game's parameters (only the Sandbox crate may)"
            );
            users.push(name);
        }
    }
    assert!(scanned > 100, "only {scanned} files scanned");
    // The scan sees the definition ...
    assert!(users.iter().any(|u| u == tooling), "{users:?}");
    // ... and inside the Sandbox crate's own sources there is one call
    // site, the retune: every parameter change of a session goes through
    // it, so every change re-latches the pawn and none skips validation.
    // (The crate's tests may call the door directly; that is how the guards
    // prove that handing a game its own set changes nothing.)
    let retune = format!("{sandbox}src/relatch.rs");
    let in_sources: Vec<&String> = users
        .iter()
        .filter(|u| u.starts_with(&format!("{sandbox}src/")))
        .collect();
    assert_eq!(
        in_sources,
        [&retune],
        "inside the Sandbox crate only src/relatch.rs may call Game::{DOOR}"
    );
}

/// The resources whose mutable borrow means "may change the simulation or
/// its clocks", found in whitespace-free code: for every `ResMut<...>` the
/// inner type is read with its generics, and a `Sim` or a `Time` of the
/// virtual or the fixed clock counts, however the path is written.
fn simulation_writes(code: &str) -> Vec<String> {
    const OPEN: &str = "ResMut<";
    let compact = without_whitespace(code);
    let mut found = Vec::new();
    for (at, _) in compact.match_indices(OPEN) {
        // A trailing comma is what rustfmt leaves when it breaks the line.
        let inner = generic_argument(&compact[at + OPEN.len()..]).trim_end_matches(',');
        // A system-parameter struct names the world's lifetime first
        // (`ResMut<'w, Sim>`, the form the single writer itself uses): the
        // resource is what follows it.
        let resource = match inner.split_once(',') {
            Some((lifetime, resource)) if lifetime.starts_with('\'') => resource,
            _ => inner,
        };
        if is_simulation_resource(resource) {
            found.push(format!("{OPEN}{inner}>"));
        }
    }
    found
}

/// The text of a generic argument list, given what follows its opening `<`:
/// up to the matching `>`.
fn generic_argument(rest: &str) -> &str {
    let mut depth = 1usize;
    for (i, c) in rest.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return &rest[..i];
                }
            }
            _ => {}
        }
    }
    rest
}

/// Whether a resource type as written in whitespace-free code (`Sim`,
/// `crate::Sim`, `Time<Virtual>`, `bevy::prelude::Time<bevy::time::Fixed>`)
/// is the simulation or one of the simulation's clocks.
fn is_simulation_resource(written: &str) -> bool {
    let (head, generics) = written.split_once('<').unwrap_or((written, ""));
    let last = |path: &str| path.rsplit("::").next().unwrap_or(path).to_owned();
    let type_name = last(head);
    let argument = last(generics.trim_end_matches(['>', ',']));
    type_name == "Sim" || (type_name == "Time" && (argument == "Virtual" || argument == "Fixed"))
}

/// The other ways code can get at those resources mutably, without a
/// `ResMut` parameter: world and command access by type
/// (`world.resource_mut::<Sim>()`, `commands.remove_resource::<Sim>()`,
/// `insert_resource(Sim::new(..))`). A text scan cannot follow a value
/// through a variable; it catches the type where it is spelled out.
fn world_writes(code: &str) -> Vec<String> {
    const METHODS: [&str; 7] = [
        "resource_mut",
        "get_resource_mut",
        "resource_scope",
        "remove_resource",
        "init_resource",
        "get_resource_or_insert_with",
        "get_resource_or_init",
    ];
    const INSERT: &str = "insert_resource(";
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let compact = without_whitespace(code);
    let mut found = Vec::new();
    for method in METHODS {
        let open = format!("{method}::<");
        for (at, _) in compact.match_indices(&open) {
            // The method itself, not a longer name that ends in it.
            if compact[..at].chars().next_back().is_some_and(is_ident) {
                continue;
            }
            let inner = generic_argument(&compact[at + open.len()..]);
            // `resource_scope::<Sim, _>`: the resource comes first.
            let resource = inner.split(',').next().unwrap_or(inner);
            if is_simulation_resource(resource) {
                found.push(format!("{open}{inner}>"));
            }
        }
    }
    for (at, _) in compact.match_indices(INSERT) {
        // The path of the inserted value's constructor, e.g. `Sim::new` or
        // `Time::<Fixed>::from_hz`.
        let rest = &compact[at + INSERT.len()..];
        let end = rest.find(['(', '{', ')', ',', ';']).unwrap_or(rest.len());
        let written = &rest[..end];
        let clock = has_identifier(written, "Time")
            && (has_identifier(written, "Virtual") || has_identifier(written, "Fixed"));
        if has_identifier(written, "Sim") || clock {
            found.push(format!("{INSERT}{written}"));
        }
    }
    found
}

/// The part of a source file before its first `#[cfg(test)]` item: what is
/// compiled into the app. (Every file of the plugin keeps its test modules
/// at the end, and the tests may set a simulation up by hand.)
fn non_test_code(text: &str) -> &str {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.trim() == "#[cfg(test)]" {
            return &text[..offset];
        }
        offset += line.len();
    }
    text
}

#[test]
fn only_control_rs_writes_the_simulation() {
    // The scanner itself, on samples (strings, so they are not scanned).
    for writes in [
        "fn apply(mut sim: ResMut<Sim>) {}",
        "fn apply(mut sim: ResMut<crate::Sim>) {}",
        "fn apply(\n    mut time: ResMut<\n        Time<Virtual>,\n    >,\n) {}",
        "fn apply(mut time: ResMut<Time<bevy::time::Fixed>>) {}",
        "fn apply(a: Res<Lab>, mut t: ResMut<bevy::prelude::Time<Fixed>>) {}",
        "fn apply(\n    mut sim: ResMut<\n        Sim,\n    >,\n) {}",
        // The fields of a system-parameter struct carry the world lifetime.
        "#[derive(SystemParam)]\nstruct Writer<'w> { sim: ResMut<'w, Sim> }",
        "struct Writer<'w, 's> {\n    sim: Option<ResMut<'w, crate::Sim>>,\n}",
        "struct Clocks<'w> {\n    virt: ResMut<'w, Time<Virtual>>,\n}",
        "struct Clocks<'w> { fixed: ResMut<\n    'w,\n    Time<bevy::time::Fixed>,\n> }",
        "fn apply(mut sim: ResMut<'_, Sim>) {}",
    ] {
        assert_eq!(simulation_writes(writes).len(), 1, "{writes}");
    }
    for harmless in [
        "fn draw(sim: Res<Sim>, time: Res<Time<Virtual>>) {}",
        "fn panel(mut state: ResMut<PanelState>, mut viz: ResMut<VizSettings>) {}",
        "fn real(mut time: ResMut<Time<Real>>, mut all: ResMut<Time>) {}",
        "fn other(mut s: ResMut<SimSettings>, mut q: ResMut<Assets<Mesh>>) {}",
        "struct Desk<'w> { lab: ResMut<'w, Lab>, ops: ResMut<'w, SimOps>, sim: Res<'w, Sim> }",
    ] {
        assert!(simulation_writes(harmless).is_empty(), "{harmless}");
    }
    assert!(
        simulation_writes(&code_only(
            "// takes ResMut<Sim>\nfn f() {} /* ResMut<Sim> */"
        ))
        .is_empty()
    );
    // Access by type through the world or commands.
    for writes in [
        "fn f(world: &mut World) { world.resource_mut::<Sim>().banner.clear(); }",
        "fn f(mut commands: Commands) { commands.remove_resource::<crate::Sim>(); }",
        "fn f(world: &mut World) { let _ = world.get_resource_mut::<Time<Virtual>>(); }",
        "fn f(world: &mut World) { world.resource_scope::<Sim, _>(|_, _| {}); }",
        "fn f(app: &mut App) { app.insert_resource(Sim::new(game, banner)); }",
        "fn f(mut c: Commands) { c.insert_resource(Time::<Fixed>::from_hz(30.0)); }",
        "fn f(app: &mut App) { app.init_resource::<\n    Time<Fixed>,\n>(); }",
    ] {
        assert_eq!(world_writes(writes).len(), 1, "{writes}");
    }
    for harmless in [
        "fn f(world: &World) { let _ = world.resource::<Sim>(); }",
        "fn f(app: &mut App) { app.init_resource::<SimOps>().init_resource::<PanelState>(); }",
        "fn f(world: &mut World) { world.resource_mut::<Lab>().notice = None; }",
        "fn f(app: &mut App) { app.insert_resource(Lab::new(true)).insert_resource(launch); }",
        "fn f(world: &mut World) { let _ = world.resource_mut::<Time<Real>>(); }",
        "fn f(world: &mut World) { my_resource_mut::<Sim>(world); }",
    ] {
        assert!(world_writes(harmless).is_empty(), "{harmless}");
    }
    assert_eq!(
        non_test_code("fn a() {}\n\n    #[cfg(test)]\nmod tests { fn b() {} }\n"),
        "fn a() {}\n\n"
    );
    assert_eq!(non_test_code("fn a() {}\n"), "fn a() {}\n");

    // The plugin's files.
    let app_src = root().join("apps/asamu/src");
    let mut files = files_under(&app_src.join("sandbox"), &["rs"]);
    files.push(app_src.join("sandbox.rs"));
    assert!(files.len() >= 2, "{files:?}");
    let control = "apps/asamu/src/sandbox/control.rs";
    assert!(
        files.iter().any(|f| relative(f) == control),
        "{control} is the single writer and must exist"
    );
    for file in &files {
        let name = relative(file);
        let text = read(file);
        // Mutable parameters anywhere in the file; access by type through
        // the world or commands in the code that is compiled into the app.
        let mut writes = simulation_writes(&code_only(&text));
        writes.extend(world_writes(&code_only(non_test_code(&text))));
        if name == control {
            // The scan sees the single writer do both, in the forms it uses
            // (a system-parameter struct with a lifetime among them).
            assert!(
                writes.iter().any(|w| w.starts_with("ResMut<'")),
                "{control} no longer takes the simulation through a system-parameter struct; \
                 this scan needs a new positive control ({writes:?})"
            );
            assert!(
                writes.iter().any(|w| !w.starts_with("ResMut<")),
                "{control} no longer reaches the simulation through commands; this scan needs \
                 a new positive control ({writes:?})"
            );
            continue;
        }
        assert!(
            writes.is_empty(),
            "{name} takes {writes:?}: only {control} may write the simulation or its clocks; \
             everything else sends a request"
        );
    }
}

#[test]
fn the_scan_helpers_do_what_they_say() {
    assert_eq!(
        code_only("a // b\nc /* d\n e */ f \"// not a comment\" g"),
        "a \nc \n f \"// not a comment\" g"
    );
    assert_eq!(
        code_only("let s = \"a\\\"//b\"; // c"),
        "let s = \"a\\\"//b\"; "
    );
    assert!(has_identifier("game.set_it(x)", "set_it"));
    assert!(has_identifier("Game::set_it", "set_it"));
    assert!(!has_identifier("fn set_it_now()", "set_it"));
    assert!(!has_identifier("reset_it()", "set_it"));
    assert_eq!(section("[a]\nx = 1\n# no\n\n[b]\ny = 2\n", "a"), ["x = 1"]);
    assert_eq!(section_names("[a]\nx = 1\n[b.c]\n"), ["[a]", "[b.c]"]);
}
