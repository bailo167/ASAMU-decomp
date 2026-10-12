//! Pictures composed when a material loads
//! ([`asamu_assets::materials::ComposedTexture`]): the colour in RGB and
//! the opacity in alpha of one image, because a standard material takes
//! both from the same texture and the original often does not (a cut-out
//! whose shape is a channel of a separate mask, a beam that fades along a
//! falloff texture, a fade along a mesh's own coordinates).
//!
//! Pure: a reader for the DDS files `asamu-import textures` writes (the
//! block-compressed and plain formats materials use: DXT1, DXT3, DXT5,
//! 32-bit RGBA, 8-bit grey), the composition, and the mip chain.
//! `crate::converted` reads the files and makes the images. Hostile-input
//! safe: every size and offset is checked before use, nothing panics.
//!
//! Ours, an approximation: one colour texture and one opacity source; the
//! opacity is what the importer's reduction kept of the original's graph.

use asamu_assets::materials::{ComposedAlpha, ComposedRgb, ComposedTexture};

/// Longest edge of a composed picture, texels (larger sources are read
/// from the first mip that fits).
pub const MAX_EDGE: u32 = 1024;
/// Edge of a picture that holds only gradients, texels.
pub const GRADIENT_EDGE: u32 = 256;
/// Largest DDS file read for a composition.
pub const MAX_FILE_BYTES: u64 = 96 * 1024 * 1024;
/// Largest texture edge accepted in a DDS header.
const MAX_HEADER_EDGE: u32 = 16_384;

/// Decoded texels: tightly packed RGBA8, top row first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Texels {
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    Dxt1,
    Dxt3,
    Dxt5,
    Rgba32([u32; 4]),
    Grey8,
}

impl Layout {
    fn mip_bytes(self, w: u32, h: u32) -> Option<usize> {
        let (w, h) = (usize::try_from(w).ok()?, usize::try_from(h).ok()?);
        let blocks = w.div_ceil(4).max(1).checked_mul(h.div_ceil(4).max(1))?;
        match self {
            Layout::Dxt1 => blocks.checked_mul(8),
            Layout::Dxt3 | Layout::Dxt5 => blocks.checked_mul(16),
            Layout::Rgba32(_) => w.checked_mul(h)?.checked_mul(4),
            Layout::Grey8 => w.checked_mul(h),
        }
    }
}

fn rgb565(c: u16) -> [u8; 3] {
    let (r, g, b) = (
        u32::from(c >> 11) & 31,
        u32::from(c >> 5) & 63,
        u32::from(c) & 31,
    );
    // Bit replication; every value fits a byte.
    [
        ((r << 3) | (r >> 2)) as u8,
        ((g << 2) | (g >> 4)) as u8,
        ((b << 3) | (b >> 2)) as u8,
    ]
}

/// The 16 texels of a BC1 colour block (`punch_through`: the three-colour
/// mode with a transparent texel exists, as in DXT1).
fn colour_block(block: &[u8; 8], punch_through: bool) -> [[u8; 4]; 16] {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let (p0, p1) = (rgb565(c0), rgb565(c1));
    let mix = |a: u8, b: u8, wa: u32, wb: u32, div: u32| -> u8 {
        ((u32::from(a) * wa + u32::from(b) * wb + div / 2) / div) as u8
    };
    let blend = |wa: u32, wb: u32, div: u32| -> [u8; 4] {
        [
            mix(p0[0], p1[0], wa, wb, div),
            mix(p0[1], p1[1], wa, wb, div),
            mix(p0[2], p1[2], wa, wb, div),
            255,
        ]
    };
    let mut palette = [[0u8; 4]; 4];
    palette[0] = [p0[0], p0[1], p0[2], 255];
    palette[1] = [p1[0], p1[1], p1[2], 255];
    if c0 > c1 || !punch_through {
        palette[2] = blend(2, 1, 3);
        palette[3] = blend(1, 2, 3);
    } else {
        // Three colours and a transparent texel.
        palette[2] = blend(1, 1, 2);
    }
    let bits = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    let mut out = [[0u8; 4]; 16];
    for (i, texel) in out.iter_mut().enumerate() {
        *texel = palette[((bits >> (2 * i)) & 3) as usize];
    }
    out
}

