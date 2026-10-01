// SPDX-License-Identifier: MIT
//! Rig-contract checks over a parsed GLB document.

use crate::glb;
use crate::weights;
use serde_json::{json, Value};

pub struct Diag {
    pub code: &'static str,
    pub detail: String,
}

pub struct Report {
    pub diags: Vec<Diag>,
    pub summary: String,
}

impl Report {
    pub fn failed(&self) -> bool {
        !self.diags.is_empty()
    }

    pub fn to_json(&self, path: &std::path::Path) -> String {
        let diags: Vec<Value> = self
            .diags
            .iter()
            .map(|d| json!({"code": d.code, "detail": d.detail}))
            .collect();
        json!({
            "file": path.display().to_string(),
            "ok": !self.failed(),
            "summary": self.summary,
            "diags": diags,
        })
        .to_string()
    }
}

fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn node_name(nodes: &[Value], idx: usize) -> Option<&str> {
    nodes
        .get(idx)
        .and_then(|n| n.get("name"))
        .and_then(Value::as_str)
}

fn join_names(names: &[String]) -> String {
    const CAP: usize = 5;
    if names.len() <= CAP {
        names.join(", ")
    } else {
        format!("{} (+{} more)", names[..CAP].join(", "), names.len() - CAP)
    }
}

