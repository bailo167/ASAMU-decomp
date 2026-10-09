//! Itanium C++ demangling for Mach-O symbol names.
//!
//! Mach-O prefixes every C-level name with one `_`, so `__ZN5APawn11physWalkingEfi`
//! is the Itanium name `_ZN5APawn11physWalkingEfi`. Names are demangled with
//! `cpp_demangle`; GCC-style local-variable suffixes (`.b`, `.0`, ...) that the
//! demangler rejects are retried without the suffix. The demangler is wrapped in
//! `catch_unwind` and given a recursion limit, so a hostile name can only fail,
//! never abort the tool.

use cpp_demangle::{DemangleOptions, ParseOptions, Symbol};

/// Names longer than this are not handed to the demangler.
pub const MAX_MANGLED_LEN: usize = 16 * 1024;
const RECURSION_LIMIT: u32 = 256;

/// Result of demangling one raw symbol name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Demangled {
    /// Not an Itanium name (C symbol, Objective-C, compiler label, ...).
    NotMangled,
    /// Demangled successfully.
    Ok {
        /// Full demangled text, e.g. `APawn::physWalking(float, int)`.
        full: String,
        /// Qualified name without parameters/return type, e.g. `APawn::physWalking`.
        qualified: String,
        /// A suffix that had to be stripped (e.g. `.b`), if any.
        stripped_suffix: Option<String>,
    },
    /// Looked like an Itanium name but could not be demangled.
    Failed,
}

/// Strip exactly one Mach-O leading underscore, if present.
pub fn strip_macho_underscore(raw: &str) -> &str {
    raw.strip_prefix('_').unwrap_or(raw)
}

/// True if the (underscore-stripped) name is an Itanium mangled name.
pub fn is_itanium(name: &str) -> bool {
    name.starts_with("_Z")
}

fn try_demangle(mangled: &str) -> Option<(String, String)> {
    let parse = ParseOptions::default().recursion_limit(RECURSION_LIMIT);
    let full_opts = DemangleOptions::new().recursion_limit(RECURSION_LIMIT);
    let short_opts = DemangleOptions::new()
        .no_params()
        .no_return_type()
        .recursion_limit(RECURSION_LIMIT);
    let bytes = mangled.as_bytes();
    let outcome = std::panic::catch_unwind(|| {
        let sym = Symbol::new_with_options(bytes, &parse).ok()?;
        let full = sym.demangle_with_options(&full_opts).ok()?;
        let qualified = sym.demangle_with_options(&short_opts).ok()?;
        Some((full, qualified))
    });
    match outcome {
        Ok(Some((full, qualified))) if !full.is_empty() => Some((full, qualified)),
        _ => None,
    }
}

/// Demangle a raw Mach-O symbol name.
pub fn demangle(raw: &str) -> Demangled {
    let name = strip_macho_underscore(raw);
    if !is_itanium(name) {
        return Demangled::NotMangled;
    }
    if name.len() > MAX_MANGLED_LEN {
        return Demangled::Failed;
    }
    if let Some((full, qualified)) = try_demangle(name) {
        return Demangled::Ok {
            full,
            qualified,
            stripped_suffix: None,
        };
    }
    // GCC emits local statics such as `_ZL5gDone.b` / `_ZZ...E10SrcNormals.0`.
    if let Some(dot) = name.find('.') {
        let (base, suffix) = name.split_at(dot);
        if let Some((full, qualified)) = try_demangle(base) {
            return Demangled::Ok {
                full: format!("{full} [{suffix}]"),
                qualified,
                stripped_suffix: Some(suffix.to_string()),
            };
        }
    }
    Demangled::Failed
}

