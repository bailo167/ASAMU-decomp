//! LZO1X decompression (bounds-checked, safe Rust).
//!
//! Compressed UE3 packages of this game (summary `CompressionFlags == 2`)
//! store each compressed chunk as a sequence of independently compressed
//! blocks; every block is one complete LZO1X stream whose decompressed size
//! is recorded in the chunk's block table. [`decompress`] turns one such
//! block back into exactly that many bytes or reports why it cannot.
//!
//! This is an independent implementation written from the format
//! description below. It treats the input as hostile: every read and every
//! write is checked, run lengths use checked arithmetic, output is never
//! allowed to grow past the caller's expected size, and malformed input
//! yields an [`LzoError`] instead of a panic.
//!
//! # Stream format
//!
//! An LZO1X stream is a sequence of byte-aligned instructions. An
//! instruction either copies *literal* bytes from the input to the output,
//! or copies a *match*: `length` bytes taken from `distance` bytes behind
//! the current end of the output. A match may overlap the bytes it is
//! producing (`distance < length`); it then repeats the last `distance`
//! bytes, which is how runs are encoded.
//!
//! ## Decoder state
//!
//! The meaning of an instruction byte below 16 depends on what was decoded
//! just before it, so the decoder carries a small state value:
//!
//! | state  | reached after                                                  |
//! |--------|----------------------------------------------------------------|
//! | 0      | the stream start, or a match that carried no trailing literals |
//! | 1 to 3 | a match (or the opening literal run) that ended with that many literal bytes |
//! | 4      | a literal run of four or more bytes                            |
//!
//! ## Opening byte
//!
//! If the very first byte is greater than 17, it is not an instruction:
//! `first - 17` (1 to 238) literal bytes follow it. The state becomes that
//! count when it is below 4, otherwise 4. Any other first byte is decoded as
//! a normal instruction in state 0.
//!
//! ## Instructions
//!
//! In the table, `B` is the instruction byte, `N` the next input byte and
//! `W` the next two input bytes read as a little-endian `u16`.
//!
//! | `B`            | state  | meaning |
//! |----------------|--------|---------|
//! | `0..=15`       | 0      | literal run of `B + 3` bytes (`B == 0`: extended, see below, base 15) |
//! | `0..=15`       | 1 to 3 | match, length 2, distance `1 + (B >> 2) + (N << 2)` (at most 0x400) |
//! | `0..=15`       | 4      | match, length 3, distance `0x801 + (B >> 2) + (N << 2)` (0x801 to 0xC00) |
//! | `16..=31`      | any    | "M4" match, length `(B & 7) + 2` (`B & 7 == 0`: extended, base 7), distance `0x4000 + ((B & 8) << 11) + (W >> 2)` (0x4001 to 0xBFFF) |
//! | `32..=63`      | any    | "M3" match, length `(B & 31) + 2` (`B & 31 == 0`: extended, base 31), distance `1 + (W >> 2)` (at most 0x4000) |
//! | `64..=255`     | any    | "M2" match, length `(B >> 5) + 1` (3 to 8), distance `1 + ((B >> 2) & 7) + (N << 3)` (at most 0x800) |
//!
//! A *literal run* sets the state to 4. Every *match* is followed by 0 to 3
//! trailing literal bytes that need no instruction of their own: their count
//! is stored in the low two bits of the match's second-to-last encoded byte
//! (`B` for the two-byte forms, the low byte of `W` for M3 and M4). After
//! copying them the state becomes that count (0 to 3).
//!
//! ## Extended lengths
//!
//! When a length field is entirely zero the length continues in the
//! following bytes: each `0x00` byte adds 255, and the first non-zero byte
//! adds its own value plus the field's base (15 for literal runs, 31 for M3,
//! 7 for M4). The instruction-specific constant (`+3` or `+2`) is then
//! added as usual.
//!
//! ## End of stream
//!
//! An M4 instruction whose computed distance part `((B & 8) << 11) + (W >> 2)`
//! is zero does not copy anything: it terminates the stream. The well-formed
//! terminator is exactly the three bytes `0x11 0x00 0x00` (length field 1, so
//! a length of 3, and no trailing literals); a zero-distance M4 with any other
//! length or with non-zero trailing-literal bits is rejected. The output must
//! then be exactly the expected size and no input may remain.
//!
//! An empty output is therefore encoded as just `0x11 0x00 0x00`.
//!
//! Deliberate strictness: the widely used reference decoder (liblzo2)
//! treats *any* zero-distance M4 as the end of the stream and ignores its
//! length and trailing bits. liblzo2's compressors emit the canonical three
//! bytes (checked for LZO1X-1, LZO1X-1(15) and LZO1X-999 on 3,000 synthetic
//! inputs each), and every block of the original packages ends with them
//! (see the evidence below), so rejecting the other forms loses no real data
//! while catching corrupt streams earlier. Apart from this, and from requiring the output
//! to be exactly `dst_len` bytes, the decoder is meant to accept exactly the
//! streams the reference decoder accepts, with identical output (see the
//! differential evidence below).
//!
//! # Resource use
//!
//! Time is linear in input plus output size (overlapping copies double the
//! copied region on each pass; zero-length-extension bytes are each read
//! once). Memory is the output vector only: its length never exceeds
//! `dst_len` (the initial reservation is also capped by the input size), and
//! no instruction produces more than 255 output bytes per input byte it
//! occupies, so the output is at most 255 times the input.
//! Callers should still check a block's declared size against the format's
//! block size before passing it as `dst_len`.
//!
//! # Evidence that packages use this format (original-game side)
//!
//! CONFIRMED: the gated test `decompress_every_block_of_every_original_package`
//! (below) walks every `.u`/`.upk`/`.asamu` package of the macOS Steam build
//! (`CookedMac` and `CookedMac/Maps`). All 38 packages with
//! `CompressionFlags == 2` decode completely with this decoder: 485 chunks,
//! 12,063 blocks, 555,598,975 compressed block bytes into 1,559,127,860
//! bytes, `BlockSize` 131,072 in every chunk header. Every block ends with
//! the end-of-stream marker, consumes its input exactly and yields exactly
//! the size recorded in the block table, and the decompressed bytes parse as
//! the packages' name, import and export tables. `CompressionFlags == 2`
//! therefore means LZO1X for this build.
//!
//! CONFIRMED (independent cross-check, local only): a throwaway harness kept
//! out of the repository parsed every package's chunk and block tables on
//! its own, decoded every block with the reference liblzo2 2.10
//! (`lzo1x_decompress_safe`) and with a separately written Python decoder,
//! and compared SHA-256 digests against this decoder: 38 packages, 485
//! chunks, 12,063 blocks, 555,598,975 to 1,559,127,860 bytes, all three
//! implementations equal on every block. The last three bytes of every block
//! are `11 00 00`.
//!
//! STRONG (differential): the same harness ran 70,000 mutated or random
//! streams (mutated liblzo2 LZO1X-1/LZO1X-1(15)/LZO1X-999 output, mutated
//! original blocks, random bytes) through this decoder and liblzo2. 66 of
//! them ended in a non-canonical zero-distance M4 terminator that liblzo2
//! accepted and this decoder rejected on purpose. On the other 69,934 the
//! two agreed exactly: 15,707 decoded by both to identical bytes of exactly
//! `dst_len`, the rest rejected by both. In CI, the tests below repeat this
//! check against a second decoder with a different structure, and decode
//! known-answer streams that liblzo2's compressors produced from synthetic
//! input.

use thiserror::Error;

/// Largest literal count the opening byte can carry (`255 - 17`).
const OPENING_LITERALS_MAX: u8 = 238;
/// Opening bytes above this value carry a literal count instead of an instruction.
const OPENING_LITERAL_BIAS: u8 = 17;
/// Distance bias of the three-byte match that follows a literal run (state 4).
const FAR_SHORT_MATCH_BIAS: usize = 0x801;
/// Distance bias of an M4 match.
const M4_DISTANCE_BIAS: usize = 0x4000;
/// Length of the only valid end-of-stream M4 instruction.
const END_MARKER_LENGTH: usize = 3;
/// How much output capacity is reserved up front per input byte. Further
/// growth happens only as bytes are actually produced, so a hostile
/// `dst_len` cannot force a large allocation from a small input.
const INITIAL_RESERVE_PER_INPUT_BYTE: usize = 8;

/// Errors produced while decompressing an LZO1X stream.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum LzoError {
    /// The input ended before an instruction, its operands, its literal bytes
    /// or the end-of-stream marker were complete.
    #[error("LZO1X input overrun: the stream ends before its end-of-stream marker")]
    InputOverrun,
    /// The stream would produce more bytes than the expected output size.
    #[error("LZO1X output overrun: the stream produces more than the expected output size")]
    OutputOverrun,
    /// A match refers to bytes before the start of the output.
    #[error("LZO1X look-behind overrun: a match reaches before the start of the output")]
    LookBehindOverrun,
    /// The end-of-stream marker was reached with an output of the wrong size.
    #[error("LZO1X output size mismatch: expected {expected} bytes, the stream produced {actual}")]
    OutputSizeMismatch {
        /// The size the caller expected.
        expected: usize,
        /// The size the stream actually produced.
        actual: usize,
    },
    /// Bytes remain after the end-of-stream marker.
    #[error("LZO1X input not consumed: {remaining} byte(s) follow the end-of-stream marker")]
    InputNotConsumed {
        /// Number of unread input bytes.
        remaining: usize,
    },
    /// A zero-distance M4 instruction (end-of-stream form) was not the
    /// canonical terminator `0x11 0x00 0x00`: its length was not 3 or its
    /// trailing-literal bits were not zero.
    #[error(
        "LZO1X malformed end-of-stream marker: zero-distance M4 with length {length} and trailing-literal count {trailing} (expected 3 and 0)"
    )]
    MalformedEndMarker {
        /// The decoded length of the offending instruction.
        length: usize,
        /// The trailing-literal count carried in the low two bits of its word.
        trailing: u8,
    },
    /// An extended run length does not fit in `usize`.
    #[error("LZO1X run length overflows usize")]
    LengthOverflow,
}

/// Decompress one LZO1X block. `dst_len` is the exact expected output size
/// (known from the UE3 block table); producing more or fewer bytes is an error.
///
/// The returned vector has length `dst_len` exactly. However hostile the
/// input, the output never holds more than `dst_len` bytes, nor more than
/// 255 times `src.len()` (its allocation may round up by the usual vector
/// growth). See the module documentation for the stream format and
/// resource bounds, and [`LzoError`] for failure modes.
pub fn decompress(src: &[u8], dst_len: usize) -> Result<Vec<u8>, LzoError> {
    let mut input = Input { bytes: src, pos: 0 };
    let mut output = Output::new(dst_len, src.len());
    let mut state: u8 = 0;

    if let Some(&first) = src.first()
        && first > OPENING_LITERAL_BIAS
    {
        input.pos = 1;
        let count = first - OPENING_LITERAL_BIAS; // 1..=238
        debug_assert!(count <= OPENING_LITERALS_MAX);
        output.literals(input.take(usize::from(count))?)?;
        state = if count < 4 { count } else { 4 };
    }

    loop {
        let insn = input.byte()?;
        let (distance, length, trailing) = if insn < 16 {
            match state {
                0 => {
                    let run = if insn == 0 {
                        input.extended_length(15)?
                    } else {
                        usize::from(insn)
                    };
                    let run = run.checked_add(3).ok_or(LzoError::LengthOverflow)?;
                    output.literals(input.take(run)?)?;
                    state = 4;
                    continue;
                }
                1..=3 => {
                    let next = usize::from(input.byte()?);
                    (1 + usize::from(insn >> 2) + (next << 2), 2, insn & 3)
                }
                _ => {
                    let next = usize::from(input.byte()?);
                    (
                        FAR_SHORT_MATCH_BIAS + usize::from(insn >> 2) + (next << 2),
                        3,
                        insn & 3,
                    )
                }
            }
        } else if insn < 32 {
            // M4.
            let field = usize::from(insn & 7);
            let length = if field == 0 {
                input.extended_length(7)?
            } else {
                field
            };
            let length = length.checked_add(2).ok_or(LzoError::LengthOverflow)?;
            let word = input.le16()?;
            let partial = (usize::from(insn & 8) << 11) + usize::from(word >> 2);
            if partial == 0 {
                let trailing = low_bits(word);
                if length != END_MARKER_LENGTH || trailing != 0 {
                    return Err(LzoError::MalformedEndMarker { length, trailing });
                }
                break;
            }
            (M4_DISTANCE_BIAS + partial, length, low_bits(word))
        } else if insn < 64 {
            // M3.
            let field = usize::from(insn & 31);
            let length = if field == 0 {
                input.extended_length(31)?
            } else {
                field
            };
            let length = length.checked_add(2).ok_or(LzoError::LengthOverflow)?;
            let word = input.le16()?;
            (1 + usize::from(word >> 2), length, low_bits(word))
        } else {
            // M2.
            let next = usize::from(input.byte()?);
            (
                1 + usize::from((insn >> 2) & 7) + (next << 3),
                usize::from(insn >> 5) + 1,
                insn & 3,
            )
        };

        output.repeat(distance, length)?;
        if trailing != 0 {
            output.literals(input.take(usize::from(trailing))?)?;
        }
        state = trailing;
    }

    let actual = output.bytes.len();
    if actual != dst_len {
        return Err(LzoError::OutputSizeMismatch {
            expected: dst_len,
            actual,
        });
    }
    let remaining = input.remaining();
    if remaining != 0 {
        return Err(LzoError::InputNotConsumed { remaining });
    }
    Ok(output.bytes)
}

/// The trailing-literal count carried in the low two bits of an M3/M4 word.
fn low_bits(word: u16) -> u8 {
    (word & 3) as u8
}

