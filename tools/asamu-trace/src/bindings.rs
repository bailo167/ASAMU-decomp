//! Key names → logical actions through the game's own key bindings.
//!
//! The recorder stores `PlayerInput.PressedKeys` (key names) and the game's
//! `Bindings` table, so the mapping follows whatever the player configured.
//! A binding's command is split at `|`; a part that is exactly another
//! binding's name is expanded (aliases such as `GBA_Jump`, up to depth 8);
//! parts starting with `OnRelease` are release actions and ignored. The
//! commands recognised (case-insensitive; names from `DefaultInput.ini` and
//! CORRELATION.md, CONFIRMED):
//!
//! | Command | Action |
//! |---|---|
//! | `Axis aBaseY Speed=s` | forward/back by the sign of `s` (default 1) |
//! | `Axis aStrafe Speed=s` | right/left by the sign of `s` |
//! | `Jump` | jump |
//! | `StartFire` | grapple (fire) |
//! | `StartSprinting` | sprint |
//! | `PowerJumpKeyDown` | power jump |
//! | `use` | use |
//!
//! A later binding of the same name replaces an earlier one (UE3 searches the
//! table from the end; TENTATIVE); modifier flags are ignored. The expansion
//! of one key visits at most [`MAX_PARTS_PER_KEY`] command parts (aliases
//! included), so a hostile or self-referential table with a wide fan-out
//! cannot make the conversion run for ever; real tables use a handful. The
//! Python recorder (`tools/trace-recorder/asamu_recorder_core.py`,
//! `key_actions`) implements the same rules, budget included; keep them
//! identical.

use std::collections::BTreeMap;

use crate::raw::RawBinding;

/// Maximum alias depth.
const MAX_DEPTH: u32 = 8;
/// Most command parts (split at `|`, aliases expanded) visited for one key.
/// Parts beyond the budget are ignored. Mirrored by the Python recorder.
pub const MAX_PARTS_PER_KEY: u32 = 256;

/// One logical action a key triggers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Forward (+1) or back (−1).
    Forward(i32),
    /// Right (+1) or left (−1).
    Right(i32),
    /// Jump.
    Jump,
    /// Grapple (fire).
    Grapple,
    /// Sprint.
    Sprint,
    /// Power jump.
    PowerJump,
    /// Use.
    Use,
}

/// The actions of a set of held keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Actions {
    /// Forward axis, −1, 0 or 1.
    pub forward: i32,
    /// Strafe axis, −1, 0 or 1.
    pub right: i32,
    /// Jump held.
    pub jump: bool,
    /// Grapple held.
    pub grapple: bool,
    /// Sprint held.
    pub sprint: bool,
    /// Power jump held.
    pub power_jump: bool,
    /// Use held.
    pub use_: bool,
}

/// Key name (ASCII lower case) → actions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyMap {
    map: BTreeMap<String, Vec<Action>>,
}

fn is_number(s: &str) -> bool {
    // [+-]? ( digits ( . digits* )? | . digits ) ( [eE] [+-]? digits )?
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let int_digits = i - int_start;
    let mut frac_digits = 0;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let f = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        frac_digits = i - f;
    }
    if int_digits == 0 && frac_digits == 0 {
        return false;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let e = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == e {
            return false;
        }
    }
    i == b.len()
}

/// The `Speed=` value of an `Axis` command (default 1; malformed → 0).
fn parse_speed(tokens: &[&str]) -> f64 {
    for t in tokens.iter().skip(2) {
        if t.to_ascii_lowercase().starts_with("speed=") {
            let v = &t[6..];
            return if is_number(v) {
                v.parse::<f64>().unwrap_or(0.0)
            } else {
                0.0
            };
        }
    }
    1.0
}

fn sign(v: f64) -> i32 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

impl KeyMap {
    /// Builds the map from the game's bindings.
    #[must_use]
    pub fn from_bindings(bindings: &[RawBinding]) -> Self {
        let mut table: BTreeMap<String, &str> = BTreeMap::new();
        for b in bindings {
            table.insert(b.name.to_ascii_lowercase(), b.command.as_str());
        }
        let mut map = BTreeMap::new();
        for (name, command) in &table {
            let mut acts = Vec::new();
            let mut budget = MAX_PARTS_PER_KEY;
            expand(&table, command, 0, &mut budget, &mut acts);
            map.insert(name.clone(), acts);
        }
        Self { map }
    }

    /// Actions bound to one key.
    #[must_use]
    pub fn key(&self, key: &str) -> &[Action] {
        self.map
            .get(&key.to_ascii_lowercase())
            .map_or(&[], Vec::as_slice)
    }

    /// Combined actions of the held `keys` (axes summed and clamped).
    #[must_use]
    pub fn actions(&self, keys: &[String]) -> Actions {
        let mut a = Actions::default();
        // Summed in i64: the key list comes from the recording (any length),
        // so the exact sum must not overflow before the clamp.
        let (mut forward, mut right) = (0_i64, 0_i64);
        for k in keys {
            for act in self.key(k) {
                match *act {
                    Action::Forward(s) => forward = forward.saturating_add(i64::from(s)),
                    Action::Right(s) => right = right.saturating_add(i64::from(s)),
                    Action::Jump => a.jump = true,
                    Action::Grapple => a.grapple = true,
                    Action::Sprint => a.sprint = true,
                    Action::PowerJump => a.power_jump = true,
                    Action::Use => a.use_ = true,
                }
            }
        }
        a.forward = sign_i64(forward);
        a.right = sign_i64(right);
        a
    }
}

