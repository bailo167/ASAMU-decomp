//! Texture native data (the bytes after the tagged properties) for package
//! version 868: `Texture2D` and its cooked subclasses, mip chains, bulk data
//! records, pixel formats, texture file cache references, and a block
//! decoder for previews.
//!
//! Layout (CONFIRMED by exact consumption of every texture export of the
//! macOS build; see `docs/reverse-engineering/TEXTURES.md`):
//!
//! ```text
//! UTexture      tagged properties
//!               FBulkData SourceArt                      (always empty when cooked)
//! UTexture2D    TArray<FTexture2DMipMap> Mips
//!                   FBulkData Data | i32 SizeX | i32 SizeY
//!               FGuid TextureFileCacheGuid
//!               TArray<FTexture2DMipMap> CachedPVRTCMips (always empty)
//!               i32 CachedFlashMipsMaxResolution         (non-zero in 621 textures, no data behind it)
//!               TArray<FTexture2DMipMap> CachedATITCMips (always empty)
//!               FBulkData CachedFlashMips                (always unused)
//!               TArray<FTexture2DMipMap> CachedETCMips   (always empty)
//! ULightMapTexture2D  UTexture2D | u32 LightmapFlags
//! ```
//!
//! `ShadowMapTexture2D` and `TextureFlipBook` use the plain `Texture2D`
//! layout; `TextureCube` and `TextureRenderTarget2D` store only `SourceArt`
//! (cube faces are separate `Texture2D` objects referenced by `FacePosX` ...
//! `FaceNegZ`). Class default objects have no native data.
//!
//! Each mip's bulk data holds `ElementCount` bytes of texels in the format
//! named by the `Format` property; see [`PixelFormat`] and
//! [`crate::bulkdata`].

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use serde::Serialize;
use thiserror::Error;

use crate::bulkdata::{
    self, BULK_RECORD_SIZE, BulkCompression, BulkDataRecord, BulkError, BulkStorage,
    TextureFileCaches,
};
use crate::error::Ue3Error;
use crate::flags;
use crate::model::{LoadedPackage, PackageSet};
use crate::object::ObjectError;
use crate::property::{Property, Value};
use crate::reader::Reader;
use crate::types::Guid;

/// Most mips accepted in one mip array (sanity bound; real textures have at
/// most 13).
pub const MAX_MIPS: usize = 32;

/// Smallest serialized mip entry: bulk record header plus `SizeX`/`SizeY`.
pub const MIP_ENTRY_MIN_SIZE: usize = BULK_RECORD_SIZE + 8;

/// Largest width or height accepted when decoding texels.
pub const MAX_DIMENSION: u32 = 16_384;

/// Errors from texture decoding.
#[derive(Debug, Error)]
pub enum TextureError {
    /// A low-level read failed.
    #[error(transparent)]
    Ue3(#[from] Ue3Error),
    /// Prelude or tagged properties failed to decode.
    #[error(transparent)]
    Object(#[from] ObjectError),
    /// A bulk data record is malformed or cannot be loaded.
    #[error(transparent)]
    Bulk(#[from] BulkError),
    /// The export is not a texture with a known native layout.
    #[error("export {export} ({class}) is not a decodable texture")]
    NotATexture {
        /// Export index.
        export: usize,
        /// Class path.
        class: String,
    },
    /// The native data is inconsistent.
    #[error("texture native data: {0}")]
    Malformed(String),
    /// The pixel format has no known texel layout.
    #[error("pixel format {0} has no known texel layout")]
    UnsupportedFormat(String),
}

// ---------------------------------------------------------------------------
// Pixel formats
// ---------------------------------------------------------------------------

/// `EPixelFormat` of this engine build, in enum order (the order is read from
/// `Engine.Texture.EPixelFormat` in `Engine.u`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum PixelFormat {
    /// `PF_Unknown`.
    Unknown,
    /// `PF_A32B32G32R32F`.
    A32B32G32R32F,
    /// `PF_A8R8G8B8`: 4 bytes per texel, stored B, G, R, A.
    A8R8G8B8,
    /// `PF_G8`: one byte per texel.
    G8,
    /// `PF_G16`.
    G16,
    /// `PF_DXT1` (BC1).
    Dxt1,
    /// `PF_DXT3` (BC2).
    Dxt3,
    /// `PF_DXT5` (BC3).
    Dxt5,
    /// `PF_UYVY`.
    Uyvy,
    /// `PF_FloatRGB`.
    FloatRgb,
    /// `PF_FloatRGBA`.
    FloatRgba,
    /// `PF_DepthStencil`.
    DepthStencil,
    /// `PF_ShadowDepth`.
    ShadowDepth,
    /// `PF_FilteredShadowDepth`.
    FilteredShadowDepth,
    /// `PF_R32F`.
    R32F,
    /// `PF_G16R16`.
    G16R16,
    /// `PF_G16R16F`.
    G16R16F,
    /// `PF_G16R16F_FILTER`.
    G16R16FFilter,
    /// `PF_G32R32F`.
    G32R32F,
    /// `PF_A2B10G10R10`.
    A2B10G10R10,
    /// `PF_A16B16G16R16`.
    A16B16G16R16,
    /// `PF_D24`.
    D24,
    /// `PF_R16F`.
    R16F,
    /// `PF_R16F_FILTER`.
    R16FFilter,
    /// `PF_BC5` (ATI2 / 3Dc).
    Bc5,
    /// `PF_V8U8`: two signed bytes per texel.
    V8U8,
    /// `PF_A1`.
    A1,
    /// `PF_FloatR11G11B10`.
    FloatR11G11B10,
    /// `PF_A4R4G4B4`.
    A4R4G4B4,
    /// `PF_R5G6B5`.
    R5G6B5,
}

/// Every format with its enum name, in enum order.
pub const PIXEL_FORMATS: &[(PixelFormat, &str)] = &[
    (PixelFormat::Unknown, "PF_Unknown"),
    (PixelFormat::A32B32G32R32F, "PF_A32B32G32R32F"),
    (PixelFormat::A8R8G8B8, "PF_A8R8G8B8"),
    (PixelFormat::G8, "PF_G8"),
    (PixelFormat::G16, "PF_G16"),
    (PixelFormat::Dxt1, "PF_DXT1"),
    (PixelFormat::Dxt3, "PF_DXT3"),
    (PixelFormat::Dxt5, "PF_DXT5"),
    (PixelFormat::Uyvy, "PF_UYVY"),
    (PixelFormat::FloatRgb, "PF_FloatRGB"),
    (PixelFormat::FloatRgba, "PF_FloatRGBA"),
    (PixelFormat::DepthStencil, "PF_DepthStencil"),
    (PixelFormat::ShadowDepth, "PF_ShadowDepth"),
    (PixelFormat::FilteredShadowDepth, "PF_FilteredShadowDepth"),
    (PixelFormat::R32F, "PF_R32F"),
    (PixelFormat::G16R16, "PF_G16R16"),
    (PixelFormat::G16R16F, "PF_G16R16F"),
    (PixelFormat::G16R16FFilter, "PF_G16R16F_FILTER"),
    (PixelFormat::G32R32F, "PF_G32R32F"),
    (PixelFormat::A2B10G10R10, "PF_A2B10G10R10"),
    (PixelFormat::A16B16G16R16, "PF_A16B16G16R16"),
    (PixelFormat::D24, "PF_D24"),
    (PixelFormat::R16F, "PF_R16F"),
    (PixelFormat::R16FFilter, "PF_R16F_FILTER"),
    (PixelFormat::Bc5, "PF_BC5"),
    (PixelFormat::V8U8, "PF_V8U8"),
    (PixelFormat::A1, "PF_A1"),
    (PixelFormat::FloatR11G11B10, "PF_FloatR11G11B10"),
    (PixelFormat::A4R4G4B4, "PF_A4R4G4B4"),
    (PixelFormat::R5G6B5, "PF_R5G6B5"),
];

/// Texel block layout: `block_bytes` bytes per `block_w` x `block_h` texels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BlockLayout {
    /// Block width in texels.
    pub block_w: u32,
    /// Block height in texels.
    pub block_h: u32,
    /// Bytes per block.
    pub block_bytes: u32,
}

impl PixelFormat {
    /// Format for an `EPixelFormat` enumerator name (e.g. `PF_DXT5`).
    pub fn from_enum_name(name: &str) -> Option<PixelFormat> {
        PIXEL_FORMATS
            .iter()
            .find(|(_, n)| n.eq_ignore_ascii_case(name))
            .map(|(f, _)| *f)
    }