/// The 16 alpha values of a DXT5 alpha block.
fn dxt5_alpha(block: &[u8; 8]) -> [u8; 16] {
    let (a0, a1) = (u32::from(block[0]), u32::from(block[1]));
    let mut palette = [0u8; 8];
    palette[0] = block[0];
    palette[1] = block[1];
    if a0 > a1 {
        for i in 2..8u32 {
            palette[i as usize] = (((8 - i) * a0 + (i - 1) * a1 + 3) / 7) as u8;
        }
    } else {
        for i in 2..6u32 {
            palette[i as usize] = (((6 - i) * a0 + (i - 1) * a1 + 2) / 5) as u8;
        }
        palette[6] = 0;
        palette[7] = 255;
    }
    let mut bits = 0u64;
    for (i, b) in block[2..8].iter().enumerate() {
        bits |= u64::from(*b) << (8 * i);
    }
    let mut out = [0u8; 16];
    for (i, a) in out.iter_mut().enumerate() {
        *a = palette[((bits >> (3 * i)) & 7) as usize];
    }
    out
}

/// The 16 alpha values of a DXT3 alpha block (4 bits each).
fn dxt3_alpha(block: &[u8; 8]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, a) in out.iter_mut().enumerate() {
        let nibble = (block[i / 2] >> (4 * (i % 2))) & 15;
        *a = nibble * 17;
    }
    out
}

fn decode_blocks(layout: Layout, w: u32, h: u32, data: &[u8]) -> Option<Vec<u8>> {
    let (w, h) = (usize::try_from(w).ok()?, usize::try_from(h).ok()?);
    let mut out = vec![0u8; w.checked_mul(h)?.checked_mul(4)?];
    let block_bytes = if layout == Layout::Dxt1 { 8 } else { 16 };
    let across = w.div_ceil(4).max(1);
    for (index, block) in data.chunks_exact(block_bytes).enumerate() {
        let (bx, by) = (index % across, index / across);
        let colour: &[u8; 8] = block.get(block_bytes - 8..)?.try_into().ok()?;
        let mut texels = colour_block(colour, layout == Layout::Dxt1);
        if layout != Layout::Dxt1 {
            let alpha: &[u8; 8] = block.get(..8)?.try_into().ok()?;
            let values = if layout == Layout::Dxt3 {
                dxt3_alpha(alpha)
            } else {
                dxt5_alpha(alpha)
            };
            for (t, a) in texels.iter_mut().zip(values) {
                t[3] = a;
            }
        }
        for (i, texel) in texels.iter().enumerate() {
            let (x, y) = (bx * 4 + i % 4, by * 4 + i / 4);
            if x < w
                && y < h
                && let Some(slot) = out.get_mut((y * w + x) * 4..(y * w + x) * 4 + 4)
            {
                slot.copy_from_slice(texel);
            }
        }
    }
    Some(out)
}

/// Value of the channel selected by `mask` in a 32-bit texel, as a byte
/// (255 when the format has no such channel).
fn masked(texel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 255;
    }
    let shift = mask.trailing_zeros();
    let max = u64::from(mask >> shift);
    let v = u64::from((texel & mask) >> shift);
    u8::try_from((v * 255 + max / 2) / max).unwrap_or(u8::MAX)
}

