//! Parts of the material approximation that describe what a reduced channel
//! is *not*: the inputs it lost ([`Lost`]), a function of the texture
//! coordinates that stands in for a texture ([`UvGradient`]), and what a
//! texture holds on average ([`TextureStats`]), which the reduction uses to
//! choose between two textures and to replace the one it drops by its mean.
//!
//! All of it is ours: statistics and classifications of the user's own data,
//! computed the same way on every system (integer sums and the portable
//! power), never a recovered constant of the original.

use serde::Serialize;

use crate::portable_math::powf;

// ---------------------------------------------------------------------------
// What a channel lost
// ---------------------------------------------------------------------------

/// Inputs of the original graph that a reduced channel does not have
/// (a set). A renderer uses it to tell a value it can trust from one that
/// only stands in for something else: an opacity that lost a depth term is
/// not the original's opacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Lost(u16);

impl Lost {
    /// Nothing.
    pub const NONE: Lost = Lost(0);
    /// Scene or pixel depth (soft intersections, depth fades, depth-edge
    /// outlines, distance blends).
    pub const DEPTH: Lost = Lost(1);
    /// The view direction (Fresnel, camera and reflection vectors, parallax).
    pub const VIEW: Lost = Lost(1 << 1);
    /// Texture coordinates used as a value that [`UvGradient`] cannot hold.
    pub const COORDINATES: Lost = Lost(1 << 2);
    /// Time used as a value (pulsing, flicker).
    pub const TIME: Lost = Lost(1 << 3);
    /// Vertex colour.
    pub const VERTEX_COLOR: Lost = Lost(1 << 4);
    /// Scene colour behind the surface (refraction, destination colour).
    pub const SCENE: Lost = Lost(1 << 5);
    /// World-space position, normal or object placement (up-facing blends,
    /// world-aligned effects).
    pub const WORLD: Lost = Lost(1 << 6);
    /// A second texture combined with the bound one.
    pub const TEXTURE: Lost = Lost(1 << 7);
    /// Any other expression the reduction does not model.
    pub const OTHER: Lost = Lost(1 << 8);

    /// Every member with its name in `materials.json`, in output order.
    pub const NAMES: [(Lost, &'static str); 9] = [
        (Lost::DEPTH, "depth"),
        (Lost::VIEW, "view"),
        (Lost::COORDINATES, "coordinates"),
        (Lost::TIME, "time"),
        (Lost::VERTEX_COLOR, "vertex_color"),
        (Lost::SCENE, "scene"),
        (Lost::WORLD, "world"),
        (Lost::TEXTURE, "texture"),
        (Lost::OTHER, "other"),
    ];

    /// True when nothing was lost.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// True when every member of `other` is in `self`.
    #[must_use]
    pub fn contains(self, other: Lost) -> bool {
        self.0 & other.0 == other.0
    }

    /// The union.
    #[must_use]
    pub fn with(self, other: Lost) -> Lost {
        Lost(self.0 | other.0)
    }

    /// Adds `other`.
    pub fn insert(&mut self, other: Lost) {
        self.0 |= other.0;
    }

    /// The names of the members, in [`Lost::NAMES`] order.
    #[must_use]
    pub fn names(self) -> Vec<&'static str> {
        Lost::NAMES
            .iter()
            .filter(|(l, _)| self.contains(*l))
            .map(|(_, n)| *n)
            .collect()
    }

