//! Steam metadata: `libraryfolders.vdf` and `appmanifest_<appid>.acf`.

use std::path::PathBuf;

use serde::Serialize;

use crate::vdf::{self, Document, Value};

/// One Steam library folder as listed in `libraryfolders.vdf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LibraryFolder {
    /// Library root (the folder that contains `steamapps/`).
    pub path: PathBuf,
    /// App IDs the library claims to contain (modern format only). `None` for the legacy
    /// format, which lists bare paths without app lists.
    pub apps: Option<Vec<u32>>,
}

impl LibraryFolder {
    /// Whether the library's app list names `app_id`. Unknown (legacy) lists return `None`.
    pub fn lists_app(&self, app_id: u32) -> Option<bool> {
        self.apps.as_ref().map(|apps| apps.contains(&app_id))
    }
}

/// Parse `libraryfolders.vdf` text in either of Steam's two formats.
///
/// Modern (2021+):
/// ```text
/// "libraryfolders" { "0" { "path" "/x/Steam" "apps" { "278360" "0" } } }
/// ```
/// Legacy (numeric keys mapping straight to a path; other keys such as
/// `TimeNextStatsReport` and `ContentStatsID` are metadata):
/// ```text
/// "LibraryFolders" { "TimeNextStatsReport" "123" "1" "D:\\SteamLibrary" }
/// ```
pub fn parse_library_folders(text: &str) -> Result<Vec<LibraryFolder>, vdf::VdfError> {
    let doc = vdf::parse(text)?;
    Ok(library_folders_from_doc(&doc))
}

fn library_folders_from_doc(doc: &Document) -> Vec<LibraryFolder> {
    let Some(root) = doc
        .get("libraryfolders")
        .or_else(|| doc.root().map(|(_, v)| v))
    else {
        return Vec::new();
    };
    let Some(children) = root.as_obj() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, value) in children {
        // Library entries have numeric keys in both formats.
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        match value {
            Value::Str(path) if !path.is_empty() => out.push(LibraryFolder {
                path: PathBuf::from(path),
                apps: None,
            }),
            Value::Obj(_) => {
                let Some(path) = value.get_str("path").filter(|p| !p.is_empty()) else {
                    continue;
                };
                let apps = value.get("apps").and_then(Value::as_obj).map(|apps| {
                    apps.iter()
                        .filter_map(|(id, _)| id.parse::<u32>().ok())
                        .collect::<Vec<_>>()
                });
                out.push(LibraryFolder {
                    path: PathBuf::from(path),
                    apps,
                });
            }
            Value::Str(_) => {}
        }
    }
    out
}

/// One installed depot from `InstalledDepots`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Depot {
    /// Depot ID.
    pub depot_id: u32,
    /// Manifest GID, serialized as a string because it exceeds 2^53.
    #[serde(serialize_with = "serialize_opt_u64_as_string")]
    pub manifest_id: Option<u64>,
    /// Size in bytes as recorded by Steam.
    pub size: Option<u64>,
}

fn serialize_opt_u64_as_string<S: serde::Serializer>(
    value: &Option<u64>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(v) => serializer.serialize_str(&v.to_string()),
        None => serializer.serialize_none(),
    }
}

/// The parts of `appmanifest_<appid>.acf` this project uses.
///
/// Deliberately omits account-identifying fields such as `LastOwner`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppManifest {
    /// `appid`.
    pub app_id: Option<u32>,
    /// `name`.
    pub name: Option<String>,
    /// `installdir` (folder name under `steamapps/common`).
    pub install_dir: Option<String>,
    /// `buildid`.
    pub build_id: Option<u64>,
    /// `StateFlags` (bit value 4 = fully installed).
    pub state_flags: Option<u32>,
    /// `SizeOnDisk` in bytes.
    pub size_on_disk: Option<u64>,
    /// `InstalledDepots`, in file order.
    pub depots: Vec<Depot>,
}

impl AppManifest {
    /// `StateFlags` has the "fully installed" bit (4) set.
    pub fn fully_installed(&self) -> Option<bool> {
        self.state_flags.map(|f| f & 4 != 0)
    }
}

