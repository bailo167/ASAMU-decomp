//! Particle decoding on synthetic v868 packages written byte by byte here
//! (no original game data): a complete particle system (emitter, LOD level,
//! modules, every distribution kind), archetype merging, raw arrays read
//! without a schema, strict native-tail refusal and the lookup-table read.

#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use asamu_ue3::Package;
use asamu_ue3::model::{LoadedPackage, PackageSet};
use asamu_ue3::particle::{
    Distribution, EmitterKind, Param, ParticleDecoder, ParticleError, ParticleRole, lookup_value,
};
use common::{Export, Import, Synth, W};

// ---------------------------------------------------------------------------
// Package builder
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
pub struct Builder {
    names: Vec<String>,
    imports: Vec<Import>,
    exports: Vec<Export>,
}

impl Builder {
    pub fn new() -> Builder {
        let mut b = Builder::default();
        b.n("None");
        b
    }

    pub fn n(&mut self, s: &str) -> i32 {
        if let Some(i) = self.names.iter().position(|x| x == s) {
            return i as i32;
        }
        self.names.push(s.to_owned());
        (self.names.len() - 1) as i32
    }

    fn import(&mut self, class_package: &str, class: &str, outer: i32, name: &str) -> i32 {
        let imp = Import {
            class_package: self.n(class_package),
            class_name: self.n(class),
            outer,
            name: self.n(name),
            number: 0,
        };
        if let Some(i) = self.imports.iter().position(|x| {
            x.name == imp.name && x.outer == imp.outer && x.class_name == imp.class_name
        }) {
            return -(i as i32) - 1;
        }
        self.imports.push(imp);
        -(self.imports.len() as i32)
    }

    /// Import of class `Engine.<class>`.
    pub fn class(&mut self, class: &str) -> i32 {
        let engine = self.import("Core", "Package", 0, "Engine");
        self.import("Core", "Class", engine, class)
    }

    /// An object `<package>.<name>` of class `Engine.<class>` elsewhere.
    pub fn external(&mut self, package: &str, class: &str, name: &str) -> i32 {
        let p = self.import("Core", "Package", 0, package);
        self.import("Engine", class, p, name)
    }

    pub fn export(&mut self, class: i32, outer: i32, name: &str, payload: Vec<u8>) -> i32 {
        let name = self.n(name);
        self.exports.push(Export {
            class,
            super_: 0,
            outer,
            name,
            number: 0,
            archetype: 0,
            object_flags: 0x0007_0004_0000_0000,
            payload,
            export_flags: 1,
            net_counts: Vec::new(),
            guid: [0; 4],
            package_flags: 0,
        });
        self.exports.len() as i32
    }

    pub fn reserve(&mut self, class: i32, outer: i32, name: &str) -> i32 {
        self.export(class, outer, name, Vec::new())
    }

    pub fn fill(&mut self, export: i32, payload: Vec<u8>) {
        self.exports[(export - 1) as usize].payload = payload;
    }

    pub fn set_archetype(&mut self, export: i32, archetype: i32) {
        self.exports[(export - 1) as usize].archetype = archetype;
    }

    pub fn package(&mut self) -> Package {
        let s = Synth {
            package_flags: 0x0000_0008,
            names: self
                .names
                .iter()
                .enumerate()
                .map(|(i, n)| (n.clone(), 0x0007_0010_0000_0000u64 + i as u64))
                .collect(),
            imports: self.imports.clone(),
            exports: self.exports.clone(),
            additional_packages: Vec::new(),
            texture_allocations: Vec::new(),
            guid: [1, 2, 3, 4],
            package_source: 0,
            engine_version: 12097,
            cooker_version: 136,
            depends: true,
            thumbnails: Vec::new(),
        };
        Package::from_bytes(s.build().0).unwrap()
    }
}

/// Tagged-property writer.
pub struct Tags<'b> {
    pub b: &'b mut Builder,
    pub w: W,
}

