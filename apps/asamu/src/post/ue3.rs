//! UE3 atmosphere rules, render-free: post-process settings with their
//! override and time-blend rules, post-process volume selection, the uber
//! post-process effect's parameters, the customizable tonemapper, the
//! colour-grading LUT and the exponential height fog constants.
//!
//! Behaviour recovered from the original executable (local reading of the
//! native code and of the GLSL in the shipped global shader cache; never
//! copied) and documented in our own words, with confidence labels, in
//! `docs/reverse-engineering/POST_FOG_SKY.md`. Every number below that is
//! not a property value of the level comes from that page (native code or
//! class defaults); our own approximation constants live in `post.rs` and
//! are labelled there.

use asamu_assets::scene::{FogComponentInfo, PostProcessVolumeInfo, UeProps};
use asamu_core::glam::Vec3;

// ------------------------------------------------------------ colours

/// UE3 `FColor` → `FLinearColor` for the colour properties used here: each
/// byte through `pow(c / 255, 2.2)`, rounded to `f32` (CONFIRMED: the
/// constructor reads `FLinearColor::PowOneOver255Table`, whose 256 stored
/// entries all equal that value; see `POST_FOG_SKY.md`).
#[must_use]
pub fn color8_to_linear(c: [u8; 4]) -> [f32; 3] {
    #[allow(clippy::cast_possible_truncation)]
    let f = |b: u8| (f64::from(b) / 255.0).powf(2.2) as f32;
    [f(c[0]), f(c[1]), f(c[2])]
}

/// Rec. 601-style luminance weights the uber/LUT shaders use for
/// desaturation (native constants 0.30, 0.59, 0.11; CONFIRMED).
pub const LUMINANCE_WEIGHTS: [f32; 3] = [0.30, 0.59, 0.11];

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp3(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    a + (b - a) * t
}

// ------------------------------------------------------------ settings

/// The `bOverride_*` flags of `FPostProcessSettings` that this runtime uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Overrides {
    pub enable_bloom: bool,
    pub enable_dof: bool,
    pub enable_motion_blur: bool,
    pub enable_scene_effect: bool,
    pub allow_ambient_occlusion: bool,
    pub bloom_scale: bool,
    pub bloom_threshold: bool,
    pub bloom_tint: bool,
    pub bloom_screen_blend_threshold: bool,
    pub bloom_interpolation_duration: bool,
    pub dof_interpolation_duration: bool,
    pub motion_blur_amount: bool,
    pub motion_blur_interpolation_duration: bool,
    pub scene_desaturation: bool,
    pub scene_colorize: bool,
    pub scene_tonemapper_scale: bool,
    pub scene_image_grain_scale: bool,
    pub scene_highlights: bool,
    pub scene_midtones: bool,
    pub scene_shadows: bool,
    pub scene_interpolation_duration: bool,
    pub scene_color_grading_lut: bool,
}

/// The parts of UE3 `FPostProcessSettings` this runtime uses (bloom,
/// scene colour transform, tonemapper scale, LUT and the group toggles and
/// blend durations).
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct PostSettings {
    pub enable_bloom: bool,
    pub enable_dof: bool,
    pub enable_motion_blur: bool,
    pub enable_scene_effect: bool,
    pub allow_ambient_occlusion: bool,
    pub bloom_scale: f32,
    pub bloom_threshold: f32,
    /// `Bloom_Tint` as linear RGB.
    pub bloom_tint: Vec3,
    pub bloom_screen_blend_threshold: f32,
    pub bloom_interpolation_duration: f32,
    pub dof_interpolation_duration: f32,
    pub motion_blur_amount: f32,
    pub motion_blur_interpolation_duration: f32,
    pub scene_desaturation: f32,
    pub scene_colorize: Vec3,
    pub scene_tonemapper_scale: f32,
    pub scene_image_grain_scale: f32,
    pub scene_highlights: Vec3,
    pub scene_midtones: Vec3,
    pub scene_shadows: Vec3,
    pub scene_interpolation_duration: f32,
    /// `ColorGrading_LookupTable` texture path.
    pub color_grading_lut: Option<String>,
    pub ov: Overrides,
}

impl Default for PostSettings {
    /// The struct defaults of `Engine.PostProcessSettings` (class data of
    /// `Engine.u`, CONFIRMED with `asamu-inspect class`).
    fn default() -> Self {
        Self {
            enable_bloom: true,
            enable_dof: false,
            enable_motion_blur: true,
            enable_scene_effect: true,
            allow_ambient_occlusion: true,
            bloom_scale: 1.0,
            bloom_threshold: 1.0,
            bloom_tint: Vec3::ONE,
            bloom_screen_blend_threshold: 10.0,
            bloom_interpolation_duration: 1.0,
            dof_interpolation_duration: 1.0,
            motion_blur_amount: 0.5,
            motion_blur_interpolation_duration: 1.0,
            scene_desaturation: 0.0,
            scene_colorize: Vec3::ONE,
            scene_tonemapper_scale: 1.0,
            scene_image_grain_scale: 0.0,
            scene_highlights: Vec3::ONE,
            scene_midtones: Vec3::ONE,
            scene_shadows: Vec3::ZERO,
            scene_interpolation_duration: 1.0,
            color_grading_lut: None,
            ov: Overrides {
                enable_bloom: true,
                enable_dof: true,
                enable_motion_blur: true,
                enable_scene_effect: true,
                allow_ambient_occlusion: true,
                bloom_scale: true,
                bloom_threshold: true,
                bloom_tint: true,
                bloom_screen_blend_threshold: true,
                bloom_interpolation_duration: true,
                dof_interpolation_duration: true,
                motion_blur_amount: false,
                motion_blur_interpolation_duration: false,
                scene_desaturation: true,
                scene_colorize: false,
                scene_tonemapper_scale: false,
                scene_image_grain_scale: false,
                scene_highlights: true,
                scene_midtones: true,
                scene_shadows: true,
                scene_interpolation_duration: true,
                scene_color_grading_lut: false,
            },
        }
    }
}

impl PostSettings {
    /// Reads a settings struct from scene values; anything missing keeps
    /// the struct default.
    #[must_use]
    pub fn from_props(p: &UeProps) -> Self {
        let d = Self::default();
        let f = |n: &str, v: f32| p.f32(n).unwrap_or(v);
        let b = |n: &str, v: bool| p.bool(n).unwrap_or(v);
        let v3 = |n: &str, v: Vec3| p.vec3(n).unwrap_or(v);
        let o = d.ov;
        Self {
            enable_bloom: b("bEnableBloom", d.enable_bloom),
            enable_dof: b("bEnableDOF", d.enable_dof),
            enable_motion_blur: b("bEnableMotionBlur", d.enable_motion_blur),
            enable_scene_effect: b("bEnableSceneEffect", d.enable_scene_effect),
            allow_ambient_occlusion: b("bAllowAmbientOcclusion", d.allow_ambient_occlusion),
            bloom_scale: f("Bloom_Scale", d.bloom_scale),
            bloom_threshold: f("Bloom_Threshold", d.bloom_threshold),
            bloom_tint: p
                .color8("Bloom_Tint")
                .map_or(d.bloom_tint, |c| Vec3::from_array(color8_to_linear(c))),
            bloom_screen_blend_threshold: f(
                "Bloom_ScreenBlendThreshold",
                d.bloom_screen_blend_threshold,
            ),
            bloom_interpolation_duration: f(
                "Bloom_InterpolationDuration",
                d.bloom_interpolation_duration,
            ),
            dof_interpolation_duration: f(
                "DOF_InterpolationDuration",
                d.dof_interpolation_duration,
            ),
            motion_blur_amount: f("MotionBlur_Amount", d.motion_blur_amount),
            motion_blur_interpolation_duration: f(
                "MotionBlur_InterpolationDuration",
                d.motion_blur_interpolation_duration,
            ),
            scene_desaturation: f("Scene_Desaturation", d.scene_desaturation),
            scene_colorize: v3("Scene_Colorize", d.scene_colorize),
            scene_tonemapper_scale: f("Scene_TonemapperScale", d.scene_tonemapper_scale),
            scene_image_grain_scale: f("Scene_ImageGrainScale", d.scene_image_grain_scale),
            scene_highlights: v3("Scene_HighLights", d.scene_highlights),
            scene_midtones: v3("Scene_MidTones", d.scene_midtones),
            scene_shadows: v3("Scene_Shadows", d.scene_shadows),
            scene_interpolation_duration: f(
                "Scene_InterpolationDuration",
                d.scene_interpolation_duration,
            ),
            color_grading_lut: p
                .text("ColorGrading_LookupTable")
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            ov: Overrides {
                enable_bloom: b("bOverride_EnableBloom", o.enable_bloom),
                enable_dof: b("bOverride_EnableDOF", o.enable_dof),
                enable_motion_blur: b("bOverride_EnableMotionBlur", o.enable_motion_blur),
                enable_scene_effect: b("bOverride_EnableSceneEffect", o.enable_scene_effect),
                allow_ambient_occlusion: b(
                    "bOverride_AllowAmbientOcclusion",
                    o.allow_ambient_occlusion,
                ),
                bloom_scale: b("bOverride_Bloom_Scale", o.bloom_scale),
                bloom_threshold: b("bOverride_Bloom_Threshold", o.bloom_threshold),
                bloom_tint: b("bOverride_Bloom_Tint", o.bloom_tint),
                bloom_screen_blend_threshold: b(
                    "bOverride_Bloom_ScreenBlendThreshold",
                    o.bloom_screen_blend_threshold,
                ),
                bloom_interpolation_duration: b(
                    "bOverride_Bloom_InterpolationDuration",
                    o.bloom_interpolation_duration,
                ),
                dof_interpolation_duration: b(
                    "bOverride_DOF_InterpolationDuration",
                    o.dof_interpolation_duration,
                ),
                motion_blur_amount: b("bOverride_MotionBlur_Amount", o.motion_blur_amount),
                motion_blur_interpolation_duration: b(
                    "bOverride_MotionBlur_InterpolationDuration",
                    o.motion_blur_interpolation_duration,
                ),
                scene_desaturation: b("bOverride_Scene_Desaturation", o.scene_desaturation),
                scene_colorize: b("bOverride_Scene_Colorize", o.scene_colorize),
                scene_tonemapper_scale: b(
                    "bOverride_Scene_TonemapperScale",
                    o.scene_tonemapper_scale,
                ),
                scene_image_grain_scale: b(
                    "bOverride_Scene_ImageGrainScale",
                    o.scene_image_grain_scale,
                ),
                scene_highlights: b("bOverride_Scene_HighLights", o.scene_highlights),
                scene_midtones: b("bOverride_Scene_MidTones", o.scene_midtones),
                scene_shadows: b("bOverride_Scene_Shadows", o.scene_shadows),
                scene_interpolation_duration: b(
                    "bOverride_Scene_InterpolationDuration",
                    o.scene_interpolation_duration,
                ),
                scene_color_grading_lut: b(
                    "bOverride_Scene_ColorGradingLUT",
                    o.scene_color_grading_lut,
                ),
            },
        }
    }