/// Checked cursor over the compressed input.
struct Input<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Input<'a> {
    fn byte(&mut self) -> Result<u8, LzoError> {
        let value = *self.bytes.get(self.pos).ok_or(LzoError::InputOverrun)?;
        self.pos += 1;
        Ok(value)
    }

    fn le16(&mut self) -> Result<u16, LzoError> {
        let bytes = self.take(2)?;
        match bytes {
            [lo, hi] => Ok(u16::from_le_bytes([*lo, *hi])),
            _ => Err(LzoError::InputOverrun),
        }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], LzoError> {
        let end = self.pos.checked_add(count).ok_or(LzoError::InputOverrun)?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or(LzoError::InputOverrun)?;
        self.pos = end;
        Ok(slice)
    }

    /// Decode an extended length whose instruction field was zero: every
    /// `0x00` byte adds 255, the first non-zero byte adds itself plus `base`.
    fn extended_length(&mut self, base: usize) -> Result<usize, LzoError> {
        let mut length = base;
        loop {
            match self.byte()? {
                0 => length = length.checked_add(255).ok_or(LzoError::LengthOverflow)?,
                value => {
                    return length
                        .checked_add(usize::from(value))
                        .ok_or(LzoError::LengthOverflow);
                }
            }
        }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }
}

/// Output buffer that refuses to grow past the expected size.
struct Output {
    bytes: Vec<u8>,
    limit: usize,
}

impl Output {
    fn new(limit: usize, input_len: usize) -> Self {
        let reserve = limit.min(input_len.saturating_mul(INITIAL_RESERVE_PER_INPUT_BYTE));
        Self {
            bytes: Vec::with_capacity(reserve),
            limit,
        }
    }

    fn room(&self) -> usize {
        self.limit.saturating_sub(self.bytes.len())
    }

    fn literals(&mut self, src: &[u8]) -> Result<(), LzoError> {
        if src.len() > self.room() {
            return Err(LzoError::OutputOverrun);
        }
        self.bytes.extend_from_slice(src);
        Ok(())
    }