pub fn check_glb(bytes: &[u8]) -> Report {
    let doc = match glb::parse(bytes) {
        Ok(d) => d,
        Err((code, detail)) => {
            return Report {
                diags: vec![Diag { code, detail }],
                summary: "invalid container".to_string(),
            };
        }
    };
    let json = &doc.json;
    let nodes = arr(json, "nodes");
    let skins = arr(json, "skins");
    let n_meshes = arr(json, "meshes").len();
    let clips = arr(json, "animations");

    let mut diags: Vec<Diag> = Vec::new();

    if skins.is_empty() {
        diags.push(Diag {
            code: "R_NO_SKIN",
            detail: "no skins: rig GLB must contain a skeleton".to_string(),
        });
    }

    let mut joints: Vec<String> = Vec::new();
    for (si, skin) in skins.iter().enumerate() {
        for j in arr(skin, "joints") {
            let idx = j.as_u64().and_then(|v| usize::try_from(v).ok());
            match idx.and_then(|i| node_name(nodes, i)) {
                Some(name) => joints.push(name.to_string()),
                None => diags.push(Diag {
                    code: "R_JOINT_INDEX",
                    detail: format!("skin {si} references missing node {j}"),
                }),
            }
        }
    }
    if !skins.is_empty() && joints.is_empty() {
        diags.push(Diag {
            code: "R_NO_JOINTS",
            detail: "skin has no joints".to_string(),
        });
    }
    let mut non_def: Vec<String> = joints
        .iter()
        .filter(|n| !n.starts_with("DEF-"))
        .cloned()
        .collect();
    non_def.sort();
    non_def.dedup();
    if !non_def.is_empty() {
        diags.push(Diag {
            code: "R_JOINT_PREFIX",
            detail: format!(
                "{} non-DEF joint{}: {}",
                non_def.len(),
                if non_def.len() == 1 { "" } else { "s" },
                join_names(&non_def),
            ),
        });
    }

    for (ci, clip) in clips.iter().enumerate() {
        let mut bad: Vec<String> = Vec::new();
        for ch in arr(clip, "channels") {
            let target = ch.get("target").and_then(|t| t.get("node"));
            let name = target
                .and_then(|v| v.as_u64())
                .and_then(|v| usize::try_from(v).ok())
                .and_then(|i| node_name(nodes, i))
                .map(str::to_string);
            match name {
                Some(n) if n.starts_with("DEF-") => {}
                Some(n) => bad.push(n),
                None => bad.push("<missing node>".to_string()),
            }
        }
        bad.sort();
        bad.dedup();
        if !bad.is_empty() {
            diags.push(Diag {
                code: "R_ANIM_TARGET",
                detail: format!("clip {ci} targets non-DEF nodes: {}", join_names(&bad)),
            });
        }
    }

    let (wdiags, skin) = weights::check_weights(json, doc.bin);
    for (code, detail) in wdiags {
        diags.push(Diag { code, detail });
    }

    let mut summary = format!(
        "{} joint{}, {} mesh{}, {} clip{}",
        joints.len(),
        if joints.len() == 1 { "" } else { "s" },
        n_meshes,
        if n_meshes == 1 { "" } else { "es" },
        clips.len(),
        if clips.len() == 1 { "" } else { "s" },
    );
    if skin.checked_prims > 0 {
        summary.push_str(&format!(", max {} infl/vert", skin.max_influences));
    }
    Report { diags, summary }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(doc: &Value) -> Vec<u8> {
        let body = serde_json::to_vec(doc).unwrap();
        let total = 12 + 8 + body.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x4E4F534Au32.to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    fn codes(report: &Report) -> Vec<&'static str> {
        report.diags.iter().map(|d| d.code).collect()
    }

    #[test]
    fn container_rejects_garbage() {
        assert_eq!(codes(&check_glb(b"nope")), vec!["D_GLB_MAGIC"]);
        assert_eq!(codes(&check_glb(b"glTF")), vec!["D_GLB_MAGIC"]);
    }

    #[test]
    fn container_rejects_bad_version() {
        let mut b = pack(&json!({}));
        b[4] = 1;
        assert_eq!(codes(&check_glb(&b)), vec!["D_GLB_VERSION"]);
    }

    #[test]
    fn container_rejects_truncation() {
        let mut b = pack(&json!({}));
        let len = b.len() as u32 + 100;
        b[8..12].copy_from_slice(&len.to_le_bytes());
        assert_eq!(codes(&check_glb(&b)), vec!["D_GLB_TRUNC"]);
    }

    #[test]
    fn clean_rig_passes() {
        let doc = json!({
            "nodes": [{"name": "DEF-spine"}, {"name": "Cube", "mesh": 0}],
            "meshes": [{"name": "Cube", "primitives": [{}]}],
            "skins": [{"joints": [0]}],
            "animations": [{
                "channels": [{"target": {"node": 0}}],
                "samplers": [{}],
            }],
        });
        let r = check_glb(&pack(&doc));
        assert!(
            r.diags.is_empty(),
            "unexpected diags: {:?}",
            r.diags.iter().map(|d| &d.detail).collect::<Vec<_>>()
        );
        assert_eq!(r.summary, "1 joint, 1 mesh, 1 clip");
    }

    #[test]
    fn flags_control_leaks_and_anim_targets() {
        let doc = json!({
            "nodes": [
                {"name": "DEF-spine"},
                {"name": "torso"},
                {"name": "Cube", "mesh": 0},
            ],
            "meshes": [{"name": "Cube"}],
            "skins": [{"joints": [0, 1]}],
            "animations": [{"channels": [
                {"target": {"node": 0}},
                {"target": {"node": 2}},
            ]}],
        });
        let found = codes(&check_glb(&pack(&doc)));
        assert!(
            found.contains(&"R_JOINT_PREFIX"),
            "missing PREFIX: {found:?}"
        );
        assert!(
            found.contains(&"R_ANIM_TARGET"),
            "missing TARGET: {found:?}"
        );
    }

    #[test]
    fn flags_missing_skin_and_joints() {
        let r = check_glb(&pack(&json!({"nodes": [], "meshes": []})));
        assert!(codes(&r).contains(&"R_NO_SKIN"));
        let r2 = check_glb(&pack(&json!({"nodes": [], "skins": [{"joints": []}]})));
        assert!(codes(&r2).contains(&"R_NO_JOINTS"));
    }

    #[test]
    fn flags_dangling_joint_index() {
        let r = check_glb(&pack(&json!({"nodes": [], "skins": [{"joints": [7]}]})));
        assert!(codes(&r).contains(&"R_JOINT_INDEX"));
    }

    fn pack_bin(doc: &Value, bin: &[u8]) -> Vec<u8> {
        let mut body = serde_json::to_vec(doc).unwrap();
        while !body.len().is_multiple_of(4) {
            body.push(b' ');
        }
        let mut blob = bin.to_vec();
        while !blob.len().is_multiple_of(4) {
            blob.push(0);
        }
        let total = 12 + 8 + body.len() + 8 + blob.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x4E4F534Au32.to_le_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x004E4942u32.to_le_bytes());
        out.extend_from_slice(&blob);
        out
    }

    fn skel_doc(attrs: Value, accessors: Value, views: Value, buflen: usize) -> Value {
        json!({
            "nodes": [
                {"name": "DEF-a"}, {"name": "DEF-b"},
                {"name": "DEF-c"}, {"name": "DEF-d"},
                {"name": "Body", "mesh": 0, "skin": 0},
            ],
            "meshes": [{"name": "Body", "primitives": [{"attributes": attrs}]}],
            "skins": [{"joints": [0, 1, 2, 3]}],
            "accessors": accessors,
            "bufferViews": views,
            "buffers": [{"byteLength": buflen}],
        })
    }

    fn f32s(xs: &[f32]) -> Vec<u8> {
        let mut b = Vec::with_capacity(xs.len() * 4);
        for x in xs {
            b.extend_from_slice(&x.to_le_bytes());
        }
        b
    }

    fn detail_for<'a>(r: &'a Report, code: &str) -> Option<&'a str> {
        r.diags
            .iter()
            .find(|d| d.code == code)
            .map(|d| d.detail.as_str())
    }

    fn diags_str(r: &Report) -> Vec<&str> {
        r.diags.iter().map(|d| d.detail.as_str()).collect()
    }

    #[test]
    fn weights_clean_reports_max() {
        let mut bin = vec![
            0, 1, 2, 3, // v0 joints
            0, 1, 0, 0, // v1
            2, 0, 0, 0, // v2
        ];
        bin.extend_from_slice(&f32s(&[
            0.25, 0.25, 0.25, 0.25, // v0: 4 influences, sum 1
            0.5, 0.5, 0.0, 0.0, // v1: 2 influences
            1.0, 0.0, 0.0, 0.0, // v2: 1 influence
        ]));
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1}),
            json!([
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 3},
                {"bufferView": 1, "componentType": 5126, "type": "VEC4", "count": 3},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 12},
                {"byteOffset": 12, "byteLength": 48},
            ]),
            bin.len(),
        );
        let r = check_glb(&pack_bin(&doc, &bin));
        assert!(r.diags.is_empty(), "unexpected: {:?}", diags_str(&r));
        assert_eq!(r.summary, "4 joints, 1 mesh, 0 clips, max 4 infl/vert");
    }

    #[test]
    fn weights_over_limit_second_set() {
        let mut bin = vec![
            0, 0, 0, 0, // v0 joints0
            0, 1, 2, 3, // v1
            0, 1, 0, 0, // v2
        ];
        bin.extend_from_slice(&f32s(&[
            1.0, 0.0, 0.0, 0.0, //
            0.2, 0.2, 0.2, 0.2, // v1 set0: 4 inf, sum 0.8
            0.5, 0.5, 0.0, 0.0, //
        ]));
        bin.extend_from_slice(&[0u8; 12]); // joints1
        bin.extend_from_slice(&f32s(&[
            0.0, 0.0, 0.0, 0.0, //
            0.2, 0.0, 0.0, 0.0, // v1 set1: 5th influence, sum back to 1
            0.0, 0.0, 0.0, 0.0, //
        ]));
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1, "JOINTS_1": 2, "WEIGHTS_1": 3}),
            json!([
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 3},
                {"bufferView": 1, "componentType": 5126, "type": "VEC4", "count": 3},
                {"bufferView": 2, "componentType": 5121, "type": "VEC4", "count": 3},
                {"bufferView": 3, "componentType": 5126, "type": "VEC4", "count": 3},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 12},
                {"byteOffset": 12, "byteLength": 48},
                {"byteOffset": 60, "byteLength": 12},
                {"byteOffset": 72, "byteLength": 48},
            ]),
            bin.len(),
        );
        let r = check_glb(&pack_bin(&doc, &bin));
        let d = detail_for(&r, "W_OVER_INFLUENCE").expect("missing W_OVER_INFLUENCE");
        assert!(d.contains("vertex 1"), "no vertex detail: {d}");
        assert!(d.contains("with 5"), "no count detail: {d}");
        assert!(
            detail_for(&r, "W_UNNORMALIZED").is_none(),
            "sums are 1.0, must not flag: {:?}",
            diags_str(&r)
        );
        assert!(
            r.summary.ends_with("max 5 infl/vert"),
            "summary: {}",
            r.summary
        );
    }

    #[test]
    fn weights_unnormalized() {
        let mut bin = vec![0, 1, 2, 3, 0, 1, 0, 0, 2, 0, 0, 0];
        bin.extend_from_slice(&f32s(&[
            0.25, 0.25, 0.25, 0.25, //
            0.5, 0.5, 0.0, 0.0, //
            0.25, 0.25, 0.0, 0.0, // v2 sums to 0.5
        ]));
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1}),
            json!([
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 3},
                {"bufferView": 1, "componentType": 5126, "type": "VEC4", "count": 3},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 12},
                {"byteOffset": 12, "byteLength": 48},
            ]),
            bin.len(),
        );
        let r = check_glb(&pack_bin(&doc, &bin));
        let d = detail_for(&r, "W_UNNORMALIZED").expect("missing W_UNNORMALIZED");
        assert!(d.contains("vertex 2"), "no vertex detail: {d}");
        assert!(d.contains("sum=0.500"), "no sum detail: {d}");
        assert!(detail_for(&r, "W_OVER_INFLUENCE").is_none());
    }

    #[test]
    fn weights_normalized_ubyte_stride_and_ushort_joints() {
        // USHORT joints, normalized-UBYTE weights with byteStride 8
        // (4 data + 4 pad) and split view/accessor offsets (14 + 2).
        let mut bin: Vec<u8> = Vec::new();
        for j in [0u16, 1, 2, 3, 3, 0, 0, 0] {
            bin.extend_from_slice(&j.to_le_bytes());
        }
        bin.extend_from_slice(&[255, 0, 0, 0, 0, 0, 0, 0]); // v0: sum 1
        bin.extend_from_slice(&[128, 64, 32, 31, 0, 0, 0, 0]); // v1: 255/255
        assert_eq!(bin.len(), 32);
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1}),
            json!([
                {"bufferView": 0, "componentType": 5123, "type": "VEC4", "count": 2},
                {"bufferView": 1, "byteOffset": 2, "componentType": 5121,
                 "normalized": true, "type": "VEC4", "count": 2},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 16},
                {"byteOffset": 14, "byteStride": 8, "byteLength": 14},
            ]),
            bin.len(),
        );
        let r = check_glb(&pack_bin(&doc, &bin));
        assert!(r.diags.is_empty(), "unexpected: {:?}", diags_str(&r));
        assert!(
            r.summary.ends_with("max 4 infl/vert"),
            "summary: {}",
            r.summary
        );
    }

    #[test]
    fn weights_normalized_ushort() {
        let bin_joints = [0u8, 1, 2, 3];
        let mut bin = bin_joints.to_vec();
        for w in [32768u16, 16384, 8192, 8191] {
            // sums to 65535 -> 1.0
            bin.extend_from_slice(&w.to_le_bytes());
        }
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1}),
            json!([
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 1},
                {"bufferView": 1, "componentType": 5123,
                 "normalized": true, "type": "VEC4", "count": 1},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 4},
                {"byteOffset": 4, "byteLength": 8},
            ]),
            bin.len(),
        );
        let r = check_glb(&pack_bin(&doc, &bin));
        assert!(r.diags.is_empty(), "unexpected: {:?}", diags_str(&r));
    }

    #[test]
    fn weights_sparse_is_explicit() {
        let mut bin = vec![0u8; 12];
        bin.extend_from_slice(&f32s(&[0.0; 12]));
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1}),
            json!([
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 3},
                {"bufferView": 1, "componentType": 5126, "type": "VEC4", "count": 3,
                 "sparse": {"count": 1}},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 12},
                {"byteOffset": 12, "byteLength": 48},
            ]),
            bin.len(),
        );
        let r = check_glb(&pack_bin(&doc, &bin));
        assert!(
            detail_for(&r, "W_SPARSE").is_some_and(|d| d.contains("sparse")),
            "sparse must be explicit: {:?}",
            diags_str(&r)
        );
        // Skipped, never misread: no weight verdicts on sparse data.
        assert!(detail_for(&r, "W_UNNORMALIZED").is_none());
        assert!(detail_for(&r, "W_OVER_INFLUENCE").is_none());
    }

    #[test]
    fn weights_rejects_non_normalized_int() {
        // UNSIGNED_BYTE weights without normalized=true would read as
        // 0..255 floats; that must be an error, never a silent misread.
        let mut bin = vec![0u8; 12];
        bin.extend_from_slice(&[64, 64, 64, 63, 0, 0, 0, 0, 0, 0, 0, 0]);
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1}),
            json!([
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 3},
                {"bufferView": 1, "componentType": 5121, "type": "VEC4", "count": 3},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 12},
                {"byteOffset": 12, "byteLength": 12},
            ]),
            bin.len(),
        );
        let r = check_glb(&pack_bin(&doc, &bin));
        assert!(codes(&r).contains(&"W_BAD_ACCESSOR"), "{:?}", diags_str(&r));
        assert!(detail_for(&r, "W_UNNORMALIZED").is_none());
    }

    #[test]
    fn weights_missing_attrs_and_bad_shapes() {
        // Skinned primitive with no skinning attributes at all.
        let doc = skel_doc(
            json!({"POSITION": 0}),
            json!([
                {"bufferView": 0, "componentType": 5126, "type": "VEC3", "count": 3},
            ]),
            json!([{"byteOffset": 0, "byteLength": 36}]),
            36,
        );
        let r = check_glb(&pack_bin(&doc, &[0u8; 36]));
        assert!(
            detail_for(&r, "W_BAD_ACCESSOR").is_some_and(|d| d.contains("lacks JOINTS_0")),
            "{:?}",
            diags_str(&r)
        );

        // JOINTS/WEIGHTS counts disagree.
        let mut bin = vec![0u8; 12];
        bin.extend_from_slice(&f32s(&[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]));
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1}),
            json!([
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 3},
                {"bufferView": 1, "componentType": 5126, "type": "VEC4", "count": 2},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 12},
                {"byteOffset": 12, "byteLength": 32},
            ]),
            bin.len(),
        );
        let r = check_glb(&pack_bin(&doc, &bin));
        assert!(
            detail_for(&r, "W_BAD_ACCESSOR").is_some_and(|d| d.contains("counts disagree")),
            "{:?}",
            diags_str(&r)
        );

        // Unpaired second set.
        let mut bin = vec![0u8; 12];
        bin.extend_from_slice(&f32s(&[1.0, 0.0, 0.0, 0.0]));
        bin.extend_from_slice(&[0u8; 4]);
        let doc = skel_doc(
            json!({"JOINTS_0": 0, "WEIGHTS_0": 1, "JOINTS_1": 2}),
            json!([
                {"bufferView": 0, "componentType": 5121, "type": "VEC4", "count": 1},
                {"bufferView": 1, "componentType": 5126, "type": "VEC4", "count": 1},
                {"bufferView": 2, "componentType": 5121, "type": "VEC4", "count": 1},
            ]),
            json!([
                {"byteOffset": 0, "byteLength": 12},
                {"byteOffset": 12, "byteLength": 16},
                {"byteOffset": 28, "byteLength": 4},
            ]),
            bin.len(),
        );
        let r = check_glb(&pack_bin(&doc, &bin));
        assert!(
            detail_for(&r, "W_BAD_ACCESSOR").is_some_and(|d| d.contains("JOINTS_1")),
            "{:?}",
            diags_str(&r)
        );
    }
}
