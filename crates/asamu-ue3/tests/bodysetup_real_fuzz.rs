//! Hostile-input checks of the `RB_BodySetup` decoder on the user's own
//! installed game (read-only): every shipped body setup payload is cut short,
//! extended and corrupted in memory, with the package's real name table.
//!
//! The synthetic tests (`bodysetup_hostile.rs`) do this on one hand-written
//! payload and a 46-entry name table; here the same properties are checked on
//! all 543 shipped payloads, whose name tables hold thousands of names (so a
//! corrupted name index usually still resolves to *some* name):
//!
//! - a strict prefix of a payload is never accepted (every byte is accounted
//!   for, so a shorter payload cannot be a body setup);
//! - a payload with bytes appended is never accepted;
//! - a corrupted payload never panics, and when the decoder accepts it, the
//!   decoded fields re-encode to exactly the corrupted bytes (nothing the
//!   decoder accepts is silently normalised or dropped).
//!
//! Reads `ASAMU_ORIGINAL_DIR` (the folder containing
//! `A Story About My Uncle.app`) or the default macOS Steam location and
//! skips cleanly when the data is absent. Nothing is copied or written; only
//! counts are printed (`-- --nocapture`).

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use asamu_ue3::bodysetup::{
    NameIndex, decode_body_setup, decode_body_setup_payload, encode_body_setup, is_body_setup,
};
use asamu_ue3::flags;
use asamu_ue3::model::PackageSet;

const COOKED: &str = "A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac";

fn cooked_dir() -> Option<PathBuf> {
    let root = match std::env::var_os("ASAMU_ORIGINAL_DIR") {
        Some(r) => PathBuf::from(r),
        None => {
            let home = std::env::var_os("HOME")?;
            PathBuf::from(home)
                .join("Library/Application Support/Steam/steamapps/common/A Story About My Uncle")
        }
    };
    let dir = root.join(COOKED);
    dir.is_dir().then_some(dir)
}

fn packages(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in [dir.to_path_buf(), dir.join("Maps")] {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut v: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .map(|x| x.to_string_lossy().to_ascii_lowercase())
                        .is_some_and(|x| matches!(x.as_str(), "u" | "upk" | "asamu"))
            })
            .collect();
        v.sort();
        out.extend(v);
    }
    out
}

/// xorshift64*: a fixed, portable sequence (no dependency, same on every run).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n.max(1)).unwrap()).unwrap()
    }
}

/// Payloads up to this size are cut at every offset; larger ones at their
/// first and last `EDGE` offsets and at `SAMPLED` pseudo-random ones.
const EVERY_OFFSET_LIMIT: usize = 4096;
const EDGE: usize = 96;
const SAMPLED: usize = 160;
/// Corruptions per payload, of each kind.
const BIT_FLIPS: usize = 160;
const BYTE_WRITES: usize = 60;
const WORD_WRITES: usize = 60;

#[derive(Default)]
struct Tally {
    payloads: usize,
    truncations: usize,
    extensions: usize,
    corruptions: usize,
    accepted: usize,
    unchanged: usize,
}

