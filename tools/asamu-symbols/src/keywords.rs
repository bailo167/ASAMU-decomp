//! Keyword search over demangled names (sanitized output: counts + names only).
//!
//! Identifiers are split into CamelCase / underscore tokens
//! (`UASAMUSystemSettingsManager` → `UASAMU System Settings Manager`,
//! `ENGINE_NotifyJumpApex` → `ENGINE Notify Jump Apex`) and a keyword matches a
//! token it is a case-insensitive prefix of (`Rope` matches `Ropes`, not
//! `Property`). `ASAMU` and `Grapple` additionally use a loose case-insensitive
//! substring match so a zero result is as strong as possible. Both the token and
//! the substring counts are reported for every keyword.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::analysis::{Analysis, Entry};
use crate::classify::Category;
use crate::demangle::{split_owner, strip_template_args};
use crate::macho::SymKind;

/// How a keyword is matched for the primary count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MatchMode {
    /// Case-insensitive prefix of a CamelCase token.
    TokenPrefix,
    /// Case-insensitive token equality.
    TokenExact,
    /// Case-insensitive substring of the whole name.
    Substring,
}

/// One keyword.
#[derive(Debug, Clone, Copy)]
pub struct KeywordSpec {
    /// The keyword.
    pub keyword: &'static str,
    /// Primary match mode.
    pub mode: MatchMode,
    /// Requested by the task (true) or added because the data suggested it.
    pub requested: bool,
}

const fn kw(keyword: &'static str, mode: MatchMode, requested: bool) -> KeywordSpec {
    KeywordSpec {
        keyword,
        mode,
        requested,
    }
}

/// Keywords searched, in output order.
pub const KEYWORDS: &[KeywordSpec] = &[
    kw("ASAMU", MatchMode::Substring, true),
    kw("Grapple", MatchMode::Substring, true),
    kw("Hook", MatchMode::TokenPrefix, true),
    kw("Tether", MatchMode::TokenPrefix, true),
    kw("Player", MatchMode::TokenPrefix, true),
    kw("Pawn", MatchMode::TokenPrefix, true),
    kw("Controller", MatchMode::TokenPrefix, true),
    kw("Movement", MatchMode::TokenPrefix, true),
    kw("Velocity", MatchMode::TokenPrefix, true),
    kw("Jump", MatchMode::TokenPrefix, true),
    kw("Swing", MatchMode::TokenPrefix, true),
    kw("Rope", MatchMode::TokenPrefix, true),
    kw("Target", MatchMode::TokenPrefix, true),
    kw("Checkpoint", MatchMode::TokenPrefix, true),
    kw("Respawn", MatchMode::TokenPrefix, true),
    kw("Death", MatchMode::TokenPrefix, true),
    kw("Camera", MatchMode::TokenPrefix, true),
    kw("FOV", MatchMode::TokenPrefix, true),
    kw("Input", MatchMode::TokenPrefix, true),
    kw("Save", MatchMode::TokenPrefix, true),
    kw("Progress", MatchMode::TokenPrefix, true),
    kw("Collectible", MatchMode::TokenPrefix, true),
    kw("Story", MatchMode::TokenPrefix, true),
    kw("Narrative", MatchMode::TokenPrefix, true),
    kw("Sequence", MatchMode::TokenPrefix, true),
    kw("Kismet", MatchMode::TokenPrefix, true),
    kw("Matinee", MatchMode::TokenPrefix, true),
    kw("Seq", MatchMode::TokenExact, false),
    kw("Interp", MatchMode::TokenPrefix, false),
    kw("Physics", MatchMode::TokenPrefix, false),
    kw("Trace", MatchMode::TokenPrefix, false),
    kw("Fall", MatchMode::TokenPrefix, false),
    kw("Landed", MatchMode::TokenPrefix, false),
    kw("Constraint", MatchMode::TokenPrefix, false),
    kw("Spline", MatchMode::TokenPrefix, false),
    kw("Ladder", MatchMode::TokenPrefix, false),
    kw("Swim", MatchMode::TokenPrefix, false),
    kw("Crouch", MatchMode::TokenPrefix, false),
    kw("Teleport", MatchMode::TokenPrefix, false),
];

/// Maximum names listed per keyword.
pub const NAMES_PER_KEYWORD: usize = 25;