/// Decodes a 2D DDS file to RGBA8: the first mip whose longest edge is at
/// most `max_edge` (the smallest one the file has when none is). `None` for
/// anything else than a DXT1/DXT3/DXT5, 32-bit RGB(A) or 8-bit grey 2D
/// texture, or a truncated or inconsistent file.
#[must_use]
pub fn decode_dds(data: &[u8], max_edge: u32) -> Option<Texels> {
    if data.get(0..4)? != b"DDS " || u32_at(data, 4)? != 124 {
        return None;
    }
    let (height, width) = (u32_at(data, 12)?, u32_at(data, 16)?);
    if width == 0 || height == 0 || width > MAX_HEADER_EDGE || height > MAX_HEADER_EDGE {
        return None;
    }
    let mips = u32_at(data, 28)?.clamp(1, 16);
    let flags = u32_at(data, 80)?;
    let bits = u32_at(data, 88)?;
    // A cube map is not a picture for a surface.
    if u32_at(data, 112)? & 0x200 != 0 {
        return None;
    }
    let layout = if flags & 0x4 != 0 {
        match data.get(84..88)? {
            b"DXT1" => Layout::Dxt1,
            b"DXT3" => Layout::Dxt3,
            b"DXT5" => Layout::Dxt5,
            _ => return None,
        }
    } else if flags & 0x40 != 0 && bits == 32 {
        Layout::Rgba32([
            u32_at(data, 92)?,
            u32_at(data, 96)?,
            u32_at(data, 100)?,
            if flags & 0x1 != 0 {
                u32_at(data, 104)?
            } else {
                0
            },
        ])
    } else if flags & 0x2_0000 != 0 && bits == 8 {
        Layout::Grey8
    } else {
        return None;
    };
    let mut offset = 128usize;
    let (mut w, mut h) = (width, height);
    let mut level = 1;
    while level < mips && w.max(h) > max_edge {
        offset = offset.checked_add(layout.mip_bytes(w, h)?)?;
        (w, h) = ((w / 2).max(1), (h / 2).max(1));
        level += 1;
    }
    let bytes = data.get(offset..offset.checked_add(layout.mip_bytes(w, h)?)?)?;
    let rgba = match layout {
        Layout::Dxt1 | Layout::Dxt3 | Layout::Dxt5 => decode_blocks(layout, w, h, bytes)?,
        Layout::Rgba32(masks) => bytes
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|t| {
                let texel = u32::from_le_bytes(*t);
                masks.map(|m| masked(texel, m))
            })
            .collect(),
        Layout::Grey8 => bytes.iter().flat_map(|g| [*g, *g, *g, 255]).collect(),
    };
    Some(Texels {
        width: w,
        height: h,
        rgba,
    })
}

/// A composed picture with its mip chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    /// Width of the largest mip.
    pub width: u32,
    /// Height of the largest mip.
    pub height: u32,
    /// Number of mips in `data`.
    pub mips: u32,
    /// RGBA8 texels of every mip, largest first.
    pub data: Vec<u8>,
    /// The colour channels are sRGB-encoded (alpha never is).
    pub srgb: bool,
}