    /// The `EPixelFormat` enumerator name.
    pub fn enum_name(self) -> &'static str {
        PIXEL_FORMATS
            .iter()
            .find(|(f, _)| *f == self)
            .map_or("PF_Unknown", |(_, n)| n)
    }

    /// Texel layout of the format as stored in mip bulk data, for the formats
    /// whose layout is fixed by the format itself (the block-compressed
    /// formats and the plain per-texel formats). `None` for `PF_Unknown` and
    /// the depth, shadow, 1-bit and `FloatRGB` formats, whose storage size is
    /// platform-dependent and which no shipped texture uses.
    pub fn layout(self) -> Option<BlockLayout> {
        let (block_w, block_h, block_bytes) = match self {
            PixelFormat::Dxt1 => (4, 4, 8),
            PixelFormat::Dxt3 | PixelFormat::Dxt5 | PixelFormat::Bc5 => (4, 4, 16),
            PixelFormat::Uyvy => (2, 1, 4),
            PixelFormat::A32B32G32R32F => (1, 1, 16),
            PixelFormat::FloatRgba | PixelFormat::G32R32F | PixelFormat::A16B16G16R16 => (1, 1, 8),
            PixelFormat::A8R8G8B8
            | PixelFormat::R32F
            | PixelFormat::G16R16
            | PixelFormat::G16R16F
            | PixelFormat::G16R16FFilter
            | PixelFormat::A2B10G10R10
            | PixelFormat::FloatR11G11B10 => (1, 1, 4),
            PixelFormat::G16
            | PixelFormat::R16F
            | PixelFormat::R16FFilter
            | PixelFormat::V8U8
            | PixelFormat::A4R4G4B4
            | PixelFormat::R5G6B5 => (1, 1, 2),
            PixelFormat::G8 => (1, 1, 1),
            PixelFormat::Unknown
            | PixelFormat::FloatRgb
            | PixelFormat::DepthStencil
            | PixelFormat::ShadowDepth
            | PixelFormat::FilteredShadowDepth
            | PixelFormat::D24
            | PixelFormat::A1 => return None,
        };
        Some(BlockLayout {
            block_w,
            block_h,
            block_bytes,
        })
    }

    /// True for the 4x4 block-compressed formats (DXT1/3/5, BC5).
    pub fn is_block_compressed(self) -> bool {
        self.layout()
            .is_some_and(|l| l.block_w == 4 && l.block_h == 4)
    }

    /// Bytes of one `w` x `h` mip (partial blocks round up), or `None` when
    /// the layout is unknown or the size overflows.
    pub fn mip_bytes(self, w: u32, h: u32) -> Option<u64> {
        let l = self.layout()?;
        let bw = u64::from(w.div_ceil(l.block_w));
        let bh = u64::from(h.div_ceil(l.block_h));
        bw.checked_mul(bh)?.checked_mul(u64::from(l.block_bytes))
    }
}

/// Natural size of mip `level` of a `w` x `h` texture (halved per level,
/// never below 1).
pub fn mip_dims(w: u32, h: u32, level: u32) -> (u32, u32) {
    let shift = |v: u32| v.checked_shr(level).unwrap_or(0).max(1);
    (shift(w), shift(h))
}

// ---------------------------------------------------------------------------
// Classes and native layouts
// ---------------------------------------------------------------------------

/// Texture classes of this build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum TextureClass {
    /// `Engine.Texture2D`.
    Texture2D,
    /// `Engine.LightMapTexture2D` (native-only class).
    LightMapTexture2D,
    /// `Engine.ShadowMapTexture2D`.
    ShadowMapTexture2D,
    /// `Engine.TextureFlipBook`.
    TextureFlipBook,
    /// `Engine.TextureCube`.
    TextureCube,
    /// `Engine.TextureMovie`.
    TextureMovie,
    /// `Engine.TextureRenderTarget2D`.
    TextureRenderTarget2D,
    /// `Engine.TextureRenderTargetCube`.
    TextureRenderTargetCube,
    /// Any other class derived from `Engine.Texture`.
    Other,
}

/// Native data layout after the tagged properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum NativeLayout {
    /// `SourceArt` + `Texture2D` mips and cached platform data.
    Texture2D,
    /// [`NativeLayout::Texture2D`] + `u32 LightmapFlags`.
    LightMapTexture2D,
    /// Only `UTexture`'s `SourceArt` record.
    SourceArtOnly,
    /// Not decoded (no shipped instance to verify a layout against).
    Undecoded,
}

const CLASS_NAMES: &[(&str, TextureClass)] = &[
    ("Texture2D", TextureClass::Texture2D),
    ("LightMapTexture2D", TextureClass::LightMapTexture2D),
    ("ShadowMapTexture2D", TextureClass::ShadowMapTexture2D),
    ("TextureFlipBook", TextureClass::TextureFlipBook),
    ("TextureCube", TextureClass::TextureCube),
    ("TextureMovie", TextureClass::TextureMovie),
    ("TextureRenderTarget2D", TextureClass::TextureRenderTarget2D),
    (
        "TextureRenderTargetCube",
        TextureClass::TextureRenderTargetCube,
    ),
];

impl TextureClass {
    /// The class for a known class name, bare (`Texture2D`) or qualified
    /// with the `Engine` package (`Engine.Texture2D`), in any case.
    pub fn from_class_name(name: &str) -> Option<TextureClass> {
        let bare = match name.split_once('.') {
            Some((package, rest)) if package.eq_ignore_ascii_case("Engine") => rest,
            Some(_) => return None,
            None => name,
        };
        CLASS_NAMES
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(bare))
            .map(|(_, c)| *c)
    }

    /// Classify a class from its path and super chain (qualified paths, the
    /// class itself first or not). Returns `None` when the class is neither
    /// `Engine.Texture` itself nor derived from it.
    pub fn classify(class_path: &str, super_chain: &[String]) -> Option<TextureClass> {
        if let Some(c) = TextureClass::from_class_name(class_path) {
            return Some(c);
        }
        let is_texture = |s: &str| s.eq_ignore_ascii_case("Engine.Texture");
        let derives = is_texture(class_path) || super_chain.iter().any(|s| is_texture(s));
        derives.then_some(TextureClass::Other)
    }

    /// Native layout of the class.
    pub fn layout(self) -> NativeLayout {
        match self {
            TextureClass::Texture2D
            | TextureClass::ShadowMapTexture2D
            | TextureClass::TextureFlipBook => NativeLayout::Texture2D,
            TextureClass::LightMapTexture2D => NativeLayout::LightMapTexture2D,
            TextureClass::TextureCube
            | TextureClass::TextureRenderTarget2D
            | TextureClass::TextureRenderTargetCube => NativeLayout::SourceArtOnly,
            TextureClass::TextureMovie | TextureClass::Other => NativeLayout::Undecoded,
        }
    }

    /// Class name.
    pub fn name(self) -> &'static str {
        CLASS_NAMES
            .iter()
            .find(|(_, c)| *c == self)
            .map_or("Texture (other)", |(n, _)| n)
    }
}

/// One `FTexture2DMipMap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MipMap {
    /// Texel bulk data.
    pub data: BulkDataRecord,
    /// Stored `SizeX`.
    pub size_x: i32,
    /// Stored `SizeY`.
    pub size_y: i32,
}

/// Decoded `Texture2D` native data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Texture2DNative {
    /// `SourceArt` (empty in cooked packages).
    pub source_art: BulkDataRecord,
    /// The mip chain, largest first.
    pub mips: Vec<MipMap>,
    /// `TextureFileCacheGuid`.
    pub file_cache_guid: Guid,
    /// `CachedPVRTCMips`.
    pub cached_pvrtc_mips: Vec<MipMap>,
    /// `CachedFlashMipsMaxResolution`.
    pub cached_flash_mips_max_resolution: i32,
    /// `CachedATITCMips`.
    pub cached_atitc_mips: Vec<MipMap>,
    /// `CachedFlashMips`.
    pub cached_flash_mips: BulkDataRecord,
    /// `CachedETCMips`.
    pub cached_etc_mips: Vec<MipMap>,
    /// `LightmapFlags` (`LightMapTexture2D` only).
    pub lightmap_flags: Option<u32>,
}

/// Native data of a texture export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum TextureNative {
    /// Class default object: no native data.
    None,
    /// `SourceArt` only (`TextureCube`, render targets).
    SourceArtOnly {
        /// `SourceArt`.
        source_art: BulkDataRecord,
    },
    /// `Texture2D` layout.
    Texture2D(Texture2DNative),
}

