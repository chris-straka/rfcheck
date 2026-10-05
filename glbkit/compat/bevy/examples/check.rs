//! `cargo run --release --example check -- a.glb b.glb ...`: load each GLB through Bevy's own
//! glTF loader (headless, no GPU, no window), spawn its default scene, and
//! report what Bevy built. Exit 1 if any file fails to load, has a mesh
//! without positions, or declares skins that never become `SkinnedMesh`
//! entities with joint indices and weights.

use std::path::Path;
use std::time::Duration;

use bevy::asset::LoadState;
use bevy::gltf::{Gltf, GltfMesh};
use bevy::mesh::skinning::SkinnedMesh;
use bevy::prelude::*;
use bevy::render::{RenderPlugin, settings::WgpuSettings};
use bevy::window::ExitCondition;
use bevy::world_serialization::WorldAssetRoot;

const MAX_UPDATES: usize = 2000;

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        eprintln!("usage: cargo run --release --example check -- <file.glb>...");
        std::process::exit(2);
    }
    let mut failed = 0;
    for f in &files {
        match check(Path::new(f)) {
            Ok(line) => println!("ok   {f}: {line}"),
            Err(e) => {
                failed += 1;
                println!("FAIL {f}: {e}");
            }
        }
    }
    println!("{} of {} loaded cleanly in Bevy 0.19.1", files.len() - failed, files.len());
    std::process::exit(if failed == 0 { 0 } else { 1 });
}

fn app_for(dir: &Path) -> App {
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                ..default()
            })
            .set(RenderPlugin {
                render_creation: WgpuSettings {
                    backends: None,
                    ..default()
                }
                .into(),
                ..default()
            })
            .set(AssetPlugin {
                file_path: dir.to_string_lossy().into_owned(),
                ..default()
            })
            .disable::<bevy::winit::WinitPlugin>()
            .disable::<bevy::log::LogPlugin>(),
    );
    // App::run would do this; driving update() by hand must too (the glTF
    // loader registers itself in GltfPlugin::finish).
    app.finish();
    app.cleanup();
    app
}

fn check(path: &Path) -> Result<String, String> {
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    let dir = path.parent().ok_or("no parent dir")?;
    let name = path.file_name().ok_or("no file name")?.to_string_lossy().into_owned();
    let mut app = app_for(dir);
    let handle: Handle<Gltf> = app.world().resource::<AssetServer>().load(name);

    // Wait for the glTF and everything it depends on (meshes, skins, clips).
    let mut loaded = false;
    for _ in 0..MAX_UPDATES {
        app.update();
        let server = app.world().resource::<AssetServer>();
        match server.load_state(&handle) {
            LoadState::Failed(e) => return Err(format!("load failed: {e}")),
            LoadState::Loaded if server.is_loaded_with_dependencies(&handle) => {
                loaded = true;
                break;
            }
            _ => std::thread::sleep(Duration::from_millis(1)),
        }
    }
    if !loaded {
        return Err(format!("still loading after {MAX_UPDATES} updates"));
    }

    let world = app.world();
    let gltf = world.resource::<Assets<Gltf>>().get(&handle).ok_or("Gltf asset missing")?;
    let (n_meshes, n_skins, n_clips) = (gltf.meshes.len(), gltf.skins.len(), gltf.animations.len());
    let scene = gltf.default_scene.clone().or_else(|| gltf.scenes.first().cloned());
    let gltf_meshes = world.resource::<Assets<GltfMesh>>();
    let meshes = world.resource::<Assets<Mesh>>();
    let mut prims = 0;
    let mut skinned_prims = 0;
    let mut verts = 0;
    for gm in &gltf.meshes {
        let gm = gltf_meshes.get(gm).ok_or("GltfMesh missing")?;
        for p in &gm.primitives {
            let mesh = meshes.get(&p.mesh).ok_or("Mesh missing")?;
            let n = mesh
                .attribute(Mesh::ATTRIBUTE_POSITION)
                .ok_or_else(|| format!("mesh '{}' has no positions", gm.name))?
                .len();
            verts += n;
            prims += 1;
            let joints = mesh.attribute(Mesh::ATTRIBUTE_JOINT_INDEX).map(|a| a.len());
            let weights = mesh.attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT).map(|a| a.len());
            match (joints, weights) {
                (Some(j), Some(w)) if j == n && w == n => skinned_prims += 1,
                (None, None) => {}
                _ => return Err(format!("mesh '{}': joints/weights missing or miscounted", gm.name)),
            }
        }
    }

    // Spawn the scene, as a game does, and count the skinned entities.
    let mut skinned_entities = 0;
    if let Some(scene) = scene {
        app.world_mut().spawn(WorldAssetRoot(scene));
        for _ in 0..30 {
            app.update();
        }
        let world = app.world_mut();
        let mut q = world.query::<&SkinnedMesh>();
        for skinned in q.iter(world) {
            if skinned.joints.is_empty() {
                return Err("SkinnedMesh with no joints".into());
            }
            skinned_entities += 1;
        }
    }
    if n_skins > 0 && (skinned_prims == 0 || skinned_entities == 0) {
        return Err(format!(
            "{n_skins} skins but {skinned_prims} skinned primitives, {skinned_entities} SkinnedMesh entities"
        ));
    }
    Ok(format!(
        "{n_meshes} meshes / {prims} prims / {verts} verts, {n_skins} skins, \
         {skinned_entities} SkinnedMesh entities, {n_clips} clips"
    ))
}
