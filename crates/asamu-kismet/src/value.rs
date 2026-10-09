//! Property values as the runtime sees them ([`KValue`]) and their JSON
//! encoding in the importer's runtime graph (`asamu-kismet-runtime`).
//!
//! Encoding (written by `tools/asamu-import/src/kismet.rs`):
//!
//! | JSON | value |
//! |---|---|
//! | `null` | [`KValue::None`] |
//! | `true` / `false` | [`KValue::Bool`] |
//! | integer number | [`KValue::Int`] (out of `i32` range: [`KValue::Float`]) |
//! | number with a fraction or exponent | [`KValue::Float`] |
//! | string | [`KValue::Str`] (strings, names and enumerators) |
//! | `{"$obj": "path"}` / `{"$obj": null}` | [`KValue::Obj`] |
//! | array | [`KValue::Array`] |
//! | `{"$struct": "Name", ...fields}` or any other object | [`KValue::Struct`] |
//!
//! Parsing is bounded ([`MAX_VALUE_DEPTH`], [`MAX_VALUE_ITEMS`]) and never
//! panics on hostile input.

use std::collections::BTreeMap;

/// Deepest nesting of arrays/structs accepted.
pub const MAX_VALUE_DEPTH: usize = 32;
/// Most array elements or struct fields accepted in one value.
pub const MAX_VALUE_ITEMS: usize = 1 << 16;

/// A Kismet property value.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum KValue {
    /// No value (null, or not representable).
    #[default]
    None,
    /// `BoolProperty`.
    Bool(bool),
    /// `IntProperty` / plain `ByteProperty`.
    Int(i32),
    /// `FloatProperty`.
    Float(f32),
    /// `StrProperty`, `NameProperty` or an enumerator name.
    Str(String),
    /// Object reference by path (`None` = null).
    Obj(Option<String>),
    /// Dynamic array.
    Array(Vec<KValue>),
    /// Struct: field name → value (names as stored; look up with
    /// [`KValue::field`], case-insensitive).
    Struct(BTreeMap<String, KValue>),
}

impl KValue {
    /// Parses the importer's JSON encoding (see the module docs). Values
    /// nested deeper than [`MAX_VALUE_DEPTH`] or with more than
    /// [`MAX_VALUE_ITEMS`] items become [`KValue::None`].
    #[must_use]
    pub fn from_json(v: &serde_json::Value) -> KValue {
        Self::from_json_depth(v, 0)
    }

    fn from_json_depth(v: &serde_json::Value, depth: usize) -> KValue {
        use serde_json::Value as J;
        if depth > MAX_VALUE_DEPTH {
            return KValue::None;
        }
        match v {
            J::Null => KValue::None,
            J::Bool(b) => KValue::Bool(*b),
            J::Number(n) => {
                if let Some(i) = n.as_i64() {
                    match i32::try_from(i) {
                        Ok(i) => KValue::Int(i),
                        Err(_) => KValue::Float(i as f32),
                    }
                } else if let Some(u) = n.as_u64() {
                    KValue::Float(u as f32)
                } else {
                    KValue::Float(n.as_f64().map_or(0.0, |f| f as f32))
                }
            }
            J::String(s) => KValue::Str(s.clone()),
            J::Array(items) => {
                if items.len() > MAX_VALUE_ITEMS {
                    return KValue::None;
                }
                KValue::Array(
                    items
                        .iter()
                        .map(|i| Self::from_json_depth(i, depth + 1))
                        .collect(),
                )
            }
            J::Object(map) => {
                if let Some(o) = map.get("$obj") {
                    return KValue::Obj(o.as_str().map(str::to_owned));
                }
                if map.len() > MAX_VALUE_ITEMS {
                    return KValue::None;
                }
                KValue::Struct(
                    map.iter()
                        .filter(|(k, _)| k.as_str() != "$struct")
                        .map(|(k, v)| (k.clone(), Self::from_json_depth(v, depth + 1)))
                        .collect(),
                )
            }
        }
    }

    /// Boolean view: `Bool`, non-zero `Int`/`Float`; `false` otherwise.
    #[must_use]
    pub fn as_bool(&self) -> bool {
        match self {
            KValue::Bool(b) => *b,
            KValue::Int(i) => *i != 0,
            KValue::Float(f) => *f != 0.0,
            _ => false,
        }
    }