fn read_mip_array(r: &mut Reader<'_>, what: &'static str) -> Result<Vec<MipMap>, TextureError> {
    let offset = r.position();
    let count = r.read_count(what, MIP_ENTRY_MIN_SIZE)?;
    if count > MAX_MIPS {
        return Err(TextureError::Malformed(format!(
            "{what} count {count} at payload offset {offset} exceeds {MAX_MIPS}"
        )));
    }
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let data = bulkdata::read_bulk_record(r)?;
        let size_x = r.read_i32()?;
        let size_y = r.read_i32()?;
        out.push(MipMap {
            data,
            size_x,
            size_y,
        });
    }
    Ok(out)
}

/// Read the native data of `layout` from `payload` starting at `start` (the
/// end of the tagged properties). Returns the data and the payload offset
/// where it ends; callers that want strict decoding compare that offset with
/// the payload length.
pub fn read_native(
    payload: &[u8],
    start: usize,
    layout: NativeLayout,
) -> Result<(TextureNative, usize), TextureError> {
    let mut r = Reader::at(payload, start)?;
    let native = match layout {
        NativeLayout::Undecoded => {
            return Err(TextureError::Malformed(
                "no verified native layout for this class".to_owned(),
            ));
        }
        NativeLayout::SourceArtOnly => TextureNative::SourceArtOnly {
            source_art: bulkdata::read_bulk_record(&mut r)?,
        },
        NativeLayout::Texture2D | NativeLayout::LightMapTexture2D => {
            let source_art = bulkdata::read_bulk_record(&mut r)?;
            let mips = read_mip_array(&mut r, "Mips")?;
            let file_cache_guid = r.read_guid()?;
            let cached_pvrtc_mips = read_mip_array(&mut r, "CachedPVRTCMips")?;
            let cached_flash_mips_max_resolution = r.read_i32()?;
            let cached_atitc_mips = read_mip_array(&mut r, "CachedATITCMips")?;
            let cached_flash_mips = bulkdata::read_bulk_record(&mut r)?;
            let cached_etc_mips = read_mip_array(&mut r, "CachedETCMips")?;
            let lightmap_flags = if layout == NativeLayout::LightMapTexture2D {
                Some(r.read_u32()?)
            } else {
                None
            };
            TextureNative::Texture2D(Texture2DNative {
                source_art,
                mips,
                file_cache_guid,
                cached_pvrtc_mips,
                cached_flash_mips_max_resolution,
                cached_atitc_mips,
                cached_flash_mips,
                cached_etc_mips,
                lightmap_flags,
            })
        }
    };
    Ok((native, r.position()))
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

/// The texture properties a converter needs, from tagged properties merged
/// over the class defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TextureProps {
    /// `SizeX`.
    pub size_x: Option<i32>,
    /// `SizeY`.
    pub size_y: Option<i32>,
    /// `OriginalSizeX`.
    pub original_size_x: Option<i32>,
    /// `OriginalSizeY`.
    pub original_size_y: Option<i32>,
    /// `Format` enumerator name.
    pub format: Option<String>,
    /// `TextureFileCacheName`.
    pub file_cache_name: Option<String>,
    /// `MipTailBaseIdx`.
    pub mip_tail_base_idx: Option<i32>,
    /// `FirstResourceMemMip`.
    pub first_resource_mem_mip: Option<i32>,
    /// `SRGB`.
    pub srgb: Option<bool>,
    /// `CompressionSettings` enumerator name.
    pub compression_settings: Option<String>,
    /// `LODGroup` enumerator name.
    pub lod_group: Option<String>,
    /// `LODBias`.
    pub lod_bias: Option<i32>,
    /// `AddressX` enumerator name.
    pub address_x: Option<String>,
    /// `AddressY` enumerator name.
    pub address_y: Option<String>,
    /// `Filter` enumerator name.
    pub filter: Option<String>,
    /// `NeverStream`.
    pub never_stream: Option<bool>,
    /// `CompressionNoAlpha`.
    pub compression_no_alpha: Option<bool>,
    /// `CompressionNone`.
    pub compression_none: Option<bool>,
    /// `ShadowmapFlags` (`ShadowMapTexture2D`).
    pub shadowmap_flags: Option<i32>,
    /// Cube faces `FacePosX`, `FaceNegX`, `FacePosY`, `FaceNegY`, `FacePosZ`,
    /// `FaceNegZ` (qualified object paths; `TextureCube`).
    pub faces: [Option<String>; 6],
    /// Flip-book `HorizontalImages`.
    pub horizontal_images: Option<i32>,
    /// Flip-book `VerticalImages`.
    pub vertical_images: Option<i32>,
}

const FACE_NAMES: [&str; 6] = [
    "FacePosX", "FaceNegX", "FacePosY", "FaceNegY", "FacePosZ", "FaceNegZ",
];

impl TextureProps {
    /// Apply one property value (top-level, array index 0 only).
    pub fn apply(&mut self, name: &str, array_index: i32, value: &Value) {
        if array_index != 0 {
            return;
        }
        let int = || match value {
            Value::Int(v) => Some(*v),
            _ => None,
        };
        let boolean = || match value {
            Value::Bool(v) => Some(*v),
            _ => None,
        };
        let text = || match value {
            Value::Enum(s) | Value::Name(s) | Value::Str(s) => Some(s.clone()),
            _ => None,
        };
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "sizex" => self.size_x = int(),
            "sizey" => self.size_y = int(),
            "originalsizex" => self.original_size_x = int(),
            "originalsizey" => self.original_size_y = int(),
            "format" => self.format = text(),
            "texturefilecachename" => self.file_cache_name = text(),
            "miptailbaseidx" => self.mip_tail_base_idx = int(),
            "firstresourcememmip" => self.first_resource_mem_mip = int(),
            "srgb" => self.srgb = boolean(),
            "compressionsettings" => self.compression_settings = text(),
            "lodgroup" => self.lod_group = text(),
            "lodbias" => self.lod_bias = int(),
            "addressx" => self.address_x = text(),
            "addressy" => self.address_y = text(),
            "filter" => self.filter = text(),
            "neverstream" => self.never_stream = boolean(),
            "compressionnoalpha" => self.compression_no_alpha = boolean(),
            "compressionnone" => self.compression_none = boolean(),
            "shadowmapflags" => self.shadowmap_flags = int(),
            "horizontalimages" => self.horizontal_images = int(),
            "verticalimages" => self.vertical_images = int(),
            _ => {
                if let Some(k) = FACE_NAMES.iter().position(|f| f.eq_ignore_ascii_case(name))
                    && let Some(slot) = self.faces.get_mut(k)
                {
                    *slot = match value {
                        Value::Object(o) if o.index != 0 => Some(o.path.clone()),
                        _ => None,
                    };
                }
            }
        }
    }

    /// Properties from tagged properties applied over `defaults`.
    pub fn from_properties(props: &[Property], defaults: Option<&TextureProps>) -> TextureProps {
        let mut out = defaults.cloned().unwrap_or_default();
        for p in props {
            out.apply(&p.name, p.array_index, &p.value);
        }
        out
    }

    /// The pixel format (`PF_Unknown` when no `Format` is set anywhere, which
    /// is the enum's zero value).
    pub fn pixel_format(&self) -> Option<PixelFormat> {
        match &self.format {
            Some(n) => PixelFormat::from_enum_name(n),
            None => Some(PixelFormat::Unknown),
        }
    }
}

// ---------------------------------------------------------------------------
// Decoding texture exports
// ---------------------------------------------------------------------------

/// A decoded texture export.
#[derive(Debug, Clone, Serialize)]
pub struct Texture {
    /// Export index.
    pub export_index: usize,
    /// Qualified object path.
    pub path: String,
    /// Qualified class path.
    pub class_path: String,
    /// Texture class.
    pub class: TextureClass,
    /// True for a class default object.
    pub is_default_object: bool,
    /// Properties (tags over class defaults).
    pub props: TextureProps,
    /// Properties from the object's own tags only (no class defaults). For
    /// `LightMapTexture2D`, which has no script class and no default object
    /// in the data, this is all that is known for certain.
    pub tagged: TextureProps,
    /// Pixel format (`None` for an unrecognised `Format` name).
    pub format: Option<PixelFormat>,
    /// Native data.
    pub native: TextureNative,
    /// Export `SerialOffset` (absolute stream offset of the payload).
    pub serial_offset: i64,
    /// Payload offset where the tagged properties end.
    pub properties_end: usize,
}

impl Texture {
    /// The mip chain (empty unless the texture has the `Texture2D` layout).
    pub fn mips(&self) -> &[MipMap] {
        match &self.native {
            TextureNative::Texture2D(t) => &t.mips,
            _ => &[],
        }
    }

