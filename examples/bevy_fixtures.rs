// SPDX-License-Identifier: MIT
//! `cargo run --example bevy_fixtures -- DIR`: write one clean GLB and one
//! GLB per Bevy-layer code (B_*, L_*) into DIR, so each rule can be
//! checked against Bevy's real glTF loader (`glbkit/compat/bevy`).
//! File names start with the code the file should raise; `ok_*` files
//! must pass rfcheck and load in Bevy.

use serde_json::{json, Value};
use std::path::Path;

/// 1x1 RGBA PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0xF0,
    0x1F, 0x00, 0x05, 0x00, 0x01, 0xFF, 0x89, 0x99, 0x3D, 0x1D, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

fn f32s(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// BIN layout shared by every fixture:
/// 0: triangle POSITION (3 x VEC3), 36 B
/// 1: keyframe times [0, 1], 8 B
/// 2: translations (2 x VEC3), 24 B
/// 3: PNG image
fn base_bin() -> Vec<u8> {
    let mut bin = f32s(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    bin.extend(f32s(&[0.0, 1.0]));
    bin.extend(f32s(&[0.0, 0.0, 0.0, 0.0, 1.0, 0.0]));
    bin.extend_from_slice(PNG);
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    bin
}

/// Clean scene: Root -> Bone, Mesh (textured triangle); clip "idle"
/// moves Bone.
fn base() -> Value {
    let bin_len = base_bin().len();
    json!({
        "asset": {"version": "2.0", "generator": "rfcheck bevy_fixtures"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [
            {"name": "Root", "children": [1, 2]},
            {"name": "Bone"},
            {"name": "Mesh", "mesh": 0},
        ],
        "meshes": [{"name": "Tri", "primitives": [
            {"attributes": {"POSITION": 0}, "material": 0}]}],
        "materials": [{"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}],
        "textures": [{"source": 0}],
        "images": [{"bufferView": 3, "mimeType": "image/png", "name": "albedo"}],
        "animations": [{"name": "idle",
            "channels": [{"sampler": 0, "target": {"node": 1, "path": "translation"}}],
            "samplers": [{"input": 1, "output": 2}]}],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "type": "VEC3", "count": 3,
             "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]},
            {"bufferView": 1, "componentType": 5126, "type": "SCALAR", "count": 2,
             "min": [0.0], "max": [1.0]},
            {"bufferView": 2, "componentType": 5126, "type": "VEC3", "count": 2},
        ],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 36},
            {"buffer": 0, "byteOffset": 36, "byteLength": 8},
            {"buffer": 0, "byteOffset": 44, "byteLength": 24},
            {"buffer": 0, "byteOffset": 68, "byteLength": PNG.len()},
        ],
        "buffers": [{"byteLength": bin_len}],
    })
}

fn channel(node: usize) -> Value {
    json!({"sampler": 0, "target": {"node": node, "path": "translation"}})
}

fn lod(near: bool) -> Value {
    json!({"skein": [{"hll::level::RenderLod": {"near": near}}]})
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: cargo run --example bevy_fixtures -- DIR");
        std::process::exit(2);
    });
    let dir = Path::new(&dir);
    std::fs::create_dir_all(dir).expect("create output dir");
    let mut files: Vec<(&str, Value, Vec<u8>)> = Vec::new();
    let bin = base_bin();

    files.push(("ok_base", base(), bin.clone()));

    let mut d = base();
    d["nodes"][2]["extras"] = lod(true);
    d["nodes"][0]["children"] = json!([1, 2, 3]);
    d["nodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "Mesh_LOD1", "mesh": 0, "extras": lod(false)}));
    files.push(("ok_lod_pair", d, bin.clone()));

    let mut d = base();
    d["extensionsUsed"] = json!(["KHR_draco_mesh_compression"]);
    d["extensionsRequired"] = json!(["KHR_draco_mesh_compression"]);
    files.push(("B_EXT_REQUIRED", d, bin.clone()));

    let mut d = base();
    d["meshes"][0]["primitives"][0]["mode"] = json!(6);
    files.push(("B_PRIM_MODE", d, bin.clone()));

    let mut d = base();
    d["nodes"][1]["children"] = json!([0]);
    files.push(("B_NODE_CYCLE", d, bin.clone()));

    let mut d = base();
    d["images"][0]["mimeType"] = json!("image/webp");
    files.push(("B_IMAGE_CODEC_webp", d, bin.clone()));

    let mut d = base();
    // PNG bytes declared as KTX2: Bevy decodes by mime and fails.
    d["images"][0]["mimeType"] = json!("image/ktx2");
    files.push(("B_IMAGE_CODEC_mismatch", d, bin.clone()));

    let mut d = base();
    let targets: Vec<Value> = (0..257).map(|_| json!({"POSITION": 0})).collect();
    d["meshes"][0]["primitives"][0]["targets"] = json!(targets);
    d["meshes"][0]["weights"] = json!(vec![0.0; 257]);
    files.push(("B_MORPH_LIMIT", d, bin.clone()));

    let mut d = base();
    // "Loose" is outside the scene: its curve is dropped.
    d["nodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "Loose"}));
    d["animations"][0]["channels"] = json!([channel(1), channel(3)]);
    files.push(("B_ANIM_ORPHAN", d, bin.clone()));

    let mut d = base();
    // Two "Bone" siblings: one Bevy target for both curves.
    d["nodes"][2]["name"] = json!("Bone");
    d["animations"][0]["channels"] = json!([channel(1), channel(2)]);
    files.push(("B_ANIM_PATH", d, bin.clone()));

    let mut d = base();
    let clip = d["animations"][0].clone();
    d["animations"] = json!([clip.clone(), clip]);
    files.push(("B_CLIP_NAME", d, bin.clone()));

    let mut d = base();
    d["nodes"][2]["extras"] = lod(true);
    files.push(("L_LOD_PAIR", d, bin.clone()));

    for (name, doc, bin) in files {
        let json = serde_json::to_vec(&doc).expect("serialize");
        let glb = glbkit::container::write(&json, &bin).expect("write GLB");
        let path = dir.join(format!("{name}.glb"));
        std::fs::write(&path, glb).expect("write file");
        println!("{}", path.display());
    }
}
