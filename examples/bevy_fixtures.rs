// SPDX-License-Identifier: MIT
//! `cargo run --example bevy_fixtures -- DIR`: write clean GLBs and one
//! GLB per Bevy-layer case (B_*, L_*) into DIR, so each rule can be
//! checked against Bevy's real glTF loader (`glbkit/compat/bevy`, Bevy
//! 0.20.0 built with the games' features).
//! File names start with the code the file should raise; `ok_*` files
//! must pass rfcheck and load in Bevy. The table of what each must do is
//! `gates/rfcheck-1/gate.sh`.

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

/// 1x1 JPEG (macOS `sips` from the PNG above).
const JPEG: &[u8] = include_bytes!("one_pixel.jpg");

/// 1x1 Radiance HDR: header, then one flat RGBE pixel (under 8 wide, so not run-length coded).
fn hdr() -> Vec<u8> {
    let mut b = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X 1\n".to_vec();
    b.extend_from_slice(&[128, 64, 32, 129]);
    b
}

fn f32s(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// BIN layout shared by every fixture:
/// 0: triangle POSITION (3 x VEC3), 36 B
/// 1: keyframe times [0, 1], 8 B
/// 2: translations (2 x VEC3), 24 B
/// 3: PNG image
fn base_bin() -> Vec<u8> {
    bin_with_image(PNG)
}

/// The shared layout with other image bytes in view 3.
fn bin_with_image(image: &[u8]) -> Vec<u8> {
    let mut bin = f32s(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    bin.extend(f32s(&[0.0, 1.0]));
    bin.extend(f32s(&[0.0, 0.0, 0.0, 0.0, 1.0, 0.0]));
    bin.extend_from_slice(image);
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    bin
}

/// The clean scene with `image` (declared `mime`) as its texture.
fn with_image(image: &[u8], mime: &str) -> (Value, Vec<u8>) {
    let bin = bin_with_image(image);
    let mut d = base();
    d["images"][0]["mimeType"] = json!(mime);
    d["bufferViews"][3]["byteLength"] = json!(image.len());
    d["buffers"][0]["byteLength"] = json!(bin.len());
    (d, bin)
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
    json!({"skein": [{"demo::level::RenderLod": {"near": near}}]})
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

    // Bevy 0.20 builds gltf with KHR_materials_specular (new) and _volume: it reads them, but
    // gltf-json's validation still accepts neither as required (ENABLED_EXTENSIONS).
    let mut d = base();
    d["extensionsUsed"] = json!(["KHR_materials_specular"]);
    d["materials"][0]["extensions"] = json!({"KHR_materials_specular": {"specularFactor": 0.5}});
    files.push(("ok_ext_used_specular", d, bin.clone()));

    for ext in ["specular", "volume"] {
        let name = format!("KHR_materials_{ext}");
        let mut d = base();
        d["extensionsUsed"] = json!([name]);
        d["extensionsRequired"] = json!([name]);
        let mut e = serde_json::Map::new();
        e.insert(name.clone(), json!({}));
        d["materials"][0]["extensions"] = Value::Object(e);
        files.push((
            if ext == "specular" {
                "B_EXT_REQUIRED_specular"
            } else {
                "B_EXT_REQUIRED_volume"
            },
            d,
            bin.clone(),
        ));
    }

    // Image types: Bevy picks the decoder from the mime type, lower-cased, by exact name, and
    // decodes only the formats its features build (the games: png, hdr, ktx2).
    let (d, b) = with_image(&hdr(), "image/vnd.radiance");
    files.push(("ok_image_hdr", d, b));
    let mut d = base();
    d["images"][0]["mimeType"] = json!("image/PNG");
    files.push(("ok_image_mime_upper", d, bin.clone()));
    let (d, b) = with_image(JPEG, "image/jpeg");
    files.push(("B_IMAGE_CODEC_jpeg", d, b));
    let (d, b) = with_image(JPEG, "image/jpg");
    files.push(("B_IMAGE_CODEC_jpg_alias", d, b));

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