impl<'b> Tags<'b> {
    /// An object payload: NetIndex, then tags.
    pub fn new(b: &'b mut Builder) -> Tags<'b> {
        let mut w = W::default();
        w.i32(0);
        Tags { b, w }
    }

    /// A bare tagged stream (struct contents).
    pub fn inner(b: &'b mut Builder) -> Tags<'b> {
        Tags { b, w: W::default() }
    }

    fn header(&mut self, name: &str, ty: &str, size: usize, index: i32) {
        let (n, t) = (self.b.n(name), self.b.n(ty));
        self.w.i32(n);
        self.w.i32(0);
        self.w.i32(t);
        self.w.i32(0);
        self.w.i32(size as i32);
        self.w.i32(index);
    }

    pub fn float(mut self, name: &str, v: f32) -> Self {
        self.header(name, "FloatProperty", 4, 0);
        self.w.bytes(&v.to_le_bytes());
        self
    }

    pub fn int(mut self, name: &str, v: i32) -> Self {
        self.header(name, "IntProperty", 4, 0);
        self.w.i32(v);
        self
    }

    pub fn boolean(mut self, name: &str, v: bool) -> Self {
        self.header(name, "BoolProperty", 0, 0);
        self.w.0.push(u8::from(v));
        self
    }

    pub fn object(mut self, name: &str, v: i32) -> Self {
        self.header(name, "ObjectProperty", 4, 0);
        self.w.i32(v);
        self
    }

    pub fn name_value(mut self, name: &str, v: &str) -> Self {
        self.header(name, "NameProperty", 8, 0);
        let i = self.b.n(v);
        self.w.i32(i);
        self.w.i32(0);
        self
    }

    pub fn enumeration_at(mut self, name: &str, index: i32, enum_name: &str, v: &str) -> Self {
        self.header(name, "ByteProperty", 8, index);
        let (e, x) = (self.b.n(enum_name), self.b.n(v));
        self.w.i32(e);
        self.w.i32(0);
        self.w.i32(x);
        self.w.i32(0);
        self
    }

    pub fn byte(mut self, name: &str, v: u8) -> Self {
        self.header(name, "ByteProperty", 1, 0);
        let none = self.b.n("None");
        self.w.i32(none);
        self.w.i32(0);
        self.w.0.push(v);
        self
    }

    pub fn structure(mut self, name: &str, struct_name: &str, body: Vec<u8>) -> Self {
        self.header(name, "StructProperty", body.len(), 0);
        let s = self.b.n(struct_name);
        self.w.i32(s);
        self.w.i32(0);
        self.w.bytes(&body);
        self
    }

    /// Binary `Vector` struct (the immutable Core layout).
    pub fn vector(self, name: &str, v: [f32; 3]) -> Self {
        let mut body = W::default();
        for c in v {
            body.bytes(&c.to_le_bytes());
        }
        self.structure(name, "Vector", body.0)
    }

    pub fn float_array(mut self, name: &str, v: &[f32]) -> Self {
        self.header(name, "ArrayProperty", 4 + 4 * v.len(), 0);
        self.w.i32(v.len() as i32);
        for x in v {
            self.w.bytes(&x.to_le_bytes());
        }
        self
    }

    pub fn object_array(mut self, name: &str, v: &[i32]) -> Self {
        self.header(name, "ArrayProperty", 4 + 4 * v.len(), 0);
        self.w.i32(v.len() as i32);
        for x in v {
            self.w.i32(*x);
        }
        self
    }

    /// Array of tagged structs (each a complete tagged stream).
    pub fn struct_array(mut self, name: &str, elems: &[Vec<u8>]) -> Self {
        let size: usize = 4 + elems.iter().map(Vec::len).sum::<usize>();
        self.header(name, "ArrayProperty", size, 0);
        self.w.i32(elems.len() as i32);
        for e in elems {
            self.w.bytes(e);
        }
        self
    }

    pub fn end(mut self) -> Vec<u8> {
        self.w.i32(0);
        self.w.i32(0);
        self.w.0
    }
}

fn load(pkg: Package) -> (PackageSet, Arc<LoadedPackage>) {
    let set = PackageSet::new::<&str>(&[]);
    let lp = set.insert_package("SynthFx", pkg);
    (set, lp)
}

/// A `RawDistribution*` struct body: object reference plus a baked table.
fn raw(b: &mut Builder, dist: i32, op: u8, chunk: u8, scale: f32, table: &[f32]) -> Vec<u8> {
    Tags::inner(b)
        .object("Distribution", dist)
        .byte("Op", op)
        .byte("LookupTableNumElements", if op == 1 { 1 } else { 2 })
        .byte("LookupTableChunkSize", chunk)
        .float_array("LookupTable", table)
        .float("LookupTableTimeScale", scale)
        .end()
}

/// An `InterpCurvePoint` with a binary vector or float `OutVal`.
fn point(b: &mut Builder, t: f32, out: &[f32], mode: &str) -> Vec<u8> {
    let mut x = Tags::inner(b).float("InVal", t);
    x = if out.len() == 3 {
        x.vector("OutVal", [out[0], out[1], out[2]])
    } else {
        x.float("OutVal", out[0])
    };
    x.enumeration_at("InterpMode", 0, "EInterpCurveMode", mode)
        .end()
}

struct Fixture {
    set: PackageSet,
    lp: Arc<LoadedPackage>,
    system: usize,
    texture: usize,
    junk: usize,
    derived_module: String,
}

fn fixture() -> Fixture {
    let (mut b, system, texture, junk) = fixture_builder();
    let (set, lp) = load(b.package());
    Fixture {
        set,
        lp,
        system,
        texture,
        junk,
        derived_module: "Fx.PS_Sparks.ParticleModuleSize_1".to_owned(),
    }
}

/// The fixture's builder and the export indices of the system, the texture
/// and the junk-tailed distribution.
fn fixture_builder() -> (Builder, usize, usize, usize) {
    let mut b = Builder::new();
    let core = b.import("Core", "Package", 0, "Core");
    let package_class = b.import("Core", "Class", core, "Package");
    let root_payload = Tags::new(&mut b).end();
    let root = b.export(package_class, 0, "Fx", root_payload);
    let material = b.external("FxMats", "Material", "M_Spark");

    let c_ps = b.class("ParticleSystem");
    let c_em = b.class("ParticleSpriteEmitter");
    let c_lod = b.class("ParticleLODLevel");
    let c_req = b.class("ParticleModuleRequired");
    let c_spawn = b.class("ParticleModuleSpawn");
    let c_life = b.class("ParticleModuleLifetime");
    let c_col = b.class("ParticleModuleColorOverLife");
    let c_size = b.class("ParticleModuleSize");
    let c_fconst = b.class("DistributionFloatConstant");
    let c_funi = b.class("DistributionFloatUniform");
    let c_fcurve = b.class("DistributionFloatConstantCurve");
    let c_vcurve = b.class("DistributionVectorConstantCurve");
    let c_vuni = b.class("DistributionVectorUniform");
    let c_vparam = b.class("DistributionVectorParticleParameter");
    let c_tex = b.class("Texture2D");

    let ps = b.reserve(c_ps, root, "PS_Sparks");
    let em = b.reserve(c_em, ps, "ParticleSpriteEmitter_0");
    let lod = b.reserve(c_lod, em, "ParticleLODLevel_0");
    let req = b.reserve(c_req, ps, "ParticleModuleRequired_0");
    let spawn = b.reserve(c_spawn, ps, "ParticleModuleSpawn_0");
    let life = b.reserve(c_life, ps, "ParticleModuleLifetime_0");
    let col = b.reserve(c_col, ps, "ParticleModuleColorOverLife_0");
    let size = b.reserve(c_size, ps, "ParticleModuleSize_0");
    let size2 = b.reserve(c_size, ps, "ParticleModuleSize_1");

    // Distributions.
    let p = Tags::new(&mut b).float("Constant", 25.0).end();
    let d_rate = b.export(c_fconst, req, "DistributionFloatConstant_0", p);
    let p = Tags::new(&mut b)
        .float("Min", 40.0)
        .float("Max", 100.0)
        .end();
    let d_spawn = b.export(c_funi, spawn, "DistributionFloatUniform_0", p);
    let p = Tags::new(&mut b).float("Min", 1.0).float("Max", 2.0).end();
    let d_life = b.export(c_funi, life, "DistributionFloatUniform_1", p);
    let pts = vec![
        point(&mut b, 0.0, &[1.0, 0.5, 0.25], "CIM_Linear"),
        point(&mut b, 1.0, &[0.0, 0.0, 0.0], "CIM_Linear"),
    ];
    let curve = Tags::inner(&mut b).struct_array("Points", &pts).end();
    let p = Tags::new(&mut b)
        .structure("ConstantCurve", "InterpCurveVector", curve)
        .enumeration_at("LockedAxes", 0, "EDistributionVectorLockFlags", "EDVLF_XY")
        .end();
    let d_color = b.export(c_vcurve, col, "DistributionVectorConstantCurve_0", p);
    let apts = vec![
        point(&mut b, 0.0, &[1.0], "CIM_Linear"),
        point(&mut b, 1.0, &[0.0], "CIM_Constant"),
    ];
    let acurve = Tags::inner(&mut b).struct_array("Points", &apts).end();
    let p = Tags::new(&mut b)
        .structure("ConstantCurve", "InterpCurveFloat", acurve)
        .end();
    let d_alpha = b.export(c_fcurve, col, "DistributionFloatConstantCurve_0", p);
    let p = Tags::new(&mut b)
        .vector("Max", [10.0, 20.0, 30.0])
        .vector("Min", [1.0, 2.0, 3.0])
        .enumeration_at(
            "MirrorFlags",
            1,
            "EDistributionVectorMirrorFlags",
            "EDVMF_Mirror",
        )
        .boolean("bUseExtremes", true)
        .end();
    let d_size = b.export(c_vuni, size, "DistributionVectorUniform_0", p);
    let p = Tags::new(&mut b)
        .name_value("ParameterName", "SparkSize")
        .vector("Constant", [5.0, 5.0, 5.0])
        .vector("MaxInput", [1.0, 1.0, 1.0])
        .vector("MaxOutput", [8.0, 8.0, 8.0])
        .enumeration_at("ParamModes", 2, "DistributionParamMode", "DPM_Direct")
        .end();
    let d_param = b.export(c_vparam, size2, "DistributionVectorParticleParameter_0", p);

    // Modules.
    let rate = raw(&mut b, d_rate, 1, 1, 0.0, &[25.0, 25.0, 25.0, 25.0]);
    let p = Tags::new(&mut b)
        .object("Material", material)
        .boolean("bEnabled", true)
        .boolean("bSpawnModule", true)
        .boolean("bUseLocalSpace", true)
        .int("SubImages_Horizontal", 4)
        .structure("SpawnRate", "RawDistributionFloat", rate)
        .structure("ModuleEditorColor", "Color", vec![1, 2, 3, 255])
        .end();
    b.fill(req, p);
    let burst = Tags::inner(&mut b)
        .int("Count", 5)
        .int("CountLow", -1)
        .float("Time", 0.25)
        .end();
    let rate = raw(
        &mut b,
        d_spawn,
        2,
        2,
        0.0,
        &[40.0, 100.0, 40.0, 100.0, 40.0, 100.0],
    );
    let p = Tags::new(&mut b)
        .boolean("bEnabled", true)
        .structure("Rate", "RawDistributionFloat", rate)
        .struct_array("BurstList", &[burst])
        .end();
    b.fill(spawn, p);
    let lt = raw(&mut b, d_life, 2, 2, 0.0, &[1.0, 2.0, 1.0, 2.0, 1.0, 2.0]);
    let p = Tags::new(&mut b)
        .boolean("bEnabled", true)
        .boolean("bSpawnModule", true)
        .structure("Lifetime", "RawDistributionFloat", lt)
        .end();
    b.fill(life, p);
    let c = raw(
        &mut b,
        d_color,
        1,
        3,
        1.0,
        &[0.0, 1.0, 1.0, 1.0, 0.25, 0.0, 0.0, 0.0],
    );
    let a = raw(&mut b, d_alpha, 1, 1, 1.0, &[0.0, 1.0, 1.0, 1.0]);
    let p = Tags::new(&mut b)
        .boolean("bEnabled", true)
        .boolean("bUpdateModule", true)
        .structure("ColorOverLife", "RawDistributionVector", c)
        .structure("AlphaOverLife", "RawDistributionFloat", a)
        .end();
    b.fill(col, p);
    let s = raw(&mut b, d_size, 2, 6, 0.0, &[1.0, 30.0]);
    let p = Tags::new(&mut b)
        .boolean("bEnabled", true)
        .boolean("bSpawnModule", true)
        .structure("StartSize", "RawDistributionVector", s)
        .end();
    b.fill(size, p);
    // The second size module stores only its distribution: everything else
    // comes from its archetype (the first size module).
    let s = raw(&mut b, d_param, 0, 0, 0.0, &[]);
    let p = Tags::new(&mut b)
        .structure("StartSize", "RawDistributionVector", s)
        .end();
    b.fill(size2, p);
    b.set_archetype(size2, size);

    // Structure.
    let p = Tags::new(&mut b)
        .object("RequiredModule", req)
        .object("SpawnModule", spawn)
        .object_array("Modules", &[life, col, size, size2, 0])
        .boolean("bEnabled", true)
        .int("PeakActiveParticles", 42)
        .end();
    b.fill(lod, p);
    let p = Tags::new(&mut b)
        .name_value("EmitterName", "Sparks")
        .object_array("LODLevels", &[lod])
        .end();
    b.fill(em, p);
    let p = Tags::new(&mut b)
        .float("UpdateTime_FPS", 60.0)
        .object_array("Emitters", &[em, 0])
        .float_array("LODDistances", &[0.0, 1500.0])
        .end();
    b.fill(ps, p);

    let p = Tags::new(&mut b).end();
    let tex = b.export(c_tex, root, "T_Spark", p);
    let mut p = Tags::new(&mut b).float("Constant", 1.0).end();
    p.extend_from_slice(&[1, 2, 3, 4]);
    let junk = b.export(c_fconst, root, "DistributionFloatConstant_Junk", p);

    (
        b,
        (ps - 1) as usize,
        (tex - 1) as usize,
        (junk - 1) as usize,
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn classify_by_chain_and_by_name() {
    let chain = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        ParticleRole::classify(
            "UTGame.UTParticleSystemComponent",
            &chain(&[
                "Engine.ParticleSystemComponent",
                "Engine.PrimitiveComponent"
            ])
        ),
        Some(ParticleRole::Component)
    );
    assert_eq!(
        ParticleRole::classify(
            "Engine.DistributionFloatParticleParameter",
            &chain(&[
                "Engine.DistributionFloatParameterBase",
                "Engine.DistributionFloatConstant",
                "Core.DistributionFloat",
                "Core.Component"
            ])
        ),
        Some(ParticleRole::FloatDistribution)
    );
    assert_eq!(
        ParticleRole::classify("Engine.Emitter", &chain(&["Engine.Actor", "Core.Object"])),
        None
    );
    assert_eq!(
        ParticleRole::classify("Engine.ParticleSystemReplay", &chain(&["Core.Object"])),
        None
    );
    // Without a chain the name decides.
    assert_eq!(
        ParticleRole::classify("Engine.ParticleModuleOrbit", &[]),
        Some(ParticleRole::Module)
    );
    assert_eq!(
        ParticleRole::classify("Engine.ParticleSpriteEmitter", &[]),
        Some(ParticleRole::Emitter)
    );
    assert_eq!(ParticleRole::classify("Engine.Texture2D", &[]), None);
    assert_eq!(
        EmitterKind::of_type_data(Some("ParticleModuleTypeDataBeam2")),
        EmitterKind::Beam
    );
    assert_eq!(
        EmitterKind::of_type_data(Some("ParticleModuleTypeDataMeshPhysX")),
        EmitterKind::Other
    );
    assert_eq!(EmitterKind::of_type_data(None), EmitterKind::Sprite);
}

#[test]
fn complete_system_decodes() {
    let f = fixture();
    let dec = ParticleDecoder::new(&f.set);
    let s = dec.system(&f.lp, f.system).unwrap();
    assert_eq!(s.path, "Fx.PS_Sparks");
    assert_eq!(s.package, "SynthFx");
    assert_eq!(s.emitters.len(), 1);
    assert_eq!(s.skipped_emitters, 1, "the null emitter slot");
    assert_eq!(s.params.get("UpdateTime_FPS"), Some(&Param::Float(60.0)));
    assert_eq!(
        s.params.get("LODDistances"),
        Some(&Param::List(vec![Param::Float(0.0), Param::Float(1500.0)])),
        "float array read without a schema"
    );

    let e = &s.emitters[0];
    assert_eq!(e.name, "Sparks");
    assert_eq!(e.kind, EmitterKind::Sprite);
    assert_eq!(e.lods.len(), 1);
    let l = &e.lods[0];
    assert!(l.enabled);
    assert_eq!(l.peak_active_particles, 42);
    assert_eq!(l.modules.len(), 4, "the null module slot is skipped");
    assert!(
        s.notes.iter().any(|n| n.contains("module slot 4 is null")),
        "{:?}",
        s.notes
    );

    // Required: material, sub images, spawn rate constant; editor colour left out.
    let req = l.required.as_ref().unwrap();
    assert!(req.enabled && req.spawn && !req.update);
    assert_eq!(
        req.param("Material"),
        Some(&Param::Text("FxMats.M_Spark".to_owned()))
    );
    assert_eq!(req.param("SubImages_Horizontal"), Some(&Param::Int(4)));
    assert!(req.param("ModuleEditorColor").is_none());
    let rate = req.param("SpawnRate").unwrap().as_distribution().unwrap();
    assert_eq!(rate.dist, "float");
    assert_eq!(
        rate.object.as_deref(),
        Some("Fx.PS_Sparks.ParticleModuleRequired_0.DistributionFloatConstant_0")
    );
    assert_eq!(
        rate.value,
        Distribution::Constant {
            value: vec![25.0],
            locked_axes: 0
        }
    );
    assert_eq!(rate.baked.range, Some([25.0, 25.0]));
    assert_eq!(rate.baked.len, 4);

    // Spawn: uniform rate and a burst list read as tagged structs.
    let spawn = l.spawn.as_ref().unwrap();
    let r = spawn.param("Rate").unwrap().as_distribution().unwrap();
    assert!(
        matches!(&r.value, Distribution::Uniform { min, max, .. } if min == &vec![40.0] && max == &vec![100.0])
    );
    let Some(Param::List(bursts)) = spawn.param("BurstList") else {
        panic!("burst list {:?}", spawn.param("BurstList"));
    };
    let Param::Struct(b0) = &bursts[0] else {
        panic!()
    };
    assert_eq!(b0.get("Count"), Some(&Param::Int(5)));
    assert_eq!(b0.get("CountLow"), Some(&Param::Int(-1)));
    assert_eq!(b0.get("Time"), Some(&Param::Float(0.25)));

    // Colour over life: vector curve with locked axes, float curve.
    let col = &l.modules[1];
    assert_eq!(col.class, "ParticleModuleColorOverLife");
    assert!(col.update && !col.spawn);
    let c = col
        .param("ColorOverLife")
        .unwrap()
        .as_distribution()
        .unwrap();
    let Distribution::ConstantCurve { curve, locked_axes } = &c.value else {
        panic!("{:?}", c.value)
    };
    assert_eq!(*locked_axes, 1, "EDVLF_XY");
    assert_eq!(curve.dim, 3);
    assert_eq!(curve.keys.len(), 2);
    assert_eq!(curve.keys[0].v, vec![1.0, 0.5, 0.25]);
    assert_eq!(curve.keys[1].t, 1.0);
    assert_eq!(curve.keys[0].mode, "linear");
    let a = col
        .param("AlphaOverLife")
        .unwrap()
        .as_distribution()
        .unwrap();
    let Distribution::ConstantCurve { curve, .. } = &a.value else {
        panic!()
    };
    assert_eq!(curve.dim, 1);
    assert_eq!(curve.keys[1].mode, "constant");

    // Size: vector uniform with a mirrored Y and extremes.
    let size = &l.modules[2];
    let d = size.param("StartSize").unwrap().as_distribution().unwrap();
    assert_eq!(
        d.value,
        Distribution::Uniform {
            min: vec![1.0, 2.0, 3.0],
            max: vec![10.0, 20.0, 30.0],
            locked_axes: 0,
            mirror: [0, 2, 0],
            use_extremes: true,
        }
    );
}

#[test]
fn archetype_values_merge_under_the_instance() {
    let f = fixture();
    let dec = ParticleDecoder::new(&f.set);
    let mut notes = Vec::new();
    let m = dec.module(&f.derived_module, &mut notes).unwrap();
    // From the archetype (the first size module).
    assert!(m.enabled && m.spawn, "flags inherited from the archetype");
    // Own value: a vector particle parameter with an empty table.
    let d = m.param("StartSize").unwrap().as_distribution().unwrap();
    let Distribution::Parameter {
        name,
        modes,
        constant,
        max_output,
        class,
        ..
    } = &d.value
    else {
        panic!("{:?}", d.value)
    };
    assert_eq!(name, "SparkSize");
    assert_eq!(modes, &vec![0, 0, 2]);
    assert_eq!(constant, &vec![5.0, 5.0, 5.0]);
    assert_eq!(max_output, &vec![8.0, 8.0, 8.0]);
    assert_eq!(class, "DistributionVectorParticleParameter");
    assert_eq!(d.baked.len, 0);
    assert_eq!(d.baked.range, None);
    let r = dec.resolve_path(&f.derived_module).unwrap().unwrap();
    assert_eq!(
        r.archetype.as_deref(),
        Some("Fx.PS_Sparks.ParticleModuleSize_0")
    );
    assert_eq!(r.own.len(), 1);
}

#[test]
fn strict_payload_and_role_checks() {
    let f = fixture();
    let dec = ParticleDecoder::new(&f.set);
    match dec.resolve(&f.lp, f.junk) {
        Err(ParticleError::NativeTail { end, size, .. }) => assert_eq!(size, end + 4),
        other => panic!("{other:?}"),
    }
    match dec.resolve(&f.lp, f.texture) {
        Err(ParticleError::NotParticle { class, .. }) => assert!(class.ends_with("Texture2D")),
        other => panic!("{other:?}"),
    }
    // The cached failure is reported again, not re-decoded into success.
    assert!(dec.resolve(&f.lp, f.junk).is_err());
    assert!(dec.system(&f.lp, f.junk).is_err());
    // Every export of the fixture except the two bad ones decodes exactly.
    let mut exact = 0;
    for i in 0..f.lp.package.exports.len() {
        if dec.export_role(&f.lp, i).is_some() && i != f.junk {
            dec.resolve(&f.lp, i).unwrap();
            exact += 1;
        }
    }
    assert_eq!(exact, 16, "9 structural objects and 7 distributions");
}

#[test]
fn missing_distribution_object_falls_back_to_the_table() {
    let mut b = Builder::new();
    let core = b.import("Core", "Package", 0, "Core");
    let package_class = b.import("Core", "Class", core, "Package");
    let root_payload = Tags::new(&mut b).end();
    let root = b.export(package_class, 0, "Fx", root_payload);
    let gone = b.external("Elsewhere", "DistributionFloatConstant", "Gone");
    let c_life = b.class("ParticleModuleLifetime");
    let t = raw(&mut b, gone, 1, 1, 2.0, &[0.5, 1.5, 0.5, 1.5]);
    let none = raw(&mut b, 0, 1, 1, 0.0, &[3.0, 3.0, 3.0, 3.0]);
    let p = Tags::new(&mut b)
        .structure("Lifetime", "RawDistributionFloat", t)
        .structure("Other", "RawDistributionFloat", none)
        .end();
    b.export(c_life, root, "ParticleModuleLifetime_0", p);
    let (set, _lp) = load(b.package());
    let dec = ParticleDecoder::new(&set);
    let mut notes = Vec::new();
    let m = dec
        .module("Fx.ParticleModuleLifetime_0", &mut notes)
        .unwrap();
    let d = m.param("Lifetime").unwrap().as_distribution().unwrap();
    assert_eq!(d.object.as_deref(), Some("Elsewhere.Gone"));
    assert!(
        matches!(&d.value, Distribution::Lookup { table, time_scale, .. } if table.len() == 4 && *time_scale == 2.0)
    );
    assert!(notes.iter().any(|n| n.contains("not found")), "{notes:?}");
    let o = m.param("Other").unwrap().as_distribution().unwrap();
    assert_eq!(o.object, None);
    assert!(matches!(
        &o.value,
        Distribution::Lookup {
            op: 1,
            chunk: 1,
            ..
        }
    ));
}

/// A placed component: template, activation flag and instance parameters
/// (an array of tagged structs read without a schema).
#[test]
fn component_values_decode() {
    let mut b = Builder::new();
    let core = b.import("Core", "Package", 0, "Core");
    let package_class = b.import("Core", "Class", core, "Package");
    let root_payload = Tags::new(&mut b).end();
    let root = b.export(package_class, 0, "Map", root_payload);
    let template = b.external("Fx", "ParticleSystem", "PS_Sparks");
    let c_psc = b.class("ParticleSystemComponent");
    let scalar = Tags::inner(&mut b)
        .name_value("Name", "Glow")
        .enumeration_at("ParamType", 0, "EParticleSysParamType", "PSPT_Scalar")
        .float("Scalar", 2.5)
        .end();
    let vector = Tags::inner(&mut b)
        .name_value("Name", "Wind")
        .enumeration_at("ParamType", 0, "EParticleSysParamType", "PSPT_Vector")
        .vector("Vector", [1.0, 2.0, 3.0])
        .structure("Color", "Color", vec![10, 20, 30, 255])
        .end();
    let p = Tags::new(&mut b)
        .object("Template", template)
        .boolean("bAutoActivate", true)
        .boolean("bKillOnDeactivate", true)
        .float("WarmupTime", 1.5)
        .struct_array("InstanceParameters", &[scalar, vector])
        .end();
    let psc = b.export(c_psc, root, "ParticleSystemComponent_0", p);
    let p = Tags::new(&mut b).end();
    let bare = b.export(c_psc, root, "ParticleSystemComponent_1", p);
    let (set, lp) = load(b.package());
    let dec = ParticleDecoder::new(&set);
    let c = dec.component(&lp, (psc - 1) as usize).unwrap();
    assert_eq!(c.path, "Map.ParticleSystemComponent_0");
    assert_eq!(c.class, "ParticleSystemComponent");
    assert_eq!(c.template.as_deref(), Some("Fx.PS_Sparks"));
    assert!(c.auto_activate && c.kill_on_deactivate && !c.kill_on_completed);
    assert_eq!(c.warmup_time, 1.5);
    assert_eq!(c.instance_parameters.len(), 2);
    assert_eq!(c.instance_parameters[0].name, "Glow");
    assert_eq!(c.instance_parameters[0].param_type, "PSPT_Scalar");
    assert_eq!(c.instance_parameters[0].scalar, 2.5);
    assert_eq!(c.instance_parameters[1].vector, [1.0, 2.0, 3.0]);
    assert_eq!(c.instance_parameters[1].color, [10, 20, 30, 255]);
    // Nothing stored: no template, not auto-activating (no class defaults
    // in a synthetic set).
    let c = dec.component(&lp, (bare - 1) as usize).unwrap();
    assert_eq!(
        (c.template, c.auto_activate, c.instance_parameters.len()),
        (None, false, 0)
    );
    // A module is not a component.
    assert!(dec.component(&lp, (root - 1) as usize).is_err());
}

/// `FRawDistribution::GetValue1` reads: clamp below the start, lerp inside,
/// hold the last entry past the end; hand-computed values.
#[test]
fn lookup_table_reads() {
    // range (0, 10), entries 0 at t=0, 10 at t=1, 4 at t=2.
    let table = [0.0, 10.0, 0.0, 10.0, 4.0];
    let v = |t: f32| lookup_value(&table, 1, 1.0, 0.0, t, 1).unwrap()[0];
    assert_eq!(v(0.0), 0.0);
    assert_eq!(v(0.5), 5.0);
    assert_eq!(v(1.25), 8.5);
    assert_eq!(
        v(-3.0),
        0.0,
        "times before the start clamp to the first entry"
    );
    assert_eq!(v(2.0), 4.0);
    assert_eq!(v(9.0), 4.0, "the last entry holds");
    assert_eq!(v(f32::NAN), 0.0, "NaN reads the first entry");
    // Start time and scale: entries every 0.5 s from t = 1.
    let w = |t: f32| lookup_value(&table, 1, 2.0, 1.0, t, 1).unwrap()[0];
    assert_eq!(w(1.25), 5.0);
    // Vector entries (chunk 3) and a random table (chunk 6, min/max pairs).
    let vt = [0.0, 1.0, 1.0, 0.5, 0.25, 0.0, 0.0, 0.0];
    assert_eq!(
        lookup_value(&vt, 3, 1.0, 0.0, 0.5, 3).unwrap(),
        vec![0.5, 0.25, 0.125]
    );
    assert_eq!(
        lookup_value(&vt, 3, 0.0, 0.0, 7.0, 3).unwrap(),
        vec![1.0, 0.5, 0.25]
    );
    // Malformed tables yield None.
    assert!(lookup_value(&[1.0], 1, 1.0, 0.0, 0.0, 1).is_none());
    assert!(lookup_value(&table, 0, 1.0, 0.0, 0.0, 1).is_none());
    assert!(lookup_value(&vt, 3, 1.0, 0.0, 0.0, 4).is_none());
}

// ---------------------------------------------------------------------------
// Hostile input
// ---------------------------------------------------------------------------

/// Decode everything reachable; must never panic.
fn exercise(b: &mut Builder, system: usize) {
    let Ok(pkg) = Package::from_bytes(return_bytes(b)) else {
        return;
    };
    let (set, lp) = load(pkg);
    let dec = ParticleDecoder::new(&set);
    let _ = dec.system(&lp, system);
    for i in 0..lp.package.exports.len() {
        let _ = dec.resolve(&lp, i);
        let _ = dec.component(&lp, i);
    }
    let _ = dec.census(std::slice::from_ref(&lp));
}

fn return_bytes(b: &mut Builder) -> Vec<u8> {
    let s = Synth {
        package_flags: 0x0000_0008,
        names: b
            .names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), 0x0007_0010_0000_0000u64 + i as u64))
            .collect(),
        imports: b.imports.clone(),
        exports: b.exports.clone(),
        additional_packages: Vec::new(),
        texture_allocations: Vec::new(),
        guid: [1, 2, 3, 4],
        package_source: 0,
        engine_version: 12097,
        cooker_version: 136,
        depends: true,
        thumbnails: Vec::new(),
    };
    s.build().0
}

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[test]
fn truncated_payloads_never_panic() {
    let (base, system, _, _) = fixture_builder();
    for e in 0..base.exports.len() {
        let len = base.exports[e].payload.len();
        for cut in (0..len).step_by(3) {
            let mut b = base.clone();
            b.exports[e].payload.truncate(cut);
            exercise(&mut b, system);
        }
    }
}

