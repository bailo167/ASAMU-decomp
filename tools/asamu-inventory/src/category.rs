//! Category rules. Applied in order; the first matching rule wins. Path components and
//! file names are compared ASCII-case-insensitively; `ext` is already lowercase.
//!
//! | # | Category | Rule |
//! |---|---|---|
//! | 1 | `executable` | sniffed Mach-O / fat Mach-O / PE / ELF, or extension `exe` `dll` `dylib` `so` `com` |
//! | 2 | `editor-resource` | a path component `EditorResources`, or `Engine/Extras/...` (DCC tool scripts) |
//! | 3 | `shader` | a path component `Shaders`; a name starting `GlobalShaderCache`, `RefShaderCache` or `LocalShaderCache`; sniffed `BMSG`; extension `usf` `ush` |
//! | 4 | `map` | extension `asamu` (ASAMU's `MapExt`), `umap`, `ut3`, `udk` |
//! | 5 | `texture-related` | extension `tfc` `dds`, or sniffed UE3 compressed-chunk header |
//! | 6 | `ue3-package` | sniffed UE3 package tag, or extension `u` `upk` |
//! | 7 | `config` | extension `ini`, or a path component `Config` |
//! | 8 | `localization` | a path component `Localization`, or a UE3 language extension (`int`, `deu`, `fra`, ...) |
//! | 9 | `audio` | sniffed Ogg / WAVE, or extension `ogg` `wav` `mp3` `xma` `fsb` `bnk` `flac` `opus` |
//! | 10 | `movie` | sniffed Bink / SWF / GFx, or extension `bik` `bk2` `usm` `mp4` `avi` `webm` `swf` `gfx` |
//! | 11 | `font` | extension `ttf` `otf` `ttc` `fon` |
//! | 12 | `mesh-animation` | extension `psk` `psa` `fbx` `ase` `obj` `fxa` `apx` `apb` |
//! | 13 | `metadata` | `Info.plist`, `PkgInfo`, `steam_appid.txt`, a cooker TOC (`*TOC.txt`, `*TOC_<LANG>.txt`), a name starting `CookerSync`, extension `plist` `strings` `nib` |
//! | 14 | `image` | sniffed raster image or image extension, outside editor resources (e.g. splash screens) |
//! | 15 | `engine-resource` | `Engine/Stats/...` (FPS/memory chart HTML/CSS templates) |
//! | 16 | `documentation` | extension `txt` `html` `htm` `rtf` `chm` `pdf` `md` |
//! | 17 | `unknown` | everything else |

use serde::{Serialize, Serializer};

use crate::sniff::FileType;

/// Inventory category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    /// Native executables and shared libraries.
    Executable,
    /// Editor-only resources (icons, layouts, FaceFX editor data, DCC scripts).
    EditorResource,
    /// Shader source/binaries and shader caches.
    Shader,
    /// Cooked maps.
    Map,
    /// Texture file caches and loose texture formats.
    TextureRelated,
    /// UE3 packages (`.u`, `.upk`).
    Ue3Package,
    /// Configuration (`.ini`).
    Config,
    /// Localization text.
    Localization,
    /// Loose audio.
    Audio,
    /// Movies (Bink, Flash/Scaleform).
    Movie,
    /// Fonts.
    Font,
    /// Loose mesh/animation source formats.
    MeshAnimation,
    /// Bundle, Steam and cooker metadata.
    Metadata,
    /// Loose standalone images (splash screens).
    Image,
    /// Loose engine support files (stats templates).
    EngineResource,
    /// Documentation.
    Documentation,
    /// Nothing matched.
    Unknown,
}

impl Category {
    /// Stable identifier used in JSON and summaries.
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Executable => "executable",
            Category::EditorResource => "editor-resource",
            Category::Shader => "shader",
            Category::Map => "map",
            Category::TextureRelated => "texture-related",
            Category::Ue3Package => "ue3-package",
            Category::Config => "config",
            Category::Localization => "localization",
            Category::Audio => "audio",
            Category::Movie => "movie",
            Category::Font => "font",
            Category::MeshAnimation => "mesh-animation",
            Category::Metadata => "metadata",
            Category::Image => "image",
            Category::EngineResource => "engine-resource",
            Category::Documentation => "documentation",
            Category::Unknown => "unknown",
        }
    }
}