/// Parse `appmanifest_<appid>.acf` text. Missing fields become `None`.
pub fn parse_app_manifest(text: &str) -> Result<AppManifest, vdf::VdfError> {
    let doc = vdf::parse(text)?;
    let empty = Value::Obj(Vec::new());
    let state = doc
        .get("AppState")
        .or_else(|| doc.root().map(|(_, v)| v))
        .unwrap_or(&empty);
    let num = |key: &str| {
        state
            .get_str(key)
            .and_then(|v| v.trim().parse::<u64>().ok())
    };
    let depots = state
        .get("InstalledDepots")
        .and_then(Value::as_obj)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(id, body)| {
                    let depot_id = id.trim().parse::<u32>().ok()?;
                    let field =
                        |k: &str| body.get_str(k).and_then(|v| v.trim().parse::<u64>().ok());
                    Some(Depot {
                        depot_id,
                        manifest_id: field("manifest"),
                        size: field("size"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(AppManifest {
        app_id: num("appid").and_then(|v| u32::try_from(v).ok()),
        name: state.get_str("name").map(str::to_string),
        install_dir: state
            .get_str("installdir")
            .filter(|d| !d.is_empty())
            .map(str::to_string),
        build_id: num("buildid"),
        state_flags: num("StateFlags").and_then(|v| u32::try_from(v).ok()),
        size_on_disk: num("SizeOnDisk"),
        depots,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODERN: &str = r#""libraryfolders"
{
	"0"
	{
		"path"		"/var/games/u/.local/share/Steam"
		"label"		""
		"contentid"		"1"
		"apps"
		{
			"228980"		"1"
		}
	}
	"1"
	{
		"path"		"D:\\Games\\SteamLibrary"
		"apps"
		{
			"278360"		"1246349269"
		}
	}
}
"#;

    const LEGACY: &str = "\"LibraryFolders\"\r\n{\r\n\t\"TimeNextStatsReport\"\t\t\"1600000000\"\r\n\t\"ContentStatsID\"\t\t\"-1\"\r\n\t\"1\"\t\t\"E:\\\\Steam Library\"\r\n\t\"2\"\t\t\"/mnt/games/steam\"\r\n}\r\n";

    #[test]
    fn modern_library_folders() {
        let libs = parse_library_folders(MODERN).unwrap();
        assert_eq!(libs.len(), 2);
        assert_eq!(
            libs[0].path,
            PathBuf::from("/var/games/u/.local/share/Steam")
        );
        assert_eq!(libs[0].lists_app(278360), Some(false));
        assert_eq!(libs[1].path, PathBuf::from(r"D:\Games\SteamLibrary"));
        assert_eq!(libs[1].lists_app(278360), Some(true));
    }

    #[test]
    fn legacy_library_folders() {
        let libs = parse_library_folders(LEGACY).unwrap();
        assert_eq!(libs.len(), 2);
        assert_eq!(libs[0].path, PathBuf::from(r"E:\Steam Library"));
        assert_eq!(libs[0].apps, None);
        assert_eq!(libs[1].path, PathBuf::from("/mnt/games/steam"));
    }

    #[test]
    fn library_folders_ignore_junk_entries() {
        let text = r#""libraryfolders" { "x" "y" "0" { "label" "no path" } "1" "" "2" { "path" "/ok" "apps" { "abc" "1" "5" "2" } } }"#;
        let libs = parse_library_folders(text).unwrap();
        assert_eq!(libs.len(), 1);
        assert_eq!(libs[0].apps, Some(vec![5]));
        assert!(
            parse_library_folders("\"libraryfolders\" \"flat\"")
                .unwrap()
                .is_empty()
        );
        assert!(parse_library_folders("").unwrap().is_empty());
        assert!(parse_library_folders("\"libraryfolders\" {").is_err());
    }

    #[test]
    fn library_folders_key_case_and_escaped_paths() {
        // Mixed key case (KeyValues keys are case-insensitive), a UNC path and a trailing
        // escaped backslash; duplicate library numbers keep file order.
        let text = "\u{feff}\"LibraryFolders\"\r\n{\r\n\
                    \t\"0\"\r\n\t{\r\n\t\t\"Path\"\t\t\"C:\\\\Program Files (x86)\\\\Steam\"\r\n\
                    \t\t\"Apps\"\r\n\t\t{\r\n\t\t\t\"278360\"\t\t\"1246349269\"\r\n\t\t}\r\n\t}\r\n\
                    \t\"1\"\t\t\"\\\\\\\\nas\\\\games\\\\\"\r\n\
                    \t\"1\"\t\t\"F:\\\\dup\"\r\n\
                    }\r\n";
        let libs = parse_library_folders(text).unwrap();
        assert_eq!(libs.len(), 3);
        assert_eq!(libs[0].path, PathBuf::from(r"C:\Program Files (x86)\Steam"));
        assert_eq!(libs[0].lists_app(278360), Some(true));
        assert_eq!(libs[1].path, PathBuf::from(r"\\nas\games\"));
        assert_eq!(libs[1].lists_app(278360), None);
        assert_eq!(libs[2].path, PathBuf::from(r"F:\dup"));
    }

    #[test]
    fn library_folders_without_the_expected_root_key() {
        // Some tools write a different root key; the first root block is used.
        let libs = parse_library_folders(r#""LibraryFoldersBackup" { "1" "/srv/steam" }"#).unwrap();
        assert_eq!(libs.len(), 1);
        assert_eq!(libs[0].path, PathBuf::from("/srv/steam"));
        // App IDs that overflow u32 or are not numbers are skipped, not fatal.
        let libs = parse_library_folders(
            r#""libraryfolders" { "0" { "path" "/x" "apps" { "99999999999" "1" "-5" "1" "278360" "1" } } }"#,
        )
        .unwrap();
        assert_eq!(libs[0].apps, Some(vec![278360]));
    }

    #[test]
    fn app_manifest_key_case_and_whitespace() {
        let m = parse_app_manifest(
            "\"appstate\" { \"AppID\" \" 278360 \" \"InstallDir\" \"A Story About My Uncle\" \
             \"BuildID\" \"1822049\" \"stateflags\" \"6\" \"installeddepots\" { \"278362\" { \"Manifest\" \"18446744073709551615\" } \"278363\" \"flat\" } }",
        )
        .unwrap();
        assert_eq!(m.app_id, Some(278360));
        assert_eq!(m.install_dir.as_deref(), Some("A Story About My Uncle"));
        assert_eq!(m.build_id, Some(1822049));
        assert_eq!(m.fully_installed(), Some(true));
        // Max u64 manifest id parses; a flat (non-block) depot entry yields no fields.
        assert_eq!(m.depots.len(), 2);
        assert_eq!(m.depots[0].manifest_id, Some(u64::MAX));
        assert_eq!(m.depots[1].manifest_id, None);
        let m = parse_app_manifest(r#""AppState" { "StateFlags" "1026" }"#).unwrap();
        assert_eq!(m.fully_installed(), Some(false));
    }

    #[test]
    fn app_manifest_fields() {
        let text = r#""AppState"
{
	"appid"		"278360"
	"name"		"A Story About My Uncle"
	"StateFlags"		"4"
	"installdir"		"A Story About My Uncle"
	"SizeOnDisk"		"1246349269"
	"buildid"		"1822049"
	"LastOwner"		"1"
	"InstalledDepots"
	{
		"278362"
		{
			"manifest"		"7137994883443283717"
			"size"		"1246349269"
		}
		"notanumber" { "manifest" "1" }
	}
}"#;
        let m = parse_app_manifest(text).unwrap();
        assert_eq!(m.app_id, Some(278360));
        assert_eq!(m.install_dir.as_deref(), Some("A Story About My Uncle"));
        assert_eq!(m.build_id, Some(1822049));
        assert_eq!(m.fully_installed(), Some(true));
        assert_eq!(
            m.depots,
            vec![Depot {
                depot_id: 278362,
                manifest_id: Some(7137994883443283717),
                size: Some(1246349269),
            }]
        );
        let json = serde_json::to_string(&m.depots[0]).unwrap();
        assert!(json.contains("\"7137994883443283717\""), "{json}");
    }

    #[test]
    fn app_manifest_tolerates_missing_and_bad_fields() {
        let m = parse_app_manifest(
            r#""AppState" { "appid" "99999999999" "buildid" "x" "installdir" "" }"#,
        )
        .unwrap();
        assert_eq!(m.app_id, None);
        assert_eq!(m.build_id, None);
        assert_eq!(m.install_dir, None);
        assert!(m.depots.is_empty());
        let m = parse_app_manifest("").unwrap();
        assert_eq!(m.app_id, None);
        assert!(parse_app_manifest("\"AppState\" { \"appid\" ").is_err());
    }
}