    /// UE3 `FPostProcessSettings::OverrideSettingsFor(dst, alpha)`: `self`
    /// (a volume's settings) overrides `dst` (the world defaults). A group
    /// toggle is copied when its override flag is set; then, only while the
    /// destination's group is enabled, every value whose override flag is
    /// set moves towards `self` by `alpha` and its override flag is set in
    /// `dst`. The LUT reference is copied, not blended. (CONFIRMED: native
    /// code.)
    pub fn override_into(&self, dst: &mut PostSettings, alpha: f32) {
        if alpha <= 0.0 {
            return;
        }
        let s = self;
        let o = s.ov;
        if o.enable_bloom {
            dst.enable_bloom = s.enable_bloom;
        }
        if dst.enable_bloom {
            if o.bloom_scale {
                dst.bloom_scale = lerp(dst.bloom_scale, s.bloom_scale, alpha);
                dst.ov.bloom_scale = true;
            }
            if o.bloom_threshold {
                dst.bloom_threshold = lerp(dst.bloom_threshold, s.bloom_threshold, alpha);
                dst.ov.bloom_threshold = true;
            }
            if o.bloom_screen_blend_threshold {
                dst.bloom_screen_blend_threshold = lerp(
                    dst.bloom_screen_blend_threshold,
                    s.bloom_screen_blend_threshold,
                    alpha,
                );
                dst.ov.bloom_screen_blend_threshold = true;
            }
            if o.bloom_interpolation_duration {
                dst.bloom_interpolation_duration = lerp(
                    dst.bloom_interpolation_duration,
                    s.bloom_interpolation_duration,
                    alpha,
                );
                dst.ov.bloom_interpolation_duration = true;
            }
            if o.bloom_tint {
                dst.bloom_tint = lerp3(dst.bloom_tint, s.bloom_tint, alpha);
                dst.ov.bloom_tint = true;
            }
        }
        if o.enable_dof {
            dst.enable_dof = s.enable_dof;
        }
        if dst.enable_dof && o.dof_interpolation_duration {
            dst.dof_interpolation_duration = lerp(
                dst.dof_interpolation_duration,
                s.dof_interpolation_duration,
                alpha,
            );
            dst.ov.dof_interpolation_duration = true;
        }
        if o.enable_motion_blur {
            dst.enable_motion_blur = s.enable_motion_blur;
        }
        if dst.enable_motion_blur {
            if o.motion_blur_amount {
                dst.motion_blur_amount = lerp(dst.motion_blur_amount, s.motion_blur_amount, alpha);
                dst.ov.motion_blur_amount = true;
            }
            if o.motion_blur_interpolation_duration {
                dst.motion_blur_interpolation_duration = lerp(
                    dst.motion_blur_interpolation_duration,
                    s.motion_blur_interpolation_duration,
                    alpha,
                );
                dst.ov.motion_blur_interpolation_duration = true;
            }
        }
        if o.enable_scene_effect {
            dst.enable_scene_effect = s.enable_scene_effect;
        }
        if dst.enable_scene_effect {
            if o.scene_highlights {
                dst.scene_highlights = lerp3(dst.scene_highlights, s.scene_highlights, alpha);
                dst.ov.scene_highlights = true;
            }
            if o.scene_midtones {
                dst.scene_midtones = lerp3(dst.scene_midtones, s.scene_midtones, alpha);
                dst.ov.scene_midtones = true;
            }
            if o.scene_shadows {
                dst.scene_shadows = lerp3(dst.scene_shadows, s.scene_shadows, alpha);
                dst.ov.scene_shadows = true;
            }
            if o.scene_desaturation {
                dst.scene_desaturation = lerp(dst.scene_desaturation, s.scene_desaturation, alpha);
                dst.ov.scene_desaturation = true;
            }
            if o.scene_colorize {
                dst.scene_colorize = lerp3(dst.scene_colorize, s.scene_colorize, alpha);
                dst.ov.scene_colorize = true;
            }
            if o.scene_interpolation_duration {
                dst.scene_interpolation_duration = lerp(
                    dst.scene_interpolation_duration,
                    s.scene_interpolation_duration,
                    alpha,
                );
                dst.ov.scene_interpolation_duration = true;
            }
            if o.scene_tonemapper_scale {
                dst.scene_tonemapper_scale =
                    lerp(dst.scene_tonemapper_scale, s.scene_tonemapper_scale, alpha);
                dst.ov.scene_tonemapper_scale = true;
            }
            if o.scene_image_grain_scale {
                dst.scene_image_grain_scale = lerp(
                    dst.scene_image_grain_scale,
                    s.scene_image_grain_scale,
                    alpha,
                );
                dst.ov.scene_image_grain_scale = true;
            }
        }
        if o.allow_ambient_occlusion {
            dst.allow_ambient_occlusion = s.allow_ambient_occlusion;
        }
        if o.scene_color_grading_lut {
            dst.color_grading_lut.clone_from(&s.color_grading_lut);
            dst.ov.scene_color_grading_lut = true;
        }
    }
}

// ------------------------------------------------------------ volumes

/// True when `p` (UE3, UU) is inside any of the volume's convex hulls
/// (every plane gives `n·p − w ≤ 0`). A volume without hull planes falls
/// back to its bounding box (UE3 tests the brush itself: `AVolume::
/// Encompasses`; the shipped post-process volumes are all single boxes).
#[must_use]
pub fn volume_contains(v: &PostProcessVolumeInfo, p: Vec3) -> bool {
    if !v.hulls.is_empty() {
        return v.hulls.iter().any(|hull| {
            hull.iter()
                .all(|q| q[0] * p.x + q[1] * p.y + q[2] * p.z - q[3] <= 0.0)
        });
    }
    v.bounds
        .is_some_and(|(lo, hi)| p.cmpge(lo).all() && p.cmple(hi).all())
}

/// The volume UE3 applies at `p`: the first enabled volume that contains
/// the point, in priority order (`AWorldInfo::GetPostProcessSettings`;
/// only one volume applies, there is no distance blending in UE3 — the
/// transition is a time blend, [`PostBlender`]). Returns an index into
/// `volumes` (the priority-sorted list).
#[must_use]
pub fn select_volume(volumes: &[&PostProcessVolumeInfo], p: Vec3) -> Option<usize> {
    volumes
        .iter()
        .position(|v| v.enabled && volume_contains(v, p))
}

