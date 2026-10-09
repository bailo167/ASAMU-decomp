//! Small value types that appear throughout a UE3 package.

use std::fmt;

use serde::{Serialize, Serializer};

/// 128-bit GUID, serialized as four little-endian `u32`s (A, B, C, D).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Guid {
    /// First dword.
    pub a: u32,
    /// Second dword.
    pub b: u32,
    /// Third dword.
    pub c: u32,
    /// Fourth dword.
    pub d: u32,
}

impl Guid {
    /// True when all four dwords are zero.
    pub fn is_zero(&self) -> bool {
        self.a == 0 && self.b == 0 && self.c == 0 && self.d == 0
    }
}

impl fmt::Display for Guid {
    /// UE3 style: 32 upper-case hex digits, A B C D in order.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:08X}{:08X}{:08X}{:08X}",
            self.a, self.b, self.c, self.d
        )
    }
}

impl Serialize for Guid {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

/// A serialized name reference: index into the name table plus an instance number.
///
/// `number == 0` means no suffix; `number > 0` displays as `Name_{number-1}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize)]
pub struct FName {
    /// Index into the package name table (validated when the table is resolved).
    pub index: i32,
    /// Instance number (0 = none).
    pub number: i32,
}

impl FName {
    /// Format with the UE3 instance suffix rule given the base name string.
    pub fn display_with(&self, base: &str) -> String {
        if self.number > 0 {
            // number - 1 cannot overflow because number > 0.
            format!("{base}_{}", i64::from(self.number) - 1)
        } else {
            base.to_owned()
        }
    }
}

/// UE3 object reference inside a package (`FPackageIndex` semantics):
/// `0` = null, `> 0` = export `i - 1`, `< 0` = import `-i - 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize)]
#[serde(transparent)]
pub struct PackageIndex(pub i32);

/// Decoded form of a [`PackageIndex`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndexKind {
    /// Null reference.
    Null,
    /// Zero-based import-table index.
    Import(usize),
    /// Zero-based export-table index.
    Export(usize),
}

impl PackageIndex {
    /// The null reference.
    pub const NULL: PackageIndex = PackageIndex(0);

    /// Reference to zero-based export `i`, if representable.
    pub fn from_export(i: usize) -> Option<Self> {
        let v = i32::try_from(i.checked_add(1)?).ok()?;
        Some(PackageIndex(v))
    }

    /// Reference to zero-based import `i`, if representable.
    pub fn from_import(i: usize) -> Option<Self> {
        let v = i64::try_from(i).ok()?.checked_add(1)?;
        let v = i32::try_from(-v).ok()?;
        Some(PackageIndex(v))
    }

    /// True for the null reference.
    pub fn is_null(self) -> bool {
        self.0 == 0
    }

    /// Decode into null / import / export without range checking against tables.
    pub fn kind(self) -> IndexKind {
        let v = i64::from(self.0);
        if v == 0 {
            IndexKind::Null
        } else if v > 0 {
            // v - 1 is in [0, i32::MAX - 1]; fits usize on every supported target.
            IndexKind::Export(usize::try_from(v - 1).unwrap_or(usize::MAX))
        } else {
            // -v - 1 is in [0, i32::MAX]; computed in i64 so i32::MIN cannot overflow.
            IndexKind::Import(usize::try_from(-v - 1).unwrap_or(usize::MAX))
        }
    }

    /// Zero-based export index when this refers to an export.
    pub fn export_index(self) -> Option<usize> {
        match self.kind() {
            IndexKind::Export(i) => Some(i),
            _ => None,
        }
    }

    /// Zero-based import index when this refers to an import.
    pub fn import_index(self) -> Option<usize> {
        match self.kind() {
            IndexKind::Import(i) => Some(i),
            _ => None,
        }
    }
}

impl fmt::Display for PackageIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind() {
            IndexKind::Null => write!(f, "null"),
            IndexKind::Import(i) => write!(f, "import[{i}]"),
            IndexKind::Export(i) => write!(f, "export[{i}]"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_index_decoding() {
        assert_eq!(PackageIndex(0).kind(), IndexKind::Null);
        assert_eq!(PackageIndex(1).kind(), IndexKind::Export(0));
        assert_eq!(PackageIndex(-1).kind(), IndexKind::Import(0));
        assert_eq!(
            PackageIndex(i32::MAX).kind(),
            IndexKind::Export(i32::MAX as usize - 1)
        );
        assert_eq!(
            PackageIndex(i32::MIN).kind(),
            IndexKind::Import(i32::MAX as usize)
        );
        assert_eq!(PackageIndex::from_export(0), Some(PackageIndex(1)));
        assert_eq!(PackageIndex::from_import(0), Some(PackageIndex(-1)));
        assert_eq!(
            PackageIndex::from_import(i32::MAX as usize),
            Some(PackageIndex(i32::MIN))
        );
        assert_eq!(PackageIndex::from_export(i32::MAX as usize), None);
    }

    #[test]
    fn fname_suffix() {
        let n = FName {
            index: 0,
            number: 0,
        };
        assert_eq!(n.display_with("Foo"), "Foo");
        let n = FName {
            index: 0,
            number: 1,
        };
        assert_eq!(n.display_with("Foo"), "Foo_0");
        let n = FName {
            index: 0,
            number: 13,
        };
        assert_eq!(n.display_with("Foo"), "Foo_12");
        let n = FName {
            index: 0,
            number: -5,
        };
        assert_eq!(n.display_with("Foo"), "Foo");
    }

    #[test]
    fn guid_display() {
        let g = Guid {
            a: 0x0102_0304,
            b: 0xAABB_CCDD,
            c: 0,
            d: 0xFFFF_FFFF,
        };
        assert_eq!(g.to_string(), "01020304AABBCCDD00000000FFFFFFFF");
        assert!(!g.is_zero());
        assert!(Guid::default().is_zero());
    }
}