#[test]
fn mutated_payloads_never_panic() {
    let (base, system, _, _) = fixture_builder();
    let mut rng = Rng(0x005E_ED0F_FA57);
    let extremes = [0u32, 1, 0x7FFF_FFFF, 0x8000_0000, 0xFFFF_FFFF, 0x0010_0000];
    for _ in 0..1500 {
        let mut b = base.clone();
        let e = (rng.next() % b.exports.len() as u64) as usize;
        let len = b.exports[e].payload.len();
        if len == 0 {
            continue;
        }
        for _ in 0..1 + rng.next() % 4 {
            let at = (rng.next() % len as u64) as usize;
            match rng.next() % 3 {
                0 => b.exports[e].payload[at] ^= 1 << (rng.next() % 8),
                1 => {
                    let v = extremes[(rng.next() % extremes.len() as u64) as usize].to_le_bytes();
                    for (k, x) in v.iter().enumerate() {
                        if let Some(slot) = b.exports[e].payload.get_mut(at + k) {
                            *slot = *x;
                        }
                    }
                }
                _ => b.exports[e].payload[at] = rng.next() as u8,
            }
        }
        exercise(&mut b, system);
    }
}

/// Archetype cycles and long chains are bounded.
#[test]
fn archetype_cycles_are_bounded() {
    let (mut base, system, _, _) = fixture_builder();
    // Find the two size modules and make them each other's archetype.
    let idx = |b: &Builder, name: &str| {
        let n = b.names.iter().position(|x| x == name).unwrap() as i32;
        b.exports.iter().position(|e| e.name == n).unwrap() as i32 + 1
    };
    let a = idx(&base, "ParticleModuleSize_0");
    let c = idx(&base, "ParticleModuleSize_1");
    base.set_archetype(a, c);
    base.set_archetype(c, a);
    let pkg = Package::from_bytes(return_bytes(&mut base)).unwrap();
    let (set, lp) = load(pkg);
    let dec = ParticleDecoder::new(&set);
    let s = dec.system(&lp, system).unwrap();
    assert_eq!(s.emitters.len(), 1);
    assert!(
        dec.notes().iter().any(|n| n.contains("archetype")),
        "{:?}",
        dec.notes()
    );
}