    /// The `Texture2D` native data, if any.
    pub fn texture2d(&self) -> Option<&Texture2DNative> {
        match &self.native {
            TextureNative::Texture2D(t) => Some(t),
            _ => None,
        }
    }

    /// Load mip `level`'s texels (decompressed, exactly `ElementCount`
    /// bytes). `payload` is this export's payload; `caches` resolves
    /// `.tfc`-resident mips by `TextureFileCacheName`.
    pub fn load_mip(
        &self,
        payload: &[u8],
        caches: Option<&TextureFileCaches>,
        level: usize,
    ) -> Result<Vec<u8>, TextureError> {
        let mip = self
            .mips()
            .get(level)
            .ok_or_else(|| TextureError::Malformed(format!("no mip {level}")))?;
        Ok(bulkdata::load(
            &mip.data,
            payload,
            caches,
            self.props.file_cache_name.as_deref(),
            1,
        )?)
    }

    /// Index of the first mip whose texels are stored (inline or in a
    /// `.tfc`), skipping mips the cooker marked unused.
    pub fn first_stored_mip(&self) -> Option<usize> {
        self.mips()
            .iter()
            .position(|m| m.data.storage() != BulkStorage::Unused && m.data.element_count > 0)
    }
}

/// Decodes texture exports of a [`PackageSet`], caching class defaults.
pub struct TextureDecoder<'a> {
    set: &'a PackageSet,
    defaults: RefCell<HashMap<String, TextureProps>>,
    chains: RefCell<HashMap<String, Option<TextureClass>>>,
}

impl<'a> TextureDecoder<'a> {
    /// Decoder over `set`.
    pub fn new(set: &'a PackageSet) -> TextureDecoder<'a> {
        TextureDecoder {
            set,
            defaults: RefCell::default(),
            chains: RefCell::default(),
        }
    }

    /// The package set.
    pub fn set(&self) -> &'a PackageSet {
        self.set
    }

    /// Texture class of the class at qualified `class_path`, if it is one.
    pub fn class_of(&self, class_path: &str) -> Option<TextureClass> {
        let key = class_path.to_ascii_lowercase();
        if let Some(c) = self.chains.borrow().get(&key) {
            return *c;
        }
        let c = TextureClass::from_class_name(class_path)
            .or_else(|| TextureClass::classify(class_path, &self.set.super_chain(class_path)));
        self.chains.borrow_mut().insert(key, c);
        c
    }

    /// Texture class of export `index`, if it is a texture.
    pub fn export_class(&self, lp: &LoadedPackage, index: usize) -> Option<(String, TextureClass)> {
        let class_path =
            crate::object::export_class_path(&lp.package, Some(&lp.name), index).ok()?;
        let c = self.class_of(&class_path)?;
        Some((class_path, c))
    }

    /// Merged class defaults for `class_path` as [`TextureProps`].
    ///
    /// A native-only class without a script definition or default object in
    /// the data (`LightMapTexture2D`) gets no defaults at all, not those of
    /// `Engine.Texture2D`: its native constructor may override them, so its
    /// [`Texture::props`] are exactly its own tags (CONFIRMED: real lightmaps
    /// have `props == tagged`).
    pub fn class_defaults(&self, class_path: &str) -> TextureProps {
        let key = class_path.to_ascii_lowercase();
        if let Some(d) = self.defaults.borrow().get(&key) {
            return d.clone();
        }
        let merged = self.set.inherited_defaults(class_path);
        let mut props = TextureProps::default();
        if let Ok(d) = merged {
            for v in &d.values {
                props.apply(&v.name, v.array_index, &v.value);
            }
        }
        self.defaults.borrow_mut().insert(key, props.clone());
        props
    }

    /// Decode export `index` of `lp` strictly: prelude, tagged properties
    /// and native data must consume exactly `SerialSize` bytes.
    pub fn decode(&self, lp: &LoadedPackage, index: usize) -> Result<Texture, TextureError> {
        let Some((class_path, class)) = self.export_class(lp, index) else {
            let class = crate::object::export_class_path(&lp.package, Some(&lp.name), index)
                .unwrap_or_default();
            return Err(TextureError::NotATexture {
                export: index,
                class,
            });
        };
        let entry = lp.package.export(index)?;
        let is_default_object = entry.object_flags & flags::object::CLASS_DEFAULT_OBJECT != 0;
        let serial_offset = i64::from(entry.serial_offset);
        let obj = self.set.decode(lp, index)?;
        let payload = lp.package.export_data(index)?;
        let defaults = self.class_defaults(&class_path);
        let props = TextureProps::from_properties(&obj.properties, Some(&defaults));
        let tagged = TextureProps::from_properties(&obj.properties, None);
        let native = if is_default_object {
            if obj.properties_end != payload.len() {
                return Err(TextureError::Malformed(format!(
                    "class default object {} has {} bytes after its tagged properties",
                    obj.path,
                    payload.len().saturating_sub(obj.properties_end)
                )));
            }
            TextureNative::None
        } else {
            let layout = class.layout();
            if layout == NativeLayout::Undecoded {
                return Err(TextureError::NotATexture {
                    export: index,
                    class: class_path,
                });
            }
            let (native, end) = read_native(payload, obj.properties_end, layout)?;
            if end != payload.len() {
                return Err(TextureError::Malformed(format!(
                    "{}: native data ends at {end} of {} payload bytes",
                    obj.path,
                    payload.len()
                )));
            }
            native
        };
        let format = props.pixel_format();
        Ok(Texture {
            export_index: index,
            path: obj.path,
            class_path,
            class,
            is_default_object,
            props,
            tagged,
            format,
            native,
            serial_offset,
            properties_end: obj.properties_end,
        })
    }
}

// ---------------------------------------------------------------------------
// Texel decoding (previews)
// ---------------------------------------------------------------------------

fn expand565(c: u16) -> [u8; 3] {
    let r = ((c >> 11) & 0x1f) as u8;
    let g = ((c >> 5) & 0x3f) as u8;
    let b = (c & 0x1f) as u8;
    [
        (r << 3) | (r >> 2),
        (g << 2) | (g >> 4),
        (b << 3) | (b >> 2),
    ]
}

fn le16(b: &[u8], at: usize) -> u16 {
    match b.get(at..at + 2) {
        Some(s) => u16::from_le_bytes([s[0], s[1]]),
        None => 0,
    }
}

fn le32(b: &[u8], at: usize) -> u32 {
    match b.get(at..at + 4) {
        Some(s) => u32::from_le_bytes([s[0], s[1], s[2], s[3]]),
        None => 0,
    }
}

/// Decode one 8-byte BC1/DXT1 colour block into 16 RGBA texels (row-major).
/// With `force_four_colour` (DXT3/DXT5 colour blocks) the three-colour
/// punch-through mode is never used.
pub fn decode_bc1_block(block: &[u8], force_four_colour: bool) -> [[u8; 4]; 16] {
    let c0 = le16(block, 0);
    let c1 = le16(block, 2);
    let bits = le32(block, 4);
    let p0 = expand565(c0);
    let p1 = expand565(c1);
    let mix = |a: u8, b: u8, wa: u16, wb: u16, d: u16| -> u8 {
        ((u16::from(a) * wa + u16::from(b) * wb) / d) as u8
    };
    let mut palette = [[0u8; 4]; 4];
    palette[0] = [p0[0], p0[1], p0[2], 255];
    palette[1] = [p1[0], p1[1], p1[2], 255];
    if c0 > c1 || force_four_colour {
        for ch in 0..3 {
            palette[2][ch] = mix(p0[ch], p1[ch], 2, 1, 3);
            palette[3][ch] = mix(p0[ch], p1[ch], 1, 2, 3);
        }
        palette[2][3] = 255;
        palette[3][3] = 255;
    } else {
        for ch in 0..3 {
            palette[2][ch] = mix(p0[ch], p1[ch], 1, 1, 2);
        }
        palette[2][3] = 255;
        palette[3] = [0, 0, 0, 0];
    }
    let mut out = [[0u8; 4]; 16];
    for (i, texel) in out.iter_mut().enumerate() {
        let code = (bits >> (2 * i)) & 3;
        *texel = palette[code as usize];
    }
    out
}

