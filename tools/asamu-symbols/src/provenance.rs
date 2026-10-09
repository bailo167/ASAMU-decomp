//! Provenance of a symbol: which compilation unit (or archive) produced it, or
//! which dylib an undefined symbol is bound to.
//!
//! The original executable keeps its STABS "debug map" (`N_SO` / `N_OSO` /
//! `N_FUN` / `N_STSYM` / `N_GSYM` entries). Those entries contain absolute
//! build-machine paths of third parties. This module reduces every path to a
//! **sanitized** component label plus a path relative to a known marker
//! (`Development/Src/`, `External/`), so no user or machine names are kept.

use serde::Serialize;

/// Where a compilation unit came from (sanitized).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(tag = "type", content = "name", rename_all = "kebab-case")]
pub enum Component {
    /// An Unreal Engine 3 source module under `Development/Src/<Module>/`.
    Ue3Module(String),
    /// A PhysX static archive (`libPhysXCore.a`, ...) or PhysX SDK source.
    Physx(String),
    /// A Scaleform GFx static archive (`libgfx.a`, ...) or Scaleform source.
    Scaleform(String),
    /// A library under `External/<lib>-<version>/` built into the executable.
    External(String),
    /// Debug-map entry whose path matched no known marker.
    Other,
}

impl Component {
    /// Stable string id, e.g. `ue3:Engine`, `physx:libPhysXCore.a`, `external:zlib`.
    pub fn id(&self) -> String {
        match self {
            Component::Ue3Module(m) => format!("ue3:{m}"),
            Component::Physx(a) => format!("physx:{a}"),
            Component::Scaleform(a) => format!("scaleform:{a}"),
            Component::External(l) => format!("external:{l}"),
            Component::Other => "other".to_string(),
        }
    }

    /// The UE3 module name, if this is a UE3 module.
    pub fn ue3_module(&self) -> Option<&str> {
        match self {
            Component::Ue3Module(m) => Some(m),
            _ => None,
        }
    }
}

/// A sanitized compilation unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompileUnit {
    /// Component label.
    pub component: Component,
    /// Path relative to the marker (e.g. `Engine/Src/UnPhysic.cpp`,
    /// `External/zlib/Src/inflate.c`) or just the file name for archives.
    pub rel_path: String,
    /// Library version hint taken from the marker directory name
    /// (e.g. `1.3.2` from `libvorbis-1.3.2`), when present.
    pub version_hint: Option<String>,
    /// SDK marker directory for PhysX / Scaleform (e.g. `PhysX_284_UE3`).
    pub sdk_marker: Option<String>,
}

/// Lexically resolve `.` and `..` segments (`a/b/../c` → `a/c`). Leading `..`
/// segments that cannot be resolved are kept.
pub fn normalize_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if matches!(out.last(), Some(last) if *last != "..") {
                    out.pop();
                } else if !absolute {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    let joined = out.join("/");
    let trailing = path.ends_with('/') && !joined.is_empty();
    match (absolute, trailing) {
        (true, true) => format!("/{joined}/"),
        (true, false) => format!("/{joined}"),
        (false, true) => format!("{joined}/"),
        (false, false) => joined,
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Split `archive.a(member.o)` into (`archive.a` basename, `member.o`).
fn split_archive(oso: &str) -> Option<(&str, &str)> {
    let inner = oso.strip_suffix(')')?;
    let open = inner.rfind('(')?;
    let archive = inner.get(..open)?;
    let member = inner.get(open + 1..)?;
    Some((basename(archive), member))
}

/// Split `libvorbis-1.3.2` into (`libvorbis`, Some(`1.3.2`)).
fn split_version(segment: &str) -> (String, Option<String>) {
    let bytes = segment.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'-'
            && bytes
                .get(i + 1)
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false)
        {
            let name = segment.get(..i).unwrap_or(segment);
            let ver = segment.get(i + 1..).unwrap_or("");
            return (name.to_string(), Some(ver.to_string()));
        }
    }
    (segment.to_string(), None)
}

/// Find a path segment that starts with `prefix` (e.g. `PhysX_` or `SF4`).
fn segment_with_prefix<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    path.split('/').find(|s| s.starts_with(prefix))
}