/// A tagged struct is a delta against the archetype's, member by member:
/// a module that stores only `Distribution` in its `RawDistribution` keeps
/// the archetype's baked table next to its own object.
#[test]
fn raw_distribution_members_merge_over_the_archetype() {
    let mut b = Builder::new();
    let core = b.import("Core", "Package", 0, "Core");
    let package_class = b.import("Core", "Class", core, "Package");
    let p = Tags::new(&mut b).end();
    let root = b.export(package_class, 0, "Fx", p);
    let c_size = b.class("ParticleModuleSize");
    let c_vuni = b.class("DistributionVectorUniform");
    let base = b.reserve(c_size, root, "ParticleModuleSize_0");
    let derived = b.reserve(c_size, root, "ParticleModuleSize_1");
    let p = Tags::new(&mut b)
        .vector("Max", [10.0, 20.0, 30.0])
        .vector("Min", [1.0, 2.0, 3.0])
        .end();
    let d_base = b.export(c_vuni, base, "DistributionVectorUniform_0", p);
    let p = Tags::new(&mut b).vector("Max", [7.0, 7.0, 7.0]).end();
    let d_own = b.export(c_vuni, derived, "DistributionVectorUniform_0", p);
    let table = [1.0, 30.0, 1.0, 2.0, 3.0, 10.0, 20.0, 30.0];
    let s = raw(&mut b, d_base, 2, 6, 0.0, &table);
    let p = Tags::new(&mut b)
        .boolean("bEnabled", true)
        .structure("StartSize", "RawDistributionVector", s)
        .end();
    b.fill(base, p);
    // Only the object reference is stored.
    let s = Tags::inner(&mut b).object("Distribution", d_own).end();
    let p = Tags::new(&mut b)
        .structure("StartSize", "RawDistributionVector", s)
        .end();
    b.fill(derived, p);
    b.set_archetype(derived, base);
    let (set, _lp) = load(b.package());
    let dec = ParticleDecoder::new(&set);
    let mut notes = Vec::new();
    let m = dec.module("Fx.ParticleModuleSize_1", &mut notes).unwrap();
    assert!(m.enabled, "inherited flag");
    let d = m.param("StartSize").unwrap().as_distribution().unwrap();
    assert_eq!(
        d.object.as_deref(),
        Some("Fx.ParticleModuleSize_1.DistributionVectorUniform_0"),
        "own object"
    );
    assert!(
        matches!(&d.value, Distribution::Uniform { min, max, .. } if max == &vec![7.0; 3] && min == &vec![0.0; 3]),
        "{:?}",
        d.value
    );
    // Op, chunk size and the table come from the archetype's struct.
    assert_eq!((d.baked.op, d.baked.chunk, d.baked.len), (2, 6, 8));
    assert_eq!(d.baked.range, Some([1.0, 30.0]));
    assert_eq!(d.table, table.to_vec());
    assert!(notes.is_empty(), "{notes:?}");
}