#[test]
fn shipped_payloads_cut_extended_and_corrupted() {
    let Some(dir) = cooked_dir() else {
        eprintln!(
            "SKIP: original game data not found (set ASAMU_ORIGINAL_DIR to the folder \
             containing 'A Story About My Uncle.app')"
        );
        return;
    };
    let mut rng = Rng(0x0BAD_5EED_CAFE_F00D);
    let mut t = Tally::default();
    for path in packages(&dir) {
        let set = PackageSet::new(&[dir.clone(), dir.join("Maps")]);
        let lp = set.open_file(&path).unwrap();
        let pkg = &lp.package;
        let mut index = None;
        for i in 0..pkg.exports.len() {
            if !is_body_setup(pkg, i) {
                continue;
            }
            let cdo =
                pkg.export(i).unwrap().object_flags & flags::object::CLASS_DEFAULT_OBJECT != 0;
            let data = pkg.export_data(i).unwrap();
            let names = index.get_or_insert_with(|| NameIndex::new(pkg));
            // The untouched payload: accepted, and exactly re-encoded.
            let whole = decode_body_setup(pkg, i).unwrap();
            assert_eq!(
                encode_body_setup(&whole, names).as_deref(),
                Some(data),
                "{}: export {i} does not re-encode",
                lp.name
            );
            t.payloads += 1;

            // 1. Strict prefixes.
            let mut cuts: Vec<usize> = if data.len() <= EVERY_OFFSET_LIMIT {
                (0..data.len()).collect()
            } else {
                (0..EDGE)
                    .chain(data.len() - EDGE..data.len())
                    .chain((0..SAMPLED).map(|_| rng.below(data.len())))
                    .collect()
            };
            cuts.sort_unstable();
            cuts.dedup();
            for cut in cuts {
                assert!(
                    decode_body_setup_payload(&data[..cut], pkg, cdo).is_err(),
                    "{}: export {i} cut to {cut} of {} bytes was accepted",
                    lp.name,
                    data.len()
                );
                t.truncations += 1;
            }

            // 2. Appended bytes (zeros would read as an empty array count).
            let mut longer = data.to_vec();
            for extra in [0u8, 0, 0, 0, 1, 0xFF, 0, 0] {
                longer.push(extra);
                assert!(
                    decode_body_setup_payload(&longer, pkg, cdo).is_err(),
                    "{}: export {i} with {} bytes appended was accepted",
                    lp.name,
                    longer.len() - data.len()
                );
                t.extensions += 1;
            }

            // 3. Corruptions: one bit, one byte, one 32-bit word.
            let mut check = |bytes: &[u8], what: &str, at: usize| {
                t.corruptions += 1;
                if bytes == data {
                    t.unchanged += 1;
                    return;
                }
                // A panic anywhere in here fails the test.
                if let Ok(b) = decode_body_setup_payload(bytes, pkg, cdo) {
                    t.accepted += 1;
                    assert_eq!(
                        encode_body_setup(&b, names).as_deref(),
                        Some(bytes),
                        "{}: export {i}, {what} at {at}: accepted but re-encodes differently",
                        lp.name
                    );
                }
            };
            let mut work = data.to_vec();
            for _ in 0..BIT_FLIPS {
                let at = rng.below(work.len());
                let bit = 1u8 << rng.below(8);
                work[at] ^= bit;
                check(&work, "bit flip", at);
                work[at] ^= bit;
            }
            for _ in 0..BYTE_WRITES {
                let at = rng.below(work.len());
                let old = work[at];
                work[at] = u8::try_from(rng.next() & 0xFF).unwrap();
                check(&work, "byte write", at);
                work[at] = old;
            }
            if work.len() >= 4 {
                for k in 0..WORD_WRITES {
                    let at = rng.below(work.len() - 3);
                    let old = [work[at], work[at + 1], work[at + 2], work[at + 3]];
                    let v: i32 = match k % 6 {
                        0 => -1,
                        1 => i32::MAX,
                        2 => i32::MIN,
                        3 => 0,
                        4 => i32::from_le_bytes(old).wrapping_add(1),
                        _ => i32::from_le_bytes(old).wrapping_sub(1),
                    };
                    work[at..at + 4].copy_from_slice(&v.to_le_bytes());
                    check(&work, "word write", at);
                    work[at..at + 4].copy_from_slice(&old);
                }
            }
            assert_eq!(work, data);
        }
    }
    eprintln!(
        "payloads {} | prefixes rejected {} | extensions rejected {} | corruptions {} \
         (unchanged {}, accepted and exactly re-encoded {})",
        t.payloads, t.truncations, t.extensions, t.corruptions, t.unchanged, t.accepted
    );
    // The shipped game has 543 body setups (MESHES.md, "Simple collision").
    assert_eq!(t.payloads, 543);
    assert_eq!(t.extensions, 543 * 8);
    assert!(t.truncations >= 543 * 2 * EDGE);
    assert_eq!(t.corruptions, 543 * (BIT_FLIPS + BYTE_WRITES + WORD_WRITES));
    // Most corruptions land in float data and are accepted; the test would be
    // vacuous if none were.
    assert!(t.accepted > t.corruptions / 4, "{} accepted", t.accepted);
    assert!(t.accepted < t.corruptions - t.unchanged);
}