/// Decode one 8-byte BC4 block (the DXT5 alpha block) into 16 values.
pub fn decode_bc4_block(block: &[u8]) -> [u8; 16] {
    let a0 = block.first().copied().unwrap_or(0);
    let a1 = block.get(1).copied().unwrap_or(0);
    let mut table = [0u8; 8];
    table[0] = a0;
    table[1] = a1;
    let (a0w, a1w) = (u32::from(a0), u32::from(a1));
    if a0 > a1 {
        for k in 2..8u32 {
            table[k as usize] = (((8 - k) * a0w + (k - 1) * a1w) / 7) as u8;
        }
    } else {
        for k in 2..6u32 {
            table[k as usize] = (((6 - k) * a0w + (k - 1) * a1w) / 5) as u8;
        }
        table[6] = 0;
        table[7] = 255;
    }
    let mut bits: u64 = 0;
    for i in 0..6 {
        bits |= u64::from(block.get(2 + i).copied().unwrap_or(0)) << (8 * i);
    }
    let mut out = [0u8; 16];
    for (i, v) in out.iter_mut().enumerate() {
        *v = table[((bits >> (3 * i)) & 7) as usize];
    }
    out
}

/// Decode one 16-byte DXT3 block (explicit 4-bit alpha + colour).
pub fn decode_dxt3_block(block: &[u8]) -> [[u8; 4]; 16] {
    let mut out = decode_bc1_block(block.get(8..16).unwrap_or(&[]), true);
    for (i, texel) in out.iter_mut().enumerate() {
        let byte = block.get(i / 2).copied().unwrap_or(0);
        let nibble = if i % 2 == 0 { byte & 0x0f } else { byte >> 4 };
        texel[3] = nibble * 17;
    }
    out
}

/// Decode one 16-byte DXT5 block (interpolated alpha + colour).
pub fn decode_dxt5_block(block: &[u8]) -> [[u8; 4]; 16] {
    let alpha = decode_bc4_block(block.get(0..8).unwrap_or(&[]));
    let mut out = decode_bc1_block(block.get(8..16).unwrap_or(&[]), true);
    for (texel, a) in out.iter_mut().zip(alpha) {
        texel[3] = a;
    }
    out
}