/// Split an identifier-ish string into CamelCase / underscore tokens.
pub fn tokens(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for word in s.split(|c: char| !c.is_ascii_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        let b = word.as_bytes();
        let mut start = 0usize;
        for i in 1..b.len() {
            let prev = b[i - 1];
            let cur = b[i];
            let next = b.get(i + 1).copied();
            let lower_to_upper =
                (prev.is_ascii_lowercase() || prev.is_ascii_digit()) && cur.is_ascii_uppercase();
            let acronym_end = prev.is_ascii_uppercase()
                && cur.is_ascii_uppercase()
                && next.map(|n| n.is_ascii_lowercase()).unwrap_or(false);
            if lower_to_upper || acronym_end {
                if let Some(t) = word.get(start..i) {
                    out.push(t);
                }
                start = i;
            }
        }
        if let Some(t) = word.get(start..) {
            out.push(t);
        }
    }
    out
}

/// Does the keyword match `text` in the given mode?
pub fn matches(text: &str, keyword: &str, mode: MatchMode) -> bool {
    let kw = keyword.to_ascii_lowercase();
    match mode {
        MatchMode::Substring => text.to_ascii_lowercase().contains(&kw),
        MatchMode::TokenExact => tokens(text).iter().any(|t| t.eq_ignore_ascii_case(&kw)),
        MatchMode::TokenPrefix => tokens(text).iter().any(|t| {
            t.len() >= kw.len()
                && t.get(..kw.len()).map(|p| p.eq_ignore_ascii_case(&kw)) == Some(true)
        }),
    }
}

/// Result for one keyword.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct KeywordResult {
    /// Primary match mode.
    pub mode: String,
    /// Requested by the task.
    pub requested: bool,
    /// Symbols matching in the primary mode.
    pub symbols: u64,
    /// Symbols matching as a loose case-insensitive substring.
    pub substring_symbols: u64,
    /// Primary matches per category.
    pub by_category: BTreeMap<String, u64>,
    /// Distinct game-layer (asamu/udk/ue3) function/data names matching.
    pub game_layer_distinct_names: u64,
    /// Up to [`NAMES_PER_KEYWORD`] most relevant game-layer names
    /// (`Class::function`, templates and parameters removed).
    pub top_names: Vec<String>,
}

/// Boilerplate leaves generated by `DECLARE_CLASS` / `IMPLEMENT_CLASS`.
fn is_boilerplate_leaf(owner: &str, leaf: &str) -> bool {
    let owner_leaf = owner.rsplit("::").next().unwrap_or(owner);
    leaf.starts_with("GetPrivateStaticClass")
        || leaf.starts_with("InitializePrivateStaticClass")
        || matches!(
            leaf,
            "InternalConstructor"
                | "HasParentClassChanged"
                | "HasUniqueStaticConfigName"
                | "PrivateStaticClass"
                | "StaticConstructor"
        )
        || (!owner_leaf.is_empty() && leaf == owner_leaf)
}

fn is_noise(a: &Analysis, e: &Entry) -> bool {
    if e.category == Category::Compiler {
        return true;
    }
    let full = a.display(e);
    if crate::classify::is_special_name(full) || full.contains('~') {
        return true;
    }
    let q = a.qualified(e);
    // Function-local statics and local classes (`F(int)::x`), initialisers.
    if q.contains('(') || q.starts_with("_GLOBAL__sub_I_") {
        return true;
    }
    // `int<Class>exec<Func>` registration pointers duplicate the exec thunk.
    if e.full.is_none() && q.starts_with("int") && q.contains("exec") {
        return true;
    }
    let (owner, leaf) = split_owner(q);
    is_boilerplate_leaf(owner, leaf)
}

/// Short display name used in sanitized lists: qualified name with template
/// arguments removed, capped at 120 characters.
pub fn short_name(a: &Analysis, e: &Entry) -> String {
    let mut s = strip_template_args(a.qualified(e));
    if s.len() > 120 {
        let mut cut = 120;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push('…');
    }
    s
}

