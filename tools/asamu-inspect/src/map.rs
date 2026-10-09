//! Metadata-only map summary: export class census grouped into gameplay-relevant
//! categories. Works from the export table alone (no payload decoding).

use std::collections::BTreeMap;

use anyhow::Result;
use asamu_ue3::Package;
use serde::Serialize;

/// Packages treated as stock Unreal Engine 3 / UDK engine code. Classes from
/// any other package (including the UDK sample game `UTGame`/`UTGameContent`
/// and the game's own `ASAMU`) are listed as non-stock.
pub const STOCK_ENGINE_PACKAGES: &[&str] = &[
    "Core",
    "Engine",
    "GameFramework",
    "GFxUI",
    "GFxUIEditor",
    "IpDrv",
    "OnlineSubsystemSteamworks",
    "UDKBase",
    "UnrealEd",
    "Editor",
    "WinDrv",
    "UTEditor",
];

/// One category of the map census.
#[derive(Debug, Default, Serialize)]
pub struct Category {
    /// Total exports in this category.
    pub total: usize,
    /// Per-class export counts.
    pub classes: BTreeMap<String, usize>,
}

/// One class row of the census.
#[derive(Debug, Serialize)]
pub struct ClassRow {
    /// Class name.
    pub class: String,
    /// Outermost package of the class (`<this package>` when defined at top
    /// level in the file itself).
    pub package: String,
    /// Export count.
    pub count: usize,
}

/// Map summary.
#[derive(Debug, Serialize)]
pub struct MapSummary {
    /// File name.
    pub file: String,
    /// `PKG_ContainsMap` flag.
    pub contains_map_flag: bool,
    /// Names of exports whose class is `World`.
    pub worlds: Vec<String>,
    /// Paths of exports whose class is `Level`.
    pub levels: Vec<String>,
    /// An export named `PersistentLevel` exists.
    pub has_persistent_level: bool,
    /// Additional packages to cook (sublevels named by the summary).
    pub additional_packages: Vec<String>,
    /// Totals.
    pub name_count: usize,
    /// Import count.
    pub import_count: usize,
    /// Export count.
    pub export_count: usize,
    /// Distinct export classes.
    pub distinct_classes: usize,
    /// Gameplay-relevant categories (heuristic, by class name; may overlap).
    pub categories: BTreeMap<String, Category>,
    /// Classes whose package is not a stock engine package.
    pub non_stock_classes: Vec<ClassRow>,
    /// Full class census, most frequent first.
    pub classes: Vec<ClassRow>,
}

type Rule = fn(&str) -> bool;

/// Category name and membership rule (applied to the export's class name).
pub const CATEGORIES: &[(&str, Rule)] = &[
    ("player_starts", |c| c.contains("PlayerStart")),
    ("static_mesh_actors", |c| c.contains("StaticMeshActor")),
    ("lights", |c| {
        c.ends_with("Light")
            || c.ends_with("LightToggleable")
            || c.ends_with("LightMovable")
            || c.ends_with("LightActor")
    }),
    ("light_components", |c| c.contains("LightComponent")),
    ("triggers_volumes", |c| {
        c.contains("Trigger") || c.ends_with("Volume")
    }),
    ("kismet", |c| {
        c.starts_with("Seq")
            || c.contains("SeqAct_")
            || c.contains("SeqEvent_")
            || c.contains("SeqCond_")
            || c.contains("SeqVar_")
    }),
    ("matinee", |c| {
        c.starts_with("InterpActor")
            || c.starts_with("InterpData")
            || c.starts_with("InterpGroup")
            || c.starts_with("InterpTrack")
            || c == "SeqAct_Interp"
            || c.contains("Matinee")
    }),
    ("sound", |c| c.contains("Sound")),
];

/// Build the map summary for a parsed package.
pub fn summarize(file: &str, pkg: &Package) -> Result<MapSummary> {
    let mut census: BTreeMap<String, (String, usize)> = BTreeMap::new();
    let mut worlds = Vec::new();
    let mut levels = Vec::new();
    for i in 0..pkg.exports.len() {
        let class = pkg.export_class_name(i)?;
        let package = pkg
            .export_class_package(i)?
            .unwrap_or_else(|| "<this package>".to_owned());
        if class == "World" {
            worlds.push(pkg.export_path(i)?);
        }
        if class == "Level" {
            levels.push(pkg.export_path(i)?);
        }
        let e = census.entry(class).or_insert((package, 0));
        e.1 += 1;
    }
    let has_persistent_level = !pkg.find_exports("PersistentLevel").is_empty();

    let mut categories: BTreeMap<String, Category> = BTreeMap::new();
    for &(name, _) in CATEGORIES {
        categories.insert(name.to_owned(), Category::default());
    }
    for (class, (_, count)) in &census {
        for &(name, rule) in CATEGORIES {
            if rule(class)
                && let Some(cat) = categories.get_mut(name)
            {
                cat.total += count;
                cat.classes.insert(class.clone(), *count);
            }
        }
    }

    let mut classes: Vec<ClassRow> = census
        .into_iter()
        .map(|(class, (package, count))| ClassRow {
            class,
            package,
            count,
        })
        .collect();
    classes.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.class.cmp(&b.class)));
    let non_stock_classes = classes
        .iter()
        .filter(|r| !STOCK_ENGINE_PACKAGES.contains(&r.package.as_str()))
        .map(|r| ClassRow {
            class: r.class.clone(),
            package: r.package.clone(),
            count: r.count,
        })
        .collect();

    Ok(MapSummary {
        file: file.to_owned(),
        contains_map_flag: pkg.summary.contains_map(),
        worlds,
        levels,
        has_persistent_level,
        additional_packages: pkg.summary.additional_packages_to_cook.clone(),
        name_count: pkg.names.len(),
        import_count: pkg.imports.len(),
        export_count: pkg.exports.len(),
        distinct_classes: classes.len(),
        categories,
        non_stock_classes,
        classes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cats(class: &str) -> Vec<&'static str> {
        CATEGORIES
            .iter()
            .filter(|(_, rule)| rule(class))
            .map(|(n, _)| *n)
            .collect()
    }

    #[test]
    fn category_rules() {
        assert_eq!(cats("PlayerStart"), vec!["player_starts"]);
        assert_eq!(cats("StaticMeshActor"), vec!["static_mesh_actors"]);
        assert_eq!(cats("PointLightToggleable"), vec!["lights"]);
        assert_eq!(cats("SpotLight"), vec!["lights"]);
        assert_eq!(cats("PointLightComponent"), vec!["light_components"]);
        assert_eq!(cats("TriggerVolume"), vec!["triggers_volumes"]);
        assert_eq!(cats("BlockingVolume"), vec!["triggers_volumes"]);
        assert_eq!(cats("SeqAct_Interp"), vec!["kismet", "matinee"]);
        assert_eq!(cats("Sequence"), vec!["kismet"]);
        assert_eq!(cats("InterpActor"), vec!["matinee"]);
        assert_eq!(cats("AmbientSoundSimple"), vec!["sound"]);
        assert!(cats("Texture2D").is_empty());
    }
}
