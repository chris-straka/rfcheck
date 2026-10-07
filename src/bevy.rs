// SPDX-License-Identifier: MIT
//! v6: Bevy loader checks (B_* layer) and RenderLod markers (L_*).
//!
//! The games load GLBs through Bevy 0.19.1's glTF loader (`bevy_gltf`).
//! Every B_* code is a file that loader rejects outright or silently
//! loses data from, read off its source (cited per check), so like X_*
//! these are FAIL-level and fire only on what the JSON proves.
//!
//! L_* covers HLL's distant-mesh markers: Skein extras
//! `{"skein": [{"hll::level::RenderLod": {"near": bool}}]}` written by
//! `tools/add_port_lods.py`. A near mesh `X` pairs with a far mesh
//! `X_LOD1`; on mobile the near one hides past 50 m and the far one
//! shows, so an unpaired marker is a mesh that vanishes (or doubles).

use crate::util::{acc_count, arr, as_idx, as_usize, join_names, mesh_tag, plural};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// `extensionsRequired` entries Bevy 0.19.1 accepts: gltf-json's
/// `ENABLED_EXTENSIONS` for the features `bevy_gltf` turns on. Any other
/// required extension (Draco, meshopt, KHR_mesh_quantization, basisu,
/// webp, ...) fails validation and the whole file fails to load.
pub const BEVY_REQUIRED_OK: &[&str] = &[
    "KHR_lights_punctual",
    "KHR_materials_unlit",
    "KHR_texture_transform",
    "KHR_materials_transmission",
    "KHR_materials_ior",
    "KHR_materials_emissive_strength",
];

/// Image codecs the game's Bevy build decodes. HLL builds Bevy with
/// `default-features = false, features = ["2d", "3d", "ui"]`, which
/// brings `png`, `hdr`, and `ktx2` (+ zstd) but not `jpeg`, `webp`, or
/// `dds`. A texture in a missing codec fails the whole GLB load
/// ("You may need to add the feature for the file format").
pub const DEFAULT_CODECS: &[&str] = &["png", "ktx2"];

pub const CODEC_NAMES: &str = "png, jpeg, ktx2, webp, dds, hdr";

/// glTF morph targets per mesh Bevy accepts (`MAX_MORPH_WEIGHTS`).
pub const MAX_MORPH_TARGETS: usize = 256;
/// Bevy packs 9 floats per vertex per target into a texture at most
/// 2048 x 2048 (`MAX_TEXTURE_WIDTH`, `MorphAttributes::COMPONENT_COUNT`).
pub const MAX_MORPH_VERTS: usize = 2048 * 2048 / 9;

/// The Skein type path HLL's level loader registers.
pub const RENDER_LOD: &str = "hll::level::RenderLod";
/// Far-mesh name suffix (`add_port_lods.py`).
pub const LOD_SUFFIX: &str = "_LOD1";

/// Codec name for a glTF image mime type; None when Bevy knows no such
/// format (it then fails with "invalid image mime type").
fn codec_of_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" | "image/jpg" => Some("jpeg"),
        "image/ktx2" => Some("ktx2"),
        "image/webp" => Some("webp"),
        "image/vnd-ms.dds" | "image/x-dds" => Some("dds"),
        "image/vnd.radiance" | "image/x-hdr" => Some("hdr"),
        _ => None,
    }
}

/// Codec name from the image bytes themselves.
fn sniff_codec(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpeg")
    } else if b.starts_with(b"\xabKTX 20\xbb\r\n\x1a\n") {
        Some("ktx2")
    } else if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("webp")
    } else if b.starts_with(b"DDS ") {
        Some("dds")
    } else if b.starts_with(b"#?RADIANCE") || b.starts_with(b"#?RGBE") {
        Some("hdr")
    } else {
        None
    }
}

