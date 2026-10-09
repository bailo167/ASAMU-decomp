//! Decoder coverage over real packages: exact payload consumption of script
//! objects, class default objects and (optionally) every other export, plus
//! structural cross-checks of the decoded tagged properties.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use crate::flags;
use crate::model::{LoadedPackage, PackageSet};
use crate::property::Property;
use crate::schema::Schema;
use crate::script::{KindCoverage, ScriptKind, script_coverage};

/// Most failure/warning samples kept per statistic.
const MAX_SAMPLES: usize = 8;

/// Statistics over generically decoded objects (prelude + tagged properties).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ObjectStats {
    /// Objects examined.
    pub total: usize,
    /// Objects whose prelude and tagged stream decoded.
    pub decoded: usize,
    /// Objects whose tagged stream ends exactly at `SerialSize`.
    pub exact: usize,
    /// Objects with class-specific native data after the tagged stream.
    pub native_tail: usize,
    /// Objects with at least one raw (undecoded) value.
    pub with_raw_values: usize,
    /// Objects with at least one decoding warning.
    pub with_warnings: usize,
    /// Objects whose tags are not in the class's property-link order.
    pub order_violations: usize,
    /// Tags whose name is not a property of the object's (known) class.
    pub undeclared_tags: usize,
    /// Objects with tags whose class definition was not found in the set.
    pub unknown_class: usize,
    /// Classes not found (first few).
    pub unknown_classes: Vec<String>,
    /// Tags per tag type name (top level only).
    pub tag_types: BTreeMap<String, usize>,
    /// First failures.
    pub failures: Vec<String>,
    /// First warnings.
    pub warning_samples: Vec<String>,
}

/// Coverage of one package.
#[derive(Debug, Clone, Serialize)]
pub struct PackageCoverage {
    /// Package name.
    pub package: String,
    /// Script objects per kind.
    pub script: BTreeMap<ScriptKind, KindCoverage>,
    /// Class default objects.
    pub cdo: ObjectStats,
    /// Every other non-script export (when requested).
    pub objects: Option<ObjectStats>,
}

/// Position of each tag in the property link of `owner`; returns
/// (order violation found, undeclared tag count).
pub fn check_tag_order(schema: &dyn Schema, owner: &str, props: &[Property]) -> (bool, usize) {
    let link = schema.property_link(owner);
    let mut pos: HashMap<String, usize> = HashMap::with_capacity(link.len());
    for (i, d) in link.iter().enumerate() {
        pos.entry(d.name.to_ascii_lowercase()).or_insert(i);
    }
    let mut last: Option<(usize, i32)> = None;
    let mut violation = false;
    let mut undeclared = 0usize;
    for p in props {
        let Some(&i) = pos.get(&p.name.to_ascii_lowercase()) else {
            undeclared += 1;
            continue;
        };
        if let Some((li, lidx)) = last
            && (i < li || (i == li && p.array_index <= lidx))
        {
            violation = true;
        }
        last = Some((i, p.array_index));
    }
    (violation, undeclared)
}

fn record(stats: &mut ObjectStats, set: &PackageSet, lp: &LoadedPackage, i: usize) {
    stats.total += 1;
    match set.decode(lp, i) {
        Ok(o) => {
            stats.decoded += 1;
            if o.native_tail() == 0 {
                stats.exact += 1;
            } else {
                stats.native_tail += 1;
            }
            if o.properties.iter().any(|p| p.value.has_raw()) {
                stats.with_raw_values += 1;
            }
            if !o.warnings.is_empty() {
                stats.with_warnings += 1;
                for w in o.warnings.iter().take(2) {
                    if stats.warning_samples.len() < MAX_SAMPLES {
                        stats.warning_samples.push(format!("{}: {w}", o.path));
                    }
                }
            }
            for p in &o.properties {
                *stats.tag_types.entry(p.type_name.clone()).or_insert(0) += 1;
            }
            if !o.properties.is_empty() && set.struct_def(&o.class).is_none() {
                stats.unknown_class += 1;
                if stats.unknown_classes.len() < MAX_SAMPLES * 4
                    && !stats.unknown_classes.contains(&o.class)
                {
                    stats.unknown_classes.push(o.class.clone());
                }
            } else {
                let (violation, undeclared) = check_tag_order(set, &o.class, &o.properties);
                if violation {
                    stats.order_violations += 1;
                }
                stats.undeclared_tags += undeclared;
            }
        }
        Err(e) => {
            if stats.failures.len() < MAX_SAMPLES {
                stats
                    .failures
                    .push(format!("{i} {}: {e}", lp.qualified(i).unwrap_or_default()));
            }
        }
    }
}

/// Coverage of `lp`, using `set` as schema. With `all_objects`, every
/// non-script, non-CDO export is decoded generically too.
pub fn package_coverage(
    set: &PackageSet,
    lp: &LoadedPackage,
    all_objects: bool,
) -> PackageCoverage {
    let script = script_coverage(&lp.package, Some(&lp.name));
    let mut cdo = ObjectStats::default();
    let mut objects = all_objects.then(ObjectStats::default);
    for (i, e) in lp.package.exports.iter().enumerate() {
        if e.object_flags & flags::object::CLASS_DEFAULT_OBJECT != 0 {
            record(&mut cdo, set, lp, i);
        } else if let Some(stats) = objects.as_mut()
            && ScriptKind::of_export(&lp.package, i).is_none()
        {
            record(stats, set, lp, i);
        }
    }
    PackageCoverage {
        package: lp.name.clone(),
        script,
        cdo,
        objects,
    }
}

impl ObjectStats {
    /// Add another set of statistics.
    pub fn absorb(&mut self, o: &ObjectStats) {
        self.total += o.total;
        self.decoded += o.decoded;
        self.exact += o.exact;
        self.native_tail += o.native_tail;
        self.with_raw_values += o.with_raw_values;
        self.with_warnings += o.with_warnings;
        self.order_violations += o.order_violations;
        self.undeclared_tags += o.undeclared_tags;
        self.unknown_class += o.unknown_class;
        for c in &o.unknown_classes {
            if self.unknown_classes.len() < MAX_SAMPLES * 4 && !self.unknown_classes.contains(c) {
                self.unknown_classes.push(c.clone());
            }
        }
        for (k, v) in &o.tag_types {
            *self.tag_types.entry(k.clone()).or_insert(0) += v;
        }
        for f in &o.failures {
            if self.failures.len() < MAX_SAMPLES {
                self.failures.push(f.clone());
            }
        }
        for w in &o.warning_samples {
            if self.warning_samples.len() < MAX_SAMPLES {
                self.warning_samples.push(w.clone());
            }
        }
    }
}