/// sRGB texel value (0..255, may be fractional after filtering) to linear.
fn srgb_to_linear(v: f32) -> f32 {
    let c = (v / 255.0).clamp(0.0, 1.0);
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn to_byte(v: f32) -> u8 {
    // The clamp makes the cast exact (NaN becomes 0).
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The texel of `t` at `(u, v)` in 0..1 (bilinear, clamped at the edges),
/// channels as floats in 0..255.
fn sample(t: &Texels, u: f32, v: f32) -> [f32; 4] {
    let (w, h) = (t.width.max(1) as usize, t.height.max(1) as usize);
    let at = |x: usize, y: usize| -> [f32; 4] {
        let i = (y.min(h - 1) * w + x.min(w - 1)) * 4;
        match t.rgba.get(i..i + 4) {
            Some(p) => [
                f32::from(p[0]),
                f32::from(p[1]),
                f32::from(p[2]),
                f32::from(p[3]),
            ],
            None => [0.0; 4],
        }
    };
    let (fx, fy) = (
        (u * w as f32 - 0.5).clamp(0.0, (w - 1) as f32),
        (v * h as f32 - 0.5).clamp(0.0, (h - 1) as f32),
    );
    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
    let (tx, ty) = (fx - fx.floor(), fy - fy.floor());
    let (a, b, c, d) = (
        at(x0, y0),
        at(x0 + 1, y0),
        at(x0, y0 + 1),
        at(x0 + 1, y0 + 1),
    );
    let mut out = [0.0f32; 4];
    for (i, slot) in out.iter_mut().enumerate() {
        let top = a[i] + (b[i] - a[i]) * tx;
        let bottom = c[i] + (d[i] - c[i]) * tx;
        *slot = top + (bottom - top) * ty;
    }
    out
}

/// The mips below `rgba` (`w` x `h`), each the 2 x 2 box average of the one
/// above, appended to it; returns the data and the number of mips.
fn with_mips(mut w: usize, mut h: usize, rgba: Vec<u8>) -> (Vec<u8>, u32) {
    let mut data = rgba;
    let mut start = 0usize;
    let mut count = 1u32;
    while w > 1 || h > 1 {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut next = Vec::with_capacity(nw * nh * 4);
        for y in 0..nh {
            for x in 0..nw {
                let mut sum = [0u32; 4];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (sx, sy) = ((x * 2 + dx).min(w - 1), (y * 2 + dy).min(h - 1));
                    let i = start + (sy * w + sx) * 4;
                    if let Some(p) = data.get(i..i + 4) {
                        for (s, c) in sum.iter_mut().zip(p) {
                            *s += u32::from(*c);
                        }
                    }
                }
                next.extend(sum.map(|s| ((s + 2) / 4) as u8));
            }
        }
        start = data.len();
        data.extend_from_slice(&next);
        (w, h) = (nw, nh);
        count += 1;
    }
    (data, count)
}

/// Puts the picture of `spec` together. `rgb` and `alpha` are the decoded
/// textures the specification names (`None` where it names none); `alpha_srgb`
/// says whether the opacity texture's colour channels are sRGB-encoded.
/// `None` when a texture the specification needs is missing or empty.
#[must_use]
pub fn compose(
    spec: &ComposedTexture,
    rgb: Option<&Texels>,
    alpha: Option<&Texels>,
    alpha_srgb: bool,
) -> Option<Picture> {
    let usable = |t: Option<&Texels>| -> Option<Texels> {
        let t = t?;
        let len = (t.width as usize)
            .checked_mul(t.height as usize)?
            .checked_mul(4)?;
        (t.width > 0 && t.height > 0 && t.rgba.len() == len).then(|| t.clone())
    };
    let rgb_texels = match &spec.rgb {
        ComposedRgb::Texture(_) => Some(usable(rgb)?),
        _ => None,
    };
    let alpha_texels = match &spec.alpha {
        ComposedAlpha::Texture { .. } => Some(usable(alpha)?),
        _ => None,
    };
    let sizes: Vec<(u32, u32)> = [&rgb_texels, &alpha_texels]
        .into_iter()
        .flatten()
        .map(|t| (t.width, t.height))
        .collect();
    let (width, height) = if sizes.is_empty() {
        (GRADIENT_EDGE, GRADIENT_EDGE)
    } else {
        (
            sizes.iter().map(|s| s.0).max().unwrap_or(1).min(MAX_EDGE),
            sizes.iter().map(|s| s.1).max().unwrap_or(1).min(MAX_EDGE),
        )
    };
    let (w, h) = (width as usize, height as usize);
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let v = (y as f32 + 0.5) / h as f32;
        for x in 0..w {
            let u = (x as f32 + 0.5) / w as f32;
            let colour = match (&spec.rgb, &rgb_texels) {
                (ComposedRgb::Texture(_), Some(t)) => {
                    let s = sample(t, u, v);
                    [
                        to_byte(s[0] / 255.0),
                        to_byte(s[1] / 255.0),
                        to_byte(s[2] / 255.0),
                    ]
                }
                (ComposedRgb::Gradient(g), _) => [to_byte(g.eval(u, v)); 3],
                _ => [255; 3],
            };
            let a = match (&spec.alpha, &alpha_texels) {
                (
                    ComposedAlpha::Texture {
                        channel,
                        scale,
                        bias,
                        ..
                    },
                    Some(t),
                ) => {
                    let c = (*channel).min(3);
                    let raw = sample(t, u, v)[c];
                    let value = if alpha_srgb && c < 3 {
                        srgb_to_linear(raw)
                    } else {
                        raw / 255.0
                    };
                    to_byte(value * scale + bias)
                }
                (ComposedAlpha::Gradient(g), _) => to_byte(g.eval(u, v)),
                _ => 255,
            };
            out.extend_from_slice(&[colour[0], colour[1], colour[2], a]);
        }
    }
    let (data, mips) = with_mips(w, h, out);
    Some(Picture {
        width,
        height,
        mips,
        data,
        srgb: matches!(&spec.rgb, ComposedRgb::Texture(t) if t.srgb),
    })
}

#[cfg(test)]
mod tests {
    use asamu_assets::TextureBinding;
    use asamu_assets::manifest::AddressMode;
    use asamu_assets::materials::{Gradient, GradientFactor};

    use super::*;

    /// A DDS header as the importer writes it (2D, no DX10 extension).
    fn header(w: u32, h: u32, mips: u32, pf: [u32; 7]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut put = |v: u32| out.extend_from_slice(&v.to_le_bytes());
        put(u32::from_le_bytes(*b"DDS "));
        put(124);
        put(0x1007);
        put(h);
        put(w);
        put(0);
        put(0);
        put(mips);
        for _ in 0..11 {
            put(0);
        }
        put(32);
        for v in pf {
            put(v);
        }
        for _ in 0..5 {
            put(0);
        }
        assert_eq!(out.len(), 128);
        out
    }