/// Codec of a relative image path, by extension (Bevy picks the loader
/// by extension for external files).
fn codec_of_path(uri: &str) -> Option<&'static str> {
    let ext = uri.rsplit('.').next()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("png"),
        "jpg" | "jpeg" => Some("jpeg"),
        "ktx2" => Some("ktx2"),
        "webp" => Some("webp"),
        "dds" => Some("dds"),
        "hdr" => Some("hdr"),
        _ => None,
    }
}

/// Parse a `--image-codecs` list (comma-separated names).
pub fn parse_codecs(list: &str) -> Result<Vec<&'static str>, String> {
    let mut out = Vec::new();
    for name in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let known = ["png", "jpeg", "ktx2", "webp", "dds", "hdr"];
        let c = known
            .iter()
            .find(|k| **k == name || (name == "jpg" && **k == "jpeg"))
            .ok_or_else(|| format!("unknown image codec '{name}' (expected {CODEC_NAMES})"))?;
        if !out.contains(c) {
            out.push(*c);
        }
    }
    if out.is_empty() {
        return Err("--image-codecs needs at least one codec".to_string());
    }
    Ok(out)
}

fn name_of(nodes: &[Value], i: usize) -> Option<&str> {
    nodes.get(i)?.get("name")?.as_str()
}

fn node_tag(nodes: &[Value], i: usize) -> String {
    match name_of(nodes, i) {
        Some(n) if !n.is_empty() => format!("node {i} '{n}'"),
        _ => format!("node {i}"),
    }
}

/// Bevy names an unnamed node `GltfNode{index}` (`node_name`), which is
/// what animation target paths are built from.
fn bevy_name(nodes: &[Value], i: usize) -> String {
    match name_of(nodes, i) {
        Some(n) => n.to_string(),
        None => format!("GltfNode{i}"),
    }
}