/// Reconstruct the Z of a unit normal from X and Y stored as unsigned bytes.
fn normal_z(x: u8, y: u8) -> u8 {
    let fx = f32::from(x) / 127.5 - 1.0;
    let fy = f32::from(y) / 127.5 - 1.0;
    let z = (1.0 - fx * fx - fy * fy).max(0.0).sqrt();
    ((z * 0.5 + 0.5) * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Decode one 16-byte BC5 block (red then green BC4 blocks). The preview
/// puts the reconstructed normal Z in blue and 255 in alpha.
pub fn decode_bc5_block(block: &[u8]) -> [[u8; 4]; 16] {
    let r = decode_bc4_block(block.get(0..8).unwrap_or(&[]));
    let g = decode_bc4_block(block.get(8..16).unwrap_or(&[]));
    let mut out = [[0u8; 4]; 16];
    for i in 0..16 {
        out[i] = [r[i], g[i], normal_z(r[i], g[i]), 255];
    }
    out
}

/// Decode a `w` x `h` mip in `format` to tightly packed RGBA8 (row-major,
/// top row first). `data` must hold at least
/// [`PixelFormat::mip_bytes`]`(w, h)` bytes.
///
/// Conventions: `A8R8G8B8` texels are stored B, G, R, A; `G8` becomes grey;
/// `V8U8` signed bytes are biased by 128 into R and G with the reconstructed
/// normal Z in blue; `BC5` likewise. Only formats used by the shipped data
/// (and DXT3) are supported.
pub fn decode_to_rgba8(
    format: PixelFormat,
    w: u32,
    h: u32,
    data: &[u8],
) -> Result<Vec<u8>, TextureError> {
    if w == 0 || h == 0 || w > MAX_DIMENSION || h > MAX_DIMENSION {
        return Err(TextureError::Malformed(format!("bad mip size {w}x{h}")));
    }
    let need = format
        .mip_bytes(w, h)
        .ok_or_else(|| TextureError::UnsupportedFormat(format.enum_name().to_owned()))?;
    if (data.len() as u64) < need {
        return Err(TextureError::Malformed(format!(
            "{}x{h} {} mip needs {need} bytes, got {}",
            w,
            format.enum_name(),
            data.len()
        )));
    }
    let (wu, hu) = (w as usize, h as usize);
    let mut out = vec![0u8; wu * hu * 4];
    let put = |out: &mut [u8], x: usize, y: usize, px: [u8; 4]| {
        let at = (y * wu + x) * 4;
        if let Some(dst) = out.get_mut(at..at + 4) {
            dst.copy_from_slice(&px);
        }
    };
    match format {
        PixelFormat::Dxt1 | PixelFormat::Dxt3 | PixelFormat::Dxt5 | PixelFormat::Bc5 => {
            let block_bytes = if format == PixelFormat::Dxt1 { 8 } else { 16 };
            let bw = wu.div_ceil(4);
            let bh = hu.div_ceil(4);
            for by in 0..bh {
                for bx in 0..bw {
                    let at = (by * bw + bx) * block_bytes;
                    let block = data.get(at..at + block_bytes).unwrap_or(&[]);
                    let texels = match format {
                        PixelFormat::Dxt1 => decode_bc1_block(block, false),
                        PixelFormat::Dxt3 => decode_dxt3_block(block),
                        PixelFormat::Dxt5 => decode_dxt5_block(block),
                        _ => decode_bc5_block(block),
                    };
                    for (i, px) in texels.iter().enumerate() {
                        let x = bx * 4 + i % 4;
                        let y = by * 4 + i / 4;
                        if x < wu && y < hu {
                            put(&mut out, x, y, *px);
                        }
                    }
                }
            }
        }
        PixelFormat::A8R8G8B8 => {
            for (i, px) in data.as_chunks::<4>().0.iter().take(wu * hu).enumerate() {
                put(&mut out, i % wu, i / wu, [px[2], px[1], px[0], px[3]]);
            }
        }
        PixelFormat::G8 => {
            for (i, &v) in data.iter().take(wu * hu).enumerate() {
                put(&mut out, i % wu, i / wu, [v, v, v, 255]);
            }
        }
        PixelFormat::G16 => {
            for (i, px) in data.as_chunks::<2>().0.iter().take(wu * hu).enumerate() {
                let v = px[1];
                put(&mut out, i % wu, i / wu, [v, v, v, 255]);
            }
        }
        PixelFormat::V8U8 => {
            for (i, px) in data.as_chunks::<2>().0.iter().take(wu * hu).enumerate() {
                let u = px[0].wrapping_add(128);
                let v = px[1].wrapping_add(128);
                put(&mut out, i % wu, i / wu, [u, v, normal_z(u, v), 255]);
            }
        }
        other => {
            return Err(TextureError::UnsupportedFormat(
                other.enum_name().to_owned(),
            ));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Coverage over real packages
// ---------------------------------------------------------------------------

/// Most failure samples kept per statistic.
const MAX_SAMPLES: usize = 16;

fn sample(v: &mut Vec<String>, s: String) {
    if v.len() < MAX_SAMPLES {
        v.push(s);
    }
}

/// Per-class counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ClassCoverage {
    /// Exports of the class (including class default objects).
    pub total: usize,
    /// Class default objects (no native data; checked to be empty).
    pub default_objects: usize,
    /// Exports whose native data decoded and consumed exactly `SerialSize`.
    pub exact: usize,
    /// Exports that failed to decode.
    pub failed: usize,
}

/// Per-format counts over `Texture2D`-layout exports.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FormatCoverage {
    /// Textures.
    pub textures: usize,
    /// Mips in total.
    pub mips: usize,
    /// Mips stored inline in the package.
    pub inline_mips: usize,
    /// Mips stored in a `.tfc`.
    pub tfc_mips: usize,
    /// Mips marked unused.
    pub unused_mips: usize,
    /// Mips whose payload is LZO-compressed.
    pub lzo_mips: usize,
    /// Mips whose `ElementCount` equals the format math for their stored size.
    pub size_ok: usize,
    /// Mips whose payload was loaded (and decompressed) to exactly `ElementCount` bytes.
    pub loaded_ok: usize,
    /// Bytes of texels (sum of `ElementCount` over stored mips).
    pub texel_bytes: u64,
    /// Bytes stored on disk for those mips.
    pub stored_bytes: u64,
}

/// One package's texture coverage.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PackageTextureCoverage {
    /// Package name.
    pub package: String,
    /// Per class.
    pub classes: BTreeMap<String, ClassCoverage>,
    /// Per pixel format (by enum name).
    pub formats: BTreeMap<String, FormatCoverage>,
    /// Problems found (first few).
    pub failures: Vec<String>,
    /// Number of problems found.
    pub failure_count: usize,
}

/// Payload ranges of one `.tfc` referenced by mips.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FileCacheCoverage {
    /// File length.
    pub file_len: u64,
    /// Mip records pointing into the file.
    pub records: usize,
    /// Distinct `(offset, size)` ranges.
    pub distinct_ranges: usize,
    /// Distinct ranges that overlap another distinct range.
    pub overlapping_ranges: usize,
    /// Bytes covered by the distinct ranges.
    pub covered_bytes: u64,
    /// Bytes of the file not covered by any range.
    pub uncovered_bytes: u64,
    /// Gaps between consecutive ranges (and before the first / after the last).
    pub gaps: usize,
    /// Distinct `TextureFileCacheGuid` values of textures using the file.
    pub distinct_guids: usize,
    /// Records whose range lies outside the file.
    pub out_of_range: usize,
    /// Well-formed compressed payloads found by walking the uncovered gaps
    /// (data in the file that no shipped texture references).
    pub gap_payloads: usize,
    /// Gap bytes tiled exactly by those payloads.
    pub gap_bytes_tiled: u64,
}

/// Coverage of every texture export in a set of packages, with the
/// structural facts recorded in `TEXTURES.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TextureCoverage {
    /// Per package.
    pub packages: Vec<PackageTextureCoverage>,
    /// Per class, all packages.
    pub classes: BTreeMap<String, ClassCoverage>,
    /// Per pixel format, all packages.
    pub formats: BTreeMap<String, FormatCoverage>,
    /// Per `.tfc` (lower-case name).
    pub file_caches: BTreeMap<String, FileCacheCoverage>,
    /// Bulk flag values of mip records (hex) and their counts.
    pub mip_flags: BTreeMap<String, usize>,
    /// Bulk flag values of `SourceArt` records.
    pub source_art_flags: BTreeMap<String, usize>,
    /// Bulk flag values of `CachedFlashMips` records.
    pub cached_flash_flags: BTreeMap<String, usize>,
    /// `SourceArt` records with a non-zero element count or stored size.
    pub source_art_non_empty: usize,
    /// Textures with any non-empty PVRTC/ATITC/ETC mip array.
    pub cached_platform_mips_non_empty: usize,
    /// Textures with a non-zero `CachedFlashMipsMaxResolution`.
    pub cached_flash_resolution_non_zero: usize,
    /// `LightmapFlags` values and counts.
    pub lightmap_flags: BTreeMap<u32, usize>,
    /// Inline records whose `BulkDataOffsetInFile` equals their stream position.
    pub inline_offset_ok: usize,
    /// Inline records whose offset does not.
    pub inline_offset_bad: usize,
    /// `SourceArt` records (all classes) whose `BulkDataOffsetInFile` equals
    /// their own absolute stream position.
    pub source_art_offset_ok: usize,
    /// `SourceArt` records whose offset does not.
    pub source_art_offset_bad: usize,
    /// Mips whose stored size equals the natural halving of mip 0's size.
    pub mip_dims_natural: usize,
    /// Mips (block-compressed formats) whose stored size is the natural size
    /// clamped up to the 4x4 block.
    pub mip_dims_block_clamped: usize,
    /// Mips matching neither rule.
    pub mip_dims_other: usize,
    /// Textures whose mip 0 size equals `SizeX` x `SizeY`.
    pub mip0_matches_size: usize,
    /// Textures whose mip 0 size differs from `SizeX` x `SizeY`.
    pub mip0_differs_from_size: usize,
    /// Textures where `FirstResourceMemMip` (0 when absent) equals the index
    /// of the first inline mip (the number of leading unused and `.tfc` mips).
    pub first_resource_mem_mip_ok: usize,
    /// Textures where it does not.
    pub first_resource_mem_mip_other: usize,
    /// Multi-mip textures whose `MipTailBaseIdx` equals the last mip index.
    pub mip_tail_is_last: usize,
    /// Multi-mip textures where it does not (or is absent).
    pub mip_tail_other: usize,
    /// Single-mip textures (their `MipTailBaseIdx` is not checked).
    pub single_mip_textures: usize,
    /// `MipTailBaseIdx` of single-mip textures: `"absent"`, `"full chain"`
    /// (the last index of a full chain for `SizeX` x `SizeY`, i.e.
    /// `floor(log2(largest edge))`), `"full chain + 1"`, or `"other"`.
    pub single_mip_tail: BTreeMap<String, usize>,
    /// Multi-mip textures whose chain runs from `SizeX` x `SizeY` down to 1x1.
    pub full_chains: usize,
    /// Multi-mip textures whose chain is shorter or longer than that.
    pub partial_chains: usize,
    /// Textures whose unused mips (if any) all precede the stored ones.
    pub unused_mips_leading: usize,
    /// Multi-mip textures larger than 1024 texels or with unused mips, by
    /// `"<LODGroup> <largest edge> unused=<count>"` (an untagged group is
    /// `TEXTUREGROUP_World`, the enum's zero value; untagged lightmaps are
    /// listed separately because their class has no default object).
    pub stripped_mips_by_group: BTreeMap<String, usize>,
    /// Textures whose `.tfc` mips all precede their inline mips.
    pub tfc_mips_leading: usize,
    /// Textures that follow the streaming split: with several mips and
    /// without `NeverStream`, exactly the stored mips whose larger edge is at
    /// least 128 are in the `.tfc`; single-mip and `NeverStream` textures
    /// keep every mip inline.
    pub streaming_split_ok: usize,
    /// Textures that do not.
    pub streaming_split_other: usize,
    /// `"<class> <CompressionSettings> -> <Format>"` counts (an untagged
    /// setting is `TC_Default`, the enum's zero value).
    pub format_by_settings: BTreeMap<String, usize>,
    /// Textures with `.tfc` mips but no `TextureFileCacheName`.
    pub tfc_without_name: usize,
    /// Textures whose `TextureFileCacheGuid` is zero.
    pub zero_guid: usize,
    /// Textures with at least one `.tfc` mip.
    pub textures_with_tfc_mips: usize,
    /// Inline mips whose payload is LZO-compressed.
    pub inline_lzo_mips: usize,
    /// `BlockSize` of the LZO payloads loaded, and how many payloads use it.
    pub lzo_block_sizes: BTreeMap<u32, usize>,
    /// LZO blocks decompressed in total.
    pub lzo_blocks: usize,
    /// Distinct `.tfc` ranges referenced with more than one
    /// `TextureFileCacheGuid` (the same cached data always carries one GUID
    /// when this is 0).
    pub range_guid_conflicts: usize,
    /// `A8R8G8B8` textures whose loaded mip 0 has a constant 0xFF in byte
    /// lane 0, 1, 2, 3 (lanes in storage order).
    pub argb_lane_constant_ff: [usize; 4],
    /// `A8R8G8B8` textures whose loaded mip 0 has a constant value (any) in
    /// each byte lane.
    pub argb_lane_constant: [usize; 4],
    /// `A8R8G8B8` textures whose mip 0 was examined for the two counters above.
    pub argb_textures: usize,
    /// Number of problems found in all packages.
    pub failure_count: usize,
    #[serde(skip)]
    ranges: BTreeMap<String, Vec<(u64, u64, Guid)>>,
}

fn bump<K: Ord>(m: &mut BTreeMap<K, usize>, k: K) {
    *m.entry(k).or_insert(0) += 1;
}

fn hex_flags(f: u32) -> String {
    format!("{f:#04x}")
}

impl TextureCoverage {
    /// Decode and check every texture export of `lp`. With `load_payloads`,
    /// every stored mip is also read (from the package or its `.tfc`) and
    /// decompressed, and must yield exactly `ElementCount` bytes.
    pub fn add_package(
        &mut self,
        decoder: &TextureDecoder<'_>,
        lp: &LoadedPackage,
        caches: &TextureFileCaches,
        load_payloads: bool,
    ) {
        let mut pc = PackageTextureCoverage {
            package: lp.name.clone(),
            ..PackageTextureCoverage::default()
        };
        for index in 0..lp.package.exports.len() {
            let Some((_, class)) = decoder.export_class(lp, index) else {
                continue;
            };
            let class_name = class.name().to_owned();
            let cc = pc.classes.entry(class_name.clone()).or_default();
            cc.total += 1;
            let tex = match decoder.decode(lp, index) {
                Ok(t) => t,
                Err(e) => {
                    cc.failed += 1;
                    pc.failure_count += 1;
                    sample(
                        &mut pc.failures,
                        format!("{index} {}: {e}", lp.qualified(index).unwrap_or_default()),
                    );
                    continue;
                }
            };
            if tex.is_default_object {
                cc.default_objects += 1;
                continue;
            }
            cc.exact += 1;
            let payload = lp.package.export_data(index).unwrap_or(&[]);
            match &tex.native {
                TextureNative::None => {}
                TextureNative::SourceArtOnly { source_art } => {
                    self.check_source_art(&tex, source_art, &mut pc);
                }
                TextureNative::Texture2D(t) => {
                    self.check_texture2d(&tex, t, payload, caches, load_payloads, &mut pc);
                }
            }
        }
        for (k, v) in &pc.classes {
            let c = self.classes.entry(k.clone()).or_default();
            c.total += v.total;
            c.default_objects += v.default_objects;
            c.exact += v.exact;
            c.failed += v.failed;
        }
        for (k, v) in &pc.formats {
            let f = self.formats.entry(k.clone()).or_default();
            f.textures += v.textures;
            f.mips += v.mips;
            f.inline_mips += v.inline_mips;
            f.tfc_mips += v.tfc_mips;
            f.unused_mips += v.unused_mips;
            f.lzo_mips += v.lzo_mips;
            f.size_ok += v.size_ok;
            f.loaded_ok += v.loaded_ok;
            f.texel_bytes += v.texel_bytes;
            f.stored_bytes += v.stored_bytes;
        }
        self.failure_count += pc.failure_count;
        self.packages.push(pc);
    }

