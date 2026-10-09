//! Minimal reader for the uncompressed 32-bit DDS files `asamu-import
//! textures` writes for UE3 `PF_A8R8G8B8` textures (the colour-grading
//! LUTs). Only mip 0 is read; anything else is refused. Hostile-input
//! safe: every size is checked before use.

/// `"DDS "`.
const MAGIC: &[u8; 4] = b"DDS ";
/// Header size after the magic.
const HEADER_SIZE: usize = 124;
/// `DDPF_FOURCC`.
const DDPF_FOURCC: u32 = 0x4;
/// `DDPF_RGB`.
const DDPF_RGB: u32 = 0x40;
/// Largest edge accepted (LUTs are 256 × 16).
const MAX_EDGE: usize = 4096;

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Value of the channel selected by `mask` in a 32-bit texel, scaled to a
/// byte (0 when the mask is empty).
fn channel(texel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let max = mask >> shift;
    let v = (texel & mask) >> shift;
    let scaled = (u64::from(v) * 255 + u64::from(max) / 2) / u64::from(max);
    u8::try_from(scaled).unwrap_or(u8::MAX)
}

/// Mip 0 of an uncompressed 32-bit RGB(A) DDS as `(width, height, RGBA8)`.
#[must_use]
pub fn read_rgba8(data: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    if data.get(0..4)? != MAGIC || u32_at(data, 4)? as usize != HEADER_SIZE {
        return None;
    }
    let height = u32_at(data, 12)? as usize;
    let width = u32_at(data, 16)? as usize;
    if width == 0 || height == 0 || width > MAX_EDGE || height > MAX_EDGE {
        return None;
    }
    let pf_flags = u32_at(data, 80)?;
    let bits = u32_at(data, 88)?;
    if pf_flags & DDPF_FOURCC != 0 || pf_flags & DDPF_RGB == 0 || bits != 32 {
        return None;
    }
    let masks = [
        u32_at(data, 92)?,
        u32_at(data, 96)?,
        u32_at(data, 100)?,
        u32_at(data, 104)?,
    ];
    let start = 4 + HEADER_SIZE;
    let len = width.checked_mul(height)?.checked_mul(4)?;
    let texels = data.get(start..start.checked_add(len)?)?;
    let mut out = Vec::with_capacity(len);
    for t in texels.as_chunks::<4>().0 {
        let texel = u32::from_le_bytes(*t);
        out.push(channel(texel, masks[0]));
        out.push(channel(texel, masks[1]));
        out.push(channel(texel, masks[2]));
        out.push(if masks[3] == 0 {
            255
        } else {
            channel(texel, masks[3])
        });
    }
    Some((width, height, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-made 2 × 1 A8R8G8B8 DDS (bytes stored B, G, R, A).
    fn dds(width: u32, height: u32, flags: u32, bits: u32, texels: &[u8]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(b"DDS ");
        let mut h = [0u32; 31];
        h[0] = 124;
        h[2] = height;
        h[3] = width;
        h[18] = 32; // pixel format size
        h[19] = flags;
        h[21] = bits;
        h[22] = 0x00ff_0000;
        h[23] = 0x0000_ff00;
        h[24] = 0x0000_00ff;
        h[25] = 0xff00_0000;
        for v in h {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.extend_from_slice(texels);
        d
    }

    #[test]
    fn reads_bgra_texels_as_rgba() {
        let d = dds(2, 1, 0x41, 32, &[1, 2, 3, 4, 10, 20, 30, 40]);
        let (w, h, px) = read_rgba8(&d).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(px, vec![3, 2, 1, 4, 30, 20, 10, 40]);
    }

    #[test]
    fn refuses_other_formats_and_truncation() {
        let good = dds(2, 1, 0x41, 32, &[0; 8]);
        assert!(read_rgba8(&good[..good.len() - 1]).is_none());
        assert!(read_rgba8(&dds(2, 1, 0x4, 32, &[0; 8])).is_none(), "FourCC");
        assert!(
            read_rgba8(&dds(2, 1, 0x41, 24, &[0; 8])).is_none(),
            "24-bit"
        );
        assert!(read_rgba8(&dds(0, 1, 0x41, 32, &[])).is_none());
        assert!(read_rgba8(&dds(100_000, 1, 0x41, 32, &[])).is_none());
        let mut bad = good.clone();
        bad[0] = b'X';
        assert!(read_rgba8(&bad).is_none());
        for cut in 0..good.len() {
            let _ = read_rgba8(&good[..cut]);
        }
        assert_eq!(channel(0x1234, 0), 0);
        assert_eq!(channel(0b1100, 0b1100), 255);
    }
}
