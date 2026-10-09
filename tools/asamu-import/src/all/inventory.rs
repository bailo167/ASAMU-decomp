//! Compare the conversions' input files with the committed, sanitized
//! inventories (`docs/reverse-engineering/data/inventory/*.json`).
//!
//! The inventories are embedded at compile time (they hold only paths, sizes
//! and SHA-256 hashes, which the repository publishes anyway). Only the files
//! the importer reads are compared: the packages (`.u`, `.upk`, `.asamu`) and
//! texture file caches (`.tfc`) directly inside the cooked folder and its
//! `Maps` folder. Files are matched by their path relative to the cooked
//! folder, so a copied data tree verifies as well as a Steam install, and an
//! inventory is used only when its cooked folder has the same name as the
//! install's (`CookedMac` vs `CookedPC`: different cooks never "mismatch",
//! they have no inventory).
//!
//! Policy (set by `all`): missing files refuse the run (a conversion would
//! silently produce less), different sizes or hashes only warn (another
//! build of the game may still convert).

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The committed inventories: (id, JSON).
pub const EMBEDDED: &[(&str, &str)] = &[(
    "mac-depot278362-build1822049",
    include_str!(
        "../../../../docs/reverse-engineering/data/inventory/mac-depot278362-build1822049.json"
    ),
)];

/// Extensions of the files the conversions read.
pub const INPUT_EXTENSIONS: &[&str] = &["u", "upk", "asamu", "tfc"];

/// Is `name` (a file name) an input of the conversions?
pub fn is_input_name(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| INPUT_EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext)))
}

#[derive(Debug, Deserialize)]
struct RawInventory {
    schema: String,
    layout: String,
    steam: RawSteam,
    files: Vec<RawFile>,
}

#[derive(Debug, Deserialize)]
struct RawSteam {
    #[serde(default)]
    build_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RawFile {
    path: String,
    size: u64,
    sha256: String,
}

/// One inventoried input file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryFile {
    /// Path relative to the cooked folder, `/`-separated (`Core.u`, `Maps/X.asamu`).
    pub cooked_rel: String,
    /// Size in bytes.
    pub size: u64,
    /// SHA-256, lower-case hex.
    pub sha256: String,
}

/// The input-file part of one inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inventory {
    /// Identifier (the file stem).
    pub id: String,
    /// Install layout label (`mac-app`).
    pub layout: String,
    /// Steam build id.
    pub build_id: Option<u64>,
    /// Name of the cooked folder (`CookedMac`).
    pub cooked_dir: String,
    /// Input files, sorted by `cooked_rel`.
    pub files: Vec<InventoryFile>,
}

impl Inventory {
    /// Parse an `asamu-inventory/1` document, keeping the input files.
    pub fn parse(id: &str, json: &str) -> Result<Inventory> {
        let raw: RawInventory =
            serde_json::from_str(json).with_context(|| format!("parsing inventory {id}"))?;
        if raw.schema != "asamu-inventory/1" {
            anyhow::bail!("inventory {id}: unsupported schema {:?}", raw.schema);
        }
        let mut cooked_dir: Option<String> = None;
        let mut files = Vec::new();
        for f in raw.files {
            let parts: Vec<&str> = f.path.split('/').collect();
            let Some(pos) = parts
                .iter()
                .position(|p| p.to_ascii_lowercase().starts_with("cooked"))
            else {
                continue;
            };
            let rest = &parts[pos + 1..];
            let name = rest.last().copied().unwrap_or_default();
            // Directly in the cooked folder, or in its Maps folder.
            let direct = rest.len() == 1
                || (rest.len() == 2
                    && rest.first().is_some_and(|d| d.eq_ignore_ascii_case("maps")));
            if !direct || !is_input_name(name) {
                continue;
            }
            cooked_dir.get_or_insert_with(|| parts[pos].to_owned());
            files.push(InventoryFile {
                cooked_rel: rest.join("/"),
                size: f.size,
                sha256: f.sha256.to_ascii_lowercase(),
            });
        }
        files.sort_by(|a, b| a.cooked_rel.cmp(&b.cooked_rel));
        Ok(Inventory {
            id: id.to_owned(),
            layout: raw.layout,
            build_id: raw.steam.build_id,
            cooked_dir: cooked_dir.unwrap_or_default(),
            files,
        })
    }

    /// Every embedded inventory.
    pub fn embedded() -> Result<Vec<Inventory>> {
        EMBEDDED
            .iter()
            .map(|(id, json)| Inventory::parse(id, json))
            .collect()
    }
}

/// An input file found on disk, with its hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskFile {
    /// Path relative to the cooked folder, `/`-separated.
    pub cooked_rel: String,
    /// Size in bytes.
    pub size: u64,
    /// SHA-256, lower-case hex.
    pub sha256: String,
}