    /// Flags, emptiness and the absolute offset of a `SourceArt` record.
    fn check_source_art(
        &mut self,
        tex: &Texture,
        source_art: &BulkDataRecord,
        pc: &mut PackageTextureCoverage,
    ) {
        bump(&mut self.source_art_flags, hex_flags(source_art.flags));
        if source_art.element_count != 0 || source_art.size_on_disk != 0 {
            self.source_art_non_empty += 1;
        }
        if source_art.has_inline_bytes() && source_art.inline_offset_matches(tex.serial_offset) {
            self.source_art_offset_ok += 1;
        } else {
            self.source_art_offset_bad += 1;
            pc.failure_count += 1;
            sample(
                &mut pc.failures,
                format!(
                    "{}: SourceArt offset {} is not its stream position",
                    tex.path, source_art.offset_in_file
                ),
            );
        }
    }

    fn check_texture2d(
        &mut self,
        tex: &Texture,
        t: &Texture2DNative,
        payload: &[u8],
        caches: &TextureFileCaches,
        load_payloads: bool,
        pc: &mut PackageTextureCoverage,
    ) {
        let mut problems: Vec<String> = Vec::new();
        let fail = |problems: &mut Vec<String>, msg: String| problems.push(msg);
        let format_name = tex
            .props
            .format
            .clone()
            .unwrap_or_else(|| "PF_Unknown".to_owned());
        let format = tex.format;
        self.check_source_art(tex, &t.source_art, pc);
        bump(
            &mut self.cached_flash_flags,
            hex_flags(t.cached_flash_mips.flags),
        );
        if !t.cached_pvrtc_mips.is_empty()
            || !t.cached_atitc_mips.is_empty()
            || !t.cached_etc_mips.is_empty()
        {
            self.cached_platform_mips_non_empty += 1;
        }
        if t.cached_flash_mips_max_resolution != 0 {
            self.cached_flash_resolution_non_zero += 1;
        }
        if let Some(f) = t.lightmap_flags {
            bump(&mut self.lightmap_flags, f);
        }
        if t.file_cache_guid.is_zero() {
            self.zero_guid += 1;
        }

        let mut fc = FormatCoverage {
            textures: 1,
            ..FormatCoverage::default()
        };

        // Mip sizes against mip 0 and the SizeX/SizeY properties.
        let (w0, h0) = match t.mips.first() {
            Some(m) => (
                u32::try_from(m.size_x).unwrap_or(0),
                u32::try_from(m.size_y).unwrap_or(0),
            ),
            None => (0, 0),
        };
        if let Some(m) = t.mips.first() {
            if Some(m.size_x) == tex.props.size_x && Some(m.size_y) == tex.props.size_y {
                self.mip0_matches_size += 1;
            } else {
                self.mip0_differs_from_size += 1;
            }
        }
        let block = format.is_some_and(PixelFormat::is_block_compressed);
        let mut tfc_count = 0usize;
        let mut first_inline: Option<usize> = None;
        let mut seen_stored = false;
        let mut unused_after_stored = false;
        let mut tfc_after_inline = false;
        for (level, mip) in t.mips.iter().enumerate() {
            let rec = &mip.data;
            bump(&mut self.mip_flags, hex_flags(rec.flags));
            fc.mips += 1;
            let (nw, nh) = mip_dims(w0, h0, u32::try_from(level).unwrap_or(u32::MAX));
            let stored = (
                u32::try_from(mip.size_x).unwrap_or(0),
                u32::try_from(mip.size_y).unwrap_or(0),
            );
            if stored == (nw, nh) {
                self.mip_dims_natural += 1;
            } else if block && stored == (nw.max(4), nh.max(4)) {
                self.mip_dims_block_clamped += 1;
            } else {
                self.mip_dims_other += 1;
                fail(
                    &mut problems,
                    format!(
                        "mip {level} stored as {}x{}, natural {nw}x{nh}",
                        stored.0, stored.1
                    ),
                );
            }
            if rec.compression() == BulkCompression::Lzo {
                fc.lzo_mips += 1;
            }
            match rec.storage() {
                BulkStorage::Unused => {
                    fc.unused_mips += 1;
                    if seen_stored {
                        unused_after_stored = true;
                    }
                    continue;
                }
                BulkStorage::Inline => {
                    fc.inline_mips += 1;
                    first_inline.get_or_insert(level);
                    if rec.compression() == BulkCompression::Lzo {
                        self.inline_lzo_mips += 1;
                    }
                    if rec.inline_offset_matches(tex.serial_offset) {
                        self.inline_offset_ok += 1;
                    } else {
                        self.inline_offset_bad += 1;
                        fail(
                            &mut problems,
                            format!("mip {level} inline offset {}", rec.offset_in_file),
                        );
                    }
                }
                BulkStorage::SeparateFile => {
                    fc.tfc_mips += 1;
                    tfc_count += 1;
                    if first_inline.is_some() {
                        tfc_after_inline = true;
                    }
                    match tex.props.file_cache_name.as_deref() {
                        Some(name) => {
                            let off = u64::try_from(rec.offset_in_file).unwrap_or(0);
                            self.ranges
                                .entry(name.to_ascii_lowercase())
                                .or_default()
                                .push((off, rec.stored_len() as u64, t.file_cache_guid));
                        }
                        None => fail(
                            &mut problems,
                            format!("mip {level} in a .tfc but no TextureFileCacheName"),
                        ),
                    }
                }
            }
            seen_stored = true;
            let count = u64::try_from(rec.element_count).unwrap_or(0);
            fc.texel_bytes += count;
            fc.stored_bytes += rec.stored_len() as u64;
            match format.and_then(|f| f.mip_bytes(stored.0, stored.1)) {
                Some(expect) if expect == count => fc.size_ok += 1,
                Some(expect) => fail(
                    &mut problems,
                    format!(
                        "mip {level} {}x{} holds {count} bytes, {format_name} needs {expect}",
                        stored.0, stored.1
                    ),
                ),
                None => fail(
                    &mut problems,
                    format!("mip {level}: no texel layout for {format_name}"),
                ),
            }
            if load_payloads {
                match self.load_and_record(tex, rec, payload, caches) {
                    Ok(bytes) if bytes.len() as u64 == count => {
                        fc.loaded_ok += 1;
                        if level == 0 && format == Some(PixelFormat::A8R8G8B8) {
                            self.argb_lanes(&bytes);
                        }
                    }
                    Ok(bytes) => fail(
                        &mut problems,
                        format!("mip {level} loaded {} bytes, expected {count}", bytes.len()),
                    ),
                    Err(e) => fail(&mut problems, format!("mip {level}: {e}")),
                }
            }
        }
        let agg = pc.formats.entry(format_name.clone()).or_default();
        agg.textures += fc.textures;
        agg.mips += fc.mips;
        agg.inline_mips += fc.inline_mips;
        agg.tfc_mips += fc.tfc_mips;
        agg.unused_mips += fc.unused_mips;
        agg.lzo_mips += fc.lzo_mips;
        agg.size_ok += fc.size_ok;
        agg.loaded_ok += fc.loaded_ok;
        agg.texel_bytes += fc.texel_bytes;
        agg.stored_bytes += fc.stored_bytes;
        pc.failure_count += problems.len();
        for msg in problems {
            sample(&mut pc.failures, format!("{}: {msg}", tex.path));
        }
        if tfc_count > 0 {
            self.textures_with_tfc_mips += 1;
            if tex.props.file_cache_name.is_none() {
                self.tfc_without_name += 1;
            }
        }
        if !tfc_after_inline {
            self.tfc_mips_leading += 1;
        }
        if !unused_after_stored {
            self.unused_mips_leading += 1;
        }
        bump(
            &mut self.format_by_settings,
            format!(
                "{} {} -> {format_name}",
                tex.class.name(),
                tex.props
                    .compression_settings
                    .as_deref()
                    .unwrap_or("TC_Default")
            ),
        );
        let streams = t.mips.len() > 1 && tex.props.never_stream != Some(true);
        let split_ok = t.mips.iter().all(|m| {
            let edge = m.size_x.max(m.size_y);
            match m.data.storage() {
                BulkStorage::Unused => true,
                BulkStorage::SeparateFile => streams && edge >= 128,
                BulkStorage::Inline => !streams || edge < 128,
            }
        });
        if split_ok {
            self.streaming_split_ok += 1;
        } else {
            self.streaming_split_other += 1;
        }
        let largest = tex.props.size_x.max(tex.props.size_y).unwrap_or(0);
        if t.mips.len() > 1 && (largest > 1024 || fc.unused_mips > 0) {
            let group = match (&tex.props.lod_group, tex.class) {
                (Some(g), _) => g.as_str(),
                // No default object in the data: the native default is unknown.
                (None, TextureClass::LightMapTexture2D) => "LightMapTexture2D(untagged)",
                (None, _) => "TEXTUREGROUP_World",
            };
            bump(
                &mut self.stripped_mips_by_group,
                format!("{group} {largest} unused={}", fc.unused_mips),
            );
        }
        let first_mem = tex.props.first_resource_mem_mip.unwrap_or(0);
        if usize::try_from(first_mem).ok() == Some(first_inline.unwrap_or(t.mips.len())) {
            self.first_resource_mem_mip_ok += 1;
        } else {
            self.first_resource_mem_mip_other += 1;
        }
        if t.mips.len() == 1 {
            self.single_mip_textures += 1;
            let full_last = u32::try_from(largest)
                .ok()
                .filter(|&v| v > 0)
                .map(|v| i64::from(v.ilog2()));
            let kind = match (tex.tagged.mip_tail_base_idx.map(i64::from), full_last) {
                (None, _) => "absent",
                (Some(v), Some(f)) if v == f => "full chain",
                (Some(v), Some(f)) if Some(v) == f.checked_add(1) => "full chain + 1",
                _ => "other",
            };
            bump(&mut self.single_mip_tail, kind.to_owned());
        } else {
            let last = t.mips.len().checked_sub(1);
            if tex
                .props
                .mip_tail_base_idx
                .and_then(|v| usize::try_from(v).ok())
                .is_some_and(|v| Some(v) == last)
            {
                self.mip_tail_is_last += 1;
            } else {
                self.mip_tail_other += 1;
            }
            let full = u32::try_from(largest)
                .ok()
                .filter(|&v| v > 0)
                .map(|v| (v.ilog2() as usize) + 1);
            if full == Some(t.mips.len()) {
                self.full_chains += 1;
            } else {
                self.partial_chains += 1;
            }
        }
    }