    /// Integer view: `Int`, `Bool` (0/1), `Float` truncated toward zero
    /// (non-finite → 0); 0 otherwise.
    #[must_use]
    pub fn as_int(&self) -> i32 {
        match self {
            KValue::Int(i) => *i,
            KValue::Bool(b) => i32::from(*b),
            KValue::Float(f) if f.is_finite() => *f as i32,
            _ => 0,
        }
    }

    /// Float view: `Float`, `Int`; 0 otherwise.
    #[must_use]
    pub fn as_float(&self) -> f32 {
        match self {
            KValue::Float(f) => *f,
            KValue::Int(i) => *i as f32,
            _ => 0.0,
        }
    }

    /// String view (`Str` only).
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            KValue::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Object path (`Obj` with a path only).
    #[must_use]
    pub fn as_obj(&self) -> Option<&str> {
        match self {
            KValue::Obj(Some(p)) => Some(p),
            _ => None,
        }
    }

    /// Array items (empty for non-arrays).
    #[must_use]
    pub fn items(&self) -> &[KValue] {
        match self {
            KValue::Array(a) => a,
            _ => &[],
        }
    }

    /// Struct field `name` (case-insensitive).
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&KValue> {
        match self {
            KValue::Struct(m) => m
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    /// A `Vector` struct (`X`, `Y`, `Z`), if all three are numbers.
    #[must_use]
    pub fn as_vec3(&self) -> Option<[f32; 3]> {
        let c = |n: &str| match self.field(n)? {
            KValue::Float(f) => Some(*f),
            KValue::Int(i) => Some(*i as f32),
            _ => None,
        };
        Some([c("X")?, c("Y")?, c("Z")?])
    }
}

impl serde::Serialize for KValue {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        match self {
            KValue::None => s.serialize_none(),
            KValue::Bool(b) => s.serialize_bool(*b),
            KValue::Int(i) => s.serialize_i32(*i),
            KValue::Float(f) => s.serialize_f32(*f),
            KValue::Str(v) => s.serialize_str(v),
            KValue::Obj(o) => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("$obj", o)?;
                m.end()
            }
            KValue::Array(items) => {
                let mut q = s.serialize_seq(Some(items.len()))?;
                for i in items {
                    q.serialize_element(i)?;
                }
                q.end()
            }
            KValue::Struct(fields) => {
                let mut m = s.serialize_map(Some(fields.len()))?;
                for (k, v) in fields {
                    m.serialize_entry(k, v)?;
                }
                m.end()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_encoding_round_trips_every_kind() {
        let v = json!({
            "a": null, "b": true, "c": 7, "d": 2.5, "e": "Name",
            "f": {"$obj": "Pkg.Thing"}, "g": {"$obj": null},
            "h": [1, 2.0], "i": {"$struct": "Vector", "X": 1.0, "Y": 2, "Z": -3.5},
            "j": 4_000_000_000_i64
        });
        let k = KValue::from_json(&v);
        assert_eq!(k.field("A"), Some(&KValue::None));
        assert!(k.field("b").unwrap().as_bool());
        assert_eq!(k.field("c").unwrap().as_int(), 7);
        assert_eq!(k.field("d").unwrap().as_float(), 2.5);
        assert_eq!(k.field("e").unwrap().as_str(), Some("Name"));
        assert_eq!(k.field("f").unwrap().as_obj(), Some("Pkg.Thing"));
        assert_eq!(k.field("g").unwrap(), &KValue::Obj(None));
        assert_eq!(
            k.field("h").unwrap().items(),
            &[KValue::Int(1), KValue::Float(2.0)]
        );
        assert_eq!(k.field("i").unwrap().as_vec3(), Some([1.0, 2.0, -3.5]));
        assert!(k.field("i").unwrap().field("$struct").is_none());
        assert_eq!(k.field("j").unwrap(), &KValue::Float(4_000_000_000.0));
        assert_eq!(KValue::Float(f32::NAN).as_int(), 0);
        assert_eq!(KValue::Float(-2.9).as_int(), -2);
    }

    #[test]
    fn hostile_nesting_is_bounded() {
        let mut v = json!(1);
        for _ in 0..(MAX_VALUE_DEPTH + 8) {
            v = json!([v]);
        }
        // Parses without panicking; the innermost levels collapse to None.
        let mut k = KValue::from_json(&v);
        let mut depth = 0;
        while let KValue::Array(mut a) = k {
            k = a.pop().unwrap_or_default();
            depth += 1;
        }
        assert!(depth <= MAX_VALUE_DEPTH + 1);
        assert_eq!(k, KValue::None);
    }
}