fn sign_i64(v: i64) -> i32 {
    match v.cmp(&0) {
        std::cmp::Ordering::Greater => 1,
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
    }
}

fn expand(
    table: &BTreeMap<String, &str>,
    command: &str,
    depth: u32,
    budget: &mut u32,
    out: &mut Vec<Action>,
) {
    for part in command.split('|') {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let tokens: Vec<&str> = part.split_ascii_whitespace().collect();
        let Some(first) = tokens.first() else {
            continue;
        };
        let head = first.to_ascii_lowercase();
        if head == "onrelease" {
            continue;
        }
        if tokens.len() == 1
            && let Some(alias) = table.get(&head)
        {
            if depth < MAX_DEPTH {
                expand(table, alias, depth + 1, budget, out);
            }
            continue;
        }
        match head.as_str() {
            "axis" if tokens.len() >= 2 => {
                let axis = tokens[1].to_ascii_lowercase();
                let s = sign(parse_speed(&tokens));
                if s != 0 {
                    match axis.as_str() {
                        "abasey" => out.push(Action::Forward(s)),
                        "astrafe" => out.push(Action::Right(s)),
                        _ => {}
                    }
                }
            }
            "jump" => out.push(Action::Jump),
            "startfire" => out.push(Action::Grapple),
            "startsprinting" => out.push(Action::Sprint),
            "powerjumpkeydown" => out.push(Action::PowerJump),
            "use" => out.push(Action::Use),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::tests::header;

    fn keys(k: &[&str]) -> Vec<String> {
        k.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn default_bindings() {
        let m = KeyMap::from_bindings(&header().bindings);
        assert_eq!(m.key("w"), &[Action::Forward(1)]);
        assert_eq!(m.key("S"), &[Action::Forward(-1)]);
        assert_eq!(m.key("D"), &[Action::Right(1)]);
        assert_eq!(m.key("SpaceBar"), &[Action::Jump]);
        assert_eq!(m.key("LeftMouseButton"), &[Action::Grapple]);
        assert_eq!(m.key("RightMouseButton"), &[Action::PowerJump]);
        assert_eq!(m.key("E"), &[Action::Use]);
        assert!(m.key("F7").is_empty());
        let a = m.actions(&keys(&["W", "S", "D", "SpaceBar", "LeftShift"]));
        assert_eq!((a.forward, a.right), (0, 1));
        assert!(a.jump && a.sprint && !a.grapple && !a.use_ && !a.power_jump);
        let a = m.actions(&keys(&["W", "Up"]));
        assert_eq!(a.forward, 1);
    }

    #[test]
    fn aliases_overrides_and_odd_commands() {
        let b = |n: &str, c: &str| RawBinding {
            name: n.into(),
            command: c.into(),
        };
        let m = KeyMap::from_bindings(&[
            b("Loop", "Loop"),
            b("Q", "Axis aStrafe Speed=-.5|  axis   ABASEY   speed=+2e0 "),
            b("Z", "Axis aBaseY Speed=1_0"),
            b("X", "Axis aBaseY"),
            b("C", "Axis aBaseY Speed=0"),
            b("V", "onrelease Jump"),
            b("K", "GBA_Fire"),
            b("K", "Jump"),
        ]);
        assert!(
            m.key("Loop").is_empty(),
            "a self alias ends at the depth limit"
        );
        assert_eq!(m.key("Q"), &[Action::Right(-1), Action::Forward(1)]);
        assert!(
            m.key("Z").is_empty(),
            "Python-only number syntax is rejected"
        );
        assert_eq!(m.key("X"), &[Action::Forward(1)]);
        assert!(m.key("C").is_empty());
        assert!(m.key("V").is_empty());
        assert_eq!(m.key("K"), &[Action::Jump], "the later binding wins");
    }

    #[test]
    fn alias_fan_out_is_bounded() {
        let b = |n: &str, c: &str| RawBinding {
            name: n.into(),
            command: c.into(),
        };
        // Six aliases with a fan-out of 16 (within the depth limit): 16^6
        // expansions without a budget.
        let wide = |next: &str| vec![next; 16].join("|");
        let mut table: Vec<RawBinding> = (0..6)
            .map(|i| b(&format!("L{i}"), &wide(&format!("L{}", i + 1))))
            .collect();
        table.push(b("L6", "Jump | Axis aBaseY Speed=1"));
        table.push(b("K", "L0"));
        let t0 = std::time::Instant::now();
        let m = KeyMap::from_bindings(&table);
        assert!(t0.elapsed().as_secs() < 5, "expansion is bounded");
        let k = m.key("K");
        assert!(!k.is_empty() && k.len() <= MAX_PARTS_PER_KEY as usize);
        // Within the budget every part counts once, aliases included: "A" is
        // 3 alias parts plus 3 x 2 parts of "B" = 9 parts, 6 actions.
        let m = KeyMap::from_bindings(&[
            b("B", "Jump|use"),
            b("A", "B|B|B"),
            b("C", "Axis aBaseY Speed=1"),
        ]);
        assert_eq!(m.key("A").len(), 6);
        // Many keys with the same axis: summed exactly, then clamped.
        let held: Vec<String> = std::iter::repeat_n("C".to_owned(), 100_000).collect();
        assert_eq!(m.actions(&held).forward, 1);
    }

    #[test]
    fn number_syntax() {
        for ok in ["1", "+1.0", "-.1", "1.", "2e3", "2E-3", ".5e+1"] {
            assert!(is_number(ok), "{ok}");
        }
        for bad in ["", "+", ".", "1e", "1_0", "inf", "nan", "0x1", "1.0f", " 1"] {
            assert!(!is_number(bad), "{bad}");
        }
    }
}