/// The settings UE3 wants at `p` before time blending: the world defaults,
/// overridden by the selected volume (alpha 1).
#[must_use]
pub fn desired_settings(
    world: &PostSettings,
    volumes: &[(&PostProcessVolumeInfo, PostSettings)],
    p: Vec3,
) -> (PostSettings, Option<usize>) {
    let mut out = world.clone();
    let refs: Vec<&PostProcessVolumeInfo> = volumes.iter().map(|(v, _)| *v).collect();
    let selected = select_volume(&refs, p);
    if let Some(i) = selected
        && let Some((_, s)) = volumes.get(i)
    {
        s.override_into(&mut out, 1.0);
    }
    (out, selected)
}

/// Blend factor of one UE3 time-blend step: `delta` seconds since the last
/// step, `elapsed` seconds of the blend already done before that step.
#[must_use]
pub fn blend_factor(duration: f32, elapsed: f32, delta: f32) -> f32 {
    let remaining = (duration - elapsed).max(0.0);
    if delta < remaining {
        (delta / remaining).clamp(0.0, 1.0)
    } else {
        1.0
    }
}

/// UE3's time blend of the post-process settings (`ULocalPlayer::
/// UpdatePostProcessSettings` / `UpdatePPSetting`, CONFIRMED: native code):
/// when the applied volume changes the blend restarts; every frame each
/// enabled group of the current settings moves towards the desired values
/// by [`blend_factor`] with that group's `*_InterpolationDuration` (a
/// linear blend over the duration); group toggles switch at once.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PostBlender {
    current: Option<PostSettings>,
    volume: Option<Option<usize>>,
    blend_start: f32,
    last_blend: f32,
}

impl PostBlender {
    /// Forgets the current settings (the next update snaps).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The blended settings, if any update ran.
    #[cfg(test)]
    #[must_use]
    pub fn current(&self) -> Option<&PostSettings> {
        self.current.as_ref()
    }

    /// One frame at real time `now` (seconds).
    pub fn update(
        &mut self,
        desired: &PostSettings,
        volume: Option<usize>,
        now: f32,
    ) -> &PostSettings {
        if self.volume != Some(volume) {
            self.volume = Some(volume);
            self.blend_start = now;
        }
        let delta = (now - self.last_blend).max(0.0);
        let elapsed = (self.last_blend - self.blend_start).max(0.0);
        self.last_blend = now;
        let cur = self.current.get_or_insert_with(|| desired.clone());
        let d = desired;
        cur.enable_bloom = d.enable_bloom;
        cur.enable_dof = d.enable_dof;
        cur.enable_motion_blur = d.enable_motion_blur;
        cur.enable_scene_effect = d.enable_scene_effect;
        cur.allow_ambient_occlusion = d.allow_ambient_occlusion;
        if d.enable_bloom {
            let t = blend_factor(d.bloom_interpolation_duration, elapsed, delta);
            cur.bloom_scale = lerp(cur.bloom_scale, d.bloom_scale, t);
            cur.bloom_threshold = lerp(cur.bloom_threshold, d.bloom_threshold, t);
            cur.bloom_screen_blend_threshold = lerp(
                cur.bloom_screen_blend_threshold,
                d.bloom_screen_blend_threshold,
                t,
            );
            cur.bloom_interpolation_duration = lerp(
                cur.bloom_interpolation_duration,
                d.bloom_interpolation_duration,
                t,
            );
            cur.bloom_tint = lerp3(cur.bloom_tint, d.bloom_tint, t);
            cur.ov.bloom_scale = d.ov.bloom_scale;
            cur.ov.bloom_threshold = d.ov.bloom_threshold;
            cur.ov.bloom_screen_blend_threshold = d.ov.bloom_screen_blend_threshold;
            cur.ov.bloom_interpolation_duration = d.ov.bloom_interpolation_duration;
            cur.ov.bloom_tint = d.ov.bloom_tint;
        }
        if d.enable_dof {
            let t = blend_factor(d.dof_interpolation_duration, elapsed, delta);
            cur.dof_interpolation_duration = lerp(
                cur.dof_interpolation_duration,
                d.dof_interpolation_duration,
                t,
            );
        }
        if d.enable_motion_blur {
            let t = blend_factor(d.motion_blur_interpolation_duration, elapsed, delta);
            cur.motion_blur_amount = lerp(cur.motion_blur_amount, d.motion_blur_amount, t);
            cur.motion_blur_interpolation_duration = lerp(
                cur.motion_blur_interpolation_duration,
                d.motion_blur_interpolation_duration,
                t,
            );
            cur.ov.motion_blur_amount = d.ov.motion_blur_amount;
        }
        if d.enable_scene_effect {
            let t = blend_factor(d.scene_interpolation_duration, elapsed, delta);
            cur.scene_desaturation = lerp(cur.scene_desaturation, d.scene_desaturation, t);
            cur.scene_colorize = lerp3(cur.scene_colorize, d.scene_colorize, t);
            cur.scene_tonemapper_scale =
                lerp(cur.scene_tonemapper_scale, d.scene_tonemapper_scale, t);
            cur.scene_image_grain_scale =
                lerp(cur.scene_image_grain_scale, d.scene_image_grain_scale, t);
            cur.scene_highlights = lerp3(cur.scene_highlights, d.scene_highlights, t);
            cur.scene_midtones = lerp3(cur.scene_midtones, d.scene_midtones, t);
            cur.scene_shadows = lerp3(cur.scene_shadows, d.scene_shadows, t);
            cur.scene_interpolation_duration = lerp(
                cur.scene_interpolation_duration,
                d.scene_interpolation_duration,
                t,
            );
            cur.ov.scene_desaturation = d.ov.scene_desaturation;
            cur.ov.scene_colorize = d.ov.scene_colorize;
            cur.ov.scene_tonemapper_scale = d.ov.scene_tonemapper_scale;
            cur.ov.scene_image_grain_scale = d.ov.scene_image_grain_scale;
            cur.ov.scene_highlights = d.ov.scene_highlights;
            cur.ov.scene_midtones = d.ov.scene_midtones;
            cur.ov.scene_shadows = d.ov.scene_shadows;
        }
        // The LUT switches with the settings (UE3 blends LUT textures over
        // time in its LUT blender; one LUT per shipped level, so not
        // modelled: TENTATIVE).
        cur.color_grading_lut.clone_from(&d.color_grading_lut);
        cur.ov.scene_color_grading_lut = d.ov.scene_color_grading_lut;
        cur
    }
}

// ------------------------------------------------------------ uber effect

/// UE3 `ETonemapperType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TonemapperType {
    /// `Tonemapper_Off`: gamma only.
    Off,
    /// `Tonemapper_Filmic` (no shipped chain uses it; its shader variant was
    /// not read: UNKNOWN, approximated by the customizable curve without
    /// the toe).
    Filmic,
    /// `Tonemapper_Customizable` (what the shipped chain uses).
    Customizable,
}

/// `Engine.UberPostProcessEffect` properties (class defaults of `Engine.u`
/// merged with the chain's values; CONFIRMED with `asamu-inspect`).
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct UberEffect {
    pub show_in_game: bool,
    pub use_world_settings: bool,
    pub tonemapper: TonemapperType,
    pub tonemapper_range: f32,
    pub tonemapper_toe_factor: f32,
    pub tonemapper_scale: f32,
    pub bloom_scale: f32,
    pub bloom_threshold: f32,
    pub bloom_tint: Vec3,
    pub bloom_screen_blend_threshold: f32,
    pub scene_shadows: Vec3,
    pub scene_highlights: Vec3,
    pub scene_midtones: Vec3,
    pub scene_desaturation: f32,
    pub scene_colorize: Vec3,
    pub scene_image_grain_scale: f32,
}

impl Default for UberEffect {
    /// The inherited class defaults of `Engine.UberPostProcessEffect`.
    fn default() -> Self {
        Self {
            show_in_game: true,
            use_world_settings: false,
            tonemapper: TonemapperType::Off,
            tonemapper_range: 8.0,
            tonemapper_toe_factor: 1.0,
            tonemapper_scale: 1.0,
            bloom_scale: 1.0,
            bloom_threshold: 1.0,
            bloom_tint: Vec3::ONE,
            bloom_screen_blend_threshold: 10.0,
            scene_shadows: Vec3::new(0.0, 0.0, -0.003),
            scene_highlights: Vec3::splat(0.8),
            scene_midtones: Vec3::splat(1.3),
            scene_desaturation: 0.4,
            scene_colorize: Vec3::ONE,
            scene_image_grain_scale: 0.02,
        }
    }
}