    /// The class of an unsupported expression kind (the `MaterialExpression`
    /// class name without its prefix, or one of the reduction's own labels).
    #[must_use]
    pub fn of_kind(kind: &str) -> Lost {
        let k = kind.to_ascii_lowercase();
        let has = |keys: &[&str]| keys.iter().any(|x| k.contains(x));
        if has(&["depth"]) {
            Lost::DEPTH
        } else if has(&["time"]) {
            Lost::TIME
        } else if has(&["coordinate"]) {
            Lost::COORDINATES
        } else if has(&[
            "fresnel",
            "cameravector",
            "reflectionvector",
            "lightvector",
            "cameraworldposition",
        ]) {
            Lost::VIEW
        } else if has(&["scenetexture", "destcolor", "screenposition", "scenecolor"]) {
            Lost::SCENE
        } else if has(&[
            "worldposition",
            "worldnormal",
            "objectradius",
            "objectorientation",
            "transform",
            "dotproduct",
            "crossproduct",
            "normalize",
            "windowdirection",
            "foliage",
            "lensflare",
        ]) {
            Lost::WORLD
        } else if has(&["vertexcolor"]) {
            Lost::VERTEX_COLOR
        } else {
            Lost::OTHER
        }
    }

    /// True for an unsupported kind that is a unit direction (its components
    /// are centred on 0): a constant times one of its components is still
    /// such a component, and in a sum it stands for 0.
    #[must_use]
    pub fn is_direction(kind: &str) -> bool {
        let k = kind.to_ascii_lowercase();
        ["reflectionvector", "cameravector", "lightvector"]
            .iter()
            .any(|x| k.contains(x))
    }

    /// True for an unsupported kind whose value is a length or a position in
    /// world units rather than a factor in 0..1: constants combined with it
    /// (its own scale and offset) belong to it and say nothing without it.
    #[must_use]
    pub fn is_dimensional(kind: &str) -> bool {
        let k = kind.to_ascii_lowercase();
        [
            "pixeldepth",
            "scenedepth",
            "destdepth",
            "worldposition",
            "cameraworldposition",
            "objectradius",
        ]
        .iter()
        .any(|x| k.contains(x))
    }
}

// ---------------------------------------------------------------------------
// Functions of the texture coordinates
// ---------------------------------------------------------------------------

/// Most factors a [`UvGradient`] holds.
pub const MAX_GRADIENT_FACTORS: usize = 4;

/// One factor of a [`UvGradient`]: `max(scale * uv[component] + offset, 0) ^
/// power`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct GradientFactor {
    /// UV channel (`TexCoord` index).
    pub uv_channel: i32,
    /// `"u"` or `"v"`.
    pub component: &'static str,
    /// Multiplier of the coordinate.
    pub scale: f32,
    /// Added after the multiplier.
    pub offset: f32,
    /// Exponent.
    pub power: f32,
}

/// A channel computed from the texture coordinates alone: the channel's
/// `value` times the product of the factors, the product clamped to 0..1
/// (a fade along one axis of a mesh's own coordinates).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UvGradient {
    /// The factors (at most [`MAX_GRADIENT_FACTORS`]).
    pub factors: Vec<GradientFactor>,
}