    /// Load a mip like [`Texture::load_mip`], recording the compressed
    /// payload layout on the way.
    fn load_and_record(
        &mut self,
        tex: &Texture,
        rec: &BulkDataRecord,
        payload: &[u8],
        caches: &TextureFileCaches,
    ) -> Result<Vec<u8>, BulkError> {
        let expected = bulkdata::expected_len(rec, 1)?;
        let stored = bulkdata::read_stored(
            rec,
            payload,
            Some(caches),
            tex.props.file_cache_name.as_deref(),
        )?;
        if rec.compression() == BulkCompression::Lzo {
            let layout = bulkdata::parse_compressed(&stored)?;
            bump(&mut self.lzo_block_sizes, layout.block_size);
            self.lzo_blocks += layout.blocks.len();
        }
        bulkdata::decode_payload(rec, &stored, expected)
    }

    fn argb_lanes(&mut self, bytes: &[u8]) {
        let Some(first) = bytes.get(0..4) else {
            return;
        };
        let mut constant = [true; 4];
        for px in bytes.as_chunks::<4>().0 {
            for lane in 0..4 {
                if px[lane] != first[lane] {
                    constant[lane] = false;
                }
            }
        }
        self.argb_textures += 1;
        for lane in 0..4 {
            if constant[lane] {
                self.argb_lane_constant[lane] += 1;
                if first[lane] == 0xFF {
                    self.argb_lane_constant_ff[lane] += 1;
                }
            }
        }
    }

    /// Compute the per-`.tfc` range statistics, and walk the bytes no
    /// shipped texture references to see whether they hold well-formed
    /// compressed payloads. Call once after the last
    /// [`TextureCoverage::add_package`].
    pub fn finish(&mut self, caches: &TextureFileCaches) {
        for (name, len) in caches.iter().map(|(n, _, l)| (n.to_owned(), l)) {
            self.file_caches.entry(name).or_default().file_len = len;
        }
        let ranges = std::mem::take(&mut self.ranges);
        for (name, mut list) in ranges {
            let fcc = self.file_caches.entry(name.clone()).or_default();
            fcc.records = list.len();
            list.sort_unstable_by_key(|&(off, size, g)| (off, size, g.a, g.b, g.c, g.d));
            let mut guids: Vec<(u32, u32, u32, u32)> =
                list.iter().map(|&(_, _, g)| (g.a, g.b, g.c, g.d)).collect();
            guids.sort_unstable();
            guids.dedup();
            fcc.distinct_guids = guids.len();
            // `list` is sorted by range, so the records of one range are adjacent.
            let mut distinct: Vec<(u64, u64)> = Vec::with_capacity(list.len());
            for group in list.chunk_by(|a, b| (a.0, a.1) == (b.0, b.1)) {
                if let Some(&(off, size, g)) = group.first() {
                    if group.iter().any(|&(_, _, other)| other != g) {
                        self.range_guid_conflicts += 1;
                    }
                    distinct.push((off, size));
                }
            }
            fcc.distinct_ranges = distinct.len();
            let mut covered = 0u64;
            let mut pos = 0u64;
            let mut overlapping = 0usize;
            let mut gap_list: Vec<(u64, u64)> = Vec::new();
            for &(off, size) in &distinct {
                let end = off.saturating_add(size);
                if end > fcc.file_len {
                    fcc.out_of_range += 1;
                }
                if off > pos {
                    gap_list.push((pos, off));
                    covered += size;
                } else if off < pos {
                    overlapping += 1;
                    covered += end.saturating_sub(pos);
                } else {
                    covered += size;
                }
                pos = pos.max(end);
            }
            if pos < fcc.file_len {
                gap_list.push((pos, fcc.file_len));
            }
            fcc.gaps = gap_list.len();
            fcc.overlapping_ranges = overlapping;
            fcc.covered_bytes = covered;
            fcc.uncovered_bytes = fcc.file_len.saturating_sub(covered);
            for (start, end) in gap_list {
                let (n, tiled) = scan_gap(caches, &name, start, end);
                fcc.gap_payloads += n;
                fcc.gap_bytes_tiled += tiled;
            }
        }
    }
}

/// Walk `[start, end)` of cache `name` as back-to-back compressed payloads
/// and return how many parse (and decompress) and how many bytes they tile.
fn scan_gap(caches: &TextureFileCaches, name: &str, start: u64, end: u64) -> (usize, u64) {
    let Some(len) = end.checked_sub(start).and_then(|l| usize::try_from(l).ok()) else {
        return (0, 0);
    };
    if len > bulkdata::MAX_BULK_SIZE {
        return (0, 0);
    }
    let probe = BulkDataRecord {
        flags: bulkdata::flags::STORE_IN_SEPARATE_FILE,
        element_count: 0,
        size_on_disk: i32::try_from(len).unwrap_or(0),
        offset_in_file: i32::try_from(start).unwrap_or(-1),
        header_offset: 0,
    };
    let Ok(bytes) = caches.read_stored(name, &probe) else {
        return (0, 0);
    };
    let (mut pos, mut n) = (0usize, 0usize);
    while pos < bytes.len() {
        let rest = bytes.get(pos..).unwrap_or(&[]);
        let Ok(layout) = bulkdata::parse_compressed(rest) else {
            break;
        };
        let total = layout.total_len();
        let Some(chunk) = rest.get(..total) else {
            break;
        };
        if bulkdata::decompress_lzo(chunk, layout.uncompressed_size as usize).is_err() {
            break;
        }
        n += 1;
        pos += total;
    }
    (n, pos as u64)
}