impl UberEffect {
    /// Reads the effect from a chain entry's values (missing → defaults).
    #[must_use]
    pub fn from_props(p: &UeProps) -> Self {
        let d = Self::default();
        let f = |n: &str, v: f32| p.f32(n).unwrap_or(v);
        let v3 = |n: &str, v: Vec3| p.vec3(n).unwrap_or(v);
        let tonemapper = match p.text("TonemapperType") {
            Some(t) if t.eq_ignore_ascii_case("Tonemapper_Customizable") => {
                TonemapperType::Customizable
            }
            Some(t) if t.eq_ignore_ascii_case("Tonemapper_Filmic") => TonemapperType::Filmic,
            _ => d.tonemapper,
        };
        Self {
            show_in_game: p.bool("bShowInGame").unwrap_or(d.show_in_game),
            use_world_settings: p.bool("bUseWorldSettings").unwrap_or(d.use_world_settings),
            tonemapper,
            tonemapper_range: f("TonemapperRange", d.tonemapper_range),
            tonemapper_toe_factor: f("TonemapperToeFactor", d.tonemapper_toe_factor),
            tonemapper_scale: f("TonemapperScale", d.tonemapper_scale),
            bloom_scale: f("BloomScale", d.bloom_scale),
            bloom_threshold: f("BloomThreshold", d.bloom_threshold),
            bloom_tint: p
                .color8("BloomTint")
                .map_or(d.bloom_tint, |c| Vec3::from_array(color8_to_linear(c))),
            bloom_screen_blend_threshold: f(
                "BloomScreenBlendThreshold",
                d.bloom_screen_blend_threshold,
            ),
            scene_shadows: v3("SceneShadows", d.scene_shadows),
            scene_highlights: v3("SceneHighLights", d.scene_highlights),
            scene_midtones: v3("SceneMidTones", d.scene_midtones),
            scene_desaturation: f("SceneDesaturation", d.scene_desaturation),
            scene_colorize: v3("SceneColorize", d.scene_colorize),
            scene_image_grain_scale: f("SceneImageGrainScale", d.scene_image_grain_scale),
        }
    }
}

/// The colour transform the LUT blender bakes into the final LUT
/// (`ColorTransformMaterialProperties`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorTransform {
    pub shadows: Vec3,
    pub highlights: Vec3,
    pub midtones: Vec3,
    pub desaturation: f32,
    pub colorize: Vec3,
}

impl ColorTransform {
    /// No change.
    pub const IDENTITY: Self = Self {
        shadows: Vec3::ZERO,
        highlights: Vec3::ONE,
        midtones: Vec3::ONE,
        desaturation: 0.0,
        colorize: Vec3::ONE,
    };

    /// One gamma-space colour through the transform (the LUT blender's
    /// per-entry maths, CONFIRMED from its GLSL and parameter setters):
    /// subtract shadows and clamp, divide by highlights, raise to the
    /// midtones exponent, desaturate towards luminance, multiply by
    /// colorize. The view's colour scale / overlay (camera fades) are 1 / 0
    /// here, and the final display-gamma exponent is 1 for a display gamma
    /// of 2.2 (STRONG).
    #[must_use]
    pub fn apply(&self, c: Vec3) -> Vec3 {
        let inv_h = Vec3::new(
            inv_or_big(self.highlights.x),
            inv_or_big(self.highlights.y),
            inv_or_big(self.highlights.z),
        );
        let c = (c - self.shadows).clamp(Vec3::ZERO, Vec3::ONE) * inv_h;
        let c = Vec3::new(
            c.x.max(0.0).powf(self.midtones.x),
            c.y.max(0.0).powf(self.midtones.y),
            c.z.max(0.0).powf(self.midtones.z),
        );
        let lum = c.dot(Vec3::from_array(LUMINANCE_WEIGHTS) * self.desaturation);
        let c = c * (1.0 - self.desaturation) + Vec3::splat(lum);
        (c * self.colorize).max(Vec3::ZERO)
    }
}

fn inv_or_big(x: f32) -> f32 {
    if x.abs() > 1e-6 { 1.0 / x } else { 1e6 }
}

/// Constants of the customizable tonemapper (CONFIRMED: native setter and
/// blend-shader GLSL; the variant choice for `Tonemapper_Customizable` is
/// STRONG, see the doc).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tonemapper {
    /// Curve shoulder `A` (= 0.22 / scale).
    pub a: f32,
    /// Curve gain `B` (= (range + A) / range).
    pub b: f32,
    /// Where the toe (gamma) part hands over to the curve.
    pub crossover: f32,
    /// `TonemapperScale` (≥ 1e-6).
    pub scale: f32,
    /// `TonemapperToeFactor` (0..1).
    pub toe: f32,
    pub kind: TonemapperType,
}

impl Tonemapper {
    /// Constants for a range, toe factor and scale.
    #[must_use]
    pub fn new(kind: TonemapperType, range: f32, toe: f32, scale: f32) -> Self {
        let scale = scale.max(1e-6);
        let a = 0.22 / scale;
        let range = if range.abs() > 1e-6 { range } else { 1e-6 };
        let b = 1.0 / (range / (range + a));
        let crossover = ((a * b) / scale).max(0.0).sqrt() - a;
        Self {
            a,
            b,
            crossover,
            scale,
            toe: toe.clamp(0.0, 1.0),
            kind,
        }
    }

    /// Linear HDR channel → display (gamma-space) value in 0..1.
    #[cfg(test)]
    #[must_use]
    pub fn map(&self, x: f32) -> f32 {
        let x = x.max(0.0);
        match self.kind {
            TonemapperType::Off => x.powf(1.0 / 2.2).clamp(0.0, 1.0),
            TonemapperType::Filmic => (self.b * x / (x + self.a)).clamp(0.0, 1.0),
            TonemapperType::Customizable => {
                let curve = self.b * x / (x + self.a).abs().max(1e-12);
                let toe = (x * self.scale).powf(1.0 / 2.2);
                let k = ((x - self.crossover) * 10000.0).clamp(0.0, 1.0);
                let m = toe + (curve - toe) * k;
                (m + (curve - m) * self.toe).clamp(0.0, 1.0)
            }
        }
    }
}

/// Everything the uber post-process effect uses this frame.
#[derive(Debug, Clone, PartialEq)]
pub struct UberParams {
    pub bloom_scale: f32,
    pub bloom_threshold: f32,
    pub bloom_tint: Vec3,
    pub bloom_screen_blend_threshold: f32,
    pub transform: ColorTransform,
    pub tonemapper: Tonemapper,
    pub image_grain_scale: f32,
    pub color_grading_lut: Option<String>,
    pub enable_motion_blur: bool,
    pub motion_blur_amount: f32,
}

/// The uber effect's parameters for the current settings: with
/// `bUseWorldSettings` each value comes from the settings when its
/// override flag is set, else from the effect itself; a settings block that
/// overrides bloom off zeroes the bloom scale; scene effects off gives the
/// identity transform (`FDOFAndBloomPostProcessSceneProxy` /
/// `FUberPostProcessSceneProxy` constructors, CONFIRMED).
#[must_use]
pub fn uber_params(effect: &UberEffect, settings: Option<&PostSettings>) -> UberParams {
    let s = settings.filter(|_| effect.use_world_settings);
    let pick = |ov: fn(&Overrides) -> bool, sv: f32, ev: f32| match s {
        Some(s) if ov(&s.ov) => sv,
        _ => ev,
    };
    let pick3 = |ov: fn(&Overrides) -> bool, sv: Vec3, ev: Vec3| match s {
        Some(s) if ov(&s.ov) => sv,
        _ => ev,
    };
    let d = PostSettings::default();
    let st = s.unwrap_or(&d);
    let mut bloom_scale = pick(|o| o.bloom_scale, st.bloom_scale, effect.bloom_scale);
    if s.is_some_and(|s| s.ov.enable_bloom && !s.enable_bloom) {
        bloom_scale = 0.0;
    }
    let mut transform = ColorTransform {
        shadows: pick3(|o| o.scene_shadows, st.scene_shadows, effect.scene_shadows),
        highlights: pick3(
            |o| o.scene_highlights,
            st.scene_highlights,
            effect.scene_highlights,
        ),
        midtones: pick3(
            |o| o.scene_midtones,
            st.scene_midtones,
            effect.scene_midtones,
        ),
        desaturation: pick(
            |o| o.scene_desaturation,
            st.scene_desaturation,
            effect.scene_desaturation,
        )
        .clamp(0.0, 1.0),
        colorize: pick3(
            |o| o.scene_colorize,
            st.scene_colorize,
            effect.scene_colorize,
        ),
    };
    if s.is_some_and(|s| !s.enable_scene_effect) {
        transform = ColorTransform::IDENTITY;
    }
    let scale = pick(
        |o| o.scene_tonemapper_scale,
        st.scene_tonemapper_scale,
        effect.tonemapper_scale,
    );
    UberParams {
        bloom_scale,
        bloom_threshold: pick(
            |o| o.bloom_threshold,
            st.bloom_threshold,
            effect.bloom_threshold,
        ),
        bloom_tint: pick3(|o| o.bloom_tint, st.bloom_tint, effect.bloom_tint),
        bloom_screen_blend_threshold: pick(
            |o| o.bloom_screen_blend_threshold,
            st.bloom_screen_blend_threshold,
            effect.bloom_screen_blend_threshold,
        ),
        transform,
        tonemapper: Tonemapper::new(
            effect.tonemapper,
            effect.tonemapper_range,
            effect.tonemapper_toe_factor,
            scale,
        ),
        image_grain_scale: pick(
            |o| o.scene_image_grain_scale,
            st.scene_image_grain_scale,
            effect.scene_image_grain_scale,
        ),
        color_grading_lut: s
            .filter(|s| s.ov.scene_color_grading_lut)
            .and_then(|s| s.color_grading_lut.clone()),
        enable_motion_blur: s.is_none_or(|s| s.enable_motion_blur),
        motion_blur_amount: s.map_or(0.0, |s| s.motion_blur_amount),
    }
}

