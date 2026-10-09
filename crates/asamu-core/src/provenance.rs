//! Provenance for gameplay numbers.
//!
//! The project rule is: **no gameplay constant without a source.** Every
//! numeric (or behavioural) gameplay parameter in the runtime is wrapped in a
//! [`Param`], which carries the value together with a [`Provenance`] saying
//! where it came from. Until values are recovered from the original game, all
//! defaults are [`Provenance::Placeholder`] and must never be presented as
//! matching the original.

use serde::{Deserialize, Serialize};

/// Where a gameplay value came from.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Provenance {
    /// Not recovered from the original. Chosen by us (e.g. for graybox
    /// playability). `note` must say what should replace it.
    Placeholder {
        /// Why this value exists and what should replace it.
        note: String,
    },
    /// An UnrealScript class default property read from the original packages.
    ScriptDefault {
        /// Script class, e.g. `ASAMU.SomePawnClass` (fully qualified when known).
        class: String,
        /// Default property name.
        property: String,
    },
    /// A literal constant or rule of the original's compiled UnrealScript
    /// (bytecode, or the shipped script read locally — never reproduced in
    /// this repository), as opposed to a class default property.
    ScriptCode {
        /// Script class, e.g. `asamu.ASAMUPawn`.
        class: String,
        /// Function, event or state (code label) holding the constant.
        function: String,
    },
    /// A constant or behaviour found in native code of the original executable.
    NativeCode {
        /// Symbol (function or data) in which it was found.
        symbol: String,
    },
    /// A value from an original `.ini` configuration file.
    Config {
        /// Config file name, e.g. `ASAMUGame.ini` (no user paths).
        file: String,
        /// `[Section] Key` or equivalent.
        key: String,
    },
    /// A value measured from a recorded trace of the original game.
    MeasuredTrace {
        /// Identifier of the trace (file name / hash / journal reference).
        trace_id: String,
    },
}

impl Provenance {
    /// Convenience constructor for [`Provenance::Placeholder`].
    #[must_use]
    pub fn placeholder(note: impl Into<String>) -> Self {
        Self::Placeholder { note: note.into() }
    }

    /// `true` for [`Provenance::Placeholder`].
    #[must_use]
    pub fn is_placeholder(&self) -> bool {
        matches!(self, Self::Placeholder { .. })
    }

    /// Short machine-friendly label of the variant.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Placeholder { .. } => "placeholder",
            Self::ScriptDefault { .. } => "script_default",
            Self::ScriptCode { .. } => "script_code",
            Self::NativeCode { .. } => "native_code",
            Self::Config { .. } => "config",
            Self::MeasuredTrace { .. } => "measured_trace",
        }
    }
}

impl core::fmt::Display for Provenance {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Placeholder { note } => write!(f, "PLACEHOLDER: {note}"),
            Self::ScriptDefault { class, property } => {
                write!(f, "script default {class}.{property}")
            }
            Self::ScriptCode { class, function } => write!(f, "script code {class}.{function}"),
            Self::NativeCode { symbol } => write!(f, "native code {symbol}"),
            Self::Config { file, key } => write!(f, "config {file} {key}"),
            Self::MeasuredTrace { trace_id } => write!(f, "measured trace {trace_id}"),
        }
    }
}

/// A gameplay value together with its [`Provenance`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Param<T> {
    /// The value used by the simulation.
    pub value: T,
    /// Where the value came from.
    pub provenance: Provenance,
}

impl<T> Param<T> {
    /// A value with explicit provenance.
    #[must_use]
    pub fn new(value: T, provenance: Provenance) -> Self {
        Self { value, provenance }
    }

    /// A placeholder value (not recovered from the original game).
    #[must_use]
    pub fn placeholder(value: T, note: impl Into<String>) -> Self {
        Self {
            value,
            provenance: Provenance::placeholder(note),
        }
    }

    /// `true` if the value is a placeholder.
    #[must_use]
    pub fn is_placeholder(&self) -> bool {
        self.provenance.is_placeholder()
    }

    /// Replaces the value and provenance together (they must never be changed
    /// independently).
    pub fn set(&mut self, value: T, provenance: Provenance) {
        self.value = value;
        self.provenance = provenance;
    }
}

impl<T: Copy> Param<T> {
    /// The value (for `Copy` types).
    #[must_use]
    pub fn get(&self) -> T {
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_helpers() {
        let p = Param::placeholder(1.5_f32, "tune me");
        assert!(p.is_placeholder());
        assert_eq!(p.get(), 1.5);
        assert_eq!(p.provenance.kind(), "placeholder");
        assert_eq!(p.provenance.to_string(), "PLACEHOLDER: tune me");
    }

    #[test]
    fn set_replaces_value_and_provenance() {
        let mut p = Param::placeholder(1.0_f32, "x");
        p.set(
            2.0,
            Provenance::ScriptDefault {
                class: "ASAMU.Example".into(),
                property: "Speed".into(),
            },
        );
        assert!(!p.is_placeholder());
        assert_eq!(p.get(), 2.0);
        assert_eq!(
            p.provenance.to_string(),
            "script default ASAMU.Example.Speed"
        );
    }

    #[test]
    fn serde_shape_is_tagged() {
        let all = vec![
            Provenance::placeholder("n"),
            Provenance::ScriptDefault {
                class: "C".into(),
                property: "P".into(),
            },
            Provenance::ScriptCode {
                class: "C".into(),
                function: "F".into(),
            },
            Provenance::NativeCode { symbol: "S".into() },
            Provenance::Config {
                file: "F.ini".into(),
                key: "[S] K".into(),
            },
            Provenance::MeasuredTrace {
                trace_id: "t1".into(),
            },
        ];
        let json = serde_json::to_string(&all).unwrap();
        assert!(
            json.contains(r#"{"kind":"placeholder","note":"n"}"#),
            "{json}"
        );
        assert!(json.contains(r#""kind":"script_default""#));
        assert!(json.contains(r#""kind":"script_code","class":"C","function":"F""#));
        assert!(json.contains(r#""kind":"native_code""#));
        assert!(json.contains(r#""kind":"config""#));
        assert!(json.contains(r#""kind":"measured_trace""#));
        let back: Vec<Provenance> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, all);
        let kinds: Vec<_> = back.iter().map(Provenance::kind).collect();
        assert_eq!(
            kinds,
            [
                "placeholder",
                "script_default",
                "script_code",
                "native_code",
                "config",
                "measured_trace"
            ]
        );
        for p in &back {
            assert!(!p.to_string().is_empty());
        }

        let param = Param::new(3_u32, Provenance::NativeCode { symbol: "S".into() });
        let json = serde_json::to_string(&param).unwrap();
        assert_eq!(
            json,
            r#"{"value":3,"provenance":{"kind":"native_code","symbol":"S"}}"#
        );
        let back: Param<u32> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, param);
    }
}
