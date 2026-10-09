//! Per-symbol enrichment: demangling + classification + provenance.

use crate::classify::{Category, View, classify};
use crate::demangle::{Demangled, demangle, strip_macho_underscore};
use crate::macho::{Image, SymKind, Symbol};
use crate::provenance::{CompileUnit, Component};

/// Demangling status of one symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemangleStatus {
    /// Not an Itanium name.
    NotMangled,
    /// Demangled directly.
    Ok,
    /// Demangled after stripping a GCC suffix such as `.b`.
    OkSuffixStripped,
    /// Itanium-looking but failed.
    Failed,
}

/// One enriched symbol.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Index into [`Image::symbols`].
    pub index: usize,
    /// Full demangled text.
    pub full: Option<String>,
    /// Qualified demangled name (no params / return type).
    pub qualified: Option<String>,
    /// Demangle status.
    pub demangle: DemangleStatus,
    /// Category.
    pub category: Category,
    /// Id of the classification rule that matched.
    pub rule: &'static str,
}

/// Image plus enriched entries (same order as `image.symbols`).
#[derive(Debug, Clone)]
pub struct Analysis {
    /// Parsed image.
    pub image: Image,
    /// Enriched entries.
    pub entries: Vec<Entry>,
}

impl Analysis {
    /// Demangle and classify every symbol.
    pub fn new(image: Image) -> Analysis {
        let mut entries = Vec::with_capacity(image.symbols.len());
        for (index, sym) in image.symbols.iter().enumerate() {
            let (full, qualified, status) = match demangle(&sym.raw) {
                Demangled::NotMangled => (None, None, DemangleStatus::NotMangled),
                Demangled::Ok {
                    full,
                    qualified,
                    stripped_suffix,
                } => (
                    Some(full),
                    Some(qualified),
                    if stripped_suffix.is_some() {
                        DemangleStatus::OkSuffixStripped
                    } else {
                        DemangleStatus::Ok
                    },
                ),
                Demangled::Failed => (None, None, DemangleStatus::Failed),
            };
            let component = image.unit_of(sym).map(|u| &u.component);
            let view = View {
                raw: &sym.raw,
                name: strip_macho_underscore(&sym.raw),
                full: full.as_deref(),
                qualified: qualified.as_deref(),
                kind: sym.kind,
                component,
                dylib: if sym.kind == SymKind::Undefined {
                    image.dylib_of(sym)
                } else {
                    None
                },
            };
            let (category, rule) = classify(&view);
            entries.push(Entry {
                index,
                full,
                qualified,
                demangle: status,
                category,
                rule,
            });
        }
        Analysis { image, entries }
    }

    /// The symbol behind an entry.
    pub fn symbol(&self, e: &Entry) -> Option<&Symbol> {
        self.image.symbols.get(e.index)
    }

    /// Compile unit behind an entry.
    pub fn unit(&self, e: &Entry) -> Option<&CompileUnit> {
        self.symbol(e).and_then(|s| self.image.unit_of(s))
    }

    /// Component id behind an entry (`ue3:Engine`, ..., or `dylib:<name>` for
    /// imports, `none` otherwise).
    pub fn origin_id(&self, e: &Entry) -> String {
        let Some(sym) = self.symbol(e) else {
            return "none".into();
        };
        if sym.kind == SymKind::Undefined {
            return match self.image.dylib_of(sym) {
                Some(d) => format!("dylib:{d}"),
                None => "dylib:?".into(),
            };
        }
        match self.image.unit_of(sym) {
            Some(u) => u.component.id(),
            None => "none".into(),
        }
    }

    /// UE3 module of an entry, if its unit is a UE3 module.
    pub fn module(&self, e: &Entry) -> Option<&str> {
        self.unit(e).and_then(|u| match &u.component {
            Component::Ue3Module(m) => Some(m.as_str()),
            _ => None,
        })
    }

    /// Display name: demangled text or the underscore-stripped raw name.
    pub fn display<'a>(&'a self, e: &'a Entry) -> &'a str {
        match (&e.full, self.symbol(e)) {
            (Some(f), _) => f,
            (None, Some(s)) => strip_macho_underscore(&s.raw),
            (None, None) => "",
        }
    }

    /// Qualified name (demangled without params) or the plain C name.
    pub fn qualified<'a>(&'a self, e: &'a Entry) -> &'a str {
        match (&e.qualified, self.symbol(e)) {
            (Some(q), _) => q,
            (None, Some(s)) => strip_macho_underscore(&s.raw),
            (None, None) => "",
        }
    }
}