/// The uber blend's bloom step (CONFIRMED from its GLSL): the blurred
/// bright parts, multiplied by `Bloom_Tint · Bloom_Scale`, are added to the
/// scene, faded where the scene itself is bright by `saturate(2^(−3 ·
/// luminance) · Bloom_ScreenBlendThreshold)` (luminance of the scene
/// clamped to 65503, weights 0.30 / 0.59 / 0.11). UE3 scene units.
/// Reference for the WGSL (`uber.wgsl`).
#[cfg(test)]
#[must_use]
pub fn bloom_blend(
    scene: Vec3,
    blurred: Vec3,
    tint_scale: Vec3,
    screen_blend_threshold: f32,
) -> Vec3 {
    let lum = scene
        .min(Vec3::splat(65503.0))
        .dot(Vec3::from_array(LUMINANCE_WEIGHTS));
    let fade = ((-3.0 * lum).exp2() * screen_blend_threshold).clamp(0.0, 1.0);
    scene + blurred * tint_scale * fade
}

// ------------------------------------------------------------ LUT

/// Entries per axis of a UE3 colour-grading LUT.
pub const LUT_SIZE: usize = 16;

/// A 16³ colour LUT laid out like UE3's 256 × 16 LUT textures: texel
/// `(b·16 + r, g)`; RGB in 0..1 (gamma space).
#[derive(Debug, Clone, PartialEq)]
pub struct Lut {
    /// `LUT_SIZE³` entries, index `(g·16 + b)·16 + r` (row-major texture).
    pub texels: Vec<[f32; 3]>,
}

impl Lut {
    /// The neutral LUT (each entry its own coordinate).
    #[must_use]
    pub fn neutral() -> Self {
        let mut texels = Vec::with_capacity(LUT_SIZE * LUT_SIZE * LUT_SIZE);
        for g in 0..LUT_SIZE {
            for b in 0..LUT_SIZE {
                for r in 0..LUT_SIZE {
                    texels.push([node(r), node(g), node(b)]);
                }
            }
        }
        Self { texels }
    }

    /// A LUT from a 256 × 16 texture's RGBA8 texels (rows top to bottom),
    /// `None` when the size is wrong.
    #[must_use]
    pub fn from_rgba8(width: usize, height: usize, rgba: &[u8]) -> Option<Self> {
        let n = LUT_SIZE;
        if width != n * n || height != n || rgba.len() != width * height * 4 {
            return None;
        }
        let texels = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                [
                    f32::from(p[0]) / 255.0,
                    f32::from(p[1]) / 255.0,
                    f32::from(p[2]) / 255.0,
                ]
            })
            .collect();
        Some(Self { texels })
    }

    /// The entry at grid node `(r, g, b)`.
    #[must_use]
    pub fn at(&self, r: usize, g: usize, b: usize) -> Vec3 {
        let i = (g * LUT_SIZE + b) * LUT_SIZE + r;
        self.texels
            .get(i)
            .map_or(Vec3::ZERO, |t| Vec3::from_array(*t))
    }

    /// The final LUT UE3's LUT blender builds: `source` (the settings' LUT
    /// texture, or neutral) through `transform`, per entry.
    #[must_use]
    pub fn graded(source: Option<&Lut>, transform: &ColorTransform) -> Self {
        let neutral = Self::neutral();
        let src = source.unwrap_or(&neutral);
        let texels = (0..LUT_SIZE * LUT_SIZE * LUT_SIZE)
            .map(|i| {
                let r = i % LUT_SIZE;
                let b = (i / LUT_SIZE) % LUT_SIZE;
                let g = i / (LUT_SIZE * LUT_SIZE);
                transform
                    .apply(src.at(r, g, b))
                    .clamp(Vec3::ZERO, Vec3::ONE)
                    .to_array()
            })
            .collect();
        Self { texels }
    }

    /// RGBA8 texels for a 256 × 16 texture.
    #[must_use]
    pub fn to_rgba8(&self) -> Vec<u8> {
        let byte = |x: f32| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let b = (x.clamp(0.0, 1.0) * 255.0).round() as u8;
            b
        };
        self.texels
            .iter()
            .flat_map(|t| [byte(t[0]), byte(t[1]), byte(t[2]), 255])
            .collect()
    }

    /// A lookup the way the uber shader samples it: bilinear in red/green
    /// within a blue slice, linear between the two nearest slices (the
    /// shader's slice index uses `b·14.9999`). Reference for the WGSL.
    #[cfg(test)]
    #[must_use]
    pub fn sample(&self, c: Vec3) -> Vec3 {
        let c = c.clamp(Vec3::ZERO, Vec3::ONE);
        let n = (LUT_SIZE - 1) as f32;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let slice = ((c.z * 14.9999).floor() as usize).min(LUT_SIZE - 2);
        let f = c.z * n - slice as f32;
        let bilinear = |b: usize| {
            let x = c.x * n;
            let y = c.y * n;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let (x0, y0) = (
                (x.floor() as usize).min(LUT_SIZE - 1),
                (y.floor() as usize).min(LUT_SIZE - 1),
            );
            let (x1, y1) = ((x0 + 1).min(LUT_SIZE - 1), (y0 + 1).min(LUT_SIZE - 1));
            let (fx, fy) = (x - x0 as f32, y - y0 as f32);
            let top = lerp3(self.at(x0, y0, b), self.at(x1, y0, b), fx);
            let bot = lerp3(self.at(x0, y1, b), self.at(x1, y1, b), fx);
            lerp3(top, bot, fy)
        };
        lerp3(bilinear(slice), bilinear(slice + 1), f)
    }
}

#[allow(clippy::cast_precision_loss)]
fn node(i: usize) -> f32 {
    i as f32 / (LUT_SIZE - 1) as f32
}

// ------------------------------------------------------------ height fog

/// One exponential height fog, as `FSceneRenderer::InitFogConstants` sets
/// it up (CONFIRMED: native code and the fog pixel shader's GLSL).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeightFog {
    /// `FogHeight` (UU).
    pub height: f32,
    /// `FogDensity / 1000`.
    pub density: f32,
    /// `FogHeightFalloff / 1000` (per UU).
    pub falloff: f32,
    /// `1 − FogMaxOpacity`: the least scene transmittance.
    pub min_transmittance: f32,
    /// `StartDistance` (UU).
    pub start_distance: f32,
    /// Exponent of the directional colour weight, from
    /// `LightTerminatorAngle`.
    pub terminator_exponent: f32,
    /// `OppositeLightColor` (linear) × `OppositeLightBrightness`.
    pub opposite_color: Vec3,
    /// `LightInscatteringColor` (linear) × `LightInscatteringBrightness`.
    pub inscattering_color: Vec3,
}