/// Ranking key (lower is more relevant): core gameplay class members first,
/// then the ASAMU/UDKBase/GameFramework modules, then Engine actor/object
/// members, then everything else; functions before data; then by name.
fn relevance(a: &Analysis, e: &Entry) -> (u8, u8, u8, u8) {
    let q = a.qualified(e);
    let (owner, _) = split_owner(q);
    let owner_plain = strip_template_args(owner);
    let gameplay = if crate::anchors::GAMEPLAY_CLASSES.contains(&owner_plain.as_str()) {
        0
    } else {
        1
    };
    let module = match a.module(e) {
        Some("ASAMU") => 0,
        Some("UDKBase") | Some("GameFramework") => 1,
        Some("Engine") if owner.starts_with('A') || owner.starts_with('U') => 2,
        _ => 3,
    };
    let kind = match a.symbol(e).map(|s| s.kind) {
        Some(SymKind::Text) => 0,
        _ => 1,
    };
    let member = u8::from(owner.is_empty());
    (gameplay, module, kind, member)
}

/// Pre-computed, lowercased match material for one entry.
struct Material {
    /// Lowercase raw name + "\u{0}" + lowercase qualified name.
    loose: String,
    /// Lowercase tokens of the template-stripped qualified name.
    tokens: Vec<String>,
}

/// Run every keyword.
pub fn search(a: &Analysis) -> BTreeMap<String, KeywordResult> {
    let materials: Vec<Option<Material>> = a
        .entries
        .iter()
        .map(|e| {
            let sym = a.symbol(e)?;
            let subject = a.qualified(e);
            let stripped = strip_template_args(subject);
            Some(Material {
                loose: format!(
                    "{}\u{0}{}",
                    sym.raw.to_ascii_lowercase(),
                    subject.to_ascii_lowercase()
                ),
                tokens: tokens(&stripped)
                    .into_iter()
                    .map(str::to_ascii_lowercase)
                    .collect(),
            })
        })
        .collect();

    let mut out = BTreeMap::new();
    for spec in KEYWORDS {
        let kw = spec.keyword.to_ascii_lowercase();
        let mut res = KeywordResult {
            mode: match spec.mode {
                MatchMode::TokenPrefix => "token-prefix".into(),
                MatchMode::TokenExact => "token-exact".into(),
                MatchMode::Substring => "substring".into(),
            },
            requested: spec.requested,
            ..KeywordResult::default()
        };
        let mut ranked: BTreeMap<String, (u8, u8, u8, u8)> = BTreeMap::new();
        for (e, m) in a.entries.iter().zip(materials.iter()) {
            let Some(m) = m else { continue };
            let loose = m.loose.contains(&kw);
            if loose {
                res.substring_symbols = res.substring_symbols.saturating_add(1);
            }
            let hit = match spec.mode {
                MatchMode::Substring => loose,
                MatchMode::TokenExact => m.tokens.contains(&kw),
                MatchMode::TokenPrefix => m.tokens.iter().any(|t| t.starts_with(&kw)),
            };
            if !hit {
                continue;
            }
            res.symbols = res.symbols.saturating_add(1);
            let c = res
                .by_category
                .entry(e.category.id().to_string())
                .or_insert(0);
            *c = c.saturating_add(1);
            if e.category.is_game_layer() && !is_noise(a, e) {
                let score = relevance(a, e);
                ranked
                    .entry(short_name(a, e))
                    .and_modify(|s| {
                        if score < *s {
                            *s = score;
                        }
                    })
                    .or_insert(score);
            }
        }
        res.game_layer_distinct_names = u64::try_from(ranked.len()).unwrap_or(u64::MAX);
        let mut list: Vec<(String, (u8, u8, u8, u8))> = ranked.into_iter().collect();
        list.sort_by(|x, y| x.1.cmp(&y.1).then_with(|| x.0.cmp(&y.0)));
        res.top_names = list
            .into_iter()
            .take(NAMES_PER_KEYWORD)
            .map(|(n, _)| n)
            .collect();
        out.insert(spec.keyword.to_string(), res);
    }
    out
}

/// Leaf names (after `Class::`) treated as movement-physics functions in
/// addition to every `phys[A-Z]*` leaf.
pub const PHYSICS_LEAVES: &[&str] = &[
    "performPhysics",
    "startNewPhysics",
    "CalcVelocity",
    "NewFallVelocity",
    "processLanded",
    "processHitWall",
    "stepUp",
    "moveSmooth",
    "physicsRotation",
    "SmoothHitWall",
    "FindBase",
    "CalculateSlopeSlide",
    "MaxSpeedModifier",
    "FindSlopeRotation",
    "setPhysics",
    "SetPhysics",
    "MoveActor",
    "FarMoveActor",
    "SingleLineCheck",
    "MultiLineCheck",
    "CheckStillInWorld",
    "SetBase",
    "TwoWallAdjust",
    "PostNetReceiveLocation",
];