/// Outcome of a verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerifyStatus {
    /// Every inventoried input exists with the same size and SHA-256.
    Match,
    /// All inventoried inputs exist, some differ (warning only).
    Mismatch,
    /// Inventoried inputs are missing (the run is refused unless `--no-verify`).
    MissingFiles,
    /// No committed inventory describes this cook.
    NoInventory,
    /// Not compared (`--no-verify`).
    Skipped,
}

/// A file whose size or hash differs from the inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mismatch {
    /// Path relative to the cooked folder.
    pub path: String,
    /// Inventoried size.
    pub expected_size: u64,
    /// Size on disk.
    pub size: u64,
    /// Inventoried SHA-256.
    pub expected_sha256: String,
    /// SHA-256 on disk.
    pub sha256: String,
}

/// Result of comparing the install with the inventories (deterministic).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verification {
    /// Outcome.
    pub status: VerifyStatus,
    /// Inventory used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory: Option<String>,
    /// Build id of that inventory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory_build_id: Option<u64>,
    /// Inventoried input files compared.
    pub checked: usize,
    /// Of those, identical on disk.
    pub matched: usize,
    /// Inventoried but absent on disk.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    /// Present with another size or hash.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mismatched: Vec<Mismatch>,
    /// Inputs on disk that the inventory does not list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unlisted: Vec<String>,
    /// Human-readable remarks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl Verification {
    /// The record for `--no-verify`.
    pub fn skipped() -> Verification {
        Verification {
            status: VerifyStatus::Skipped,
            inventory: None,
            inventory_build_id: None,
            checked: 0,
            matched: 0,
            missing: Vec::new(),
            mismatched: Vec::new(),
            unlisted: Vec::new(),
            notes: vec!["inventory comparison skipped (--no-verify)".to_owned()],
        }
    }
}