impl HeightFog {
    /// From an `ExponentialHeightFogComponent`'s values; `None` for other
    /// classes, a disabled component, or a component without fog.
    #[must_use]
    pub fn from_component(c: &FogComponentInfo) -> Option<Self> {
        if !c
            .class_name()
            .eq_ignore_ascii_case("ExponentialHeightFogComponent")
        {
            return None;
        }
        let p = &c.params;
        if !p.bool("bEnabled").unwrap_or(true) {
            return None;
        }
        // Class defaults of `Engine.ExponentialHeightFogComponent` when a
        // value is missing (the importer writes effective values).
        let density = p.f32("FogDensity").unwrap_or(0.02) / 1000.0;
        if density <= 0.0 {
            return None;
        }
        let angle = p.f32("LightTerminatorAngle").unwrap_or(45.0);
        let color = |name: &str, brightness: &str, default: [u8; 4], b0: f32| {
            Vec3::from_array(color8_to_linear(p.color8(name).unwrap_or(default)))
                * p.f32(brightness).unwrap_or(b0)
        };
        Some(Self {
            height: p.f32("FogHeight").unwrap_or(c.location_ue.z),
            density,
            falloff: p.f32("FogHeightFalloff").unwrap_or(0.2) / 1000.0,
            min_transmittance: 1.0 - p.f32("FogMaxOpacity").unwrap_or(1.0),
            start_distance: p.f32("StartDistance").unwrap_or(0.0),
            terminator_exponent: terminator_exponent(angle),
            opposite_color: color(
                "OppositeLightColor",
                "OppositeLightBrightness",
                [177, 208, 255, 0],
                0.2,
            ),
            inscattering_color: color(
                "LightInscatteringColor",
                "LightInscatteringBrightness",
                [245, 212, 41, 0],
                1.0,
            ),
        })
    }

    /// Density at the camera height (`FogDensity' · 2^(−falloff·(z − FogHeight))`).
    #[must_use]
    pub fn density_at(&self, camera_z: f32) -> f32 {
        self.density
            * (-self.falloff * (camera_z - self.height))
                .clamp(-126.0, 126.0)
                .exp2()
    }

    /// Scene transmittance and fog colour for a view ray of length `dist`
    /// (UU) whose vertical rise is `rise` (UU), from a camera at height
    /// `camera_z`, `cos_to_light` being the cosine between the ray and the
    /// direction towards the light. The pixel shader's maths (reference
    /// for the WGSL): line integral of the exponential density from the
    /// start distance, base-2 transmittance clamped to the least
    /// transmittance, colour blended between inscattering (towards the
    /// light) and opposite colour, scaled by `1 − transmittance`.
    #[cfg(test)]
    #[must_use]
    pub fn evaluate(&self, camera_z: f32, dist: f32, rise: f32, cos_to_light: f32) -> (f32, Vec3) {
        let z = if rise.abs() <= 0.01 { 0.01 } else { rise };
        let kz = self.falloff * z;
        let line = if kz.abs() > 1e-6 {
            (1.0 - (-kz).clamp(-126.0, 126.0).exp2()) / kz
        } else {
            std::f32::consts::LN_2
        };
        let integral = self.density_at(camera_z) * line * (dist - self.start_distance).max(0.0);
        let t = (-integral)
            .exp2()
            .clamp(0.0, 1.0)
            .max(self.min_transmittance);
        let w = (0.5 - 0.499 * cos_to_light)
            .abs()
            .powf(self.terminator_exponent);
        let color = self.inscattering_color + (self.opposite_color - self.inscattering_color) * w;
        (t, color * (1.0 - t))
    }
}