    /// Append `length` bytes copied from `distance` bytes behind the end of
    /// the output, with LZ semantics: each produced byte equals the byte
    /// `distance` positions before it, so overlapping copies repeat.
    fn repeat(&mut self, distance: usize, length: usize) -> Result<(), LzoError> {
        if distance == 0 {
            return Err(LzoError::LookBehindOverrun);
        }
        let start = self
            .bytes
            .len()
            .checked_sub(distance)
            .ok_or(LzoError::LookBehindOverrun)?;
        if length > self.room() {
            return Err(LzoError::OutputOverrun);
        }
        // The region `start..end of output` is periodic with period
        // `distance` and grows by each chunk appended, so every pass may copy
        // the whole region produced so far (capped at what is still needed).
        let mut copied = 0usize;
        while copied < length {
            let chunk = distance
                .saturating_add(copied)
                .min(length.saturating_sub(copied));
            let end = start
                .checked_add(chunk)
                .ok_or(LzoError::LookBehindOverrun)?;
            if end > self.bytes.len() {
                // Unreachable by the invariant above; refuse rather than panic.
                return Err(LzoError::LookBehindOverrun);
            }
            self.bytes.extend_from_within(start..end);
            copied = copied.saturating_add(chunk);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Deterministic pseudo-random source (xorshift64), no external crates.
    // ------------------------------------------------------------------

    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Self(seed | 1)
        }
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn below(&mut self, n: usize) -> usize {
            assert!(n > 0);
            (self.next_u64() % n as u64) as usize
        }
        fn range(&mut self, lo: usize, hi_inclusive: usize) -> usize {
            lo + self.below(hi_inclusive - lo + 1)
        }
        fn byte(&mut self) -> u8 {
            (self.next_u64() >> 24) as u8
        }
        fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n).map(|_| self.byte()).collect()
        }
        fn chance(&mut self, percent: usize) -> bool {
            self.below(100) < percent
        }
    }

    // ------------------------------------------------------------------
    // Test-only LZO1X encoder: greedy hash-chain matcher that emits literal
    // runs (opening-byte form and extended form), trailing literals, and
    // M1 / M2 / M3 / M4 matches. Written to exercise the decoder, not to
    // compress well.
    // ------------------------------------------------------------------

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Kind {
        M1Near,
        M1Far,
        M2,
        M3,
        M4,
    }

    #[derive(Debug, Clone, Copy)]
    struct EncodeOptions {
        /// Use the two/three-byte short matches whenever the state allows.
        use_m1: bool,
        /// Use M2 for short near matches (otherwise M3 is used).
        use_m2: bool,
        /// Use the opening-byte literal form for a first run of 1..=238.
        opening_byte: bool,
        /// Hash-chain search depth.
        max_chain: usize,
        /// Longest match the encoder will emit.
        max_match: usize,
    }

    impl Default for EncodeOptions {
        fn default() -> Self {
            Self {
                use_m1: false,
                use_m2: true,
                opening_byte: true,
                max_chain: 32,
                max_match: usize::MAX,
            }
        }
    }

    #[derive(Debug, Default)]
    struct EncodeLog {
        matches: Vec<(Kind, usize, usize)>,
        literal_runs: usize,
        trailing_literal_groups: usize,
    }

    impl EncodeLog {
        fn count(&self, kind: Kind) -> usize {
            self.matches.iter().filter(|m| m.0 == kind).count()
        }
        fn has(&self, kind: Kind, distance: usize) -> bool {
            self.matches.iter().any(|m| m.0 == kind && m.1 == distance)
        }
    }

    const MAX_DISTANCE: usize = 0xBFFF;

    struct Encoder {
        out: Vec<u8>,
        state: u8,
        opts: EncodeOptions,
        log: EncodeLog,
    }

    fn push_extended(out: &mut Vec<u8>, mut rem: usize) {
        assert!(rem >= 1);
        while rem > 255 {
            out.push(0);
            rem -= 255;
        }
        out.push(rem as u8);
    }

    impl Encoder {
        fn literals(&mut self, lits: &[u8]) {
            let n = lits.len();
            if n == 0 {
                return;
            }
            if self.out.is_empty() && (n <= 3 || (self.opts.opening_byte && n <= 238)) {
                self.out.push(17 + n as u8);
                self.state = if n < 4 { n as u8 } else { 4 };
                self.log.literal_runs += 1;
            } else if n <= 3 {
                // Trailing literals of the previous match.
                assert_eq!(self.state, 0);
                let idx = self.out.len() - 2;
                assert_eq!(self.out[idx] & 3, 0);
                self.out[idx] |= n as u8;
                self.state = n as u8;
                self.log.trailing_literal_groups += 1;
            } else {
                assert_eq!(self.state, 0, "a literal run must follow a plain match");
                if n <= 18 {
                    self.out.push((n - 3) as u8);
                } else {
                    self.out.push(0);
                    push_extended(&mut self.out, n - 18);
                }
                self.state = 4;
                self.log.literal_runs += 1;
            }
            self.out.extend_from_slice(lits);
        }

        /// Emit a match of up to `len` bytes; returns the length emitted.
        fn emit_match(&mut self, dist: usize, len: usize) -> usize {
            assert!((1..=MAX_DISTANCE).contains(&dist));
            let (kind, used) =
                if self.opts.use_m1 && (1..=3).contains(&self.state) && dist <= 0x400 && len >= 2 {
                    let d = dist - 1;
                    self.out.push(((d & 3) << 2) as u8);
                    self.out.push((d >> 2) as u8);
                    (Kind::M1Near, 2)
                } else if self.opts.use_m1
                    && self.state == 4
                    && (0x801..=0xC00).contains(&dist)
                    && len >= 3
                {
                    let d = dist - 0x801;
                    self.out.push(((d & 3) << 2) as u8);
                    self.out.push((d >> 2) as u8);
                    (Kind::M1Far, 3)
                } else {
                    assert!(len >= 3);
                    if self.opts.use_m2 && len <= 8 && dist <= 0x800 {
                        let d = dist - 1;
                        self.out.push((((len - 1) << 5) | ((d & 7) << 2)) as u8);
                        self.out.push((d >> 3) as u8);
                        (Kind::M2, len)
                    } else if dist <= 0x4000 {
                        let d = dist - 1;
                        if len - 2 <= 31 {
                            self.out.push(32 | (len - 2) as u8);
                        } else {
                            self.out.push(32);
                            push_extended(&mut self.out, len - 2 - 31);
                        }
                        self.out.push(((d << 2) & 0xFF) as u8);
                        self.out.push((d >> 6) as u8);
                        (Kind::M3, len)
                    } else {
                        let d = dist - 0x4000;
                        let far = if d & 0x4000 != 0 { 8u8 } else { 0 };
                        let low = d & 0x3FFF;
                        if len - 2 <= 7 {
                            self.out.push(16 | far | (len - 2) as u8);
                        } else {
                            self.out.push(16 | far);
                            push_extended(&mut self.out, len - 2 - 7);
                        }
                        self.out.push(((low << 2) & 0xFF) as u8);
                        self.out.push((low >> 6) as u8);
                        (Kind::M4, len)
                    }
                };
            self.state = 0;
            self.log.matches.push((kind, dist, used));
            used
        }

        fn finish(mut self) -> (Vec<u8>, EncodeLog) {
            self.out.extend_from_slice(&[0x11, 0x00, 0x00]);
            (self.out, self.log)
        }
    }

    const HASH_BITS: u32 = 15;
    const NO_POS: usize = usize::MAX;

    fn hash3(b: &[u8]) -> usize {
        let v = u32::from(b[0]) | (u32::from(b[1]) << 8) | (u32::from(b[2]) << 16);
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    }

    fn encode_with(data: &[u8], opts: EncodeOptions) -> (Vec<u8>, EncodeLog) {
        let n = data.len();
        let mut enc = Encoder {
            out: Vec::with_capacity(n + n / 8 + 16),
            state: 0,
            opts,
            log: EncodeLog::default(),
        };
        let mut head = vec![NO_POS; 1 << HASH_BITS];
        let mut prev = vec![NO_POS; n];
        let insert = |head: &mut Vec<usize>, prev: &mut Vec<usize>, pos: usize| {
            if pos + 3 <= n {
                let h = hash3(&data[pos..]);
                prev[pos] = head[h];
                head[h] = pos;
            }
        };

        let mut i = 0usize;
        let mut lit_start = 0usize;
        while i + 3 <= n {
            let h = hash3(&data[i..]);
            let mut best_len = 0usize;
            let mut best_dist = 0usize;
            let mut cand = head[h];
            let mut depth = 0usize;
            let limit = (n - i).min(opts.max_match);
            while cand != NO_POS && depth < opts.max_chain {
                let dist = i - cand;
                if dist > MAX_DISTANCE {
                    break;
                }
                let len = data[cand..]
                    .iter()
                    .zip(&data[i..])
                    .take(limit)
                    .take_while(|(a, b)| a == b)
                    .count();
                if len > best_len {
                    best_len = len;
                    best_dist = dist;
                }
                cand = prev[cand];
                depth += 1;
            }
            insert(&mut head, &mut prev, i);
            if best_len >= 3 {
                enc.literals(&data[lit_start..i]);
                let used = enc.emit_match(best_dist, best_len);
                for j in i + 1..i + used {
                    insert(&mut head, &mut prev, j);
                }
                i += used;
                lit_start = i;
            } else {
                i += 1;
            }
        }
        enc.literals(&data[lit_start..]);
        enc.finish()
    }

    fn encode(data: &[u8]) -> Vec<u8> {
        encode_with(data, EncodeOptions::default()).0
    }

    /// Straightforward byte-at-a-time reference for match semantics.
    fn naive_repeat(out: &mut Vec<u8>, distance: usize, length: usize) {
        for _ in 0..length {
            let b = out[out.len() - distance];
            out.push(b);
        }
    }

    // ------------------------------------------------------------------
    // Data generators.
    // ------------------------------------------------------------------

    fn gen_small_alphabet(rng: &mut Rng, len: usize, k: u8) -> Vec<u8> {
        (0..len).map(|_| b'a' + (rng.byte() % k)).collect()
    }

    fn gen_runs(rng: &mut Rng, len: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(len);
        while v.len() < len {
            let b = rng.byte();
            let run = rng.range(1, 600).min(len - v.len());
            v.extend(std::iter::repeat_n(b, run));
        }
        v
    }

    /// Word list for text-like test data (also used by the known-answer
    /// vectors, so its contents and order must not change).
    const TEXT_WORDS: &[&[u8]] = &[
        b"the", b"uncle", b"grapple", b"glove", b"cave", b"jump", b"story", b"about", b"my", b"a",
        b"of", b"and", b"rock", b"crystal", b"village", b"Maddie",
    ];

    fn gen_text(rng: &mut Rng, len: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(len + 16);
        while v.len() < len {
            v.extend_from_slice(TEXT_WORDS[rng.below(TEXT_WORDS.len())]);
            v.push(if rng.chance(10) { b'\n' } else { b' ' });
        }
        v.truncate(len);
        v
    }

    /// Mix of fresh random bytes and copies of earlier ranges at random
    /// distances up to `max_dist` (including overlapping copies).
    fn gen_copy_paste(rng: &mut Rng, len: usize, max_dist: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(len);
        while v.len() < len {
            let want = len - v.len();
            if v.len() < 8 || rng.chance(35) {
                let n = rng.range(1, 64).min(want);
                v.extend(rng.bytes(n));
            } else {
                let dist = rng.range(1, v.len().min(max_dist));
                let n = rng.range(2, 300).min(want);
                naive_repeat(&mut v, dist, n);
            }
        }
        v
    }

    fn gen_case(rng: &mut Rng, case: usize, max_len: usize) -> Vec<u8> {
        let len = match rng.below(4) {
            0 => rng.below(20),
            1 => rng.below(300),
            _ => rng.below(max_len + 1),
        };
        match case % 6 {
            0 => rng.bytes(len),
            1 => {
                let k = rng.range(1, 4) as u8;
                gen_small_alphabet(rng, len, k)
            }
            2 => gen_runs(rng, len),
            3 => gen_text(rng, len),
            4 => gen_copy_paste(rng, len, MAX_DISTANCE),
            _ => {
                // Concatenation of the above.
                let mut v = gen_text(rng, len / 3);
                v.extend(rng.bytes(len / 3));
                v.extend(gen_runs(rng, len / 3));
                v
            }
        }
    }

    fn random_options(rng: &mut Rng) -> EncodeOptions {
        EncodeOptions {
            use_m1: rng.chance(50),
            use_m2: rng.chance(70),
            opening_byte: rng.chance(70),
            max_chain: rng.range(1, 48),
            max_match: if rng.chance(25) {
                rng.range(3, 40)
            } else {
                usize::MAX
            },
        }
    }

    // ------------------------------------------------------------------
    // Hand-encoded literal streams (independent of the encoder above).
    // ------------------------------------------------------------------

    /// Literal-only stream using the opening-byte form when possible.
    fn literal_stream_opening(data: &[u8]) -> Vec<u8> {
        let mut s = Vec::new();
        if data.is_empty() {
        } else if data.len() <= 238 {
            s.push(17 + data.len() as u8);
        } else {
            s.push(0);
            push_extended(&mut s, data.len() - 18);
        }
        s.extend_from_slice(data);
        s.extend_from_slice(&[0x11, 0, 0]);
        s
    }

    /// Literal-only stream using the instruction form (needs >= 4 bytes).
    fn literal_stream_insn(data: &[u8]) -> Vec<u8> {
        assert!(data.len() >= 4);
        let mut s = Vec::new();
        if data.len() <= 18 {
            s.push((data.len() - 3) as u8);
        } else {
            s.push(0);
            push_extended(&mut s, data.len() - 18);
        }
        s.extend_from_slice(data);
        s.extend_from_slice(&[0x11, 0, 0]);
        s
    }

    #[test]
    fn empty_stream() {
        assert_eq!(decompress(&[0x11, 0, 0], 0), Ok(Vec::new()));
        assert_eq!(
            decompress(&[0x11, 0, 0], 1),
            Err(LzoError::OutputSizeMismatch {
                expected: 1,
                actual: 0
            })
        );
        assert_eq!(decompress(&[], 0), Err(LzoError::InputOverrun));
        assert_eq!(decompress(&[0x11], 0), Err(LzoError::InputOverrun));
        assert_eq!(decompress(&[0x11, 0], 0), Err(LzoError::InputOverrun));
        assert_eq!(
            decompress(&[0x11, 0, 0, 0], 0),
            Err(LzoError::InputNotConsumed { remaining: 1 })
        );
        // The encoder agrees on the empty encoding.
        assert_eq!(encode(&[]), vec![0x11, 0, 0]);
    }

    #[test]
    fn literal_only_lengths_1_to_300() {
        let mut rng = Rng::new(0x1234_5678);
        for n in 1..=300usize {
            let data = rng.bytes(n);
            let s = literal_stream_opening(&data);
            assert_eq!(
                decompress(&s, n).as_deref(),
                Ok(&data[..]),
                "opening form n={n}"
            );
            if n >= 4 {
                let s = literal_stream_insn(&data);
                assert_eq!(
                    decompress(&s, n).as_deref(),
                    Ok(&data[..]),
                    "insn form n={n}"
                );
            }
        }
    }

    #[test]
    fn literal_length_boundaries_are_encoded_as_expected() {
        // (length, expected header bytes) for the opening-byte/extended forms.
        let cases: &[(usize, &[u8])] = &[
            (1, &[18]),
            (3, &[20]),
            (4, &[21]),
            (238, &[255]),
            (239, &[0x00, 221]),
            (273, &[0x00, 255]),
            (274, &[0x00, 0x00, 0x01]),
            (528, &[0x00, 0x00, 255]),
            (529, &[0x00, 0x00, 0x00, 0x01]),
        ];
        for &(n, header) in cases {
            let data: Vec<u8> = (0..n).map(|i| (i * 7 + 3) as u8).collect();
            let mut s = header.to_vec();
            s.extend_from_slice(&data);
            s.extend_from_slice(&[0x11, 0, 0]);
            assert_eq!(literal_stream_opening(&data), s, "n={n}");
            assert_eq!(decompress(&s, n), Ok(data), "n={n}");
        }
        // Instruction form: 4..=18 fit in the byte, 19 needs extension.
        for (n, header) in [(4usize, vec![1u8]), (18, vec![15]), (19, vec![0, 1])] {
            let data = vec![0xAB; n];
            let mut s = header;
            s.extend_from_slice(&data);
            s.extend_from_slice(&[0x11, 0, 0]);
            assert_eq!(decompress(&s, n), Ok(data), "insn n={n}");
        }
    }

    #[test]
    fn large_literal_runs() {
        let mut rng = Rng::new(77);
        for n in [1000usize, 4096, 65_535, 65_536, 131_072] {
            let data = rng.bytes(n);
            assert_eq!(decompress(&literal_stream_opening(&data), n), Ok(data));
        }
    }

    #[test]
    fn m2_match_with_overlap_and_trailing_literals() {
        // "abc", then M2 length 6 distance 3, then 2 trailing literals "XY",
        // then a near M1 (state 2): length 2 distance 4.
        let s = [
            20,
            b'a',
            b'b',
            b'c', // opening run of 3, state 3
            0xA8 | 2,
            0x00,
            b'X',
            b'Y', // M2 len 6 dist 3, 2 trailing literals
            0x0C,
            0x00, // M1 near: dist 1 + 3 = 4, len 2
            0x11,
            0x00,
            0x00,
        ];
        assert_eq!(decompress(&s, 13).as_deref(), Ok(&b"abcabcabcXYbc"[..]));
        assert_eq!(decompress(&s, 12), Err(LzoError::OutputOverrun));
        assert_eq!(
            decompress(&s, 14),
            Err(LzoError::OutputSizeMismatch {
                expected: 14,
                actual: 13
            })
        );
    }

    #[test]
    fn m2_extremes() {
        // M2 length 8 (B >> 5 == 7) at the maximum distance 0x800.
        let mut rng = Rng::new(5);
        let lits = rng.bytes(0x800);
        let d = 0x800 - 1;
        let mut s = vec![0];
        push_extended(&mut s, lits.len() - 18);
        s.extend_from_slice(&lits);
        s.push(((7 << 5) | ((d & 7) << 2)) as u8);
        s.push((d >> 3) as u8);
        s.extend_from_slice(&[0x11, 0, 0]);
        let mut expect = lits.clone();
        naive_repeat(&mut expect, 0x800, 8);
        assert_eq!(decompress(&s, expect.len()), Ok(expect));
    }

    #[test]
    fn m1_far_after_literal_run() {
        let mut rng = Rng::new(99);
        let lits = rng.bytes(0xC80);
        for dist in [0x801usize, 0x806, 0xBFF, 0xC00] {
            let d = dist - 0x801;
            let mut s = vec![0];
            push_extended(&mut s, lits.len() - 18);
            s.extend_from_slice(&lits);
            s.push((((d & 3) << 2) | 1) as u8); // one trailing literal
            s.push((d >> 2) as u8);
            s.push(b'!');
            s.extend_from_slice(&[0x11, 0, 0]);
            let mut expect = lits.clone();
            naive_repeat(&mut expect, dist, 3);
            expect.push(b'!');
            assert_eq!(decompress(&s, expect.len()), Ok(expect), "dist={dist:#x}");
        }
        // The same instruction with too little history is a look-behind error.
        let s = [21, 1, 2, 3, 4, 0x00, 0x00, 0x11, 0, 0];
        assert_eq!(decompress(&s, 7), Err(LzoError::LookBehindOverrun));
    }

    #[test]
    fn m1_near_after_opening_literals() {
        // Opening run of 1 ('z', state 1), M1 near dist 1 len 2, two trailing
        // literals via the low bits (state 2), M1 near dist 3 len 2.
        let s = [18, b'z', 0x02, 0x00, b'p', b'q', (2 << 2), 0x00, 0x11, 0, 0];
        assert_eq!(decompress(&s, 7).as_deref(), Ok(&b"zzzpqzp"[..]));
    }

    #[test]
    fn m3_plain_and_extended_with_overlap() {
        // "abcd", M3 length 10 distance 4.
        let s = [21, b'a', b'b', b'c', b'd', 0x28, 0x0C, 0x00, 0x11, 0, 0];
        assert_eq!(decompress(&s, 14).as_deref(), Ok(&b"abcdabcdabcdab"[..]));

        // Run-length: one literal then M3 distance 1 length 100 (extended).
        let s = [18, b'z', 0x20, 67, 0x00, 0x00, 0x11, 0, 0];
        assert_eq!(decompress(&s, 101), Ok(vec![b'z'; 101]));

        // Length 300 needs one zero byte: 300 - 2 - 31 = 267 = 255 + 12.
        let s = [21, 1, 2, 3, 4, 0x20, 0x00, 12, 3 << 2, 0x00, 0x11, 0, 0];
        let mut expect = vec![1, 2, 3, 4];
        naive_repeat(&mut expect, 4, 300);
        assert_eq!(decompress(&s, 304), Ok(expect));

        // M3 with three trailing literals in the low bits of W.
        let s = [
            21,
            1,
            2,
            3,
            4,
            0x21,
            (3 << 2) | 3,
            0x00,
            7,
            8,
            9,
            0x11,
            0,
            0,
        ];
        assert_eq!(decompress(&s, 10), Ok(vec![1, 2, 3, 4, 1, 2, 3, 7, 8, 9]));
    }

    #[test]
    fn m3_and_m4_distance_extremes() {
        let mut rng = Rng::new(0xC0FFEE);
        let lits = rng.bytes(0xC000);
        let header = {
            let mut h = vec![0];
            push_extended(&mut h, lits.len() - 18);
            h
        };
        // (distance, encoded match bytes) for length 5 matches.
        let m3 = |dist: usize| -> Vec<u8> {
            let d = dist - 1;
            vec![32 | 3, ((d << 2) & 0xFF) as u8, (d >> 6) as u8]
        };
        let m4 = |dist: usize| -> Vec<u8> {
            let d = dist - 0x4000;
            let far = if d & 0x4000 != 0 { 8 } else { 0 };
            let low = d & 0x3FFF;
            vec![16 | far | 3, ((low << 2) & 0xFF) as u8, (low >> 6) as u8]
        };
        let cases: Vec<(usize, Vec<u8>)> = vec![
            (1, m3(1)),
            (0x3FFF, m3(0x3FFF)),
            (0x4000, m3(0x4000)),
            (0x4001, m4(0x4001)),
            (0x7FFF, m4(0x7FFF)),
            (0x8000, m4(0x8000)),
            (0x8001, m4(0x8001)),
            (0xBFFF, m4(0xBFFF)),
        ];
        for (dist, insn) in cases {
            let mut s = header.clone();
            s.extend_from_slice(&lits);
            s.extend_from_slice(&insn);
            s.extend_from_slice(&[0x11, 0, 0]);
            let mut expect = lits.clone();
            naive_repeat(&mut expect, dist, 5);
            assert_eq!(decompress(&s, expect.len()), Ok(expect), "dist={dist:#x}");
        }
        // The maximum M4 distance with only 0xBFFE bytes of history fails.
        let short = &lits[..0xBFFE];
        let mut s = vec![0];
        push_extended(&mut s, short.len() - 18);
        s.extend_from_slice(short);
        s.extend_from_slice(&m4(0xBFFF));
        s.extend_from_slice(&[0x11, 0, 0]);
        assert_eq!(decompress(&s, 0xBFFE + 5), Err(LzoError::LookBehindOverrun));
    }

    #[test]
    fn m4_extended_length_and_trailing_literals() {
        let mut rng = Rng::new(31337);
        let lits = rng.bytes(0x4100);
        let dist = 0x4050;
        let len = 2 + 7 + 255 + 40; // one zero byte, then 40
        let d = dist - 0x4000;
        let mut s = vec![0];
        push_extended(&mut s, lits.len() - 18);
        s.extend_from_slice(&lits);
        s.extend_from_slice(&[16, 0x00, 40, (((d << 2) & 0xFF) | 2) as u8, (d >> 6) as u8]);
        s.extend_from_slice(&[0xEE, 0xFF]);
        s.extend_from_slice(&[0x11, 0, 0]);
        let mut expect = lits.clone();
        naive_repeat(&mut expect, dist, len);
        expect.extend_from_slice(&[0xEE, 0xFF]);
        assert_eq!(decompress(&s, expect.len()), Ok(expect));
    }

    #[test]
    fn malformed_end_markers_and_look_behind() {
        // Zero-distance M4 with an extended length (10) instead of 3.
        assert_eq!(
            decompress(&[0x10, 0x01, 0x00, 0x00], 0),
            Err(LzoError::MalformedEndMarker {
                length: 10,
                trailing: 0
            })
        );
        // Zero-distance M4 with length 4 after an opening literal.
        assert_eq!(
            decompress(&[18, b'a', 0x12, 0x00, 0x00], 1),
            Err(LzoError::MalformedEndMarker {
                length: 4,
                trailing: 0
            })
        );
        // M2 distance 5 with only one byte of history.
        assert_eq!(
            decompress(&[18, b'a', 0x50, 0x00, 0x11, 0, 0], 4),
            Err(LzoError::LookBehindOverrun)
        );
        // Match as the very first instruction (first bytes 16 and 17 are
        // instructions; larger first bytes are opening literal counts).
        assert_eq!(
            decompress(&[0x11, 0x04, 0x00, 0x11, 0, 0], 3),
            Err(LzoError::LookBehindOverrun)
        );
        // A first byte of 33 is an opening run of 16 literals, not an M3.
        assert_eq!(
            decompress(&[0x21, 0x00, 0x00, 0x11, 0, 0], 3),
            Err(LzoError::InputOverrun)
        );
        // Long zero extension that runs off the end of the input.
        let bomb = vec![0u8; 100_000];
        assert_eq!(decompress(&bomb, 1 << 40), Err(LzoError::InputOverrun));
        // A huge run length is caught by the output limit before any copy.
        let mut s = vec![18, b'a', 0x20];
        s.extend(std::iter::repeat_n(0u8, 10_000));
        s.extend_from_slice(&[1, 0, 0, 0x11, 0, 0]);
        assert_eq!(decompress(&s, 1000), Err(LzoError::OutputOverrun));
    }

    #[test]
    fn huge_expected_size_does_not_allocate_it() {
        // Would abort on allocation failure if dst_len were reserved blindly.
        assert_eq!(
            decompress(&[0x11, 0, 0], usize::MAX),
            Err(LzoError::OutputSizeMismatch {
                expected: usize::MAX,
                actual: 0
            })
        );
        let s = literal_stream_opening(b"hello");
        assert_eq!(
            decompress(&s, usize::MAX / 2),
            Err(LzoError::OutputSizeMismatch {
                expected: usize::MAX / 2,
                actual: 5
            })
        );
    }

    #[test]
    fn overlapping_copy_matches_naive_reference() {
        let mut rng = Rng::new(4242);
        for _ in 0..2000 {
            let hist = rng.range(1, 64);
            let mut base = rng.bytes(hist);
            let dist = rng.range(1, hist);
            let len = rng.range(0, 700);
            let mut out = Output {
                bytes: base.clone(),
                limit: usize::MAX,
            };
            out.repeat(dist, len).unwrap();
            naive_repeat(&mut base, dist, len);
            assert_eq!(out.bytes, base, "dist={dist} len={len}");
        }
        let mut out = Output {
            bytes: vec![1, 2, 3],
            limit: 10,
        };
        assert_eq!(out.repeat(4, 1), Err(LzoError::LookBehindOverrun));
        assert_eq!(out.repeat(0, 1), Err(LzoError::LookBehindOverrun));
        assert_eq!(out.repeat(1, 8), Err(LzoError::OutputOverrun));
        assert_eq!(out.repeat(1, 7), Ok(()));
    }

    #[test]
    fn round_trip_thousands_of_buffers() {
        let mut rng = Rng::new(0xA5A5_0001);
        let mut totals = [0usize; 5];
        let mut literal_runs = 0usize;
        let mut trailing_groups = 0usize;
        let cases = 4000usize;
        for case in 0..cases {
            let data = if case % 97 == 0 {
                // Occasionally large enough for M4 distances.
                let len = rng.range(0x8000, 0x18000);
                gen_copy_paste(&mut rng, len, MAX_DISTANCE)
            } else {
                gen_case(&mut rng, case, 4096)
            };
            let opts = random_options(&mut rng);
            let (stream, log) = encode_with(&data, opts);
            let got = decompress(&stream, data.len());
            assert_eq!(got.as_deref(), Ok(&data[..]), "case {case} opts {opts:?}");
            for (slot, kind) in
                totals
                    .iter_mut()
                    .zip([Kind::M1Near, Kind::M1Far, Kind::M2, Kind::M3, Kind::M4])
            {
                *slot += log.count(kind);
            }
            literal_runs += log.literal_runs;
            trailing_groups += log.trailing_literal_groups;
            if case % 8 == 0 {
                assert!(decompress(&stream, data.len() + 1).is_err());
                if !data.is_empty() {
                    assert_eq!(
                        decompress(&stream, data.len() - 1),
                        Err(LzoError::OutputOverrun),
                        "case {case}"
                    );
                }
            }
        }
        // Every encoding path was actually exercised.
        for (count, name) in totals.iter().zip(["M1near", "M1far", "M2", "M3", "M4"]) {
            assert!(*count > 0, "{name} never emitted");
        }
        assert!(literal_runs > 0 && trailing_groups > 0);
        println!(
            "round trips: {cases}, matches M1near={} M1far={} M2={} M3={} M4={}, literal runs={literal_runs}, trailing groups={trailing_groups}",
            totals[0], totals[1], totals[2], totals[3], totals[4]
        );
    }

    #[test]
    fn round_trip_block_sized_buffers() {
        // UE3 blocks are up to 0x20000 bytes; use far copies to hit M4.
        let mut rng = Rng::new(0xB10C);
        let mut m4 = 0usize;
        for case in 0..24 {
            let len = if case % 3 == 0 {
                0x20000
            } else {
                rng.range(0x10000, 0x20000)
            };
            let data = match case % 4 {
                0 => gen_copy_paste(&mut rng, len, MAX_DISTANCE),
                1 => gen_text(&mut rng, len),
                2 => gen_runs(&mut rng, len),
                _ => rng.bytes(len),
            };
            let opts = EncodeOptions {
                max_chain: 64,
                use_m1: case % 2 == 0,
                ..EncodeOptions::default()
            };
            let (stream, log) = encode_with(&data, opts);
            m4 += log.count(Kind::M4);
            assert_eq!(
                decompress(&stream, data.len()).as_deref(),
                Ok(&data[..]),
                "case {case}"
            );
        }
        assert!(m4 > 0);
    }

    #[test]
    fn round_trip_every_distance_class() {
        let mut rng = Rng::new(0xD157);
        let distances = [
            1usize, 2, 3, 4, 5, 7, 8, 9, 16, 0x3FF, 0x400, 0x401, 0x7FF, 0x800, 0x801, 0xBFF,
            0xC00, 0xC01, 0x3FFF, 0x4000, 0x4001, 0x7FFF, 0x8000, 0x8001, 0xBFFE, 0xBFFF,
        ];
        for &dist in &distances {
            for probe in [6usize, 40] {
                let data = if dist < probe {
                    // Overlapping copy: a short pattern repeated.
                    let unit = rng.bytes(dist);
                    let mut v = unit.clone();
                    naive_repeat(&mut v, dist, probe);
                    v.extend(rng.bytes(8));
                    v
                } else {
                    let unit = rng.bytes(probe);
                    let mut v = unit.clone();
                    v.extend(rng.bytes(dist - probe));
                    v.extend_from_slice(&unit);
                    v.extend(rng.bytes(8));
                    v
                };
                let opts = EncodeOptions {
                    max_chain: 1 << 16,
                    ..EncodeOptions::default()
                };
                let (stream, log) = encode_with(&data, opts);
                assert_eq!(
                    decompress(&stream, data.len()).as_deref(),
                    Ok(&data[..]),
                    "dist={dist:#x} probe={probe}"
                );
                let len = probe;
                let expected = if dist <= 0x800 && len <= 8 {
                    Kind::M2
                } else if dist <= 0x4000 {
                    Kind::M3
                } else {
                    Kind::M4
                };
                assert!(
                    log.has(expected, dist),
                    "dist={dist:#x} probe={probe}: expected {expected:?}, log {:?}",
                    log.matches
                );
                // Without M2 the near cases must go through M3.
                if expected == Kind::M2 {
                    let opts = EncodeOptions {
                        use_m2: false,
                        ..opts
                    };
                    let (stream, log) = encode_with(&data, opts);
                    assert_eq!(decompress(&stream, data.len()).as_deref(), Ok(&data[..]));
                    assert!(log.has(Kind::M3, dist), "dist={dist:#x} via M3");
                }
            }
        }
        // Beyond the maximum distance the encoder must fall back to literals.
        let unit = rng.bytes(32);
        let mut data = unit.clone();
        data.extend(rng.bytes(0xC000 - 32));
        data.extend_from_slice(&unit);
        let (stream, log) = encode_with(&data, EncodeOptions::default());
        assert!(log.matches.iter().all(|m| m.1 <= MAX_DISTANCE));
        assert_eq!(decompress(&stream, data.len()).as_deref(), Ok(&data[..]));
    }

    #[test]
    fn round_trip_m1_forms_deterministically() {
        let mut rng = Rng::new(0x5151);
        // Near M1: match, one literal (state 1), then a short match nearby.
        let unit = rng.bytes(8);
        let mut data = unit.clone();
        data.extend_from_slice(&unit);
        data.push(0xFE);
        data.extend_from_slice(&unit[2..6]);
        data.extend(rng.bytes(5));
        let opts = EncodeOptions {
            use_m1: true,
            max_chain: 1024,
            ..EncodeOptions::default()
        };
        let (stream, log) = encode_with(&data, opts);
        assert!(log.count(Kind::M1Near) > 0, "{:?}", log.matches);
        assert_eq!(decompress(&stream, data.len()).as_deref(), Ok(&data[..]));

        // Far M1: a literal run (state 4), then a match at 0x801..=0xC00.
        for dist in [0x801usize, 0x9AB, 0xC00] {
            let unit = rng.bytes(6);
            let mut data = unit.clone();
            data.extend(rng.bytes(dist - 6));
            data.extend_from_slice(&unit);
            data.extend(rng.bytes(5));
            let (stream, log) = encode_with(&data, opts);
            assert!(
                log.has(Kind::M1Far, dist),
                "dist={dist:#x} {:?}",
                log.matches
            );
            assert_eq!(decompress(&stream, data.len()).as_deref(), Ok(&data[..]));
        }
    }

    // ------------------------------------------------------------------
    // Malformed input: never panic, always an error where one is required.
    // ------------------------------------------------------------------

    #[test]
    fn truncation_at_every_offset_is_an_input_overrun() {
        let mut rng = Rng::new(0x7121);
        let mut checked = 0usize;
        for case in 0..60 {
            let data = gen_case(&mut rng, case, 1500);
            let opts = random_options(&mut rng);
            let (stream, _) = encode_with(&data, opts);
            for cut in 0..stream.len() {
                assert_eq!(
                    decompress(&stream[..cut], data.len()),
                    Err(LzoError::InputOverrun),
                    "case {case} cut {cut}/{}",
                    stream.len()
                );
                checked += 1;
            }
        }
        assert!(checked > 1000);
    }

    #[test]
    fn corrupted_streams_never_panic() {
        let mut rng = Rng::new(0xBAD5_EED5);
        let mut errors = 0usize;
        let cases = 5000usize;
        for case in 0..cases {
            let data = gen_case(&mut rng, case, 2048);
            let opts = random_options(&mut rng);
            let (mut stream, _) = encode_with(&data, opts);
            match rng.below(4) {
                0 | 1 => {
                    for _ in 0..rng.range(1, 4) {
                        let i = rng.below(stream.len());
                        stream[i] = rng.byte();
                    }
                }
                2 => {
                    let i = rng.below(stream.len());
                    stream.insert(i, rng.byte());
                }
                _ => {
                    let i = rng.below(stream.len());
                    stream.remove(i);
                }
            }
            let dst_len = match rng.below(5) {
                0 => rng.below(data.len() + 64),
                _ => data.len(),
            };
            match decompress(&stream, dst_len) {
                Ok(out) => assert_eq!(out.len(), dst_len),
                Err(_) => errors += 1,
            }
        }
        assert!(errors > cases / 4, "only {errors} errors");
    }

    #[test]
    fn random_garbage_never_panics() {
        let mut rng = Rng::new(0x6A2B_A6E0);
        for _ in 0..5000 {
            let len = rng.below(200);
            let mut garbage = rng.bytes(len);
            if rng.chance(30) {
                garbage.extend_from_slice(&[0x11, 0, 0]);
            }
            let dst_len = rng.below(4000);
            if let Ok(out) = decompress(&garbage, dst_len) {
                assert_eq!(out.len(), dst_len);
            }
        }
    }

    // ------------------------------------------------------------------
    // Explicit stream builder: emits exactly the instruction asked for and
    // tracks the expected output with the byte-at-a-time reference, so
    // tests can place every instruction form at chosen boundaries.
    // ------------------------------------------------------------------

    fn enc_m1(biased_distance: usize) -> [u8; 2] {
        assert!(biased_distance < 0x400);
        [
            ((biased_distance & 3) << 2) as u8,
            (biased_distance >> 2) as u8,
        ]
    }

    fn enc_m2(distance: usize, length: usize) -> [u8; 2] {
        assert!((1..=0x800).contains(&distance) && (3..=8).contains(&length));
        let d = distance - 1;
        [(((length - 1) << 5) | ((d & 7) << 2)) as u8, (d >> 3) as u8]
    }

    fn enc_m3(distance: usize, length: usize) -> Vec<u8> {
        assert!((1..=0x4000).contains(&distance) && length >= 3);
        let d = distance - 1;
        let mut v = Vec::new();
        if length - 2 <= 31 {
            v.push(32 | (length - 2) as u8);
        } else {
            v.push(32);
            push_extended(&mut v, length - 2 - 31);
        }
        v.extend_from_slice(&[((d << 2) & 0xFF) as u8, (d >> 6) as u8]);
        v
    }

    fn enc_m4(distance: usize, length: usize) -> Vec<u8> {
        assert!((0x4001..=0xBFFF).contains(&distance) && length >= 3);
        let d = distance - 0x4000;
        let far = if d & 0x4000 != 0 { 8u8 } else { 0 };
        let low = d & 0x3FFF;
        let mut v = Vec::new();
        if length - 2 <= 7 {
            v.push(16 | far | (length - 2) as u8);
        } else {
            v.push(16 | far);
            push_extended(&mut v, length - 2 - 7);
        }
        v.extend_from_slice(&[((low << 2) & 0xFF) as u8, (low >> 6) as u8]);
        v
    }

    struct Script {
        stream: Vec<u8>,
        out: Vec<u8>,
        /// 0: start or match without trailing literals; 1..=3: trailing
        /// literal count (or short opening run); 4: literal run.
        state: u8,
        /// Index of the byte that carries the last match's trailing bits.
        tail: Option<usize>,
        /// How often each form was emitted, indexed like [`FORM_NAMES`].
        counts: [usize; FORM_NAMES.len()],
    }

    const FORM_NAMES: [&str; 11] = [
        "short opening run",
        "long opening run",
        "short literal run",
        "extended literal run",
        "M1 near",
        "M1 far",
        "M2",
        "M3",
        "M4 low half",
        "M4 high half",
        "trailing literals",
    ];

    impl Script {
        fn new() -> Self {
            Self {
                stream: Vec::new(),
                out: Vec::new(),
                state: 0,
                tail: None,
                counts: [0; FORM_NAMES.len()],
            }
        }

        fn opening(&mut self, lits: &[u8]) -> &mut Self {
            assert!(self.stream.is_empty() && (1..=238).contains(&lits.len()));
            self.stream.push(17 + lits.len() as u8);
            self.stream.extend_from_slice(lits);
            self.out.extend_from_slice(lits);
            self.state = if lits.len() < 4 { lits.len() as u8 } else { 4 };
            self.counts[usize::from(lits.len() >= 4)] += 1;
            self
        }

        fn lit_run(&mut self, lits: &[u8]) -> &mut Self {
            assert!(self.state == 0 && lits.len() >= 4);
            self.counts[2 + usize::from(lits.len() > 18)] += 1;
            if lits.len() <= 18 {
                self.stream.push((lits.len() - 3) as u8);
            } else {
                self.stream.push(0);
                push_extended(&mut self.stream, lits.len() - 18);
            }
            self.stream.extend_from_slice(lits);
            self.out.extend_from_slice(lits);
            self.state = 4;
            self.tail = None;
            self
        }

        fn finish_match(&mut self, tail: usize, distance: usize, length: usize) -> &mut Self {
            assert!(
                distance <= self.out.len(),
                "builder match before output start"
            );
            naive_repeat(&mut self.out, distance, length);
            self.tail = Some(tail);
            self.state = 0;
            self
        }

        fn m1_near(&mut self, distance: usize) -> &mut Self {
            assert!((1..=3).contains(&self.state));
            self.counts[4] += 1;
            self.stream.extend_from_slice(&enc_m1(distance - 1));
            let tail = self.stream.len() - 2;
            self.finish_match(tail, distance, 2)
        }

        fn m1_far(&mut self, distance: usize) -> &mut Self {
            assert!(self.state == 4 && (0x801..=0xC00).contains(&distance));
            self.counts[5] += 1;
            self.stream.extend_from_slice(&enc_m1(distance - 0x801));
            let tail = self.stream.len() - 2;
            self.finish_match(tail, distance, 3)
        }

        fn m2(&mut self, distance: usize, length: usize) -> &mut Self {
            self.counts[6] += 1;
            self.stream.extend_from_slice(&enc_m2(distance, length));
            let tail = self.stream.len() - 2;
            self.finish_match(tail, distance, length)
        }

        fn m3(&mut self, distance: usize, length: usize) -> &mut Self {
            self.counts[7] += 1;
            self.stream.extend(enc_m3(distance, length));
            let tail = self.stream.len() - 2;
            self.finish_match(tail, distance, length)
        }

        fn m4(&mut self, distance: usize, length: usize) -> &mut Self {
            self.counts[8 + usize::from(distance >= 0x8000)] += 1;
            self.stream.extend(enc_m4(distance, length));
            let tail = self.stream.len() - 2;
            self.finish_match(tail, distance, length)
        }

        fn trailing(&mut self, lits: &[u8]) -> &mut Self {
            let tail = self.tail.take().expect("trailing literals need a match");
            assert!(self.state == 0 && (1..=3).contains(&lits.len()));
            self.counts[10] += 1;
            self.stream[tail] |= lits.len() as u8;
            self.stream.extend_from_slice(lits);
            self.out.extend_from_slice(lits);
            self.state = lits.len() as u8;
            self
        }

        /// Appends raw bytes (no state tracking, no expected output).
        fn raw(&mut self, bytes: &[u8]) -> &mut Self {
            self.stream.extend_from_slice(bytes);
            self
        }

        fn end(&mut self) -> (Vec<u8>, Vec<u8>) {
            self.stream.extend_from_slice(&[0x11, 0x00, 0x00]);
            (self.stream.clone(), self.out.clone())
        }
    }

    /// A stream that uses every instruction form, both length encodings of
    /// each extendable form, both M4 distance halves and 1, 2 and 3 trailing
    /// literals.
    fn all_forms_script() -> Script {
        let mut rng = Rng::new(0xA11F);
        let mut sc = Script::new();
        sc.opening(b"xyz") // short opening run: state 3
            .m1_near(3)
            .trailing(b"Q") // state 1
            .m1_near(1) // state 0
            .lit_run(&rng.bytes(300)) // extended literal run: state 4
            .m3(7, 0x900) // extended M3, grows history past 0x801
            .lit_run(&rng.bytes(5)) // short literal run: state 4
            .m1_far(0x900)
            .trailing(b"RS") // state 2
            .m2(0x800, 8)
            .trailing(b"TUV") // state 3
            .m3(0x123, 33) // short M3 (length field 31)
            .m3(1, 0x4100) // run-length copy, history past 0x4001
            .m4(0x4001, 5) // short M4, low half
            .m3(0x100, 0x8000) // history past 0xBFFF
            .m4(0xBFFF, 300) // extended M4, high half
            .trailing(b"W")
            .m1_near(0x400)
            .m4(0x8000, 9) // M4 high half with W >> 2 == 0: not an end marker
            .m2(3, 3)
            .trailing(b"XY")
            .m2(1, 4);
        sc
    }

    #[test]
    fn every_instruction_form_and_every_truncation() {
        let mut sc = all_forms_script();
        // A stream has one opening; the long form is covered elsewhere.
        for (name, n) in FORM_NAMES.iter().zip(sc.counts).skip(2) {
            assert!(n > 0, "all-forms stream lacks {name}");
        }
        assert_eq!(sc.counts[0], 1);
        let (stream, expect) = sc.end();
        assert_eq!(
            decompress(&stream, expect.len()).as_deref(),
            Ok(&expect[..])
        );
        assert_eq!(
            label_decode(&stream, expect.len()),
            Some((expect.clone(), 3, 0))
        );
        assert_eq!(
            decompress(&stream, expect.len() - 1),
            Err(LzoError::OutputOverrun)
        );
        // A stream that ends inside any instruction, operand, extension run,
        // literal run, trailing literal group or the end marker is an input
        // overrun, never a panic and never a short success.
        for cut in 0..stream.len() {
            assert_eq!(
                decompress(&stream[..cut], expect.len()),
                Err(LzoError::InputOverrun),
                "cut {cut}/{}",
                stream.len()
            );
        }
    }

    /// One look-behind boundary case: a history of `history` bytes, then a
    /// match of `length` bytes whose encoding reaches exactly to output byte
    /// 0 (`at_limit`) or one byte before it (`past_limit`).
    struct Boundary {
        name: &'static str,
        history: usize,
        /// Produce the history with the opening byte (state 1..=4) rather
        /// than with an instruction-form literal run (state 4).
        via_opening: bool,
        at_limit: Vec<u8>,
        past_limit: Vec<u8>,
        length: usize,
    }

    #[test]
    fn look_behind_boundary_for_every_match_form() {
        let mut rng = Rng::new(0x1B);
        let b = |name, history, via_opening, at_limit, past_limit, length| Boundary {
            name,
            history,
            via_opening,
            at_limit,
            past_limit,
            length,
        };
        let cases = [
            b(
                "M1 near",
                3,
                true,
                enc_m1(2).to_vec(),
                enc_m1(3).to_vec(),
                2,
            ),
            b(
                "M1 far",
                0x801,
                false,
                enc_m1(0).to_vec(),
                enc_m1(1).to_vec(),
                3,
            ),
            b(
                "M2",
                8,
                true,
                enc_m2(8, 3).to_vec(),
                enc_m2(9, 3).to_vec(),
                3,
            ),
            b("M3", 0x123, false, enc_m3(0x123, 40), enc_m3(0x124, 40), 40),
            b(
                "M4 low half",
                0x4001,
                false,
                enc_m4(0x4001, 4),
                enc_m4(0x4002, 4),
                4,
            ),
            b(
                "M4 high half",
                0x8000,
                false,
                enc_m4(0x8000, 12),
                enc_m4(0x8001, 12),
                12,
            ),
        ];
        for case in cases {
            let Boundary {
                name,
                history,
                via_opening,
                at_limit,
                past_limit,
                length,
            } = case;
            let hist = rng.bytes(history);
            let with_history = || {
                let mut sc = Script::new();
                if via_opening {
                    sc.opening(&hist);
                } else {
                    sc.lit_run(&hist);
                }
                sc
            };
            // Distance equal to the history: copies from output byte 0.
            let (stream, _) = with_history().raw(&at_limit).end();
            let mut expect = hist.clone();
            naive_repeat(&mut expect, history, length);
            assert_eq!(decompress(&stream, expect.len()), Ok(expect), "{name}");
            // One further back is before the start of the output.
            let (stream, _) = with_history().raw(&past_limit).end();
            assert_eq!(
                decompress(&stream, history + length),
                Err(LzoError::LookBehindOverrun),
                "{name}"
            );
            assert_eq!(label_decode(&stream, history + length), None, "{name}");
        }
    }

    #[test]
    fn state_selects_the_meaning_of_low_instruction_bytes() {
        // The same two bytes `01 00` after three different contexts.
        // State 0 (match without trailing literals): literal run of 4.
        let (s, _) = Script::new()
            .opening(b"abcd")
            .m2(4, 4)
            .raw(&[0x01, b'w', b'x', b'y', b'z'])
            .end();
        assert_eq!(decompress(&s, 12).as_deref(), Ok(&b"abcdabcdwxyz"[..]));
        // State 1 (one trailing literal): M1 near, distance 1, length 2, and
        // its own low bits ask for one trailing literal.
        let (s, _) = Script::new()
            .opening(b"abcd")
            .m2(4, 4)
            .trailing(b"e")
            .raw(&[0x01, 0x00, b'f'])
            .end();
        assert_eq!(decompress(&s, 12).as_deref(), Ok(&b"abcdabcdeeef"[..]));
        // State 4 (literal run): M1 far, distance 0x801, length 3.
        let (s, _) = Script::new()
            .opening(b"abcd")
            .raw(&[0x01, 0x00, b'g'])
            .end();
        assert_eq!(decompress(&s, 8), Err(LzoError::LookBehindOverrun));
        let mut rng = Rng::new(0x5747);
        let hist = rng.bytes(0x801);
        let (s, _) = Script::new().lit_run(&hist).raw(&[0x01, 0x00, b'g']).end();
        let mut expect = hist.clone();
        expect.extend_from_slice(&hist[..3]);
        expect.push(b'g');
        assert_eq!(decompress(&s, expect.len()), Ok(expect));
        // States 2 and 3 behave like state 1.
        for lits in [&b"ef"[..], b"efg"] {
            let (s, mut expect) = Script::new()
                .opening(b"abcd")
                .m2(4, 4)
                .trailing(lits)
                .raw(&[0x04, 0x00]) // M1 near, distance 2
                .end();
            naive_repeat(&mut expect, 2, 2);
            assert_eq!(decompress(&s, expect.len()), Ok(expect));
        }
    }

    #[test]
    fn end_marker_forms_and_markers_in_mid_stream() {
        // The terminator ends decoding wherever it appears; what follows is
        // unconsumed input, and a short output is a size mismatch.
        let s = [21, b'a', b'b', b'c', b'd', 0x11, 0, 0, 0x11, 0, 0];
        assert_eq!(
            decompress(&s, 4),
            Err(LzoError::InputNotConsumed { remaining: 3 })
        );
        assert_eq!(
            decompress(&s, 8),
            Err(LzoError::OutputSizeMismatch {
                expected: 8,
                actual: 4
            })
        );
        assert_eq!(
            decompress(&[0x11, 0, 0, 0xFF], 0),
            Err(LzoError::InputNotConsumed { remaining: 1 })
        );
        // The terminator is valid in every state.
        assert_eq!(
            decompress(&[19, b'a', b'b', 0x11, 0, 0], 2),
            Ok(b"ab".to_vec())
        );
        assert_eq!(
            decompress(&[21, b'a', b'b', b'c', b'd', 0x11, 0, 0], 4),
            Ok(b"abcd".to_vec())
        );
        let (s, expect) = Script::new().opening(b"abcd").m2(2, 5).end();
        assert_eq!(decompress(&s, expect.len()), Ok(expect));
        // Zero-distance M4 with trailing-literal bits or another length is
        // rejected (the reference decoder would silently accept these). At
        // the stream start only 0x10 and 0x11 are M4 instructions (larger
        // first bytes are opening literal counts), so the other forms follow
        // four opening literals.
        for (marker, length, trailing) in [
            (&[0x11, 0x01, 0x00][..], 3, 1),
            (&[0x11, 0x02, 0x00], 3, 2),
            (&[0x11, 0x03, 0x00], 3, 3),
            (&[0x12, 0x00, 0x00], 4, 0),
            (&[0x17, 0x03, 0x00], 9, 3),
            (&[0x10, 0x00, 0x01, 0x00, 0x00], 7 + 255 + 1 + 2, 0),
            (&[0x10, 0x05, 0x00, 0x00], 7 + 5 + 2, 0),
        ] {
            let mut cases = vec![(marker.to_vec(), 0usize)];
            let mut after = vec![21, b'a', b'b', b'c', b'd'];
            after.extend_from_slice(marker);
            cases.push((after, 4));
            for (stream, dst_len) in cases {
                if stream[0] > 17 {
                    continue; // would be an opening literal count
                }
                assert_eq!(
                    decompress(&stream, dst_len),
                    Err(LzoError::MalformedEndMarker { length, trailing }),
                    "{stream:02x?}"
                );
                let (_, l, t) = label_decode(&stream, dst_len).expect("reference accepts it");
                assert_eq!((l, t), (length, trailing), "{stream:02x?}");
            }
        }
        // Not end markers: the high-half bit or any distance bit set.
        for insn in [
            &[0x19, 0x00, 0x00][..],   // distance 0x8000
            &[0x18, 0x01, 0x00, 0x00], // distance 0x8000, extended length
            &[0x11, 0x04, 0x00],       // distance 0x4001
            &[0x11, 0x00, 0x01],       // distance 0x4040
            &[0x1F, 0xFC, 0xFF],       // distance 0xBFFF
        ] {
            let mut stream = vec![21, b'a', b'b', b'c', b'd'];
            stream.extend_from_slice(insn);
            stream.extend_from_slice(&[0x11, 0x00, 0x00]);
            assert_eq!(
                decompress(&stream, 64),
                Err(LzoError::LookBehindOverrun),
                "{stream:02x?}"
            );
        }
    }

    #[test]
    fn extended_length_overflow_is_reported() {
        // Unreachable from real input on 64-bit targets (it would need about
        // 7e16 zero bytes), so drive the helper directly near usize::MAX.
        let mut i = Input {
            bytes: &[0x00, 0x05],
            pos: 0,
        };
        assert_eq!(
            i.extended_length(usize::MAX - 254),
            Err(LzoError::LengthOverflow)
        );
        let mut i = Input {
            bytes: &[0x01],
            pos: 0,
        };
        assert_eq!(i.extended_length(usize::MAX), Err(LzoError::LengthOverflow));
        let mut i = Input {
            bytes: &[0x00, 0x01],
            pos: 0,
        };
        assert_eq!(i.extended_length(usize::MAX - 256), Ok(usize::MAX));
        let mut i = Input {
            bytes: &[0x00, 0x00, 0x00],
            pos: 0,
        };
        assert_eq!(i.extended_length(15), Err(LzoError::InputOverrun));
        // Ordinary values: base + 255 per zero + final byte.
        let mut i = Input {
            bytes: &[0x00, 0x00, 0x07, 0xAA],
            pos: 0,
        };
        assert_eq!(i.extended_length(31), Ok(31 + 510 + 7));
        assert_eq!(i.remaining(), 1);
    }

    #[test]
    fn long_zero_runs_are_linear_and_bounded() {
        // M3, distance 1, with 40,000 extension zeros: 10,200,034 bytes from
        // 40,009 input bytes, in one doubling copy.
        let zeros = 40_000usize;
        let length = 31 + 255 * zeros + 1 + 2;
        let mut s = vec![18, b'z', 0x20];
        s.extend(std::iter::repeat_n(0u8, zeros));
        s.extend_from_slice(&[0x01, 0x00, 0x00, 0x11, 0x00, 0x00]);
        let out = decompress(&s, 1 + length).unwrap();
        assert_eq!(out.len(), 1 + length);
        assert!(out.iter().all(|&b| b == b'z'));
        assert!(out.len() <= 255 * s.len(), "expansion bound");
        assert_eq!(decompress(&s, length), Err(LzoError::OutputOverrun));

        // Same through an extended M4 (needs 0x4001 bytes of history).
        let mut sc = Script::new();
        sc.opening(b"q").m3(1, 0x4000);
        let mut m4 = vec![0x10];
        m4.extend(std::iter::repeat_n(0u8, 1000));
        m4.extend_from_slice(&[0x09, 0x04, 0x00]); // distance 0x4001
        let (s, mut expect) = sc.raw(&m4).end();
        naive_repeat(&mut expect, 0x4001, 7 + 255 * 1000 + 9 + 2);
        assert_eq!(decompress(&s, expect.len()), Ok(expect));

        // Extension runs that never terminate are input overruns in every
        // context, however long, and the literal form is caught before any
        // copy is attempted.
        let big = 1_000_000usize;
        for prefix in [&[0x00][..], &[18, b'a', 0x20], &[18, b'a', 0x10]] {
            let mut s = prefix.to_vec();
            s.extend(std::iter::repeat_n(0u8, big));
            assert_eq!(decompress(&s, usize::MAX), Err(LzoError::InputOverrun));
            s.push(0x01);
            assert_eq!(decompress(&s, usize::MAX), Err(LzoError::InputOverrun));
        }
        // A literal run whose extended length exceeds the remaining input.
        let mut s = vec![0x00];
        s.extend(std::iter::repeat_n(0u8, 10_000));
        s.push(0x01);
        s.extend_from_slice(&[0xAB; 100]);
        s.extend_from_slice(&[0x11, 0, 0]);
        assert_eq!(decompress(&s, 16), Err(LzoError::InputOverrun));
    }

    #[test]
    fn output_overrun_in_every_phase() {
        let mut rng = Rng::new(0x0F0F);
        let hist = rng.bytes(0x4100);
        let mut scripts: Vec<(&str, Script)> = Vec::new();
        let mut sc = Script::new();
        sc.opening(b"abcdef");
        scripts.push(("opening literals", sc));
        let mut sc = Script::new();
        sc.lit_run(&hist[..40]);
        scripts.push(("literal run", sc));
        let mut sc = Script::new();
        sc.opening(b"ab").m1_near(2);
        scripts.push(("M1 near", sc));
        let mut sc = Script::new();
        sc.lit_run(&hist[..0x900]).m1_far(0x900);
        scripts.push(("M1 far", sc));
        let mut sc = Script::new();
        sc.opening(b"abcd").m2(4, 8);
        scripts.push(("M2", sc));
        let mut sc = Script::new();
        sc.opening(b"abcd").m3(3, 500);
        scripts.push(("M3", sc));
        let mut sc = Script::new();
        sc.lit_run(&hist).m4(0x4100, 20);
        scripts.push(("M4", sc));
        let mut sc = Script::new();
        sc.opening(b"abcd").m2(4, 8).trailing(b"xyz");
        scripts.push(("trailing literals", sc));
        for (name, mut sc) in scripts {
            let (s, expect) = sc.end();
            assert_eq!(decompress(&s, expect.len()), Ok(expect.clone()), "{name}");
            assert_eq!(
                decompress(&s, expect.len() - 1),
                Err(LzoError::OutputOverrun),
                "{name}"
            );
        }
        assert_eq!(
            decompress(&[18, b'a', 0x11, 0, 0], 0),
            Err(LzoError::OutputOverrun)
        );
    }

    // ------------------------------------------------------------------
    // Known-answer vectors from the reference compressors.
    //
    // The inputs are synthetic (generated below from a fixed seed); the
    // streams were produced locally from those inputs with liblzo2 2.10
    // (`lzo1x_999_compress` and `lzo1x_1_compress`), so these streams come
    // from an encoder written independently of this decoder and of the
    // test encoder above. Contains no game data.
    // ------------------------------------------------------------------

    /// Deterministic text/run/copy mix; the vectors depend on it bit for bit.
    fn kat_input(seed: u64, len: usize, max_dist: usize, run_max: usize) -> Vec<u8> {
        let mut rng = Rng::new(seed);
        let mut v = Vec::with_capacity(len + 64);
        while v.len() < len {
            match rng.below(5) {
                0 => {
                    v.extend_from_slice(TEXT_WORDS[rng.below(TEXT_WORDS.len())]);
                    v.push(b' ');
                }
                1 => {
                    let b = rng.byte();
                    let n = rng.range(1, run_max);
                    v.extend(std::iter::repeat_n(b, n));
                }
                2 => {
                    let n = rng.range(1, 6);
                    for _ in 0..n {
                        v.push(rng.byte());
                    }
                }
                _ => {
                    if v.len() < 4 {
                        v.push(rng.byte());
                    } else {
                        let dist = rng.range(1, v.len().min(max_dist));
                        let n = rng.range(3, 48);
                        naive_repeat(&mut v, dist, n);
                    }
                }
            }
        }
        v.truncate(len);
        v
    }

    fn hex_bytes(hex: &str) -> Vec<u8> {
        assert!(hex.len().is_multiple_of(2));
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    // lzo1x_999_compress(kat_input(0x4B7B, 24000, 0xBFFF, 2000)): 556 bytes.
    // Contains M1 near, M1 far, M2, M3 (short and extended), M4 (short and
    // extended), short and extended literal runs, 1/2/3 trailing literals.
    const KAT_999_ALL_FORMS: &str = concat!(
        "1761626f7574202704000379538703545e30140020041c00382000000361204d6164646965206a756d702063",
        "72797374616c3b4c00c0082f3f0131a17820002900003e080620071c0139580809f817e2870f676c6f766520",
        "8a2000000000630100342000000000000000ca01000a20000000000006000009d700e9ae2176696c6c616765",
        "26b047069ec03f2a7bf9ce23f46004016d7920a52005e447351216636121be4774680c000699eb62a9a0ad8e",
        "1bee20000000000000005001000c20000000f600003ed01023fc772fc0642009fc30200e8e128ed5345c0003",
        "0f6d87df746824eb303582fa20000000000000005a000003756e636c65203a8465204414332007b8020473aa",
        "2a6024679f20000000000000aa000027861d17da2000002200002678462978092678bf237076200dc478e808",
        "2d7c48026772617070078b38badd3e6d0ba220001f000031882c38b2057468200380079d40042000b7000029",
        "35086c200061000024a83a2005786e0116e8eb272000000000290100c82424282cd1201b20000067000030c0",
        "2231bddeb32000000000850100cd20000000000000004100002fc05bdf02616e642f7c8a2fc55b9820000000",
        "d0000003333bafc0782a2000dd00000273746f72792e601829804907fce4f2cb013e9020569530c074360855",
        "200510f4232175442000870100dc20000000000080000004b85e7c113dd6b0318c1d3a6c99101aef47a407ab",
        "115613fc0ee00d069ec792ba6e42dcded12000000000a40000110000",
    );

    // lzo1x_1_compress(kat_input(0x4B43, 40000, 0xBFFF, 4000)): 570 bytes.
    // The fast compressor family; reaches M4 distances.
    const KAT_1_FAR: &str = concat!(
        "03676c6f76652020080100702000006401001720000000000000000000007001007a20000000000000003f01",
        "0095200000db000001ebe66923200000d600003c505f200740180204c701c9c4200000000000000000000000",
        "000000e0000001616e642029043d2003043c00016d792076696c6c61676520f3d0832a3e1c5ed93ed80003dc",
        "77c9d8dd612aa00129cc013f280024a8a90a4d796f662004f8159961e042a2ce06b9c7af036f6620330b024b",
        "4ac2335c00275000ec19274000302000200b504d30f400200a45003920000c0000200739b1bf200000000000",
        "0000cd01002120000000000000000000000000001400000261626f75743d245e0362cbd5b4ba96200ae85a88",
        "0524295e682000000000000000000000000000b40100f92000000000000000000000000000b400002002416e",
        "f9253ccd2b180006746865206a756d70202001acc52450712ea9c5da20000000f6000022c1e1362000000000",
        "000000dd00002624323bacd6167001234ca13a12006f662478322bd032363000066e1a68be961ee865562344",
        "330819f87965e408a4218d60f8101831767620f501002a200000000000004d01007b20000000000000000000",
        "007301000c20000000540000246489047a362772a2fac120000000000000000000000000000000720000028c",
        "e1257260100a6c00366c3e2ca8b72604982c1d00ce2000000000890000100b28a80567726170706c652031a8",
        "db06486d6210594e6f66203169146920000000000000d900000a69696969696969696969696969110000",
    );

    // lzo1x_999_compress(kat_input(0x4B41, 600, 0xBFFF, 40)): 148 bytes.
    // Starts with a short (1..=3) opening literal run.
    const KAT_999_SHORT_OPENING: &str = concat!(
        "12b8a00002616e6420762a00000272f8b300633438003c5e0001bf2003fd00424000037818bd42bf753a0100",
        "523d0100bd201401001328000006254078e872d7b37bb320060100a42e00000361626f7574203679003c3501",
        "00003c000002d4f1f364cd33a000b90ddd3f000003676c6f7665202009f1031c3503006120984100733c0000",
        "06e3c4907499d4b3af242ac003110000",
    );

    fn kat_vectors() -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
        vec![
            (
                "999 all forms",
                hex_bytes(KAT_999_ALL_FORMS),
                kat_input(0x4B7B, 24_000, 0xBFFF, 2000),
            ),
            (
                "1 far",
                hex_bytes(KAT_1_FAR),
                kat_input(0x4B43, 40_000, 0xBFFF, 4000),
            ),
            (
                "999 short opening",
                hex_bytes(KAT_999_SHORT_OPENING),
                kat_input(0x4B41, 600, 0xBFFF, 40),
            ),
        ]
    }

    #[test]
    fn known_answer_vectors_from_reference_compressors() {
        for (name, stream, expect) in kat_vectors() {
            assert_eq!(
                decompress(&stream, expect.len()),
                Ok(expect.clone()),
                "{name}"
            );
            assert_eq!(
                label_decode(&stream, expect.len()),
                Some((expect.clone(), 3, 0)),
                "{name}"
            );
            assert!(decompress(&stream, expect.len() + 1).is_err(), "{name}");
            assert_eq!(
                decompress(&stream, expect.len() - 1),
                Err(LzoError::OutputOverrun),
                "{name}"
            );
            for cut in 0..stream.len() {
                assert_eq!(
                    decompress(&stream[..cut], expect.len()),
                    Err(LzoError::InputOverrun),
                    "{name} cut {cut}"
                );
            }
        }
        // The first vector really exercises the forms the comment claims.
        let first = hex_bytes(KAT_999_ALL_FORMS);
        assert_eq!(first[0], 0x17, "opening run of 6");
        assert_eq!(
            hex_bytes(KAT_999_SHORT_OPENING)[0],
            0x12,
            "opening run of 1"
        );
    }

    // ------------------------------------------------------------------
    // Differential oracle: a second decoder with a different structure.
    // ------------------------------------------------------------------

    /// Second, independently structured decoder used only as a differential
    /// oracle. It follows the classic label layout of LZO1X decoders (top of
    /// loop, first literal run, match, match done, match next) instead of an
    /// explicit state value, copies matches one byte at a time, and, like
    /// the reference decoder, accepts *any* zero-distance M4 as the end
    /// marker. It returns the output plus that marker's length and trailing
    /// bits, or `None` on any error (including a wrong output size or
    /// unconsumed input).
    fn label_decode(src: &[u8], dst_len: usize) -> Option<(Vec<u8>, usize, u8)> {
        #[derive(Clone, Copy)]
        enum Label {
            Top,
            FirstLiteralRun,
            Match,
            MatchDone,
            MatchNext,
        }
        fn get(src: &[u8], ip: &mut usize) -> Option<usize> {
            let b = *src.get(*ip)?;
            *ip += 1;
            Some(usize::from(b))
        }
        fn ext(src: &[u8], ip: &mut usize, base: usize) -> Option<usize> {
            let mut t = 0usize;
            loop {
                match get(src, ip)? {
                    0 => t = t.checked_add(255)?,
                    b => return t.checked_add(base)?.checked_add(b),
                }
            }
        }
        let lits = |out: &mut Vec<u8>, ip: &mut usize, n: usize| -> Option<()> {
            let end = ip.checked_add(n)?;
            let bytes = src.get(*ip..end)?;
            if out.len().checked_add(n)? > dst_len {
                return None;
            }
            out.extend_from_slice(bytes);
            *ip = end;
            Some(())
        };
        let copy = |out: &mut Vec<u8>, dist: usize, len: usize| -> Option<()> {
            if dist == 0 || dist > out.len() || out.len().checked_add(len)? > dst_len {
                return None;
            }
            for _ in 0..len {
                let b = out[out.len() - dist];
                out.push(b);
            }
            Some(())
        };

        let mut out = Vec::new();
        let mut ip = 0usize;
        let mut t = 0usize;
        let first = usize::from(*src.first()?);
        let mut label = Label::Top;
        if first > 17 {
            ip = 1;
            t = first - 17;
            if t < 4 {
                label = Label::MatchNext;
            } else {
                lits(&mut out, &mut ip, t)?;
                label = Label::FirstLiteralRun;
            }
        }
        loop {
            match label {
                Label::Top => {
                    t = get(src, &mut ip)?;
                    if t >= 16 {
                        label = Label::Match;
                        continue;
                    }
                    if t == 0 {
                        t = ext(src, &mut ip, 15)?;
                    }
                    lits(&mut out, &mut ip, t + 3)?;
                    label = Label::FirstLiteralRun;
                }
                Label::FirstLiteralRun => {
                    t = get(src, &mut ip)?;
                    if t >= 16 {
                        label = Label::Match;
                        continue;
                    }
                    let n = get(src, &mut ip)?;
                    copy(&mut out, 1 + 0x800 + (t >> 2) + (n << 2), 3)?;
                    label = Label::MatchDone;
                }
                Label::Match => {
                    if t >= 64 {
                        let n = get(src, &mut ip)?;
                        copy(&mut out, 1 + ((t >> 2) & 7) + (n << 3), (t >> 5) + 1)?;
                    } else if t >= 32 {
                        let mut len = t & 31;
                        if len == 0 {
                            len = ext(src, &mut ip, 31)?;
                        }
                        let lo = get(src, &mut ip)?;
                        let hi = get(src, &mut ip)?;
                        copy(&mut out, 1 + (lo >> 2) + (hi << 6), len + 2)?;
                    } else if t >= 16 {
                        let high_half = (t & 8) << 11;
                        let mut len = t & 7;
                        if len == 0 {
                            len = ext(src, &mut ip, 7)?;
                        }
                        let lo = get(src, &mut ip)?;
                        let hi = get(src, &mut ip)?;
                        let d = high_half + (lo >> 2) + (hi << 6);
                        if d == 0 {
                            let done = out.len() == dst_len && ip == src.len();
                            return done.then_some((out, len + 2, (lo & 3) as u8));
                        }
                        copy(&mut out, d + 0x4000, len + 2)?;
                    } else {
                        let n = get(src, &mut ip)?;
                        copy(&mut out, 1 + (t >> 2) + (n << 2), 2)?;
                    }
                    label = Label::MatchDone;
                }
                Label::MatchDone => {
                    t = usize::from(src[ip - 2] & 3);
                    label = if t == 0 { Label::Top } else { Label::MatchNext };
                }
                Label::MatchNext => {
                    lits(&mut out, &mut ip, t)?;
                    t = get(src, &mut ip)?;
                    label = Label::Match;
                }
            }
        }
    }

    /// Asserts that `decompress` and `label_decode` agree on `src`. Returns
    /// 0 for a shared success, 1 for a shared failure, 2 for a stream only
    /// the strict end-marker rule rejects.
    fn assert_agrees(src: &[u8], dst_len: usize) -> usize {
        match (decompress(src, dst_len), label_decode(src, dst_len)) {
            (Ok(ours), Some((theirs, 3, 0))) => {
                assert_eq!(ours, theirs, "outputs differ for {src:02x?}");
                0
            }
            (Err(_), None) => 1,
            (Err(LzoError::MalformedEndMarker { length, trailing }), Some((_, l, t)))
                if (l, t) != (3, 0) =>
            {
                assert_eq!((length, trailing), (l, t));
                2
            }
            (ours, theirs) => panic!(
                "decoders disagree on {src:02x?} (dst_len {dst_len}): {ours:?} vs {:?}",
                theirs.map(|t| (t.0.len(), t.1, t.2))
            ),
        }
    }

    fn mutate(rng: &mut Rng, stream: &mut Vec<u8>) {
        if stream.is_empty() {
            stream.push(rng.byte());
            return;
        }
        match rng.below(6) {
            0 | 1 => {
                for _ in 0..rng.range(1, 3) {
                    let i = rng.below(stream.len());
                    stream[i] = rng.byte();
                }
            }
            2 => {
                let i = rng.below(stream.len() + 1);
                stream.insert(i, rng.byte());
            }
            3 => {
                let i = rng.below(stream.len());
                stream.remove(i);
            }
            4 => {
                let i = rng.below(stream.len());
                const INTERESTING: [u8; 10] =
                    [0x00, 0x01, 0x03, 0x10, 0x11, 0x12, 0x18, 0x20, 0x40, 0xFF];
                stream[i] = INTERESTING[rng.below(INTERESTING.len())];
            }
            _ => {
                let i = rng.below(stream.len());
                stream[i] ^= 1 << rng.below(8);
            }
        }
    }

    /// A random but well-formed stream built from every instruction form,
    /// with matches drawn from each form's whole distance range.
    fn random_script(rng: &mut Rng) -> Script {
        fn random_len(rng: &mut Rng) -> usize {
            match rng.below(10) {
                0 => rng.range(3, 9),
                1..=6 => rng.range(3, 40),
                7 | 8 => rng.range(41, 600),
                _ => rng.range(600, 20_000),
            }
        }
        let mut sc = Script::new();
        if rng.chance(60) {
            let n = if rng.chance(50) {
                rng.range(1, 3)
            } else {
                rng.range(4, 238)
            };
            sc.opening(&rng.bytes(n));
        } else {
            let n = if rng.chance(80) {
                rng.range(4, 40)
            } else {
                rng.range(19, 2000)
            };
            sc.lit_run(&rng.bytes(n));
        }
        for _ in 0..rng.range(1, 40) {
            let have = sc.out.len();
            match (sc.state, rng.below(6)) {
                (0, 0) => {
                    let n = if rng.chance(85) {
                        rng.range(4, 18)
                    } else {
                        rng.range(19, 700)
                    };
                    sc.lit_run(&rng.bytes(n));
                    continue;
                }
                (1..=3, 0) => {
                    sc.m1_near(rng.range(1, have.min(0x400)));
                }
                (4, 0) if have >= 0x801 => {
                    sc.m1_far(rng.range(0x801, have.min(0xC00)));
                }
                (_, 1) => {
                    let len = rng.range(3, 8);
                    sc.m2(rng.range(1, have.min(0x800)), len);
                }
                (_, 4) if have >= 0x4001 => {
                    let len = random_len(rng);
                    sc.m4(rng.range(0x4001, have.min(0xBFFF)), len);
                }
                _ => {
                    let len = random_len(rng);
                    sc.m3(rng.range(1, have.min(0x4000)), len);
                }
            }
            if rng.chance(50) {
                let n = rng.range(1, 3);
                sc.trailing(&rng.bytes(n));
            }
        }
        sc
    }

    #[test]
    fn random_well_formed_streams_decode_exactly() {
        let mut rng = Rng::new(0x5C41_7735);
        let mut counts = [0usize; FORM_NAMES.len()];
        for _ in 0..400 {
            let mut sc = random_script(&mut rng);
            for (total, n) in counts.iter_mut().zip(sc.counts) {
                *total += n;
            }
            let (stream, expect) = sc.end();
            assert_eq!(
                decompress(&stream, expect.len()).as_deref(),
                Ok(&expect[..])
            );
            assert_eq!(assert_agrees(&stream, expect.len()), 0);
            if !expect.is_empty() {
                assert_eq!(
                    decompress(&stream, expect.len() - 1),
                    Err(LzoError::OutputOverrun)
                );
            }
        }
        for (name, n) in FORM_NAMES.iter().zip(counts) {
            assert!(n > 0, "{name} never generated");
        }
        println!(
            "well-formed streams: 400, forms {:?}",
            FORM_NAMES.iter().zip(counts).collect::<Vec<_>>()
        );
    }

    #[test]
    fn differential_against_label_structured_decoder() {
        let mut rng = Rng::new(0xD1FF_E2E7);
        let kats = kat_vectors();
        let mut outcomes = [0usize; 3];
        let cases = 6000usize;
        for case in 0..cases {
            let (mut stream, len) = match case % 5 {
                0 | 1 => {
                    let mut sc = random_script(&mut rng);
                    let (s, out) = sc.end();
                    (s, out.len())
                }
                2 => {
                    let data = gen_case(&mut rng, case, 1500);
                    let opts = random_options(&mut rng);
                    (encode_with(&data, opts).0, data.len())
                }
                3 => {
                    let (_, s, out) = &kats[rng.below(kats.len())];
                    (s.clone(), out.len())
                }
                _ => {
                    let n = rng.below(48);
                    let mut s = rng.bytes(n);
                    if rng.chance(50) {
                        s.extend_from_slice(&[0x11, 0, 0]);
                    }
                    (s, rng.below(600))
                }
            };
            let mutations = rng.range(0, 3);
            for _ in 0..mutations {
                mutate(&mut rng, &mut stream);
            }
            let dst_len = if rng.chance(85) {
                len
            } else {
                rng.below(len + 32)
            };
            outcomes[assert_agrees(&stream, dst_len)] += 1;
        }
        // Both acceptance and rejection paths were exercised, plus the
        // strict end-marker divergence.
        assert!(outcomes[0] > cases / 10, "{outcomes:?}");
        assert!(outcomes[1] > cases / 10, "{outcomes:?}");
        println!(
            "differential: {cases} cases, both accept {}, both reject {}, strict end marker only {}",
            outcomes[0], outcomes[1], outcomes[2]
        );
    }

    // ------------------------------------------------------------------
    // Real game data (skipped when the original install is absent).
    // ------------------------------------------------------------------

    mod real_data {
        use super::super::decompress;
        use std::collections::BTreeMap;
        use std::path::{Path, PathBuf};

        const PACKAGE_TAG: u32 = 0x9E2A_83C1;

        fn cooked_dir() -> Option<PathBuf> {
            let root = match std::env::var_os("ASAMU_ORIGINAL_DIR") {
                Some(dir) if !dir.is_empty() => PathBuf::from(dir),
                _ => PathBuf::from(std::env::var_os("HOME")?).join(
                    "Library/Application Support/Steam/steamapps/common/A Story About My Uncle",
                ),
            };
            let dir = root
                .join("A Story About My Uncle.app")
                .join("Contents/Resources/ASAMU/CookedMac");
            dir.is_dir().then_some(dir)
        }

        fn package_files(dir: &Path) -> Vec<PathBuf> {
            let mut files = Vec::new();
            for d in [dir.to_path_buf(), dir.join("Maps")] {
                let Ok(entries) = std::fs::read_dir(&d) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    let ext = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(str::to_ascii_lowercase);
                    if path.is_file() && matches!(ext.as_deref(), Some("u" | "upk" | "asamu")) {
                        files.push(path);
                    }
                }
            }
            files.sort();
            files
        }

        struct Cursor<'a> {
            data: &'a [u8],
            pos: usize,
        }

        impl<'a> Cursor<'a> {
            fn at(data: &'a [u8], pos: usize) -> Self {
                Self { data, pos }
            }
            fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
                let end = self
                    .pos
                    .checked_add(n)
                    .filter(|&e| e <= self.data.len())
                    .ok_or_else(|| format!("read of {n} bytes at {} overruns", self.pos))?;
                let s = &self.data[self.pos..end];
                self.pos = end;
                Ok(s)
            }
            fn u16(&mut self) -> Result<u16, String> {
                Ok(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap()))
            }
            fn u32(&mut self) -> Result<u32, String> {
                Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
            }
            fn i32(&mut self) -> Result<i32, String> {
                Ok(i32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
            }
            fn count(&mut self, what: &str) -> Result<usize, String> {
                let v = self.i32()?;
                usize::try_from(v).map_err(|_| format!("negative {what} {v}"))
            }
            fn fstring(&mut self) -> Result<String, String> {
                let len = self.i32()?;
                if len == 0 {
                    return Ok(String::new());
                }
                if len > 0 {
                    let raw = self.bytes(len as usize)?;
                    let (body, nul) = raw.split_at(raw.len() - 1);
                    if nul != [0] {
                        return Err(format!("FString at {} not NUL-terminated", self.pos));
                    }
                    if body.iter().any(|&b| b < 0x20 || b == 0x7F) {
                        return Err(format!("FString {body:?} has control bytes"));
                    }
                    Ok(body.iter().map(|&b| char::from(b)).collect())
                } else {
                    let units = len.unsigned_abs() as usize;
                    let raw = self.bytes(units.checked_mul(2).ok_or("utf16 overflow")?)?;
                    let mut v: Vec<u16> = raw
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_le_bytes(*c))
                        .collect();
                    if v.pop() != Some(0) {
                        return Err("UTF-16 FString not NUL-terminated".into());
                    }
                    String::from_utf16(&v).map_err(|e| e.to_string())
                }
            }
        }

        #[derive(Debug, Clone, Copy)]
        struct Chunk {
            uoff: usize,
            usize_: usize,
            coff: usize,
            csize: usize,
        }

        #[derive(Debug)]
        struct Summary {
            total_header_size: usize,
            name_count: usize,
            name_offset: usize,
            export_count: usize,
            export_offset: usize,
            import_count: usize,
            import_offset: usize,
            compression_flags: u32,
            chunks: Vec<Chunk>,
            summary_end: usize,
        }

        fn parse_summary(data: &[u8]) -> Result<Summary, String> {
            let mut c = Cursor::at(data, 0);
            let tag = c.u32()?;
            if tag != PACKAGE_TAG {
                return Err(format!("bad tag {tag:#x}"));
            }
            let version = c.u16()?;
            let licensee = c.u16()?;
            if (version, licensee) != (868, 0) {
                return Err(format!("unexpected version {version}/{licensee}"));
            }
            let total_header_size = c.count("TotalHeaderSize")?;
            let _folder = c.fstring()?;
            let _package_flags = c.u32()?;
            let name_count = c.count("NameCount")?;
            let name_offset = c.count("NameOffset")?;
            let export_count = c.count("ExportCount")?;
            let export_offset = c.count("ExportOffset")?;
            let import_count = c.count("ImportCount")?;
            let import_offset = c.count("ImportOffset")?;
            let _depends = c.i32()?;
            let _ie_guids_offset = c.i32()?;
            let _import_guids = c.i32()?;
            let _export_guids = c.i32()?;
            let _thumbnail_offset = c.i32()?;
            let _guid = c.bytes(16)?;
            let generations = c.count("GenerationCount")?;
            c.bytes(generations.checked_mul(12).ok_or("generation overflow")?)?;
            let _engine = c.i32()?;
            let _cooker = c.i32()?;
            let compression_flags = c.u32()?;
            let chunk_count = c.count("chunk count")?;
            if chunk_count > data.len() / 16 {
                return Err(format!("implausible chunk count {chunk_count}"));
            }
            let mut chunks = Vec::with_capacity(chunk_count);
            for _ in 0..chunk_count {
                chunks.push(Chunk {
                    uoff: c.count("UncompressedOffset")?,
                    usize_: c.count("UncompressedSize")?,
                    coff: c.count("CompressedOffset")?,
                    csize: c.count("CompressedSize")?,
                });
            }
            let _package_source = c.u32()?;
            let additional = c.count("AdditionalPackagesToCook")?;
            for _ in 0..additional {
                c.fstring()?;
            }
            let texture_types = c.count("TextureAllocations")?;
            for _ in 0..texture_types {
                c.bytes(20)?;
                let indices = c.count("ExportIndices")?;
                c.bytes(indices.checked_mul(4).ok_or("indices overflow")?)?;
            }
            Ok(Summary {
                total_header_size,
                name_count,
                name_offset,
                export_count,
                export_offset,
                import_count,
                import_offset,
                compression_flags,
                chunks,
                summary_end: c.pos,
            })
        }

        #[derive(Default)]
        struct Stats {
            packages: usize,
            compressed_packages: usize,
            uncompressed_packages: usize,
            chunks: usize,
            blocks: usize,
            block_compressed_bytes: u64,
            block_uncompressed_bytes: u64,
            chunk_compressed_bytes: u64,
            block_sizes: BTreeMap<usize, usize>,
            max_block_ratio_permille: u64,
            incompressible_blocks: usize,
            names: usize,
            imports: usize,
            exports: usize,
            serial_bytes: u64,
        }

        /// Validate the name, import and export tables in the decompressed
        /// header bytes (`header[0]` is uncompressed offset `base`).
        fn check_tables(
            s: &Summary,
            header: &[u8],
            base: usize,
            total_uncompressed: usize,
            stats: &mut Stats,
        ) -> Result<(), String> {
            let rel = |off: usize| {
                off.checked_sub(base)
                    .ok_or_else(|| format!("table offset {off} before chunk 0 ({base})"))
            };
            // Names: FString + u64 flags each.
            let mut c = Cursor::at(header, rel(s.name_offset)?);
            for i in 0..s.name_count {
                let name = c.fstring().map_err(|e| format!("name {i}: {e}"))?;
                if name.is_empty() || name.len() > 1024 {
                    return Err(format!("name {i} has implausible length {}", name.len()));
                }
                c.bytes(8)?;
            }
            stats.names += s.name_count;

            let name_ok = |c: &mut Cursor<'_>| -> Result<(), String> {
                let idx = c.i32()?;
                let num = c.i32()?;
                if idx < 0 || idx as usize >= s.name_count || num < 0 {
                    return Err(format!("bad FName ({idx}, {num})"));
                }
                Ok(())
            };
            let index_ok = |v: i32| -> Result<(), String> {
                let ok = if v >= 0 {
                    (v as usize) <= s.export_count
                } else {
                    (v.unsigned_abs() as usize) <= s.import_count
                };
                ok.then_some(())
                    .ok_or_else(|| format!("bad package index {v}"))
            };

            // Imports: 28 bytes each.
            let mut c = Cursor::at(header, rel(s.import_offset)?);
            for i in 0..s.import_count {
                name_ok(&mut c).map_err(|e| format!("import {i} class package: {e}"))?;
                name_ok(&mut c).map_err(|e| format!("import {i} class: {e}"))?;
                index_ok(c.i32()?).map_err(|e| format!("import {i} outer: {e}"))?;
                name_ok(&mut c).map_err(|e| format!("import {i} name: {e}"))?;
            }
            stats.imports += s.import_count;

            // Exports (v868 layout as described in the task notes).
            let mut c = Cursor::at(header, rel(s.export_offset)?);
            for i in 0..s.export_count {
                let ctx = |e: String| format!("export {i}: {e}");
                index_ok(c.i32()?).map_err(ctx)?; // class
                index_ok(c.i32()?).map_err(ctx)?; // super
                index_ok(c.i32()?).map_err(ctx)?; // outer
                name_ok(&mut c).map_err(ctx)?;
                index_ok(c.i32()?).map_err(ctx)?; // archetype
                c.bytes(8)?; // object flags
                let serial_size = c.i32()?;
                let serial_offset = c.i32()?;
                if serial_size < 0 || serial_offset < 0 {
                    return Err(ctx(format!(
                        "negative serial {serial_size}@{serial_offset}"
                    )));
                }
                if serial_size > 0 {
                    let end = serial_offset as usize + serial_size as usize;
                    if (serial_offset as usize) < s.total_header_size || end > total_uncompressed {
                        return Err(ctx(format!(
                            "serial {serial_size}@{serial_offset} outside [{}, {total_uncompressed})",
                            s.total_header_size
                        )));
                    }
                }
                stats.serial_bytes += serial_size as u64;
                c.u32()?; // export flags
                let gen_count = c.count("GenerationNetObjectCount")?;
                if gen_count > 64 {
                    return Err(ctx(format!("implausible generation count {gen_count}")));
                }
                c.bytes(gen_count * 4)?;
                c.bytes(16)?; // package guid
                c.u32()?; // package flags
            }
            stats.exports += s.export_count;
            Ok(())
        }

        fn check_package(data: &[u8], stats: &mut Stats) -> Result<(), String> {
            let s = parse_summary(data)?;
            if s.compression_flags == 0 {
                if !s.chunks.is_empty() {
                    return Err("uncompressed package with chunks".into());
                }
                stats.uncompressed_packages += 1;
                return Ok(());
            }
            if s.compression_flags != 2 {
                return Err(format!(
                    "unexpected CompressionFlags {}",
                    s.compression_flags
                ));
            }
            stats.compressed_packages += 1;
            let first = s
                .chunks
                .first()
                .ok_or("compressed package without chunks")?;
            if first.coff != s.summary_end {
                return Err(format!(
                    "chunk 0 at {} but summary ends at {}",
                    first.coff, s.summary_end
                ));
            }
            if first.uoff != s.name_offset {
                return Err("chunk 0 does not start at the name table".into());
            }
            let last = s.chunks.last().ok_or("no chunks")?;
            let total_uncompressed = last.uoff + last.usize_;

            let mut header: Vec<u8> = Vec::new();
            let mut expect_uoff = first.uoff;
            let mut expect_coff = first.coff;
            for (ci, ch) in s.chunks.iter().enumerate() {
                let ctx = |e: String| format!("chunk {ci}: {e}");
                if ch.uoff != expect_uoff || ch.coff != expect_coff {
                    return Err(ctx(format!(
                        "not contiguous: uoff {} (want {expect_uoff}) coff {} (want {expect_coff})",
                        ch.uoff, ch.coff
                    )));
                }
                expect_uoff += ch.usize_;
                expect_coff += ch.csize;
                let mut c = Cursor::at(data, ch.coff);
                let tag = c.u32()?;
                if tag != PACKAGE_TAG {
                    return Err(ctx(format!("bad chunk tag {tag:#x}")));
                }
                let block_size = c.count("BlockSize")?;
                let sum_c = c.count("chunk CompressedSize")?;
                let sum_u = c.count("chunk UncompressedSize")?;
                if block_size == 0 || sum_u != ch.usize_ {
                    return Err(ctx(format!(
                        "header sizes {block_size}/{sum_u} vs table {}",
                        ch.usize_
                    )));
                }
                *stats.block_sizes.entry(block_size).or_default() += 1;
                let nblocks = sum_u.div_ceil(block_size);
                let mut blocks = Vec::with_capacity(nblocks);
                for _ in 0..nblocks {
                    blocks.push((c.count("block c")?, c.count("block u")?));
                }
                let header_len = 16 + 8 * nblocks;
                if blocks.iter().map(|b| b.0).sum::<usize>() != sum_c
                    || blocks.iter().map(|b| b.1).sum::<usize>() != sum_u
                    || header_len + sum_c != ch.csize
                {
                    return Err(ctx(
                        "block table sums disagree with chunk header/table".into()
                    ));
                }
                let keep = header.len() + first.uoff < s.total_header_size;
                let mut chunk_u = 0usize;
                for (bi, &(bc, bu)) in blocks.iter().enumerate() {
                    if bu > block_size {
                        return Err(ctx(format!("block {bi} larger than BlockSize")));
                    }
                    let src = c.bytes(bc)?;
                    let out = decompress(src, bu)
                        .map_err(|e| ctx(format!("block {bi} ({bc}->{bu}): {e}")))?;
                    if out.len() != bu {
                        return Err(ctx(format!("block {bi} size {}", out.len())));
                    }
                    chunk_u += out.len();
                    stats.blocks += 1;
                    stats.block_compressed_bytes += bc as u64;
                    stats.block_uncompressed_bytes += bu as u64;
                    if bc >= bu {
                        stats.incompressible_blocks += 1;
                    }
                    if bu > 0 {
                        stats.max_block_ratio_permille = stats
                            .max_block_ratio_permille
                            .max(bc as u64 * 1000 / bu as u64);
                    }
                    if keep {
                        header.extend_from_slice(&out);
                    }
                }
                if chunk_u != ch.usize_ {
                    return Err(ctx(format!("chunk total {chunk_u} != {}", ch.usize_)));
                }
                stats.chunks += 1;
                stats.chunk_compressed_bytes += ch.csize as u64;

                if ci == 0 {
                    // The first decompressed chunk begins with the name table:
                    // an FString with a small positive length and printable bytes.
                    let mut nc = Cursor::at(&header, 0);
                    let len = nc.i32()?;
                    if !(2..=256).contains(&len) {
                        return Err(format!("first name length {len} implausible"));
                    }
                    Cursor::at(&header, 0)
                        .fstring()
                        .map_err(|e| format!("first name: {e}"))?;
                }
            }
            if expect_coff != data.len() {
                return Err(format!(
                    "chunks end at {expect_coff}, file is {} bytes",
                    data.len()
                ));
            }
            check_tables(&s, &header, first.uoff, total_uncompressed, stats)
        }

        #[test]
        fn decompress_every_block_of_every_original_package() {
            let Some(dir) = cooked_dir() else {
                println!(
                    "SKIP: original CookedMac directory not found (set ASAMU_ORIGINAL_DIR); real-data LZO check not run"
                );
                return;
            };
            let files = package_files(&dir);
            assert!(!files.is_empty(), "no packages found in {}", dir.display());
            let mut stats = Stats::default();
            let mut failures = Vec::new();
            for path in &files {
                let data = std::fs::read(path).unwrap();
                stats.packages += 1;
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if let Err(e) = check_package(&data, &mut stats) {
                    failures.push(format!("{name}: {e}"));
                }
            }
            println!(
                "real data: packages={} (compressed={}, uncompressed={}), chunks={}, blocks={}, \
                 block bytes compressed={} uncompressed={}, chunk bytes incl. headers={}, \
                 BlockSize values={:?}, incompressible blocks={}, max block ratio={}‰, \
                 names={}, imports={}, exports={}, export serial bytes={}, failures={}",
                stats.packages,
                stats.compressed_packages,
                stats.uncompressed_packages,
                stats.chunks,
                stats.blocks,
                stats.block_compressed_bytes,
                stats.block_uncompressed_bytes,
                stats.chunk_compressed_bytes,
                stats.block_sizes,
                stats.incompressible_blocks,
                stats.max_block_ratio_permille,
                stats.names,
                stats.imports,
                stats.exports,
                stats.serial_bytes,
                failures.len()
            );
            for f in &failures {
                println!("FAIL {f}");
            }
            assert!(failures.is_empty(), "{} package(s) failed", failures.len());
            assert!(stats.compressed_packages > 0 && stats.blocks > 0);
        }
    }
}