    fn fourcc(name: &[u8; 4]) -> [u32; 7] {
        [0x4, u32::from_le_bytes(*name), 0, 0, 0, 0, 0]
    }

    const RGBA32: [u32; 7] = [
        0x41,
        0,
        32,
        0x00FF_0000,
        0x0000_FF00,
        0x0000_00FF,
        0xFF00_0000,
    ];

    fn binding(srgb: bool) -> TextureBinding {
        TextureBinding {
            texture: "Pkg.T".to_owned(),
            file: "Map/Pkg/T.dds".to_owned(),
            srgb,
            address: [AddressMode::Wrap; 2],
            size: [4, 4],
        }
    }

    #[test]
    fn plain_formats_decode() {
        // 2 x 1, B G R A in the file.
        let mut dds = header(2, 1, 1, RGBA32);
        dds.extend_from_slice(&[10, 20, 30, 40, 50, 60, 70, 80]);
        let t = decode_dds(&dds, 64).unwrap();
        assert_eq!((t.width, t.height), (2, 1));
        assert_eq!(t.rgba, vec![30, 20, 10, 40, 70, 60, 50, 80]);
        // Without an alpha mask the texel is opaque.
        let mut pf = RGBA32;
        pf[0] = 0x40;
        let mut dds = header(1, 1, 1, pf);
        dds.extend_from_slice(&[1, 2, 3, 0]);
        assert_eq!(decode_dds(&dds, 64).unwrap().rgba, vec![3, 2, 1, 255]);
        // 8-bit grey.
        let mut dds = header(2, 2, 1, [0x2_0000, 0, 8, 0xFF, 0, 0, 0]);
        dds.extend_from_slice(&[0, 64, 128, 255]);
        let t = decode_dds(&dds, 64).unwrap();
        assert_eq!(&t.rgba[4..8], &[64, 64, 64, 255]);
        assert_eq!(&t.rgba[12..16], &[255, 255, 255, 255]);
    }