/// One movement-physics function.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysicsFn {
    /// `Class::function`.
    pub name: String,
    /// Demangled signature.
    pub signature: String,
    /// Address (hex).
    pub address: String,
    /// Sanitized compilation unit (`Engine/Src/UnPhysic.cpp`), if known.
    pub unit: Option<String>,
}

/// Is this qualified name a movement-physics function?
pub fn is_physics_fn(qualified: &str) -> bool {
    let (owner, leaf) = split_owner(qualified);
    if owner.is_empty() || owner.contains('(') {
        return false;
    }
    let phys = leaf
        .strip_prefix("phys")
        .and_then(|r| r.chars().next())
        .map(|c| c.is_ascii_uppercase())
        .unwrap_or(false);
    phys || PHYSICS_LEAVES.contains(&leaf)
}

/// All game-layer movement-physics functions (text symbols), sorted by name.
pub fn physics_functions(a: &Analysis) -> Vec<PhysicsFn> {
    let mut out = Vec::new();
    for e in &a.entries {
        let Some(sym) = a.symbol(e) else { continue };
        if sym.kind != SymKind::Text || !e.category.is_game_layer() {
            continue;
        }
        let Some(q) = e.qualified.as_deref() else {
            continue;
        };
        if !is_physics_fn(q) {
            continue;
        }
        out.push(PhysicsFn {
            name: strip_template_args(q),
            signature: a.display(e).to_string(),
            address: format!("0x{:x}", sym.value),
            unit: a.unit(e).map(|u| u.rel_path.clone()),
        });
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_case_tokens() {
        assert_eq!(
            tokens("UASAMUSystemSettingsManager"),
            ["UASAMU", "System", "Settings", "Manager"]
        );
        assert_eq!(
            tokens("APawn::physWalking"),
            ["A", "Pawn", "phys", "Walking"]
        );
        assert_eq!(
            tokens("ENGINE_NotifyJumpApex"),
            ["ENGINE", "Notify", "Jump", "Apex"]
        );
        assert_eq!(tokens("FOVAngle"), ["FOV", "Angle"]);
        assert_eq!(tokens("USeqAct_Interp"), ["U", "Seq", "Act", "Interp"]);
        assert_eq!(tokens("Vector3d"), ["Vector3d"]);
        assert_eq!(tokens("a2DView"), ["a2", "D", "View"]);
        assert_eq!(tokens("Swing1Limit"), ["Swing1", "Limit"]);
        assert!(tokens("").is_empty());
        assert!(tokens("::<>").is_empty());
    }

    #[test]
    fn keyword_matching_modes() {
        assert!(matches(
            "UProperty::Serialize",
            "Rope",
            MatchMode::Substring
        ));
        assert!(!matches(
            "UProperty::Serialize",
            "Rope",
            MatchMode::TokenPrefix
        ));
        assert!(matches(
            "AGrappleRope::Ropes",
            "Rope",
            MatchMode::TokenPrefix
        ));
        assert!(matches("GasamuUFooNatives", "ASAMU", MatchMode::Substring));
        assert!(matches(
            "APlayerController::GetFOVAngle",
            "FOV",
            MatchMode::TokenPrefix
        ));
        assert!(matches(
            "USeqAct_Interp::Activated",
            "Seq",
            MatchMode::TokenExact
        ));
        assert!(!matches(
            "USequence::Activated",
            "Seq",
            MatchMode::TokenExact
        ));
        assert!(!matches("UHistory", "Story", MatchMode::TokenPrefix));
    }

    #[test]
    fn physics_function_detection() {
        assert!(is_physics_fn("APawn::physWalking"));
        assert!(is_physics_fn("AActor::moveSmooth"));
        assert!(is_physics_fn("APawn::CalcVelocity"));
        assert!(!is_physics_fn("APawn::physical"));
        assert!(!is_physics_fn("physWalking"));
        assert!(!is_physics_fn("F(int)::physX"));
    }
}