impl UvGradient {
    /// The product of the factors at `uv` (per channel: `uv(channel)`
    /// returns that channel's coordinates), clamped to 0..1. Not finite
    /// input gives 0.
    #[must_use]
    pub fn eval(&self, uv: impl Fn(i32) -> [f32; 2]) -> f32 {
        let mut out = 1.0f32;
        for f in &self.factors {
            let c = uv(f.uv_channel);
            let x = if f.component == "v" { c[1] } else { c[0] };
            let base = (f.scale * x + f.offset).max(0.0);
            let v = if f.power == 1.0 {
                base
            } else if f.power == 0.0 {
                1.0
            } else {
                powf(base, f.power)
            };
            out *= v;
        }
        if out.is_finite() {
            out.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

// ---------------------------------------------------------------------------
// Texture statistics
// ---------------------------------------------------------------------------

/// Longest edge of the mip the statistics are taken from, texels.
pub const STATS_MAX_EDGE: u32 = 64;

/// Mean saturation (`max - min` of R, G, B, 0..1) below which a texture
/// counts as grey: a shading or weight map rather than a colour map. Ours
/// (a classification threshold on our own statistic, chosen in the gap the
/// shipped textures leave: occlusion and cavity maps measure up to 0.021,
/// coloured surface textures from 0.035).
pub const GREY_CHROMA: f32 = 0.028;

/// What a texture holds on average (over one small mip).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct TextureStats {
    /// Mean colour: R, G, B linear (an sRGB texture's texels are linearised
    /// before averaging), A as stored.
    pub mean: [f32; 4],
    /// Mean of `max(R, G, B) - min(R, G, B)` over the stored texel values,
    /// 0..1.
    pub chroma: f32,
    /// Every channel is the same everywhere (within 2 of 255): the texture
    /// is a constant colour.
    pub uniform: bool,
}

impl TextureStats {
    /// True when the texture carries no colour ([`GREY_CHROMA`]).
    #[must_use]
    pub fn grey(&self) -> bool {
        self.chroma < GREY_CHROMA
    }

    /// The statistics of tightly packed RGBA8 texels (`srgb`: the colour
    /// channels are sRGB-encoded). `None` for no texels.
    #[must_use]
    pub fn of_rgba8(rgba: &[u8], srgb: bool) -> Option<TextureStats> {
        let (texels, _) = rgba.as_chunks::<4>();
        if texels.is_empty() {
            return None;
        }
        // Colour values through a table: 256 portable powers, then sums.
        let mut table = [0f32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let c = i as f32 / 255.0;
            *slot = if !srgb {
                c
            } else if c <= 0.04045 {
                c / 12.92
            } else {
                powf((c + 0.055) / 1.055, 2.4)
            };
        }
        let mut sum = [0f64; 4];
        let mut chroma = 0u64;
        let mut lo = [u8::MAX; 4];
        let mut hi = [u8::MIN; 4];
        for px in texels {
            for (s, v) in sum.iter_mut().zip(px.iter()).take(3) {
                *s += f64::from(table[usize::from(*v)]);
            }
            sum[3] += f64::from(px[3]) / 255.0;
            let max = px[0].max(px[1]).max(px[2]);
            let min = px[0].min(px[1]).min(px[2]);
            chroma += u64::from(max - min);
            for ((l, h), v) in lo.iter_mut().zip(hi.iter_mut()).zip(px.iter()) {
                *l = (*l).min(*v);
                *h = (*h).max(*v);
            }
        }
        let n = texels.len() as f64;
        let mean = sum.map(|s| (s / n) as f32);
        Some(TextureStats {
            mean,
            chroma: (chroma as f64 / (n * 255.0)) as f32,
            uniform: lo.iter().zip(hi.iter()).all(|(l, h)| h - l <= 2),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lost_names_and_classes() {
        let mut l = Lost::NONE;
        assert!(l.is_empty() && l.names().is_empty());
        l.insert(Lost::VIEW);
        l.insert(Lost::DEPTH);
        assert_eq!(l.names(), vec!["depth", "view"]);
        assert!(l.contains(Lost::DEPTH) && !l.contains(Lost::TIME));
        assert_eq!(Lost::DEPTH.with(Lost::TIME).names(), vec!["depth", "time"]);
        for (kind, class) in [
            ("PixelDepth", Lost::DEPTH),
            ("SceneDepth", Lost::DEPTH),
            ("DepthBiasedAlpha-unconnected", Lost::DEPTH),
            ("Fresnel", Lost::VIEW),
            ("ReflectionVector", Lost::VIEW),
            ("CameraVector", Lost::VIEW),
            ("SceneTexture", Lost::SCENE),
            ("DestColor", Lost::SCENE),
            ("WorldPosition", Lost::WORLD),
            ("Transform", Lost::WORLD),
            ("DotProduct", Lost::WORLD),
            ("Sine(Time)", Lost::TIME),
            ("coordinate-math", Lost::COORDINATES),
            ("Custom", Lost::OTHER),
            ("", Lost::OTHER),
        ] {
            assert_eq!(Lost::of_kind(kind), class, "{kind}");
        }
        assert!(Lost::is_dimensional("PixelDepth"));
        assert!(Lost::is_dimensional("scenedepth"));
        assert!(!Lost::is_dimensional("Fresnel"));
        assert!(!Lost::is_dimensional("DepthBiasedAlpha"));
        assert!(Lost::is_direction("ReflectionVector") && Lost::is_direction("cameravector"));
        assert!(!Lost::is_direction("Fresnel") && !Lost::is_direction("PixelDepth"));
        // Every member has exactly one name.
        let all = Lost::NAMES.iter().fold(Lost::NONE, |a, (l, _)| a.with(*l));
        assert_eq!(all.names().len(), Lost::NAMES.len());
    }

    #[test]
    fn gradient_is_a_clamped_product_of_powers() {
        let g = UvGradient {
            factors: vec![GradientFactor {
                uv_channel: 0,
                component: "v",
                scale: 1.0,
                offset: 0.0,
                power: 5.0,
            }],
        };
        let at = |v: f32| g.eval(|_| [0.3, v]);
        assert_eq!(at(0.0), 0.0);
        assert_eq!(at(1.0), 1.0);
        assert!((at(0.5) - 0.03125).abs() < 1e-6);
        assert_eq!(at(-2.0), 0.0, "a negative base is 0");
        assert_eq!(at(3.0), 1.0, "clamped");
        assert_eq!(at(f32::NAN), 0.0);
        // One minus u, times v, in another channel.
        let g = UvGradient {
            factors: vec![
                GradientFactor {
                    uv_channel: 1,
                    component: "u",
                    scale: -1.0,
                    offset: 1.0,
                    power: 1.0,
                },
                GradientFactor {
                    uv_channel: 0,
                    component: "v",
                    scale: 2.0,
                    offset: 0.0,
                    power: 1.0,
                },
            ],
        };
        let v = g.eval(|c| if c == 1 { [0.25, 9.0] } else { [9.0, 0.25] });
        assert!((v - 0.375).abs() < 1e-6);
        assert_eq!(UvGradient { factors: vec![] }.eval(|_| [0.0, 0.0]), 1.0);
    }

    #[test]
    fn texture_statistics() {
        assert!(TextureStats::of_rgba8(&[], true).is_none());
        assert!(TextureStats::of_rgba8(&[1, 2, 3], true).is_none());
        // A constant white texture.
        let white = [255u8; 16];
        let s = TextureStats::of_rgba8(&white, true).unwrap();
        assert!(s.uniform && s.grey());
        assert_eq!(s.mean, [1.0, 1.0, 1.0, 1.0]);
        // Grey shading: no chroma, not uniform; sRGB 128 is about 0.216 linear.
        let grey = [128, 128, 128, 255, 64, 64, 64, 0];
        let s = TextureStats::of_rgba8(&grey, true).unwrap();
        assert!(!s.uniform && s.grey());
        assert_eq!(s.chroma, 0.0);
        assert!((s.mean[3] - 0.5).abs() < 1e-6);
        let expect = (0.215_860_5 + 0.051_269_46) / 2.0;
        assert!((s.mean[0] - expect).abs() < 1e-4, "{}", s.mean[0]);
        // The same values of a linear texture are averaged as stored.
        let s = TextureStats::of_rgba8(&grey, false).unwrap();
        assert!((s.mean[0] - (128.0 + 64.0) / 510.0).abs() < 1e-6);
        // A coloured texture.
        let colour = [200, 100, 50, 255, 190, 110, 60, 255];
        let s = TextureStats::of_rgba8(&colour, true).unwrap();
        assert!(!s.grey());
        assert!((s.chroma - (150.0 + 130.0) / 510.0).abs() < 1e-6);
        // Within 2 of 255 everywhere still counts as one colour (block
        // compression of a flat texture).
        let nearly = [10, 10, 10, 255, 12, 11, 10, 255];
        assert!(TextureStats::of_rgba8(&nearly, false).unwrap().uniform);
        let not = [10, 10, 10, 255, 13, 10, 10, 255];
        assert!(!TextureStats::of_rgba8(&not, false).unwrap().uniform);
    }
}