/// A system may list one emitter, LOD level and module over and over; the
/// decoded size is then far larger than the package. The per-system budget
/// stops that: 256 × 16 × 256 uses of a 64 KiB module would be 64 GiB.
#[test]
fn repeated_references_are_held_to_the_system_budget() {
    use asamu_ue3::particle::{MAX_NOTES, MAX_SYSTEM_BYTES};
    let mut b = Builder::new();
    let core = b.import("Core", "Package", 0, "Core");
    let package_class = b.import("Core", "Class", core, "Package");
    let p = Tags::new(&mut b).end();
    let root = b.export(package_class, 0, "Fx", p);
    let c_ps = b.class("ParticleSystem");
    let c_em = b.class("ParticleSpriteEmitter");
    let c_lod = b.class("ParticleLODLevel");
    let c_life = b.class("ParticleModuleLifetime");
    let c_fconst = b.class("DistributionFloatConstant");
    let ps = b.reserve(c_ps, root, "PS_Bomb");
    let em = b.reserve(c_em, ps, "ParticleSpriteEmitter_0");
    let lod = b.reserve(c_lod, em, "ParticleLODLevel_0");
    let life = b.reserve(c_life, ps, "ParticleModuleLifetime_0");
    let p = Tags::new(&mut b).float("Constant", 1.0).end();
    let d = b.export(c_fconst, life, "DistributionFloatConstant_0", p);
    let table = vec![1.0f32; 16 * 1024];
    let lt = raw(&mut b, d, 1, 1, 0.0, &table);
    let p = Tags::new(&mut b)
        .boolean("bEnabled", true)
        .structure("Lifetime", "RawDistributionFloat", lt)
        .end();
    let module_bytes = p.len();
    b.fill(life, p);
    let p = Tags::new(&mut b)
        .object_array("Modules", &[life; 256])
        .boolean("bEnabled", true)
        .end();
    b.fill(lod, p);
    let p = Tags::new(&mut b)
        .object_array("LODLevels", &[lod; 16])
        .end();
    b.fill(em, p);
    let p = Tags::new(&mut b).object_array("Emitters", &[em; 256]).end();
    b.fill(ps, p);
    let (set, lp) = load(b.package());
    let dec = ParticleDecoder::new(&set);
    let started = std::time::Instant::now();
    let s = dec.system(&lp, (ps - 1) as usize).unwrap();
    let modules: usize = s
        .emitters
        .iter()
        .flat_map(|e| &e.lods)
        .map(|l| l.modules.len())
        .sum();
    assert!(
        modules >= 256,
        "the first LOD level decodes whole: {modules}"
    );
    assert!(
        modules <= MAX_SYSTEM_BYTES / module_bytes,
        "{modules} modules of {module_bytes} bytes"
    );
    assert!(s.notes.len() <= MAX_NOTES);
    assert!(
        s.notes.iter().any(|n| n.contains("budget")),
        "{:?}",
        &s.notes[..s.notes.len().min(4)]
    );
    assert!(started.elapsed().as_secs() < 30);
    // The budget is per system: the next one decodes normally.
    let f = fixture();
    let dec = ParticleDecoder::new(&f.set);
    let _ = dec.system(&f.lp, f.system).unwrap();
    let again = dec.system(&f.lp, f.system).unwrap();
    assert_eq!(again.emitters[0].lods[0].modules.len(), 4);
    assert!(!again.notes.iter().any(|n| n.contains("budget")));
}