/// Sanitize one debug-map unit. `dir` and `file` come from the `N_SO` pair,
/// `oso` from `N_OSO` (object path or `archive.a(member.o)`).
///
/// Rules (ordered, first match wins):
/// 1. `N_OSO` names a static archive `libPhysX*.a` / `libLowLevel.a` → PhysX;
///    `libgfx*.a` → Scaleform.
/// 2. Source path contains `/Development/Src/` → UE3 module (first segment after it).
/// 3. Source path contains `External/` → external library (segment after it,
///    version suffix split off).
/// 4. Source directory contains a `PhysX_*` segment → PhysX; a `Scaleform` segment → Scaleform.
/// 5. Otherwise → `Other` (only the file name is kept).
pub fn sanitize_unit(dir: &str, file: &str, oso: &str) -> CompileUnit {
    let full = normalize_path(&if file.starts_with('/') || dir.is_empty() {
        file.to_string()
    } else {
        format!("{dir}{file}")
    });
    let dir = normalize_path(dir);
    let dir = dir.as_str();
    let file_name = basename(if file.is_empty() { oso } else { file }).to_string();
    let physx_marker = segment_with_prefix(dir, "PhysX_").map(str::to_string);
    let sf_marker = segment_with_prefix(dir, "SF4").map(str::to_string);

    if let Some((archive, _member)) = split_archive(oso) {
        if archive.starts_with("libPhysX") || archive.starts_with("libLowLevel") {
            return CompileUnit {
                component: Component::Physx(archive.to_string()),
                rel_path: file_name,
                version_hint: None,
                sdk_marker: physx_marker,
            };
        }
        if archive.starts_with("libgfx") {
            return CompileUnit {
                component: Component::Scaleform(archive.to_string()),
                rel_path: file_name,
                version_hint: None,
                sdk_marker: sf_marker,
            };
        }
    }
    if let Some(pos) = full.find("/Development/Src/") {
        let rest = full.get(pos + "/Development/Src/".len()..).unwrap_or("");
        let module = rest.split('/').next().unwrap_or("").to_string();
        if !module.is_empty() {
            return CompileUnit {
                component: Component::Ue3Module(module),
                rel_path: rest.to_string(),
                version_hint: None,
                sdk_marker: None,
            };
        }
    }
    if let Some(pos) = full.find("External/") {
        let rest = full.get(pos + "External/".len()..).unwrap_or("");
        let segment = rest.split('/').next().unwrap_or("");
        if !segment.is_empty() {
            let (lib, version) = split_version(segment);
            return CompileUnit {
                component: Component::External(lib),
                rel_path: format!("External/{rest}"),
                version_hint: version,
                sdk_marker: None,
            };
        }
    }
    if physx_marker.is_some() {
        return CompileUnit {
            component: Component::Physx("source".to_string()),
            rel_path: file_name,
            version_hint: None,
            sdk_marker: physx_marker,
        };
    }
    if dir.split('/').any(|s| s == "Scaleform") {
        return CompileUnit {
            component: Component::Scaleform("source".to_string()),
            rel_path: file_name,
            version_hint: None,
            sdk_marker: sf_marker,
        };
    }
    CompileUnit {
        component: Component::Other,
        rel_path: file_name,
        version_hint: None,
        sdk_marker: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ue3_module_paths_are_relative() {
        let u = sanitize_unit(
            "/Build/someone/P4/ws/UnrealEngine3/Development/Src/Engine/Src/",
            "UnPhysic.cpp",
            "/Build/someone/build/UnPhysic.cpp.o",
        );
        assert_eq!(u.component, Component::Ue3Module("Engine".into()));
        assert_eq!(u.rel_path, "Engine/Src/UnPhysic.cpp");
        assert!(!u.rel_path.contains("someone"));
    }

    #[test]
    fn archives_win_over_paths() {
        let u = sanitize_unit(
            "/Build/x/depot/PhysX/Epic/PhysX_284_UE3/SDKs/Physics/src/",
            "NpScene.cpp",
            "/Build/y/External/PhysX/SDKs/lib/osxstatic/libPhysXCore.a(NpScene.o)",
        );
        assert_eq!(u.component, Component::Physx("libPhysXCore.a".into()));
        assert_eq!(u.sdk_marker.as_deref(), Some("PhysX_284_UE3"));
        assert_eq!(u.rel_path, "NpScene.cpp");
        let g = sanitize_unit(
            "/Build/z/depot/Partners/Scaleform/SF4-Pure/Src/GFx/",
            "GFx_Player.cpp",
            "/a/External/GFx/Lib/libgfx_as3.a(GFx_Player.o)",
        );
        assert_eq!(g.component, Component::Scaleform("libgfx_as3.a".into()));
        assert_eq!(g.sdk_marker.as_deref(), Some("SF4-Pure"));
    }

    #[test]
    fn external_libraries_split_versions() {
        let u = sanitize_unit(
            "../External/libvorbis-1.3.2/lib/",
            "block.c",
            "/b/block.c.o",
        );
        assert_eq!(u.component, Component::External("libvorbis".into()));
        assert_eq!(u.version_hint.as_deref(), Some("1.3.2"));
        assert_eq!(u.rel_path, "External/libvorbis-1.3.2/lib/block.c");
        let z = sanitize_unit(
            "/Build/q/UE3/Development/External/zlib/Src/",
            "inflate.c",
            "",
        );
        assert_eq!(z.component, Component::External("zlib".into()));
        assert_eq!(z.version_hint, None);
    }

    #[test]
    fn ue3_engine_physx_glue_stays_ue3() {
        let u = sanitize_unit(
            "/Build/a/UnrealEngine3/Development/Src/Engine/Src/",
            "UnPhysAssetTools.cpp",
            "/x/PhysX_glue.o",
        );
        assert_eq!(u.component, Component::Ue3Module("Engine".into()));
    }

    #[test]
    fn dot_dot_segments_are_resolved_before_matching() {
        let u = sanitize_unit(
            "/Build/a/UnrealEngine3/Development/Src/",
            "../External/libvorbis-1.3.2/lib/block.c",
            "",
        );
        assert_eq!(u.component, Component::External("libvorbis".into()));
        assert_eq!(normalize_path("/a/b/../c/./d/"), "/a/c/d/");
        assert_eq!(normalize_path("../x/../../y"), "../../y");
        assert_eq!(normalize_path("/.."), "/");
        assert_eq!(normalize_path(""), "");
    }

    #[test]
    fn unknown_paths_keep_only_file_name() {
        let u = sanitize_unit("/opt/secret/project/", "thing.c", "/opt/secret/thing.o");
        assert_eq!(u.component, Component::Other);
        assert_eq!(u.rel_path, "thing.c");
    }

    #[test]
    fn malformed_archive_strings_do_not_panic() {
        for oso in ["(", ")", "()", "a(", "lib.a(b", "x)", ""] {
            let _ = sanitize_unit("", "", oso);
        }
        let _ = split_version("-");
        let _ = split_version("lib-");
    }
}