    #[test]
    fn block_formats_decode() {
        // DXT1, four-colour mode: c0 = white, c1 = black, the 16 indices
        // 0, 1, 2, 3 repeating.
        let mut dds = header(4, 4, 1, fourcc(b"DXT1"));
        dds.extend_from_slice(&[0xFF, 0xFF, 0x00, 0x00, 0xE4, 0xE4, 0xE4, 0xE4]);
        let t = decode_dds(&dds, 64).unwrap();
        assert_eq!(&t.rgba[0..4], &[255, 255, 255, 255]);
        assert_eq!(&t.rgba[4..8], &[0, 0, 0, 255]);
        assert_eq!(&t.rgba[8..12], &[170, 170, 170, 255]);
        assert_eq!(&t.rgba[12..16], &[85, 85, 85, 255]);
        // DXT1, three-colour mode (c0 <= c1): index 3 is transparent.
        let mut dds = header(4, 4, 1, fourcc(b"DXT1"));
        dds.extend_from_slice(&[0x00, 0x00, 0xFF, 0xFF, 0xE4, 0xE4, 0xE4, 0xE4]);
        let t = decode_dds(&dds, 64).unwrap();
        assert_eq!(&t.rgba[8..12], &[128, 128, 128, 255]);
        assert_eq!(&t.rgba[12..16], &[0, 0, 0, 0]);
        // DXT5: alpha endpoints 255 and 0 (eight-value mode), the first
        // texel index 1 (= 0), the second index 0 (= 255), then index 2.
        let mut dds = header(4, 4, 1, fourcc(b"DXT5"));
        dds.extend_from_slice(&[255, 0, 0b1000_0001, 0, 0, 0, 0, 0]);
        dds.extend_from_slice(&[0xFF, 0xFF, 0x00, 0x00, 0, 0, 0, 0]);
        let t = decode_dds(&dds, 64).unwrap();
        assert_eq!(t.rgba[3], 0);
        assert_eq!(t.rgba[7], 255);
        assert_eq!(t.rgba[11], 219, "(6 * 255 + 0 + 3) / 7");
        assert_eq!(&t.rgba[0..3], &[255, 255, 255]);
        // DXT5 with a0 <= a1: indices 6 and 7 are 0 and 255.
        let mut dds = header(4, 4, 1, fourcc(b"DXT5"));
        dds.extend_from_slice(&[0, 100, 0b0011_1110, 0, 0, 0, 0, 0]);
        dds.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        let t = decode_dds(&dds, 64).unwrap();
        assert_eq!((t.rgba[3], t.rgba[7]), (0, 255));
        // DXT3: 4-bit alpha, expanded by 17.
        let mut dds = header(4, 4, 1, fourcc(b"DXT3"));
        dds.extend_from_slice(&[0xF0, 0x08, 0, 0, 0, 0, 0, 0]);
        dds.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0]);
        let t = decode_dds(&dds, 64).unwrap();
        assert_eq!((t.rgba[3], t.rgba[7], t.rgba[11]), (0, 255, 136));
        // A 5 x 3 texture uses 2 x 1 blocks and keeps only its own texels.
        let mut dds = header(5, 3, 1, fourcc(b"DXT1"));
        dds.extend_from_slice(&[0xFF, 0xFF, 0x00, 0x00, 0, 0, 0, 0].repeat(2));
        let t = decode_dds(&dds, 64).unwrap();
        assert_eq!(t.rgba.len(), 5 * 3 * 4);
        assert!(t.rgba.iter().all(|b| *b == 255));
    }

    #[test]
    fn a_mip_that_fits_is_chosen() {
        let mut dds = header(8, 4, 4, [0x2_0000, 0, 8, 0xFF, 0, 0, 0]);
        dds.extend(std::iter::repeat_n(1u8, 32));
        dds.extend(std::iter::repeat_n(2u8, 8));
        dds.extend(std::iter::repeat_n(3u8, 2));
        dds.push(4);
        let t = decode_dds(&dds, 4).unwrap();
        assert_eq!((t.width, t.height, t.rgba[0]), (4, 2, 2));
        let t = decode_dds(&dds, 64).unwrap();
        assert_eq!((t.width, t.rgba[0]), (8, 1));
        // Nothing fits: the smallest mip the file has.
        let t = decode_dds(&dds, 0).unwrap();
        assert_eq!((t.width, t.height, t.rgba[0]), (1, 1, 4));
    }

    #[test]
    fn hostile_files_are_refused() {
        let mut good = header(4, 4, 1, fourcc(b"DXT5"));
        good.extend_from_slice(&[0u8; 16]);
        assert!(decode_dds(&good, 64).is_some());
        // Every truncation.
        for cut in 0..good.len() {
            assert!(decode_dds(&good[..cut], 64).is_none(), "cut at {cut}");
        }
        for (what, edit) in [
            ("magic", (0usize, 0u32)),
            ("header size", (4, 100)),
            ("zero height", (12, 0)),
            ("zero width", (16, 0)),
            ("huge width", (16, u32::MAX)),
            ("huge height", (12, 1 << 20)),
            ("unknown FourCC", (84, u32::from_le_bytes(*b"ATI2"))),
            ("DX10 extension", (84, u32::from_le_bytes(*b"DX10"))),
            ("cube map", (112, 0x200)),
            ("no known format flag", (80, 0)),
        ] {
            let mut bad = good.clone();
            bad[edit.0..edit.0 + 4].copy_from_slice(&edit.1.to_le_bytes());
            assert!(decode_dds(&bad, 64).is_none(), "{what}");
        }
        // More mips declared than stored: asking for a small one fails,
        // the stored one still reads.
        let mut bad = good.clone();
        bad[28..32].copy_from_slice(&9u32.to_le_bytes());
        assert!(decode_dds(&bad, 1).is_none());
        assert!(decode_dds(&bad, 64).is_some());
        // Noise never panics.
        let mut state = 0x2545_F491u32;
        for _ in 0..2000 {
            let mut noise = good.clone();
            for _ in 0..6 {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let at = state as usize % noise.len();
                noise[at] = (state >> 8) as u8;
            }
            let _ = decode_dds(&noise, 64);
        }
        assert!(decode_dds(&[], 64).is_none());
    }

    fn texels(width: u32, height: u32, texel: impl Fn(u32, u32) -> [u8; 4]) -> Texels {
        let mut rgba = Vec::new();
        for y in 0..height {
            for x in 0..width {
                rgba.extend_from_slice(&texel(x, y));
            }
        }
        Texels {
            width,
            height,
            rgba,
        }
    }

    #[test]
    fn a_mask_channel_becomes_the_alpha_of_a_white_picture() {
        // The shape is in the red channel of a linear mask.
        let mask = texels(2, 2, |x, _| {
            if x == 0 {
                [255, 0, 0, 255]
            } else {
                [51, 0, 0, 255]
            }
        });
        let spec = ComposedTexture {
            rgb: ComposedRgb::White,
            alpha: ComposedAlpha::Texture {
                texture: binding(false),
                channel: 0,
                scale: 1.0,
                bias: 0.0,
            },
            emissive: false,
        };
        let p = compose(&spec, None, Some(&mask), false).unwrap();
        assert_eq!((p.width, p.height, p.mips, p.srgb), (2, 2, 2, false));
        assert_eq!(&p.data[0..8], &[255, 255, 255, 255, 255, 255, 255, 51]);
        // The second mip is the average: (255 + 51 + 255 + 51) / 4.
        assert_eq!(&p.data[16..20], &[255, 255, 255, 153]);
        assert_eq!(p.data.len(), 20);
        // A colour channel of an sRGB texture is linearised, as the GPU
        // samples it; alpha never is.
        let p = compose(&spec, None, Some(&mask), true).unwrap();
        assert_eq!(p.data[3], 255);
        assert_eq!(p.data[7], 8, "sRGB 51 is 0.0331 linear");
        let alpha_spec = ComposedTexture {
            alpha: ComposedAlpha::Texture {
                texture: binding(true),
                channel: 3,
                scale: 0.5,
                bias: 0.25,
            },
            ..spec.clone()
        };
        let p = compose(&alpha_spec, None, Some(&mask), true).unwrap();
        assert_eq!(p.data[3], 191, "1.0 * 0.5 + 0.25");
        // Without the texture there is no picture.
        assert!(compose(&spec, None, None, false).is_none());
        let empty = Texels {
            width: 2,
            height: 2,
            rgba: vec![0; 3],
        };
        assert!(compose(&spec, None, Some(&empty), false).is_none());
    }

    #[test]
    fn colour_and_mask_of_different_sizes_compose() {
        let colour = texels(4, 4, |x, y| [(x * 60) as u8, (y * 60) as u8, 9, 255]);
        let mask = texels(2, 2, |x, y| [0, if x == y { 255 } else { 0 }, 0, 255]);
        let spec = ComposedTexture {
            rgb: ComposedRgb::Texture(binding(true)),
            alpha: ComposedAlpha::Texture {
                texture: binding(false),
                channel: 1,
                scale: 1.0,
                bias: 0.0,
            },
            emissive: false,
        };
        let p = compose(&spec, Some(&colour), Some(&mask), false).unwrap();
        assert_eq!((p.width, p.height, p.mips, p.srgb), (4, 4, 3, true));
        // Colour texels are copied as stored; the mask is filtered up.
        assert_eq!(&p.data[0..3], &[0, 0, 9]);
        assert_eq!(&p.data[4 * 5..4 * 5 + 3], &[60, 60, 9]);
        assert_eq!(p.data[3], 255, "top-left corner of the mask");
        assert_eq!(p.data[4 * 3 + 3], 0, "top-right corner");
        let middle = p.data[4 * 5 + 3];
        assert!(middle > 100 && middle < 220, "{middle}");
    }

    #[test]
    fn a_gradient_fills_colour_and_alpha() {
        let fade = Gradient {
            factors: vec![GradientFactor {
                v: true,
                scale: 1.0,
                offset: 0.0,
                power: 5.0,
            }],
        };
        let spec = ComposedTexture {
            rgb: ComposedRgb::Gradient(fade.clone()),
            alpha: ComposedAlpha::Gradient(fade),
            emissive: true,
        };
        let p = compose(&spec, None, None, false).unwrap();
        assert_eq!(
            (p.width, p.height, p.srgb),
            (GRADIENT_EDGE, GRADIENT_EDGE, false)
        );
        assert_eq!(p.mips, 9);
        let row = |y: usize| &p.data[y * GRADIENT_EDGE as usize * 4..][..4];
        assert_eq!(row(0), &[0, 0, 0, 0]);
        let last = row(GRADIENT_EDGE as usize - 1);
        assert!(last[0] > 245 && last[0] == last[3], "{last:?}");
        // v = 0.5 (between rows 127 and 128): 0.5^5 = 0.031.
        assert!((i32::from(row(128)[3]) - 8).abs() <= 1);
        // The whole chain: 256^2 * 4 * (1 + 1/4 + ...) down to one texel.
        let total: usize = (0..9).map(|l| (256usize >> l).pow(2) * 4).sum();
        assert_eq!(p.data.len(), total);
    }
}
