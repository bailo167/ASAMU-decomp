//! Converted materials for skinned meshes (hands, villagers, Maddie, the
//! worm).
//!
//! The importer's skeletal glTF files carry one placeholder material per
//! mesh section, named after the section's UE3 material. Once the converted
//! material descriptions and the texture manifest are loaded, every mesh
//! primitive below an [`NpcSkin`] gets the same approximate material a level
//! mesh with that UE3 material gets (`converted::add_render_material`:
//! base colour, emissive, opacity mode; normal maps only with
//! `--normal-maps`). A section whose material was not converted keeps the
//! placeholder.

use std::collections::HashMap;
use std::path::PathBuf;

use asamu_assets::Manifests;
use bevy::gltf::GltfMaterialName;
use bevy::prelude::*;
use bevy::tasks::futures::check_ready;
use bevy::tasks::{AsyncComputeTaskPool, Task};

use super::skins::{NpcSkin, ancestor_with};
use crate::converted::{self, ConvertedLevel, TextureUsers};

/// A mesh primitive whose material has been looked at.
#[derive(Component)]
pub(super) struct SkinMaterialDone;

/// The converted material descriptions and the materials built from them.
#[derive(Resource, Default)]
pub(super) struct SkinMaterials {
    /// Converted directory the manifests belong to.
    root: Option<PathBuf>,
    task: Option<Task<Option<Manifests>>>,
    manifests: Option<Manifests>,
    /// Built materials by lower-case UE3 path (`None`: no description).
    built: HashMap<String, Option<Handle<StandardMaterial>>>,
}

/// Loads the manifests of the converted directory (again when it changes).
pub(super) fn load_skin_materials(
    level: Option<Res<ConvertedLevel>>,
    mut state: ResMut<SkinMaterials>,
) {
    let Some(level) = level else {
        return;
    };
    let root = level.dir.root();
    if state.root.as_deref() != Some(root) {
        state.root = Some(root.to_path_buf());
        state.manifests = None;
        state.built.clear();
        let dir = level.dir.clone();
        state.task = Some(AsyncComputeTaskPool::get().spawn(async move {
            match dir.load_manifests() {
                Ok(m) => Some(m),
                Err(e) => {
                    warn!("skinned-mesh materials: {e}");
                    None
                }
            }
        }));
    }
    if let Some(task) = state.task.as_mut()
        && let Some(loaded) = check_ready(task)
    {
        state.task = None;
        // Without manifests the placeholders stay.
        state.manifests = Some(loaded.unwrap_or_default());
    }
}

/// Gives the primitives of skinned meshes their converted materials.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) fn apply_skin_materials(
    mut commands: Commands,
    level: Option<Res<ConvertedLevel>>,
    mut state: ResMut<SkinMaterials>,
    server: Res<AssetServer>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    users: Option<ResMut<TextureUsers>>,
    primitives: Query<(Entity, &GltfMaterialName), (With<Mesh3d>, Without<SkinMaterialDone>)>,
    parents: Query<&ChildOf>,
    skins: Query<(), With<NpcSkin>>,
) {
    let (Some(level), Some(mut users)) = (level, users) else {
        return;
    };
    if primitives.is_empty() {
        return;
    }
    let state = &mut *state;
    let Some(manifests) = &state.manifests else {
        return; // still loading
    };
    let mut applied = 0usize;
    let mut kept = 0usize;
    for (entity, name) in &primitives {
        commands.entity(entity).insert(SkinMaterialDone);
        if ancestor_with(&parents, entity, |a| skins.contains(a)).is_none() {
            continue;
        }
        let handle = state
            .built
            .entry(name.0.to_ascii_lowercase())
            .or_insert_with(|| {
                let description = manifests
                    .materials
                    .as_ref()?
                    .render_material(&name.0, manifests.textures.as_ref())?;
                Some(converted::add_render_material(
                    &description,
                    &level.settings,
                    &server,
                    &mut materials,
                    &mut users,
                ))
            });
        match handle {
            Some(h) => {
                commands.entity(entity).insert(MeshMaterial3d(h.clone()));
                applied += 1;
            }
            None => kept += 1,
        }
    }
    if applied + kept > 0 {
        debug!(
            "skinned-mesh materials: {applied} primitive(s) converted, {kept} left with the \
             placeholder"
        );
    }
}