fn check_extensions(json: &Value, out: &mut Vec<(&'static str, String)>) {
    let mut bad: Vec<String> = arr(json, "extensionsRequired")
        .iter()
        .filter_map(Value::as_str)
        .filter(|e| !BEVY_REQUIRED_OK.contains(e))
        .map(str::to_string)
        .collect();
    bad.sort();
    bad.dedup();
    if !bad.is_empty() {
        out.push((
            "B_EXT_REQUIRED",
            format!(
                "requires {} Bevy cannot load: {}",
                plural(bad.len(), "an extension", "extensions"),
                join_names(&bad),
            ),
        ));
    }
}

/// `primitive_topology`: LINE_LOOP (2) and TRIANGLE_FAN (6) are
/// `GltfError::UnsupportedPrimitive`.
fn check_modes(json: &Value, out: &mut Vec<(&'static str, String)>) {
    let meshes = arr(json, "meshes");
    for (mi, mesh) in meshes.iter().enumerate() {
        for (pi, prim) in arr(mesh, "primitives").iter().enumerate() {
            let mode = prim.get("mode").and_then(Value::as_u64).unwrap_or(4);
            let what = match mode {
                2 => "LINE_LOOP",
                6 => "TRIANGLE_FAN",
                _ => continue,
            };
            out.push((
                "B_PRIM_MODE",
                format!(
                    "{} prim {pi}: mode {mode} ({what}) is unsupported in Bevy",
                    mesh_tag(meshes, mi)
                ),
            ));
        }
    }
}

/// `check_for_cycles`: the node graph must be a tree, else
/// `GltfError::CircularChildren`. Checked over every node (the spec
/// forbids cycles anywhere; Bevy walks from scene roots).
fn check_cycles(json: &Value, out: &mut Vec<(&'static str, String)>) {
    let nodes = arr(json, "nodes");
    let n = nodes.len();
    // 0 = unvisited, 1 = on stack, 2 = done.
    let mut state = vec![0u8; n];
    let mut reported: BTreeSet<usize> = BTreeSet::new();
    for start in 0..n {
        if state[start] != 0 {
            continue;
        }
        // Iterative DFS: (node, next child position).
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        state[start] = 1;
        while let Some(&mut (v, ref mut k)) = stack.last_mut() {
            let kids = arr(&nodes[v], "children");
            if *k >= kids.len() {
                state[v] = 2;
                stack.pop();
                continue;
            }
            let c = kids[*k].clone();
            *k += 1;
            let Some(c) = as_idx(&c).filter(|c| *c < n) else {
                continue;
            };
            match state[c] {
                0 => {
                    state[c] = 1;
                    stack.push((c, 0));
                }
                1 if reported.insert(c) => {
                    out.push((
                        "B_NODE_CYCLE",
                        format!(
                            "{} is its own ancestor (Bevy needs a node tree)",
                            node_tag(nodes, c)
                        ),
                    ));
                }
                _ => {}
            }
        }
    }
}

fn image_codec_issue(json: &Value, bin: &[u8], im: &Value, allowed: &[&str]) -> Option<String> {
    let mime = im.get("mimeType").and_then(Value::as_str);
    if let Some(bvi) = im.get("bufferView").and_then(as_idx) {
        let Some(mime) = mime else {
            return Some("embedded image has no mimeType".to_string());
        };
        let Some(codec) = codec_of_mime(mime) else {
            return Some(format!(
                "mimeType '{mime}' is not an image format Bevy knows"
            ));
        };
        // Bevy decodes by the declared mime: bytes in another codec fail.
        let bv = arr(json, "bufferViews").get(bvi)?;
        if bv.get("buffer").and_then(Value::as_u64).unwrap_or(0) == 0 {
            let off = bv.get("byteOffset").and_then(as_idx).unwrap_or(0);
            let len = as_usize(bv, "byteLength")?;
            let bytes = bin.get(off..off.checked_add(len)?)?;
            if let Some(real) = sniff_codec(bytes) {
                if real != codec {
                    return Some(format!("declared {mime} but the bytes are {real}"));
                }
            }
        }
        if !allowed.contains(&codec) {
            return Some(format!(
                "{codec} texture; the game's Bevy build decodes {}",
                allowed.join("/")
            ));
        }
        return None;
    }
    let uri = im.get("uri").and_then(Value::as_str)?;
    let codec = if let Some(rest) = uri.strip_prefix("data:") {
        let m = rest.split([';', ',']).next().unwrap_or("");
        match codec_of_mime(m) {
            Some(c) => c,
            None => {
                return Some(format!(
                    "data URI type '{m}' is not an image format Bevy knows"
                ))
            }
        }
    } else {
        codec_of_path(uri)?
    };
    if !allowed.contains(&codec) {
        return Some(format!(
            "{codec} texture; the game's Bevy build decodes {}",
            allowed.join("/")
        ));
    }
    None
}

/// Textures Bevy cannot decode fail the whole GLB load. Only images a
/// texture actually uses are loaded, so orphan images are skipped.
fn check_images(json: &Value, bin: &[u8], allowed: &[&str], out: &mut Vec<(&'static str, String)>) {
    let images = arr(json, "images");
    let used: BTreeSet<usize> = arr(json, "textures")
        .iter()
        .filter_map(|t| t.get("source").and_then(as_idx))
        .collect();
    for ii in used {
        let Some(im) = images.get(ii) else { continue };
        if let Some(why) = image_codec_issue(json, bin, im, allowed) {
            let tag = match im.get("name").and_then(Value::as_str) {
                Some(n) if !n.is_empty() => format!("image {ii} '{n}'"),
                _ => format!("image {ii}"),
            };
            out.push(("B_IMAGE_CODEC", format!("{tag}: {why}")));
        }
    }
}

/// `MorphWeights::new` / `MorphTargetImage::new`: more than 256 targets
/// or more than 2048^2 / 9 vertices per morphed primitive fail.
fn check_morph(json: &Value, out: &mut Vec<(&'static str, String)>) {
    let meshes = arr(json, "meshes");
    for (mi, mesh) in meshes.iter().enumerate() {
        let mut targets = 0usize;
        let mut verts = 0usize;
        for prim in arr(mesh, "primitives") {
            let t = arr(prim, "targets").len();
            if t == 0 {
                continue;
            }
            targets = targets.max(t);
            if let Some(v) = prim
                .get("attributes")
                .and_then(|a| a.get("POSITION"))
                .and_then(as_idx)
                .and_then(|ai| acc_count(json, ai))
            {
                verts = verts.max(v);
            }
        }
        if targets > MAX_MORPH_TARGETS {
            out.push((
                "B_MORPH_LIMIT",
                format!(
                    "{}: {targets} morph targets exceed Bevy's {MAX_MORPH_TARGETS}",
                    mesh_tag(meshes, mi)
                ),
            ));
        }
        if targets > 0 && verts > MAX_MORPH_VERTS {
            out.push((
                "B_MORPH_LIMIT",
                format!(
                    "{}: {verts} morphed verts exceed Bevy's {MAX_MORPH_VERTS} (2048^2 / 9 texels)",
                    mesh_tag(meshes, mi)
                ),
            ));
        }
    }
}

/// Bevy's animation target id is the node-name path from a scene root
/// (`collect_path`, `AnimationTargetId::from_names`). Index of every
/// reachable node -> its path (first scene/root wins, as in Bevy).
fn scene_paths(json: &Value) -> BTreeMap<usize, Vec<String>> {
    let nodes = arr(json, "nodes");
    let mut paths: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for scene in arr(json, "scenes") {
        for root in arr(scene, "nodes").iter().filter_map(as_idx) {
            if root >= nodes.len() {
                continue;
            }
            let mut seen: BTreeSet<usize> = BTreeSet::new();
            let mut stack: Vec<(usize, Vec<String>)> = vec![(root, Vec::new())];
            while let Some((v, mut path)) = stack.pop() {
                if !seen.insert(v) {
                    continue;
                }
                path.push(bevy_name(nodes, v));
                for c in arr(&nodes[v], "children").iter().filter_map(as_idx) {
                    if c < nodes.len() && !seen.contains(&c) {
                        stack.push((c, path.clone()));
                    }
                }
                paths.insert(v, path);
            }
        }
    }
    paths
}

/// Animation curves Bevy drops or merges without an error:
/// - a target node outside every scene has no path, so its curves are
///   ignored ("Animation ignored for node ...");
/// - two animated nodes with the same name path share one
///   `AnimationTargetId`, so one drives both and the other is lost;
/// - clips sharing a name collide in `Gltf::named_animations` (the game
///   looks clips up by name).
fn check_anims(json: &Value, out: &mut Vec<(&'static str, String)>) {
    let clips = arr(json, "animations");
    if clips.is_empty() {
        return;
    }
    let nodes = arr(json, "nodes");
    let paths = scene_paths(json);
    let mut animated: BTreeSet<usize> = BTreeSet::new();
    for (ci, clip) in clips.iter().enumerate() {
        let mut orphans: Vec<String> = Vec::new();
        for ch in arr(clip, "channels") {
            let Some(t) = ch
                .get("target")
                .and_then(|t| t.get("node"))
                .and_then(as_idx)
                .filter(|t| *t < nodes.len())
            else {
                continue; // dangling: the R_/X_ layers' business
            };
            if paths.contains_key(&t) {
                animated.insert(t);
            } else {
                orphans.push(bevy_name(nodes, t));
            }
        }
        orphans.sort();
        orphans.dedup();
        if !orphans.is_empty() {
            out.push((
                "B_ANIM_ORPHAN",
                format!(
                    "clip {ci} animates {} outside every scene (Bevy ignores them): {}",
                    plural(orphans.len(), "a node", "nodes"),
                    join_names(&orphans),
                ),
            ));
        }
    }
    let mut by_path: BTreeMap<&[String], Vec<usize>> = BTreeMap::new();
    for t in &animated {
        by_path.entry(paths[t].as_slice()).or_default().push(*t);
    }
    for (path, ids) in by_path {
        if ids.len() > 1 {
            out.push((
                "B_ANIM_PATH",
                format!(
                    "{} animated nodes share the name path '{}' (one Bevy target): nodes {}",
                    ids.len(),
                    path.join("/"),
                    ids.iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
            ));
        }
    }
    let mut names: BTreeMap<&str, usize> = BTreeMap::new();
    for clip in clips {
        if let Some(n) = clip.get("name").and_then(Value::as_str) {
            *names.entry(n).or_default() += 1;
        }
    }
    let dups: Vec<String> = names
        .iter()
        .filter(|(_, c)| **c > 1)
        .map(|(n, c)| format!("'{n}' x{c}"))
        .collect();
    if !dups.is_empty() {
        out.push((
            "B_CLIP_NAME",
            format!(
                "duplicate clip {} (Bevy's named_animations keeps one): {}",
                plural(dups.len(), "name", "names"),
                join_names(&dups),
            ),
        ));
    }
}

/// One node's RenderLod marker: Some(Ok(near)), Some(Err(why)) when the
/// marker is present but unreadable, None when there is no marker.
fn render_lod(node: &Value) -> Option<Result<bool, String>> {
    let entries = node.get("extras")?.get("skein")?.as_array()?;
    let e = entries.iter().find_map(|e| e.get(RENDER_LOD))?;
    Some(match e.get("near").and_then(Value::as_bool) {
        Some(b) => Ok(b),
        None => Err(format!("{RENDER_LOD} marker without a boolean 'near'")),
    })
}

/// Name -> (node index, near) for every RenderLod-marked node.
pub fn lod_markers(
    json: &Value,
    out: &mut Vec<(&'static str, String)>,
) -> BTreeMap<String, (usize, bool)> {
    let nodes = arr(json, "nodes");
    let mut marks: BTreeMap<String, (usize, bool)> = BTreeMap::new();
    for (i, nd) in nodes.iter().enumerate() {
        match render_lod(nd) {
            None => {}
            Some(Err(why)) => out.push(("L_LOD_PAIR", format!("{}: {why}", node_tag(nodes, i)))),
            Some(Ok(near)) => {
                marks.insert(bevy_name(nodes, i), (i, near));
            }
        }
    }
    marks
}

/// Every near mesh `X` needs a far `X_LOD1` and every far mesh its near
/// `X`: on mobile the near one hides past 50 m and the far one shows
/// until then, so an unpaired marker is a mesh that pops out (or a far
/// mesh drawn on top of geometry with no hand-off).
fn check_lods(json: &Value, out: &mut Vec<(&'static str, String)>) {
    let nodes = arr(json, "nodes");
    let marks = lod_markers(json, out);
    for (name, (i, near)) in &marks {
        let problem = if *near {
            if name.ends_with(LOD_SUFFIX) {
                Some("named like a far mesh but marked near".to_string())
            } else if marks.contains_key(&format!("{name}{LOD_SUFFIX}")) {
                None // a mislabeled partner reports itself
            } else {
                Some(format!("near mesh has no far '{name}{LOD_SUFFIX}'"))
            }
        } else {
            match name.strip_suffix(LOD_SUFFIX) {
                None => Some(format!("far mesh is not named '<near>{LOD_SUFFIX}'")),
                Some(base) if marks.contains_key(base) => None,
                Some(base) => Some(format!("far mesh has no near '{base}'")),
            }
        };
        if let Some(p) = problem {
            out.push(("L_LOD_PAIR", format!("{}: {p}", node_tag(nodes, *i))));
        }
    }
}

pub struct BevyOpts<'a> {
    pub codecs: &'a [&'a str],
}

impl Default for BevyOpts<'_> {
    fn default() -> Self {
        BevyOpts {
            codecs: DEFAULT_CODECS,
        }
    }
}

pub fn check_bevy(json: &Value, bin: &[u8], opts: &BevyOpts) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    check_extensions(json, &mut out);
    check_modes(json, &mut out);
    check_cycles(json, &mut out);
    check_images(json, bin, opts.codecs, &mut out);
    check_morph(json, &mut out);
    check_anims(json, &mut out);
    check_lods(json, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn codes_of(json: &Value, bin: &[u8], codecs: &[&str]) -> Vec<(&'static str, String)> {
        check_bevy(json, bin, &BevyOpts { codecs })
    }

    fn run(json: Value) -> Vec<(&'static str, String)> {
        codes_of(&json, &[], DEFAULT_CODECS)
    }

    fn only(found: &[(&'static str, String)], code: &str) -> Vec<String> {
        found
            .iter()
            .filter(|(c, _)| *c == code)
            .map(|(_, d)| d.clone())
            .collect()
    }

    #[test]
    fn required_extensions() {
        let ok =
            run(json!({"extensionsRequired": ["KHR_lights_punctual", "KHR_texture_transform"]}));
        assert!(ok.is_empty(), "{ok:?}");
        let bad = run(json!({"extensionsRequired": [
            "KHR_draco_mesh_compression", "KHR_lights_punctual", "EXT_meshopt_compression"]}));
        let d = only(&bad, "B_EXT_REQUIRED");
        assert_eq!(d.len(), 1);
        assert!(
            d[0].contains("KHR_draco_mesh_compression, EXT_meshopt_compression")
                || d[0].contains("EXT_meshopt_compression, KHR_draco_mesh_compression"),
            "{d:?}"
        );
        // Used-but-optional extensions are fine (Bevy ignores them).
        assert!(run(json!({"extensionsUsed": ["KHR_draco_mesh_compression"]})).is_empty());
    }

    #[test]
    fn primitive_modes() {
        let doc = json!({"meshes": [{"name": "Fan", "primitives": [
            {"mode": 4}, {"mode": 6}, {"mode": 2}, {"mode": 5}, {}]}]});
        let d = only(&run(doc), "B_PRIM_MODE");
        assert_eq!(d.len(), 2, "{d:?}");
        assert!(
            d[0].contains("mesh 0 'Fan' prim 1: mode 6 (TRIANGLE_FAN)"),
            "{d:?}"
        );
        assert!(d[1].contains("LINE_LOOP"));
    }

    #[test]
    fn node_cycles() {
        let tree = json!({"nodes": [{"children": [1, 2]}, {"children": [2]}, {}]});
        assert!(run(tree).is_empty(), "shared child is not a cycle");
        let cyc =
            json!({"nodes": [{"name": "A", "children": [1]}, {"name": "B", "children": [0]}]});
        let d = only(&run(cyc), "B_NODE_CYCLE");
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].contains("own ancestor"));
        let selfloop = json!({"nodes": [{"children": [0]}]});
        assert_eq!(only(&run(selfloop), "B_NODE_CYCLE").len(), 1);
    }

    fn image_doc(mime: Option<&str>, bytes_len: usize) -> Value {
        let mut im = json!({"bufferView": 0, "name": "albedo"});
        if let Some(m) = mime {
            im["mimeType"] = json!(m);
        }
        json!({
            "images": [im, {"uri": "unused.jpg"}],
            "textures": [{"source": 0}],
            "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": bytes_len}],
            "buffers": [{"byteLength": bytes_len}],
        })
    }

    #[test]
    fn image_codecs() {
        let png = b"\x89PNG\r\n\x1a\n0000".to_vec();
        let jpg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0, 0];
        // PNG declared PNG: clean (the unused JPEG image is never loaded).
        let doc = image_doc(Some("image/png"), png.len());
        assert!(codes_of(&doc, &png, DEFAULT_CODECS).is_empty());
        // JPEG: not in HLL's Bevy build, fine once the game enables it.
        let doc = image_doc(Some("image/jpeg"), jpg.len());
        let d = only(&codes_of(&doc, &jpg, DEFAULT_CODECS), "B_IMAGE_CODEC");
        assert_eq!(d.len(), 1);
        assert!(d[0].contains("image 0 'albedo': jpeg texture"), "{d:?}");
        assert!(d[0].contains("decodes png/ktx2"), "{d:?}");
        assert!(codes_of(&doc, &jpg, &["png", "jpeg"]).is_empty());
        // Declared PNG, bytes JPEG: Bevy decodes by mime and fails.
        let doc = image_doc(Some("image/png"), jpg.len());
        let d = only(&codes_of(&doc, &jpg, DEFAULT_CODECS), "B_IMAGE_CODEC");
        assert!(
            d[0].contains("declared image/png but the bytes are jpeg"),
            "{d:?}"
        );
        // No mime on an embedded image, and an unknown mime.
        let doc = image_doc(None, png.len());
        assert!(
            only(&codes_of(&doc, &png, DEFAULT_CODECS), "B_IMAGE_CODEC")[0].contains("no mimeType")
        );
        let doc = image_doc(Some("image/gif"), png.len());
        assert!(
            only(&codes_of(&doc, &png, DEFAULT_CODECS), "B_IMAGE_CODEC")[0].contains("image/gif")
        );
        // Data URIs and external files by type/extension.
        let doc = json!({"images": [{"uri": "data:image/webp;base64,AAAA"}, {"uri": "t.png"},
                                    {"uri": "t.JPG"}],
                         "textures": [{"source": 0}, {"source": 1}, {"source": 2}]});
        let d = only(&codes_of(&doc, &[], DEFAULT_CODECS), "B_IMAGE_CODEC");
        assert_eq!(d.len(), 2, "{d:?}");
        assert!(d[0].contains("webp") && d[1].contains("jpeg"), "{d:?}");
    }

    #[test]
    fn codec_list_parses() {
        assert_eq!(
            parse_codecs("png, jpg,ktx2,png").unwrap(),
            vec!["png", "jpeg", "ktx2"]
        );
        assert!(parse_codecs("gif")
            .unwrap_err()
            .contains("unknown image codec 'gif'"));
        assert!(parse_codecs(" , ").is_err());
    }

    #[test]
    fn morph_limits() {
        let targets: Vec<Value> = (0..257).map(|_| json!({"POSITION": 0})).collect();
        let doc = json!({
            "meshes": [{"name": "Face", "primitives": [
                {"attributes": {"POSITION": 0}, "targets": targets}]}],
            "accessors": [{"count": 10}],
        });
        let d = only(&run(doc), "B_MORPH_LIMIT");
        assert_eq!(d.len(), 1);
        assert!(
            d[0].contains("257 morph targets exceed Bevy's 256"),
            "{d:?}"
        );
        let doc = json!({
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "targets": [{"POSITION": 0}]}]}],
            "accessors": [{"count": MAX_MORPH_VERTS + 1}],
        });
        assert!(only(&run(doc), "B_MORPH_LIMIT")[0].contains("morphed verts"));
        // Big meshes without morphs are the budget layer's business.
        let doc = json!({
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
            "accessors": [{"count": MAX_MORPH_VERTS + 1}],
        });
        assert!(run(doc).is_empty());
    }

    fn rig(names: &[&str], children: &[(usize, usize)]) -> Vec<Value> {
        let mut nodes: Vec<Value> = names.iter().map(|n| json!({"name": n})).collect();
        for (p, c) in children {
            let list = nodes[*p]
                .get("children")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut list = list;
            list.push(json!(c));
            nodes[*p]["children"] = json!(list);
        }
        nodes
    }

    #[test]
    fn animation_targets() {
        // Root -> DEF-a, DEF-b: both reachable, distinct paths: clean.
        let nodes = rig(&["Root", "DEF-a", "DEF-b", "Loose"], &[(0, 1), (0, 2)]);
        let clip = |targets: &[usize], name: &str| {
            json!({"name": name,
                   "channels": targets.iter().map(|t| json!({"target": {"node": t}})).collect::<Vec<_>>()})
        };
        let doc = json!({"nodes": nodes, "scenes": [{"nodes": [0]}],
                         "animations": [clip(&[1, 2], "idle"), clip(&[1], "walk")]});
        assert!(run(doc).is_empty());
        // Node 3 is outside the scene: Bevy drops its curves.
        let doc = json!({"nodes": nodes, "scenes": [{"nodes": [0]}],
                         "animations": [clip(&[1, 3], "idle")]});
        let d = only(&run(doc), "B_ANIM_ORPHAN");
        assert!(
            d[0].contains("clip 0 animates a node outside every scene"),
            "{d:?}"
        );
        assert!(d[0].contains("Loose"));
        // No scenes at all: every animated node is orphaned.
        let doc = json!({"nodes": nodes, "animations": [clip(&[1], "idle")]});
        assert_eq!(only(&run(doc), "B_ANIM_ORPHAN").len(), 1);
        // Two siblings with one name collide on one target id.
        let twins = rig(&["Root", "DEF-a", "DEF-a"], &[(0, 1), (0, 2)]);
        let doc = json!({"nodes": twins, "scenes": [{"nodes": [0]}],
                         "animations": [clip(&[1, 2], "idle")]});
        let d = only(&run(doc), "B_ANIM_PATH");
        assert!(
            d[0].contains("'Root/DEF-a'") && d[0].contains("nodes 1, 2"),
            "{d:?}"
        );
        // Same names under different parents are distinct paths.
        let ok = rig(
            &["Root", "L", "R", "DEF-x", "DEF-x"],
            &[(0, 1), (0, 2), (1, 3), (2, 4)],
        );
        let doc = json!({"nodes": ok, "scenes": [{"nodes": [0]}],
                         "animations": [clip(&[3, 4], "idle")]});
        assert!(run(doc).is_empty());
        // Duplicate clip names collide in named_animations.
        let doc = json!({"nodes": nodes, "scenes": [{"nodes": [0]}],
                         "animations": [clip(&[1], "run"), clip(&[2], "run"), clip(&[1], "idle")]});
        let d = only(&run(doc), "B_CLIP_NAME");
        assert!(d[0].contains("'run' x2"), "{d:?}");
    }

    fn lod(name: &str, near: Value) -> Value {
        json!({"name": name, "extras": {"skein": [
            {"hll::level::LevelCollider": {}},
            {RENDER_LOD: {"near": near}}]}})
    }

    #[test]
    fn render_lod_pairs() {
        let paired = json!({"nodes": [lod("Rock", json!(true)), lod("Rock_LOD1", json!(false)),
                                      {"name": "Plain", "extras": {"skein": []}}]});
        assert!(run(paired).is_empty());
        let doc = json!({"nodes": [lod("Rock", json!(true)), lod("Tree_LOD1", json!(false)),
                                   lod("Bush", json!(false)), lod("Moss", json!("yes"))]});
        let d = only(&run(doc), "L_LOD_PAIR");
        assert_eq!(d.len(), 4, "{d:?}");
        let all = d.join("\n");
        assert!(
            all.contains("'Rock': near mesh has no far 'Rock_LOD1'"),
            "{all}"
        );
        assert!(
            all.contains("'Tree_LOD1': far mesh has no near 'Tree'"),
            "{all}"
        );
        assert!(
            all.contains("'Bush': far mesh is not named '<near>_LOD1'"),
            "{all}"
        );
        assert!(
            all.contains("'Moss': hll::level::RenderLod marker without a boolean 'near'"),
            "{all}"
        );
        // Both halves marked near: reported once, on the far-named one.
        let doc = json!({"nodes": [lod("Rock", json!(true)), lod("Rock_LOD1", json!(true))]});
        let d = only(&run(doc), "L_LOD_PAIR");
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].contains("'Rock_LOD1': named like a far mesh but marked near"));
    }
}