/// Exponent of the fog's directional colour weight for a terminator angle
/// (degrees): with `x = 0.5 − 0.5·min(cos θ, 0.99999)` (0.999995 when
/// `cos θ < −0.99999`), `−0.30103 / ln x` (natural log, as the native code
/// computes it; CONFIRMED). The native float 0.30103 rounds to the same
/// `f32` as log10(2).
#[must_use]
pub fn terminator_exponent(angle_degrees: f32) -> f32 {
    let c = angle_degrees.to_radians().cos();
    let x = if c < -0.99999 {
        0.999_995
    } else {
        0.5 - 0.5 * c.min(0.99999)
    };
    -std::f32::consts::LOG10_2 / x.ln()
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_assets::scene::UeProps;

    fn props(text: &str) -> UeProps {
        UeProps::parse(text).expect("test JSON")
    }

    fn volume(name: &str, priority: f32, enabled: bool, lo: f32, hi: f32) -> PostProcessVolumeInfo {
        let box_planes = vec![
            [1.0, 0.0, 0.0, hi],
            [-1.0, 0.0, 0.0, -lo],
            [0.0, 1.0, 0.0, hi],
            [0.0, -1.0, 0.0, -lo],
            [0.0, 0.0, 1.0, hi],
            [0.0, 0.0, -1.0, -lo],
        ];
        PostProcessVolumeInfo {
            level: 0,
            actor_slot: 0,
            actor_name: name.to_owned(),
            priority,
            enabled,
            override_world_chain: false,
            settings: UeProps::default(),
            hulls: vec![box_planes],
            bounds: None,
        }
    }

    #[test]
    fn settings_read_values_flags_and_defaults() {
        let s = PostSettings::from_props(&props(
            r#"{"Bloom_Scale": 0.3, "bOverride_Bloom_Scale": false,
                "Bloom_Tint": {"R": 255, "G": 0, "B": 0, "A": 0},
                "Scene_HighLights": {"X": 1.0, "Y": 1.0, "Z": 0.7}, "bEnableSceneEffect": false,
                "ColorGrading_LookupTable": "Pkg.lut.LUT_Night",
                "bOverride_Scene_ColorGradingLUT": true}"#,
        ));
        assert_eq!(s.bloom_scale, 0.3);
        assert!(!s.ov.bloom_scale);
        assert_eq!(s.bloom_tint, Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(s.scene_highlights, Vec3::new(1.0, 1.0, 0.7));
        assert!(!s.enable_scene_effect);
        assert_eq!(s.color_grading_lut.as_deref(), Some("Pkg.lut.LUT_Night"));
        assert!(s.ov.scene_color_grading_lut);
        // Untouched values keep the struct defaults.
        assert_eq!(s.bloom_screen_blend_threshold, 10.0);
        assert!(s.ov.scene_shadows && !s.ov.scene_colorize);
    }

    #[test]
    fn volume_overrides_follow_flags_and_group_toggles() {
        let mut world = PostSettings {
            bloom_threshold: 0.0,
            bloom_screen_blend_threshold: 50000.0,
            color_grading_lut: Some("LUT".to_owned()),
            ..PostSettings::default()
        };
        world.ov.scene_color_grading_lut = true;
        world.ov.scene_highlights = false;
        let mut vol = PostSettings {
            bloom_scale: 0.3,
            scene_highlights: Vec3::new(1.0, 1.0, 0.7),
            ..PostSettings::default()
        };
        vol.ov.scene_tonemapper_scale = false;
        vol.scene_tonemapper_scale = 5.0;
        let mut out = world.clone();
        vol.override_into(&mut out, 1.0);
        assert_eq!(out.bloom_scale, 0.3);
        // The volume's struct-default threshold (override flag set by
        // default) replaces the world's.
        assert_eq!(out.bloom_threshold, 1.0);
        assert_eq!(out.bloom_screen_blend_threshold, 10.0);
        assert_eq!(out.scene_highlights, Vec3::new(1.0, 1.0, 0.7));
        assert!(out.ov.scene_highlights, "override flags propagate");
        assert_eq!(out.scene_tonemapper_scale, 1.0, "flag clear: kept");
        assert_eq!(out.color_grading_lut.as_deref(), Some("LUT"), "LUT kept");
        // A volume that turns bloom off disables the group: its bloom
        // values are not applied.
        let mut off = PostSettings {
            enable_bloom: false,
            bloom_scale: 9.0,
            ..PostSettings::default()
        };
        off.ov.enable_bloom = true;
        let mut out = world.clone();
        off.override_into(&mut out, 1.0);
        assert!(!out.enable_bloom);
        assert_eq!(out.bloom_scale, 1.0);
        // Half alpha blends.
        let mut out = world.clone();
        vol.override_into(&mut out, 0.5);
        assert!((out.bloom_scale - 0.65).abs() < 1e-6);
        let mut same = world.clone();
        vol.override_into(&mut same, 0.0);
        assert_eq!(same, world);
    }

    #[test]
    fn the_highest_priority_enclosing_enabled_volume_applies() {
        let big = volume("big", 0.0, true, -100.0, 100.0);
        let small = volume("small", 1.0, true, -10.0, 10.0);
        let disabled = volume("off", 5.0, false, -100.0, 100.0);
        let mut list = vec![&big, &small, &disabled];
        list.sort_by(|a, b| b.priority.total_cmp(&a.priority));
        assert_eq!(
            select_volume(&list, Vec3::ZERO).map(|i| list[i].actor_name.as_str()),
            Some("small")
        );
        assert_eq!(
            select_volume(&list, Vec3::new(50.0, 0.0, 0.0)).map(|i| list[i].actor_name.as_str()),
            Some("big")
        );
        assert_eq!(select_volume(&list, Vec3::splat(500.0)), None);
        // On the boundary counts as inside.
        assert!(volume_contains(&small, Vec3::new(10.0, 0.0, 0.0)));
        // Bounds fallback without planes.
        let mut b = volume("b", 0.0, true, 0.0, 0.0);
        b.hulls.clear();
        b.bounds = Some((Vec3::splat(-1.0), Vec3::splat(1.0)));
        assert!(volume_contains(&b, Vec3::ZERO));
        assert!(!volume_contains(&b, Vec3::splat(2.0)));
        b.bounds = None;
        assert!(!volume_contains(&b, Vec3::ZERO));
        // Desired settings: world overridden by the selected volume.
        let world = PostSettings::default();
        let vs = PostSettings {
            bloom_scale: 0.25,
            ..PostSettings::default()
        };
        let pairs = vec![(&small, vs.clone()), (&big, world.clone())];
        let (d, sel) = desired_settings(&world, &pairs, Vec3::ZERO);
        assert_eq!(sel, Some(0));
        assert_eq!(d.bloom_scale, 0.25);
        let (d, sel) = desired_settings(&world, &pairs, Vec3::splat(1000.0));
        assert_eq!(sel, None);
        assert_eq!(d, world);
    }

    #[test]
    fn the_time_blend_is_linear_over_the_duration() {
        // Remaining 1 s, 0.25 s step → a quarter.
        assert_eq!(blend_factor(1.0, 0.0, 0.25), 0.25);
        assert_eq!(blend_factor(1.0, 0.75, 0.25), 1.0);
        assert_eq!(blend_factor(0.0, 0.0, 0.0), 1.0);
        let a = PostSettings::default();
        let b = PostSettings {
            bloom_scale: 0.0,
            scene_highlights: Vec3::splat(0.5),
            bloom_interpolation_duration: 2.0,
            scene_interpolation_duration: 1.0,
            ..PostSettings::default()
        };
        let mut blend = PostBlender::default();
        assert_eq!(blend.update(&a, None, 10.0).bloom_scale, 1.0);
        // Enter a volume at t = 10; steps of 0.5 s.
        let mut values = Vec::new();
        for i in 0..6 {
            let now = 10.0 + 0.5 * i as f32;
            let s = blend.update(&b, Some(0), now);
            values.push((s.bloom_scale, s.scene_highlights.x));
        }
        // Bloom (2 s): 1.0 → 0 linearly; scene (1 s): 1.0 → 0.5 in 1 s.
        let expect_bloom = [1.0, 0.75, 0.5, 0.25, 0.0, 0.0];
        let expect_scene = [1.0, 0.75, 0.5, 0.5, 0.5, 0.5];
        for (i, (bl, sc)) in values.iter().enumerate() {
            assert!((bl - expect_bloom[i]).abs() < 1e-5, "{i}: {bl}");
            assert!((sc - expect_scene[i]).abs() < 1e-5, "{i}: {sc}");
        }
        // A disabled group keeps its values; toggles snap.
        let c = PostSettings {
            enable_bloom: false,
            bloom_scale: 7.0,
            ..b.clone()
        };
        let s = blend.update(&c, Some(1), 20.0).clone();
        assert!(!s.enable_bloom);
        assert_eq!(s.bloom_scale, 0.0);
        blend.reset();
        assert!(blend.current().is_none());
    }

    #[test]
    fn uber_parameters_pick_settings_or_effect_values() {
        let effect = UberEffect::from_props(&props(
            r#"{"TonemapperType": "Tonemapper_Customizable", "TonemapperToeFactor": 0.5,
                "bUseWorldSettings": true, "BloomScale": 1.0}"#,
        ));
        assert_eq!(effect.tonemapper, TonemapperType::Customizable);
        assert_eq!(effect.tonemapper_range, 8.0, "class default");
        let mut s = PostSettings {
            bloom_scale: 0.3,
            scene_midtones: Vec3::new(1.25, 1.3, 1.3),
            ..PostSettings::default()
        };
        let p = uber_params(&effect, Some(&s));
        assert_eq!(p.bloom_scale, 0.3);
        assert_eq!(p.transform.midtones, Vec3::new(1.25, 1.3, 1.3));
        // Colorize is not overridden by default: the effect's value.
        assert_eq!(p.transform.colorize, Vec3::ONE);
        assert_eq!(p.tonemapper.scale, 1.0);
        assert!((p.tonemapper.toe - 0.5).abs() < 1e-6);
        // Without world settings the effect's own (class default) colour
        // transform applies.
        let own = UberEffect {
            use_world_settings: false,
            ..effect.clone()
        };
        let q = uber_params(&own, Some(&s));
        assert_eq!(q.transform.highlights, Vec3::splat(0.8));
        assert!((q.transform.desaturation - 0.4).abs() < 1e-6);
        // Scene effect off → identity; bloom overridden off → zero.
        s.enable_scene_effect = false;
        s.enable_bloom = false;
        let r = uber_params(&effect, Some(&s));
        assert_eq!(r.transform, ColorTransform::IDENTITY);
        assert_eq!(r.bloom_scale, 0.0);
    }

    #[test]
    fn bloom_fades_on_bright_pixels() {
        let glow = Vec3::splat(0.5);
        let tint = Vec3::new(1.0, 0.5, 0.25);
        // Dark scene: full bloom, tinted.
        let dark = bloom_blend(Vec3::ZERO, glow, tint, 10.0);
        assert!((dark - Vec3::new(0.5, 0.25, 0.125)).length() < 1e-6);
        // Luminance 2 with threshold 10: 2^−6 · 10 = 0.156 of the bloom.
        let bright = bloom_blend(Vec3::splat(2.0), glow, Vec3::ONE, 10.0);
        assert!((bright - Vec3::splat(2.0 + 0.5 * 10.0 / 64.0)).length() < 1e-5);
        // A huge threshold never fades (the cave maps' 50000).
        let never = bloom_blend(Vec3::splat(2.0), glow, Vec3::ONE, 50_000.0);
        assert!((never - Vec3::splat(2.5)).length() < 1e-6);
        // Bloom off: tint·scale 0.
        assert_eq!(bloom_blend(Vec3::ONE, glow, Vec3::ZERO, 10.0), Vec3::ONE);
    }

    #[test]
    fn tonemapper_constants_and_curve() {
        let t = Tonemapper::new(TonemapperType::Customizable, 8.0, 0.5, 1.0);
        assert!((t.a - 0.22).abs() < 1e-6);
        assert!((t.b - 8.22 / 8.0).abs() < 1e-6);
        assert!((t.crossover - ((0.22f32 * 1.0275).sqrt() - 0.22)).abs() < 1e-6);
        assert_eq!(t.map(0.0), 0.0);
        // Monotonic and bounded.
        let mut last = 0.0;
        for i in 1..200 {
            let v = t.map(i as f32 * 0.05);
            assert!(v >= last - 1e-6 && v <= 1.0);
            last = v;
        }
        assert!(t.map(100.0) > 0.99);
        // Toe factor 1 → the pure curve; Off → gamma only.
        let pure = Tonemapper::new(TonemapperType::Customizable, 8.0, 1.0, 1.0);
        assert!((pure.map(0.5) - 1.0275 * 0.5 / 0.72).abs() < 1e-5);
        let off = Tonemapper::new(TonemapperType::Off, 8.0, 0.5, 1.0);
        assert!((off.map(0.5) - 0.5f32.powf(1.0 / 2.2)).abs() < 1e-6);
        // Below the crossover with toe 0: gamma of the scaled value.
        let toe = Tonemapper::new(TonemapperType::Customizable, 8.0, 0.0, 1.0);
        assert!((toe.map(0.1) - 0.1f32.powf(1.0 / 2.2)).abs() < 1e-6);
    }

    #[test]
    fn colour_bytes_convert_through_the_2_2_table() {
        assert_eq!(color8_to_linear([0, 255, 0, 0]), [0.0, 1.0, 0.0]);
        // pow(128 / 255, 2.2) and pow(1 / 255, 2.2), rounded to f32.
        let [a, b, _] = color8_to_linear([128, 1, 0, 0]);
        assert!((f64::from(a) - 0.219_519_719_5).abs() < 1e-8, "{a}");
        assert!((f64::from(b) - 5.077_051_9e-6).abs() < 1e-12, "{b}");
        let mut last = -1.0;
        for i in 0..=255u8 {
            let v = color8_to_linear([i, 0, 0, 0])[0];
            assert!(v > last);
            last = v;
        }
    }

    /// A volume change part-way through a blend restarts it from the
    /// current (partly blended) values. As in `UpdatePPSetting`, a step
    /// uses the time since the previous update and the blend time elapsed
    /// up to that previous update, so the change frame already moves by its
    /// own frame time.
    #[test]
    fn a_volume_change_restarts_the_blend_from_the_current_values() {
        let a = PostSettings::default();
        let b = PostSettings {
            bloom_scale: 0.0,
            ..PostSettings::default()
        };
        let mut blend = PostBlender::default();
        let mut step =
            |s: &PostSettings, v: Option<usize>, now: f32| blend.update(s, v, now).bloom_scale;
        assert_eq!(step(&a, None, 0.0), 1.0);
        assert_eq!(step(&a, None, 0.5), 1.0);
        // Enter the volume (1 s blend), leave it half-way.
        assert!((step(&b, Some(0), 1.0) - 0.5).abs() < 1e-6);
        assert!((step(&a, None, 1.5) - 0.75).abs() < 1e-6);
        assert!((step(&a, None, 2.0) - 0.875).abs() < 1e-6);
        assert!((step(&a, None, 2.5) - 1.0).abs() < 1e-6);
        // Time going backwards (a clock reset) never extrapolates.
        let s = step(&b, Some(0), 1.0);
        assert!((0.0..=1.0).contains(&s), "{s}");
    }

    #[test]
    fn color_transform_and_lut() {
        let id = ColorTransform::IDENTITY;
        let c = Vec3::new(0.2, 0.5, 0.9);
        assert!((id.apply(c) - c).length() < 1e-6);
        let t = ColorTransform {
            highlights: Vec3::new(1.0, 1.0, 0.5),
            desaturation: 1.0,
            ..ColorTransform::IDENTITY
        };
        // Full desaturation: all channels equal the weighted luminance of
        // the highlight-scaled colour.
        let o = t.apply(Vec3::new(0.2, 0.5, 0.4));
        let l = 0.3 * 0.2 + 0.59 * 0.5 + 0.11 * 0.8;
        assert!((o - Vec3::splat(l)).length() < 1e-5);
        let shadows = ColorTransform {
            shadows: Vec3::splat(0.1),
            midtones: Vec3::splat(2.0),
            ..ColorTransform::IDENTITY
        };
        assert!((shadows.apply(Vec3::splat(0.5)).x - 0.16).abs() < 1e-5);
        assert_eq!(shadows.apply(Vec3::splat(0.05)), Vec3::ZERO);
        // The neutral LUT samples to the input; the graded one applies the
        // transform at the nodes.
        let n = Lut::neutral();
        for c in [
            Vec3::ZERO,
            Vec3::ONE,
            Vec3::new(0.3, 0.6, 0.85),
            Vec3::new(1.0, 0.0, 0.5),
        ] {
            assert!((n.sample(c) - c).length() < 1e-5, "{c}");
        }
        let g = Lut::graded(None, &t);
        let node = Vec3::new(node(3), node(9), node(12));
        assert!((g.at(3, 9, 12) - t.apply(node).clamp(Vec3::ZERO, Vec3::ONE)).length() < 1e-6);
        // Texture round trip in the UE3 layout (x = b·16 + r, y = g).
        let bytes = n.to_rgba8();
        assert_eq!(bytes.len(), 256 * 16 * 4);
        let i = (5 * 256 + 7 * 16 + 2) * 4;
        assert_eq!(&bytes[i..i + 4], &[34, 85, 119, 255]);
        let back = Lut::from_rgba8(256, 16, &bytes).unwrap();
        assert!((back.at(2, 5, 7) - n.at(2, 5, 7)).length() < 0.003);
        assert!(Lut::from_rgba8(16, 16, &bytes).is_none());
    }

    const FOG_JSON: &str = r#"{"FogDensity": 0.02, "FogHeightFalloff": 0.2, "FogMaxOpacity": 1.0,
        "StartDistance": 1024.0, "LightTerminatorAngle": 50.0, "FogHeight": 100.0,
        "OppositeLightColor": {"R": 255, "G": 255, "B": 255, "A": 0}, "OppositeLightBrightness": 1.0,
        "LightInscatteringColor": {"R": 255, "G": 0, "B": 0, "A": 0},
        "LightInscatteringBrightness": 2.0}"#;

    #[test]
    fn height_fog_constants_and_integral() {
        // AG-StarHaven-like values (the component's stored numbers are
        // used only as a plausible example here).
        let c = FogComponentInfo {
            level: 0,
            actor_slot: 0,
            actor_name: "F".to_owned(),
            actor_class: "Engine.ExponentialHeightFog".to_owned(),
            location_ue: Vec3::new(0.0, 0.0, 100.0),
            class: "Engine.ExponentialHeightFogComponent".to_owned(),
            params: props(FOG_JSON),
        };
        let f = HeightFog::from_component(&c).unwrap();
        assert!((f.density - 2e-5).abs() < 1e-12);
        assert!((f.falloff - 2e-4).abs() < 1e-10);
        assert_eq!(f.min_transmittance, 0.0);
        assert!((f.terminator_exponent - terminator_exponent(50.0)).abs() < 1e-7);
        assert_eq!(f.inscattering_color, Vec3::new(2.0, 0.0, 0.0));
        // Density halves every 1/falloff UU above the fog height.
        assert!((f.density_at(100.0 + 5000.0) - 1e-5).abs() < 1e-11);
        // Nothing before the start distance.
        let (t, col) = f.evaluate(100.0, 1000.0, 0.0, 0.0);
        assert_eq!(t, 1.0);
        assert_eq!(col, Vec3::ZERO);
        // Horizontal ray: line integral factor → ln 2 (the shader's 0.01 UU
        // minimum rise). In f32, as in the original shader, `1 − 2^(−kz)`
        // for such a small `kz` loses digits: a few per cent of the integral.
        let (t, _) = f.evaluate(100.0, 1024.0 + 100_000.0, 0.0, 0.0);
        let expect = (-(2e-5 * std::f32::consts::LN_2 * 100_000.0)).exp2();
        assert!((t - expect).abs() < 1e-2, "{t} vs {expect}");
        // Rays going down accumulate more fog than rays going up.
        let (up, _) = f.evaluate(100.0, 50_000.0, 20_000.0, 0.0);
        let (down, _) = f.evaluate(100.0, 50_000.0, -20_000.0, 0.0);
        assert!(down < up);
        // Infinite distance looking down: fully fogged (limited by the
        // max opacity); looking up: finite.
        let (t, _) = f.evaluate(100.0, 1e9, -1e8, 0.0);
        assert_eq!(t, 0.0);
        let (t, _) = f.evaluate(100.0, 1e9, 5e8, 0.0);
        assert!(t > 0.5);
        // Colour: towards the light (cos = 1) mostly inscattering, away
        // (cos = −1) the opposite colour.
        let (_, toward) = f.evaluate(100.0, 1e9, -1e8, 1.0);
        let (_, away) = f.evaluate(100.0, 1e9, -1e8, -1.0);
        assert!(toward.x > toward.y && (away - Vec3::ONE).length() < 0.01);
        // Max opacity floors the transmittance.
        let mut c2 = c.clone();
        c2.params = props(&FOG_JSON.replace("\"FogMaxOpacity\": 1.0", "\"FogMaxOpacity\": 0.7"));
        let f2 = HeightFog::from_component(&c2).unwrap();
        assert!((f2.evaluate(100.0, 1e9, -1e8, 0.0).0 - 0.3).abs() < 1e-6);
        // Disabled or other classes: none.
        c2.params =
            props(&FOG_JSON.replace("\"FogDensity\"", "\"bEnabled\": false, \"FogDensity\""));
        assert!(HeightFog::from_component(&c2).is_none());
        let mut other = c.clone();
        other.class = "Engine.HeightFogComponent".to_owned();
        assert!(HeightFog::from_component(&other).is_none());
    }

    #[test]
    fn terminator_exponent_matches_the_native_formula() {
        // 45° (class default): x = 0.5 − 0.5·cos 45°.
        let x = 0.5 - 0.5 * 45f32.to_radians().cos();
        assert!((terminator_exponent(45.0) - (-std::f32::consts::LOG10_2 / x.ln())).abs() < 1e-6);
        // 0° clamps cos to 0.99999; 180° uses 0.999995.
        assert!(
            (terminator_exponent(0.0)
                - (-std::f32::consts::LOG10_2 / (0.5 - 0.5 * 0.99999f32).ln()))
            .abs()
                < 1e-3
        );
        assert!(
            (terminator_exponent(180.0) - (-std::f32::consts::LOG10_2 / 0.999_995f32.ln())).abs()
                < 1.0
        );
        assert!(terminator_exponent(90.0) > 0.0);
    }
}