/// Remove balanced template argument lists: `TArray<int, A<B>>::Add` → `TArray::Add`.
/// `operator<`, `operator<<`, `operator<=` and `operator->` are preserved.
pub fn strip_template_args(s: &str) -> String {
    const OPERATORS: [&str; 9] = ["<<=", "<<", "<=", "<", ">>=", ">>", ">=", ">", "->"];
    let mut out = String::with_capacity(s.len());
    let mut depth: usize = 0;
    let mut chars = s.char_indices();
    while let Some((idx, c)) = chars.next() {
        if depth == 0 && out.ends_with("operator") {
            let tail = s.get(idx..).unwrap_or("");
            if let Some(op) = OPERATORS.iter().find(|op| tail.starts_with(**op)) {
                out.push_str(op);
                // Operators are ASCII: skip the remaining bytes of the operator.
                for _ in 1..op.len() {
                    chars.next();
                }
                continue;
            }
        }
        match c {
            '<' => depth = depth.saturating_add(1),
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Split a qualified C++ name into (owner, leaf) at the last top-level `::`.
/// `APawn::physWalking` → (`APawn`, `physWalking`); `foo` → (``, `foo`).
pub fn split_owner(qualified: &str) -> (&str, &str) {
    let bytes = qualified.as_bytes();
    let mut depth: i32 = 0;
    let mut split: Option<usize> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'<' | b'(' => depth += 1,
            b'>' | b')' => depth -= 1,
            b':' if depth == 0 && bytes.get(i + 1) == Some(&b':') => {
                split = Some(i);
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    match split {
        Some(pos) => (
            qualified.get(..pos).unwrap_or(""),
            qualified.get(pos + 2..).unwrap_or(""),
        ),
        None => ("", qualified),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(raw: &str) -> (String, String) {
        match demangle(raw) {
            Demangled::Ok {
                full, qualified, ..
            } => (full, qualified),
            other => panic!("{raw}: {other:?}"),
        }
    }

    #[test]
    fn macho_underscore_is_stripped_once() {
        let (full, q) = ok("__ZN5APawn11physWalkingEfi");
        assert_eq!(full, "APawn::physWalking(float, int)");
        assert_eq!(q, "APawn::physWalking");
        // Without the Mach-O underscore it is not an Itanium name.
        assert_eq!(demangle("_ZN5APawn11physWalkingEfi"), Demangled::NotMangled);
    }

    #[test]
    fn plain_c_and_objc_names_are_not_mangled() {
        for raw in [
            "_inflate",
            "GCC_except_table12",
            "_OBJC_CLASS_$_NSObject",
            "-[Foo bar]",
            "",
            "_",
            "__",
            "___cxa_atexit",
        ] {
            assert_eq!(demangle(raw), Demangled::NotMangled, "{raw}");
        }
    }

    #[test]
    fn special_names() {
        let (full, _) = ok("__ZTV27UASAMUSystemSettingsManager");
        assert_eq!(full, "{vtable(UASAMUSystemSettingsManager)}");
        let (full, q) = ok("__ZN27UASAMUSystemSettingsManager18PrivateStaticClassE");
        assert_eq!(full, "UASAMUSystemSettingsManager::PrivateStaticClass");
        assert_eq!(q, full);
        let (full, _) = ok("__Z30AutoInitializeRegistrantsASAMURi");
        assert_eq!(full, "AutoInitializeRegistrantsASAMU(int&)");
    }

    #[test]
    fn gcc_local_suffixes_are_handled() {
        match demangle("__ZL5gDone.b") {
            Demangled::Ok { qualified, .. } => assert_eq!(qualified, "gDone"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn garbage_fails_without_panicking() {
        for raw in [
            "__Z",
            "__Zx",
            "__ZN",
            "__ZN5APawn",
            "__ZN999999999999999999A",
            "__ZSt",
            "__Z1",
            "__ZNKSt3__112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEE",
        ] {
            let _ = demangle(raw);
        }
        let deep = format!("__Z{}", "PPPPPPPPPP".repeat(500));
        let _ = demangle(&deep);
        let long = format!("__Z{}", "x".repeat(MAX_MANGLED_LEN + 10));
        assert_eq!(demangle(&long), Demangled::Failed);
    }

    #[test]
    fn template_stripping() {
        assert_eq!(
            strip_template_args("TArray<int, FDefaultAllocator>::Add"),
            "TArray::Add"
        );
        assert_eq!(strip_template_args("A<B<C>>::f"), "A::f");
        assert_eq!(
            strip_template_args("FVector::operator<"),
            "FVector::operator<"
        );
        assert_eq!(
            strip_template_args("FArchive::operator<<"),
            "FArchive::operator<<"
        );
        assert_eq!(strip_template_args("unbalanced>"), "unbalanced>");
    }

    #[test]
    fn owner_split() {
        assert_eq!(split_owner("APawn::physWalking"), ("APawn", "physWalking"));
        assert_eq!(split_owner("a::b::c"), ("a::b", "c"));
        assert_eq!(split_owner("T<a::b>::f"), ("T<a::b>", "f"));
        assert_eq!(split_owner("free"), ("", "free"));
        assert_eq!(split_owner(""), ("", ""));
    }
}