impl Serialize for Category {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// UE3 localization file extensions (three-letter language codes) seen in UE3 games; the
/// ASAMU Mac depot uses `int bra cze deu esn fin fra hun ita nld pol por slo tur`.
pub const LANGUAGE_EXTENSIONS: &[&str] = &[
    "int", "bra", "chn", "cht", "cze", "dan", "deu", "dut", "esm", "esn", "fin", "fra", "hun",
    "ita", "jpn", "kor", "nld", "nor", "pol", "por", "ptb", "rus", "slo", "swe", "tha", "tur",
];

fn has_component(components: &[&str], name: &str) -> bool {
    components.iter().any(|c| c.eq_ignore_ascii_case(name))
}

/// `parent/child` appears as consecutive directory components.
fn has_pair(dirs: &[&str], parent: &str, child: &str) -> bool {
    dirs.windows(2)
        .any(|w| w[0].eq_ignore_ascii_case(parent) && w[1].eq_ignore_ascii_case(child))
}

fn starts_with_ci(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// Whether a file name is a UE3 cooker table of contents (`PCTOC.txt`, `PCTOC_DEU.txt`,
/// `MacTOC.txt`, ...).
pub fn is_toc_name(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    let Some(stem) = lower.strip_suffix(".txt") else {
        return false;
    };
    stem.split('_').next().is_some_and(|s| s.ends_with("toc"))
}

/// Categorize one file. `rel_path` uses `/` separators.
pub fn categorize(rel_path: &str, ext: Option<&str>, kind: FileType) -> Category {
    let components: Vec<&str> = rel_path.split('/').filter(|c| !c.is_empty()).collect();
    let (file_name, dirs) = match components.split_last() {
        Some((name, dirs)) => (*name, dirs),
        None => ("", &[][..]),
    };
    let ext = ext.unwrap_or("");
    let ext_in = |list: &[&str]| list.contains(&ext);

    if kind.is_executable_image() || ext_in(&["exe", "dll", "dylib", "so", "com"]) {
        return Category::Executable;
    }
    if has_component(dirs, "EditorResources") || has_pair(dirs, "Engine", "Extras") {
        return Category::EditorResource;
    }
    if has_component(dirs, "Shaders")
        || ["GlobalShaderCache", "RefShaderCache", "LocalShaderCache"]
            .iter()
            .any(|p| starts_with_ci(file_name, p))
        || kind == FileType::Ue3GlobalShaderCache
        || ext_in(&["usf", "ush"])
    {
        return Category::Shader;
    }
    if ext_in(&["asamu", "umap", "ut3", "udk"]) {
        return Category::Map;
    }
    if ext_in(&["tfc", "dds"]) || kind == FileType::Ue3CompressedChunks {
        return Category::TextureRelated;
    }
    if matches!(kind, FileType::Ue3Package | FileType::Ue3PackageBigEndian) || ext_in(&["u", "upk"])
    {
        return Category::Ue3Package;
    }
    if ext == "ini" || has_component(dirs, "Config") {
        return Category::Config;
    }
    if has_component(dirs, "Localization") || ext_in(LANGUAGE_EXTENSIONS) {
        return Category::Localization;
    }
    if matches!(kind, FileType::Ogg | FileType::Wav)
        || ext_in(&["ogg", "wav", "mp3", "xma", "fsb", "bnk", "flac", "opus"])
    {
        return Category::Audio;
    }
    if matches!(
        kind,
        FileType::Bink | FileType::Bink2 | FileType::Swf | FileType::Gfx
    ) || ext_in(&["bik", "bk2", "usm", "mp4", "avi", "webm", "swf", "gfx"])
    {
        return Category::Movie;
    }
    if ext_in(&["ttf", "otf", "ttc", "fon"]) {
        return Category::Font;
    }
    if ext_in(&["psk", "psa", "fbx", "ase", "obj", "fxa", "apx", "apb"]) {
        return Category::MeshAnimation;
    }
    if ["Info.plist", "PkgInfo", "steam_appid.txt"]
        .iter()
        .any(|n| file_name.eq_ignore_ascii_case(n))
        || is_toc_name(file_name)
        || starts_with_ci(file_name, "CookerSync")
        || ext_in(&["plist", "strings", "nib"])
    {
        return Category::Metadata;
    }
    if kind.is_image() || ext_in(&["png", "bmp", "tga", "ico", "gif", "jpg", "jpeg"]) {
        return Category::Image;
    }
    if has_pair(dirs, "Engine", "Stats") {
        return Category::EngineResource;
    }
    if ext_in(&["txt", "html", "htm", "rtf", "chm", "pdf", "md"]) {
        return Category::Documentation;
    }
    Category::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: &str = "A Story About My Uncle.app/Contents/Resources";

    fn cat(rel: &str, ext: Option<&str>, kind: FileType) -> &'static str {
        categorize(&format!("{R}/{rel}"), ext, kind).as_str()
    }

    #[test]
    fn rules_on_mac_layout_paths() {
        use FileType as T;
        assert_eq!(
            categorize(
                "A Story About My Uncle.app/Contents/MacOS/ASAMU",
                None,
                T::MachO
            )
            .as_str(),
            "executable"
        );
        assert_eq!(
            cat("../MacOS/openal.dylib", Some("dylib"), T::MachOFat),
            "executable"
        );
        assert_eq!(
            cat("ASAMU/CookedMac/Core.u", Some("u"), T::Ue3Package),
            "ue3-package"
        );
        assert_eq!(
            cat(
                "ASAMU/CookedMac/Maps/TheCore_LOC_INT.upk",
                Some("upk"),
                T::Ue3Package
            ),
            "ue3-package"
        );
        assert_eq!(
            cat(
                "ASAMU/CookedMac/Maps/TheCore.asamu",
                Some("asamu"),
                T::Ue3Package
            ),
            "map"
        );
        assert_eq!(
            cat(
                "ASAMU/CookedMac/Textures.tfc",
                Some("tfc"),
                T::Ue3CompressedChunks
            ),
            "texture-related"
        );
        assert_eq!(
            cat(
                "ASAMU/CookedMac/GlobalShaderCache-PC-D3D-SM3.bin",
                Some("bin"),
                T::Ue3GlobalShaderCache
            ),
            "shader"
        );
        assert_eq!(
            cat(
                "ASAMU/CookedMac/RefShaderCache-PC-OpenGL.upk",
                Some("upk"),
                T::Ue3Package
            ),
            "shader"
        );
        assert_eq!(
            cat(
                "Engine/Shaders/Binaries/BasePassCommon.bin",
                Some("bin"),
                T::Unknown
            ),
            "shader"
        );
        assert_eq!(
            cat("ASAMU/Config/DefaultEngine.ini", Some("ini"), T::AsciiText),
            "config"
        );
        assert_eq!(
            cat(
                "ASAMU/Localization/INT/ASAMU.int",
                Some("int"),
                T::Utf16LeText
            ),
            "localization"
        );
        assert_eq!(
            cat(
                "Engine/Localization/POL/Core.POL",
                Some("pol"),
                T::Utf16LeText
            ),
            "localization"
        );
        assert_eq!(
            cat("Engine/EditorResources/wxRes/x.bmp", Some("bmp"), T::Bmp),
            "editor-resource"
        );
        assert_eq!(
            cat(
                "Engine/EditorResources/WPF/Controls/x.xaml",
                Some("xaml"),
                T::Utf16LeText
            ),
            "editor-resource"
        );
        assert_eq!(
            cat(
                "Engine/Extras/3dsMaxScripts/PivotPainter.ms",
                Some("ms"),
                T::AsciiText
            ),
            "editor-resource"
        );
        assert_eq!(
            cat("ASAMU/Splash/Mac/Splash.bmp", Some("bmp"), T::Bmp),
            "image"
        );
        assert_eq!(
            cat("ASAMU/PCTOC.txt", Some("txt"), T::AsciiText),
            "metadata"
        );
        assert_eq!(
            cat("ASAMU/PCTOC_DEU.txt", Some("txt"), T::AsciiText),
            "metadata"
        );
        assert_eq!(
            cat("ASAMU/Build/CookerSync_Game.xml", Some("xml"), T::Xml),
            "metadata"
        );
        assert_eq!(
            cat("steam_appid.txt", Some("txt"), T::AsciiText),
            "metadata"
        );
        assert_eq!(
            cat("English.lproj/MainMenu.nib", Some("nib"), T::BinaryPlist),
            "metadata"
        );
        assert_eq!(
            categorize(
                "A Story About My Uncle.app/Contents/Info.plist",
                Some("plist"),
                T::XmlPlist
            )
            .as_str(),
            "metadata"
        );
        assert_eq!(
            categorize(
                "A Story About My Uncle.app/Contents/PkgInfo",
                None,
                T::AsciiText
            )
            .as_str(),
            "metadata"
        );
        assert_eq!(
            cat("Engine/Stats/FPSChart_Row.html", Some("html"), T::AsciiText),
            "engine-resource"
        );
    }

    #[test]
    fn rules_for_other_platforms_and_media() {
        use FileType as T;
        assert_eq!(
            categorize("Binaries/Win32/ASAMU.exe", Some("exe"), T::Pe).as_str(),
            "executable"
        );
        assert_eq!(
            categorize("Binaries/Win32/x.dll", Some("dll"), T::Unknown).as_str(),
            "executable"
        );
        assert_eq!(
            categorize("ASAMU/Movies/Intro.bik", Some("bik"), T::Bink).as_str(),
            "movie"
        );
        assert_eq!(categorize("a/b.swf", Some("swf"), T::Swf).as_str(), "movie");
        assert_eq!(categorize("a/b.ogg", Some("ogg"), T::Ogg).as_str(), "audio");
        assert_eq!(categorize("a/b.dat", Some("dat"), T::Wav).as_str(), "audio");
        assert_eq!(
            categorize("a/b.ttf", Some("ttf"), T::Unknown).as_str(),
            "font"
        );
        assert_eq!(
            categorize("a/b.psk", Some("psk"), T::Unknown).as_str(),
            "mesh-animation"
        );
        assert_eq!(
            categorize("a/readme.txt", Some("txt"), T::AsciiText).as_str(),
            "documentation"
        );
        assert_eq!(
            categorize("a/b.xyz", Some("xyz"), T::Unknown).as_str(),
            "unknown"
        );
        assert_eq!(categorize("", None, T::Unknown).as_str(), "unknown");
        assert_eq!(
            categorize("x.upk", Some("upk"), T::Unknown).as_str(),
            "ue3-package"
        );
    }

    #[test]
    fn toc_names() {
        assert!(is_toc_name("PCTOC.txt"));
        assert!(is_toc_name("pctoc_bra.TXT"));
        assert!(is_toc_name("MacTOC.txt"));
        assert!(!is_toc_name("steam_appid.txt"));
        assert!(!is_toc_name("TOCs.txt"));
        assert!(!is_toc_name("PCTOC.ini"));
    }
}