/// Compare `disk` (the input files found in an install whose cooked folder is
/// named `cooked_dir`, Steam build `build_id`) with the matching inventory.
pub fn verify(
    inventories: &[Inventory],
    cooked_dir: &str,
    build_id: Option<u64>,
    disk: &[DiskFile],
) -> Verification {
    let candidates: Vec<&Inventory> = inventories
        .iter()
        .filter(|i| i.cooked_dir.eq_ignore_ascii_case(cooked_dir))
        .collect();
    // Prefer the inventory of the same build; otherwise the first one.
    let chosen = candidates
        .iter()
        .find(|i| build_id.is_some() && i.build_id == build_id)
        .or_else(|| candidates.first())
        .copied();
    let Some(inv) = chosen else {
        return Verification {
            status: VerifyStatus::NoInventory,
            inventory: None,
            inventory_build_id: None,
            checked: 0,
            matched: 0,
            missing: Vec::new(),
            mismatched: Vec::new(),
            unlisted: Vec::new(),
            notes: vec![format!(
                "no committed inventory describes a {cooked_dir} cook; the conversions are \
                 verified on the Mac build (CookedMac, Steam build 1822049) only"
            )],
        };
    };
    let mut notes = Vec::new();
    match (build_id, inv.build_id) {
        (Some(b), Some(i)) if b != i => notes.push(format!(
            "Steam build {b} differs from the inventoried build {i}; comparing files anyway"
        )),
        (None, Some(i)) => notes.push(format!(
            "Steam build unknown (no app manifest next to the install); comparing with build {i}"
        )),
        _ => {}
    }
    let on_disk: BTreeMap<String, &DiskFile> = disk
        .iter()
        .map(|d| (d.cooked_rel.to_ascii_lowercase(), d))
        .collect();
    let mut listed = std::collections::BTreeSet::new();
    let mut missing = Vec::new();
    let mut mismatched = Vec::new();
    let mut matched = 0;
    for f in &inv.files {
        let key = f.cooked_rel.to_ascii_lowercase();
        listed.insert(key.clone());
        match on_disk.get(&key) {
            None => missing.push(f.cooked_rel.clone()),
            Some(d) if d.size == f.size && d.sha256.eq_ignore_ascii_case(&f.sha256) => {
                matched += 1;
            }
            Some(d) => mismatched.push(Mismatch {
                path: f.cooked_rel.clone(),
                expected_size: f.size,
                size: d.size,
                expected_sha256: f.sha256.clone(),
                sha256: d.sha256.clone(),
            }),
        }
    }
    let unlisted: Vec<String> = disk
        .iter()
        .filter(|d| !listed.contains(&d.cooked_rel.to_ascii_lowercase()))
        .map(|d| d.cooked_rel.clone())
        .collect();
    let status = if !missing.is_empty() {
        VerifyStatus::MissingFiles
    } else if !mismatched.is_empty() {
        VerifyStatus::Mismatch
    } else {
        VerifyStatus::Match
    };
    Verification {
        status,
        inventory: Some(inv.id.clone()),
        inventory_build_id: inv.build_id,
        checked: inv.files.len(),
        matched,
        missing,
        mismatched,
        unlisted,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inv_json(files: &[(&str, u64, &str)]) -> String {
        let rows: Vec<String> = files
            .iter()
            .map(|(p, s, h)| {
                format!(
                    r#"{{"path":"Root.app/Contents/Resources/ASAMU/CookedMac/{p}","size":{s},"sha256":"{h}","type":"x","category":"y"}}"#
                )
            })
            .collect();
        format!(
            r#"{{"schema":"asamu-inventory/1","layout":"mac-app","steam":{{"app_id":278360,"build_id":7}},"files":[{{"path":"Root.app/Contents/Info.plist","size":1,"sha256":"00","type":"x","category":"y"}},{}]}}"#,
            rows.join(",")
        )
    }

    fn disk(p: &str, size: u64, h: &str) -> DiskFile {
        DiskFile {
            cooked_rel: p.to_owned(),
            size,
            sha256: h.to_owned(),
        }
    }

    #[test]
    fn embedded_inventory_lists_the_mac_inputs() {
        let all = Inventory::embedded().unwrap();
        let mac = all
            .iter()
            .find(|i| i.id == "mac-depot278362-build1822049")
            .unwrap();
        assert_eq!(mac.layout, "mac-app");
        assert_eq!(mac.build_id, Some(1_822_049));
        assert_eq!(mac.cooked_dir, "CookedMac");
        // 42 packages + 3 texture file caches (INVENTORY.md).
        assert_eq!(mac.files.len(), 45);
        assert_eq!(
            mac.files
                .iter()
                .filter(|f| f.cooked_rel.starts_with("Maps/"))
                .count(),
            21
        );
        assert!(mac.files.iter().all(|f| f.sha256.len() == 64 && f.size > 0));
    }

    #[test]
    fn match_mismatch_missing_unlisted() {
        let inv = Inventory::parse(
            "t",
            &inv_json(&[
                ("Core.u", 3, "aa"),
                ("Maps/M.asamu", 5, "bb"),
                ("T.tfc", 2, "cc"),
            ]),
        )
        .unwrap();
        assert_eq!(inv.files.len(), 3);
        assert_eq!(inv.cooked_dir, "CookedMac");

        let ok = [
            disk("Core.u", 3, "aa"),
            disk("maps/m.asamu", 5, "BB"),
            disk("T.tfc", 2, "cc"),
        ];
        let v = verify(std::slice::from_ref(&inv), "CookedMac", Some(7), &ok);
        assert_eq!(v.status, VerifyStatus::Match);
        assert_eq!((v.checked, v.matched), (3, 3));
        assert!(v.notes.is_empty());

        let changed = [
            disk("Core.u", 3, "ab"),
            disk("Maps/M.asamu", 5, "bb"),
            disk("T.tfc", 2, "cc"),
            disk("Extra.upk", 1, "dd"),
        ];
        let v = verify(std::slice::from_ref(&inv), "CookedMac", Some(8), &changed);
        assert_eq!(v.status, VerifyStatus::Mismatch);
        assert_eq!(v.mismatched.len(), 1);
        assert_eq!(v.unlisted, vec!["Extra.upk".to_owned()]);
        assert_eq!(v.notes.len(), 1, "build difference noted");

        let missing = [disk("Core.u", 3, "aa")];
        let v = verify(std::slice::from_ref(&inv), "CookedMac", Some(7), &missing);
        assert_eq!(v.status, VerifyStatus::MissingFiles);
        assert_eq!(v.missing.len(), 2);

        // Another cook has no inventory.
        let v = verify(std::slice::from_ref(&inv), "CookedPC", None, &ok);
        assert_eq!(v.status, VerifyStatus::NoInventory);
        assert!(v.inventory.is_none());
    }

    #[test]
    fn input_names() {
        assert!(is_input_name("Startup.upk"));
        assert!(is_input_name("AG-IceCave.ASAMU"));
        assert!(is_input_name("Textures.tfc"));
        assert!(!is_input_name("PCTOC.txt"));
        assert!(!is_input_name("upk"));
    }

    #[test]
    fn rejects_other_schemas() {
        assert!(
            Inventory::parse(
                "x",
                r#"{"schema":"other","layout":"","steam":{},"files":[]}"#
            )
            .is_err()
        );
        assert!(Inventory::parse("x", "not json").is_err());
    }
}
