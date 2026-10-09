//! Fuzz-style robustness: truncated, bit-flipped and randomly mutated synthetic
//! packages must parse to `Ok` or `Err` but never panic, hang or allocate
//! absurd amounts of memory.

#![allow(clippy::unwrap_used)] // test helpers outside #[test] fns

mod common;

use std::time::{Duration, Instant};

use asamu_ue3::{Package, ReadOptions, Summary, Ue3Error};
use common::*;

fn parse(bytes: &[u8]) -> Result<Package, Ue3Error> {
    // Single-threaded to keep the fuzz loops cheap and deterministic.
    let opts = ReadOptions {
        threads: 1,
        ..Default::default()
    };
    let r = Package::from_bytes_with(bytes.to_vec(), &opts);
    if let Ok(p) = &r {
        exercise(p);
    }
    r
}

fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    let s = Synth::sample();
    let (plain, _) = s.build();
    let (comp_small, _) = s.build_compressed(200, 64);
    let (comp_one, _) = s.build_compressed(100_000, 131_072);
    let t = Synth::with_thumbnails();
    let (thumbs_plain, _) = t.build();
    let (thumbs_comp, _) = t.build_compressed(90, 40);
    vec![
        ("uncompressed", plain),
        ("compressed-multi", comp_small),
        ("compressed-single", comp_one),
        ("uncompressed-thumbnails", thumbs_plain),
        ("compressed-thumbnails", thumbs_comp),
    ]
}

#[test]
fn every_truncation_is_handled() {
    for (name, full) in fixtures() {
        assert!(parse(&full).is_ok(), "{name}: untouched fixture must parse");
        let summary_len = Summary::parse(&full).unwrap().serialized_size;
        for cut in 0..full.len() {
            let r = parse(&full[..cut]);
            if cut < summary_len {
                assert!(r.is_err(), "{name}: truncated summary at {cut} parsed");
            }
            if !name.starts_with("uncompressed") {
                // Any truncation of a compressed package loses chunk data.
                assert!(r.is_err(), "{name}: truncated at {cut} parsed");
            }
        }
    }
}

#[test]
fn every_byte_flip_is_handled() {
    for (name, full) in fixtures() {
        let mut buf = full.clone();
        for off in 0..full.len() {
            for m in [0xFFu8, 0x80, 0x01, 0x40] {
                buf[off] = full[off] ^ m;
                let _ = parse(&buf);
            }
            for v in [0x00u8, 0xFF, 0x7F] {
                buf[off] = v;
                let _ = parse(&buf);
            }
            buf[off] = full[off];
        }
        let _ = name;
    }
}

#[test]
fn every_i32_field_extreme_is_handled() {
    // Overwrite every aligned-or-not 4-byte window with extreme values; this
    // hits every count, offset and size field in summary, chunk headers,
    // block tables and tables.
    for (_, full) in fixtures() {
        let mut buf = full.clone();
        for off in 0..full.len().saturating_sub(3) {
            for v in [i32::MAX, i32::MIN, -1, 0x7FFF_0000, 0x0001_0000] {
                buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
                let _ = parse(&buf);
            }
            buf[off..off + 4].copy_from_slice(&full[off..off + 4]);
        }
    }
}

#[test]
fn random_multi_byte_mutations_are_handled() {
    // xorshift64*; deterministic.
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    for (_, full) in fixtures() {
        for _ in 0..3000 {
            let mut buf = full.clone();
            let edits = 1 + (next() % 8) as usize;
            for _ in 0..edits {
                let off = (next() % buf.len() as u64) as usize;
                buf[off] = next() as u8;
            }
            if next() % 4 == 0 {
                let cut = (next() % buf.len() as u64) as usize;
                buf.truncate(cut);
            }
            let _ = parse(&buf);
        }
    }
}

#[test]
fn huge_counts_fail_fast_without_allocating() {
    let (plain, _) = Synth::sample().build();
    let (comp, _) = Synth::sample().build_compressed(200, 64);
    let start = Instant::now();
    let cases: Vec<(&[u8], usize)> = vec![
        (&plain, OFF_NAME_COUNT),
        (&plain, OFF_EXPORT_COUNT),
        (&plain, OFF_IMPORT_COUNT),
        (&plain, OFF_GENERATION_COUNT),
        (&comp, OFF_CHUNK_COUNT),
    ];
    for (base, off) in cases {
        for v in [i32::MAX, 0x1000_0000, 50_000_000] {
            let mut b = base.to_vec();
            put_i32(&mut b, off, v);
            let r = parse(&b);
            assert!(r.is_err(), "count {v} at {off:#x} accepted");
        }
    }
    // Chunk entry declaring a 2 GiB uncompressed size.
    let mut b = comp.clone();
    put_i32(&mut b, OFF_FIRST_CHUNK + 4, i32::MAX);
    assert!(parse(&b).is_err());
    // Chunk entry with an absurd uncompressed offset (would need a huge gap).
    let mut b = comp.clone();
    put_i32(&mut b, OFF_FIRST_CHUNK, i32::MAX - 10);
    assert!(parse(&b).is_err());
    // A limit below the real stream size is enforced.
    let opts = ReadOptions {
        max_stream_size: 64,
        threads: 1,
    };
    assert!(matches!(
        Package::from_bytes_with(comp.clone(), &opts),
        Err(Ue3Error::TooLarge { .. })
    ));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "huge-count rejection took {:?}",
        start.elapsed()
    );
}

#[test]
fn garbage_inputs_are_rejected() {
    assert!(parse(&[]).is_err());
    assert!(parse(&[0xC1]).is_err());
    assert!(parse(&[0xC1, 0x83, 0x2A, 0x9E]).is_err());
    assert!(parse(&vec![0u8; 4096]).is_err());
    assert!(parse(&vec![0xFFu8; 4096]).is_err());
    let mut tag_only = vec![0u8; 4096];
    tag_only[..4].copy_from_slice(&TAG.to_le_bytes());
    tag_only[4..6].copy_from_slice(&868u16.to_le_bytes());
    let _ = parse(&tag_only);
}
